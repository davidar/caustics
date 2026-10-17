//! The one frame this sidecar publishes: `APPLET_POWER`, two `ROW_ITEM`s and
//! a menu under each: the power button with its session menu, and the
//! battery with its own.
//!
//! Whole-state, like every rows frame: there is never a delta to track, and a
//! restart of either end needs no replay. The power menu is Lock and Log Out,
//! a separator, then Suspend, Restart and Shut Down: the two that end the
//! session apart from the three that act on the machine.
//!
//! The battery's menu, top to bottom: the state row (disabled — it is read,
//! not clicked), the brightness slider when there is a backlight, the power
//! modes as radio rows when a power-profiles daemon answers, and
//! "Power settings…" when there is a settings program. Every id is a constant, or a constant plus a profile's
//! index, so a brightness tick changes the slider's `value` and nothing else —
//! and the bar treats a frame whose ids, roles, parents, flags and label and
//! icon ranges are unchanged as one `write_buffer` (docs/abi.md, *Rows*).

use caustics_bar_abi::buffer::APPLET_POWER;
use caustics_bar_abi::wire::{
    Frame, RFLAG_ATTENTION, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_RADIO, RFLAG_SHOW,
    RowsBuilder,
};

use crate::backlight::Brightness;
use crate::profiles::{self, Profiles};
use crate::upower::{
    Battery, STATE_CHARGING, STATE_DISCHARGING, STATE_EMPTY, STATE_FULLY_CHARGED,
    STATE_PENDING_CHARGE, STATE_PENDING_DISCHARGE,
};

/// The row ids. Small integers, stable across every frame this process
/// writes — which is the contract: an `Action` comes back naming one of
/// these, and the caustic holds an open menu by them (docs/abi.md, *Rows*).
pub const ROW_POWER: u32 = 1;
pub const ROW_LOCK: u32 = 2;
pub const ROW_LOG_OUT: u32 = 3;
pub const ROW_SEP: u32 = 4;
pub const ROW_SUSPEND: u32 = 5;
pub const ROW_RESTART: u32 = 6;
pub const ROW_SHUTDOWN: u32 = 7;
pub const ROW_BATTERY: u32 = 8;
/// The battery's menu.
pub const ROW_STATE: u32 = 9;
pub const ROW_BRIGHTNESS: u32 = 10;
pub const ROW_MODES_SEP: u32 = 11;
pub const ROW_SETTINGS_SEP: u32 = 12;
pub const ROW_SETTINGS: u32 = 13;
/// A power mode's row id is this plus its index in the daemon's `Profiles`,
/// which is the same list in the same order for the daemon's whole life.
pub const ROW_PROFILE_BASE: u32 = 0x100;
/// More profiles than any daemon has; the rest are not rows.
pub const MAX_PROFILES: usize = 16;

/// The power button's own icon; the menu rows' icons are in [`MENU`].
pub const POWER_ICON: &str = "system-shutdown-symbolic";

/// The power menu, top to bottom: id, label, icon.
pub const MENU: [(u32, &str, &str); 5] = [
    (ROW_LOCK, "Lock", "system-lock-screen-symbolic"),
    (ROW_LOG_OUT, "Log Out", "system-log-out-symbolic"),
    (ROW_SUSPEND, "Suspend", "system-suspend-symbolic"),
    (ROW_RESTART, "Restart", "system-reboot-symbolic"),
    (ROW_SHUTDOWN, "Shut Down", "system-shutdown-symbolic"),
];

/// The battery menu's fixed labels, in one place because the tests read them.
pub const LABEL_BRIGHTNESS: &str = "Brightness";
pub const LABEL_SETTINGS: &str = "Power settings…";
/// `preferences-system-symbolic`, as `caustics-audio`'s settings row has.
pub const SETTINGS_ICON: &str = "preferences-system-symbolic";

/// Where the separator goes: before this many rows of [`MENU`] have been
/// written, i.e. between Log Out and Suspend.
const SEPARATOR_AFTER: usize = 2;

/// Everything the frame is made of. `battery` is `None` when there is no
/// battery (`IsPresent = false`) or no UPower at all — in both cases the frame
/// has no battery row and so no battery menu. `brightness` is `None` without a
/// backlight, `profiles` without a power-profiles daemon. `osd` is the
/// brightness on-screen display (`crate::osd`): part of the picture, so a
/// flag changing alone is a frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picture {
    pub battery: Option<Battery>,
    pub brightness: Option<Brightness>,
    pub profiles: Option<Profiles>,
    pub osd: bool,
}

/// The whole frame. `settings` says whether a settings program can be named
/// ([`crate::actions::settings_available`]); without one the battery menu
/// ends without "Power settings…" and the separator above it, since a row
/// that opens nothing is worse than no row.
pub fn picture(revision: u32, picture: &Picture, settings: bool) -> Frame {
    let mut rows = RowsBuilder::new(APPLET_POWER, revision);
    let button = rows.item(ROW_POWER, None, Some(POWER_ICON), -1.0, RFLAG_HAS_MENU);
    for (i, (id, label, icon)) in MENU.iter().enumerate() {
        if i == SEPARATOR_AFTER {
            rows.separator(button, ROW_SEP);
        }
        rows.menu(button, *id, Some(label), Some(icon), 0);
    }
    let Some(battery) = picture.battery.as_ref() else {
        return rows.frame();
    };
    // The brightness on-screen display: the battery chip asks for a toast and
    // the slider is its row. The slider hangs off the battery chip, so a
    // machine with no battery has no brightness toast; such a machine (a
    // desktop) has no backlight either.
    let osd = picture.osd && picture.brightness.is_some();
    // The percentage as the label and the fraction as the level on the icon;
    // the icon name is UPower's own, which already encodes charging.
    let chip = rows.item(
        ROW_BATTERY,
        Some(&format!("{:.0}%", battery.percent)),
        Some(&battery.icon),
        battery.percent / 100.0,
        RFLAG_HAS_MENU | if osd { RFLAG_SHOW } else { 0 },
    );
    rows.menu(
        chip,
        ROW_STATE,
        Some(&state_line(battery)),
        None,
        RFLAG_DISABLED,
    );
    if let Some(brightness) = picture.brightness {
        // Raw units both ways: the position is value over max, and a tick
        // comes back in the same units `SetBrightness` takes.
        rows.slider(
            chip,
            ROW_BRIGHTNESS,
            Some(LABEL_BRIGHTNESS),
            brightness.value as f32,
            0.0,
            brightness.max as f32,
            if osd { RFLAG_ATTENTION } else { 0 },
        );
    }
    if let Some(modes) = picture.profiles.as_ref() {
        rows.separator(chip, ROW_MODES_SEP);
        for (i, name) in modes.names.iter().take(MAX_PROFILES).enumerate() {
            let checked = if *name == modes.active {
                RFLAG_CHECKED
            } else {
                0
            };
            rows.menu(
                chip,
                ROW_PROFILE_BASE + i as u32,
                Some(profiles::label(name)),
                profiles::icon(name),
                RFLAG_RADIO | checked,
            );
        }
    }
    if settings {
        rows.separator(chip, ROW_SETTINGS_SEP);
        rows.menu(
            chip,
            ROW_SETTINGS,
            Some(LABEL_SETTINGS),
            Some(SETTINGS_ICON),
            0,
        );
    }
    rows.frame()
}

/// The state row's words, from UPower's `State` and its two times:
///
/// - charging: `"87% · 1 h 5 m until full"`, or `"87% · Charging"` while
///   UPower has no estimate;
/// - discharging: `"87% · 2 h 13 m until empty"`, or `"87% · On battery"`;
/// - fully charged: `"Fully charged"`;
/// - pending charge (plugged in, not charging — a charge threshold):
///   `"77% · On AC"` (the percentage, because the bare words would hide how
///   far the threshold let it charge);
/// - anything else: the percentage alone.
pub fn state_line(battery: &Battery) -> String {
    let percent = format!("{:.0}%", battery.percent);
    match battery.state {
        STATE_CHARGING => match duration(battery.time_to_full) {
            Some(time) => format!("{percent} · {time} until full"),
            None => format!("{percent} · Charging"),
        },
        STATE_DISCHARGING | STATE_PENDING_DISCHARGE | STATE_EMPTY => {
            match duration(battery.time_to_empty) {
                Some(time) => format!("{percent} · {time} until empty"),
                None => format!("{percent} · On battery"),
            }
        }
        STATE_FULLY_CHARGED => "Fully charged".to_owned(),
        STATE_PENDING_CHARGE => format!("{percent} · On AC"),
        _ => percent,
    }
}

/// Seconds as `"2 h 13 m"`, `"2 h"`, or `"13 m"`, to the nearest minute and
/// never less than one; `None` for UPower's "no estimate" (zero or negative).
pub fn duration(seconds: i64) -> Option<String> {
    if seconds <= 0 {
        return None;
    }
    // Saturating: UPower's `x` is an `i64`, and `i64::MAX + 30` is a panic in
    // a debug build and a negative duration in a release one.
    let minutes = (seconds.saturating_add(30) / 60).max(1);
    let (h, m) = (minutes / 60, minutes % 60);
    Some(match (h, m) {
        (0, m) => format!("{m} m"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} m"),
    })
}
