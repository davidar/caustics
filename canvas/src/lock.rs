//! The host's half of the session lock: whose lock it is, where the
//! compositor is with it, and what the lock sidecar has been told
//! (docs/abi.md, *Lock*, *Canvas event*; docs/canvas.md, *Surfaces*).
//!
//! ```text
//!            LOCK_LOCK                 `locked`
//! Unlocked ────────────▶ Locking ─────────────────▶ Locked
//!    ▲                     │ `finished`: fatal         │ LOCK_UNLOCK from its owner:
//!    │                     ▼                           │ unlock_and_destroy, the panes go,
//!    │                   exit 1                        ▼ a wl_display.sync
//!    └──────────── the sync's answer: `unlocked` ── Unlocking
//! ```
//!
//! - **A lock belongs to the sidecar whose `LOCK_LOCK` took it**, and only
//!   that sidecar's `LOCK_UNLOCK` ends it; one from any other is refused
//!   with a witness. Nothing else unlocks: not the owner's pipe closing (the
//!   lock stays, and the owner's restart, under the same name, may unlock),
//!   not a reload, not the host on its own.
//! - **`LOCK_LOCK` while locking** is no change. **While locked** it is no
//!   change to the lock either, but the asker is told `locked` again: a
//!   restarted lock sidecar re-locks on start (logind's `LockedHint`) and
//!   waits for `locked` before it begins a conversation. **While unlocking**
//!   it is held until the unlock is confirmed, then the session is locked
//!   afresh, so the owner hears `unlocked` and then `locked`, in order.
//! - **`locked`** is the compositor's word that every output shows a lock
//!   surface or its blank (the protocol's own rule); it is told to the owner
//!   as `CANVAS_EVENT_LOCKED` at once.
//! - **`unlocked`** is told after `unlock_and_destroy` and a
//!   `wl_display.sync` whose answer says the compositor has read it.
//! - **`finished`** (another client holds the lock, the compositor refused
//!   it, or it is going away) is fatal: one line and exit 1, so the unit
//!   restarts the host, and the lock sidecar learns of it by its pipe
//!   closing (docs/canvas.md, *Failure model*).
//! - **At the lock**, the prompt core is **reserved** for the owner: a
//!   conversation of another sink that is up is dismissed, and another
//!   sink's ask while locked is answered cancelled, so the lock screen's
//!   prompt is only ever the lock's. The reservation ends with the unlock.
//! - **At `locked`**, a dim that is up is ended (its overlays go; its sidecar
//!   is told nothing, as with an undim it sent). Not at the lock request:
//!   the overlays taken down then would leave the windows on screen for the
//!   frame before the lock surfaces' first, and the design has the lock
//!   continue from the fade (docs/canvas.md, *The model*). Once the
//!   compositor has locked, it shows no layer surface, so the overlays go
//!   unseen.
//! - **The lock is the deepest dim.** While locked ([`Locks::dims_at_once`]),
//!   a dim asked for is reached at once: its sidecar hears
//!   `CANVAS_EVENT_DIMMED` straight away, and no overlay is made. A dim
//!   made under the lock would be unseen, and the keys at the lock are the
//!   prompt's and cancel no dim, so it would outlive the lock and the unlock
//!   would land on a dimmed desktop.
//! - **At the unlock**, a dim still up is ended as at `locked`, without a
//!   word to its sidecar: that sidecar was told `dimmed` long ago (or its
//!   dim was ended at `locked` already), and a `dim --wait` client behind it
//!   has had its answer and gone. It ends with the `unlock_and_destroy`, not
//!   at the sync's answer, so no frame of the unlocked desktop is dimmed.
//!
//! - **Input on a lock surface** while locked (a key, the pointer moving, a
//!   button, a touch) is told to the owner as `CANVAS_EVENT_INPUT` with `id`
//!   0, at most once in [`INPUT_TOLD_EVERY`]: the lock sidecar holds its
//!   fingerprint reader while someone is at the lock, and this is how it
//!   knows. The prompt's keys are still the prompt's.
//!
//! This module owns no Wayland object and no pipe. [`Effect`]s say what to
//! do, and `wayland.rs` does it, as with the prompt core.

use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{
    CANVAS_EVENT_INPUT, CANVAS_EVENT_LOCKED, CANVAS_EVENT_UNLOCKED, LOCK_LOCK, LOCK_UNLOCK,
};

/// Input on a lock surface is told to the lock's owner at most this often:
/// the fact that someone is there, not each key.
pub const INPUT_TOLD_EVERY: Duration = Duration::from_secs(5);

/// Where the lock is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Unlocked,
    /// `ext_session_lock_manager_v1.lock` made; `locked` not yet sent.
    Locking,
    /// The compositor said `locked`.
    Locked,
    /// `unlock_and_destroy` made; the sync after it not yet answered.
    Unlocking,
}

/// What the host is to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Take the lock: `SessionLockState::lock`, and a lock pane on every
    /// output. If the compositor has no lock manager, [`Locks::refused`].
    Lock,
    /// `unlock_and_destroy`, every lock pane down, and a `wl_display.sync`
    /// whose answer is [`Locks::synced`].
    Unlock,
    /// One `CanvasEvent` down `sidecar`'s stdin.
    Tell { sidecar: String, event: u32 },
    /// End the dim that is up, if one is, without a word to its sidecar;
    /// the words say when (`at locked`, `at the unlock`), for the journal.
    EndDim(&'static str),
    /// Reserve the prompt core for this sidecar, or end the reservation.
    Reserve(Option<String>),
    /// One line for the journal.
    Note(String),
    /// A witness: a request refused, one line at warning level.
    Refused(String),
    /// The host must leave, with this line, exit 1.
    Fatal(String),
}

/// The lock, and whose it is.
#[derive(Debug)]
pub struct Locks {
    phase: Phase,
    /// The sidecar whose `LOCK_LOCK` took the lock now held.
    owner: Option<String>,
    /// A `LOCK_LOCK` that arrived while unlocking: the sidecar to lock for
    /// once the unlock is confirmed.
    relock: Option<String>,
    /// When input on a lock surface was last told to the owner.
    input_told: Option<Instant>,
}

impl Default for Locks {
    fn default() -> Locks {
        Locks {
            phase: Phase::Unlocked,
            owner: None,
            relock: None,
            input_told: None,
        }
    }
}

impl Locks {
    pub fn new() -> Locks {
        Locks::default()
    }

    #[cfg(test)]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether the lock object exists, and with it the lock panes: from the
    /// lock request until the unlock. While it holds, the prompt is drawn on
    /// the lock panes and never on the top layer.
    pub fn held(&self) -> bool {
        matches!(self.phase, Phase::Locking | Phase::Locked)
    }

    /// Whether a dim asked for now is reached at once, with nothing drawn:
    /// the compositor has said `locked` and no unlock is asked yet, so every
    /// output shows a lock surface, darker than any dim.
    pub fn dims_at_once(&self) -> bool {
        self.phase == Phase::Locked
    }

    /// A `Lock` frame from `sidecar`.
    pub fn request(&mut self, sidecar: &str, command: u32) -> Vec<Effect> {
        match command {
            LOCK_LOCK => self.lock(sidecar),
            LOCK_UNLOCK => self.unlock(sidecar),
            other => vec![Effect::Refused(format!(
                "lock: command {other} from {sidecar} is not one the host knows"
            ))],
        }
    }

    fn lock(&mut self, sidecar: &str) -> Vec<Effect> {
        match self.phase {
            Phase::Unlocked => self.take(sidecar),
            Phase::Locking => vec![Effect::Note(format!(
                "lock: {sidecar} asked to lock while locking for {}; no change",
                self.owner_name()
            ))],
            Phase::Locked => vec![
                Effect::Note(format!(
                    "lock: {sidecar} asked to lock while locked for {}; no change, and \
                     told locked again",
                    self.owner_name()
                )),
                Effect::Tell {
                    sidecar: sidecar.to_owned(),
                    event: CANVAS_EVENT_LOCKED,
                },
            ],
            Phase::Unlocking => {
                self.relock = Some(sidecar.to_owned());
                vec![Effect::Note(format!(
                    "lock: {sidecar} asked to lock while unlocking; it locks again once \
                     the unlock is confirmed"
                ))]
            }
        }
    }

    /// Unlocked to locking, for `sidecar`: the prompt is the owner's, and the
    /// lock is asked for. A dim up stays up until `locked`.
    fn take(&mut self, sidecar: &str) -> Vec<Effect> {
        self.phase = Phase::Locking;
        self.owner = Some(sidecar.to_owned());
        vec![
            Effect::Note(format!("lock: {sidecar} asked to lock; locking")),
            Effect::Reserve(Some(sidecar.to_owned())),
            Effect::Lock,
        ]
    }

    fn unlock(&mut self, sidecar: &str) -> Vec<Effect> {
        if self.phase != Phase::Unlocked && self.owner.as_deref() != Some(sidecar) {
            return vec![Effect::Refused(format!(
                "lock: an unlock from {sidecar}, whose lock this is not (it is {}'s); refused",
                self.owner_name()
            ))];
        }
        match self.phase {
            Phase::Unlocked => vec![Effect::Refused(format!(
                "lock: an unlock from {sidecar} while unlocked; nothing to do"
            ))],
            // `unlock_and_destroy` before `locked` is a protocol error, and
            // the lock sidecar never asks it: refused, and the lock goes on.
            Phase::Locking => vec![Effect::Refused(format!(
                "lock: an unlock from {sidecar} before the compositor said locked; refused, \
                 the lock goes on"
            ))],
            Phase::Unlocking => vec![Effect::Note(format!(
                "lock: an unlock from {sidecar} while unlocking; no change"
            ))],
            Phase::Locked => {
                self.phase = Phase::Unlocking;
                self.relock = None;
                vec![
                    Effect::Note(format!("lock: {sidecar} asked to unlock; unlocking")),
                    Effect::EndDim("at the unlock"),
                    Effect::Unlock,
                ]
            }
        }
    }

    /// The compositor has no `ext_session_lock_manager_v1`: there is nothing
    /// to lock with. The asker is told nothing (there is no event for it),
    /// and a `lock --wait` waits on.
    pub fn refused(&mut self) -> Vec<Effect> {
        let owner = self.owner.take().unwrap_or_default();
        self.phase = Phase::Unlocked;
        vec![
            Effect::Refused(format!(
                "lock: the compositor has no ext_session_lock_manager_v1; {owner}'s lock is \
                 refused"
            )),
            Effect::Reserve(None),
        ]
    }

    /// The compositor's `locked`.
    pub fn locked(&mut self) -> Vec<Effect> {
        if self.phase != Phase::Locking {
            return vec![Effect::Refused(format!(
                "lock: the compositor said locked while {:?}; ignored",
                self.phase
            ))];
        }
        self.phase = Phase::Locked;
        self.input_told = None;
        let owner = self.owner_name();
        vec![
            Effect::Note(format!(
                "lock: the compositor locked the session; told to {owner}"
            )),
            Effect::EndDim("at locked"),
            Effect::Tell {
                sidecar: owner,
                event: CANVAS_EVENT_LOCKED,
            },
        ]
    }

    /// `what` (a key, the pointer moving, a button, a touch) reached a lock
    /// surface at `now`. Told to the owner while locked, once in
    /// [`INPUT_TOLD_EVERY`]; nothing otherwise.
    pub fn input(&mut self, now: Instant, what: &str) -> Vec<Effect> {
        if self.phase != Phase::Locked {
            return Vec::new();
        }
        if self
            .input_told
            .is_some_and(|told| now.saturating_duration_since(told) < INPUT_TOLD_EVERY)
        {
            return Vec::new();
        }
        self.input_told = Some(now);
        let owner = self.owner_name();
        vec![
            Effect::Note(format!("lock: input at the lock ({what}); told to {owner}")),
            Effect::Tell {
                sidecar: owner,
                event: CANVAS_EVENT_INPUT,
            },
        ]
    }

    /// The compositor's `finished`: it ended the lock, or would not take it.
    /// The host cannot carry on as though it held the lock, nor pretend it
    /// was unlocked: it leaves, and the unit restarts it.
    pub fn finished(&mut self) -> Vec<Effect> {
        vec![Effect::Fatal(format!(
            "lock: the compositor ended {}'s session lock while {:?} (another locker holds \
             it, or the compositor refused or ended it)",
            self.owner_name(),
            self.phase
        ))]
    }

    /// The sync after `unlock_and_destroy` came back: the compositor has the
    /// unlock. The owner is told `unlocked`; a lock asked for meanwhile is
    /// taken now.
    pub fn synced(&mut self) -> Vec<Effect> {
        if self.phase != Phase::Unlocking {
            return Vec::new();
        }
        let owner = self.owner.take().unwrap_or_default();
        self.phase = Phase::Unlocked;
        let mut effects = vec![
            Effect::Note(format!("lock: the session is unlocked; told to {owner}")),
            Effect::Tell {
                sidecar: owner,
                event: CANVAS_EVENT_UNLOCKED,
            },
            Effect::Reserve(None),
        ];
        if let Some(next) = self.relock.take() {
            effects.extend(self.take(&next));
        }
        effects
    }

    /// `sidecar`'s pipe closed. Its lock stays: nothing unlocks but its
    /// say-so, which its restart may give.
    pub fn gone(&mut self, sidecar: &str) -> Vec<Effect> {
        if self.relock.as_deref() == Some(sidecar) {
            self.relock = None;
        }
        if self.phase == Phase::Unlocked || self.owner.as_deref() != Some(sidecar) {
            return Vec::new();
        }
        vec![Effect::Note(format!(
            "lock: {sidecar} went while {:?}; the lock stays until it says otherwise",
            self.phase
        ))]
    }

    fn owner_name(&self) -> String {
        self.owner.clone().unwrap_or_else(|| "nobody".to_owned())
    }
}

#[cfg(test)]
mod tests;
