//! The bar's GPU side: two surfaces, seven pipelines, three bind group layouts.
//!
//! The mechanism is `caustics-host`'s: the device and its refusals
//! (`caustics_host::device`), the reflection-driven layouts, the pipelines and
//! the reload swap (`caustics_host::program`), and the swapchain of one target
//! (`caustics_host::surface`), and the text engine's buffers with the
//! watchdog round the pass that walks them (`caustics_host::text`). What is
//! here is what makes them this bar: which buffers it allocates and binds by
//! which name, the seven entry points and the order they dispatch in, and
//! when the shape pass runs.
//!
//! Three shapes here are deliberate:
//!
//! - **passthrough SPIR-V.** The caustic is Slang; wgpu takes the module
//!   through `create_shader_module_passthrough`, which performs no validation
//!   of its own, so every pipeline build sits inside a validation error scope
//!   and a reload is all-or-nothing ([`Gpu::reload_all`]).
//! - **the mirror.** The caustic owns [`State`]; the CPU reads it one frame
//!   late out of a `MAP_READ` readback buffer, through `caustics_host::surface::refresh_mirror`. Nothing on the CPU ever writes it, and it is the *only* buffer read back at all (docs/abi.md, *Who writes what*).
//! - **format variants.** Storage-image formats are baked into SPIR-V, so the
//!   surface format decides which of build.rs's two sets of seven modules loads.
//!
//! What is new here is *two targets*. The bar and its popup are separate
//! Wayland surfaces and therefore separate swapchains, but they share one
//! state buffer, one command ring, one set of fonts and one bind-group layout:
//! the per-target resources are the ids texture, the destination image and the
//! `Params` uniform, and everything else is bound identically for both. One
//! `bar_shape` and one `bar_apply` serve both targets; each target then gets
//! its own visibility → sample → draw.
//!
//! # Three bind groups, and where they come from
//!
//! Group 0 is the bar's own.
//! Group 1 is the text engine's `text.slang`'s four curve buffers, whose binding numbers
//! that module declares for itself — which is what let it be written and tested
//! before anything imported it. Group 2 is the engine's `shape.slang`'s font tables plus
//! the strings buffer; that module declares no bindings at all, so the numbers
//! there are `bar_state.slang`'s choice and `shape_tables()` in it is the gather.
//!
//! **The layouts are built from the compiler's reflection**, not from a list here
//! ([`baked_reflection`]), so there is no second copy of every binding here to
//! keep dense, in order, and in agreement with the `[[vk::binding(n, g)]]`
//! literals in the caustics. The caustics are the one declaration; what is
//! left here is the two things a reflection cannot
//! carry, both keyed on the parameter's name: a storage image's texel format
//! ([`storage_texture_format`]) and which buffer to bind.
//!
//! **And the bind *groups* are built the same way**
//! (`caustics_host::program::group_entries`, docs/abi.md, *Bindings and the oracle*): each entry takes its number from
//! the reflected [`Binding`] and its resource from a `match` on that binding's
//! name, so no number is written on this side at all. A binding added in Slang
//! needs one arm here and nothing else; a binding that moves moves in one file.
//!
//! Fifteen storage buffers across the three, which is well past wgpu's default
//! per-stage limit of eight — `caustics_host::device::Device::open` raises it
//! to whatever the adapter allows and says so, counting from the reflection.

use std::ptr::NonNull;

use anyhow::{Context, Result, anyhow, bail};
use caustics_host::compile::Wake;
use caustics_host::device::{self, Device, Presentation};
use caustics_host::program::{Entry, Landed, Program, Refused, group_entries};
use caustics_host::surface::{self, ID_FORMAT, Surface, acquire, create_surface, refresh_mirror};
use caustics_host::text::{Group, TextBuffers, Watchdog, growable_buffer};
use punchcutter_core::gpu::storage_buffer;
use punchcutter_slang::{Binding, BindingKind, Reflection};

use crate::abi::{self, Command, FrameInputs, Params, State};

/// The debug label on this bar's visibility buffer.
const IDS_LABEL: &str = "caustics-bar-ids";

/// Both are Rgba8Unorm: the image slots hold straight alpha, the preview slot
/// premultiplied.
const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Workgroup edge of the two full-target entries (`[numthreads(8, 8, 1)]`).
const TILE: u32 = 8;

/// Threads per workgroup in `bar_shape` (`[numthreads(1, 1, 1)]`) — one block
/// per *group*, and the note beside that attribute in `caustics/bar.slang` says
/// why it is not 64: the pass is small, and a group of one is a free margin against driver trouble.
const SHAPE_GROUP: u32 = 1;

/// How many `bar_shape` threads a submission dispatches: one per slot of the
/// caustic's own block table.
///
/// The cap is `caustics/bar_model.slang`'s `MAX_BLOCKS`, and the ABI describes no word of the caustic's memory (docs/abi.md, *The private buffer*) — so
/// this is a number the embassy has to *cover* rather than know. A thread past
/// `live_blocks()` returns on its first line, so over-dispatching costs a
/// handful of immediately-retired threads; under-dispatching would leave the
/// tail of the table unshaped, which is why
/// `the_shape_dispatch_covers_the_caustics_block_table` in `bar/src/abi/tests.rs`
/// reads the caustic's own number out of the module's text and checks this one
/// against it.
pub const SHAPE_BLOCKS: u32 = 512;

// The seven entry points' positions in `abi::BAR_ENTRIES`, the one list build.rs
// bakes and `caustics-reload` recompiles: a pipeline is named by its index in
// that list, which is also the order `reload_all` takes the modules in.
const BUILD: usize = 0;
const COMPOSE: usize = 1;
const SHAPE: usize = 2;
const APPLY: usize = 3;
const VISIBILITY: usize = 4;
const SAMPLE: usize = 5;
const DRAW: usize = 6;

/// The order the pipelines are built in, which is the order of need: the
/// frame submission's four first, so the slab is drawn as soon as they land;
/// then the model passes, so the chips are; the shaper, most of the compile,
/// last, so the text comes in after everything else is already on screen.
const NEED: [usize; abi::BAR_ENTRIES.len()] =
    [APPLY, VISIBILITY, SAMPLE, DRAW, BUILD, COMPOSE, SHAPE];

/// What a frame submission dispatches.
const FRAME_PIPELINES: [usize; 4] = [APPLY, VISIBILITY, SAMPLE, DRAW];

/// The compile workers' count (`caustics_host::compile`). Two: on RADV with
/// a cold cache the shaper builds beside the rest instead of after it, which
/// is the bar's whole program in 11.7 s rather than 16.2 s with its chips no
/// later; the canvas's are the shaper and two small ones, and come in no
/// slower. A third bought the bar half a second.
const COMPILE_WORKERS: usize = 2;

/// The seven entry points, in `abi::BAR_ENTRIES` order: each one's name, its
/// pipeline's label, and its `[numthreads]`.
static ENTRY_POINTS: [Entry; abi::BAR_ENTRIES.len()] = [
    Entry {
        name: abi::BAR_ENTRIES[BUILD],
        label: "caustics-bar-build",
        workgroup: (1, 1, 1),
    },
    Entry {
        name: abi::BAR_ENTRIES[COMPOSE],
        label: "caustics-bar-compose",
        workgroup: (1, 1, 1),
    },
    Entry {
        name: abi::BAR_ENTRIES[SHAPE],
        label: "caustics-bar-shape",
        workgroup: (SHAPE_GROUP, 1, 1),
    },
    Entry {
        name: abi::BAR_ENTRIES[APPLY],
        label: "caustics-bar-apply",
        workgroup: (1, 1, 1),
    },
    Entry {
        name: abi::BAR_ENTRIES[VISIBILITY],
        label: "caustics-bar-visibility",
        workgroup: (TILE, TILE, 1),
    },
    Entry {
        name: abi::BAR_ENTRIES[SAMPLE],
        label: "caustics-bar-sample",
        workgroup: (1, 1, 1),
    },
    Entry {
        name: abi::BAR_ENTRIES[DRAW],
        label: "caustics-bar-draw",
        workgroup: (TILE, TILE, 1),
    },
];

/// The seven baked modules for one swapchain format, in `abi::BAR_ENTRIES` order.
type BakedSpv = [&'static [u8]; abi::BAR_ENTRIES.len()];

const SPV_RGBA8UNORM: BakedSpv = [
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_build.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_compose.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_shape.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_apply.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_visibility.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_sample.rgba8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_draw.rgba8unorm.spv")),
];

const SPV_BGRA8UNORM: BakedSpv = [
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_build.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_compose.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_shape.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_apply.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_visibility.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_sample.bgra8unorm.spv")),
    include_bytes!(concat!(env!("OUT_DIR"), "/bar_draw.bgra8unorm.spv")),
];

/// Which target a surface is. The values are the ABI's `TARGET_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Bar,
    Popup,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Bar => "bar",
            Target::Popup => "popup",
        }
    }
}

pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The negotiated swapchain format, alpha mode and present mode; both
    /// surfaces use them, and the format decides which SPIR-V variant the
    /// pipelines were built from.
    presentation: Presentation,

    /// The three layouts (group 0, the bar's own; group 1, the engine's
    /// `text.slang`'s four curve buffers at the bindings that module declares
    /// for itself; group 2, the engine's `shape.slang`'s font tables and the
    /// strings buffer at the bindings `bar.slang` chose for them), the
    /// pipeline layout over them, the seven pipelines, and the bindings they
    /// were built from, which a reload is checked against.
    program: Program,

    /// The one buffer the embassy reads back (docs/abi.md, *Who writes what*): the
    /// caustic's polled words and its two rings. Written by the caustic and
    /// **never** by this side — wgpu zero-initialises it at creation, which is
    /// the whole of its initialisation.
    state: wgpu::Buffer,
    /// Every shaped glyph on the bar, written by `bar_shape` and read by
    /// `bar_draw`. Private in the same sense as `private`: allocated here,
    /// never written or read here.
    runs: wgpu::Buffer,
    commands: wgpu::Buffer,
    /// **What the caustic keeps for itself** (docs/abi.md, *The private buffer*): the
    /// element, item and block tables, the scratch, the springs, the popup's
    /// want, the witness memory. One element of the stride Slang's reflection
    /// reports for `Private`, `STORAGE` only — no `COPY_SRC`, no `COPY_DST`,
    /// so neither a readback nor a `write_buffer` can be written against it by
    /// accident.
    private: wgpu::Buffer,
    /// The samples frame (binding 10): one [`Samples`] as a
    /// sidecar wrote it. The bar copies the bytes in and reads none of them —
    /// every chart on the strip is drawn out of this buffer by the caustic.
    samples: wgpu::Buffer,
    /// The literal pool (binding 11): every `getStringHash` literal
    /// the running caustic names, as `(hash, offset, len)` into the strings
    /// buffer. Rebuilt with the content, and read by `bar_compose` alone.
    literals: wgpu::Buffer,
    /// **What the embassy knows** (docs/abi.md, *The facts*): the facts tables
    /// `bar_build` turns into the item and block tables. 10.5 KiB, written
    /// whole by `upload_facts`.
    facts: wgpu::Buffer,
    /// **The rows arena** (docs/abi.md, *The arenas*): `MAX_SLOTS` regions of `ROWS_ARENA_BYTES`,
    /// each holding one sidecar's last `Rows` body exactly as it crossed the
    /// wire. Written one region at a time by `write_rows_region`; the embassy
    /// parses none of it.
    rows: wgpu::Buffer,
    /// **The answers arena** (docs/abi.md, *The arenas*): every `Glyph` and `App` body the embassy
    /// has received, concatenated, written whole with the facts.
    answers: wgpu::Buffer,
    inputs: wgpu::Buffer,
    mirror_readback: wgpu::Buffer,

    /// Every string on the bar, UTF-8, four bytes per word. A `TextBlock` names
    /// a byte range of this and nothing else.
    strings: wgpu::Buffer,
    /// The text engine's six buffers (`caustics_host::text`): the curve
    /// buffers of group 1 and the shaper's tables of group 2, filled by
    /// `caustics-glyphs`'s `Bytes`. Each group is rebuilt only when a buffer
    /// in it has outgrown its headroom.
    text: TextBuffers,
    text_group: wgpu::BindGroup,
    shape_group: wgpu::BindGroup,

    /// A 1x1 texture of the swapchain's format, for the shape pass's bind
    /// group: group 0's layout declares a storage image (`dst`) and
    /// `bar_shape` writes no pixels, so it is given somewhere harmless rather
    /// than a swapchain image it has no business holding.
    shape_dst: wgpu::Texture,
    /// Set when the blocks changed and the shape pass owes them their metrics.
    shape_pending: bool,
    /// The watchdog round `bar_shape` (`caustics_host::text::Watchdog`): the
    /// device's own timing of the pass, the threshold the caustic pushed, and
    /// the stop a trip holds until the arena's front block is written again.
    watchdog: Watchdog,
    /// What the last build/compose/shape submission cost, until the mirror
    /// carrying what it built comes back and the two are logged together.
    model_pass_ms: Option<f64>,
    /// `Params.cap_height`, in em; set by `set_cap_height` when a font is
    /// known and 0 before.
    cap_height: f32,

    /// The image slots: one [`abi::IMAGE_SLOT_PX`]² layer per slot.
    images: wgpu::Texture,
    images_view: wgpu::TextureView,
    tex_sampler: wgpu::Sampler,

    /// The one preview slot (binding 9). A fixed
    /// [`abi::PREVIEW_MAX_DIM`]² texture whose valid extent rides in
    /// `FrameInputs::preview_size`: the capture feeder runs one session at a
    /// time, so one slot is enough, and a fixed size keeps a reallocation out
    /// of the hover path.
    ///
    /// The handle is held rather than dropped because it is what
    /// [`Gpu::upload_preview`]'s `write_texture` needs; only the view is bound.
    preview: wgpu::Texture,
    preview_view: wgpu::TextureView,

    bar: Surface,
    popup: Option<Surface>,

    /// The state buffer as the CPU last saw it. Boxed because it is 21 KiB
    /// and gets handed around by reference every frame.
    mirror: Box<State>,
    /// `bar_check`, the test-only eighth entry point, once the oracle has
    /// compiled and loaded one (docs/abi.md, *Bindings and the oracle*). `None` in every other
    /// build.
    #[cfg(test)]
    check: Option<wgpu::ComputePipeline>,
    /// The frame this device is on (docs/abi.md, *The three rules the buffer adds*, rule 1). Starts at 1,
    /// stamped into `FrameInputs` before the first dispatch of a frame, and
    /// advanced **only after that frame's readback has completed** — so a
    /// frame that failed at the swapchain keeps its stamp and the next attempt
    /// appends to the rings it already pushed into.
    frame: u32,
}

impl Gpu {
    /// Bring wgpu up on the bar's `wl_surface`.
    ///
    /// # Safety
    ///
    /// `display` and `surface` must be a live `wl_display` and `wl_surface`
    /// that outlive the returned [`Gpu`]. The caller (`wayland.rs`) keeps the
    /// connection and the layer surface for the life of the process, and
    /// drops the `Gpu` before either.
    ///
    /// The pipelines are not built when this returns: they are queued on the
    /// compile workers, `wake` is called each time one lands, and the host
    /// installs it with [`Gpu::poll_pipelines`]. A frame before the frame
    /// submission's are built is not drawn ([`Gpu::can_render`]).
    pub unsafe fn new(
        display: NonNull<std::ffi::c_void>,
        surface: NonNull<std::ffi::c_void>,
        width: u32,
        height: u32,
        wake: Wake,
    ) -> Result<Self> {
        let instance = device::instance();
        // SAFETY: the caller's contract, above.
        let bar_surface = unsafe { create_surface(&instance, display, surface)? };
        Gpu::bring_up(instance, Some(bar_surface), width, height, wake)
    }

    /// The same device, the same buffers and the same pipelines with **no
    /// surface**: what the oracle runs the caustic on.
    ///
    /// Nothing to present to and nothing to be compatible with, so the adapter
    /// is chosen the way the text engine's harnesses choose one — the first
    /// integrated or CPU adapter whose name does not say NVIDIA — and the
    /// format is fixed rather than negotiated. Everything after that is the
    /// production path, which is the whole point: the oracle tests what ships.
    ///
    /// Every pipeline is built before this returns, so the oracle runs them
    /// synchronously.
    #[cfg(test)]
    pub fn headless(width: u32, height: u32) -> Result<Self> {
        let mut gpu = Gpu::bring_up(
            device::instance(),
            None,
            width,
            height,
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
            .context("the bar caustic baked in at build time does not load")
    }

    fn bring_up(
        instance: wgpu::Instance,
        bar_surface: Option<wgpu::Surface<'static>>,
        width: u32,
        height: u32,
        wake: Wake,
    ) -> Result<Self> {
        // The device, its refusals and its exit on loss are the crate's
        // (`caustics_host::device`). Its storage-buffer limit is counted from
        // the reflection of the caustic itself, so a binding added in Slang is
        // counted without anyone editing this file.
        let reflected = baked_bindings();
        let Device {
            instance,
            adapter,
            device,
            queue,
            shader_clock: clock,
            features,
        } = Device::open(
            instance,
            bar_surface.as_ref(),
            &reflected,
            "caustics-bar-device",
        )?;
        let timing = features.contains(wgpu::Features::TIMESTAMP_QUERY);

        // What the surface will take, when there is one. Headless — the
        // oracle — fixes all three instead.
        let presentation = match bar_surface.as_ref() {
            Some(negotiate) => Presentation::negotiate(negotiate, &adapter, features)?,
            None => Presentation::HEADLESS,
        };
        let format = presentation.format;

        // Three layouts from the reflection and the pipeline layout over
        // them, now; the seven pipelines queued on the compile workers in the
        // order of need, each in a validation scope of its own
        // (`caustics_host::program`). The sets the reflection declares must
        // be exactly these three, or this stops here.
        let program = Program::new(
            &device,
            &queue,
            reflected,
            &[
                "caustics-bar-bgl",
                "caustics-bar-text-bgl",
                "caustics-bar-shape-bgl",
            ],
            "caustics-bar-pipeline-layout",
            &|name| storage_texture_format(name, format),
            &ENTRY_POINTS,
            baked_spv(format),
            &NEED,
            COMPILE_WORKERS,
            wake,
        )
        .context("the bar caustic baked in at build time does not load")?;
        let reflected = program.bindings();

        let state = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-state"),
            size: abi::STATE_BUFFER_SIZE,
            // COPY_SRC for the mirror copy, and nothing else: the caustic owns
            // every byte and wgpu zero-initialises a buffer at creation, which
            // is the whole of its initialisation (docs/abi.md, *Who writes what*).
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // What the caustic keeps for itself, at the stride Slang's reflection
        // reports for `Private` (docs/abi.md, *The private buffer*). `STORAGE` alone: no
        // readback, no upload, and the usage flags say so.
        let private = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-private"),
            size: private_stride(reflected),
            // `COPY_DST` only in a test build, where `set_menu_level` writes
            // one field of it so a scenario can name an open menu without a
            // click. A shipped bar cannot write this buffer at all, and the
            // usage flags are where that is enforced rather than promised.
            // `COPY_SRC` likewise: the layout test reads the element table
            // back.
            usage: if cfg!(test) {
                wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC
            } else {
                wgpu::BufferUsages::STORAGE
            },
            mapped_at_creation: false,
        });
        // The buffer the shape pass writes and nothing on the CPU reads. Its
        // stride is the ABI's rather than the reflection's because it is a
        // module's struct (`punchcutter_shape::ShapedGlyph`) and not the bar's.
        // `COPY_SRC` for the oracle's readback alone (`Gpu::read_runs`): nothing in
        // production copies out of this buffer.
        let runs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-runs"),
            size: abi::GLYPH_BUFFER_SIZE,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let strings = growable_buffer(&device, "caustics-bar-strings", abi::STRINGS_BUFFER_SIZE);
        let commands = storage_buffer(&device, "caustics-bar-commands", abi::COMMAND_RING_SIZE);
        let samples = storage_buffer(
            &device,
            "caustics-bar-samples",
            abi::wire::SAMPLES_SIZE as u64,
        );
        let literals = storage_buffer(&device, "caustics-bar-literals", abi::LITERALS_BUFFER_SIZE);
        let facts = storage_buffer(&device, "caustics-bar-facts", abi::FACTS_BUFFER_SIZE);
        // The two arenas (docs/abi.md, *The arenas*): what sidecars sent, byte for
        // byte. The embassy writes them and reads nothing back; the caustic
        // reads them through the wire's own structs.
        let rows = storage_buffer(&device, "caustics-bar-rows", abi::ROWS_BUFFER_SIZE);
        let answers = storage_buffer(&device, "caustics-bar-answers", abi::ANSWERS_BUFFER_SIZE);
        let inputs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-inputs"),
            size: size_of::<FrameInputs>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mirror_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-mirror-readback"),
            size: abi::STATE_BUFFER_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // The engine's six buffers start as small as a legal binding:
        // nothing can be drawn until `caustics-glyphs`'s first `Bytes` lands,
        // and `write_bytes` grows each to the `total` its writer declares.
        // `request_count` and `glyph_capacity` are the batch's, and this bar
        // drives the pass from the block table instead, so they carry the
        // capacities, which is what a reader of the buffer in a debugger
        // wants to see.
        let text = TextBuffers::new(
            &device,
            "caustics-bar",
            clock,
            SHAPE_BLOCKS,
            abi::MAX_GLYPHS as u32,
        );
        let text_group = create_text_group(&device, program.layout(SET_TEXT), reflected, &text);
        let shape_group = create_shape_group(
            &device,
            program.layout(SET_SHAPE),
            reflected,
            &text,
            &strings,
        );

        // Two timestamps round `bar_shape`: the device's own answer to "how
        // long did it run".
        let software = adapter.get_info().device_type == wgpu::DeviceType::Cpu;
        let watchdog = Watchdog::new(
            &device,
            &queue,
            timing,
            software,
            "caustics-bar",
            "bar_shape",
            SHAPE_BLOCKS,
        );

        let shape_dst = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("caustics-bar-shape-dst"),
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
        });
        let (images, images_view) = create_images(&device);
        let (preview, preview_view) = create_preview(&device);
        let tex_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("caustics-bar-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bar = configure_surface(
            &device,
            bar_surface,
            &presentation,
            width,
            height,
            Target::Bar,
        );

        // **Nothing is zeroed here.** wgpu zero-initialises every buffer at
        // creation, and the bar leans on that deliberately (docs/abi.md, *Who writes what*):
        // the state buffer and `private` are the caustic's and `ensure_private`
        // writes the non-zero defaults on its first pass; the run, item and
        // block tables are inside `private` or are read only against a count
        // the caustic wrote; a zeroed samples body reads as no charts; zeroed
        // facts read as no windows and no apps; and the strings buffer's
        // composed region has to be zero before `bar_compose` fills it, which it is by construction.

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            presentation,
            program,
            state,
            runs,
            commands,
            private,
            samples,
            rows,
            answers,
            inputs,
            mirror_readback,
            strings,
            literals,
            facts,
            text,
            text_group,
            shape_group,
            shape_dst,
            shape_pending: false,
            watchdog,
            model_pass_ms: None,
            cap_height: 0.0,
            images,
            images_view,
            tex_sampler,
            preview,
            preview_view,
            bar,
            popup: None,
            mirror: Box::new(State::ZERO),
            #[cfg(test)]
            check: None,
            // ≥ 1, because 0 is the ring stamp that means "no frame"
            // (docs/abi.md, *The three rules the buffer adds*, rule 1).
            frame: 1,
        })
    }

    /// The define and the format-unknown switch matching the live swapchain
    /// format, for a compile on this device (the oracle's). Mirrors build.rs, which compiles one set per `DstFormat::ALL` with `format.args()`.
    pub fn dst_format_args(&self) -> (&'static str, bool) {
        surface::dst_format(self.presentation.format)
            .unwrap_or_else(|| {
                unreachable!(
                    "surface format {:?} is not one this bar can select",
                    self.presentation.format
                )
            })
            .args()
    }

    /// The font's cap height in em, published to the caustic in `Params` on
    /// every frame (docs/abi.md, *Font*).
    pub fn set_cap_height(&mut self, em: f32) {
        self.cap_height = em;
    }

    /// The state buffer as of the last readback (docs/abi.md, *Who writes what*).
    ///
    /// `render` hands the same reference back, so nothing in the frame loop
    /// has a reason to ask; this is for a reader of the buffer in a debugger.
    #[allow(dead_code)]
    pub fn mirror(&self) -> &State {
        &self.mirror
    }

    pub fn bar_size(&self) -> (u32, u32) {
        self.bar.size()
    }

    pub fn popup_size(&self) -> (u32, u32) {
        match &self.popup {
            Some(popup) => popup.size(),
            None => (0, 0),
        }
    }

    /// Resize the bar's swapchain (and its ids texture) after a configure.
    pub fn resize_bar(&mut self, width: u32, height: u32) {
        self.bar.resize(&self.device, width, height);
    }

    /// Attach the popup's `wl_surface` as the second target.
    ///
    /// # Safety
    ///
    /// As [`Gpu::new`]: the handles must outlive the surface, which
    /// `wayland.rs` guarantees by destroying the wgpu surface (via
    /// [`Gpu::drop_popup`]) before the `xdg_popup`.
    pub unsafe fn attach_popup(
        &mut self,
        display: NonNull<std::ffi::c_void>,
        surface: NonNull<std::ffi::c_void>,
        width: u32,
        height: u32,
    ) -> Result<()> {
        // SAFETY: the caller's contract, above.
        let wgpu_surface = unsafe { create_surface(&self.instance, display, surface)? };
        let caps = wgpu_surface.get_capabilities(&self.adapter);
        if !caps.formats.contains(&self.presentation.format) {
            bail!(
                "the popup surface does not offer the bar's format {:?} (offers {:?})",
                self.presentation.format,
                caps.formats
            );
        }
        self.popup = Some(configure_surface(
            &self.device,
            Some(wgpu_surface),
            &self.presentation,
            width,
            height,
            Target::Popup,
        ));
        Ok(())
    }

    /// Resize the popup's swapchain after a reposition's configure. A menu
    /// that descends into a submenu changes size without being destroyed, so
    /// this is the popup's `resize_bar`.
    pub fn resize_popup(&mut self, width: u32, height: u32) -> bool {
        let Some(popup) = self.popup.as_mut() else {
            return false;
        };
        popup.resize(&self.device, width, height)
    }

    /// Drop the popup's swapchain. Must happen before the `xdg_popup` and its
    /// `wl_surface` are destroyed — the surface holds raw pointers to both.
    pub fn drop_popup(&mut self) {
        self.popup = None;
    }

    /// Put one image in its slot: exactly [`abi::IMAGE_SLOT_PX`]² straight RGBA, at a layer of the array.
    ///
    /// There is no
    /// dirty rectangle and no `bytes_per_row` arithmetic because a slot is a
    /// whole image of a known size: `caustics_host::images::Images` hands over the slots
    /// whose pixels changed and this writes each one.
    pub fn upload_image_slot(&mut self, slot: u32, rgba_premul: &[u8]) {
        let px = abi::IMAGE_SLOT_PX;
        let expected = (px as usize) * (px as usize) * 4;
        if slot as usize >= abi::MAX_IMAGE_SLOTS || rgba_premul.len() != expected {
            tracing::warn!(
                "gpu: refusing slot {slot} with {} bytes (want {expected} in \
                 0..{})",
                rgba_premul.len(),
                abi::MAX_IMAGE_SLOTS
            );
            return;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.images,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            rgba_premul,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(px * 4),
                rows_per_image: Some(px),
            },
            wgpu::Extent3d {
                width: px,
                height: px,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Put one run of a sidecar's bytes into the buffer it names
    /// (docs/abi.md, *Bytes*): the one method for all six, which is
    /// `caustics_host::text::TextBuffers::write_bytes`.
    ///
    /// The font is read by a sidecar, not the bar, and nothing here reads a
    /// byte of the run. What is the bar's is what follows a write: the one
    /// group naming a grown buffer rebuilt, the watchdog released when the
    /// arena's front block is rewritten (a sidecar that restarts sends the
    /// whole arena from offset zero, so this is also what a fixed
    /// `caustics-glyphs` looks like from here), and the shape pass owed.
    pub fn write_bytes(&mut self, buffer: u32, offset: u32, total: u32, bytes: &[u8]) {
        let written =
            self.text
                .write_bytes(&self.device, &self.queue, buffer, offset, total, bytes);
        match written.regroup {
            Some(Group::Text) => {
                self.text_group = create_text_group(
                    &self.device,
                    self.program.layout(SET_TEXT),
                    self.program.bindings(),
                    &self.text,
                );
            }
            Some(Group::Shape) => {
                self.shape_group = create_shape_group(
                    &self.device,
                    self.program.layout(SET_SHAPE),
                    self.program.bindings(),
                    &self.text,
                    &self.strings,
                );
            }
            None => {}
        }
        if written.front_block {
            self.watchdog.release();
        }
        // Whatever was shaped against the buffers as they were is owed another
        // pass now that more of them has landed.
        if written.landed {
            self.shape_pending = true;
        }
    }

    /// Glyph records the curve buffers hold — the font's, plus the icons the
    /// sidecar appended to them. What the health line reports.
    pub fn curve_glyphs(&self) -> u32 {
        self.text.curve_glyphs()
    }

    /// Bytes the four curve buffers hold, by the writer's own totals.
    pub fn curve_bytes(&self) -> u64 {
        self.text.curve_bytes()
    }

    /// Drop one captured still into the preview slot, at (0, 0).
    ///
    /// The bytes must be **premultiplied** RGBA8: the caustic (`caustics/bar_paint.slang`)
    /// composites the sample with `over` and never unpremultiplies, so there
    /// is exactly one alpha convention in the bar and the conversion from the
    /// capture feeder's straight alpha happens on this side of the upload.
    /// Anything past [`abi::PREVIEW_MAX_DIM`] is refused rather than clamped —
    /// a preview that big is a bug upstream, and the capture feeder already
    /// caps at the same number.
    pub fn upload_preview(&mut self, rgba_premul: &[u8], width: u32, height: u32) -> bool {
        let max = abi::PREVIEW_MAX_DIM;
        if width == 0 || height == 0 || width > max || height > max {
            return false;
        }
        let expected = (width as usize) * (height as usize) * 4;
        if rgba_premul.len() < expected {
            return false;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.preview,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba_premul[..expected],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        true
    }

    /// Upload one samples frame. Called once per sidecar tick — a
    /// second apart, 8 KiB at a time — with the frame exactly as it arrived on
    /// the child's stdout. The bar has checked the magic and the version and
    /// parsed nothing else; the caustic is the only reader.
    pub fn upload_samples(&mut self, bytes: &[u8]) {
        self.queue.write_buffer(&self.samples, 0, bytes);
    }

    /// Upload what the embassy knows, the bytes it points into, and every
    /// answer a sidecar has given (docs/abi.md, *The facts*, *The arenas*).
    ///
    /// Four writes and no decisions: the facts tables, the UTF-8 every offset
    /// in them names, the literal pool that indexes the head of those bytes,
    /// and the answers arena whole. The model passes owe a run afterwards,
    /// because what the caustic *builds* from all four has changed.
    pub fn upload_facts(
        &mut self,
        facts: &abi::Facts,
        strings: &[u8],
        literals: &[abi::Literal],
        answers: &[u8],
    ) {
        self.queue
            .write_buffer(&self.facts, 0, abi::as_bytes(facts));
        if !strings.is_empty() {
            // Rounded up to a whole word, because the caustic reads the buffer
            // as `uint[]` and `write_buffer` needs a multiple of four.
            let mut words = strings.to_vec();
            words.resize(strings.len().next_multiple_of(4), 0);
            self.queue.write_buffer(&self.strings, 0, &words);
        }
        // Whole and zero-filled past the live entries: a stale entry past
        // `inputs.literal_count` would answer for a hash the running caustic
        // no longer names.
        let mut padded = vec![abi::Literal::ZERO; abi::MAX_LITERALS];
        for (slot, literal) in padded.iter_mut().zip(literals) {
            *slot = *literal;
        }
        self.queue
            .write_buffer(&self.literals, 0, abi::slice_as_bytes(&padded));
        if !answers.is_empty() {
            // Rounded up to a whole word, as the strings are and for the same
            // reason: the caustic reads the arena as `uint[]`.
            let mut words = answers.to_vec();
            words.resize(answers.len().next_multiple_of(4), 0);
            self.queue.write_buffer(&self.answers, 0, &words);
        }
        self.shape_pending = true;
    }

    /// One sidecar's `Rows` body into its region of the rows arena
    /// (docs/abi.md, *The arenas*).
    ///
    /// **One `write_buffer` and nothing else.** The bytes are the frame's own
    /// — header, rows, blob at the writer's offsets, verbatim — and the embassy has read none of them. Whether the
    /// frame is worth a rebuild is `bar_apply`'s, from the `CMD_ROWS` that
    /// follows this write: it compares the region's shape against the copy it
    /// keeps, so a slider tick costs no model pass at all. The write is
    /// ordered against the frame's own submission on the queue timeline, so a
    /// draw never sees half a written word.
    pub fn write_rows_region(&mut self, slot: u32, body: &[u8]) {
        let base = slot as u64 * abi::ROWS_ARENA_BYTES as u64;
        let mut words = body.to_vec();
        words.resize(body.len().next_multiple_of(4), 0);
        self.queue.write_buffer(&self.rows, base, &words);
    }

    /// A sidecar that exited or was replaced: its region's header is zeroed,
    /// which is what makes `bar_build` skip the slot (its `row_count` is 0).
    pub fn zero_rows_header(&mut self, slot: u32) {
        let base = slot as u64 * abi::ROWS_ARENA_BYTES as u64;
        self.queue.write_buffer(&self.rows, base, &[0u8; 16]);
    }

    /// Owe the compose and shape passes a run without changing a block.
    ///
    /// The one caller is the clock's minute tick: the clock's words in `FrameInputs` moved and the bytes the caustic composes from them have to be rewritten,
    /// but the item table, the blocks and every other string are untouched,
    /// so nothing else is uploaded to move the clock.
    pub fn mark_shape_pending(&mut self) {
        self.shape_pending = true;
    }

    /// Rebuild every pipeline from freshly compiled SPIR-V, or change
    /// nothing. The state buffer is never touched, so the bar keeps its popup,
    /// its menu level and its animations across a reload.
    ///
    /// `reflection` is that same compile's reflection. The three bind
    /// group layouts were built at startup from the *baked* one and cannot be
    /// rebuilt without rebuilding the pipeline layout and every bind group
    /// hanging off it, so a caustic that moved, added or retyped a binding is
    /// refused here rather than swapped in: what a live reload would otherwise
    /// produce is a dispatch whose descriptors and whose `[[vk::binding]]`
    /// literals disagree — a device-lost on one driver, silently wrong buffers
    /// on another.
    ///
    /// `spirv` is one module per entry point, in `abi::BAR_ENTRIES` order. The
    /// swap and its refusal are `caustics_host::program::Program::reload`'s;
    /// what is the bar's is the wording of a refusal and the cold pass it
    /// owes the watchdog.
    ///
    /// The bindings are checked here; the pipelines are queued on the
    /// compile workers and swapped in together when the last is built
    /// ([`Landed::Swapped`] from [`Gpu::poll_pipelines`]), or refused, the
    /// bar that runs untouched either way. Returns the generation's number.
    pub fn reload_all(&mut self, reflection: &Reflection, spirv: &[&[u8]]) -> Result<u64> {
        match self.program.reload(reflection, spirv) {
            Ok(generation) => Ok(generation),
            Err(Refused::Bindings(witness)) => {
                bail!("reload refused: bindings changed ({witness}); restart the bar")
            }
            Err(Refused::Pipelines(err)) => Err(err),
        }
    }

    /// Install every pipeline that has landed and say what each did. A
    /// swapped generation owes the watchdog a cold pass and the model passes
    /// a run; so does a pipeline of the running program, since the shape
    /// submission it completes may have run without it.
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

    /// Whether a frame can be drawn: the frame submission's four pipelines
    /// are built. The model passes and the shaper come after, on their own.
    ///
    /// Nor while a pipeline is being warmed: a frame submitted behind a
    /// warming dispatch waits for the driver's compile (`Program::warming`).
    pub fn can_render(&self) -> bool {
        self.program.has(&FRAME_PIPELINES) && !self.program.warming()
    }

    /// One frame: drain the command ring, run apply once, then
    /// visibility → sample → draw per mapped target, present, and refresh the
    /// mirror. Returns the mirror as of this frame.
    pub fn render(&mut self, inputs: &FrameInputs, commands: &[Command]) -> Result<&State> {
        // **The frame stamp is this device's**, not the caller's: it has to be
        // the same for the shape submission and the frame's, and it may only
        // advance once the readback below has completed (docs/abi.md, *The two contracts*,
        // rule 1). A caller that filled in `frame` is overruled here.
        let inputs = &FrameInputs {
            frame: self.frame,
            ..*inputs
        };
        // The caller's check (`can_render`) less its warming half: a frame
        // asked for before the pipelines are built is an error rather than an
        // image acquired and presented unwritten, but a warming dispatch
        // begins on a compile worker at any moment, so a frame that passed
        // the caller's check and races one is submitted behind it, which is
        // slow on a driver that compiles late and nothing worse. Read here
        // again, that race was an exit.
        let apply = self
            .program
            .pipeline(APPLY)
            .filter(|_| self.program.has(&FRAME_PIPELINES))
            .context("a frame was asked for before its pipelines were built")?
            .clone();
        self.shape_if_pending(inputs);
        // The ring: the caustic reads `command_count` entries from the front,
        // so only the live prefix needs writing.
        if !commands.is_empty() {
            let live = &commands[..commands.len().min(abi::MAX_COMMANDS)];
            self.queue
                .write_buffer(&self.commands, 0, abi::slice_as_bytes(live));
        }
        self.queue
            .write_buffer(&self.inputs, 0, abi::as_bytes(inputs));

        let bar_params = Params {
            target: abi::TARGET_BAR,
            cap_height: self.cap_height,
            ..Params::default()
        };
        self.queue
            .write_buffer(&self.bar.params, 0, abi::as_bytes(&bar_params));
        if let Some(popup) = &self.popup {
            let params = Params {
                target: abi::TARGET_POPUP,
                cap_height: self.cap_height,
                ..Params::default()
            };
            self.queue
                .write_buffer(&popup.params, 0, abi::as_bytes(&params));
        }

        // Acquire every target before recording: a lost swapchain has to be
        // discovered before anything is dispatched, or half the frame is
        // already committed to a texture that will never be presented.
        let bar_swapchain = self
            .bar
            .surface
            .as_ref()
            .context("this device presents nothing; `render` is not its path")?;
        let bar_frame = acquire(bar_swapchain, "bar")
            .map_err(|err| anyhow!("the bar's swapchain texture could not be acquired: {err}"))?;
        let bar_view = bar_frame
            .texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let popup_frame = match &self.popup {
            Some(popup) => Some(
                acquire(
                    popup
                        .surface
                        .as_ref()
                        .expect("a mapped popup has a surface"),
                    "popup",
                )
                .map_err(|err| {
                    anyhow!("the popup's swapchain texture could not be acquired: {err}")
                })?,
            ),
            None => None,
        };
        let popup_view = popup_frame.as_ref().map(|frame| {
            frame
                .texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default())
        });

        let bar_group = self.bind_group(&self.bar, &bar_view, "bar");
        let popup_group = match (&self.popup, &popup_view) {
            (Some(popup), Some(view)) => Some(self.bind_group(popup, view, "popup")),
            _ => None,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-frame"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-bar-pass"),
                timestamp_writes: None,
            });

            // Groups 1 and 2 — the curves and the font tables — are the same
            // for every pass and every target, so they are set once here and
            // never again this frame. `dispatch_target` rebinds only group 0.
            pass.set_bind_group(1, &self.text_group, &[]);
            pass.set_bind_group(2, &self.shape_group, &[]);
            pass.set_bind_group(0, &bar_group, &[]);

            // apply runs once and lays out every target, so a popup frame
            // never needs a second one. It is bound with the bar's group;
            // it reads `params` only through `inputs`.
            pass.set_pipeline(&apply);
            pass.dispatch_workgroups(1, 1, 1);

            let bar_dims = (self.bar.config.width, self.bar.config.height);
            dispatch_target(&mut pass, &self.program, &bar_group, bar_dims);
            if let Some(group) = &popup_group {
                let popup = self.popup.as_ref().expect("a group implies a surface");
                let dims = (popup.config.width, popup.config.height);
                dispatch_target(&mut pass, &self.program, group, dims);
            }
        }
        // **The only readback in the bar** (docs/abi.md, *Who writes what*): the
        // caustic's polled words and its two rings.
        encoder.copy_buffer_to_buffer(
            &self.state,
            0,
            &self.mirror_readback,
            0,
            abi::STATE_BUFFER_SIZE,
        );
        self.queue.submit(Some(encoder.finish()));
        // wgpu 30 presents through the queue rather than the texture.
        let bar_suboptimal = bar_frame.suboptimal;
        let popup_suboptimal = popup_frame.as_ref().is_some_and(|frame| frame.suboptimal);
        self.queue.present(bar_frame.texture);
        if let Some(frame) = popup_frame {
            self.queue.present(frame.texture);
        }

        // A suboptimal swapchain is drawn, presented, and *then* reconfigured
        // once: on some drivers every acquire comes back suboptimal, and a
        // per-frame `warn!` there wrote 1419 journal lines in fifteen seconds
        // while fixing nothing.
        if bar_suboptimal {
            self.bar.refit_once(&self.device);
        }
        if popup_suboptimal && let Some(popup) = &mut self.popup {
            popup.refit_once(&self.device);
        }

        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|err| anyhow!("device wait failed: {err}"))?;
        self.refresh_mirror();
        if let Some(ms) = self.model_pass_ms.take() {
            // One line per submission, which is per model change and never per
            // frame — the same budget the `layout:` lines are on, and under a
            // target the journal filters the same way. The counts are the
            // caustic's `built:` witness, pushed by the pass that knows them.
            tracing::debug!(
                target: "caustics_bar::shape",
                "shape: built, composed and shaped in {ms:.2} ms",
            );
        }
        // The readback has completed, so this frame is over and the stamp may
        // advance (docs/abi.md, *The three rules the buffer adds*, rule 1). Every `?` above returns before
        // this line, which is what makes a frame that failed at the swapchain
        // keep its stamp — and the rings it pushed into.
        self.frame = self.frame.wrapping_add(1).max(1);
        Ok(&self.mirror)
    }

    /// Compose the clock's bytes and shape every text block, **in its own
    /// submission**, when the blocks have changed since the last one.
    ///
    /// Three passes, in this order and in one encoder: `bar_build` writes the item and block tables, `bar_compose` writes the
    /// bytes of every BLOCK_COMPOSED block and the byte length back into the
    /// block, then `bar_shape` shapes every block including
    /// those. They are two *passes* and not two dispatches for the same reason
    /// this whole submission is separate from the frame's; see below.
    ///
    /// Two facts force the separate submit, and they are worth stating because
    /// the obvious arrangement — one more dispatch at the top of the frame's
    /// own pass — is the one that does not work:
    ///
    /// 1. `bar_apply`'s layout *reads* what this writes. Dispatches inside one
    ///    compute pass have no memory dependency between them (wgpu inserts a
    ///    barrier on a resource's state *transition*, and storage-to-storage is
    ///    not one), so the layout could read a `TextBlock::size` mid-write. It
    ///    did: on lavapipe that is a torn float, a NaN width, and a segfault
    ///    three passes later in a walk sized from it — the bar crashed on its
    ///    first frame, every time, until the dispatch moved out.
    /// 2. Shaping is not per-frame work. A string changes when the model does —
    ///    a clock tick, a click, a title — so this runs there, and a frame that
    ///    only animates a hover spring submits nothing extra.
    fn shape_if_pending(&mut self, inputs: &FrameInputs) {
        if !self.shape_pending {
            return;
        }
        // A pass went over the threshold and the arena has not been rewritten
        // since. The pending flag stays set, so whatever is
        // owed is shaped on the frame after the release — this is a pause, not
        // a discard.
        if self.watchdog.blocked() {
            return;
        }
        // A pipeline is warming on the queue, and this submission would wait
        // behind it; still owed, and run on the frame after it lands.
        if self.program.warming() {
            return;
        }
        // The model passes are not built yet: still owed, and run when they
        // land. The shaper may come after them; until it does, the blocks
        // are laid out unshaped, which is what they are before a font
        // arrives: text of no size and no glyphs, and every chip and icon
        // where it goes. Its landing owes the passes another run
        // (`poll_pipelines`).
        let (Some(build), Some(compose)) =
            (self.program.pipeline(BUILD), self.program.pipeline(COMPOSE))
        else {
            return;
        };
        let (build, compose) = (build.clone(), compose.clone());
        let shape = self.program.pipeline(SHAPE).cloned();
        self.shape_pending = false;
        // The passes read the frame stamp, the scale and the three clock words,
        // so the uniform has to be current *before* they run rather than with
        // the rest of the frame. (The string and literal counts are
        // `FactsHeader`'s and arrive with the facts.)
        self.queue
            .write_buffer(&self.inputs, 0, abi::as_bytes(inputs));
        // Bind group 0 wants a destination image and neither pass writes one,
        // so it is handed the bar's ids view — a storage image of the right
        // format is the one thing the layout insists on, and neither
        // `bar_compose` nor `bar_shape` touches it or `dst`.
        let group = self.shape_bind_group();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-shape"),
            });
        {
            // **Its own pass, first**: `bar_build` writes the item and block
            // tables `bar_compose` and `bar_shape` then walk, and two
            // dispatches inside one compute pass have no memory dependency
            // between them (the paragraph above). Re-binding a read-write
            // storage buffer at the top of a new pass is what emits the
            // barrier (docs/abi.md, *Who writes what*).
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-bar-build-pass"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &group, &[]);
            pass.set_bind_group(1, &self.text_group, &[]);
            pass.set_bind_group(2, &self.shape_group, &[]);
            pass.set_pipeline(&build);
            pass.dispatch_workgroups(1, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-bar-compose-pass"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &group, &[]);
            pass.set_bind_group(1, &self.text_group, &[]);
            pass.set_bind_group(2, &self.shape_group, &[]);
            pass.set_pipeline(&compose);
            pass.dispatch_workgroups(1, 1, 1);
        }
        if let Some(shape) = &shape {
            // **Its own pass**, and that is the whole of why the compose pass
            // is correct: `bar_shape` reads the byte length `bar_compose`
            // writes into a block, and two dispatches inside one compute pass
            // have no memory dependency between them — the paragraph above is
            // where that was learned. Two passes in one encoder do have one:
            // the strings buffer and the block buffer are read-write storage
            // bindings, which wgpu never treats as an ordered state, so binding
            // them again at the top of this pass emits the barrier. One
            // submission still, which is what keeps the single wait below.
            // The watchdog's pair goes round **this** pass and no other:
            // `bar_build` and `bar_compose` walk tables
            // whose size the embassy set, and `bar_shape` is the one that
            // walks bytes a sidecar sent.
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-bar-shape-pass"),
                timestamp_writes: self.watchdog.timestamp_writes(),
            });
            pass.set_bind_group(0, &group, &[]);
            pass.set_bind_group(1, &self.text_group, &[]);
            pass.set_bind_group(2, &self.shape_group, &[]);
            pass.set_pipeline(shape);
            pass.dispatch_workgroups(SHAPE_BLOCKS.div_ceil(SHAPE_GROUP), 1, 1);
        }
        // The watchdog times the shape pass and nothing else, so a
        // submission without one has nothing for it to read.
        if shape.is_some() {
            self.watchdog.resolve(&mut encoder);
        }
        let started = std::time::Instant::now();
        self.queue.submit(Some(encoder.finish()));
        // Waited out before the frame's own submission: the layout reads what
        // this wrote, and two command buffers in flight have no ordering
        // between them that a bar can rely on.
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        // The three passes' cost, held until the mirror comes back: what they
        // *built* is a word `bar_build` writes, and this frame's readback is
        // where it lands. It is the only instrument the bar has for what the
        // model passes cost, and `bar_build`, the first of them, is the one
        // that does the work.
        self.model_pass_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        // What the device says `bar_shape` cost, and what to do about it
        // (`caustics_host::text::Watchdog::check`): the threshold arrived as
        // an `ACT_WATCHDOG` and the response is fixed. What was owed is still
        // owed: the flag the pass cleared goes back on, so the release
        // re-shapes rather than leaving last arena's metrics.
        if shape.is_some() && self.watchdog.check(&self.device) {
            self.shape_pending = true;
        }
    }

    /// The caustic's `SHAPE_WATCHDOG_MS`, as an `ACT_WATCHDOG`. One number in,
    /// and nothing else changes; on a software adapter it is scaled by
    /// `caustics_host::text::WATCHDOG_CPU_SCALE` (`Watchdog::set_threshold_ms`
    /// says why).
    pub fn set_watchdog_ms(&mut self, ms: u32) {
        self.watchdog.set_threshold_ms(ms);
    }

    /// Whether the watchdog has stopped the shape pass, for the health line.
    pub fn shape_blocked(&self) -> bool {
        self.watchdog.blocked()
    }

    /// A threshold no pass can meet, for the oracle's watchdog case: the
    /// caustic's `ACT_WATCHDOG` path only carries whole milliseconds.
    #[cfg(test)]
    pub fn set_watchdog_threshold(&mut self, ms: f64) -> bool {
        self.watchdog.set_threshold(ms)
    }

    /// `reload_all` with the baked modules, as `caustics-reload` would deliver an
    /// unchanged caustic: the swap without a compile.
    /// Synchronous, as the oracle wants it: queued, waited out, installed.
    #[cfg(test)]
    pub fn reload_baked(&mut self) -> Result<()> {
        self.reload_all(baked_reflection(), baked_spv(self.presentation.format))?;
        self.program.wait_staged().map_err(|err| anyhow!("{err}"))?;
        self.watchdog.swapped();
        self.shape_pending = true;
        Ok(())
    }

    /// Bind group 0 for the shape pass: the bar's own resources, with a
    /// throwaway view where the swapchain image would be.
    fn shape_bind_group(&self) -> wgpu::BindGroup {
        let view = self
            .shape_dst
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.bind_group(&self.bar, &view, "caustics-bar-shape")
    }

    /// Bind group 0 for one target, by name from the reflection. Rebuilt every
    /// frame because `dst` is the acquired swapchain image.
    fn bind_group(
        &self,
        target: &Surface,
        dst: &wgpu::TextureView,
        label: &str,
    ) -> wgpu::BindGroup {
        let entries: Vec<wgpu::BindGroupEntry> =
            group_entries(self.program.bindings(), SET_BAR, |name| match name {
                "state" => self.state.as_entire_binding(),
                "runs" => self.runs.as_entire_binding(),
                "commands" => self.commands.as_entire_binding(),
                "inputs" => self.inputs.as_entire_binding(),
                "params" => target.params.as_entire_binding(),
                "ids" => wgpu::BindingResource::TextureView(
                    target
                        .ids
                        .as_ref()
                        .expect("every bar target allocates its ids texture"),
                ),
                "dst" => wgpu::BindingResource::TextureView(dst),
                "images" => wgpu::BindingResource::TextureView(&self.images_view),
                "tex_sampler" => wgpu::BindingResource::Sampler(&self.tex_sampler),
                "preview" => wgpu::BindingResource::TextureView(&self.preview_view),
                "samples" => self.samples.as_entire_binding(),
                "literals" => self.literals.as_entire_binding(),
                "facts" => self.facts.as_entire_binding(),
                "private" => self.private.as_entire_binding(),
                "rows" => self.rows.as_entire_binding(),
                "answers" => self.answers.as_entire_binding(),
                other => panic!("{}", unknown_binding(other, SET_BAR)),
            });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: self.program.layout(SET_BAR),
            entries: &entries,
        })
    }

    /// Put the caustic's menu level where it keeps it, for the oracle: the
    /// `Private` field `bar_build` reads to know whose rows to publish
    /// (docs/abi.md, *Bindings and the oracle*).
    ///
    /// **The one write to `private` anywhere**, and it is `#[cfg(test)]`: in
    /// production the embassy allocates that buffer and never touches it
    /// again, and this exists only so that a scenario can name an open menu
    /// without a click. The offset comes from the reflection by field name,
    /// which is legibility rather than a promise — the ABI describes no word
    /// of `Private`.
    #[cfg(test)]
    pub fn set_menu_level(&mut self, lo: u32, hi: u32) {
        let at = private_field_offset("menu_level");
        let mut words = [0u8; 8];
        words[0..4].copy_from_slice(&lo.to_le_bytes());
        words[4..8].copy_from_slice(&hi.to_le_bytes());
        self.queue.write_buffer(&self.private, at as u64, &words);
    }

    /// Write one word of `Private` by field name, for a test that needs the
    /// caustic in a state only a click would otherwise reach — a menu popup
    /// that is up. Test-only for the reasons
    /// `set_menu_level` gives.
    #[cfg(test)]
    pub fn set_private_word(&mut self, field: &str, value: u32) {
        let at = private_field_offset(field);
        self.queue
            .write_buffer(&self.private, at as u64, &value.to_le_bytes());
    }

    /// Run `bar_apply` once, headless, with these inputs and commands, and read
    /// the state buffer back: the layout without a surface to present to
    /// The model passes run first if anything is owed,
    /// as in `render`; visibility, sample and draw do not run at all.
    #[cfg(test)]
    pub fn run_apply(&mut self, inputs: &FrameInputs, commands: &[Command]) -> Box<State> {
        let inputs = &FrameInputs {
            frame: self.frame,
            command_count: commands.len().min(abi::MAX_COMMANDS) as u32,
            ..*inputs
        };
        self.shape_if_pending(inputs);
        if !commands.is_empty() {
            let live = &commands[..commands.len().min(abi::MAX_COMMANDS)];
            self.queue
                .write_buffer(&self.commands, 0, abi::slice_as_bytes(live));
        }
        self.queue
            .write_buffer(&self.inputs, 0, abi::as_bytes(inputs));
        // Bound as `render` binds it, with the bar's `params`: a headless
        // device has no swapchain and `bar_apply` writes no texel, but the
        // pass reads the target and the cap height like any frame's.
        let params = Params {
            target: abi::TARGET_BAR,
            cap_height: self.cap_height,
            ..Params::default()
        };
        self.queue
            .write_buffer(&self.bar.params, 0, abi::as_bytes(&params));
        let group = self.shape_bind_group();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-apply-headless"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-bar-apply-headless-pass"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &group, &[]);
            pass.set_bind_group(1, &self.text_group, &[]);
            pass.set_bind_group(2, &self.shape_group, &[]);
            pass.set_pipeline(
                self.program
                    .pipeline(APPLY)
                    .expect("a headless device waits"),
            );
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.state,
            0,
            &self.mirror_readback,
            0,
            abi::STATE_BUFFER_SIZE,
        );
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        self.refresh_mirror();
        self.frame = self.frame.wrapping_add(1).max(1);
        self.mirror.clone()
    }

    /// The caustic's private buffer, whole, for a test to read by the
    /// reflection's offsets (docs/abi.md, *The private buffer*: legibility, not a promise).
    #[cfg(test)]
    pub fn read_private(&mut self) -> Vec<u8> {
        let size = self.private.size();
        let back = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-private-readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-private-copy"),
            });
        encoder.copy_buffer_to_buffer(&self.private, 0, &back, 0, size);
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let mut words = vec![0u32; (size / 4) as usize];
        surface::read_words(&self.device, &back, &mut words);
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    /// The strings buffer, whole, as the device holds it — for a test that reads
    /// the bytes a model pass composed into its own reservations (the
    /// calendar's title and days) after `run_apply` ran
    /// that pass.
    #[cfg(test)]
    pub fn read_strings(&mut self) -> Vec<u8> {
        let back = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-strings-readback"),
            size: abi::STRINGS_BUFFER_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-strings-copy"),
            });
        encoder.copy_buffer_to_buffer(&self.strings, 0, &back, 0, abi::STRINGS_BUFFER_SIZE);
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let mut words = vec![0u32; abi::MAX_STRING_WORDS];
        surface::read_words(&self.device, &back, &mut words);
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    /// Run the three model passes and read the state buffer back, for the
    /// oracle.
    ///
    /// `shape_if_pending` is the production submission, unchanged: the same
    /// three passes in the same encoder, waited out the same way. What this
    /// adds is the readback, which the running bar gets from the frame's own
    /// encoder instead — and the strings, because some of their bytes are the
    /// build pass's own (a dock entry's uppercased letter).
    #[cfg(test)]
    pub fn run_model_passes(&mut self, caffeine: u32) -> (Box<State>, Vec<u8>) {
        let inputs = FrameInputs {
            frame: self.frame,
            // The one per-frame input a scenario has an opinion about: the
            // caffeine chip is published from `inputs.caffeine`.
            caffeine,
            scale: 1,
            bar_size: [self.bar.config.width, self.bar.config.height],
            // A real date rather than zeroes, so the clock composes a weekday
            // and a scenario whose level is the clock builds a whole month
            // for the calendar: 09:30 on Saturday 10 October 2026, a month
            // of 31 days whose 1st was a Thursday.
            clock_time: 9 * 60 + 30,
            clock_date: 6 << 16 | 10 << 8 | 10,
            clock_month: 2026 << 16 | 4 << 8 | 31,
            ..FrameInputs::default()
        };
        self.shape_pending = true;
        self.model_pass_ms = None;
        self.shape_if_pending(&inputs);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-oracle-readback"),
            });
        // `bar_check`, when the oracle compiled one: an eighth entry point that
        // walks `private` after the three passes and pushes `check:` witnesses
        // into the same ring (docs/abi.md, *Bindings and the oracle*). Its own pass, so the
        // tables the three wrote are visible to it.
        if let Some(check) = &self.check {
            let group = self.shape_bind_group();
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("caustics-bar-check-pass"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &group, &[]);
            pass.set_bind_group(1, &self.text_group, &[]);
            pass.set_bind_group(2, &self.shape_group, &[]);
            pass.set_pipeline(check);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.state,
            0,
            &self.mirror_readback,
            0,
            abi::STATE_BUFFER_SIZE,
        );
        let bytes = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-strings-readback"),
            size: abi::STRINGS_BUFFER_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&self.strings, 0, &bytes, 0, abi::STRINGS_BUFFER_SIZE);
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        self.refresh_mirror();
        self.frame = self.frame.wrapping_add(1).max(1);
        let mut strings_back = vec![0u32; abi::MAX_STRING_WORDS];
        surface::read_words(&self.device, &bytes, &mut strings_back);
        (
            self.mirror.clone(),
            strings_back
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect(),
        )
    }

    /// The glyph buffer, as the oracle reads it back. `bar_shape`'s output is
    /// the caustic's own buffer and nothing on this side ever looks at it in
    /// production — which is exactly why one headless test does: a driver that
    /// miscompiled the shaper drew the first glyph of every block as `.notdef`
    /// (docs/architecture.md, *Rules*).
    #[cfg(test)]
    pub fn read_runs(&mut self) -> Vec<punchcutter_shape::abi::ShapedGlyph> {
        let size = abi::GLYPH_BUFFER_SIZE;
        let back = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-runs-readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-runs-copy"),
            });
        encoder.copy_buffer_to_buffer(&self.runs, 0, &back, 0, size);
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let mut words = vec![0u32; abi::MAX_GLYPHS * 8];
        surface::read_words(&self.device, &back, &mut words);
        words
            .chunks_exact(8)
            .map(|w| punchcutter_shape::abi::ShapedGlyph {
                gid: w[0],
                x: w[1] as i32,
                y: w[2] as i32,
                cluster: w[3],
                flags: w[4],
                _rsvd0: w[5],
                _rsvd1: w[6],
                _rsvd2: w[7],
            })
            .collect()
    }

    /// The first `len` bytes of one curve buffer or the arena, as the GPU
    /// holds them — for the oracle's check that a record a sidecar filled in
    /// place is the record the caustic reads.
    #[cfg(test)]
    pub fn read_buffer(&mut self, buffer: u32, len: u64) -> Vec<u8> {
        let Some(slot) = self.text.buffer(buffer) else {
            return Vec::new();
        };
        let len = len.min(slot.size()).next_multiple_of(4);
        if len == 0 {
            return Vec::new();
        }
        let back = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-buffer-readback"),
            size: len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("caustics-bar-buffer-copy"),
            });
        encoder.copy_buffer_to_buffer(slot, 0, &back, 0, len);
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let mut words = vec![0u32; (len / 4) as usize];
        surface::read_words(&self.device, &back, &mut words);
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    /// Load `bar_check` as an eighth pipeline, for the oracle alone
    /// (docs/abi.md, *Bindings and the oracle*). Production's `BAR_ENTRIES` never names it and the
    /// baked SPIR-V never carries it.
    #[cfg(test)]
    pub fn set_check(&mut self, spirv: &[u8]) -> Result<()> {
        let check = [Entry {
            name: "bar_check",
            label: "caustics-bar-check",
            workgroup: (1, 1, 1),
        }];
        let mut pipelines = caustics_host::program::build_pipelines(
            &self.device,
            self.program.pipeline_layout(),
            &check,
            &[spirv],
        )
        .map_err(|err| anyhow!("bar_check does not load: {err}"))?;
        self.check = pipelines.pop();
        Ok(())
    }
    /// Pull the mirror out of the readback buffer, the device having already
    /// been waited out — `surface::refresh_mirror`, where the second
    /// poll and the keep-the-old-mirror failure policy are explained.
    fn refresh_mirror(&mut self) {
        refresh_mirror(&self.device, &self.mirror_readback, &mut *self.mirror);
    }
}

/// visibility → sample → draw for one target, in that order: `bar_sample`
/// reads the ids this frame just wrote, so `hover_id` in the mirror is current
/// as of the pointer position the CPU supplied for this frame.
fn dispatch_target(
    pass: &mut wgpu::ComputePass<'_>,
    program: &Program,
    group: &wgpu::BindGroup,
    (width, height): (u32, u32),
) {
    let groups_x = width.div_ceil(TILE).max(1);
    let groups_y = height.div_ceil(TILE).max(1);
    let (Some(visibility), Some(sample), Some(draw)) = (
        program.pipeline(VISIBILITY),
        program.pipeline(SAMPLE),
        program.pipeline(DRAW),
    ) else {
        // `render` checked `can_render` before acquiring anything.
        return;
    };
    pass.set_bind_group(0, group, &[]);
    pass.set_pipeline(visibility);
    pass.dispatch_workgroups(groups_x, groups_y, 1);
    pass.set_pipeline(sample);
    pass.dispatch_workgroups(1, 1, 1);
    pass.set_pipeline(draw);
    pass.dispatch_workgroups(groups_x, groups_y, 1);
}

/// One target's swapchain (`caustics_host::surface::Surface`), with its ids
/// texture under this bar's label and a `Params` uniform labelled by target.
fn configure_surface(
    device: &wgpu::Device,
    surface: Option<wgpu::Surface<'static>>,
    presentation: &Presentation,
    width: u32,
    height: u32,
    target: Target,
) -> Surface {
    Surface::configure(
        device,
        surface,
        presentation,
        width,
        height,
        Some(IDS_LABEL),
        target.label(),
        size_of::<Params>() as u64,
    )
}

/// The image slots: [`abi::MAX_IMAGE_SLOTS`] layers of
/// [`abi::IMAGE_SLOT_PX`]² straight RGBA, created once and never resized.
///
/// 1 MiB at 64 slots of 64 px, and *fixed*: nothing is packed or moved, so nothing the caustic has sampled is ever invalidated.
fn create_images(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("caustics-bar-images"),
        size: wgpu::Extent3d {
            width: abi::IMAGE_SLOT_PX,
            height: abi::IMAGE_SLOT_PX,
            depth_or_array_layers: abi::MAX_IMAGE_SLOTS as u32,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: IMAGE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    (texture, view)
}

fn create_text_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    bindings: &[Binding],
    text: &TextBuffers,
) -> wgpu::BindGroup {
    let entries = group_entries(bindings, SET_TEXT, |name| {
        text.resource(name)
            .unwrap_or_else(|| panic!("{}", unknown_binding(name, SET_TEXT)))
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("caustics-bar-text"),
        layout,
        entries: &entries,
    })
}

/// The engine's tables, by their reflected names (`TextBuffers::resource`),
/// and the bar's strings buffer behind `shapeUtf8`.
fn create_shape_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    bindings: &[Binding],
    text: &TextBuffers,
    strings: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let entries = group_entries(bindings, SET_SHAPE, |name| match name {
        "shapeUtf8" => strings.as_entire_binding(),
        other => text
            .resource(other)
            .unwrap_or_else(|| panic!("{}", unknown_binding(other, SET_SHAPE))),
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("caustics-bar-shape"),
        layout,
        entries: &entries,
    })
}

/// What a reflected name this side has no resource for reads as. A panic like
/// `storage_texture_format`'s: the caustic wants a descriptor the embassy
/// cannot name, and there is no frame worth drawing after that.
fn unknown_binding(name: &str, set: u32) -> String {
    format!(
        "the caustic declares `{name}` in set {set} and `bar/src/gpu.rs` has no resource for \
         it. A binding added in Slang needs one line here, naming the buffer or texture it \
         should be handed; the number is read from the reflection and is not this file's."
    )
}

/// The preview slot: one fixed [`abi::PREVIEW_MAX_DIM`]² Rgba8Unorm texture,
/// sampled with the image slots' sampler. Fixed because the alternative is
/// reallocating a texture and rebuilding the bind-group layout on the first
/// frame of every hover, and 2048² is 16 MiB. The valid extent of what is in it rides in
/// `FrameInputs::preview_size`; the caustic's uv walk stops there.
fn create_preview(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("caustics-bar-preview"),
        size: wgpu::Extent3d {
            width: abi::PREVIEW_MAX_DIM,
            height: abi::PREVIEW_MAX_DIM,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: IMAGE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// The compiler's reflection of the baked program, as JSON — `bar/build.rs`
/// writes it beside the SPIR-V out of the first format's compile, and every
/// entry point and both [`DstFormat`](caustics_bar_abi::DstFormat) variants
/// reflect the same parameters. Read through [`baked_reflection`].
///
/// This is the *only* declaration of the bar's bindings on the CPU side. The
/// `[[vk::binding(n, g)]]` literals in the caustics are the source of truth;
/// they are read here rather than transcribed into hand-written layout
/// entries that tests would have to keep in agreement.
pub(crate) const BAKED_REFLECTION: &str =
    include_str!(concat!(env!("OUT_DIR"), "/bar.reflection.json"));

/// [`BAKED_REFLECTION`], read once.
///
/// Panics rather than returns: a reflection the bar cannot read means it
/// does not know what descriptors its own caustic wants, and there is no frame
/// worth drawing after that. It is baked at build time by the same crate
/// that reads it here, so this can only fail on a build whose two halves
/// disagree — which is a build to fix, not a session to limp through.
fn baked_reflection() -> &'static Reflection {
    static BAKED: std::sync::OnceLock<Reflection> = std::sync::OnceLock::new();
    BAKED.get_or_init(|| caustics_host::program::read_baked(BAKED_REFLECTION))
}

/// The bar's own bind group: the state buffer, the rings, the target's images, the facts, the arenas and the samples frame.
const SET_BAR: u32 = 0;
/// The engine's `text.slang`'s four curve buffers, at the bindings that module
/// declares for itself.
const SET_TEXT: u32 = 1;
/// The engine's `shape.slang`'s scalars and tables plus the strings buffer.
const SET_SHAPE: u32 = 2;

/// Every descriptor set the bar builds a layout for, in the order the pipeline
/// layout takes them: the three labels `bring_up` hands `Program::new`, which
/// asserts the reflection declares exactly these.
#[cfg(test)]
const SETS: [u32; 3] = [SET_BAR, SET_TEXT, SET_SHAPE];

/// The stride Slang reports for `Private`, which is how many bytes the
/// caustic's own memory takes (docs/abi.md, *The private buffer*).
///
/// The embassy allocates this many and then never writes or reads one of them.
/// It comes from the *reflection* rather than from a constant because the ABI
/// describes no word of that struct: `bar_state.slang` may add a field, and the only
/// thing on this side that has to change is this number. A reload whose stride
/// differs is refused by the existing bindings diff, which is the design.
fn private_stride(bindings: &[Binding]) -> u64 {
    for binding in bindings {
        if binding.name != "private" {
            continue;
        }
        if let BindingKind::Storage {
            stride: Some(stride),
            ..
        } = binding.kind
        {
            return u64::from(stride);
        }
    }
    panic!(
        "the caustic declares no `private` storage buffer with a reflected stride; \
         docs/abi.md, *The private buffer* is what it should look like"
    )
}

/// The byte offset of one field of `Private`, by name, from the baked
/// reflection. Test-only, and legibility rather than a promise: nothing in a
/// shipped bar reads a word of that buffer.
#[cfg(test)]
pub(crate) fn private_field_offset(field: &str) -> usize {
    reflected_offset("Private", field)
}

/// The byte offset of one field of any struct the baked reflection lays out
/// — `Element`'s, for the layout test that reads the element table back
/// Test-only, like the above.
#[cfg(test)]
pub(crate) fn reflected_offset(structure: &str, field: &str) -> usize {
    *baked_reflection()
        .structs
        .get(structure)
        .unwrap_or_else(|| panic!("the reflection has no `{structure}` struct"))
        .get(field)
        .unwrap_or_else(|| panic!("`{structure}` has no field `{field}`"))
}

/// Every binding the baked program declares, sorted by `(set, index)`.
fn baked_bindings() -> Vec<Binding> {
    baked_reflection().bindings.clone()
}

/// The one fact about a binding the reflection cannot carry: a storage image's
/// texel format.
///
/// SPIR-V bakes it into the module, and the `dst` image is declared
/// format-*unknown* on the Bgra8 variant ([`DstFormat::Bgra8`](caustics_bar_abi::DstFormat::Bgra8)) precisely
/// because SPIR-V has no BGRA storage format to name — so Slang reflects no
/// format for it at all there. Two images, two arms, and a third one is a
/// panic rather than a guess: a storage image bound at the wrong format is
/// wrong pixels on one driver and a device-lost on another.
fn storage_texture_format(name: &str, dst: wgpu::TextureFormat) -> wgpu::TextureFormat {
    match name {
        "ids" => ID_FORMAT,
        "dst" => dst,
        other => panic!(
            "`{other}` is a storage image this bar has no texel format for; `bar/src/gpu.rs` \
             knows `ids` (the visibility buffer) and `dst` (the swapchain image) and has to be \
             told about any third."
        ),
    }
}

/// The baked modules matching a swapchain format. Panics on a format
/// [`Gpu::new`] cannot select, which is unreachable: the same match governs
/// the surface-format choice.
fn baked_spv(format: wgpu::TextureFormat) -> &'static BakedSpv {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => &SPV_RGBA8UNORM,
        wgpu::TextureFormat::Bgra8Unorm => &SPV_BGRA8UNORM,
        other => unreachable!("surface format {other:?} is not one this bar can select"),
    }
}

#[cfg(test)]
mod tests;
