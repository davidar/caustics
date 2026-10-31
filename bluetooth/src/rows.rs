//! The one frame this sidecar publishes: `APPLET_BLUETOOTH`, a chip, the
//! adapter's on/off row, one row per paired device, one per device in range
//! that is not paired yet (while the menu is open).
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
//! disconnects it, and a **right click forgets it** (`RemoveDevice`; a
//! touchpad's two-finger tap, or a mouse's middle button) at once, with no
//! confirmation: the bar has none, and the device can be paired again. A device in range's row pairs it, trusts it and connects it. The
//! agent's rows answer the question they ask ([`crate::pairing`]).

use caustics_bar_abi::buffer::APPLET_BLUETOOTH;
use caustics_bar_abi::wire::{
    Frame, RFLAG_ATTENTION, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU,
    RFLAG_SHOW, RowsBuilder,
};

use crate::ask;
use crate::bluez::State;
use crate::pairing::View;

/// The bar item. A click opens the menu (`RFLAG_HAS_MENU`); a middle click
/// comes back as `ACTION_MIDDLE` and toggles the radio.
pub const ROW_CHIP: u32 = 1;
/// "Bluetooth": `CHECKABLE`, `CHECKED` when the adapter is powered.
pub const ROW_POWER: u32 = 2;
/// Between the toggle and the devices, written only when a device is listed.
pub const ROW_SEP_DEVICES: u32 = 3;
// 4 was the separator above the settings row and 5 the settings row; ids are
// never reused.
/// Between the paired devices and the devices in range, written only when
/// there are both. Its id is 6, after the retired 4 and 5, because ids are
/// never renumbered.
pub const ROW_SEP_NEARBY: u32 = 6;
/// The pairing agent's rows (`crate::pairing`), above the toggle while it
/// asks something: the yes-or-no question, disabled so a click on it in a
/// toast does nothing, then its two answers, then a code to type on the
/// other device, then a separator in the menu. Fixed ids below
/// [`DEVICE_ID_BIT`], so no device row can wear one.
pub const ROW_QUESTION: u32 = 7;
pub const ROW_CONFIRM: u32 = 8;
pub const ROW_REJECT: u32 = 9;
pub const ROW_DISPLAY: u32 = 10;
pub const ROW_SEP_AGENT: u32 = 11;

/// The bit every device row id carries, so no derived id can ever equal one of
/// the constants above or the retired ids.
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

/// The whole picture with nothing asked: [`picture_with`] and an empty
/// [`View`].
pub fn picture(revision: u32, state: &State) -> Frame {
    picture_with(revision, state, &View::default())
}

/// The whole picture. No adapter is a frame with **no rows**: the chip
/// disappears and the bar's health line still says `running`; a missing
/// BlueZ costs this chip and nothing else. The one exception is the agent
/// asking something (`asking`), which is news whatever the picture: the
/// chip is there for its rows, and the menu is those rows alone.
///
/// While the agent asks, the chip wears `RFLAG_SHOW` and its rows
/// `RFLAG_ATTENTION`, so the bar shows them as a toast without a grab
/// (docs/abi.md, *Toasts*), and they lead the menu when it is open.
pub fn picture_with(revision: u32, state: &State, asking: &View) -> Frame {
    let mut rows = RowsBuilder::new(APPLET_BLUETOOTH, revision);
    let adapter = state.adapter.as_ref();
    if adapter.is_none() && !asking.showing() {
        return rows.frame();
    }

    let powered = adapter.is_some_and(|a| a.powered);
    let icon = if powered { ICON_ON } else { ICON_OFF };
    let mut flags = RFLAG_HAS_MENU;
    if asking.showing() {
        flags |= RFLAG_SHOW;
    }
    // `level` −1: a radio is on or off, and a fraction drawn on the icon would
    // be a number this applet does not have.
    let chip = rows.item(ROW_CHIP, None, Some(icon), -1.0, flags);

    if let Some(question) = &asking.question {
        rows.menu(
            chip,
            ROW_QUESTION,
            Some(question),
            None,
            RFLAG_ATTENTION | RFLAG_DISABLED,
        );
        rows.menu(chip, ROW_CONFIRM, Some(ask::CONFIRM), None, RFLAG_ATTENTION);
        rows.menu(chip, ROW_REJECT, Some(ask::REJECT), None, RFLAG_ATTENTION);
    }
    if let Some(display) = &asking.display {
        rows.menu(chip, ROW_DISPLAY, Some(display), None, RFLAG_ATTENTION);
    }
    let Some(adapter) = adapter else {
        return rows.frame();
    };
    if asking.showing() {
        rows.separator(chip, ROW_SEP_AGENT);
    }

    let mut flags = RFLAG_CHECKABLE;
    if adapter.powered {
        flags |= RFLAG_CHECKED;
    }
    rows.menu(chip, ROW_POWER, Some(POWER_LABEL), None, flags);
    if !state.devices.is_empty() || !state.nearby.is_empty() {
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
    // pairs (`crate::actions`); whatever the pairing asks, this sidecar's
    // agent asks: a yes or a no in the rows above, a code to type on the
    // canvas's prompt.
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

    rows.frame()
}
