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
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SECONDARY,
    ACTION_SET_VALUE,
};
use caustics_bluetooth::actions::{Call, Discovery, Op, SETTINGS_PANEL, op_for};
use caustics_bluetooth::bluez::{ADAPTER, Adapter, CONNECT, DISCONNECT, Device, State};
use caustics_bluetooth::rows::{
    ROW_CHIP, ROW_POWER, ROW_SEP_DEVICES, ROW_SEP_NEARBY, ROW_SEP_SETTINGS, ROW_SETTINGS,
    device_row, nearby_row,
};

const HCI0: &str = "/org/bluez/hci0";
const MOUSE_ADDRESS: &str = "02:00:00:00:EE:01";
const BUDS_ADDRESS: &str = "02:00:00:00:EE:02";
const MOUSE_PATH: &str = "/org/bluez/hci0/dev_02_00_00_00_EE_01";
const BUDS_PATH: &str = "/org/bluez/hci0/dev_02_00_00_00_EE_02";
const SPEAKER_ADDRESS: &str = "02:00:00:00:EE:03";
const SPEAKER_PATH: &str = "/org/bluez/hci0/dev_02_00_00_00_EE_03";
const HCI1: &str = "/org/bluez/hci1";

/// One adapter, discovering, one idle mouse, one connected pair of buds, and
/// a speaker in range.
fn state(powered: bool) -> State {
    State {
        adapter: Some(Adapter {
            path: HCI0.to_owned(),
            name: "workstation".to_owned(),
            powered,
            discovering: powered,
        }),
        devices: vec![
            Device {
                path: MOUSE_PATH.to_owned(),
                address: MOUSE_ADDRESS.to_owned(),
                alias: "MX Master".to_owned(),
                icon: "input-mouse".to_owned(),
                connected: false,
                rssi: None,
            },
            Device {
                path: BUDS_PATH.to_owned(),
                address: BUDS_ADDRESS.to_owned(),
                alias: "Pixel Buds".to_owned(),
                icon: "audio-headset".to_owned(),
                connected: true,
                rssi: None,
            },
        ],
        nearby: vec![Device {
            path: SPEAKER_PATH.to_owned(),
            address: SPEAKER_ADDRESS.to_owned(),
            alias: "Example Speaker".to_owned(),
            icon: "audio-card".to_owned(),
            connected: false,
            rssi: Some(-50),
        }],
        radios: if powered {
            vec![HCI0.to_owned()]
        } else {
            Vec::new()
        },
    }
}

fn speaker_row() -> u32 {
    nearby_row(SPEAKER_ADDRESS)
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
        Op::Pair {
            path: SPEAKER_PATH.to_owned(),
            alias: "Example Speaker".to_owned(),
        },
        Op::Forget {
            adapter: HCI0.to_owned(),
            path: BUDS_PATH.to_owned(),
            alias: "Pixel Buds".to_owned(),
        },
        Op::Discover {
            adapter: HCI0.to_owned(),
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
            "pair Example Speaker",
            "forget Pixel Buds",
            "discover on /org/bluez/hci0",
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
fn only_activate_and_middle_clicks_perform() {
    let state = state(true);
    for kind in [ACTION_SECONDARY, ACTION_SET_VALUE] {
        for row in [
            ROW_CHIP,
            ROW_POWER,
            ROW_SETTINGS,
            mouse_row(),
            buds_row(),
            speaker_row(),
        ] {
            assert_eq!(op_for(&state, row, kind), None, "row {row} kind {kind}");
        }
    }
    // A middle click is the radio on the chip, a paired device's forgetting,
    // and nothing on the other menu rows.
    for row in [ROW_POWER, ROW_SETTINGS, ROW_SEP_NEARBY, speaker_row()] {
        assert_eq!(op_for(&state, row, ACTION_MIDDLE), None, "row {row}");
    }
}

#[test]
fn a_device_in_range_pairs_and_a_middle_click_forgets_a_paired_one() {
    let state = state(true);
    assert_eq!(
        op_for(&state, speaker_row(), ACTION_ACTIVATE),
        Some(Op::Pair {
            path: SPEAKER_PATH.to_owned(),
            alias: "Example Speaker".to_owned()
        })
    );
    // Even a connected one, and with the radio off: forgetting needs no radio.
    for (s, row, path, alias) in [
        (state.clone(), buds_row(), BUDS_PATH, "Pixel Buds"),
        (off(), mouse_row(), MOUSE_PATH, "MX Master"),
    ] {
        assert_eq!(
            op_for(&s, row, ACTION_MIDDLE),
            Some(Op::Forget {
                adapter: HCI0.to_owned(),
                path: path.to_owned(),
                alias: alias.to_owned()
            })
        );
    }
    assert_eq!(
        Op::Pair {
            path: SPEAKER_PATH.to_owned(),
            alias: "Example Speaker".to_owned()
        }
        .call(),
        Call::PairTrustConnect {
            path: SPEAKER_PATH.to_owned()
        }
    );
    assert_eq!(
        Op::Forget {
            adapter: HCI0.to_owned(),
            path: BUDS_PATH.to_owned(),
            alias: "Pixel Buds".to_owned()
        }
        .call(),
        Call::RemoveDevice {
            adapter: HCI0.to_owned(),
            device: BUDS_PATH.to_owned()
        }
    );
    // A device in range with the radio off is not paired.
    assert_eq!(op_for(&off(), speaker_row(), ACTION_ACTIVATE), None);
}

fn off() -> State {
    state(false)
}

#[test]
fn discovery_runs_on_every_powered_adapter_while_the_menu_is_open() {
    let mut state = state(true);
    state.radios.push(HCI1.to_owned());
    let mut discovery = Discovery::default();
    // Nothing while closed.
    assert_eq!(discovery.changed(&state), vec![]);
    assert_eq!(
        discovery.notice(ACTION_MENU_OPEN, &state),
        Some(vec![
            Op::Discover {
                adapter: HCI0.to_owned()
            },
            Op::Discover {
                adapter: HCI1.to_owned()
            },
        ])
    );
    assert!(discovery.is_open());
    // The same picture again starts nothing twice.
    assert_eq!(discovery.changed(&state), vec![]);
    // An adapter powered off has stopped by itself; powered again, it starts.
    state.radios.retain(|r| r != HCI1);
    assert_eq!(discovery.changed(&state), vec![]);
    state.radios.push(HCI1.to_owned());
    assert_eq!(
        discovery.changed(&state),
        vec![Op::Discover {
            adapter: HCI1.to_owned()
        }]
    );
    assert_eq!(
        discovery.notice(ACTION_MENU_CLOSE, &state),
        Some(vec![
            Op::StopDiscovery {
                adapter: HCI0.to_owned()
            },
            Op::StopDiscovery {
                adapter: HCI1.to_owned()
            },
        ])
    );
    assert!(!discovery.is_open());
    assert_eq!(discovery.close(), vec![], "nothing left to stop");
    assert_eq!(discovery.notice(ACTION_ACTIVATE, &state), None);
    assert_eq!(
        Op::Discover {
            adapter: HCI0.to_owned()
        }
        .call(),
        Call::StartDiscovery {
            adapter: HCI0.to_owned()
        }
    );
    assert_eq!(
        Op::StopDiscovery {
            adapter: HCI0.to_owned()
        }
        .describe(),
        "stop discovery on /org/bluez/hci0"
    );
}
