//! The text engine's buffers on a host's device, and the two round trips
//! that keep them current: the `Bytes` runs a glyphs sidecar writes into
//! them (docs/abi.md, *Bytes*), and the `REQ_COVER` a caustic's flagged run
//! becomes (docs/abi.md, *Request*). With them, the watchdog round the pass
//! that walks a sidecar's arena.
//!
//! Lifted out of the bar so a second host draws text the way the bar does,
//! from its own glyphs sidecar, through the same code. Mechanism only, as
//! the rest of this crate: nothing here names a sidecar, a tunable or an
//! entry point. The sidecar a cover goes to and its request kind are
//! [`Covers::new`]'s parameters; the dispatch's two capacities are
//! [`TextBuffers::new`]'s; the threshold a shape pass may take arrives from
//! the caller, who had it from its caustic.
//!
//! **Bound by name.** The six buffers are the engine's, and a caustic that
//! imports `text` and gathers `shape`'s tables declares them under the names
//! the bar's caustic uses (`textPoints`, `textBands`, `textBandCurves`,
//! `textGlyphs`, `shapeParams`, `shapeOt`); [`TextBuffers::resource`]
//! answers for those six, so one table of names serves every host. The
//! strings buffer `shapeUtf8` beside them is the host's own: what is in it
//! is the host's business.
//!
//! **Nothing here reads a byte of a run.** The counts the caustic is bounded
//! by come from the writer's own `total` (the glyph records), the
//! `BUF_SHAPE_FONT` body itself, and the arena's own header, which the
//! engine clamps to the words the driver bound (punchcutter/docs/arena.md,
//! *Bounded in time*).

use std::collections::HashSet;

use caustics_bar_abi::wire::{
    self, BUF_BAND_CURVES, BUF_BANDS, BUF_GLYPHS, BUF_OT, BUF_POINTS, BUF_SHAPE_FONT,
};
use punchcutter_core::gpu::ShaderClock;

/// One glyph record in `BUF_GLYPHS`, in bytes: the divisor that turns its
/// writer's `total` into a count (punchcutter/docs/arena.md, *The curve
/// buffers*).
pub const GLYPH_RECORD: u32 = 64;

/// Headroom left in a buffer when it is (re)created, as a fraction: an icon
/// or a cover appends to the buffers later, and a buffer rebuilt on every
/// append would rebuild its bind group with it. A font is 4 MiB and an icon
/// is 2 KB, so an eighth is hundreds of icons.
pub const CURVE_HEADROOM: f32 = 0.125;

/// How much more a software adapter's shape pass may take than the
/// caustic's number says: see [`Watchdog::set_threshold_ms`].
pub const WATCHDOG_CPU_SCALE: f64 = 8.0;

/// Which bind group a write left stale: the one holding the four curve
/// buffers, or the one holding the shaper's tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Text,
    Shape,
}

/// What one [`TextBuffers::write_bytes`] did, for the host to act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// A buffer was replaced by a bigger one: the group naming it must be
    /// built again before the next dispatch.
    pub regroup: Option<Group>,
    /// The arena's front block was written: the directory the shaper walks
    /// is a different one now, which is what releases a tripped watchdog.
    pub front_block: bool,
    /// Something landed: whatever was shaped against the buffers as they
    /// were is owed another pass.
    pub landed: bool,
}

/// The six buffers of the text engine on one device, and how much of them
/// is written.
pub struct TextBuffers {
    label: String,
    points: wgpu::Buffer,
    bands: wgpu::Buffer,
    band_curves: wgpu::Buffer,
    glyphs: wgpu::Buffer,
    /// `ShapeParams`: the font half from `BUF_SHAPE_FONT`, the dispatch half
    /// from [`TextBuffers::new`]'s two capacities and the device's clock.
    params: wgpu::Buffer,
    /// The arena.
    ot: wgpu::Buffer,
    shader_clock: ShaderClock,
    request_count: u32,
    glyph_capacity: u32,
    /// Glyph records actually written, which is what bounds every glyph id
    /// a caustic may index: `text.slang` reads `textGlyphs[gid]` against the
    /// buffer, and a record read out of the headroom is garbage.
    curve_glyphs: u32,
    /// The four curve buffers' live lengths, as the runs that filled them
    /// declared.
    curve_totals: [u32; 4],
}

impl TextBuffers {
    /// The six, each as small as a legal binding is: nothing can be drawn
    /// until a glyphs sidecar's first `Bytes` lands, and a zero-sized storage
    /// buffer is not one. `label` prefixes every buffer's debug label
    /// (`caustics-bar` makes `caustics-bar-points`); `request_count` and
    /// `glyph_capacity` are the dispatch's half of `ShapeParams`, which a
    /// host that drives the pass from its own block table fills with its
    /// capacities.
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        shader_clock: ShaderClock,
        request_count: u32,
        glyph_capacity: u32,
    ) -> TextBuffers {
        let named = |what: &str| format!("{label}-{what}");
        TextBuffers {
            points: growable_buffer(device, &named("points"), 16),
            bands: growable_buffer(device, &named("bands"), 8),
            band_curves: growable_buffer(device, &named("band-curves"), 4),
            glyphs: growable_buffer(device, &named("glyphs"), 64),
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&named("shape-params")),
                size: size_of::<punchcutter_shape::ShapeParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            // The arena starts as the front block alone: a header of zeros is
            // a directory of no faces, which shapes nothing and stalls
            // nothing.
            ot: growable_buffer(
                device,
                &named("ot"),
                u64::from(punchcutter_shape::OT_TABLES_START),
            ),
            label: label.to_owned(),
            shader_clock,
            request_count,
            glyph_capacity,
            curve_glyphs: 0,
            curve_totals: [0; 4],
        }
    }

    /// The buffer behind one of the engine's reflected names, or `None` for
    /// a name that is not one of the six.
    pub fn resource(&self, name: &str) -> Option<wgpu::BindingResource<'_>> {
        Some(match name {
            "textPoints" => self.points.as_entire_binding(),
            "textBands" => self.bands.as_entire_binding(),
            "textBandCurves" => self.band_curves.as_entire_binding(),
            "textGlyphs" => self.glyphs.as_entire_binding(),
            "shapeParams" => self.params.as_entire_binding(),
            "shapeOt" => self.ot.as_entire_binding(),
            _ => return None,
        })
    }

    /// Put one run of a sidecar's bytes into the buffer it names
    /// (docs/abi.md, *Bytes*): the one method for all six.
    ///
    /// This grows and says so: when `total` does not fit, a fresh buffer
    /// with [`CURVE_HEADROOM`] of tail is created, **what is already there is
    /// copied into it** (a cover's `Bytes` is a delta, so the font behind it
    /// has to survive the growth), and [`Written::regroup`] names the group
    /// the caller must build again.
    pub fn write_bytes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        buffer: u32,
        offset: u32,
        total: u32,
        bytes: &[u8],
    ) -> Written {
        let mut written = Written::default();
        if bytes.is_empty() {
            return written;
        }
        if buffer == BUF_SHAPE_FONT {
            written.landed = self.write_shape_font(queue, bytes);
            return written;
        }
        let Some(label) = self.buffer_label(buffer) else {
            tracing::warn!("gpu: no buffer {buffer}; {} bytes dropped", bytes.len());
            return written;
        };
        let want = u64::from(total).max(u64::from(offset) + bytes.len() as u64);
        let Some(have) = self.slot(buffer).map(|slot| slot.size()) else {
            return written;
        };
        if want > have {
            // Rounded up to sixteen bytes: wgpu requires a storage binding's
            // effective size to be a multiple of four, and the headroom
            // multiply does not respect that on its own (a 487575-byte
            // band-curve buffer was refused). Sixteen rather than four
            // because std430 aligns these structs to sixteen anyway.
            let size = ((want as f64 * (1.0 + CURVE_HEADROOM as f64)) as u64)
                .max(want)
                .max(64)
                .next_multiple_of(16);
            let fresh = growable_buffer(device, &label, size);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(&label),
            });
            if let Some(old) = self.slot(buffer) {
                encoder.copy_buffer_to_buffer(old, 0, &fresh, 0, have.min(size));
            }
            queue.submit(Some(encoder.finish()));
            self.put(buffer, fresh);
            written.regroup = Some(if curve_index(buffer).is_some() {
                Group::Text
            } else {
                Group::Shape
            });
            tracing::info!(
                "gpu: {label} grown to {} KiB, holding {want} bytes",
                size / 1024
            );
        }
        if let Some(slot) = self.slot(buffer) {
            queue.write_buffer(slot, u64::from(offset), bytes);
        }
        if let Some(i) = curve_index(buffer) {
            self.curve_totals[i] = total;
        }
        if buffer == BUF_GLYPHS {
            self.curve_glyphs = total / GLYPH_RECORD;
        }
        written.front_block = buffer == BUF_OT && offset == 0;
        written.landed = true;
        written
    }

    /// `BUF_SHAPE_FONT`: the `font` half of `ShapeParams`, verbatim, and
    /// behind it the four scalars that are the *dispatch's* rather than the
    /// font's. `false` for a body that is not a `ShapeFont`.
    fn write_shape_font(&mut self, queue: &wgpu::Queue, bytes: &[u8]) -> bool {
        let expected = size_of::<punchcutter_shape::abi::ShapeFont>();
        if bytes.len() != expected {
            tracing::warn!(
                "gpu: BUF_SHAPE_FONT is {} bytes, not {expected}",
                bytes.len()
            );
            return false;
        }
        queue.write_buffer(&self.params, 0, bytes);
        let mut tail = [0u8; 16];
        tail[0..4].copy_from_slice(&self.request_count.to_le_bytes());
        tail[4..8].copy_from_slice(&self.glyph_capacity.to_le_bytes());
        tail[8..12].copy_from_slice(&punchcutter_shape::SHAPE_ABI_VERSION.to_le_bytes());
        // The shaper's wall-clock deadline. Zero where the adapter has no
        // `VK_KHR_shader_clock`, which the device's bring-up has already
        // said: never a deadline that silently does nothing.
        let deadline = match self.shader_clock {
            ShaderClock::Device => punchcutter_shape::abi::SHAPE_DEADLINE_TICKS,
            ShaderClock::Absent => 0,
        };
        tail[12..16].copy_from_slice(&deadline.to_le_bytes());
        queue.write_buffer(&self.params, expected as u64, &tail);
        true
    }

    /// Glyph records the curve buffers hold: the font's, plus whatever the
    /// sidecar appended.
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

    /// The buffer a `BUF_*` names, for a test that reads one back; `None`
    /// for `BUF_SHAPE_FONT`, which is half of a uniform, and for one outside
    /// the six.
    pub fn buffer(&self, buffer: u32) -> Option<&wgpu::Buffer> {
        self.slot(buffer)
    }

    fn slot(&self, buffer: u32) -> Option<&wgpu::Buffer> {
        Some(match buffer {
            BUF_POINTS => &self.points,
            BUF_BANDS => &self.bands,
            BUF_BAND_CURVES => &self.band_curves,
            BUF_GLYPHS => &self.glyphs,
            BUF_OT => &self.ot,
            _ => return None,
        })
    }

    fn put(&mut self, buffer: u32, fresh: wgpu::Buffer) {
        match buffer {
            BUF_POINTS => self.points = fresh,
            BUF_BANDS => self.bands = fresh,
            BUF_BAND_CURVES => self.band_curves = fresh,
            BUF_GLYPHS => self.glyphs = fresh,
            BUF_OT => self.ot = fresh,
            _ => {}
        }
    }

    /// The debug label of the buffer a `BUF_*` names, and the proof that it
    /// is one of the six (docs/abi.md, *Bytes*).
    fn buffer_label(&self, buffer: u32) -> Option<String> {
        let what = match buffer {
            BUF_POINTS => "points",
            BUF_BANDS => "bands",
            BUF_BAND_CURVES => "band-curves",
            BUF_GLYPHS => "glyphs",
            BUF_SHAPE_FONT => "shape-params",
            BUF_OT => "ot",
            _ => return None,
        };
        Some(format!("{}-{what}", self.label))
    }
}

/// Which of the four curve buffers a `BUF_*` is, or `None` for a shape
/// table: which is also which bind group a growth rebuilds.
fn curve_index(buffer: u32) -> Option<usize> {
    match buffer {
        BUF_POINTS => Some(0),
        BUF_BANDS => Some(1),
        BUF_BAND_CURVES => Some(2),
        BUF_GLYPHS => Some(3),
        _ => None,
    }
}

/// A storage buffer that can be **grown**: `COPY_SRC` beside the usual pair,
/// so what is in it survives into its replacement.
pub fn growable_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

// ---------------------------------------------------------------------------
// The watchdog
// ---------------------------------------------------------------------------

/// A timestamp pair round one compute pass, somewhere to resolve it and
/// somewhere to read it.
struct Timer {
    set: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// Nanoseconds per tick, from the queue.
    period_ns: f32,
}

/// The watchdog round a shape pass: the device's own answer to "how long
/// did it run", judged against a threshold the caustic sent, and the stop
/// that follows a trip.
///
/// **Translate, never decide**: the threshold is the caustic's and the
/// response is fixed. One line, and no more shaping until the arena's front
/// block is written again ([`Watchdog::release`]). The shaper's in-kernel
/// deadline bounds one *run*; this bounds the *pass*. An arena that is not
/// an arena makes every run short and far too many of them, which measured
/// 152 ms a dispatch.
pub struct Watchdog {
    timer: Option<Timer>,
    /// The threshold, ms; zero until the caustic has sent one, which keeps
    /// the watchdog silent before the caustic has run at all.
    threshold_ms: f64,
    /// The next timed pass is the first since the pipelines were created or
    /// swapped, and is logged but not judged: on lavapipe it times llvmpipe
    /// building the new variant (12 ms warm, 18 s with the cache off), not
    /// the arena.
    cold: bool,
    /// A pass ran over the threshold: nothing is shaped until the arena's
    /// front block is written again.
    blocked: bool,
    /// The adapter is a CPU: see [`Watchdog::set_threshold_ms`].
    software: bool,
    /// The entry point timed, and how many runs a pass dispatches, for the
    /// line a trip writes.
    entry: &'static str,
    runs: u32,
}

impl Watchdog {
    /// The query set and its two buffers, when the device has
    /// `TIMESTAMP_QUERY` (`timing`), and one line either way: without it a
    /// runaway shape pass is a stuttering desktop and no journal line.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        timing: bool,
        software: bool,
        label: &str,
        entry: &'static str,
        runs: u32,
    ) -> Watchdog {
        let timer = timing.then(|| Timer {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some(&format!("{label}-shape-watchdog")),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            // The resolve target must be its own buffer: `QUERY_RESOLVE` and
            // `MAP_READ` may not share one.
            resolved: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label}-shape-watchdog-resolved")),
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label}-shape-watchdog-readback")),
                size: 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            period_ns: queue.get_timestamp_period(),
        });
        match &timer {
            Some(timer) => tracing::info!(
                "gpu: the shape pass is timed on the device ({:.1} ns a tick)",
                timer.period_ns
            ),
            // Never silent: a shaper reading a mismatched arena once took
            // 150 ms a dispatch and said nothing.
            None => tracing::warn!(
                "gpu: this adapter has no TIMESTAMP_QUERY — the shape pass has no watchdog"
            ),
        }
        Watchdog {
            timer,
            threshold_ms: 0.0,
            cold: true,
            blocked: false,
            software,
            entry,
            runs,
        }
    }

    /// The pair to hand the timed pass's descriptor, or `None` without a
    /// timer. Round **that** pass and no other: it is the one that walks
    /// bytes a sidecar sent.
    pub fn timestamp_writes(&self) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        self.timer
            .as_ref()
            .map(|timer| wgpu::ComputePassTimestampWrites {
                query_set: &timer.set,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: Some(1),
            })
    }

    /// Resolve the pair and copy it where it can be mapped, at the end of the
    /// encoder that holds the timed pass.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(timer) = &self.timer {
            encoder.resolve_query_set(&timer.set, 0..2, &timer.resolved, 0);
            encoder.copy_buffer_to_buffer(&timer.resolved, 0, &timer.readback, 0, 16);
        }
    }

    /// What the device says the pass cost, once its submission has been
    /// waited out, and what to do about it: `true` when it tripped, after
    /// which [`Watchdog::blocked`] holds until a release. What was owed is
    /// still owed: a caller that cleared its pending flag for the pass sets
    /// it again on a trip, so the release re-shapes.
    pub fn check(&mut self, device: &wgpu::Device) -> bool {
        let Some(timer) = &self.timer else {
            return false;
        };
        // Zero until the caustic's first frame has sent a threshold. The
        // buffer is **not mapped** on this path and must not be unmapped:
        // unmapping an idle buffer is a validation error, and a validation
        // error is an exit. A cold pass under no threshold yet has still been
        // and gone: the next one is warm.
        if self.threshold_ms <= 0.0 {
            self.cold = false;
            return false;
        }
        let mut ticks = [0u64; 2];
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        timer
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        // The submission is already waited out; this is the second poll
        // wgpu-core needs to drain its ready-to-map list.
        if device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
            return false;
        }
        // A map that failed left the buffer idle: there is nothing to unmap,
        // and nothing to read either.
        if !matches!(rx.try_recv(), Ok(Ok(()))) {
            return false;
        }
        // The view must die before the unmap, hence the block.
        {
            if let Ok(view) = timer.readback.slice(..).get_mapped_range()
                && view.len() >= 16
            {
                for (i, slot) in ticks.iter_mut().enumerate() {
                    let mut word = [0u8; 8];
                    word.copy_from_slice(&view[i * 8..i * 8 + 8]);
                    *slot = u64::from_le_bytes(word);
                }
            }
            // A timestamp that did not come back is not a runaway.
        }
        timer.readback.unmap();

        let Some(elapsed) = ticks[1].checked_sub(ticks[0]) else {
            return false;
        };
        let ms = elapsed as f64 * f64::from(timer.period_ns) / 1.0e6;
        // The first pass on new pipelines times the driver's JIT, not the
        // arena. A hostile arena still trips on the next pass: the arena write
        // that re-arms shaping is also what asks for a re-shape.
        if std::mem::take(&mut self.cold) {
            tracing::debug!(
                "shape watchdog: the cold pass (the first on new pipelines) took {ms:.1} ms; \
                 not judged"
            );
            return false;
        }
        if ms <= self.threshold_ms {
            return false;
        }
        tracing::error!(
            "shape watchdog: {} took {ms:.1} ms over {} runs, past the {:.0} ms this caustic \
             allows. Not shaping again until the OpenType arena is written. A sidecar from \
             another tree, or an arena that is not one.",
            self.entry,
            self.runs,
            self.threshold_ms
        );
        self.blocked = true;
        true
    }

    /// The caustic's threshold, whole milliseconds. One number in, and
    /// nothing else changes.
    ///
    /// On a software adapter (lavapipe, what a VM usually has) the number is
    /// scaled by [`WATCHDOG_CPU_SCALE`]: there the pass is CPU time on vCPUs
    /// that are the host's cores, and it has been measured stretching from
    /// 70 to 155 ms while the host was busy beside the guest, tripping a
    /// watchdog meant for a GPU. The tunable keeps its meaning (a ceiling on
    /// a healthy pass on a real GPU) and a hostile arena (3 ms, with the
    /// shaper's own deadline) is still far under eight times it.
    pub fn set_threshold_ms(&mut self, ms: u32) {
        if self.timer.is_none() {
            return;
        }
        let software = self.software;
        let ms = f64::from(ms) * if software { WATCHDOG_CPU_SCALE } else { 1.0 };
        if self.threshold_ms == ms {
            return;
        }
        self.threshold_ms = ms;
        tracing::info!(
            "gpu: a shape pass may take {ms:.0} ms{}",
            if software {
                " (a software adapter: the caustic's number times 8)"
            } else {
                ""
            }
        );
    }

    /// A threshold set as it is, unscaled, for a test that needs one no pass
    /// can meet. `false` without a timer.
    pub fn set_threshold(&mut self, ms: f64) -> bool {
        if self.timer.is_none() {
            return false;
        }
        self.threshold_ms = ms;
        true
    }

    /// Whether a trip has stopped the shape pass.
    pub fn blocked(&self) -> bool {
        self.blocked
    }

    /// The arena's front block was rewritten: the thing the shaper walks is
    /// a different thing now, so shaping goes on. A sidecar that restarts
    /// sends the whole arena from offset zero, so this is also what a fixed
    /// glyphs sidecar looks like from here.
    pub fn release(&mut self) {
        if self.blocked {
            self.blocked = false;
            tracing::info!("gpu: the arena's front block was rewritten; shaping again");
        }
    }

    /// New pipelines: the next timed pass is cold.
    pub fn swapped(&mut self) {
        self.cold = true;
    }
}

// ---------------------------------------------------------------------------
// Covers
// ---------------------------------------------------------------------------

/// Distinct strings [`Covers::ask`] will ever ask about: a host's own bound
/// on a question whose answer is another process's memory. The pipe caps
/// what it remembers at 4096 (docs/abi.md, *Request*); this is well inside
/// that.
pub const MAX_COVERED: usize = 1024;

/// The strings a cover request has gone out for (docs/abi.md, *Request*):
/// one question per distinct string, for the life of the glyph space it
/// was asked in.
///
/// The host has the bytes (they are in its own copy of the buffer the
/// caustic's flagged run was shaped from) and sends them as they are. It
/// decodes nothing: which codepoints are missing, which face has them and
/// whether any face does is the glyphs sidecar's business.
#[derive(Debug)]
pub struct Covers {
    sidecar: &'static str,
    kind: u32,
    covered: HashSet<String>,
    capped: bool,
}

impl Covers {
    /// Covers asked of `sidecar` as requests of `kind` (the bar's and the
    /// canvas's: `caustics-glyphs`, `REQ_COVER`).
    pub fn new(sidecar: &'static str, kind: u32) -> Covers {
        Covers {
            sidecar,
            kind,
            covered: HashSet::new(),
            capped: false,
        }
    }

    /// One string to cover, cut at a character boundary to the wire's
    /// `MAX_NAME_LEN` (a longer one loses its tail's glyphs and nothing
    /// else): the `(sidecar, kind, text)` of the request to send, or `None`
    /// when it was asked before, is empty, or the bound is reached.
    pub fn ask(&mut self, text: &str) -> Option<(&'static str, u32, String)> {
        let text = cut(text, wire::MAX_NAME_LEN);
        if text.is_empty() || self.covered.contains(text) {
            return None;
        }
        if self.covered.len() >= MAX_COVERED {
            // A host whose text has produced a thousand distinct unshapeable
            // strings is looking at a font that is missing a script, not at a
            // string: one witness, and it stops asking.
            if !self.capped {
                self.capped = true;
                tracing::info!(
                    "text: {MAX_COVERED} strings have been sent for cover; not asking again"
                );
            }
            return None;
        }
        self.covered.insert(text.to_owned());
        Some((self.sidecar, self.kind, text.to_owned()))
    }

    /// A `Font` frame: a different glyph space, so every string covered in
    /// the last one must be asked about again.
    pub fn forget(&mut self) {
        self.covered.clear();
    }

    /// Strings asked about.
    pub fn len(&self) -> usize {
        self.covered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.covered.is_empty()
    }
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let end = (0..=max)
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    &text[..end]
}

#[cfg(test)]
mod tests;
