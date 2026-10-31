//! The one frame this sidecar publishes: `APPLET_BLUETOOTH`, a chip, the
//! adapter's on/off row, one row per paired device, one per device in range
//! that is not paired yet (while the menu is open), and the settings row.
//!
//! Whole-state, like every rows frame: there is never a delta to track, and a
//! restart of either end needs no replay.
//!
//! **Ids.** The fixed rows have constant ids; a device's row id is derived from
//! its **address** (FNV-1a, with bit 8 set so it can never collide with a fixed
//! id, and bit 9 saying whether it is a device in range), never from its
//! position. That is the contract the bar's
//! rebuild-vs-upload distinction rests on: ids, roles and parents stay put when
//! only a value moved, so a device connecting does not move a menu row's index
//! under the popup it is open inside of (docs/abi.md, *Rows*).
//!
//! **Clicks** (`crate::actions`). A paired device's row connects it or
//! disconnects it, and a **middle click forgets it** (`RemoveDevice`) at once,
//! with no confirmation: the bar has none, and the device can be paired
//! again. A device in range's row pairs it, trusts it and connects it.

use caustics_bar_abi::buffer::APPLET_BLUETOOTH;
use caustics_bar_abi::wire::{
    Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, RowsBuilder,
};

use crate::bluez::State;

/// The bar item. A click opens the menu (`RFLAG_HAS_MENU`); a middle click
/// comes back as `ACTION_MIDDLE` and toggles the radio.
pub const ROW_CHIP: u32 = 1;
/// "Bluetooth": `CHECKABLE`, `CHECKED` when the adapter is powered.
pub const ROW_POWER: u32 = 2;
/// Between the toggle and the devices.
pub const ROW_SEP_DEVICES: u32 = 3;
/// Between the devices and Settings, written only when there was a device
/// and there is a settings row.
pub const ROW_SEP_SETTINGS: u32 = 4;
/// "Bluetooth Settings": opens the settings program's Bluetooth page
/// (`caustics_sidecar::settings`); written only when there is one to open.
pub const ROW_SETTINGS: u32 = 5;
/// Between the paired devices and the devices in range, written only when
/// there are both. Its id is 6, after `ROW_SETTINGS`, because ids are never
/// renumbered.
pub const ROW_SEP_NEARBY: u32 = 6;

/// The bit every device row id carries, so no derived id can ever equal one of
/// the constants above.
pub const DEVICE_ID_BIT: u32 = 0x100;
/// The bit a device in range's row id carries and a paired device's never
/// does, so the two lists cannot share an id.
pub const NEARBY_ID_BIT: u32 = 0x200;

/// The chip's icon when the adapter is on, and when it is off.
pub const ICON_ON: &str = "bluetooth-active-symbolic";
pub const ICON_OFF: &str = "bluetooth-disabled-symbolic";
/// A device whose `Icon` BlueZ does not know. Many devices declare no class
/// BlueZ can map to an icon (a tracker tag, for one), so this is an ordinary
/// case, not a rare one.
pub const ICON_DEVICE: &str = "bluetooth-symbolic";

pub const POWER_LABEL: &str = "Bluetooth";
pub const SETTINGS_LABEL: &str = "Bluetooth Settings";

/// A paired device's row id: FNV-1a over its address, with [`DEVICE_ID_BIT`]
/// set and [`NEARBY_ID_BIT`] clear.
///
/// The address is the one property of a paired device that never changes — an
/// alias is renamed, a path is BlueZ's own spelling of the address, an index
/// moves whenever a device is paired or removed.
pub fn device_row(address: &str) -> u32 {
    (fnv1a(address) | DEVICE_ID_BIT) & !NEARBY_ID_BIT
}

/// A device in range's row id: the same hash with both bits set.
pub fn nearby_row(address: &str) -> u32 {
    fnv1a(address) | DEVICE_ID_BIT | NEARBY_ID_BIT
}

fn fnv1a(address: &str) -> u32 {
    const OFFSET: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;

    let mut hash = OFFSET;
    for byte in address.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The whole picture. No adapter is a frame with **no rows**: the chip
/// disappears and the bar's health line still says `running`; a missing
/// BlueZ costs this chip and nothing else.
///
/// `settings` says whether a settings program can be named
/// ([`crate::actions::settings_available`]). Without one the settings row is
/// left out, and so is any separator that would then end the menu: a row
/// that opens nothing is worse than no row.
pub fn picture(revision: u32, state: &State, settings: bool) -> Frame {
    let mut rows = RowsBuilder::new(APPLET_BLUETOOTH, revision);
    let Some(adapter) = state.adapter.as_ref() else {
        return rows.frame();
    };

    let icon = if adapter.powered { ICON_ON } else { ICON_OFF };
    // `level` −1: a radio is on or off, and a fraction drawn on the icon would
    // be a number this applet does not have.
    let chip = rows.item(ROW_CHIP, None, Some(icon), -1.0, RFLAG_HAS_MENU);

    let mut flags = RFLAG_CHECKABLE;
    if adapter.powered {
        flags |= RFLAG_CHECKED;
    }
    rows.menu(chip, ROW_POWER, Some(POWER_LABEL), None, flags);
    let listed = !state.devices.is_empty() || !state.nearby.is_empty();
    if settings || listed {
        rows.separator(chip, ROW_SEP_DEVICES);
    }

    for device in &state.devices {
        let mut flags = RFLAG_CHECKABLE;
        if device.connected {
            flags |= RFLAG_CHECKED;
        }
        // BlueZ cannot connect through a radio that is off, so the row says so
        // rather than failing when it is clicked.
        if !adapter.powered {
            flags |= RFLAG_DISABLED;
        }
        let icon = if device.icon.is_empty() {
            ICON_DEVICE
        } else {
            device.icon.as_str()
        };
        rows.menu(
            chip,
            device_row(&device.address),
            Some(&device.alias),
            Some(icon),
            flags,
        );
    }
    if !state.devices.is_empty() && !state.nearby.is_empty() {
        rows.separator(chip, ROW_SEP_NEARBY);
    }
    // The devices in range that are not paired. **No `CHECKABLE`**: the
    // missing mark is what tells them from the paired ones above. A click
    // pairs (`crate::actions`); whatever the pairing asks is asked by the
    // canvas's prompt, never here.
    for device in &state.nearby {
        let icon = if device.icon.is_empty() {
            ICON_DEVICE
        } else {
            device.icon.as_str()
        };
        rows.menu(
            chip,
            nearby_row(&device.address),
            Some(&device.alias),
            Some(icon),
            0,
        );
    }
    if settings {
        if listed {
            rows.separator(chip, ROW_SEP_SETTINGS);
        }
        rows.menu(chip, ROW_SETTINGS, Some(SETTINGS_LABEL), None, 0);
    }

    rows.frame()
}
