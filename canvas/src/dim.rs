//! The host's half of a dim: whose it is, and what that sidecar has been told
//! (docs/abi.md, *Canvas event*).
//!
//! A dim belongs to the sidecar that sent its `CANVAS_DIM`, under the id that
//! sidecar gave it. The host writes that sidecar two things about it, each at
//! most once: `CANVAS_EVENT_DIMMED` when the caustic says the dim has reached
//! its depth (`CanvasState::dim_reached` for this id), and
//! `CANVAS_EVENT_INPUT` when input cancels it, after which the dim is over.
//! An undim ends it without a word (the sidecar sent the undim), as does a
//! newer dim, which replaces it; and so does the sidecar's pipe closing,
//! because a dim without its owner is undone: a crashed sidecar never leaves
//! a dark screen.
//!
//! **The lock is the deepest dim.** A `CANVAS_DIM` asked while the session
//! is locked ([`Dims::ask`] with `locked`) is reached at once: its sidecar is
//! told `CANVAS_EVENT_DIMMED` there and then, no overlay is made and nothing
//! is drawn. The screen is already as dark as a dim would make it, and the
//! keys typed at the lock are the lock prompt's, which cancel no dim: a dim
//! made under the lock would outlive it and greet the unlock with a dimmed
//! desktop (docs/canvas.md, *Surfaces*).
//!
//! What a dim looks like and when it is at its depth are the caustic's; this
//! is routing, and it decides nothing.

/// The dim up now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dim {
    /// The sidecar whose dim it is.
    pub sidecar: String,
    /// Its `CanvasBody::id`.
    pub id: u32,
    /// Whether `CANVAS_EVENT_DIMMED` has been written.
    pub said_dimmed: bool,
}

/// What a `CANVAS_DIM` comes to ([`Dims::ask`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// It is the dim now: its overlays go up. `replaced` is the dim it
    /// replaced, which nobody is told about.
    Raise { replaced: Option<Dim> },
    /// The session is locked: the dim is reached already. Its sidecar is to
    /// be written `CANVAS_EVENT_DIMMED` under its id now; nothing is up.
    Reached { sidecar: String, id: u32 },
}

/// Whether a dim is up, and whose.
#[derive(Debug, Default)]
pub struct Dims {
    up: Option<Dim>,
}

impl Dims {
    pub fn new() -> Dims {
        Dims::default()
    }

    pub fn is_up(&self) -> bool {
        self.up.is_some()
    }

    /// A `CANVAS_DIM` from `sidecar` under `id`, with `locked` the lock's
    /// word on whether the session is locked: reached at once if it is, the
    /// dim up now otherwise.
    pub fn ask(&mut self, sidecar: &str, id: u32, locked: bool) -> Asked {
        if locked {
            return Asked::Reached {
                sidecar: sidecar.to_owned(),
                id,
            };
        }
        Asked::Raise {
            replaced: self.begin(sidecar, id),
        }
    }

    /// A `CANVAS_DIM` from `sidecar`: it is the dim now, whatever was before.
    /// Returns the dim it replaced, which nobody is told about.
    pub fn begin(&mut self, sidecar: &str, id: u32) -> Option<Dim> {
        self.up.replace(Dim {
            sidecar: sidecar.to_owned(),
            id,
            said_dimmed: false,
        })
    }

    /// A `CANVAS_UNDIM`: no dim. Returns the one it ended.
    pub fn end(&mut self) -> Option<Dim> {
        self.up.take()
    }

    /// What the caustic read back: if it says the dim up now has reached its
    /// depth and its sidecar has not been told, the sidecar and id to write
    /// `CANVAS_EVENT_DIMMED` to, once.
    pub fn reached(&mut self, dim_reached: u32, dim_id: u32) -> Option<(String, u32)> {
        let dim = self.up.as_mut()?;
        if dim_reached == 0 || dim_id != dim.id || dim.said_dimmed {
            return None;
        }
        dim.said_dimmed = true;
        Some((dim.sidecar.clone(), dim.id))
    }

    /// Input: the dim is over, and its sidecar is to be written
    /// `CANVAS_EVENT_INPUT`. `None` when no dim is up, so a burst of input
    /// cancels once.
    pub fn cancel(&mut self) -> Option<Dim> {
        self.up.take()
    }

    /// A sidecar's pipe closed: if the dim up was its, the dim goes with it.
    pub fn gone(&mut self, sidecar: &str) -> Option<Dim> {
        if self.up.as_ref().is_some_and(|dim| dim.sidecar == sidecar) {
            return self.up.take();
        }
        None
    }
}

#[cfg(test)]
mod tests;
