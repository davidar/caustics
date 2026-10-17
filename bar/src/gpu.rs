//! The bar's GPU side: two surfaces, seven pipelines, three bind group layouts.
//!
//! Three shapes here are deliberate:
//!
//! - **passthrough SPIR-V.** The caustic is Slang; wgpu takes the module
//!   through `create_shader_module_passthrough`, which performs no validation
//!   of its own, so every pipeline build sits inside a validation error scope
//!   and a reload is all-or-nothing ([`Gpu::reload_all`]).
//! - **the mirror.** The caustic owns [`State`]; the CPU reads it one frame
//!   late out of a `MAP_READ` readback buffer, through `surface::refresh_mirror`. Nothing on the CPU ever writes it, and it is the *only* buffer read back at all (docs/abi.md, *Who writes what*).
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
//! **The layouts are built from slangc's reflection**, not from a list here
//! ([`BAKED_REFLECTION`]), so there is no second copy of every binding here to
//! keep dense, in order, and in agreement with the `[[vk::binding(n, g)]]`
//! literals in the caustics. The caustics are the one declaration; what is
//! left here is the two things a reflection cannot
//! carry, both keyed on the parameter's name: a storage image's texel format
//! ([`storage_texture_format`]) and which buffer to bind.
//!
//! **And the bind *groups* are built the same way**
//! ([`group_entries`], docs/abi.md, *Bindings and the oracle*): each entry takes its number from
//! the reflected [`Binding`] and its resource from a `match` on that binding's
//! name, so no number is written on this side at all. A binding added in Slang
//! needs one arm here and nothing else; a binding that moves moves in one file.
//!
//! Fifteen storage buffers across the three, which is well past wgpu's default
//! per-stage limit of eight — [`Gpu::new`] raises it to whatever the adapter
//! allows and says so, counting from the reflection.

use std::num::NonZeroU64;
use std::ptr::NonNull;

use anyhow::{Context, Result, anyhow, bail};
use punchcutter_core::gpu::{passthrough_pipeline, storage_buffer};
use punchcutter_core::slang::{self, Binding, BindingKind};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

use crate::abi::{self, Command, FrameInputs, Params, State};

use self::surface::{ID_FORMAT, acquire, create_id_texture, refresh_mirror};

mod surface;

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

/// Headroom left in the curve buffers when they are (re)created, as a fraction:
/// an icon resolved later appends to them, and a buffer that has to be rebuilt
/// on every new icon would rebuild its bind group with it. A font is 4 MiB and
/// an icon is 2 KB, so an eighth is hundreds of icons.
const CURVE_HEADROOM: f32 = 0.125;

/// One glyph record in `BUF_GLYPHS`, in bytes (`punchcutter_text::Glyph`, which the bar does not link: `caustics-glyphs` writes them and this is the divisor
/// that turns its `total` into a count). The ABI's own table says 64 B
/// (docs/abi.md, *Bytes*) and `bar/src/abi/tests.rs` checks the layout against
/// `text.slang`'s reflection.
const GLYPH_RECORD: u32 = 64;

// The seven entry-point names come from `abi::ENTRIES`, the one list build.rs bakes and `caustics-reload` recompiles; the indices below are that list's order, which `reload_all` also takes.
const BUILD_ENTRY: &str = abi::ENTRIES[0];
const COMPOSE_ENTRY: &str = abi::ENTRIES[1];
const SHAPE_ENTRY: &str = abi::ENTRIES[2];
const APPLY_ENTRY: &str = abi::ENTRIES[3];
const VISIBILITY_ENTRY: &str = abi::ENTRIES[4];
const SAMPLE_ENTRY: &str = abi::ENTRIES[5];
const DRAW_ENTRY: &str = abi::ENTRIES[6];

/// The seven baked modules for one swapchain format.
struct BakedSpv<'a> {
    build: &'a [u8],
    compose: &'a [u8],
    shape: &'a [u8],
    apply: &'a [u8],
    visibility: &'a [u8],
    sample: &'a [u8],
    draw: &'a [u8],
}

const SPV_RGBA8UNORM: BakedSpv<'static> = BakedSpv {
    build: include_bytes!(concat!(env!("OUT_DIR"), "/bar_build.rgba8unorm.spv")),
    compose: include_bytes!(concat!(env!("OUT_DIR"), "/bar_compose.rgba8unorm.spv")),
    shape: include_bytes!(concat!(env!("OUT_DIR"), "/bar_shape.rgba8unorm.spv")),
    apply: include_bytes!(concat!(env!("OUT_DIR"), "/bar_apply.rgba8unorm.spv")),
    visibility: include_bytes!(concat!(env!("OUT_DIR"), "/bar_visibility.rgba8unorm.spv")),
    sample: include_bytes!(concat!(env!("OUT_DIR"), "/bar_sample.rgba8unorm.spv")),
    draw: include_bytes!(concat!(env!("OUT_DIR"), "/bar_draw.rgba8unorm.spv")),
};

const SPV_BGRA8UNORM: BakedSpv<'static> = BakedSpv {
    build: include_bytes!(concat!(env!("OUT_DIR"), "/bar_build.bgra8unorm.spv")),
    compose: include_bytes!(concat!(env!("OUT_DIR"), "/bar_compose.bgra8unorm.spv")),
    shape: include_bytes!(concat!(env!("OUT_DIR"), "/bar_shape.bgra8unorm.spv")),
    apply: include_bytes!(concat!(env!("OUT_DIR"), "/bar_apply.bgra8unorm.spv")),
    visibility: include_bytes!(concat!(env!("OUT_DIR"), "/bar_visibility.bgra8unorm.spv")),
    sample: include_bytes!(concat!(env!("OUT_DIR"), "/bar_sample.bgra8unorm.spv")),
    draw: include_bytes!(concat!(env!("OUT_DIR"), "/bar_draw.bgra8unorm.spv")),
};

/// The four curve buffers the engine's `text.slang` reads, and how much they hold.
///
/// Kept together because they are replaced together: an icon appended to the
/// font's glyphs grows all four, and a bind group naming any of them has to be
/// rebuilt when one is recreated.
struct Curves {
    points: wgpu::Buffer,
    bands: wgpu::Buffer,
    band_curves: wgpu::Buffer,
    glyphs: wgpu::Buffer,
}

/// The shaper's buffers: the dispatch's scalars, and the **arena**.
///
/// One arena rather than a buffer per flattened table, each of which would be a shape the CPU had to understand in order to build. `ot` is `cmap`, `hmtx`, `GDEF`, `GSUB` and `GPOS` as they
/// sit in the font files, behind a directory `caustics-glyphs` wrote and
/// the engine's `ot.slang` reads. Adwaita Sans is 221 KiB this way.

struct ShapeBuffers {
    params: wgpu::Buffer,
    ot: wgpu::Buffer,
}

/// What it takes to time one compute pass on the device itself.
///
/// Wall clock around the submit would do for an order of magnitude, and
/// `model_pass_ms` is exactly that — but it times three passes plus a submit
/// and a fence wait, which on a loaded host is mostly the host. The question
/// here is narrower and needs the device's own answer: how long did
/// **`bar_shape`** run.
struct Watchdog {
    set: wgpu::QuerySet,
    /// Where `resolve_query_set` puts the two timestamps.
    resolved: wgpu::Buffer,
    /// ...and where they are copied to be mapped.
    readback: wgpu::Buffer,
    /// Nanoseconds per tick, from the queue.
    period_ns: f32,
    /// `SHAPE_WATCHDOG_MS` as the caustic last pushed it (`ACT_WATCHDOG`);
    /// zero until the first frame, which is what makes the watchdog silent
    /// before the caustic has run at all.
    threshold_ms: f64,
}

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

/// One swapchain and the per-target resources that hang off it.
struct Surface {
    /// `None` only for a headless device — the oracle's, which presents
    /// nothing. Everything else about the target is real,
    /// because the passes bind its `params` and its `ids` like any other.
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    ids: wgpu::TextureView,
    params: wgpu::Buffer,
    /// Has a suboptimal acquire already been answered with a reconfigure
    /// against *this* configuration? Cleared whenever the config changes.
    refit: bool,
}

impl Surface {
    /// Answer a suboptimal acquire with one reconfigure, and only one.
    ///
    /// A suboptimal swapchain is still drawable, so the frame is presented
    /// first and the surface rebuilt after it. Rebuilding on *every* frame
    /// would be worse than the warning it replaces — on some drivers every
    /// acquire reports suboptimal — so the answer
    /// happens once per configuration, and a genuine resize re-arms it.
    fn refit_once(&mut self, device: &wgpu::Device) {
        let Some(surface) = &self.surface else {
            return;
        };
        if self.refit {
            return;
        }
        self.refit = true;
        tracing::debug!(
            width = self.config.width,
            height = self.config.height,
            "reconfiguring a suboptimal swapchain"
        );
        surface.configure(device, &self.config);
    }
}

/// The seven pipelines, swapped together or not at all.
struct Pipelines {
    build: wgpu::ComputePipeline,
    compose: wgpu::ComputePipeline,
    shape: wgpu::ComputePipeline,
    apply: wgpu::ComputePipeline,
    visibility: wgpu::ComputePipeline,
    sample: wgpu::ComputePipeline,
    draw: wgpu::ComputePipeline,
}

/// How much more a software adapter's shape pass may take than the caustic's
/// `SHAPE_WATCHDOG_MS` says: see [`Gpu::set_watchdog_ms`].
pub const WATCHDOG_CPU_SCALE: f64 = 8.0;

pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Whether the device came up with `VK_KHR_shader_clock`, which decides
    /// whether the shaper gets a wall-clock deadline.
    shader_clock: punchcutter_core::gpu::ShaderClock,

    /// The negotiated swapchain format; both surfaces use it, and it decides
    /// which SPIR-V variant the pipelines were built from.
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    /// Mailbox where the surface offers it, Fifo otherwise. Fifo is the
    /// trap: Mesa's Wayland WSI implements it by blocking the *next* present
    /// on the previous commit's frame callback, and a popup the compositor
    /// has just unmapped never gets one, so the bar hangs (seen on niri).
    /// Under Mailbox nothing in `present`
    /// waits on the compositor; pacing is the embassy's own frame callback.
    present_mode: wgpu::PresentMode,

    bind_group_layout: wgpu::BindGroupLayout,
    /// Group 1: the engine's `text.slang`'s four curve buffers, at the bindings that
    /// module declares for itself.
    text_layout: wgpu::BindGroupLayout,
    /// Group 2: the engine's `shape.slang`'s font tables and the strings buffer, at
    /// the bindings `bar.slang` chose for them.
    shape_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: Pipelines,

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
    /// want, the witness memory. One element of the stride slangc's reflection
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
    /// The curve buffers (group 1), and the bind group over them. Rebuilt — both
    /// of them — only when an icon has outgrown the headroom.
    curves: Curves,
    text_group: wgpu::BindGroup,
    /// The shape tables (group 2). Written once per font, which is once.
    shape: ShapeBuffers,
    shape_group: wgpu::BindGroup,

    /// A 1x1 texture of the swapchain's format, for the shape pass's bind
    /// group: group 0's layout declares a storage image (`dst`) and
    /// `bar_shape` writes no pixels, so it is given somewhere harmless rather
    /// than a swapchain image it has no business holding.
    shape_dst: wgpu::Texture,
    /// Set when the blocks changed and the shape pass owes them their metrics.
    shape_pending: bool,
    /// The watchdog around `bar_shape`: a timestamp query
    /// pair, somewhere to resolve it, somewhere to read it, and the threshold
    /// the caustic pushed. `None` on an adapter without `TIMESTAMP_QUERY`,
    /// which `Gpu::new` says out loud.
    watchdog: Option<Watchdog>,
    /// A shape pass ran over the threshold: nothing is shaped until the
    /// OpenType arena's front block is written again (`write_bytes`). The
    /// pending flag is kept, so the release re-shapes what was owed.
    shape_blocked: bool,
    /// The next timed shape pass is the first since the pipelines were
    /// created or swapped, and is logged but not judged: on lavapipe it times
    /// llvmpipe building the new variant (12 ms warm, 18 s with the cache
    /// off), not the arena, and judging it would be a false watchdog trip.
    shape_cold: bool,
    /// What the last build/compose/shape submission cost, until the mirror
    /// carrying what it built comes back and the two are logged together.
    model_pass_ms: Option<f64>,
    /// The bindings the three layouts above were built from, as the baked
    /// reflection declared them. A reload compares its own compile's against
    /// these and refuses rather than swap in modules the layouts do not
    /// describe.
    bindings: Vec<Binding>,
    /// `Params.cap_height`, in em; set by `set_cap_height` when a font is
    /// known and 0 before.
    cap_height: f32,
    /// Glyph records actually *written* into the curve buffers, which is the
    /// font's glyphs plus the icons appended to them — not the buffers'
    /// capacity, which carries [`CURVE_HEADROOM`] of unwritten tail. Every
    /// glyph id the caustic indexes must be below this: `text.slang` reads
    /// `textGlyphs[glyphIndex]` with no bound of its own, and a record read out
    /// of the headroom has a garbage `band_base`.
    curve_glyphs: u32,
    /// The four curve buffers' live lengths, as the `Bytes` runs that filled
    /// them declared. The health line's `curves=` clause and nothing else.
    curve_totals: [u32; 4],

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
    /// build, and in a test run that could not reach slangc.
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
    pub unsafe fn new(
        display: NonNull<std::ffi::c_void>,
        surface: NonNull<std::ffi::c_void>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        // SAFETY: the caller's contract, above.
        let bar_surface = unsafe { create_surface(&instance, display, surface)? };
        Gpu::bring_up(instance, Some(bar_surface), width, height)
    }

    /// The same device, the same buffers and the same pipelines with **no
    /// surface**: what the oracle runs the caustic on.
    ///
    /// Nothing to present to and nothing to be compatible with, so the adapter
    /// is chosen the way the text engine's harnesses choose one — the first
    /// integrated or CPU adapter whose name does not say NVIDIA — and the
    /// format is fixed rather than negotiated. Everything after that is the
    /// production path, which is the whole point: the oracle tests what ships.
    #[cfg(test)]
    pub fn headless(width: u32, height: u32) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        Gpu::bring_up(instance, None, width, height)
    }

    fn bring_up(
        instance: wgpu::Instance,
        bar_surface: Option<wgpu::Surface<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let adapter = match &bar_surface {
            Some(surface) => {
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    // A status bar is a handful of texels a second. Never the
                    // dGPU.
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: Some(surface),
                    ..Default::default()
                }))
                .context("no Vulkan adapter can present to the bar's layer surface")?
            }
            // A CPU adapter is taken for the same reason the discrete one is
            // refused: a software rasteriser cannot be the dGPU, which is what
            // lets the oracle run in a VM whose only Vulkan driver is lavapipe.
            None => pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
                .into_iter()
                .find(|adapter| {
                    let info = adapter.get_info();
                    let usable = info.device_type == wgpu::DeviceType::IntegratedGpu
                        || info.device_type == wgpu::DeviceType::Cpu;
                    if !usable || info.name.to_ascii_uppercase().contains("NVIDIA") {
                        tracing::info!(adapter = %info.name, device_type = ?info.device_type,
                                       "adapter refused");
                        return false;
                    }
                    true
                })
                .context(
                    "no integrated or CPU Vulkan adapter; refusing to run the caustic on a \
                     discrete GPU. Pin the Vulkan loader to the integrated GPU's ICD with \
                     VK_DRIVER_FILES (bar/systemd/dropins/igpu-only.conf shows how); \
                     CAUSTICS_BAR_ALLOW_DGPU does not apply to the headless device",
                )?,
        };

        let info = adapter.get_info();
        tracing::info!(
            adapter = %info.name,
            driver = %info.driver_info,
            device_type = ?info.device_type,
            backend = ?info.backend,
            "wgpu adapter"
        );

        // docs/architecture.md, *Power and the discrete GPU*: the unit pins VK_DRIVER_FILES at
        // the integrated ICD, and this is the belt to those braces. Waking a
        // discrete GPU to draw a status bar is the one failure the bar must
        // never commit on a hybrid laptop.
        if info.name.to_ascii_uppercase().contains("NVIDIA")
            && std::env::var_os("CAUSTICS_BAR_ALLOW_DGPU").is_none()
        {
            tracing::error!(
                adapter = %info.name,
                "refusing to run the bar on a discrete GPU: it would keep the dGPU awake for \
                 the life of the session. Pin VK_DRIVER_FILES at the integrated ICD, or set \
                 CAUSTICS_BAR_ALLOW_DGPU=1 to override."
            );
            std::process::exit(2);
        }

        if !adapter
            .features()
            .contains(wgpu::Features::PASSTHROUGH_SHADERS)
        {
            bail!(
                "adapter '{}' lacks PASSTHROUGH_SHADERS; the bar's Slang->SPIR-V caustic \
                 cannot be loaded here",
                info.name
            );
        }

        let mut features = wgpu::Features::PASSTHROUGH_SHADERS;
        if adapter
            .features()
            .contains(wgpu::Features::BGRA8UNORM_STORAGE)
        {
            features |= wgpu::Features::BGRA8UNORM_STORAGE;
        }
        // The shape pass's watchdog. Both adapters the bar is tested on have it — RADV and lavapipe — and one that does not gets a
        // warning and no watchdog rather than no bar.
        let timing = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        if timing {
            features |= wgpu::Features::TIMESTAMP_QUERY;
        }

        // The bar binds more storage buffers than wgpu's default per-stage limit
        // of eight allows — the bar's own nine, `text.slang`'s four curves and
        // `shape.slang`'s tables — so it asks for what the adapter has rather
        // than for a number: RADV's ceiling is in the millions and lavapipe's is
        // its own business, and a bar that asked for 32 would fail on a driver
        // that offered 31 for no reason. The counts come from slangc's
        // reflection of the caustic itself, so a binding added in Slang is
        // counted here without anyone editing this file.
        let reflected = baked_bindings();
        assert_sets(&reflected);
        let (bindings, groups, storage_buffers) = binding_counts(&reflected);
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffers_per_shader_stage = adapter
            .limits()
            .max_storage_buffers_per_shader_stage
            .max(limits.max_storage_buffers_per_shader_stage);
        tracing::info!(
            storage_buffers_allowed = limits.max_storage_buffers_per_shader_stage,
            bind_groups = limits.max_bind_groups,
            bindings,
            storage_buffers,
            "device limits (the bar declares {bindings} bindings across {groups} groups, \
             {storage_buffers} of them storage buffers)"
        );

        // **Owns a file descriptor** (the first membership test, docs/architecture.md, *The bar process*): the device is the bar's,
        // and it is opened through the text engine's device helper
        // rather than `request_device` — because wgpu will not ask for
        // `VK_KHR_shader_clock` and the shaper's SPIR-V carries the
        // capability. The shaper holds a wall-clock deadline inside the shader with it, which is what stands between a bar fed
        // a bad arena and a reset graphics ring.
        let (device, queue, clock) = punchcutter_core::gpu::open_device(
            &adapter,
            &wgpu::DeviceDescriptor {
                label: Some("caustics-bar-device"),
                required_features: features,
                required_limits: limits,
                ..Default::default()
            },
        )
        .context("Vulkan device creation failed")?;
        // **The device is the bar's, and its loss is the bar's exit.**
        // wgpu's default for both of these is a panic,
        // which under `Restart=always` is a bar that comes back, is handed the
        // same bytes by the same sidecars, and dies again. One witness and
        // status 3 is what the unit's `RestartPreventExitStatus` can act on.
        //
        // `Destroyed` is the ordinary teardown and says nothing.
        device.set_device_lost_callback(|reason, message| {
            if reason == wgpu::DeviceLostReason::Destroyed {
                tracing::info!("gpu: the device was destroyed ({message})");
                return;
            }
            tracing::error!(
                "gpu: the device is gone ({reason:?}): {message}. Exiting; this bar is not \
                 coming back on its own (exit 3; the unit does not restart it)."
            );
            std::process::exit(crate::EXIT_DEVICE);
        });
        device.on_uncaptured_error(std::sync::Arc::new(|error| {
            tracing::error!("gpu: {error}. Exiting (exit 3; the unit does not restart it).");
            std::process::exit(crate::EXIT_DEVICE);
        }));

        match clock {
            punchcutter_core::gpu::ShaderClock::Device => {
                tracing::info!("gpu: VK_KHR_shader_clock is on; the shaper holds a deadline")
            }
            // Never silent: a bar without it runs on the operation budget alone,
            // and the operation budget is only as good as its accounting.
            punchcutter_core::gpu::ShaderClock::Absent => tracing::warn!(
                "gpu: this adapter has no VK_KHR_shader_clock — the shaper's run deadline \
                 is off and the caps and the operation budget are the only bound"
            ),
        }

        // What the surface will take, when there is one. Headless — the
        // oracle — fixes all three instead: it compares a table and not a
        // picture, and `Rgba8Unorm` is the variant the baked SPIR-V
        // declares explicitly.
        let (format, alpha_mode, present_mode) = match bar_surface.as_ref() {
            Some(negotiate) => {
                let caps = negotiate.get_capabilities(&adapter);
                if !caps.usages.contains(wgpu::TextureUsages::STORAGE_BINDING) {
                    bail!(
                        "the layer surface does not support STORAGE_BINDING (usages: {:?}); the bar \
                     draws the swapchain from a compute pass and has no fallback",
                        caps.usages
                    );
                }
                let format = if caps.formats.contains(&wgpu::TextureFormat::Rgba8Unorm) {
                    wgpu::TextureFormat::Rgba8Unorm
                } else if caps.formats.contains(&wgpu::TextureFormat::Bgra8Unorm)
                    && features.contains(wgpu::Features::BGRA8UNORM_STORAGE)
                {
                    wgpu::TextureFormat::Bgra8Unorm
                } else {
                    bail!(
                        "no storage-writable surface format available (formats: {:?}, \
                     BGRA8UNORM_STORAGE: {})",
                        caps.formats,
                        features.contains(wgpu::Features::BGRA8UNORM_STORAGE)
                    );
                };

                // The slab is translucent, so the alpha the caustic writes has to mean
                // something. PreMultiplied is what the caustic writes; anything else
                // and the bar will look wrong, which is worth a warning and not worth
                // refusing to run over.
                let alpha_mode = if caps
                    .alpha_modes
                    .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
                {
                    wgpu::CompositeAlphaMode::PreMultiplied
                } else {
                    let fallback = *caps
                        .alpha_modes
                        .first()
                        .context("the surface offers no alpha mode at all")?;
                    tracing::warn!(
                        offered = ?caps.alpha_modes,
                        chosen = ?fallback,
                        "the surface does not offer PreMultiplied alpha; the translucent slab will \
                         composite wrong, but the bar still runs"
                    );
                    fallback
                };

                let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                    wgpu::PresentMode::Mailbox
                } else {
                    tracing::warn!(
                        offered = ?caps.present_modes,
                        "the surface does not offer Mailbox presentation; falling back to Fifo, \
                         which can block on an unmapped popup's frame callback"
                    );
                    wgpu::PresentMode::Fifo
                };

                tracing::info!(
                    ?format,
                    ?alpha_mode,
                    ?present_mode,
                    offered_formats = ?caps.formats,
                    offered_alpha = ?caps.alpha_modes,
                    offered_present = ?caps.present_modes,
                    "surface format negotiated"
                );
                (format, alpha_mode, present_mode)
            }
            None => (
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::PresentMode::Fifo,
            ),
        };

        let bind_group_layout =
            create_layout(&device, "caustics-bar-bgl", &reflected, SET_BAR, format);
        let text_layout = create_layout(
            &device,
            "caustics-bar-text-bgl",
            &reflected,
            SET_TEXT,
            format,
        );
        let shape_layout = create_layout(
            &device,
            "caustics-bar-shape-bgl",
            &reflected,
            SET_SHAPE,
            format,
        );
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("caustics-bar-pipeline-layout"),
            bind_group_layouts: &[
                Some(&bind_group_layout),
                Some(&text_layout),
                Some(&shape_layout),
            ],
            immediate_size: 0,
        });
        let baked = baked_spv(format);
        let pipelines = build_pipelines(&device, &pipeline_layout, baked)
            .context("the bar caustic baked in at build time does not load")?;

        let state = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-state"),
            size: abi::STATE_BUFFER_SIZE,
            // COPY_SRC for the mirror copy, and nothing else: the caustic owns
            // every byte and wgpu zero-initialises a buffer at creation, which
            // is the whole of its initialisation (docs/abi.md, *Who writes what*).
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // What the caustic keeps for itself, at the stride slangc's reflection
        // reports for `Private` (docs/abi.md, *The private buffer*). `STORAGE` alone: no
        // readback, no upload, and the usage flags say so.
        let private = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-bar-private"),
            size: private_stride(&reflected),
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

        // The curve and table buffers start as one element each: nothing can be
        // drawn until `caustics-glyphs`'s first `Bytes` lands, and a zero-sized
        // storage buffer is not a legal binding. `write_bytes` grows each one to
        // the `total` its writer declares, which is why they are `growable` —
        // an icon's delta needs what is already there copied forward.
        let curves = Curves {
            points: growable_buffer(&device, "caustics-bar-points", 16),
            bands: growable_buffer(&device, "caustics-bar-bands", 8),
            band_curves: growable_buffer(&device, "caustics-bar-band-curves", 4),
            glyphs: growable_buffer(&device, "caustics-bar-glyphs", 64),
        };
        let text_group = create_text_group(&device, &text_layout, &reflected, &curves);
        let shape = ShapeBuffers {
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("caustics-bar-shape-params"),
                size: size_of::<punchcutter_shape::ShapeParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            // The arena starts as the front block alone: the caustic reads
            // its own bound out of the header, and a header of zeros is a
            // directory of no faces, which shapes nothing and stalls nothing.
            ot: growable_buffer(
                &device,
                "caustics-bar-ot",
                punchcutter_shape::OT_TABLES_START as u64,
            ),
        };
        let shape_group = create_shape_group(&device, &shape_layout, &reflected, &shape, &strings);

        // Two timestamps, one pass: the device's own answer to "how long did
        // `bar_shape` run". Eight bytes each, and the
        // resolve target must be its own buffer because `QUERY_RESOLVE` and
        // `MAP_READ` may not share one.
        let watchdog = timing.then(|| {
            let set = device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("caustics-bar-shape-watchdog"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            });
            Watchdog {
                set,
                resolved: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("caustics-bar-shape-watchdog-resolved"),
                    size: 16,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("caustics-bar-shape-watchdog-readback"),
                    size: 16,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                period_ns: queue.get_timestamp_period(),
                threshold_ms: 0.0,
            }
        });
        match &watchdog {
            Some(watchdog) => tracing::info!(
                "gpu: the shape pass is timed on the device ({:.1} ns a tick)",
                watchdog.period_ns
            ),
            // Never silent: without it a runaway shape pass is a stuttering
            // desktop and no journal line: a shaper reading a mismatched arena
            // once took 150 ms a dispatch and said nothing.
            None => tracing::warn!(
                "gpu: this adapter has no TIMESTAMP_QUERY — the shape pass has no watchdog"
            ),
        }

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
            format,
            alpha_mode,
            present_mode,
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
            shader_clock: clock,
            format,
            alpha_mode,
            present_mode,
            bind_group_layout,
            text_layout,
            shape_layout,
            pipeline_layout,
            pipelines,
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
            curves,
            text_group,
            shape,
            shape_group,
            shape_dst,
            shape_pending: false,
            watchdog,
            shape_blocked: false,
            shape_cold: true,
            model_pass_ms: None,
            bindings: reflected,
            cap_height: 0.0,
            curve_glyphs: 0,
            curve_totals: [0; 4],
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

    /// The slangc defines and extra flags matching the live swapchain format,
    /// for a reload compile. Mirrors build.rs, which compiles one set per `DstFormat::ALL` with `format.args()`.
    pub fn dst_format_args(&self) -> (&'static str, &'static [&'static str]) {
        surface::dst_format(self.format)
            .unwrap_or_else(|| {
                unreachable!(
                    "surface format {:?} is not one this bar can select",
                    self.format
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
        (self.bar.config.width, self.bar.config.height)
    }

    pub fn popup_size(&self) -> (u32, u32) {
        match &self.popup {
            Some(popup) => (popup.config.width, popup.config.height),
            None => (0, 0),
        }
    }

    /// Resize the bar's swapchain (and its ids texture) after a configure.
    pub fn resize_bar(&mut self, width: u32, height: u32) {
        if (self.bar.config.width, self.bar.config.height) == (width.max(1), height.max(1)) {
            return;
        }
        self.bar.config.width = width.max(1);
        self.bar.config.height = height.max(1);
        if let Some(surface) = &self.bar.surface {
            surface.configure(&self.device, &self.bar.config);
        }
        // A new configuration; the next suboptimal acquire deserves an answer.
        self.bar.refit = false;
        self.bar.ids = create_id_texture(
            &self.device,
            IDS_LABEL,
            self.bar.config.width,
            self.bar.config.height,
        );
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
        if !caps.formats.contains(&self.format) {
            bail!(
                "the popup surface does not offer the bar's format {:?} (offers {:?})",
                self.format,
                caps.formats
            );
        }
        self.popup = Some(configure_surface(
            &self.device,
            Some(wgpu_surface),
            self.format,
            self.alpha_mode,
            self.present_mode,
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
        if (popup.config.width, popup.config.height) == (width.max(1), height.max(1)) {
            return false;
        }
        popup.config.width = width.max(1);
        popup.config.height = height.max(1);
        if let Some(surface) = &popup.surface {
            surface.configure(&self.device, &popup.config);
        }
        popup.refit = false;
        popup.ids = create_id_texture(
            &self.device,
            IDS_LABEL,
            popup.config.width,
            popup.config.height,
        );
        true
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
    /// whole image of a known size: `crate::images::Images` hands over the slots
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
    /// (docs/abi.md, *Bytes*) — the one method for all six.
    ///
    /// The font is read by a sidecar, not the bar. This grows and rebinds:
    /// when `total` does not fit, a fresh buffer with
    /// [`CURVE_HEADROOM`] of tail is created, **what is already there is copied
    /// into it** — an icon's `Bytes` is a delta, so the font behind it has to
    /// survive the growth — and the one group naming it is rebuilt.
    ///
    /// Nothing here reads a byte of the run. The caustic's counts come from the
    /// writer's own `total`: `curve_glyphs` is `BUF_GLYPHS`'s `total /
    /// GLYPH_RECORD`, the `ShapeFont` counts are the `BUF_SHAPE_FONT` body
    /// itself, and the arena's are in its own header, which `ot.slang` clamps
    /// to the words the driver bound. A malformed table draws wrong and stalls
    /// nothing, because every index in `text.slang`, `ot.slang` and
    /// `shape.slang` is bounded.
    pub fn write_bytes(&mut self, buffer: u32, offset: u32, total: u32, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if buffer == abi::wire::BUF_SHAPE_FONT {
            self.write_shape_font(bytes);
            return;
        }
        let Some(label) = buffer_label(buffer) else {
            tracing::warn!("gpu: no buffer {buffer}; {} bytes dropped", bytes.len());
            return;
        };
        let want = u64::from(total).max(u64::from(offset) + bytes.len() as u64);
        let Some(have) = self.slot(buffer).map(|slot| slot.size()) else {
            return;
        };
        if want > have {
            // Rounded up to sixteen bytes: wgpu requires a storage binding's
            // effective size to be a multiple of four, and the headroom
            // multiply does not respect that on its own (a 487575-byte
            // band-curve buffer was refused). Sixteen rather
            // than four because std430 aligns these structs to sixteen anyway.
            let size = ((want as f64 * (1.0 + CURVE_HEADROOM as f64)) as u64)
                .max(want)
                .max(64)
                .next_multiple_of(16);
            let fresh = growable_buffer(&self.device, label, size);
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
            if let Some(old) = self.slot(buffer) {
                encoder.copy_buffer_to_buffer(old, 0, &fresh, 0, have.min(size));
            }
            self.queue.submit(Some(encoder.finish()));
            self.put(buffer, fresh);
            tracing::info!(
                "gpu: {label} grown to {} KiB, holding {want} bytes",
                size / 1024
            );
        }
        if let Some(slot) = self.slot(buffer) {
            self.queue.write_buffer(slot, u64::from(offset), bytes);
        }
        if let Some(i) = curve_index(buffer) {
            self.curve_totals[i] = total;
        }
        if buffer == abi::wire::BUF_GLYPHS {
            // Glyph records actually written, which is what bounds every glyph
            // id the caustic may index: `text.slang` reads `textGlyphs[gid]`
            // with no bound of its own. The writer's `total` is the only thing
            // that knows, and a record is [`GLYPH_RECORD`] bytes.
            self.curve_glyphs = total / GLYPH_RECORD;
        }
        // The watchdog's release: the arena's **front
        // block** is its directory, and rewriting it is the only event that
        // means "the thing the shaper walks is a different thing now". A
        // sidecar that restarts sends the whole arena from offset zero, so
        // this is also what a fixed `caustics-glyphs` looks like from here.
        if buffer == abi::wire::BUF_OT && offset == 0 && self.shape_blocked {
            self.shape_blocked = false;
            tracing::info!("gpu: the arena's front block was rewritten; shaping again");
        }
        // Whatever was shaped against the buffers as they were is owed another
        // pass now that more of them has landed.
        self.shape_pending = true;
    }

    /// `BUF_SHAPE_FONT`: the `font` half of `ShapeParams`, verbatim, and behind
    /// it the four scalars that are the *dispatch's* rather than the font's.
    fn write_shape_font(&mut self, bytes: &[u8]) {
        let expected = size_of::<punchcutter_shape::abi::ShapeFont>();
        if bytes.len() != expected {
            tracing::warn!(
                "gpu: BUF_SHAPE_FONT is {} bytes, not {expected}",
                bytes.len()
            );
            return;
        }
        self.queue.write_buffer(&self.shape.params, 0, bytes);
        // `request_count` and `glyph_capacity` are the batch's, and this bar
        // drives the pass from the block table instead — so they carry the
        // capacities, which is what a reader of the buffer in a debugger wants to see.
        let mut tail = [0u8; 16];
        tail[0..4].copy_from_slice(&SHAPE_BLOCKS.to_le_bytes());
        tail[4..8].copy_from_slice(&(abi::MAX_GLYPHS as u32).to_le_bytes());
        tail[8..12].copy_from_slice(&punchcutter_shape::SHAPE_ABI_VERSION.to_le_bytes());
        // The shaper's wall-clock deadline. Zero where
        // the adapter has no `VK_KHR_shader_clock`, which `Gpu::new` has
        // already warned about — never a deadline that silently does nothing.
        let deadline = match self.shader_clock {
            punchcutter_core::gpu::ShaderClock::Device => {
                punchcutter_shape::abi::SHAPE_DEADLINE_TICKS
            }
            punchcutter_core::gpu::ShaderClock::Absent => 0,
        };
        tail[12..16].copy_from_slice(&deadline.to_le_bytes());
        self.queue
            .write_buffer(&self.shape.params, expected as u64, &tail);
        self.shape_pending = true;
    }

    /// The buffer a `BUF_*` names, or `None` for one outside the six.
    fn slot(&self, buffer: u32) -> Option<&wgpu::Buffer> {
        use abi::wire::*;
        Some(match buffer {
            BUF_POINTS => &self.curves.points,
            BUF_BANDS => &self.curves.bands,
            BUF_BAND_CURVES => &self.curves.band_curves,
            BUF_GLYPHS => &self.curves.glyphs,
            BUF_OT => &self.shape.ot,
            _ => return None,
        })
    }

    /// Swap a grown buffer in, and rebuild the one bind group that names it.
    fn put(&mut self, buffer: u32, fresh: wgpu::Buffer) {
        use abi::wire::*;
        match buffer {
            BUF_POINTS => self.curves.points = fresh,
            BUF_BANDS => self.curves.bands = fresh,
            BUF_BAND_CURVES => self.curves.band_curves = fresh,
            BUF_GLYPHS => self.curves.glyphs = fresh,
            BUF_OT => self.shape.ot = fresh,
            _ => return,
        }
        if curve_index(buffer).is_some() {
            self.text_group = create_text_group(
                &self.device,
                &self.text_layout,
                &self.bindings,
                &self.curves,
            );
        } else {
            self.shape_group = create_shape_group(
                &self.device,
                &self.shape_layout,
                &self.bindings,
                &self.shape,
                &self.strings,
            );
        }
    }

    /// Glyph records the curve buffers hold — the font's, plus the icons the
    /// sidecar appended to them. What the health line reports.
    pub fn curve_glyphs(&self) -> u32 {
        self.curve_glyphs
    }

    /// Bytes the four curve buffers hold, by the writer's own totals.
    pub fn curve_bytes(&self) -> u64 {
        self.curve_totals
            .iter()
            .map(|total| u64::from(*total))
            .sum()
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
    /// `reflection` is that same compile's reflection JSON. The three bind
    /// group layouts were built at startup from the *baked* one and cannot be
    /// rebuilt without rebuilding the pipeline layout and every bind group
    /// hanging off it, so a caustic that moved, added or retyped a binding is
    /// refused here rather than swapped in: what a live reload would otherwise
    /// produce is a dispatch whose descriptors and whose `[[vk::binding]]`
    /// literals disagree — a device-lost on one driver, silently wrong buffers
    /// on another.
    #[allow(clippy::too_many_arguments)]
    pub fn reload_all(
        &mut self,
        reflection: &str,
        build: &[u8],
        compose: &[u8],
        shape: &[u8],
        apply: &[u8],
        visibility: &[u8],
        sample: &[u8],
        draw: &[u8],
    ) -> Result<()> {
        let fresh = slang::bindings(reflection).map_err(|err| {
            anyhow!("reload refused: its reflection cannot be read ({err}); restart the bar")
        })?;
        if let Some(witness) = first_difference(&self.bindings, &fresh) {
            bail!("reload refused: bindings changed ({witness}); restart the bar");
        }
        let pipelines = build_pipelines(
            &self.device,
            &self.pipeline_layout,
            &BakedSpv {
                build,
                compose,
                shape,
                apply,
                visibility,
                sample,
                draw,
            },
        )?;
        self.pipelines = pipelines;
        self.shape_cold = true;
        Ok(())
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
            pass.set_pipeline(&self.pipelines.apply);
            pass.dispatch_workgroups(1, 1, 1);

            let bar_dims = (self.bar.config.width, self.bar.config.height);
            dispatch_target(&mut pass, &self.pipelines, &bar_group, bar_dims);
            if let Some(group) = &popup_group {
                let popup = self.popup.as_ref().expect("a group implies a surface");
                let dims = (popup.config.width, popup.config.height);
                dispatch_target(&mut pass, &self.pipelines, group, dims);
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
        if self.shape_blocked {
            return;
        }
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
            pass.set_pipeline(&self.pipelines.build);
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
            pass.set_pipeline(&self.pipelines.compose);
            pass.dispatch_workgroups(1, 1, 1);
        }
        {
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
                timestamp_writes: self.watchdog.as_ref().map(|watchdog| {
                    wgpu::ComputePassTimestampWrites {
                        query_set: &watchdog.set,
                        beginning_of_pass_write_index: Some(0),
                        end_of_pass_write_index: Some(1),
                    }
                }),
            });
            pass.set_bind_group(0, &group, &[]);
            pass.set_bind_group(1, &self.text_group, &[]);
            pass.set_bind_group(2, &self.shape_group, &[]);
            pass.set_pipeline(&self.pipelines.shape);
            pass.dispatch_workgroups(SHAPE_BLOCKS.div_ceil(SHAPE_GROUP), 1, 1);
        }
        if let Some(watchdog) = &self.watchdog {
            encoder.resolve_query_set(&watchdog.set, 0..2, &watchdog.resolved, 0);
            encoder.copy_buffer_to_buffer(&watchdog.resolved, 0, &watchdog.readback, 0, 16);
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
        self.check_watchdog();
    }

    /// What the device says `bar_shape` cost, and what to do about it.
    ///
    /// **Translate, never decide**: the threshold arrived as an `ACT_WATCHDOG`
    /// from the caustic and the response is fixed — one witness, and no more
    /// shaping until the OpenType arena's front block is written again. The
    /// embassy is here because it is the only side that holds the queue, the
    /// query set and the wall clock the ticks convert against.
    ///
    /// The shaper's in-kernel deadline bounds one *run*; this bounds the
    /// *pass*. An arena that is not an arena makes every run short and far too
    /// many of them, which measured 152 ms a dispatch.
    fn check_watchdog(&mut self) {
        let Some(watchdog) = &self.watchdog else {
            return;
        };
        // Zero until the caustic's first frame has pushed a threshold: a bar
        // whose caustic has not run yet has nothing to compare against. The
        // buffer is **not mapped** on this path and must not be unmapped —
        // unmapping an idle buffer is a validation error, and a validation
        // error is an exit.
        // A cold pass under no threshold yet has still been and gone: the
        // next one is warm.
        if watchdog.threshold_ms <= 0.0 {
            self.shape_cold = false;
            return;
        }
        let mut ticks = [0u64; 2];
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        watchdog
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        // The submission above is already waited out; this is the second poll
        // wgpu-core needs to drain its ready-to-map list.
        if self
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .is_err()
        {
            return;
        }
        // A map that failed left the buffer idle: there is nothing to unmap,
        // and nothing to read either.
        if !matches!(rx.try_recv(), Ok(Ok(()))) {
            return;
        }
        // The view must die before the unmap, hence the block.
        {
            match watchdog.readback.slice(..).get_mapped_range() {
                Ok(view) if view.len() >= 16 => {
                    for (i, slot) in ticks.iter_mut().enumerate() {
                        let mut word = [0u8; 8];
                        word.copy_from_slice(&view[i * 8..i * 8 + 8]);
                        *slot = u64::from_le_bytes(word);
                    }
                }
                // A timestamp that did not come back is not a runaway.
                _ => {}
            }
        }
        watchdog.readback.unmap();

        let Some(elapsed) = ticks[1].checked_sub(ticks[0]) else {
            return;
        };
        let ms = elapsed as f64 * f64::from(watchdog.period_ns) / 1.0e6;
        // The first pass on new pipelines times the driver's JIT, not the
        // arena. A hostile arena still trips on the next pass: the arena write
        // that re-arms shaping is also what asks for a re-shape.
        if std::mem::take(&mut self.shape_cold) {
            tracing::debug!(
                "shape watchdog: the cold pass (the first on new pipelines) took {ms:.1} ms; \
                 not judged"
            );
            return;
        }
        if ms <= watchdog.threshold_ms {
            return;
        }
        tracing::error!(
            "shape watchdog: bar_shape took {ms:.1} ms over {SHAPE_BLOCKS} runs, past the \
             {:.0} ms this caustic allows. Not shaping again until the OpenType arena is \
             written. A sidecar from another tree, or an arena that is not one.",
            watchdog.threshold_ms
        );
        self.shape_blocked = true;
        // What was owed is still owed: the flag the pass cleared goes back on,
        // so the release re-shapes rather than leaving last arena's metrics.
        self.shape_pending = true;
    }

    /// The caustic's `SHAPE_WATCHDOG_MS`, as an `ACT_WATCHDOG`. One number in,
    /// and nothing else changes.
    ///
    /// On a software adapter (lavapipe, what a VM usually has) the number is scaled
    /// by [`WATCHDOG_CPU_SCALE`]: there the pass is CPU time on vCPUs that are
    /// the host's cores, and it has been measured stretching from 70 to 155 ms
    /// while the host was busy beside the guest, tripping a watchdog meant for
    /// a GPU and stopping every popup's text for that bar's life. The tunable
    /// keeps its meaning — a ceiling on a healthy pass on a real GPU — and a
    /// hostile arena (3 ms, with the shaper's own deadline) is still far under
    /// eight times it.
    pub fn set_watchdog_ms(&mut self, ms: u32) {
        let Some(watchdog) = self.watchdog.as_mut() else {
            return;
        };
        let software = self.adapter.get_info().device_type == wgpu::DeviceType::Cpu;
        let ms = f64::from(ms) * if software { WATCHDOG_CPU_SCALE } else { 1.0 };
        if watchdog.threshold_ms == ms {
            return;
        }
        watchdog.threshold_ms = ms;
        tracing::info!(
            "gpu: a shape pass may take {ms:.0} ms{}",
            if software {
                " (a software adapter: the caustic's number times 8)"
            } else {
                ""
            }
        );
    }

    /// Whether the watchdog has stopped the shape pass, for the health line.
    pub fn shape_blocked(&self) -> bool {
        self.shape_blocked
    }

    /// A threshold no pass can meet, for the oracle's watchdog case: the
    /// caustic's `ACT_WATCHDOG` path only carries whole milliseconds.
    #[cfg(test)]
    pub fn set_watchdog_threshold(&mut self, ms: f64) -> bool {
        let Some(watchdog) = self.watchdog.as_mut() else {
            return false;
        };
        watchdog.threshold_ms = ms;
        true
    }

    /// `reload_all` with the baked modules, as `caustics-reload` would deliver an
    /// unchanged caustic: the swap without a compile.
    #[cfg(test)]
    pub fn reload_baked(&mut self) -> Result<()> {
        let spv = baked_spv(self.format);
        self.reload_all(
            BAKED_REFLECTION,
            spv.build,
            spv.compose,
            spv.shape,
            spv.apply,
            spv.visibility,
            spv.sample,
            spv.draw,
        )
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
            group_entries(&self.bindings, SET_BAR, |name| match name {
                "state" => self.state.as_entire_binding(),
                "runs" => self.runs.as_entire_binding(),
                "commands" => self.commands.as_entire_binding(),
                "inputs" => self.inputs.as_entire_binding(),
                "params" => target.params.as_entire_binding(),
                "ids" => wgpu::BindingResource::TextureView(&target.ids),
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
            layout: &self.bind_group_layout,
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
            pass.set_pipeline(&self.pipelines.apply);
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
        refresh_mirror_slice(&self.device, &back, &mut words);
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
        refresh_mirror_slice(&self.device, &back, &mut words);
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
        refresh_mirror_slice(&self.device, &bytes, &mut strings_back);
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
        refresh_mirror_slice(&self.device, &back, &mut words);
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
        let Some(slot) = self.slot(buffer) else {
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
        refresh_mirror_slice(&self.device, &back, &mut words);
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    /// Load `bar_check` as an eighth pipeline, for the oracle alone
    /// (docs/abi.md, *Bindings and the oracle*). Production's `ENTRIES` never names it and the
    /// baked SPIR-V never carries it.
    #[cfg(test)]
    pub fn set_check(&mut self, spirv: &[u8]) -> Result<()> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = passthrough_pipeline(
            &self.device,
            &self.pipeline_layout,
            "caustics-bar-check",
            "bar_check",
            spirv,
            (1, 1, 1),
        );
        if let Some(err) = pollster::block_on(scope.pop()) {
            bail!("bar_check does not load: {err}");
        }
        self.check = Some(pipeline);
        Ok(())
    }
    /// Pull the mirror out of the readback buffer, the device having already
    /// been waited out — `surface::refresh_mirror`, where the second
    /// poll and the keep-the-old-mirror failure policy are explained.
    fn refresh_mirror(&mut self) {
        refresh_mirror(&self.device, &self.mirror_readback, &mut *self.mirror);
    }
}

/// The same readback as `refresh_mirror`, over a slice of words: what the
/// oracle pulls the strings buffer back with, since some of its bytes are the
/// build pass's own.
#[cfg(test)]
fn refresh_mirror_slice(device: &wgpu::Device, buffer: &wgpu::Buffer, out: &mut [u32]) {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    if !matches!(rx.try_recv(), Ok(Ok(()))) {
        return;
    }
    {
        if let Ok(view) = buffer.slice(..).get_mapped_range() {
            for (word, chunk) in out.iter_mut().zip(view.chunks_exact(4)) {
                *word = u32::from_le_bytes(chunk.try_into().expect("four bytes"));
            }
        }
    }
    buffer.unmap();
}

/// visibility → sample → draw for one target, in that order: `bar_sample`
/// reads the ids this frame just wrote, so `hover_id` in the mirror is current
/// as of the pointer position the CPU supplied for this frame.
fn dispatch_target(
    pass: &mut wgpu::ComputePass<'_>,
    pipelines: &Pipelines,
    group: &wgpu::BindGroup,
    (width, height): (u32, u32),
) {
    let groups_x = width.div_ceil(TILE).max(1);
    let groups_y = height.div_ceil(TILE).max(1);
    pass.set_bind_group(0, group, &[]);
    pass.set_pipeline(&pipelines.visibility);
    pass.dispatch_workgroups(groups_x, groups_y, 1);
    pass.set_pipeline(&pipelines.sample);
    pass.dispatch_workgroups(1, 1, 1);
    pass.set_pipeline(&pipelines.draw);
    pass.dispatch_workgroups(groups_x, groups_y, 1);
}

/// # Safety
///
/// `display` and `surface` must be a live `wl_display` and `wl_surface` that
/// outlive the returned surface.
unsafe fn create_surface(
    instance: &wgpu::Instance,
    display: NonNull<std::ffi::c_void>,
    surface: NonNull<std::ffi::c_void>,
) -> Result<wgpu::Surface<'static>> {
    let raw_display_handle = Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
        display,
    )));
    let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(surface));
    // SAFETY: the caller's contract, above.
    unsafe {
        instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle,
                raw_window_handle,
            })
            .context("failed to create a Vulkan surface on the wl_surface")
    }
}

fn configure_surface(
    device: &wgpu::Device,
    surface: Option<wgpu::Surface<'static>>,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    present_mode: wgpu::PresentMode,
    width: u32,
    height: u32,
    target: Target,
) -> Surface {
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::STORAGE_BINDING,
        format,
        width: width.max(1),
        height: height.max(1),
        present_mode,
        desired_maximum_frame_latency: 2,
        alpha_mode,
        color_space: wgpu::SurfaceColorSpace::Auto,
        view_formats: vec![],
    };
    if let Some(surface) = &surface {
        surface.configure(device, &config);
    }
    Surface {
        surface,
        refit: false,
        ids: create_id_texture(device, IDS_LABEL, config.width, config.height),
        params: device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(target.label()),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        config,
    }
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
    curves: &Curves,
) -> wgpu::BindGroup {
    let entries = group_entries(bindings, SET_TEXT, |name| match name {
        "textPoints" => curves.points.as_entire_binding(),
        "textBands" => curves.bands.as_entire_binding(),
        "textBandCurves" => curves.band_curves.as_entire_binding(),
        "textGlyphs" => curves.glyphs.as_entire_binding(),
        other => panic!("{}", unknown_binding(other, SET_TEXT)),
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("caustics-bar-text"),
        layout,
        entries: &entries,
    })
}

fn create_shape_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    bindings: &[Binding],
    shape: &ShapeBuffers,
    strings: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let entries = group_entries(bindings, SET_SHAPE, |name| match name {
        "shapeParams" => shape.params.as_entire_binding(),
        "shapeOt" => shape.ot.as_entire_binding(),
        "shapeUtf8" => strings.as_entire_binding(),
        other => panic!("{}", unknown_binding(other, SET_SHAPE)),
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("caustics-bar-shape"),
        layout,
        entries: &entries,
    })
}

/// One descriptor set's bind-group entries, **by name**, from the same
/// reflection the layout was built from (docs/abi.md, *Bindings and the oracle*).
///
/// The number is the caustic's; this reads it rather than transcribing it, so
/// a binding that moves moves in one file. The `[[vk::binding]]` literals stay — an attribute argument cannot be
/// a module constant — and the reflection they produce is what is read here.
fn group_entries<'a>(
    bindings: &[Binding],
    set: u32,
    mut resource: impl FnMut(&str) -> wgpu::BindingResource<'a>,
) -> Vec<wgpu::BindGroupEntry<'a>> {
    bindings
        .iter()
        .filter(|binding| binding.set == set)
        .map(|binding| wgpu::BindGroupEntry {
            binding: binding.index,
            resource: resource(&binding.name),
        })
        .collect()
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

/// slangc's reflection of the baked program, verbatim — `bar/build.rs` writes
/// it beside the SPIR-V out of the first compile, and every entry point and
/// both [`DstFormat`](caustics_bar_abi::DstFormat) variants reflect the same parameters.
///
/// This is the *only* declaration of the bar's bindings on the CPU side. The
/// `[[vk::binding(n, g)]]` literals in the caustics are the source of truth;
/// they are read here rather than transcribed into hand-written layout
/// entries that tests would have to keep in agreement.
const BAKED_REFLECTION: &str = include_str!(concat!(env!("OUT_DIR"), "/bar.reflection.json"));

/// The bar's own bind group: the state buffer, the rings, the target's images, the facts, the arenas and the samples frame.
const SET_BAR: u32 = 0;
/// The engine's `text.slang`'s four curve buffers, at the bindings that module
/// declares for itself.
const SET_TEXT: u32 = 1;
/// The engine's `shape.slang`'s scalars and tables plus the strings buffer.
const SET_SHAPE: u32 = 2;

/// Every descriptor set the bar builds a layout for, in the order the pipeline
/// layout takes them.
const SETS: [u32; 3] = [SET_BAR, SET_TEXT, SET_SHAPE];

/// The stride slangc reports for `Private`, which is how many bytes the
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
    *slang::struct_offsets(BAKED_REFLECTION)
        .get(structure)
        .unwrap_or_else(|| panic!("the reflection has no `{structure}` struct"))
        .get(field)
        .unwrap_or_else(|| panic!("`{structure}` has no field `{field}`"))
}

/// Every binding the baked program declares, sorted by `(set, index)`.
///
/// Panics rather than returns: a reflection the bar cannot classify means it
/// does not know what descriptors its own caustic wants, and there is no frame
/// worth drawing after that. It is baked at build time, so this can only fail
/// on a slangc whose output format moved — which is a build to fix, not a
/// session to limp through.
fn baked_bindings() -> Vec<Binding> {
    slang::bindings(BAKED_REFLECTION).unwrap_or_else(|err| {
        panic!("the reflection baked beside the bar's SPIR-V cannot be read: {err}")
    })
}

/// One descriptor set's layout entries, in binding order, from the reflection.
///
/// **wgpu-hal numbers a Vulkan binding by an entry's position in this list**,
/// so a gap renumbers every binding after it and the `[[vk::binding(n, g)]]`
/// literals in the caustics — which cannot be expressions — would then name
/// different buffers than the descriptors do, with no error anywhere. A gap is
/// a panic at startup, which makes the hazard unshippable rather than tested
/// for.
fn layout_entries(
    all: &[Binding],
    set: u32,
    format: wgpu::TextureFormat,
) -> Vec<wgpu::BindGroupLayoutEntry> {
    let group: Vec<&Binding> = all.iter().filter(|binding| binding.set == set).collect();
    let mut entries = Vec::with_capacity(group.len());
    for (position, binding) in group.iter().enumerate() {
        if binding.index != position as u32 {
            // `position` is the binding number that should have been here, so
            // it names the hole; the neighbours say where to look for it.
            let before = position
                .checked_sub(1)
                .and_then(|earlier| group.get(earlier))
                .map_or("the start of the set", |earlier| earlier.name.as_str());
            panic!(
                "set {set} has no binding {position}: `{}` comes after `{}` but binds at {}. \
                 wgpu-hal numbers a Vulkan binding by its entry's position in the layout, so a \
                 gap silently renumbers every binding after it — the caustic and the \
                 descriptors would name different buffers with nothing to report it.",
                binding.name, before, binding.index
            );
        }
        entries.push(layout_entry(binding, format));
    }
    entries
}

/// One reflected binding as a wgpu layout entry. Everything is `COMPUTE`: the
/// bar has no other stage.
fn layout_entry(binding: &Binding, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    let ty = match binding.kind {
        BindingKind::Uniform => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            // The struct's size is pinned far more strictly than a floor here
            // would: `bar/src/abi/tests.rs` compares slangc's own field offsets
            // against `offset_of!` on the Rust half, every build.
            min_binding_size: None,
        },
        BindingKind::Storage { read_only, stride } => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            // One element, which is what the caustic indexes by. The buffers
            // are all bound whole.
            min_binding_size: stride.and_then(|stride| NonZeroU64::new(u64::from(stride))),
        },
        BindingKind::StorageTexture { write_only } => wgpu::BindingType::StorageTexture {
            access: if write_only {
                wgpu::StorageTextureAccess::WriteOnly
            } else {
                wgpu::StorageTextureAccess::ReadWrite
            },
            format: storage_texture_format(&binding.name, format),
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        BindingKind::Texture { array } => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: if array {
                wgpu::TextureViewDimension::D2Array
            } else {
                wgpu::TextureViewDimension::D2
            },
            multisampled: false,
        },
        BindingKind::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
    };
    wgpu::BindGroupLayoutEntry {
        binding: binding.index,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

/// The one fact about a binding the reflection cannot carry: a storage image's
/// texel format.
///
/// SPIR-V bakes it into the module, and the `dst` image is declared
/// format-*unknown* on the Bgra8 variant ([`DstFormat::Bgra8`](caustics_bar_abi::DstFormat::Bgra8)) precisely
/// because SPIR-V has no BGRA storage format to name — so slangc reflects no
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

/// One descriptor set's layout.
fn create_layout(
    device: &wgpu::Device,
    label: &str,
    bindings: &[Binding],
    set: u32,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &layout_entries(bindings, set, format),
    })
}

/// What the reflection says the bar asks a driver for: bindings, descriptor
/// sets, and how many of the bindings are storage buffers — the two a driver
/// has a limit on, and the third for the log line.
///
/// Counted rather than written down, because the number moves whenever a
/// caustic adds a buffer.
fn binding_counts(bindings: &[Binding]) -> (usize, usize, usize) {
    let sets: std::collections::BTreeSet<u32> = bindings.iter().map(|b| b.set).collect();
    let storage = bindings
        .iter()
        .filter(|binding| matches!(binding.kind, BindingKind::Storage { .. }))
        .count();
    (bindings.len(), sets.len(), storage)
}

/// The first place two binding lists disagree, named — or `None` when they are
/// the same list.
///
/// Both are sorted by `(set, index)`, so this is a merge: a binding present in
/// one and not the other is named with where it sits, and one present in both
/// is compared whole (a `StructuredBuffer` that became `RWStructuredBuffer`, a
/// texture that grew an array dimension, a renamed parameter). The witness
/// goes into a log line a person has to act on, so it names one thing rather than
/// summarising the diff.
fn first_difference(old: &[Binding], new: &[Binding]) -> Option<String> {
    let where_at = |binding: &Binding| format!("set {} binding {}", binding.set, binding.index);
    let mut olds = old.iter().peekable();
    let mut news = new.iter().peekable();
    loop {
        match (olds.peek(), news.peek()) {
            (None, None) => return None,
            (Some(was), None) => {
                return Some(format!("`{}` at {} is gone", was.name, where_at(was)));
            }
            (None, Some(now)) => {
                return Some(format!("`{}` at {} is new", now.name, where_at(now)));
            }
            (Some(was), Some(now)) => match (was.set, was.index).cmp(&(now.set, now.index)) {
                std::cmp::Ordering::Less => {
                    return Some(format!("`{}` at {} is gone", was.name, where_at(was)));
                }
                std::cmp::Ordering::Greater => {
                    return Some(format!("`{}` at {} is new", now.name, where_at(now)));
                }
                std::cmp::Ordering::Equal => {
                    if was != now {
                        return Some(format!(
                            "{} was `{}` {:?} and is now `{}` {:?}",
                            where_at(was),
                            was.name,
                            was.kind,
                            now.name,
                            now.kind
                        ));
                    }
                    olds.next();
                    news.next();
                }
            },
        }
    }
}

/// The sets the reflection declares are exactly the three the pipeline layout
/// binds, or the bar stops here.
///
/// A fourth set (or a missing one) means a caustic grew a descriptor space
/// nothing on the CPU fills, which the pipeline layout would accept and the
/// first dispatch would read as garbage.
fn assert_sets(bindings: &[Binding]) {
    let found: std::collections::BTreeSet<u32> = bindings.iter().map(|b| b.set).collect();
    let expected: std::collections::BTreeSet<u32> = SETS.into_iter().collect();
    if found != expected {
        panic!(
            "the bar's caustic declares descriptor sets {found:?}, but `bar/src/gpu.rs` builds \
             layouts for {expected:?}; a set with no layout is a dispatch reading descriptors \
             nobody wrote."
        );
    }
}

/// The debug label of the buffer a `BUF_*` names, and the proof that it is one of the six (docs/abi.md, *Bytes*).
fn buffer_label(buffer: u32) -> Option<&'static str> {
    use abi::wire::*;
    Some(match buffer {
        BUF_POINTS => "caustics-bar-points",
        BUF_BANDS => "caustics-bar-bands",
        BUF_BAND_CURVES => "caustics-bar-band-curves",
        BUF_GLYPHS => "caustics-bar-glyphs",
        BUF_SHAPE_FONT => "caustics-bar-shape-params",
        BUF_OT => "caustics-bar-ot",
        _ => return None,
    })
}

/// Which of the four curve buffers a `BUF_*` is, or `None` for a shape table —
/// which is also which bind group a growth rebuilds.
fn curve_index(buffer: u32) -> Option<usize> {
    use abi::wire::*;
    match buffer {
        BUF_POINTS => Some(0),
        BUF_BANDS => Some(1),
        BUF_BAND_CURVES => Some(2),
        BUF_GLYPHS => Some(3),
        _ => None,
    }
}

/// A storage buffer that can be **grown**: `COPY_SRC` beside the usual pair, so
/// what is in it survives into its replacement.
///
/// `punchcutter_core::gpu::storage_buffer` makes a buffer nothing copies out of; this is the bar's own, for the six buffers `caustics-glyphs` fills a run at a time.
fn growable_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// The baked modules matching a swapchain format. Panics on a format
/// [`Gpu::new`] cannot select, which is unreachable: the same match governs
/// the surface-format choice.
fn baked_spv(format: wgpu::TextureFormat) -> &'static BakedSpv<'static> {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => &SPV_RGBA8UNORM,
        wgpu::TextureFormat::Bgra8Unorm => &SPV_BGRA8UNORM,
        other => unreachable!("surface format {other:?} is not one this bar can select"),
    }
}

/// All seven pipelines inside one validation error scope: either every one of
/// them is good, or the caller keeps what it had.
fn build_pipelines(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    spv: &BakedSpv<'_>,
) -> Result<Pipelines> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let build = passthrough_pipeline(
        device,
        layout,
        "caustics-bar-build",
        BUILD_ENTRY,
        spv.build,
        (1, 1, 1),
    );
    let compose = passthrough_pipeline(
        device,
        layout,
        "caustics-bar-compose",
        COMPOSE_ENTRY,
        spv.compose,
        (1, 1, 1),
    );
    let shape = passthrough_pipeline(
        device,
        layout,
        "caustics-bar-shape",
        SHAPE_ENTRY,
        spv.shape,
        (SHAPE_GROUP, 1, 1),
    );
    let apply = passthrough_pipeline(
        device,
        layout,
        "caustics-bar-apply",
        APPLY_ENTRY,
        spv.apply,
        (1, 1, 1),
    );
    let visibility = passthrough_pipeline(
        device,
        layout,
        "caustics-bar-visibility",
        VISIBILITY_ENTRY,
        spv.visibility,
        (TILE, TILE, 1),
    );
    let sample = passthrough_pipeline(
        device,
        layout,
        "caustics-bar-sample",
        SAMPLE_ENTRY,
        spv.sample,
        (1, 1, 1),
    );
    let draw = passthrough_pipeline(
        device,
        layout,
        "caustics-bar-draw",
        DRAW_ENTRY,
        spv.draw,
        (TILE, TILE, 1),
    );
    if let Some(err) = pollster::block_on(scope.pop()) {
        bail!("bar pipeline creation rejected: {err}");
    }
    Ok(Pipelines {
        build,
        compose,
        shape,
        apply,
        visibility,
        sample,
        draw,
    })
}

#[cfg(test)]
mod tests;
