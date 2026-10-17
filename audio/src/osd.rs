//! The on-screen display: a volume or mute change that did not come from the
//! bar's own menu raises this sidecar's own rows as a toast for
//! [`crate::OSD_HOLD`] (docs/abi.md, **Toasts**). The chip carries
//! `RFLAG_SHOW` and the rows that changed carry `RFLAG_ATTENTION`; the caustic
//! does the rest, and a change while the toast is up is drawn in place.
//!
//! [`decide`] is the rule as a pure function of the two readings, the time of
//! this sidecar's own last write and the clock; [`Osd`] is the little state
//! the loop keeps around it (the burst's rows, its deadline, whether the
//! suppression has been said). Time is a parameter everywhere, so
//! `audio/tests/osd.rs` drives both without sleeping or a sound server.

use std::time::Instant;

use crate::wpctl::Sound;
use crate::{OSD_HOLD, OSD_OWN_HOLD};

/// Which rows an OSD burst flags. The union over the burst: a volume key
/// followed by a mute key within the hold shows both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flagged {
    /// The slider, `ROW_VOLUME`: the volume or the sink's mute moved.
    pub volume: bool,
    /// The sink's mute checkbox, `ROW_MUTE`.
    pub mute: bool,
    /// The microphone's mute checkbox, `ROW_MIC`.
    pub mic: bool,
}

impl Flagged {
    /// No row: the plain picture.
    pub const NONE: Flagged = Flagged {
        volume: false,
        mute: false,
        mic: false,
    };

    /// Whether any row is flagged, which is whether the chip asks for a toast.
    pub fn any(self) -> bool {
        self.volume || self.mute || self.mic
    }

    /// Both bursts' rows.
    pub fn union(self, other: Flagged) -> Flagged {
        Flagged {
            volume: self.volume || other.volume,
            mute: self.mute || other.mute,
            mic: self.mic || other.mic,
        }
    }
}

/// What one re-read means for the OSD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing the OSD shows moved, or the change is not news: the first
    /// reading, a device appearing, the default device moving.
    Nothing,
    /// A change this sidecar made itself, within [`OSD_OWN_HOLD`] of its own
    /// write: the user is in the menu, or has just left it.
    Own,
    /// Show these rows.
    Show(Flagged),
}

/// The rule. `before` is the last published reading (`None` before the first),
/// `after` the new one, `own` the time of this sidecar's own last write.
///
/// Only `On → On` can be a toast: the first reading and a sink appearing are
/// not changes anyone made. A default sink (or source) that moved is a device
/// change, and the volume or mute that came with it is the new device's, not
/// news. Otherwise the slider is flagged for a volume or sink-mute change, the
/// mute checkbox for the sink's mute and the microphone's for its own.
pub fn decide(
    before: Option<&Sound>,
    after: &Sound,
    own: Option<Instant>,
    now: Instant,
) -> Decision {
    let (Some(Sound::On(was, was_devices)), Sound::On(is, is_devices)) = (before, after) else {
        return Decision::Nothing;
    };
    let same_sink = was_devices.default_output() == is_devices.default_output();
    let same_source = was_devices.default_input() == is_devices.default_input();
    let mute = same_sink && was.muted != is.muted;
    let flagged = Flagged {
        volume: same_sink && (was.volume != is.volume || mute),
        mute,
        mic: same_source && was.mic_muted != is.mic_muted,
    };
    if !flagged.any() {
        return Decision::Nothing;
    }
    if own.is_some_and(|own| now.saturating_duration_since(own) < OSD_OWN_HOLD) {
        return Decision::Own;
    }
    Decision::Show(flagged)
}

/// The loop's OSD state.
#[derive(Debug, Default)]
pub struct Osd {
    /// When this sidecar last wrote the volume or a mute itself.
    own: Option<Instant>,
    /// The rows the burst flags, and when it ends.
    rows: Flagged,
    until: Option<Instant>,
    /// When a change was last suppressed, so the witness is one line per
    /// suppressed burst rather than one per frame of a drag.
    quiet: Option<Instant>,
    /// Whether this sidecar's menu is open. A change then is on screen in the
    /// menu already, and a flag set under it would pop a toast the moment the
    /// menu closed.
    menu: bool,
}

impl Osd {
    /// The rows to flag in the next frame; [`Flagged::NONE`] when no burst is up.
    pub fn flagged(&self) -> Flagged {
        self.rows
    }

    /// When the burst ends, for the loop's sleep.
    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// This sidecar wrote the volume, a mute or a default at `now`.
    pub fn own_write(&mut self, now: Instant) {
        self.own = Some(now);
    }

    /// `ACTION_MENU_OPEN` (`true`) or `ACTION_MENU_CLOSE`. Opening the menu
    /// ends a burst (the menu replaces its toast, or the toast was still
    /// waiting); `true` when it did, and the loop publishes.
    pub fn menu(&mut self, open: bool) -> bool {
        self.menu = open;
        open && self.end()
    }

    /// A re-read moved the picture from `before` to `after` at `now`. Returns
    /// the witness line to say, if any. The deadline is extended and the rows
    /// joined on every shown change.
    pub fn change(
        &mut self,
        before: Option<&Sound>,
        after: &Sound,
        now: Instant,
    ) -> Option<String> {
        match decide(before, after, self.own, now) {
            Decision::Nothing => None,
            Decision::Show(_) if self.menu => self.quiet(now, "osd: menu open, not shown"),
            Decision::Own => self.quiet(now, "osd: own change, not shown"),
            Decision::Show(rows) => {
                self.rows = self.rows.union(rows);
                self.until = Some(now + OSD_HOLD);
                let Sound::On(audio, _) = after else {
                    unreachable!("decide shows only an On reading")
                };
                let hold = OSD_HOLD.as_millis();
                Some(if rows.volume || rows.mute {
                    format!(
                        "osd: volume {:.0}% muted={} (hold {hold}ms)",
                        audio.percent(),
                        yes_no(audio.muted)
                    )
                } else {
                    format!("osd: mic muted={} (hold {hold}ms)", yes_no(audio.mic_muted))
                })
            }
        }
    }

    /// The deadline's sleep woke at `now`. `true` when the burst ended, and
    /// the loop publishes the plain picture.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.until.is_some_and(|until| now >= until) {
            return self.end();
        }
        false
    }

    /// End the burst at once (`ACTION_TOAST_SHOWN`: the toast ended by a
    /// click, a menu or the compositor). `true` when there was one to end.
    pub fn end(&mut self) -> bool {
        let was = self.until.take().is_some() || self.rows.any();
        self.rows = Flagged::NONE;
        was
    }

    /// A change that is not shown: say so once per burst of them.
    fn quiet(&mut self, now: Instant, line: &str) -> Option<String> {
        let new_burst = self
            .quiet
            .is_none_or(|last| now.saturating_duration_since(last) >= OSD_OWN_HOLD);
        self.quiet = Some(now);
        new_burst.then(|| line.to_owned())
    }
}

fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}
