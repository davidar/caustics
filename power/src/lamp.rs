//! The brightness slider as the loop knows it: which device, its last
//! reading, when the slider last moved, and whether the battery's menu is open
//! (the loop's own logic, tested in `tests/lamp.rs`).
//!
//! The rule it keeps: sysfs is read **once when the battery's menu opens and
//! once a [`crate::BRIGHTNESS_POLL`] while it stays open, never while it is
//! closed**, and a poll within [`crate::TICK_HOLD`] of the last slider tick
//! does not move the slider (logind writes the file a round trip after the
//! tick, and a read in between would pull the slider back under the pointer
//! for a frame). Time is a parameter, so `power/tests/lamp.rs` drives the hold
//! without sleeping.

use std::path::PathBuf;
use std::time::Instant;

use caustics_bar_abi::buffer::ACTION_MENU_OPEN;

use crate::backlight::{self, Backlight, Brightness};
use crate::rows::ROW_BATTERY;
use crate::{TICK_HOLD, witness};

pub struct Lamp {
    /// The backlight class directory: `/sys/class/backlight`, or a test's.
    root: PathBuf,
    backlight: Option<Backlight>,
    brightness: Option<Brightness>,
    last_tick: Option<Instant>,
    /// Whether the battery's menu is open, which is whether to poll.
    open: bool,
    /// Whether [`Lamp::sample`] has run, so the first look is witnessed
    /// either way and later ones only when the device changed.
    looked: bool,
    /// Whether "a tick with no system bus" has been said yet.
    told_no_bus: bool,
}

impl Lamp {
    /// A lamp over `root`, looked at once so the first frame has its slider
    /// (or does not).
    pub fn new(root: impl Into<PathBuf>) -> Lamp {
        let mut lamp = Lamp {
            root: root.into(),
            backlight: None,
            brightness: None,
            last_tick: None,
            open: false,
            looked: false,
            told_no_bus: false,
        };
        lamp.sample(None);
        lamp
    }

    /// The slider's reading, as the frame shows it.
    pub fn brightness(&self) -> Option<Brightness> {
        self.brightness
    }

    /// The device a tick sets.
    pub fn backlight(&self) -> Option<&Backlight> {
        self.backlight.as_ref()
    }

    /// Whether the loop's poll arm is live: the battery's menu is open.
    pub fn polling(&self) -> bool {
        self.open
    }

    /// `ACTION_MENU_OPEN` or `ACTION_MENU_CLOSE` for `row`. Only the battery's
    /// menu has anything to read; opening it reads now, whatever the hold
    /// says, because a menu that opens on a stale value is the bug the poll is
    /// for. Returns whether the menu just opened (the loop restarts its
    /// interval then, so the next poll is a whole period away).
    pub fn notice(&mut self, row: u32, kind: u32) -> bool {
        if row != ROW_BATTERY {
            return false;
        }
        self.open = kind == ACTION_MENU_OPEN;
        if self.open {
            self.sample(None);
        }
        self.open
    }

    /// The poll's tick at `now`: a read while the menu is open, nothing while
    /// it is closed. Returns whether it read.
    pub fn poll(&mut self, now: Instant) -> bool {
        if !self.open {
            return false;
        }
        self.sample(Some(now));
        true
    }

    /// Look again. A device that appeared or vanished is always taken (and
    /// witnessed); a changed value on the same device is taken unless `now`
    /// is within [`TICK_HOLD`] of the last slider tick.
    fn sample(&mut self, now: Option<Instant>) {
        let seen = backlight::sample(&self.root);
        let same_device =
            self.backlight.as_ref().map(|b| &b.name) == seen.as_ref().map(|(b, _)| &b.name);
        if !same_device || !std::mem::replace(&mut self.looked, true) {
            match &seen {
                Some((b, v)) => witness(&format!(
                    "backlight: {} {}/{} ({:?})",
                    b.name, v.value, v.max, b.kind
                )),
                None => witness("absent: backlight"),
            }
        }
        let held = match (now, self.last_tick) {
            (Some(now), Some(tick)) => now.saturating_duration_since(tick) < TICK_HOLD,
            _ => false,
        };
        if same_device && held && seen.is_some() {
            return;
        }
        let (backlight, brightness) = seen.unzip();
        self.backlight = backlight;
        self.brightness = brightness;
    }

    /// A slider tick at `now`: the new raw value, or `None` when there is no
    /// slider or the value did not change.
    pub fn tick(&mut self, value: f32, now: Instant) -> Option<u32> {
        let floor = self.backlight.as_ref()?.floor(self.brightness?.max);
        let current = self.brightness.as_mut()?;
        let raw = backlight::to_raw(value, current.max, floor);
        self.last_tick = Some(now);
        if raw == current.value {
            return None;
        }
        current.value = raw;
        Some(raw)
    }

    /// Whether "there is no system bus" is still to be said; `true` once.
    pub fn say_no_bus(&mut self) -> bool {
        !std::mem::replace(&mut self.told_no_bus, true)
    }
}
