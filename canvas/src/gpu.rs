//! The canvas's GPU side: one device, two pipelines, one bind group layout,
//! and a swapchain per output.
//!
//! The mechanism is `caustics-host`'s, as it is the bar's: the device and its
//! refusals (`caustics_host::device`), the layouts built from the reflection,
//! the pipelines and their all-or-nothing reload (`caustics_host::program`),
//! and one target's swapchain (`caustics_host::surface`). What is here is what
//! makes them the canvas: which buffers it allocates and binds by which
//! reflected name (docs/abi.md, *The canvas's buffer*), its two entry points
//! and the order they run in, and the two image slots.
//!
//! A frame is one output's: `canvas_apply` once, `canvas_draw` over that
//! output's swapchain image, one copy of `state` back, one wait. Every output
//! is paced on its own frame callbacks, so a frame never waits for another
//! output's.

use std::ptr::NonNull;

use anyhow::{Context, Result, anyhow, bail};
use caustics_bar_abi::canvas::{CanvasFacts, CanvasParams, CanvasState, SLOT_A, SLOT_B};
use caustics_bar_abi::wire::Image;
use caustics_bar_abi::{CANVAS_ENTRIES, NO_INDEX, as_bytes};
use caustics_host::device::{self, Device, Presentation};
use caustics_host::program::{Entry, Program, Refused, group_entries};
use caustics_host::surface::{
    self, AcquireError, Surface, acquire, create_surface, refresh_mirror,
};
use punchcutter_slang::{Binding, BindingKind, Reflection};

use crate::clock;

/// The program's reflection, baked by `build.rs` from the same compile as the
/// SPIR-V below: the bindings the layouts are built from, and the literals
/// the witness printer resolves.
pub const BAKED_REFLECTION: &str =
    include_str!(concat!(env!("OUT_DIR"), "/canvas.reflection.json"));

/// The two entry points' positions in `CANVAS_ENTRIES`.
const APPLY: usize = 0;
const DRAW: usize = 1;

/// Workgroup edge of `canvas_draw` (`[numthreads(8, 8, 1)]`).
const TILE: u32 = 8;

/// The two entry points, in `CANVAS_ENTRIES` order: each one's name, its
/// pipeline's label and its `[numthreads]`.
pub static CANVAS_ENTRY_TABLE: [Entry; CANVAS_ENTRIES.len()] = [
    Entry {
        name: CANVAS_ENTRIES[APPLY],
        label: "caustics-canvas-apply",
        workgroup: (1, 1, 1),
    },
    Entry {
        name: CANVAS_ENTRIES[DRAW],
        label: "caustics-canvas-draw",
        workgroup: (TILE, TILE, 1),
    },
];

type BakedSpv = [&'static [u8]; CANVAS_ENTRIES.len()];

const SPV_RGBA8UNORM: BakedSpv = [
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_apply.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_draw.rgba8unorm.spv")),
];

const SPV_BGRA8UNORM: BakedSpv = [
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_apply.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_draw.bgra8unorm.spv")),
];

/// The image slots' texel format: straight RGBA8, as the wire carries it.
const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The debug labels of each output's ids texture (the host's surface makes
/// one; the canvas binds none) and its uniform.
const IDS_LABEL: &str = "caustics-canvas-ids";
const PARAMS_LABEL: &str = "caustics-canvas-params";

/// One image slot: the texture view the caustic samples. Its size is the
/// texture's, which the caustic asks of the texture.
struct Picture {
    view: wgpu::TextureView,
}

/// The device and everything on it that is not one output's.
pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    presentation: Presentation,
    program: Program,
    state: wgpu::Buffer,
    readback: wgpu::Buffer,
    facts: wgpu::Buffer,
    private: wgpu::Buffer,
    images: [Picture; 2],
    sampler: wgpu::Sampler,
    mirror: Box<CanvasState>,
    /// The frame stamp (docs/abi.md, *The three rules the buffer adds*, rule
    /// 1): ≥ 1, written into `params.frame` and advanced after the frame's
    /// readback.
    frame: u32,
}

impl Gpu {
    /// Open the device on the first output's `wl_surface` and give that
    /// output its swapchain.
    ///
    /// # Safety
    ///
    /// `display` and `surface` must be a live `wl_display` and `wl_surface`
    /// that outlive the returned [`Surface`].
    pub unsafe fn new(
        display: NonNull<std::ffi::c_void>,
        surface: NonNull<std::ffi::c_void>,
        width: u32,
        height: u32,
    ) -> Result<(Gpu, Surface)> {
        let instance = device::instance();
        // SAFETY: the caller's contract, above.
        let wgpu_surface = unsafe { create_surface(&instance, display, surface)? };
        let bindings = baked_reflection().bindings.clone();
        let Device {
            instance,
            adapter,
            device,
            queue,
            features,
            ..
        } = Device::open(
            instance,
            Some(&wgpu_surface),
            &bindings,
            "caustics-canvas-device",
        )?;
        let presentation = Presentation::negotiate(&wgpu_surface, &adapter, features)?;
        let gpu = Gpu::bring_up(instance, adapter, device, queue, presentation, bindings)?;
        let target = gpu.configure(Some(wgpu_surface), width, height);
        Ok((gpu, target))
    }

    /// The same device, buffers and pipelines with no surface: what the
    /// tests run the caustic on.
    #[cfg(test)]
    pub fn headless() -> Result<Gpu> {
        let bindings = baked_reflection().bindings.clone();
        let Device {
            instance,
            adapter,
            device,
            queue,
            ..
        } = Device::open(
            device::instance(),
            None,
            &bindings,
            "caustics-canvas-device",
        )?;
        Gpu::bring_up(
            instance,
            adapter,
            device,
            queue,
            Presentation::HEADLESS,
            bindings,
        )
    }

    fn bring_up(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        presentation: Presentation,
        bindings: Vec<Binding>,
    ) -> Result<Gpu> {
        let format = presentation.format;
        let program = Program::new(
            &device,
            bindings,
            &["caustics-canvas-bgl"],
            "caustics-canvas-pipeline-layout",
            &|name| storage_texture_format(name, format),
            &CANVAS_ENTRY_TABLE,
            baked_spv(format)?,
        )
        .context("the canvas caustic baked in at build time does not load")?;

        let state_size = size_of::<CanvasState>() as u64;
        let state = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-canvas-state"),
            size: state_size,
            // The caustic owns every byte; wgpu zeroes it at creation.
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-canvas-state-readback"),
            size: state_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let facts = punchcutter_core::gpu::storage_buffer(
            &device,
            "caustics-canvas-facts",
            size_of::<CanvasFacts>() as u64,
        );
        // At the stride the reflection reports, and never read or written
        // here: the caustic's own (docs/abi.md, *The canvas's buffer*). The
        // tests read it back, so a test build may copy out of it.
        let private = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-canvas-private"),
            size: private_stride(program.bindings())?,
            usage: if cfg!(test) {
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC
            } else {
                wgpu::BufferUsages::STORAGE
            },
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("caustics-canvas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let images = [
            empty_picture(&device, &queue),
            empty_picture(&device, &queue),
        ];
        let gpu = Gpu {
            instance,
            adapter,
            device,
            queue,
            presentation,
            program,
            state,
            readback,
            facts,
            private,
            images,
            sampler,
            mirror: Box::new(CanvasState::ZERO),
            frame: 1,
        };
        // No picture: a zeroed `facts` would say slot A holds one.
        gpu.write_facts(&CanvasFacts {
            from_slot: NO_INDEX,
            to_slot: NO_INDEX,
            ..CanvasFacts::default()
        });
        Ok(gpu)
    }

    /// The define and the format-unknown switch matching the negotiated
    /// format: what the compiler sidecar is asked to compile for.
    pub fn dst_format_args(&self) -> (&'static str, bool) {
        surface::dst_format(self.presentation.format)
            .expect("bring_up refuses a format no caustic was baked for")
            .args()
    }

    /// A swapchain for another output, on the device this one opened.
    ///
    /// # Safety
    ///
    /// As [`Gpu::new`]: the handles must outlive the returned [`Surface`].
    pub unsafe fn attach(
        &self,
        display: NonNull<std::ffi::c_void>,
        surface: NonNull<std::ffi::c_void>,
        width: u32,
        height: u32,
    ) -> Result<Surface> {
        // SAFETY: the caller's contract, above.
        let wgpu_surface = unsafe { create_surface(&self.instance, display, surface)? };
        let caps = wgpu_surface.get_capabilities(&self.adapter);
        if !caps.formats.contains(&self.presentation.format)
            || !caps.usages.contains(wgpu::TextureUsages::STORAGE_BINDING)
        {
            bail!(
                "this output's surface does not offer {:?} as a storage image (offers {:?})",
                self.presentation.format,
                caps.formats
            );
        }
        Ok(self.configure(Some(wgpu_surface), width, height))
    }

    fn configure(
        &self,
        surface: Option<wgpu::Surface<'static>>,
        width: u32,
        height: u32,
    ) -> Surface {
        Surface::configure(
            &self.device,
            surface,
            &self.presentation,
            width,
            height,
            IDS_LABEL,
            PARAMS_LABEL,
            size_of::<CanvasParams>() as u64,
        )
    }

    /// Resize an output's swapchain after a configure or a scale change.
    pub fn resize(&self, target: &mut Surface, width: u32, height: u32) -> bool {
        target.resize(&self.device, width, height, IDS_LABEL)
    }

    /// What the host knows about the picture, written whole.
    pub fn write_facts(&self, facts: &CanvasFacts) {
        self.queue.write_buffer(&self.facts, 0, as_bytes(facts));
    }

    /// Put an image in a slot: a texture of its size, its bytes copied as
    /// they came. The slot's old texture goes when nothing binds it.
    pub fn upload(&mut self, slot: u32, image: &Image) -> Result<()> {
        if slot != SLOT_A && slot != SLOT_B {
            bail!("no image slot {slot}");
        }
        let limit = self.device.limits().max_texture_dimension_2d;
        if image.width > limit || image.height > limit {
            bail!(
                "a {}x{} image is past this device's {limit} px textures",
                image.width,
                image.height
            );
        }
        let expected = image.width as usize * image.height as usize * 4;
        if image.rgba.len() != expected {
            bail!(
                "an image of {} bytes for {}x{}",
                image.rgba.len(),
                image.width,
                image.height
            );
        }
        let texture = picture_texture(&self.device, image.width, image.height);
        self.queue.write_texture(
            texture.as_image_copy(),
            &image.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );
        self.images[slot as usize] = Picture {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        };
        Ok(())
    }

    /// Swap in a freshly compiled generation, or keep what runs.
    pub fn reload(&mut self, reflection: &Reflection, spirv: &[&[u8]]) -> Result<(), Refused> {
        self.program.reload(&self.device, reflection, spirv)
    }

    /// The state as of the last readback.
    pub fn mirror(&self) -> &CanvasState {
        &self.mirror
    }

    /// The output's next swapchain image, or `None` when there is nothing to
    /// draw into this time (outdated, occluded), which is not an error: the
    /// next configure or callback brings another chance.
    ///
    /// Apart from [`Gpu::draw`] so that the host can make its frame request
    /// between the two, riding the commit the present makes.
    pub fn acquire(&self, target: &mut Surface) -> Result<Option<surface::Frame>> {
        let swapchain = target
            .surface
            .as_ref()
            .context("this target presents nothing")?;
        match acquire(swapchain, "canvas") {
            Ok(frame) => Ok(Some(frame)),
            Err(AcquireError::Stale) => {
                target.refit_once(&self.device);
                Ok(None)
            }
            Err(AcquireError::Unavailable) => Ok(None),
            Err(err) => bail!("the swapchain texture could not be acquired: {err}"),
        }
    }

    /// Draw one frame on one output into the image [`Gpu::acquire`] gave,
    /// present it and read the state back. Returns the frame stamp the
    /// witness ring carries.
    pub fn draw(&mut self, target: &mut Surface, frame: surface::Frame, scale: u32) -> Result<u32> {
        let view = frame
            .texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let (width, height) = target.size();
        self.dispatch(&target.params, &view, width, height, scale);
        self.queue.present(frame.texture);
        if frame.suboptimal {
            target.refit_once(&self.device);
        }
        self.finish()
    }

    /// One frame's work: the uniform, both entry points, the copy back.
    fn dispatch(
        &self,
        params: &wgpu::Buffer,
        dst: &wgpu::TextureView,
        width: u32,
        height: u32,
        scale: u32,
    ) {
        let (now_lo, now_hi) = clock::words(clock::boottime_ms());
        self.write_params(params, width, height, scale, now_lo, now_hi);
        self.encode(params, dst, width, height);
    }

    fn write_params(
        &self,
        params: &wgpu::Buffer,
        width: u32,
        height: u32,
        scale: u32,
        now_lo: u32,
        now_hi: u32,
    ) {
        let words = CanvasParams {
            width,
            height,
            scale,
            frame: self.frame,
            now_lo,
            now_hi,
            ..CanvasParams::default()
        };
        self.queue.write_buffer(params, 0, as_bytes(&words));
    }

    fn encode(&self, params: &wgpu::Buffer, dst: &wgpu::TextureView, width: u32, height: u32) {
        let group = self.bind_group(params, dst);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-canvas-frame"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-canvas-pass"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &group, &[]);
            // Two dispatches, two usage scopes: the barrier between them is
            // what lets `canvas_draw` read the alpha `canvas_apply` wrote.
            pass.set_pipeline(self.program.pipeline(APPLY));
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(self.program.pipeline(DRAW));
            pass.dispatch_workgroups(width.div_ceil(TILE).max(1), height.div_ceil(TILE).max(1), 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.state,
            0,
            &self.readback,
            0,
            size_of::<CanvasState>() as u64,
        );
        self.queue.submit(Some(encoder.finish()));
    }

    /// Wait the frame out, read the state back, advance the stamp. Returns
    /// the stamp the frame was drawn under.
    fn finish(&mut self) -> Result<u32> {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|err| anyhow!("device wait failed: {err}"))?;
        refresh_mirror(&self.device, &self.readback, &mut *self.mirror);
        let drawn = self.frame;
        self.frame = self.frame.wrapping_add(1).max(1);
        Ok(drawn)
    }

    /// The bind group for one output's frame, **by name** from the
    /// reflection (docs/abi.md, *Bindings and the oracle*): no binding number
    /// is written on this side.
    fn bind_group(&self, params: &wgpu::Buffer, dst: &wgpu::TextureView) -> wgpu::BindGroup {
        let entries = group_entries(self.program.bindings(), 0, |name| match name {
            "state" => self.state.as_entire_binding(),
            "params" => params.as_entire_binding(),
            "facts" => self.facts.as_entire_binding(),
            "dst" => wgpu::BindingResource::TextureView(dst),
            "image_a" => wgpu::BindingResource::TextureView(&self.images[0].view),
            "image_b" => wgpu::BindingResource::TextureView(&self.images[1].view),
            "tex_sampler" => wgpu::BindingResource::Sampler(&self.sampler),
            "private" => self.private.as_entire_binding(),
            other => panic!("{}", unknown_binding(other)),
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("caustics-canvas-bg"),
            layout: self.program.layout(0),
            entries: &entries,
        })
    }
}

/// Every binding name the bind group knows, in the order `bind_group`'s
/// match lists them.
pub const BOUND: [&str; 8] = [
    "state",
    "params",
    "facts",
    "dst",
    "image_a",
    "image_b",
    "tex_sampler",
    "private",
];

fn unknown_binding(name: &str) -> String {
    format!(
        "the canvas caustic declares a binding `{name}` this host has no resource for; \
         canvas/src/gpu.rs binds {BOUND:?}"
    )
}

/// The baked reflection, read once.
pub fn baked_reflection() -> &'static Reflection {
    static BAKED: std::sync::OnceLock<Reflection> = std::sync::OnceLock::new();
    BAKED.get_or_init(|| caustics_host::program::read_baked(BAKED_REFLECTION))
}

/// The one fact the reflection cannot carry about a storage image: its texel
/// format. The canvas has one storage image, the output's.
fn storage_texture_format(name: &str, dst: wgpu::TextureFormat) -> wgpu::TextureFormat {
    match name {
        "dst" => dst,
        other => panic!(
            "`{other}` is a storage image the canvas has no texel format for; it knows `dst`"
        ),
    }
}

/// The stride Slang reports for the canvas's private buffer.
fn private_stride(bindings: &[Binding]) -> Result<u64> {
    bindings
        .iter()
        .find(|binding| binding.name == "private")
        .and_then(|binding| match binding.kind {
            BindingKind::Storage {
                stride: Some(stride),
                ..
            } => Some(u64::from(stride)),
            _ => None,
        })
        .context("the canvas caustic declares no `private` storage buffer with a stride")
}

/// The baked pair for a negotiated format.
fn baked_spv(format: wgpu::TextureFormat) -> Result<&'static [&'static [u8]]> {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => Ok(&SPV_RGBA8UNORM),
        wgpu::TextureFormat::Bgra8Unorm => Ok(&SPV_BGRA8UNORM),
        other => bail!("surface format {other:?} is not one the canvas was baked for"),
    }
}

fn picture_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("caustics-canvas-picture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: IMAGE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A slot before any picture: one transparent texel, which `facts` never
/// names (`NO_INDEX`), so nothing samples it.
fn empty_picture(device: &wgpu::Device, queue: &wgpu::Queue) -> Picture {
    let texture = picture_texture(device, 1, 1);
    queue.write_texture(
        texture.as_image_copy(),
        &[0, 0, 0, 0],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    Picture {
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
    }
}

#[cfg(test)]
mod tests;
