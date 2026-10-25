//! Frame pacing: when a host whose frames are driven by events may draw.
//!
//! A host draws on events, not on a vblank loop (docs/architecture.md,
//! *The frame*). Under Mailbox nothing in `present` waits on the compositor,
//! so the pacer is the host's own `wl_surface.frame` request: while one is
//! out, dirt waits, and events coalesce into one frame per compositor repaint
//! instead of one frame per pointer motion. The Wayland side (making the
//! request, receiving the callback) is the host's, because it owns the
//! surface; this is the state between the two and the rule over it.

use std::time::{Duration, Instant};

/// How long a frame request may stay unanswered before the host draws on an
/// event anyway. Longer than any real frame interval, shorter than a human
/// noticing a click was dropped.
pub const FRAME_STALL: Duration = Duration::from_millis(500);

/// The frame request in flight, and whether the caustic is animating.
#[derive(Debug)]
pub struct Pacing {
    /// A `wl_surface.frame` request rode the last commit and has not come
    /// back. While it is out, dirt waits: events coalesce into one frame per
    /// compositor repaint instead of one frame per pointer motion, which is
    /// the whole of docs/architecture.md's "render on events" under Mailbox.
    pending: bool,
    /// When the pending frame request was made, for the stall fallback.
    requested_at: Instant,
    /// The last mirror said `wants_redraw`: a spring is in flight, or the
    /// dwell clock is running, and the next frame callback is a frame.
    animating: bool,
}

impl Pacing {
    /// Nothing requested and nothing animating, as of `now`.
    pub fn new(now: Instant) -> Pacing {
        Pacing {
            pending: false,
            requested_at: now,
            animating: false,
        }
    }

    /// A frame request went out at `now`, riding the commit that presents
    /// this frame.
    pub fn requested(&mut self, now: Instant) {
        self.pending = true;
        self.requested_at = now;
    }

    /// The compositor answered the request. Returns whether that answer is
    /// itself a frame: a spring in flight is a frame per repaint, and a
    /// settled caustic draws nothing until an event dirties it.
    pub fn answered(&mut self) -> bool {
        self.pending = false;
        self.animating
    }

    /// May a dirty host draw right now? Yes when no frame request is out;
    /// otherwise the frame callback is the pacer, with one escape: a
    /// compositor that stops answering frame callbacks (an occluded surface,
    /// a compositor bug) must not freeze the host's *input* handling, so a
    /// request older than [`FRAME_STALL`] is treated as answered.
    pub fn allowed(&self) -> bool {
        self.allowed_at(Instant::now())
    }

    /// [`Pacing::allowed`] as of `now`.
    pub fn allowed_at(&self, now: Instant) -> bool {
        !self.pending || now.saturating_duration_since(self.requested_at) >= FRAME_STALL
    }

    /// When the request out stops holding a frame back ([`FRAME_STALL`]
    /// after it was made), or `None` when none is out.
    ///
    /// A host whose next frame waits on an animation and not on an event has
    /// nothing else to wake it if the compositor never answers (a surface it
    /// does not repaint), so it arms a timer for this.
    pub fn stall_deadline(&self) -> Option<Instant> {
        self.pending.then(|| self.requested_at + FRAME_STALL)
    }

    /// What the last mirror said about `wants_redraw`.
    pub fn set_animating(&mut self, animating: bool) {
        self.animating = animating;
    }

    /// Whether the last mirror said `wants_redraw`.
    pub fn animating(&self) -> bool {
        self.animating
    }
}

#[cfg(test)]
mod tests;
