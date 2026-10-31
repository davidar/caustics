//! The bar's CPU-side model: what the feeders know, in one place, and the two
//! typed streams that connect them to the embassy.
//!
//! Feeders (`feeders/*`) are event-rate translators. Each one owns a
//! connection — a Wayland global, the compositor's IPC socket, a sidecar's
//! pipe — and turns what arrives on it into [`Update`]s, sent over a calloop
//! channel to the embassy, which applies them here with [`Model::apply`] and
//! marks the frame dirty. Nothing in a feeder decides anything: a toplevel
//! handle's `done` becomes `Update::Window`, a sidecar's frame becomes
//! `Update::Rows`, and that is the whole job.
//!
//! [`Action`]s flow the other way. The caustic decides — which window a dock
//! click activates, that an applet click opens its menu, that a hover has dwelt
//! long enough for a preview — and writes the decision into the action ring
//! in the state buffer; the embassy reads the ring one frame late, turns each record's operands (a window id, a range of the strings buffer, a sidecar slot and row) into one of these, and hands
//! it to the feeder that owns it. The embassy never decides either: it is a
//! table lookup from id to `Action`, and from `Action` to the one method or
//! request that performs it.
//!
//! This file is the interface every feeder and the embassy are written
//! against: additions are cheap, renames and removals touch every feeder.

use std::collections::{BTreeMap, BTreeSet};

use crate::abi::wire::{ActionBody, Frame, Rows, Samples};

/// The channel every feeder sends on. One receiver, in the embassy's calloop.
/// The loop is sctk's re-exported calloop, so the bar names no calloop
/// version of its own that could disagree with it.
pub type Updates = smithay_client_toolkit::reexports::calloop::channel::Sender<Update>;

// ---------------------------------------------------------------------------
// Windows (ext-foreign-toplevel-list-v1, `feeders/toplevels.rs`)
// ---------------------------------------------------------------------------

/// One mapped toplevel, as the compositor describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// niri's window id: the `identifier` string of the ext handle parsed
    /// as a decimal, which is the same number `niri msg` and the IPC
    /// `FocusWindow { id }` action use. A handle whose identifier does not
    /// parse is reported once with a witness line and never modelled.
    pub id: u64,
    pub app_id: String,
    pub title: String,
    /// `activated` in the handle's state set.
    pub focused: bool,
    /// Not in ext-foreign-toplevel-list v1: only the compositor's IPC sets it
    /// (`feeders/niri_ipc.rs`), and it stays `false` without one.
    pub urgent: bool,
    /// A monotonically increasing stamp the feeder bumps every time this
    /// window becomes focused, so "most recent window of this app" is a
    /// max over its children and not a guess.
    pub focus_stamp: u64,
}

// ---------------------------------------------------------------------------
// The tray, the battery and the power menu — **not here**
// ---------------------------------------------------------------------------
//
// None of them owns a Wayland object on the bar's connection, so by the bar's
// membership test (docs/architecture.md, *Sidecars*)
// they are `caustics-tray`'s and `caustics-power`'s, D-Bus clients and all, and what
// reaches the bar is what reaches it from any sidecar: one `WIRE_ROWS` frame,
// as [`Update::Rows`]. The bar parses none of it and models none of it — the caustic's `bar_build` turns the rows into items, and that is the whole of the bar's knowledge of a tray icon.

// ---------------------------------------------------------------------------
// Caffeine (zwp_idle_inhibit_manager_v1, `feeders/caffeine.rs`)
// ---------------------------------------------------------------------------

/// Whether the session is being kept awake, and whether it can be.
///
/// Three states and not a `bool` plus an `absent` entry, because the chip is
/// published only when the manager is there at all: a compositor without
/// `zwp_idle_inhibit_manager_v1` gets no caffeine chip rather than a chip that
/// does nothing. [`Caffeine::Absent`] is the startup value and the one state
/// a toggle cannot leave.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Caffeine {
    /// The global was not offered; there is nothing to toggle.
    #[default]
    Absent,
    /// Bound, no inhibitor held: the session may idle.
    Off,
    /// An inhibitor is held on the bar's layer surface.
    On,
}

// ---------------------------------------------------------------------------
// Previews (ext-image-copy-capture-v1 with a foreign-toplevel source)
// ---------------------------------------------------------------------------

/// One captured frame of one window. Straight RGBA8, row-major, top-down,
/// tightly packed (the feeder converts from whatever shm format the
/// compositor offered and drops the stride).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub window: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

// ---------------------------------------------------------------------------
// The streams
// ---------------------------------------------------------------------------

/// What a feeder can tell the embassy. Every variant is idempotent: applying
/// the same update twice leaves the model as it was after the first.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// A window appeared or changed (sent on the handle's `done`).
    Window(Window),
    WindowClosed(u64),
    Preview(Preview),
    /// The capture for `window` could not be started or failed mid-way; the
    /// embassy shows the entry without a preview. `why` is the witness.
    PreviewFailed {
        window: u64,
        why: String,
    },
    /// An optional global, service **or sidecar** is not there. Sent once per
    /// name; the model records it and the embassy logs one witness line
    /// (docs/architecture.md, *Rules*: every optional global degrades to "feature absent"). A `String`, because a sidecar's name is the file stem of what
    /// was spawned and not a compiled-in literal (docs/abi.md, *Lifecycle*).
    ///
    /// This one carries no state with it; [`Update::SidecarGone`] does.
    Absent(String),
    /// **The one absence with state behind it.** A sidecar that cannot be
    /// spawned leaves nothing behind it that is still true: the samples body
    /// it filled goes, and so do the rows it published. Sent by the
    /// pipe feeder right after the `Absent` that witnesses the same thing,
    /// because only the pipe feeder knows which names are sidecars — the model
    /// must not have to re-resolve `$CAUSTICS_SIDECARS` to find out.
    SidecarGone(String),
    /// The wall clock moved: `(clock_time, clock_date, clock_month)` as
    /// `crate::clock::clock_numbers` packs them, which is what
    /// `crate::abi::FrameInputs` carries. The embassy's own
    /// minute timer sends this; it is an update like any other.
    ///
    /// **Numbers**, not strings: the caustic writes `HH:MM`
    /// and the date from them, in `caustics/bar_tunables.slang`'s own words and
    /// order, and its CLOCK_SHOW_DATE decides whether the date is drawn.
    /// The third is the month the calendar lays out.
    Clock(u32, u32, u32),
    /// One tick of a sampling sidecar: the whole samples body, as it will be
    /// uploaded to binding 10 (docs/abi.md, *Samples*). The model keeps the
    /// latest and nothing else — the body carries its own history. Every tick
    /// is a change (a watched chart has to move even when the value repeats),
    /// so `apply` returns true.
    Samples {
        sidecar: String,
        body: Box<Samples>,
    },
    /// The idle-inhibit toggle moved, or was found at bind time. Sent by
    /// `feeders/caffeine.rs` — once on bind when the global is there, and once
    /// per toggle after that. An inhibitor is never persisted, so the first one
    /// is always [`Caffeine::Off`].
    Caffeine(Caffeine),
    /// One `WIRE_ROWS` frame from `sidecar`: an applet's whole picture
    /// (docs/abi.md, *Rows*). Whole-state, always: it *replaces* whatever
    /// that sidecar published before, so a restart of either end needs no
    /// replay. `bar_build` turns it into one chip, its menu rows and its
    /// sliders (`caustics/bar_model.slang`'s item kinds).
    Rows {
        sidecar: String,
        rows: Rows,
    },
    /// One of the answer frames from `sidecar` — `Bytes`, `Font`, `Glyph`, `App` or
    /// `Pixmap` (docs/abi.md, *Bytes*–*Pixmap*) — carried whole. The model keeps
    /// none of it: the bytes go to a GPU buffer or an image slot and the small
    /// ones into the tables the item build reads, all in `wayland.rs`, which
    /// is why `apply` reports no change for it.
    Frame {
        sidecar: String,
        frame: Frame,
    },
}

/// What the caustic decided and the embassy must perform. `Bar::forward` turns an action-ring record (`abi::Note`: a kind and its operands) into one of these and hands it to the owning feeder.
///
/// `PartialEq` but not `Eq`: [`Action::Sidecar`] carries an [`ActionBody`],
/// whose `value` is a float.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// **Give this window the keyboard focus and raise it**, whatever that
    /// means on the compositor hosting the bar (`feeders/compositor.rs`). On
    /// niri it is the IPC's `FocusWindow`.
    Activate(u64),
    /// **Ask this window to close** — politely, the way a titlebar's × does,
    /// so the application may still refuse. On niri it is the IPC's
    /// `CloseWindow`. The **Close** row of a dock entry's right-click menu
    /// pushes it.
    Close(u64),
    /// **Show or hide the compositor's own window overview**, if it has one.
    /// On niri it is the IPC's `ToggleOverview`; a compositor with no such
    /// mode has nothing to perform, and the overview button is one witness
    /// line per click.
    ToggleOverview,
    /// Launch a pinned entry with no running window: the desktop entry the
    /// embassy resolved for this app id. Also the **New window** row of a dock
    /// entry's right-click menu, which is the same request made deliberately.
    ///
    /// This is what an `ACT_SPAWN` from a dock click becomes, and it never
    /// leaves the embassy: `Bar::perform` turns it into an
    /// [`Action::Launch`] — the one step that needs the filesystem — and the
    /// compositor is what actually starts the process.
    Spawn {
        app_id: String,
    },
    /// **Start this argv.** What an [`Action::Spawn`] becomes once the desktop
    /// entry behind the app id has been read: either `gio launch <path>` or
    /// the entry's own `Exec=`, as an argv and never through a shell.
    ///
    /// The compositor performs it — it is the thing on this machine whose job
    /// is starting clients, and it puts the child in its own scope with the
    /// session's environment, which a bar forking for itself never got quite
    /// right. On niri it is the IPC's `Spawn` (`feeders/niri_ipc.rs`). A bar
    /// hosted on a compositor with no IPC it knows drops it with one line and
    /// forks nothing: the bar owns no child process but its sidecars.
    Launch {
        argv: Vec<String>,
    },
    /// **Pin** this app to the dock, or **unpin** it: add it to or remove it
    /// from [`Model::favourites`] and write the file
    /// (`crate::favourites`). The embassy's to perform, the way `Spawn` is —
    /// no connection owns a list of app ids.
    Pin {
        app_id: String,
    },
    Unpin {
        app_id: String,
    },
    /// **Flip the idle inhibitor.** On creates a `zwp_idle_inhibitor_v1` on
    /// the bar's own layer surface; off destroys it. The caffeine feeder owns
    /// the object, so this is its action and nobody else's.
    ToggleCaffeine,
    /// Start capturing `Some(window)` for a hover preview, or stop
    /// (`None`). At most one capture session exists at a time.
    PreviewWanted(Option<u64>),
    /// The caustic decided something about one of a sidecar's rows: the
    /// sidecar's name, and the 32-byte body that goes to its stdin as a
    /// `WIRE_ACTION` frame (docs/abi.md, *Action*). The bar has already
    /// de-normalised an `ACTION_SET_VALUE` into that row's own `min..max`; it
    /// composes nothing else and waits for nothing — the sidecar's reply is
    /// its next `Rows` frame.
    Sidecar {
        name: String,
        body: ActionBody,
    },
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub windows: BTreeMap<u64, Window>,
    /// The dock's pinned app ids, in dock order — **state**, loaded once at
    /// startup from `$XDG_STATE_HOME/caustics/bar/favourites` and mutated by the
    /// dock's own Pin and Unpin rows (`crate::favourites`; docs/architecture.md,
    /// *Configuration is the caustic*).
    /// Empty by default because `Default` is what a test gets; the running bar
    /// fills it in `Bar::new` from the file, and a machine with no file has
    /// none.
    pub favourites: Vec<String>,
    /// Local wall time as `hour * 60 + minute`, published in
    /// `FrameInputs::clock_time` and spelled by the caustic.
    pub clock: u32,
    /// Local date as `weekday << 16 | month << 8 | day`, published in
    /// `FrameInputs::clock_date`.
    pub clock_date: u32,
    /// The month as `year << 16 | first << 8 | days`, published in
    /// `FrameInputs::clock_month` for the calendar.
    pub clock_month: u32,
    pub preview: Option<Preview>,
    /// Whether the session is being kept awake, and whether it can be. The
    /// chip is published only while this is not [`Caffeine::Absent`].
    pub caffeine: Caffeine,
    pub absent: BTreeSet<String>,
    /// The last `Rows` frame from each sidecar that published one, by the
    /// sidecar's name. A frame is whole-state, so this is a replacement and
    /// never a merge; `Update::SidecarGone` empties one entry.
    pub applets: BTreeMap<String, Rows>,
    /// The latest frame from the sampling sidecar, or `None` before the first
    /// one (and after that sidecar's `SidecarGone`, which clears it).
    pub samples: Option<Box<Samples>>,
    /// Which sidecar wrote `samples`, so that only its going clears them.
    pub samples_from: Option<String>,
}

impl Model {
    /// Apply one update. Returns `true` when the model changed, which is what
    /// the embassy uses to decide whether a frame is owed.
    pub fn apply(&mut self, update: Update) -> bool {
        match update {
            Update::Window(w) => {
                let changed = self.windows.get(&w.id) != Some(&w);
                self.windows.insert(w.id, w);
                changed
            }
            Update::WindowClosed(id) => {
                let removed = self.windows.remove(&id).is_some();
                if self.preview.as_ref().is_some_and(|p| p.window == id) {
                    self.preview = None;
                }
                removed
            }
            Update::Preview(p) => {
                let changed = self.preview.as_ref() != Some(&p);
                self.preview = Some(p);
                changed
            }
            Update::PreviewFailed { window, .. } => {
                if self.preview.as_ref().is_some_and(|p| p.window == window) {
                    self.preview = None;
                    true
                } else {
                    false
                }
            }
            Update::Caffeine(state) => {
                let changed = self.caffeine != state;
                self.caffeine = state;
                changed
            }
            Update::Absent(name) => self.absent.insert(name),
            Update::SidecarGone(name) => {
                // A chart strip of stale numbers is worse than no strip, and
                // an applet whose sidecar is not there is worse still: it
                // would take clicks nothing performs. The sidecar that wrote the
                // samples takes the body with it; any sidecar's rows go with it by
                // name.
                let rows = self.applets.remove(&name).is_some();
                let samples = self.samples_from.as_deref() == Some(name.as_str())
                    && self.samples.take().is_some();
                if samples {
                    self.samples_from = None;
                }
                samples || rows
            }
            Update::Rows { sidecar, rows } => {
                let changed = self.applets.get(&sidecar) != Some(&rows);
                self.applets.insert(sidecar, rows);
                changed
            }
            // Carried whole to `wayland.rs`, which copies its bytes where they
            // go; the model has nothing to keep and nothing changed here.
            Update::Frame { .. } => false,
            Update::Samples { sidecar, body } => {
                self.samples = Some(body);
                self.samples_from = Some(sidecar);
                true
            }
            Update::Clock(time, date, month) => {
                let changed =
                    self.clock != time || self.clock_date != date || self.clock_month != month;
                self.clock = time;
                self.clock_date = date;
                self.clock_month = month;
                changed
            } // (the clock's three words reach the GPU through `FrameInputs`, not
              // through a block: `wayland.rs` reads them off the model when it
              // builds a frame's inputs.)
        }
    }

    /// Windows of one app, **in no particular order**.
    ///
    /// Nothing in the running bar asks: the windows are facts at
    /// binding 12 and `bar_build` joins them to their entry through
    /// `WindowFact::app_row` (docs/abi.md, *The facts*). Only a test asks.
    ///
    /// Ordering is a decision, so it is the caustic's: the focus stamp
    /// travels in the item (a window item's `a` word, read by `item_focus_stamp`), and the order this returns is the map's, which
    /// nothing may depend on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn windows_of(&self, app_id: &str) -> Vec<&Window> {
        self.windows
            .values()
            .filter(|w| w.app_id == app_id)
            .collect()
    }

    /// Distinct running app ids, in order of first appearance by window id.
    pub fn running_app_ids(&self) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        self.windows
            .values()
            .filter(|w| seen.insert(w.app_id.as_str()))
            .map(|w| w.app_id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests;
