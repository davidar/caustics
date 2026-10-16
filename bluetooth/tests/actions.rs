//! The dispatch: each row id, with each action kind, to the call it would
//! make — asserted against the *description* of the call, so no test ever
//! powers an adapter, connects a device or disconnects one.
//!
//! The state is handed in by hand here rather than parsed: what these assert is
//! the table, and `frames.rs` is where the fixture bus bytes are. The one
//! place the table differs from `caustics-power`'s is that it takes the state —
//! a device row's id is derived from its address, and what a click means depends
//! on whether that device is connected.

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MIDDLE, ACTION_SECONDARY, ACTION_SET_VALUE,
};
use caustics_bluetooth::actions::{Call, Op, SETTINGS_PANEL, op_for};
use caustics_bluetooth::bluez::{ADAPTER, Adapter, CONNECT, DISCONNECT, Device, State};
use caustics_bluetooth::rows::{
    ROW_CHIP, ROW_POWER, ROW_SEP_DEVICES, ROW_SEP_SETTINGS, ROW_SETTINGS, device_row,
};

const HCI0: &str = "/org/bluez/hci0";
const MOUSE_ADDRESS: &str = "02:00:00:00:EE:01";
const BUDS_ADDRESS: &str = "02:00:00:00:EE:02";
const MOUSE_PATH: &str = "/org/bluez/hci0/dev_02_00_00_00_EE_01";
const BUDS_PATH: &str = "/org/bluez/hci0/dev_02_00_00_00_EE_02";

/// One adapter, one idle mouse, one connected pair of buds.
fn state(powered: bool) -> State {
    State {
        adapter: Some(Adapter {
            path: HCI0.to_owned(),
            name: "workstation".to_owned(),
            powered,
        }),
        devices: vec![
            Device {
                path: MOUSE_PATH.to_owned(),
                address: MOUSE_ADDRESS.to_owned(),
                alias: "MX Master".to_owned(),
                icon: "input-mouse".to_owned(),
                connected: false,
            },
            Device {
                path: BUDS_PATH.to_owned(),
                address: BUDS_ADDRESS.to_owned(),
                alias: "Pixel Buds".to_owned(),
                icon: "audio-headset".to_owned(),
                connected: true,
            },
        ],
    }
}

fn mouse_row() -> u32 {
    device_row(MOUSE_ADDRESS)
}

fn buds_row() -> u32 {
    device_row(BUDS_ADDRESS)
}

#[test]
fn the_toggle_row_sets_powered_to_its_negation() {
    assert_eq!(
        op_for(&state(true), ROW_POWER, ACTION_ACTIVATE),
        Some(Op::Power {
            path: HCI0.to_owned(),
            on: false
        })
    );
    assert_eq!(
        op_for(&state(false), ROW_POWER, ACTION_ACTIVATE),
        Some(Op::Power {
            path: HCI0.to_owned(),
            on: true
        })
    );
}

#[test]
fn a_middle_click_on_the_chip_is_the_same_toggle() {
    // The one action a bar item of this applet performs: its left click opens
    // the menu and never comes back (RFLAG_HAS_MENU).
    assert_eq!(
        op_for(&state(true), ROW_CHIP, ACTION_MIDDLE),
        op_for(&state(true), ROW_POWER, ACTION_ACTIVATE)
    );
    assert_eq!(op_for(&state(true), ROW_CHIP, ACTION_ACTIVATE), None);
}

#[test]
fn a_device_row_connects_when_it_is_not_connected_and_disconnects_when_it_is() {
    let state = state(true);
    assert_eq!(
        op_for(&state, mouse_row(), ACTION_ACTIVATE),
        Some(Op::Connect {
            path: MOUSE_PATH.to_owned(),
            alias: "MX Master".to_owned()
        })
    );
    assert_eq!(
        op_for(&state, buds_row(), ACTION_ACTIVATE),
        Some(Op::Disconnect {
            path: BUDS_PATH.to_owned(),
            alias: "Pixel Buds".to_owned()
        })
    );
}

#[test]
fn each_op_names_the_call_it_would_make() {
    assert_eq!(
        Op::Power {
            path: HCI0.to_owned(),
            on: true
        }
        .call(),
        Call::SetPowered {
            interface: ADAPTER,
            path: HCI0.to_owned(),
            on: true
        }
    );
    assert_eq!(
        Op::Connect {
            path: MOUSE_PATH.to_owned(),
            alias: "MX Master".to_owned()
        }
        .call(),
        Call::Device {
            member: CONNECT,
            path: MOUSE_PATH.to_owned()
        }
    );
    assert_eq!(
        Op::Disconnect {
            path: BUDS_PATH.to_owned(),
            alias: "Pixel Buds".to_owned()
        }
        .call(),
        Call::Device {
            member: DISCONNECT,
            path: BUDS_PATH.to_owned()
        }
    );
    assert_eq!(Op::Settings.call(), Call::Settings(SETTINGS_PANEL));
    assert_eq!(SETTINGS_PANEL, caustics_sidecar::settings::Panel::Bluetooth);
}

/// The settings row names a panel, never a program: `$CAUSTICS_SETTINGS` wins,
/// the first known program on `PATH` follows, and with neither there is
/// nothing to run (the click is witnessed and nothing starts).
#[test]
fn the_settings_program_is_the_override_or_the_first_found_or_none() {
    use caustics_sidecar::settings::resolve;
    use std::ffi::OsStr;

    let found = |names: &'static [&'static str]| move |name: &str| names.contains(&name);
    let gnome = Some(OsStr::new("gnome-control-center"));
    let chosen = resolve(SETTINGS_PANEL, gnome, None, found(&[])).expect("an override is taken");
    assert_eq!(chosen.describe(), "gnome-control-center bluetooth");
    let niri = Some(OsStr::new("niri"));
    let chosen = resolve(SETTINGS_PANEL, None, niri, found(&["cosmic-settings"]))
        .expect("cosmic-settings is on PATH");
    assert_eq!(chosen.describe(), "cosmic-settings bluetooth");
    assert_eq!(resolve(SETTINGS_PANEL, None, niri, found(&[])), None);
}

#[test]
fn every_op_says_what_it_is_in_its_witness() {
    let names: Vec<String> = [
        Op::Power {
            path: HCI0.to_owned(),
            on: true,
        },
        Op::Power {
            path: HCI0.to_owned(),
            on: false,
        },
        Op::Connect {
            path: MOUSE_PATH.to_owned(),
            alias: "MX Master".to_owned(),
        },
        Op::Disconnect {
            path: BUDS_PATH.to_owned(),
            alias: "Pixel Buds".to_owned(),
        },
        Op::Settings,
    ]
    .iter()
    .map(|op| op.describe())
    .collect();
    assert_eq!(
        names,
        vec![
            "power on",
            "power off",
            "connect MX Master",
            "disconnect Pixel Buds",
            "settings"
        ]
    );
}

#[test]
fn a_device_row_does_nothing_while_the_radio_is_off() {
    // The rows are `DISABLED` in that frame, so the bar should not send these
    // at all; the table says no a second time rather than failing on the bus.
    let off = state(false);
    assert_eq!(op_for(&off, mouse_row(), ACTION_ACTIVATE), None);
    assert_eq!(op_for(&off, buds_row(), ACTION_ACTIVATE), None);
    // The toggle and Settings still work with the radio off — that is the point
    // of the toggle.
    assert!(op_for(&off, ROW_POWER, ACTION_ACTIVATE).is_some());
    assert!(op_for(&off, ROW_SETTINGS, ACTION_ACTIVATE).is_some());
}

#[test]
fn the_rows_that_perform_nothing_perform_nothing() {
    let state = state(true);
    assert_eq!(op_for(&state, ROW_SEP_DEVICES, ACTION_ACTIVATE), None);
    assert_eq!(op_for(&state, ROW_SEP_SETTINGS, ACTION_ACTIVATE), None);
    // An id this applet never wrote — a stale action for a device that has been
    // removed arrives exactly like this.
    assert_eq!(
        op_for(&state, device_row("02:11:22:33:44:66"), ACTION_ACTIVATE),
        None
    );
    assert_eq!(op_for(&state, 9999, ACTION_ACTIVATE), None);
}

#[test]
fn with_no_adapter_nothing_at_all_performs() {
    // There are no rows in that frame, so any action naming one is stale.
    let empty = State::default();
    for row in [ROW_CHIP, ROW_POWER, ROW_SETTINGS, mouse_row()] {
        for kind in [ACTION_ACTIVATE, ACTION_MIDDLE] {
            assert_eq!(op_for(&empty, row, kind), None, "row {row} kind {kind}");
        }
    }
}

#[test]
fn only_activate_and_the_chip_s_middle_click_perform() {
    let state = state(true);
    for kind in [ACTION_SECONDARY, ACTION_SET_VALUE] {
        for row in [ROW_CHIP, ROW_POWER, ROW_SETTINGS, mouse_row(), buds_row()] {
            assert_eq!(op_for(&state, row, kind), None, "row {row} kind {kind}");
        }
    }
    // A middle click is the radio on the chip and nothing on a menu row.
    for row in [ROW_POWER, ROW_SETTINGS, mouse_row(), buds_row()] {
        assert_eq!(op_for(&state, row, ACTION_MIDDLE), None, "row {row}");
    }
}
