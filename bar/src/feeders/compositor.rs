//! The compositor seam: what the bar needs from whatever it is hosted on.
//!
//! The bar reads two things from its compositor that Wayland's own protocols
//! do not give a client — which window is focused, and which is urgent — and
//! asks it for four — focus this window, close that one, show the overview,
//! and start this argv.
//! Today one implementation answers ([`super::niri_ipc::NiriIpc`], over
//! `$NIRI_SOCKET`); tomorrow another might. This module is the shape of the
//! answer, so that "which compositor" is a choice made once in
//! [`super::Feeders::bind`] and nowhere else.
//!
//! Everything past the seam speaks the bar's own words: `Update`s in, [`Action`]s out (`model.rs`). The seam is one trait, one field in `Feeders` and one witness string.
//!
//! # The window key, which the seam does **not** abstract
//!
//! A [`crate::model::Window`]'s `id` is the `u64` parsed from the ext
//! foreign-toplevel handle's identifier, which is also the id niri's socket
//! addresses a window by — the join the dock depends on; a compositor whose
//! IPC cannot address a window by that identifier needs a different key
//! contract, and that is a question for that compositor, not for this seam.
//!
//! # The witness word
//!
//! Every implementation carries an inherent `const ABSENT: &str` — the name
//! it reports itself absent under, and the name the `action:` witness lines
//! use — and returns it from [`Compositor::name`]. It is an inherent const
//! rather than an associated one because an associated const would stop this
//! trait being dyn-compatible, and [`super::Feeders`] holds a
//! `Box<dyn Compositor>`.

use super::WindowSet;
use crate::model::{Action, Update, Updates};

/// The compositor, as the bar needs it.
///
/// Four requests out, two facts in, and one name for the journal. An
/// implementation is a feeder like any other (`feeders/mod.rs`): it owns a
/// connection, it never decides anything, and its absence is one witness line
/// and one `Update::Absent` rather than a failure.
pub trait Compositor: Send {
    /// The name this implementation reports itself absent under, and the one
    /// the `feeders:` and `action:` witness lines carry.
    fn name(&self) -> &'static str;

    /// Connect, and start whatever stream carries focus and urgency into
    /// `windows`. Never fails: a compositor that is not there is
    /// [`Compositor::is_bound`] answering `false`.
    fn start(windows: WindowSet, updates: Updates) -> Self
    where
        Self: Sized;

    /// Whether the connection was made.
    fn is_bound(&self) -> bool;

    /// Perform one action, and say whether it belonged here. `true` for the
    /// kinds this seam owns even when the connection is missing, so the
    /// caller does not go looking for another owner — unless there is no
    /// compositor connection at all, which is [`NoCompositor`]'s answer.
    fn perform(&mut self, action: &Action) -> bool;
}

/// No compositor IPC at all: the bar draws, and the four actions that need a
/// compositor to perform them are one witness line each.
///
/// This is what a bar on a compositor nobody has written an implementation
/// for gets. It is deliberately not "niri, absent": a socket that is not
/// there is a niri that is not running, and *no implementation* is a
/// different fact with a different witness.
pub struct NoCompositor;

impl NoCompositor {
    /// The witness word. See the module docs on why it is inherent.
    pub const ABSENT: &'static str = "compositor";
}

impl std::fmt::Debug for NoCompositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoCompositor")
    }
}

impl Compositor for NoCompositor {
    fn name(&self) -> &'static str {
        Self::ABSENT
    }

    fn start(_windows: WindowSet, updates: Updates) -> Self {
        tracing::info!("{} absent: no compositor IPC to bind", Self::ABSENT);
        let _ = updates.send(Update::Absent(Self::ABSENT.to_owned()));
        NoCompositor
    }

    fn is_bound(&self) -> bool {
        false
    }

    fn perform(&mut self, action: &Action) -> bool {
        // Not `true`: with no implementation behind the seam there is no
        // owner to claim it, and the embassy's own "nothing claimed it" line
        // is the honest end of the story. This one says which action it was.
        tracing::info!("{} {action:?}: dropped, no compositor IPC", Self::ABSENT);
        false
    }
}

#[cfg(test)]
mod tests;
