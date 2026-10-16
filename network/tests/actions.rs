//! The dispatch: each row id, with each action kind, to the call it would
//! make — asserted against the *description* of the call, so no test ever
//! joins a network, leaves one, switches off a radio or opens a settings
//! window.
//!
//! The picture these run against is the one parsed from the fixture walk, for
//! the same reason `frames.rs` uses it: the row ids are derived from the
//! profiles' UUIDs by the same code as on a live bus, so what is asserted here
//! is what would actually come back over the pipe.

use caustics_bar_abi::buffer::ACT_NONE;
// A sidecar's kinds are the wire's `ACTION_*`; `ACT_NONE` (0) from the buffer
// stands in for a zero kind. Kinds 4 and 5 are unused on the wire (a preview
// is between the caustic and the bar process), and a sidecar that meets them
// must ignore them, which these arms check.
use caustics_bar_abi::wire::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SECONDARY,
    ACTION_SET_VALUE,
};
use caustics_network::actions::{Call, Op, SETTINGS_PANEL, dry_run, op_for};
use caustics_network::model::{self, Network, Primary, Snapshot, Visible};
use caustics_network::nm::{self, SettingsByPath};
use caustics_network::rows::{ITEM, SEP_BOTTOM, SEP_TOP, SEP_VISIBLE, SETTINGS, WIFI};

const MANAGED: &[u8] = include_bytes!("data/managed-objects.bin");
const WIFI_ACTIVE: &[u8] = include_bytes!("data/settings-wifi-active.bin");
const WIFI_OTHER: &[u8] = include_bytes!("data/settings-wifi-other.bin");

const PATH_ACTIVE: &str = "/org/freedesktop/NetworkManager/Settings/1";
const PATH_OTHER: &str = "/org/freedesktop/NetworkManager/Settings/4";

fn fixture() -> Snapshot {
    let managed = nm::parse_managed_objects(MANAGED).expect("the fixture walk deserialises");
    let mut cache = SettingsByPath::new();
    for (path, body) in [(PATH_ACTIVE, WIFI_ACTIVE), (PATH_OTHER, WIFI_OTHER)] {
        cache.insert(
            path.to_owned(),
            nm::parse_settings(body).expect("the reply deserialises"),
        );
    }
    model::build(&managed, &cache)
}

/// The fixture picture with the out-of-range profile brought into range, so
/// the one row that can be joined is a row that can be joined. Derived, not in
/// the fixture — NM's idle scan list ages down to the associated AP.
fn with_other_in_range() -> Snapshot {
    let state = fixture();
    Snapshot {
        networks: state
            .networks
            .iter()
            .map(|n| Network {
                strength: Some(n.strength.unwrap_or(62)),
                ..n.clone()
            })
            .collect(),
        ..state
    }
}

#[test]
fn the_wifi_row_toggles_whatever_the_radio_currently_is() {
    let on = fixture();
    assert!(on.wireless_enabled);
    assert_eq!(
        op_for(&on, WIFI, ACTION_ACTIVATE),
        Some(Op::Wireless(false))
    );

    let off = Snapshot {
        wireless_enabled: false,
        ..on
    };
    assert_eq!(
        op_for(&off, WIFI, ACTION_ACTIVATE),
        Some(Op::Wireless(true))
    );
}

#[test]
fn a_middle_click_on_the_chip_is_the_same_switch() {
    // The chip's own click opens the menu (`RFLAG_HAS_MENU`) and never comes
    // back; a middle click does, and means the switch.
    let state = fixture();
    assert_eq!(
        op_for(&state, ITEM, ACTION_MIDDLE),
        op_for(&state, WIFI, ACTION_ACTIVATE)
    );
    assert_eq!(op_for(&state, ITEM, ACTION_ACTIVATE), None);
}

#[test]
fn a_known_network_in_range_activates_by_its_object_path() {
    let state = with_other_in_range();
    let other = &state.networks[1];
    assert_eq!(
        op_for(&state, other.row, ACTION_ACTIVATE),
        Some(Op::Activate {
            path: PATH_OTHER.to_owned(),
            label: "hotspot-example".to_owned()
        })
    );
}

#[test]
fn clicking_the_joined_network_does_nothing() {
    // Radio semantics: the checked row is already what it is asking for, and
    // re-activating would drop the connection to rebuild it.
    let state = with_other_in_range();
    let joined = &state.networks[0];
    assert!(joined.active);
    assert_eq!(op_for(&state, joined.row, ACTION_ACTIVATE), None);
}

#[test]
fn a_network_out_of_the_scan_list_is_still_activated() {
    // NM's idle scan list decays to the joined access point, so "not in range"
    // is not knowledge; `ActivateConnection` scans for the SSID itself
    // (`Snapshot::joinable`). The row is live and the click is the activation.
    let state = fixture();
    let unseen = &state.networks[1];
    assert_eq!(unseen.strength, None);
    assert_eq!(
        op_for(&state, unseen.row, ACTION_ACTIVATE),
        Some(Op::Activate {
            path: unseen.path.clone(),
            label: unseen.label.clone()
        })
    );
}

#[test]
fn no_network_is_activated_while_wifi_is_off() {
    let mut state = fixture();
    state.wireless_enabled = false;
    for network in &state.networks {
        assert_eq!(op_for(&state, network.row, ACTION_ACTIVATE), None);
    }
}

#[test]
fn each_op_names_the_call_it_would_make() {
    assert_eq!(Op::Wireless(true).call(), Call::SetWirelessEnabled(true));
    assert_eq!(Op::Wireless(false).call(), Call::SetWirelessEnabled(false));
    assert_eq!(
        Op::Activate {
            path: PATH_OTHER.to_owned(),
            label: "hotspot-example".to_owned()
        }
        .call(),
        Call::ActivateConnection(PATH_OTHER.to_owned()),
        "the device and the specific object stay \"/\": NM picks both"
    );
    assert_eq!(Op::Settings.call(), Call::Settings(SETTINGS_PANEL));
    assert_eq!(SETTINGS_PANEL, caustics_sidecar::settings::Panel::Network);
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
    assert_eq!(chosen.describe(), "gnome-control-center wifi");
    let niri = Some(OsStr::new("niri"));
    let chosen = resolve(SETTINGS_PANEL, None, niri, found(&["cosmic-settings"]))
        .expect("cosmic-settings is on PATH");
    assert_eq!(chosen.describe(), "cosmic-settings network");
    assert_eq!(resolve(SETTINGS_PANEL, None, niri, found(&[])), None);
}

#[test]
fn every_op_has_its_own_witness_name() {
    assert_eq!(Op::Wireless(true).name(), "wi-fi on");
    assert_eq!(Op::Wireless(false).name(), "wi-fi off");
    assert_eq!(Op::Settings.name(), "settings");
    assert_eq!(
        Op::Activate {
            path: PATH_OTHER.to_owned(),
            label: "hotspot example".to_owned()
        }
        .name(),
        "activate \"hotspot example\"",
        "quoted, because an SSID may have spaces in it"
    );
}

#[test]
fn the_settings_row_spawns_and_nothing_else_does() {
    let state = fixture();
    assert_eq!(
        op_for(&state, SETTINGS, ACTION_ACTIVATE),
        Some(Op::Settings)
    );
}

#[test]
fn the_rows_that_perform_nothing_perform_nothing() {
    let state = fixture();
    for row in [SEP_TOP, SEP_BOTTOM] {
        assert_eq!(
            op_for(&state, row, ACTION_ACTIVATE),
            None,
            "separator {row}"
        );
    }
    // An id this applet never wrote — including one in the derived range.
    assert_eq!(op_for(&state, 9999, ACTION_ACTIVATE), None);
    assert_eq!(
        op_for(&state, model::row_id("no such profile"), ACTION_ACTIVATE),
        None
    );
    // And every row of an empty picture, which is what "no NetworkManager"
    // publishes: nothing can be clicked because nothing is drawn.
    let nothing = Snapshot::default();
    for row in [ITEM, WIFI, SEP_TOP, SEP_BOTTOM, SETTINGS] {
        assert_eq!(op_for(&nothing, row, ACTION_MIDDLE).is_some(), row == ITEM);
    }
}

#[test]
fn only_activate_and_the_chips_middle_click_perform() {
    let state = with_other_in_range();
    let rows: Vec<u32> = [WIFI, SETTINGS]
        .into_iter()
        .chain(state.networks.iter().map(|n| n.row))
        .collect();
    for kind in [ACT_NONE, ACTION_SECONDARY, ACTION_MIDDLE, ACTION_SET_VALUE] {
        for row in &rows {
            assert_eq!(op_for(&state, *row, kind), None, "row {row} kind {kind}");
        }
    }
    // The chip takes only its middle click; a slider drag on an applet with no
    // slider means nothing.
    for kind in [ACT_NONE, ACTION_SECONDARY, ACTION_SET_VALUE] {
        assert_eq!(op_for(&state, ITEM, kind), None, "chip kind {kind}");
    }
}

// ---------------------------------------------------------------------------
// The scan, and the networks in range
// ---------------------------------------------------------------------------

/// The fixture picture with two unknown networks in range — what a walk taken
/// seconds after a `RequestScan` looks like. Derived, like everything else here
/// that NM's aged scan list could not show.
fn with_in_range() -> Snapshot {
    Snapshot {
        visible: vec![
            Visible {
                row: model::visible_row_id(b"CoffeeShop"),
                ssid: b"CoffeeShop".to_vec(),
                strength: 70,
            },
            Visible {
                row: model::visible_row_id(b"Neighbour"),
                ssid: b"Neighbour".to_vec(),
                strength: 40,
            },
        ],
        ..with_other_in_range()
    }
}

#[test]
fn the_menu_opening_is_a_scan_of_every_wifi_device_and_closing_stops_it() {
    let state = fixture();
    assert_eq!(
        state.wifi.len(),
        1,
        "the fixture machine has one Wi-Fi device: {:?}",
        state.wifi
    );

    // The row the notice carries is the chip's, but it is the *menu* that is
    // being talked about, so the row is not what decides (docs/abi.md, *Action*).
    for row in [ITEM, WIFI, SETTINGS, 9999] {
        assert_eq!(
            op_for(&state, row, ACTION_MENU_OPEN),
            Some(Op::Scan {
                devices: state.wifi.clone()
            }),
            "row {row}"
        );
        assert_eq!(
            op_for(&state, row, ACTION_MENU_CLOSE),
            Some(Op::ScanStop),
            "row {row}"
        );
    }

    assert_eq!(
        Op::Scan {
            devices: state.wifi.clone()
        }
        .call(),
        Call::RequestScan(state.wifi.clone()),
        "one RequestScan per device, and the options dictionary is empty"
    );
    assert_eq!(
        Op::ScanStop.call(),
        Call::StopScanning,
        "nothing on the bus"
    );
    assert_eq!(
        Op::Scan {
            devices: Vec::new()
        }
        .name(),
        "scan"
    );
    assert_eq!(Op::ScanStop.name(), "scan stop");

    // A machine with no radio still answers the notice, with an empty list and
    // one witness line.
    let airgapped = Snapshot {
        wifi: Vec::new(),
        ..state
    };
    assert_eq!(
        op_for(&airgapped, ITEM, ACTION_MENU_OPEN),
        Some(Op::Scan {
            devices: Vec::new()
        })
    );
}

#[test]
fn a_click_on_a_network_in_range_opens_the_settings_page() {
    // There is no password entry in the bar, ever, and joining a network NM does
    // not know is the settings program's flow. So the click is the
    // *same* op the settings row makes — not AddAndActivate, not even for an
    // open network.
    let state = with_in_range();
    for network in &state.visible {
        assert_eq!(
            op_for(&state, network.row, ACTION_ACTIVATE),
            Some(Op::Settings),
            "{}",
            network.label()
        );
    }
    assert_eq!(
        op_for(&state, state.visible[0].row, ACTION_ACTIVATE),
        op_for(&state, SETTINGS, ACTION_ACTIVATE)
    );

    // And nothing else about the menu moved: a known network still activates by
    // its object path, the joined one still does nothing.
    assert_eq!(
        op_for(&state, state.networks[1].row, ACTION_ACTIVATE),
        Some(Op::Activate {
            path: PATH_OTHER.to_owned(),
            label: "hotspot-example".to_owned()
        })
    );
    assert_eq!(op_for(&state, state.networks[0].row, ACTION_ACTIVATE), None);
    assert_eq!(
        op_for(&state, SEP_VISIBLE, ACTION_ACTIVATE),
        None,
        "its separator does nothing"
    );

    // A row in range takes nothing but a plain click.
    for kind in [ACT_NONE, ACTION_SECONDARY, ACTION_MIDDLE, ACTION_SET_VALUE] {
        assert_eq!(
            op_for(&state, state.visible[0].row, kind),
            None,
            "kind {kind}"
        );
    }

    // An in-range row survives the radio being off in the *picture* only as long
    // as NM still lists it; either way the click is still just the settings page,
    // because it never depended on the radio.
    let off = Snapshot {
        wireless_enabled: false,
        ..state
    };
    assert_eq!(
        op_for(&off, off.visible[0].row, ACTION_ACTIVATE),
        Some(Op::Settings)
    );
}

#[test]
fn the_dry_run_is_armed_by_exactly_one() {
    assert!(dry_run(Some("1")));
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("yes"),
        Some(" 1"),
    ] {
        assert!(!dry_run(value), "{value:?} armed the dry run");
    }
}

#[test]
fn a_wired_primary_changes_no_action() {
    // The chip's icon is the only thing the default route decides; the switch
    // and the known networks mean exactly what they meant before.
    let state = Snapshot {
        primary: Primary::Wired,
        ..with_other_in_range()
    };
    assert_eq!(
        op_for(&state, WIFI, ACTION_ACTIVATE),
        Some(Op::Wireless(false))
    );
    assert!(op_for(&state, state.networks[1].row, ACTION_ACTIVATE).is_some());
}
