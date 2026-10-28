//! The canvas's GPU side: one device, three pipelines, three bind group
//! layouts, and a swapchain per output.
//!
//! The mechanism is `caustics-host`'s, as it is the bar's: the device and its
//! refusals (`caustics_host::device`), the layouts built from the reflection,
//! the pipelines and their all-or-nothing reload (`caustics_host::program`),
//! and one target's swapchain (`caustics_host::surface`). What is here is what
//! makes them the canvas: which buffers it allocates and binds by which
//! reflected name (docs/abi.md, *The canvas's buffer*), its three entry
//! points and the order they run in, the two image slots, and the text: the
//! engine's buffers and the watchdog are `caustics_host::text`'s, the bar's
//! own, filled by the canvas's own `caustics-glyphs`; the literal pool and
//! the prompt's text buffer are the canvas's.
//!
//! A frame is one surface's (an output's background, its overlay while a dim
//! is up, its prompt pane while a prompt is): `canvas_apply` once,
//! `canvas_draw` over that surface's swapchain image, one copy of `state`
//! back, one wait. Before it, when the text or the font changed since the
//! last, `canvas_shape` runs in a submission of its own with the watchdog
//! round it. Every surface is paced on its own frame callbacks, so a frame
//! never waits for another's.

use std::ptr::NonNull;

use anyhow::{Context, Result, anyhow, bail};
use caustics_bar_abi::canvas::{
    CANVAS_GLYPHS, CANVAS_STRINGS_BYTES, CANVAS_TEXT_BLOCKS, CanvasFacts, CanvasParams,
    CanvasState, MAX_CANVAS_LITERALS, PROMPT_TEXT_BYTES, SLOT_A, SLOT_B, TARGET_BACKGROUND,
    TARGET_LOCK, TARGET_OVERLAY, TARGET_PROMPT, TEXT_LITERALS, TEXT_PROMPT,
};
use caustics_bar_abi::wire::Image;
use caustics_bar_abi::{CANVAS_ENTRIES, Literal, NO_INDEX, as_bytes};
use caustics_host::compile::Wake;
use caustics_host::device::{self, Device, Presentation};
use caustics_host::program::{
    Entry, Landed, Program, Refused, Specialised, Variant, group_entries,
};
use caustics_host::strings::Strings;
use caustics_host::surface::{
    self, AcquireError, Surface, acquire, create_surface, refresh_mirror,
};
use caustics_host::text::{Group, TextBuffers, Watchdog};
use punchcutter_slang::{Binding, BindingKind, Reflection};

use crate::clock;

/// The program's reflection, baked by `build.rs` from the same compile as the
/// SPIR-V below: the bindings the layouts are built from, and the literals
/// the witness printer resolves.
pub const BAKED_REFLECTION: &str =
    include_str!(concat!(env!("OUT_DIR"), "/canvas.reflection.json"));

/// The three entry points' positions in `CANVAS_ENTRIES`.
const SHAPE: usize = 0;
const APPLY: usize = 1;
const DRAW: usize = 2;

/// The order the pipelines are built in: what a frame needs first, so the
/// wallpaper is drawn before the shaper, most of the compile, is built.
const NEED: [usize; CANVAS_ENTRIES.len()] = [APPLY, DRAW, SHAPE];

/// `canvas_draw` is built once per surface, with `SPECIALISED_TARGET`
/// (`caustics/canvas_state.slang`, SpecId 0) set to it, so each pipeline
/// carries that surface's code alone (docs/architecture.md, *The caustic*).
/// The background's first, then the lock's, which a restarted canvas whose
/// session is locked wants next.
static SURFACES: [Variant; 4] = [
    Variant {
        value: TARGET_BACKGROUND,
        name: "background",
    },
    Variant {
        value: TARGET_LOCK,
        name: "lock",
    },
    Variant {
        value: TARGET_OVERLAY,
        name: "overlay",
    },
    Variant {
        value: TARGET_PROMPT,
        name: "prompt",
    },
];
const SPECIALISED: [Specialised; 1] = [Specialised {
    entry: DRAW,
    id: 0,
    values: &SURFACES,
}];

/// The compile workers' count (`caustics_host::compile`). Two: on RADV with
/// a cold cache the shaper builds beside the rest instead of after it, which
/// is the bar's whole program in 11.7 s rather than 16.2 s with its chips no
/// later; the canvas's are the shaper and two small ones, and come in no
/// slower. A third bought the bar half a second.
const COMPILE_WORKERS: usize = 2;

/// The canvas's own set, the text engine's curve buffers (`text.slang`
/// declares them in set 1), and the shaper's tables with the strings buffer.
const SET_CANVAS: u32 = 0;
const SET_TEXT: u32 = 1;
const SET_SHAPE: u32 = 2;

/// One shaped glyph in `runs` (`punchcutter_shape::ShapedGlyph`), bytes.
const SHAPED_GLYPH: u64 = 32;

/// Workgroup edge of `canvas_draw` (`[numthreads(8, 8, 1)]`).
const TILE: u32 = 8;

/// Threads per workgroup in `canvas_shape` (`[numthreads(128, 1, 1)]`): two
/// waves, one lane of each shaping a block, for the reasons beside that
/// attribute in `caustics/canvas.slang`.
/// `the_shape_dispatch_matches_the_caustics_group` in `canvas/src/gpu/tests.rs`
/// checks these two against the attribute.
const SHAPE_THREADS: u32 = 128;

/// Blocks shaped per workgroup: one per wave of 64 lanes.
const SHAPE_BLOCKS_PER_GROUP: u32 = SHAPE_THREADS / 64;

/// The three entry points, in `CANVAS_ENTRIES` order: each one's name, its
/// pipeline's label and its `[numthreads]`.
pub static CANVAS_ENTRY_TABLE: [Entry; CANVAS_ENTRIES.len()] = [
    Entry {
        name: CANVAS_ENTRIES[SHAPE],
        label: "caustics-canvas-shape",
        workgroup: (SHAPE_THREADS, 1, 1),
    },
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
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_shape.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_apply.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_draw.rgba8unorm.spv")),
];

const SPV_BGRA8UNORM: BakedSpv = [
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_shape.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_apply.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/canvas_draw.bgra8unorm.spv")),
];

/// The image slots' texel format: straight RGBA8, as the wire carries it.
const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The debug label of each surface's uniform. The canvas binds no ids
/// texture, so its surfaces make none.
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
    /// The shaped glyphs (`runs`), the caustic's own: never written or read
    /// here.
    runs: wgpu::Buffer,
    /// The literal pool: its index (`literals`) and its bytes (the strings
    /// buffer, `shapeUtf8`), and a copy of the bytes for the covers.
    literals: wgpu::Buffer,
    strings: wgpu::Buffer,
    strings_copy: Vec<u8>,
    /// The prompt's text, and a copy of it for the covers.
    prompt_text: wgpu::Buffer,
    prompt_copy: Vec<u8>,
    /// The text engine's buffers and the two groups over them.
    text: TextBuffers,
    text_group: wgpu::BindGroup,
    shape_group: wgpu::BindGroup,
    /// The watchdog round `canvas_shape`.
    watchdog: Watchdog,
    /// The text or the font changed, and `canvas_shape` is owed a pass.
    shape_pending: bool,
    /// What the shape pass binds where a frame binds a surface's: a uniform
    /// of its own and a 1x1 image of the format, neither of which it touches.
    shape_params: wgpu::Buffer,
    shape_dst: wgpu::TextureView,
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
    ///
    /// The pipelines are not built when this returns: they are queued on the
    /// compile workers, and `wake` is called each time one lands, for the
    /// host to [`Gpu::poll_pipelines`]. Until they are, a frame is skipped
    /// ([`Gpu::can_draw`]).
    pub unsafe fn new(
        display: NonNull<std::ffi::c_void>,
        surface: NonNull<std::ffi::c_void>,
        width: u32,
        height: u32,
        wake: Wake,
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
            shader_clock,
        } = Device::open(
            instance,
            Some(&wgpu_surface),
            &bindings,
            "caustics-canvas-device",
        )?;
        let presentation = Presentation::negotiate(&wgpu_surface, &adapter, features)?;
        let gpu = Gpu::bring_up(
            instance,
            adapter,
            device,
            queue,
            presentation,
            bindings,
            features.contains(wgpu::Features::TIMESTAMP_QUERY),
            shader_clock,
            wake,
        )?;
        let target = gpu.configure(Some(wgpu_surface), width, height);
        Ok((gpu, target))
    }

    /// The same device, buffers and pipelines with no surface: what the
    /// tests run the caustic on. Every pipeline is built before this
    /// returns, so a test runs them synchronously.
    #[cfg(test)]
    pub fn headless() -> Result<Gpu> {
        let bindings = baked_reflection().bindings.clone();
        let Device {
            instance,
            adapter,
            device,
            queue,
            features,
            shader_clock,
        } = Device::open(
            device::instance(),
            None,
            &bindings,
            "caustics-canvas-device",
        )?;
        let mut gpu = Gpu::bring_up(
            instance,
            adapter,
            device,
            queue,
            Presentation::HEADLESS,
            bindings,
            features.contains(wgpu::Features::TIMESTAMP_QUERY),
            shader_clock,
            std::sync::Arc::new(|| {}),
        )?;
        gpu.wait_ready()?;
        Ok(gpu)
    }

    /// Block until every pipeline is built.
    #[cfg(test)]
    pub fn wait_ready(&mut self) -> Result<()> {
        self.program
            .wait_ready()
            .context("the canvas caustic baked in at build time does not load")
    }

    #[allow(clippy::too_many_arguments)]
    fn bring_up(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        presentation: Presentation,
        bindings: Vec<Binding>,
        timing: bool,
        shader_clock: punchcutter_core::gpu::ShaderClock,
        wake: Wake,
    ) -> Result<Gpu> {
        let format = presentation.format;
        let program = Program::new_specialised(
            &device,
            &queue,
            bindings,
            &[
                "caustics-canvas-bgl",
                "caustics-canvas-text-bgl",
                "caustics-canvas-shape-bgl",
            ],
            "caustics-canvas-pipeline-layout",
            &|name| storage_texture_format(name, format),
            &CANVAS_ENTRY_TABLE,
            baked_spv(format)?,
            &NEED,
            &SPECIALISED,
            COMPILE_WORKERS,
            wake,
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
        let storage = |label: &str, size: usize| {
            punchcutter_core::gpu::storage_buffer(&device, label, size as u64)
        };
        let runs = storage(
            "caustics-canvas-runs",
            CANVAS_GLYPHS * SHAPED_GLYPH as usize,
        );
        let literals = storage(
            "caustics-canvas-literals",
            MAX_CANVAS_LITERALS * size_of::<Literal>(),
        );
        let strings = storage("caustics-canvas-strings", CANVAS_STRINGS_BYTES);
        let prompt_text = storage("caustics-canvas-prompt-text", PROMPT_TEXT_BYTES);
        // The dispatch half of `ShapeParams`: the canvas drives the pass from
        // its fixed blocks, so it carries their count and the slots.
        let text = TextBuffers::new(
            &device,
            "caustics-canvas",
            shader_clock,
            CANVAS_TEXT_BLOCKS as u32,
            CANVAS_GLYPHS as u32,
        );
        let text_group = text_group(&device, &program, &text);
        let shape_group = shape_group(&device, &program, &text, &strings);
        let software = adapter.get_info().device_type == wgpu::DeviceType::Cpu;
        let watchdog = Watchdog::new(
            &device,
            &queue,
            timing,
            software,
            "caustics-canvas",
            "canvas_shape",
            CANVAS_TEXT_BLOCKS as u32,
        );
        let shape_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-canvas-shape-params"),
            size: size_of::<CanvasParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shape_dst = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("caustics-canvas-shape-dst"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
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
            runs,
            literals,
            strings,
            strings_copy: Vec::new(),
            prompt_text,
            prompt_copy: Vec::new(),
            text,
            text_group,
            shape_group,
            watchdog,
            shape_pending: true,
            shape_params,
            shape_dst,
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
            None,
            PARAMS_LABEL,
            size_of::<CanvasParams>() as u64,
        )
    }

    /// Resize an output's swapchain after a configure or a scale change.
    pub fn resize(&self, target: &mut Surface, width: u32, height: u32) -> bool {
        target.resize(&self.device, width, height)
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

    /// Queue a freshly compiled generation, to be swapped in whole when its
    /// last pipeline is built ([`Landed::Swapped`] from
    /// [`Gpu::poll_pipelines`]), or refused, the running canvas kept either
    /// way. Returns its number.
    pub fn reload(&mut self, reflection: &Reflection, spirv: &[&[u8]]) -> Result<u64, Refused> {
        self.program.reload(reflection, spirv)
    }

    /// Install every pipeline that has landed and say what each did. A
    /// swapped generation shapes again, and its first pass is not judged; a
    /// pipeline of the running program that lands owes the text a pass,
    /// which is a no-op when nothing was waiting for it.
    pub fn poll_pipelines(&mut self) -> Vec<Landed> {
        let landed = self.program.poll();
        for news in &landed {
            match news {
                Landed::Swapped { .. } => {
                    self.watchdog.swapped();
                    self.shape_pending = true;
                }
                Landed::Live { .. } => self.shape_pending = true,
                Landed::Broken(_) | Landed::Refused { .. } => {}
            }
        }
        landed
    }

    /// Whether a frame can be drawn on a surface: `canvas_apply` and
    /// `canvas_draw` are built. The text needs `canvas_shape` as well and
    /// waits for it on its own; a frame before it draws no glyphs.
    ///
    /// Nor while a pipeline is being warmed: a frame submitted behind a
    /// warming dispatch waits for the driver's compile (`Program::warming`).
    pub fn can_draw(&self, role: u32) -> bool {
        self.program.has(&[APPLY]) && self.draw_pipeline(role).is_some() && !self.program.warming()
    }

    /// The `canvas_draw` a surface of `role` (`TARGET_*`) is drawn with: its
    /// own, specialised for it (`SURFACES`).
    fn draw_pipeline(&self, role: u32) -> Option<&wgpu::ComputePipeline> {
        self.program.variant(DRAW, role)
    }

    // -- the text ------------------------------------------------------------

    /// One run of a glyphs sidecar's bytes into the engine's buffer it names
    /// (docs/abi.md, *Bytes*), through `caustics_host::text`: the group a
    /// grown buffer is in rebuilt, the watchdog released by a rewritten
    /// front block, and the shape pass owed.
    pub fn write_bytes(&mut self, buffer: u32, offset: u32, total: u32, bytes: &[u8]) {
        let written =
            self.text
                .write_bytes(&self.device, &self.queue, buffer, offset, total, bytes);
        match written.regroup {
            Some(Group::Text) => {
                self.text_group = text_group(&self.device, &self.program, &self.text)
            }
            Some(Group::Shape) => {
                self.shape_group =
                    shape_group(&self.device, &self.program, &self.text, &self.strings)
            }
            None => {}
        }
        if written.front_block {
            self.watchdog.release();
        }
        if written.landed {
            self.shape_pending = true;
        }
    }

    /// The literal pool, from the running caustic's own table: every
    /// literal's bytes laid end to end in the strings buffer and its
    /// `(hash, off, len)` in `literals`, so the caustic can spell a word the
    /// host never learns the meaning of. Returns how many went in, which the
    /// caller puts in `facts`. A table past the buffers is cut, with a line.
    pub fn write_literals(&mut self, strings: &Strings) -> u32 {
        let mut bytes: Vec<u8> = Vec::new();
        let mut index: Vec<Literal> = Vec::new();
        for (hash, text) in strings.iter() {
            let len = text.len();
            if index.len() >= MAX_CANVAS_LITERALS || bytes.len() + len > CANVAS_STRINGS_BYTES {
                tracing::warn!(
                    "text: the literal pool is full at {} literals, {} bytes; the caustic \
                     cannot spell every literal it names",
                    index.len(),
                    bytes.len()
                );
                break;
            }
            index.push(Literal {
                hash,
                off: bytes.len() as u32,
                len: len as u32,
                _pad0: 0,
            });
            bytes.extend_from_slice(text.as_bytes());
            while bytes.len() % 4 != 0 {
                bytes.push(0);
            }
        }
        if !bytes.is_empty() {
            self.queue.write_buffer(&self.strings, 0, &bytes);
        }
        let mut table: Vec<u8> = Vec::with_capacity(index.len() * size_of::<Literal>());
        for literal in &index {
            table.extend_from_slice(as_bytes(literal));
        }
        if !table.is_empty() {
            self.queue.write_buffer(&self.literals, 0, &table);
        }
        self.strings_copy = bytes;
        self.shape_pending = true;
        index.len() as u32
    }

    /// The prompt's text, whole: what `facts.prompt`'s ranges name. At most
    /// `PROMPT_TEXT_BYTES`, which the caller's arena keeps to. Never a
    /// secret (docs/abi.md, *The canvas's buffer*).
    /// The same text again writes nothing and shapes nothing: a key typed
    /// into an echo-off ask changes a count, not the text.
    pub fn write_prompt_text(&mut self, bytes: &[u8]) {
        let bytes = &bytes[..bytes.len().min(PROMPT_TEXT_BYTES)];
        if bytes == self.prompt_copy.as_slice() {
            return;
        }
        let mut padded = bytes.to_vec();
        while padded.len() % 4 != 0 {
            padded.push(0);
        }
        if !padded.is_empty() {
            self.queue.write_buffer(&self.prompt_text, 0, &padded);
        }
        self.prompt_copy = bytes.to_vec();
        self.shape_pending = true;
    }

    /// The bytes of a run the caustic asked to have covered, from this
    /// side's own copy of the buffer it names (`TEXT_*`), or `None` for a
    /// range that is not in it or not UTF-8.
    pub fn cover_bytes(&self, source: u32, off: u32, len: u32) -> Option<String> {
        let held = match source {
            TEXT_LITERALS => &self.strings_copy,
            TEXT_PROMPT => &self.prompt_copy,
            _ => return None,
        };
        let start = off as usize;
        let end = start.checked_add(len as usize)?;
        let bytes = held.get(start..end)?;
        String::from_utf8(bytes.to_vec()).ok()
    }

    /// Whether the watchdog has stopped the shape pass.
    #[cfg(test)]
    pub fn shape_blocked(&self) -> bool {
        self.watchdog.blocked()
    }

    /// `canvas_shape`, when it is owed and the watchdog allows: its own
    /// submission, waited out before the frame's, because the frame's draw
    /// reads what it wrote and two submissions in flight have no order a host
    /// can rely on. The watchdog's pair goes round this pass and no other:
    /// it is the one that walks bytes a sidecar sent.
    fn shape_if_pending(&mut self) {
        if !self.shape_pending || self.watchdog.blocked() || self.program.warming() {
            return;
        }
        // Not built yet: still owed, and run when it lands.
        let Some(shape) = self.program.pipeline(SHAPE) else {
            return;
        };
        let shape = shape.clone();
        self.shape_pending = false;
        let group = self.bind_group(&self.shape_params, &self.shape_dst);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-canvas-shape"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-canvas-shape-pass"),
                timestamp_writes: self.watchdog.timestamp_writes(),
            });
            pass.set_bind_group(SET_CANVAS, &group, &[]);
            pass.set_bind_group(SET_TEXT, &self.text_group, &[]);
            pass.set_bind_group(SET_SHAPE, &self.shape_group, &[]);
            pass.set_pipeline(&shape);
            pass.dispatch_workgroups(
                (CANVAS_TEXT_BLOCKS as u32).div_ceil(SHAPE_BLOCKS_PER_GROUP),
                1,
                1,
            );
        }
        self.watchdog.resolve(&mut encoder);
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        // A trip leaves the pass owed, so the release shapes again.
        if self.watchdog.check(&self.device) {
            self.shape_pending = true;
        }
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

    /// Draw one frame on one surface into the image [`Gpu::acquire`] gave,
    /// as `role` (`TARGET_*`) on pane `pane` (`CanvasParams::pane`), present
    /// it and read the state back. Returns the frame stamp the witness ring
    /// carries.
    pub fn draw(
        &mut self,
        target: &mut Surface,
        frame: surface::Frame,
        scale: u32,
        role: u32,
        pane: u32,
    ) -> Result<u32> {
        let view = frame
            .texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let (width, height) = target.size();
        self.shape_if_pending();
        self.dispatch(&target.params, &view, width, height, scale, role, pane);
        self.queue.present(frame.texture);
        if frame.suboptimal {
            target.refit_once(&self.device);
        }
        self.finish()
    }

    /// One frame's work: the uniform, both entry points, the copy back.
    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &self,
        params: &wgpu::Buffer,
        dst: &wgpu::TextureView,
        width: u32,
        height: u32,
        scale: u32,
        role: u32,
        pane: u32,
    ) {
        let (now_lo, now_hi) = clock::words(clock::boottime_ms());
        self.write_params(params, width, height, scale, now_lo, now_hi, role, pane);
        self.encode(params, dst, width, height, role);
    }

    #[allow(clippy::too_many_arguments)]
    fn write_params(
        &self,
        params: &wgpu::Buffer,
        width: u32,
        height: u32,
        scale: u32,
        now_lo: u32,
        now_hi: u32,
        role: u32,
        pane: u32,
    ) {
        let words = CanvasParams {
            width,
            height,
            scale,
            frame: self.frame,
            now_lo,
            now_hi,
            target: role,
            pane,
        };
        self.queue.write_buffer(params, 0, as_bytes(&words));
    }

    fn encode(
        &self,
        params: &wgpu::Buffer,
        dst: &wgpu::TextureView,
        width: u32,
        height: u32,
        role: u32,
    ) {
        let (Some(apply), Some(draw)) = (self.program.pipeline(APPLY), self.draw_pipeline(role))
        else {
            // `can_draw` is the caller's check, and an image acquired without
            // it is presented with whatever it held.
            tracing::warn!("canvas: a frame was asked for before its pipelines were built");
            return;
        };
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
            pass.set_bind_group(SET_CANVAS, &group, &[]);
            pass.set_bind_group(SET_TEXT, &self.text_group, &[]);
            pass.set_bind_group(SET_SHAPE, &self.shape_group, &[]);
            // Two dispatches, two usage scopes: the barrier between them is
            // what lets `canvas_draw` read the alpha `canvas_apply` wrote.
            pass.set_pipeline(apply);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(draw);
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
        // The caustic's threshold for its shape pass, as it said it this
        // frame: one number in.
        if self.mirror.shape_watchdog_ms > 0 {
            self.watchdog
                .set_threshold_ms(self.mirror.shape_watchdog_ms);
        }
        let drawn = self.frame;
        self.frame = self.frame.wrapping_add(1).max(1);
        Ok(drawn)
    }

    /// The bind group for one output's frame, **by name** from the
    /// reflection (docs/abi.md, *Bindings and the oracle*): no binding number
    /// is written on this side.
    fn bind_group(&self, params: &wgpu::Buffer, dst: &wgpu::TextureView) -> wgpu::BindGroup {
        let entries = group_entries(self.program.bindings(), SET_CANVAS, |name| match name {
            "state" => self.state.as_entire_binding(),
            "params" => params.as_entire_binding(),
            "facts" => self.facts.as_entire_binding(),
            "dst" => wgpu::BindingResource::TextureView(dst),
            "image_a" => wgpu::BindingResource::TextureView(&self.images[0].view),
            "image_b" => wgpu::BindingResource::TextureView(&self.images[1].view),
            "tex_sampler" => wgpu::BindingResource::Sampler(&self.sampler),
            "private" => self.private.as_entire_binding(),
            "runs" => self.runs.as_entire_binding(),
            "literals" => self.literals.as_entire_binding(),
            "prompt_text" => self.prompt_text.as_entire_binding(),
            other => panic!("{}", unknown_binding(other)),
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("caustics-canvas-bg"),
            layout: self.program.layout(SET_CANVAS),
            entries: &entries,
        })
    }
}

/// Every binding name the canvas's own group knows, in the order
/// `bind_group`'s match lists them.
pub const BOUND: [&str; 11] = [
    "state",
    "params",
    "facts",
    "dst",
    "image_a",
    "image_b",
    "tex_sampler",
    "private",
    "runs",
    "literals",
    "prompt_text",
];

/// The text engine's names in sets 1 and 2 (`TextBuffers::resource`), and
/// the strings buffer behind `shapeUtf8`: the bar's names, so one table of
/// them serves both hosts.
pub const ENGINE_BOUND: [&str; 7] = [
    "textPoints",
    "textBands",
    "textBandCurves",
    "textGlyphs",
    "shapeParams",
    "shapeOt",
    "shapeUtf8",
];

/// Group 1: `text.slang`'s four curve buffers, by name.
fn text_group(device: &wgpu::Device, program: &Program, text: &TextBuffers) -> wgpu::BindGroup {
    let entries = group_entries(program.bindings(), SET_TEXT, |name| {
        text.resource(name)
            .unwrap_or_else(|| panic!("{}", unknown_binding(name)))
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("caustics-canvas-text"),
        layout: program.layout(SET_TEXT),
        entries: &entries,
    })
}

/// Group 2: the shaper's tables, and the literal pool's bytes as its strings.
fn shape_group(
    device: &wgpu::Device,
    program: &Program,
    text: &TextBuffers,
    strings: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let entries = group_entries(program.bindings(), SET_SHAPE, |name| match name {
        "shapeUtf8" => strings.as_entire_binding(),
        other => text
            .resource(other)
            .unwrap_or_else(|| panic!("{}", unknown_binding(other))),
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("caustics-canvas-shape"),
        layout: program.layout(SET_SHAPE),
        entries: &entries,
    })
}

fn unknown_binding(name: &str) -> String {
    format!(
        "the canvas caustic declares a binding `{name}` this host has no resource for; \
         canvas/src/gpu.rs binds {BOUND:?} and {ENGINE_BOUND:?}"
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
