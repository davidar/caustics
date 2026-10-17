//! The on-screen display for the brightness: a backlight change that did not
//! come from the bar's own slider raises the slider as a toast for
//! [`crate::OSD_HOLD`] (docs/abi.md, **Toasts**). The battery chip carries
//! `RFLAG_SHOW` and the slider `RFLAG_ATTENTION`; the caustic does the rest,
//! and a change while the toast is up moves the slider in place.
//!
//! The trigger is the kernel's uevent (`crate::uevent`), and the read it
//! causes is [`crate::lamp::Lamp::uevent`]. [`decide`] is the rule as a pure
//! function; [`Osd`] is the deadline and the witness's memory. Time is a
//! parameter, so `power/tests/osd.rs` drives both without sleeping.

use std::time::Instant;

use crate::backlight::Brightness;
use crate::{OSD_HOLD, TICK_HOLD};

/// What one uevent's read means for the OSD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// No backlight before or after, or the value did not move (a write of
    /// the value already there still emits the uevent).
    Nothing,
    /// The value moved within [`TICK_HOLD`] of the bar's own slider tick:
    /// logind writing what the drag asked for.
    Own,
    /// Show the slider at this reading.
    Show(Brightness),
}

/// The rule. `before` is the slider's last reading, `after` the file's now,
/// `tick` the last slider tick.
pub fn decide(
    before: Option<Brightness>,
    after: Option<Brightness>,
    tick: Option<Instant>,
    now: Instant,
) -> Decision {
    let (Some(before), Some(after)) = (before, after) else {
        return Decision::Nothing;
    };
    if before == after {
        return Decision::Nothing;
    }
    if tick.is_some_and(|tick| now.saturating_duration_since(tick) < TICK_HOLD) {
        return Decision::Own;
    }
    Decision::Show(after)
}

/// The loop's OSD state.
#[derive(Debug, Default)]
pub struct Osd {
    /// When the burst ends; `None` when none is up.
    until: Option<Instant>,
    /// When a change was last suppressed, so the witness is one line per
    /// suppressed burst rather than one per tick of a drag.
    quiet: Option<Instant>,
}

impl Osd {
    /// Whether the next frame carries the bits.
    pub fn on(&self) -> bool {
        self.until.is_some()
    }

    /// When the burst ends, for the loop's sleep.
    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// A uevent's read decided `decision` at `now`, with the battery's menu
    /// open or not. Returns the witness line to say, if any. An open menu
    /// shows the slider already (and its own poll moves it); a flag set under
    /// it would pop a toast the moment the menu closed.
    pub fn change(&mut self, decision: Decision, menu: bool, now: Instant) -> Option<String> {
        match decision {
            Decision::Nothing => None,
            Decision::Own => self.quiet(now, "osd: own change, not shown"),
            Decision::Show(_) if menu => self.quiet(now, "osd: menu open, not shown"),
            Decision::Show(brightness) => {
                self.until = Some(now + OSD_HOLD);
                Some(format!(
                    "osd: brightness {:.0}% (hold {}ms)",
                    percent(brightness),
                    OSD_HOLD.as_millis()
                ))
            }
        }
    }

    /// The deadline's sleep woke at `now`. `true` when the burst ended.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.until.is_some_and(|until| now >= until) {
            return self.end();
        }
        false
    }

    /// End the burst at once (`ACTION_TOAST_SHOWN`, or the battery's menu
    /// opening). `true` when there was one to end.
    pub fn end(&mut self) -> bool {
        self.until.take().is_some()
    }

    /// A change that is not shown: say so once per burst of them.
    fn quiet(&mut self, now: Instant, line: &str) -> Option<String> {
        let new_burst = self
            .quiet
            .is_none_or(|last| now.saturating_duration_since(last) >= TICK_HOLD);
        self.quiet = Some(now);
        new_burst.then(|| line.to_owned())
    }
}

/// A reading as whole percent of its maximum, for the witness.
pub fn percent(brightness: Brightness) -> f64 {
    if brightness.max == 0 {
        return 0.0;
    }
    f64::from(brightness.value) * 100.0 / f64::from(brightness.max)
}
