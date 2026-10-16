//! `ext_image_copy_capture_v1` over a foreign-toplevel source — one hover
//! preview at a time.
//!
//! The sequence, which is the part worth writing down (the sequence follows COSMIC's app-list applet, `cosmic-app-list/src/wayland_handler.rs`; no code is copied):
//!
//! 1. `ext_foreign_toplevel_image_capture_source_manager_v1.create_source`
//!    with the window's ext handle — the piece a compositor must offer for previews (niri needs a patch for it at the time of writing), and the reason the bar keeps the handle map at all.
//! 2. `ext_image_copy_capture_manager_v1.create_session(source, 0)`. Options
//!    `0` means no cursors: the pointer is over the dock, not the window.
//! 3. The session answers with a batch of constraints — `buffer_size`, one or
//!    more `shm_format`, then `done`. A patched niri offers **shm only**
//!    (`Argb8888`/`Xrgb8888`/`Abgr8888`/`Xbgr8888`, `dma: None`), so the
//!    dmabuf events are ignored and one `wl_shm` buffer is allocated in the
//!    first 8888 format offered.
//! 4. `create_frame`, `attach_buffer`, `damage_buffer` over the whole thing,
//!    `capture`.
//! 5. `ready` — the frame is in the buffer, the client may read and re-use it.
//!    Convert to straight RGBA8, send `Update::Preview`, destroy the frame,
//!    and wait [`MIN_INTERVAL`] before asking for another. `failed` and
//!    `stopped` end it.
//!
//! Previews are stills at no more than 2 fps because the capture this was
//! written against has no dmabuf path, and every shm frame is a CPU copy. One
//! session exists at a time, which is why the ABI has exactly one preview
//! texture slot.
//!
//! This feeder owns no timer. It says when it next wants a frame through
//! [`Capture::next_deadline`] and the embassy, which owns the loop, calls
//! [`Capture::tick`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay_client_toolkit::globals::ProvidesBoundGlobal;
use smithay_client_toolkit::reexports::protocols::ext::{
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};
use smithay_client_toolkit::shm::raw::RawPool;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use wayland_client::backend::protocol::Message;
use wayland_client::backend::{Backend, ObjectData, ObjectId};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use super::toplevels::Handle;
use super::{CaptureData, CaptureQueue, FeederData, Feeders};
use crate::model::{Preview, Update, Updates};

/// The floor on the preview refresh rate: no more than 2 fps. A hover preview
/// is a still, and each shm frame is a full copy through the CPU, so the bar
/// never becomes the screen recorder it is drawing.
pub const MIN_INTERVAL: Duration = Duration::from_millis(500);

/// The preview texture slot is 2048²: one fixed slot, since one capture
/// session exists at a time. A
/// capture larger than this is requested at the compositor's size — the
/// compositor's size always wins — and downscaled by an integer factor on the
/// way into the `Update`, so nothing that reaches the slot overflows it.
pub const MAX_EDGE: u32 = 2048;

/// A bound `wl_shm`, wrapped so `RawPool` will take it.
///
/// sctk's `RawPool::new` wants a `ProvidesBoundGlobal<WlShm, 1>`, which its
/// own `Shm` state implements. The bar does not necessarily have one — and if
/// it grows one it will be a *second* `wl_shm` object with sctk's user data —
/// so the capture feeder binds its own and provides the trait here. Nothing else about
/// `RawPool` needs the embassy: the pool is constructed through
/// `send_constructor` with sctk's own `ObjectData` and the buffer with
/// [`ReleaseIgnored`], so neither `wl_shm_pool` nor `wl_buffer` needs a
/// `Dispatch` impl anywhere.
///
/// `RawPool` and not `SlotPool`: niri's shm render path `ensure!`s that the
/// *pool's* length is exactly `stride * height`, and `SlotPool` rounds its pool
/// up to 64 bytes. A 720×456 window happens to be a multiple of 64 and passes;
/// a 2881×1689 one fails every frame with niri's "invalid buffer format or
/// size". A raw pool is exactly as long as asked.
#[derive(Debug)]
struct BoundShm(WlShm);

impl ProvidesBoundGlobal<WlShm, 1> for BoundShm {
    fn bound_global(&self) -> Result<WlShm, smithay_client_toolkit::error::GlobalError> {
        Ok(self.0.clone())
    }
}

/// The `wl_buffer`'s object data: `release` is the only event a buffer gets,
/// and this feeder reads the pixels on the frame's `ready` (which the protocol
/// licenses) rather than waiting for it, so there is nothing to record.
struct ReleaseIgnored;

impl ObjectData for ReleaseIgnored {
    fn event(
        self: Arc<Self>,
        _: &Backend,
        _: Message<ObjectId, OwnedFd>,
    ) -> Option<Arc<dyn ObjectData>> {
        None
    }

    fn destroyed(&self, _: ObjectId) {}
}

/// The buffer a session captures into, and everything needed to read it back.
/// Exactly `stride * height` bytes of pool, one buffer at offset 0.
struct Alloc {
    pool: RawPool,
    buffer: WlBuffer,
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
}

impl Drop for Alloc {
    fn drop(&mut self) {
        // The buffer first, then the pool (`RawPool`'s own `Drop`).
        self.buffer.destroy();
    }
}

/// One live capture session, for exactly one window.
struct Session {
    window: u64,
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    /// The frame currently in flight, if any. At most one: `capture` on a
    /// second frame before the first is done is a protocol error.
    frame: Option<ExtImageCopyCaptureFrameV1>,
    /// Constraints accumulated since the last `done`.
    pending_size: Option<(u32, u32)>,
    pending_formats: Vec<wl_shm::Format>,
    alloc: Option<Alloc>,
    /// The earliest instant at which the next frame may be asked for.
    next: Instant,
    /// Whether a `started` witness has been logged for this session.
    announced: bool,
}

impl Session {
    fn destroy(mut self) {
        if let Some(frame) = self.frame.take() {
            frame.destroy();
        }
        // Drop the buffer (and with it the wl_buffer and the pool) before the
        // session, so nothing outlives the objects it was made for.
        self.alloc = None;
        self.session.destroy();
        self.source.destroy();
    }
}

/// The capture feeder.
pub struct Capture {
    source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    copy_manager: Option<ExtImageCopyCaptureManagerV1>,
    shm: Option<BoundShm>,
    queue: Box<dyn CaptureQueue>,
    updates: Updates,
    session: Option<Session>,
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("available", &self.is_available())
            .field("window", &self.session.as_ref().map(|s| s.window))
            .finish()
    }
}

impl Capture {
    pub fn new(
        source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
        copy_manager: Option<ExtImageCopyCaptureManagerV1>,
        shm: Option<WlShm>,
        queue: Box<dyn CaptureQueue>,
        updates: Updates,
    ) -> Self {
        Self {
            source_manager,
            copy_manager,
            shm: shm.map(BoundShm),
            queue,
            updates,
            session: None,
        }
    }

    /// Whether every global a capture needs was bound.
    pub fn is_available(&self) -> bool {
        self.source_manager.is_some() && self.copy_manager.is_some() && self.shm.is_some()
    }

    /// The window a session is live for, if any.
    pub fn window(&self) -> Option<u64> {
        self.session.as_ref().map(|s| s.window)
    }

    /// `Action::PreviewWanted`. `Some(id)` starts (or keeps) a session for
    /// that window; `None` tears down.
    pub fn want(&mut self, want: Option<u64>, handles: &HashMap<u64, Handle>) {
        let Some(id) = want else {
            self.teardown("no longer wanted");
            return;
        };
        if self.window() == Some(id) {
            return;
        }
        self.teardown("superseded");

        let (Some(source_manager), Some(copy_manager)) =
            (self.source_manager.as_ref(), self.copy_manager.as_ref())
        else {
            self.fail(id, "the capture globals are absent");
            return;
        };
        if self.shm.is_none() {
            self.fail(id, "wl_shm is absent");
            return;
        }
        let Some(handle) = handles.get(&id) else {
            self.fail(id, "no toplevel handle for that window");
            return;
        };

        let source = self.queue.create_source(source_manager, handle);
        let session = self.queue.create_session(copy_manager, &source);
        self.session = Some(Session {
            window: id,
            source,
            session,
            frame: None,
            pending_size: None,
            pending_formats: Vec::new(),
            alloc: None,
            next: Instant::now(),
            announced: false,
        });
    }

    /// When the feeder next wants a frame, if a session is live and idle.
    pub fn next_deadline(&self) -> Option<Instant> {
        let session = self.session.as_ref()?;
        (session.frame.is_none() && session.alloc.is_some()).then_some(session.next)
    }

    /// Ask for a frame if one is due. The embassy calls this from the timer it
    /// armed on [`Self::next_deadline`].
    pub fn tick(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.frame.is_some() || Instant::now() < session.next {
            return;
        }
        let Some(alloc) = session.alloc.as_ref() else {
            return;
        };

        let frame = self.queue.create_frame(&session.session);
        frame.attach_buffer(&alloc.buffer);
        frame.damage_buffer(0, 0, alloc.width as i32, alloc.height as i32);
        frame.capture();
        session.frame = Some(frame);
    }

    /// Tear the session down, with a reason for the witness line.
    fn teardown(&mut self, why: &str) {
        if let Some(session) = self.session.take() {
            tracing::info!(
                "capture session for window {}: stopped ({why})",
                session.window
            );
            session.destroy();
        }
    }

    fn fail(&mut self, window: u64, why: &str) {
        tracing::info!("capture session for window {window}: failed ({why})");
        let _ = self.updates.send(Update::PreviewFailed {
            window,
            why: why.to_owned(),
        });
    }

    /// A constraints batch ended. Allocate (or reallocate) the buffer, then
    /// let the next tick ask for a frame.
    fn constraints_done(&mut self) {
        let Some(shm) = self.shm.as_ref() else { return };
        let Some(session) = self.session.as_mut() else {
            return;
        };

        let Some((width, height)) = session.pending_size else {
            tracing::info!(
                "capture session for window {}: failed (constraints without a buffer size)",
                session.window
            );
            return;
        };
        let Some(format) = session.pending_formats.iter().copied().find(is_8888) else {
            let window = session.window;
            let why = "no 8888 shm format offered".to_owned();
            tracing::info!("capture session for window {window}: failed ({why})");
            let _ = self.updates.send(Update::PreviewFailed { window, why });
            self.teardown("unusable constraints");
            return;
        };

        // Constraints may be re-sent when the window resizes; only reallocate
        // when they actually moved.
        let matches = session
            .alloc
            .as_ref()
            .is_some_and(|a| a.width == width && a.height == height && a.format == format);
        if matches {
            return;
        }

        let stride = width * 4;
        let len = (stride as usize) * (height as usize);
        // Exactly `len`: niri checks the pool length against `stride * height`
        // (see `BoundShm`), so no rounding, no slack.
        let alloc = RawPool::new(len.max(4), shm)
            .map_err(|e| e.to_string())
            .map(|mut pool| {
                let buffer = pool.create_buffer_raw(
                    0,
                    width as i32,
                    height as i32,
                    stride as i32,
                    format,
                    Arc::new(ReleaseIgnored),
                );
                Alloc {
                    pool,
                    buffer,
                    width,
                    height,
                    stride,
                    format,
                }
            });

        match alloc {
            Ok(alloc) => {
                if !session.announced {
                    tracing::info!(
                        "capture session for window {}: started ({width}x{height} {format:?})",
                        session.window
                    );
                    session.announced = true;
                } else {
                    tracing::info!(
                        "capture session for window {}: reallocated ({width}x{height} {format:?})",
                        session.window
                    );
                }
                session.alloc = Some(alloc);
                session.next = Instant::now();
            }
            Err(why) => {
                let window = session.window;
                tracing::info!("capture session for window {window}: failed ({why})");
                let _ = self.updates.send(Update::PreviewFailed { window, why });
                self.teardown("allocation failed");
            }
        }
    }

    /// A frame landed in the buffer.
    fn frame_ready(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Some(frame) = session.frame.take() {
            // "After receiving this event, the client must destroy the object."
            frame.destroy();
        }
        session.next = Instant::now() + MIN_INTERVAL;

        let Some(alloc) = session.alloc.as_mut() else {
            return;
        };
        // The protocol licenses reading here — "the buffer may be re-used by
        // the client after this event" — regardless of whether the matching
        // `wl_buffer.release` has arrived yet.
        let bytes: &[u8] = &alloc.pool.mmap()[..];
        let preview = to_rgba(bytes, alloc.width, alloc.height, alloc.stride, alloc.format);
        let _ = self.updates.send(Update::Preview(Preview {
            window: session.window,
            width: preview.0,
            height: preview.1,
            rgba: preview.2,
        }));
    }

    fn frame_failed(&mut self, reason: WEnum<ext_image_copy_capture_frame_v1::FailureReason>) {
        use ext_image_copy_capture_frame_v1::FailureReason;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let window = session.window;
        if let Some(frame) = session.frame.take() {
            frame.destroy();
        }

        match reason.into_result() {
            // "The client should re-allocate its buffers and retry": the
            // compositor follows this with a fresh constraints batch, so drop the buffer and wait for the `done` that reallocates it. Tearing
            // the session down here instead would leave the hover with no
            // preview until the pointer left and came back.
            Ok(FailureReason::BufferConstraints) => {
                tracing::info!(
                    "capture session for window {window}: failed (buffer_constraints), reallocating"
                );
                session.alloc = None;
            }
            other => {
                let why = match other {
                    Ok(reason) => format!("{reason:?}"),
                    Err(raw) => format!("unknown reason {raw}"),
                };
                tracing::info!("capture session for window {window}: failed ({why})");
                let _ = self.updates.send(Update::PreviewFailed { window, why });
                self.teardown("frame failed");
            }
        }
    }
}

/// The four 32 bpp formats this feeder accepts (what niri offers).
fn is_8888(format: &wl_shm::Format) -> bool {
    matches!(
        format,
        wl_shm::Format::Argb8888
            | wl_shm::Format::Xrgb8888
            | wl_shm::Format::Abgr8888
            | wl_shm::Format::Xbgr8888
    )
}

// ---------------------------------------------------------------------------
// shm → straight RGBA8
// ---------------------------------------------------------------------------

/// The integer factor a capture must shrink by to fit the preview slot.
fn scale_factor(width: u32, height: u32) -> u32 {
    let f = width.div_ceil(MAX_EDGE).max(height.div_ceil(MAX_EDGE));
    f.max(1)
}

/// One shm buffer into what `model::Preview` promises: straight
/// (non-premultiplied) RGBA8, row-major, top-down, tightly packed.
///
/// wl_shm's 8888 formats name a little-endian 32-bit word, so `Argb8888` is
/// the bytes `B G R A` in memory and `Abgr8888` is `R G B A` — which is why
/// only two of the four need their channels moved. The `X` variants carry no
/// alpha and become opaque. Alpha-bearing shm buffers are premultiplied by
/// the wl_shm spec, so they are divided back out here.
///
/// Returns `(width, height, rgba)` after any downscale.
pub fn to_rgba(
    src: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
) -> (u32, u32, Vec<u8>) {
    let factor = scale_factor(width, height);
    let out_w = width.div_ceil(factor);
    let out_h = height.div_ceil(factor);
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];

    for oy in 0..out_h {
        for ox in 0..out_w {
            // Box average over the factor x factor block this output pixel
            // covers, clamped to the source edges.
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for dy in 0..factor {
                let sy = oy * factor + dy;
                if sy >= height {
                    break;
                }
                for dx in 0..factor {
                    let sx = ox * factor + dx;
                    if sx >= width {
                        break;
                    }
                    let at = (sy as usize) * (stride as usize) + (sx as usize) * 4;
                    let Some(px) = src.get(at..at + 4) else {
                        continue;
                    };
                    let rgba = unpack(px, format);
                    for c in 0..4 {
                        sum[c] += rgba[c] as u32;
                    }
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let at = ((oy as usize) * (out_w as usize) + ox as usize) * 4;
            for c in 0..4 {
                out[at + c] = (sum[c] / n) as u8;
            }
        }
    }
    (out_w, out_h, out)
}

/// One source pixel to straight RGBA8.
fn unpack(px: &[u8], format: wl_shm::Format) -> [u8; 4] {
    let (r, g, b, a) = match format {
        // 0xAARRGGBB little-endian: B G R A in memory.
        wl_shm::Format::Argb8888 => (px[2], px[1], px[0], px[3]),
        wl_shm::Format::Xrgb8888 => (px[2], px[1], px[0], 255),
        // 0xAABBGGRR little-endian: R G B A in memory, already in order.
        wl_shm::Format::Abgr8888 => (px[0], px[1], px[2], px[3]),
        wl_shm::Format::Xbgr8888 => (px[0], px[1], px[2], 255),
        // Never reached: `is_8888` gates the format choice. Opaque grey is a
        // visible-but-harmless answer if it ever is.
        _ => (128, 128, 128, 255),
    };
    if a == 0 {
        return [0, 0, 0, 0];
    }
    if a == 255 {
        return [r, g, b, a];
    }
    let straight = |c: u8| ((c as u32 * 255) / a as u32).min(255) as u8;
    [straight(r), straight(g), straight(b), a]
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// The source manager and the copy-capture manager have no events; the source
/// object has none either. The impls exist so the objects can be bound and
/// created at all.
impl<D> Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, FeederData, D> for Feeders
where
    D: Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, FeederData> + 'static,
{
    fn event(
        _: &mut D,
        _: &ExtForeignToplevelImageCaptureSourceManagerV1,
        _: <ExtForeignToplevelImageCaptureSourceManagerV1 as Proxy>::Event,
        _: &FeederData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
    }
}

impl<D> Dispatch<ExtImageCopyCaptureManagerV1, FeederData, D> for Feeders
where
    D: Dispatch<ExtImageCopyCaptureManagerV1, FeederData> + 'static,
{
    fn event(
        _: &mut D,
        _: &ExtImageCopyCaptureManagerV1,
        _: <ExtImageCopyCaptureManagerV1 as Proxy>::Event,
        _: &FeederData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
    }
}

impl<D> Dispatch<ExtImageCaptureSourceV1, CaptureData, D> for Feeders
where
    D: Dispatch<ExtImageCaptureSourceV1, CaptureData> + 'static,
{
    fn event(
        _: &mut D,
        _: &ExtImageCaptureSourceV1,
        _: <ExtImageCaptureSourceV1 as Proxy>::Event,
        _: &CaptureData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
    }
}

/// `wl_shm` is bound here only to make pools with; its `format` events say
/// what the *compositor* can show, which is not what a capture session
/// negotiates, so they are ignored.
impl<D> Dispatch<WlShm, FeederData, D> for Feeders
where
    D: Dispatch<WlShm, FeederData> + 'static,
{
    fn event(
        _: &mut D,
        _: &WlShm,
        _: wl_shm::Event,
        _: &FeederData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
    }
}

impl<D> Dispatch<ExtImageCopyCaptureSessionV1, CaptureData, D> for Feeders
where
    D: Dispatch<ExtImageCopyCaptureSessionV1, CaptureData> + AsMut<Feeders> + 'static,
{
    fn event(
        state: &mut D,
        session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &CaptureData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let capture = &mut state.as_mut().capture;
        // A session that is not the live one is being torn down; its trailing
        // events are not ours to act on.
        if capture
            .session
            .as_ref()
            .is_none_or(|s| &s.session != session)
        {
            return;
        }

        match event {
            Event::BufferSize { width, height } => {
                if let Some(s) = capture.session.as_mut() {
                    s.pending_size = Some((width, height));
                    s.pending_formats.clear();
                }
            }
            Event::ShmFormat { format } => {
                if let (Some(s), WEnum::Value(format)) = (capture.session.as_mut(), format) {
                    s.pending_formats.push(format);
                }
            }
            // niri's capture reports `dma: None`, so these never arrive from it
            // and are ignored when another compositor sends them.
            Event::DmabufDevice { .. } | Event::DmabufFormat { .. } => {}
            Event::Done => capture.constraints_done(),
            Event::Stopped => capture.teardown("the compositor stopped the session"),
            _ => {}
        }
    }
}

impl<D> Dispatch<ExtImageCopyCaptureFrameV1, CaptureData, D> for Feeders
where
    D: Dispatch<ExtImageCopyCaptureFrameV1, CaptureData> + AsMut<Feeders> + 'static,
{
    fn event(
        state: &mut D,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &CaptureData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let capture = &mut state.as_mut().capture;
        if capture
            .session
            .as_ref()
            .is_none_or(|s| s.frame.as_ref() != Some(frame))
        {
            return;
        }

        match event {
            // Metadata about the frame just taken. The preview is a still of
            // one window in one orientation; none of it changes what is read.
            Event::Transform { .. } | Event::Damage { .. } | Event::PresentationTime { .. } => {}
            Event::Ready => capture.frame_ready(),
            Event::Failed { reason } => capture.frame_failed(reason),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
