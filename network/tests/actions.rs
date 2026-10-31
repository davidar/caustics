//! The dispatch: each row id, with each action kind, to the call it would
//! make — asserted against the *description* of the call, so no test ever
//! joins a network, leaves one or switches off a radio.
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
use caustics_network::actions::{Call, Join, Op, dry_run, op_for, profile};
use caustics_network::model::{self, Network, Primary, Security, Snapshot, Visible};
use caustics_network::nm::{self, SettingsByPath};
use caustics_network::rows::{ITEM, SEP_BOTTOM, SEP_TOP, WIFI};
use zbus::zvariant::Value;

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
}

#[test]
fn every_op_has_its_own_witness_name() {
    assert_eq!(Op::Wireless(true).name(), "wi-fi on");
    assert_eq!(Op::Wireless(false).name(), "wi-fi off");
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
fn the_rows_that_perform_nothing_perform_nothing() {
    let state = fixture();
    for row in [SEP_TOP, SEP_BOTTOM] {
        assert_eq!(
            op_for(&state, row, ACTION_ACTIVATE),
            None,
            "separator {row}"
        );
    }
    // An id this applet never wrote — including one in the derived range,
    // and the two retired ones of the settings row and its separator.
    assert_eq!(op_for(&state, 9999, ACTION_ACTIVATE), None);
    for retired in [5, 6] {
        assert_eq!(op_for(&state, retired, ACTION_ACTIVATE), None);
    }
    assert_eq!(
        op_for(&state, model::row_id("no such profile"), ACTION_ACTIVATE),
        None
    );
    // And every row of an empty picture, which is what "no NetworkManager"
    // publishes: nothing can be clicked because nothing is drawn.
    let nothing = Snapshot::default();
    for row in [ITEM, WIFI, SEP_TOP, SEP_BOTTOM] {
        assert_eq!(op_for(&nothing, row, ACTION_MIDDLE).is_some(), row == ITEM);
    }
}

#[test]
fn only_activate_and_the_chips_middle_click_perform() {
    let state = with_other_in_range();
    let rows: Vec<u32> = [WIFI]
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
    let state = with_other_in_range();
    let device = state.wifi[0].clone();
    let visible = |ssid: &[u8], strength, n, security| Visible {
        row: model::visible_row_id(ssid),
        ssid: ssid.to_vec(),
        strength,
        ap: format!("/org/freedesktop/NetworkManager/AccessPoint/{n}"),
        device: device.clone(),
        security,
    };
    Snapshot {
        visible: vec![
            visible(b"CoffeeShop", 70, 801, Security::Psk),
            visible(b"Neighbour", 40, 802, Security::Sae),
            visible(b"Library", 35, 803, Security::Open),
            visible(b"Corp", 30, 804, Security::Enterprise),
        ],
        ..state
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
    for row in [ITEM, WIFI, 9999] {
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
fn a_click_on_a_network_in_range_joins_it() {
    // A network NM does not know is joined: a new profile with no secret in
    // it, on the device and access point that see it, and NM asks its secret
    // agent for the passphrase. The bar never holds it.
    let state = with_in_range();
    let device = state.wifi[0].clone();
    let coffee = &state.visible[0];
    let op = op_for(&state, coffee.row, ACTION_ACTIVATE);
    let join = Join {
        ssid: b"CoffeeShop".to_vec(),
        label: "CoffeeShop".to_owned(),
        device: device.clone(),
        ap: "/org/freedesktop/NetworkManager/AccessPoint/801".to_owned(),
        security: Security::Psk,
    };
    assert_eq!(op, Some(Op::Join(join.clone())));
    let op = op.unwrap();
    assert_eq!(op.call(), Call::AddAndActivate(join));
    assert_eq!(op.name(), "join \"CoffeeShop\"");
    for (i, security) in [(1, Security::Sae), (2, Security::Open)] {
        match op_for(&state, state.visible[i].row, ACTION_ACTIVATE) {
            Some(Op::Join(join)) => assert_eq!(join.security, security),
            other => panic!("{}: {other:?}", state.visible[i].label()),
        }
    }

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
        op_for(&state, SEP_BOTTOM, ACTION_ACTIVATE),
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

    // With the radio off there is nothing to join on.
    let off = Snapshot {
        wireless_enabled: false,
        ..state
    };
    assert_eq!(op_for(&off, off.visible[0].row, ACTION_ACTIVATE), None);
}

#[test]
fn an_enterprise_network_in_range_says_it_needs_a_profile() {
    let state = with_in_range();
    let corp = &state.visible[3];
    let op = op_for(&state, corp.row, ACTION_ACTIVATE).expect("an op");
    assert_eq!(
        op,
        Op::Unjoinable {
            label: "Corp".to_owned(),
            needs: "802.1X"
        }
    );
    assert_eq!(op.call(), Call::Decline("802.1X"));
    assert_eq!(op.name(), "join \"Corp\"");
}

fn key_mgmt(
    profile: &std::collections::HashMap<&str, std::collections::HashMap<&str, Value<'_>>>,
) -> Option<String> {
    match profile.get("802-11-wireless-security")?.get("key-mgmt")? {
        Value::Str(s) => Some(s.as_str().to_owned()),
        other => panic!("key-mgmt is {other:?}"),
    }
}

#[test]
fn the_new_profile_names_the_network_and_carries_no_secret() {
    let join = Join {
        ssid: b"CoffeeShop".to_vec(),
        label: "CoffeeShop".to_owned(),
        device: "/org/freedesktop/NetworkManager/Devices/3".to_owned(),
        ap: "/org/freedesktop/NetworkManager/AccessPoint/801".to_owned(),
        security: Security::Psk,
    };
    let psk = profile(&join);
    assert_eq!(psk["connection"]["id"], Value::from("CoffeeShop"));
    assert_eq!(psk["connection"]["type"], Value::from("802-11-wireless"));
    assert_eq!(
        psk["802-11-wireless"]["ssid"],
        Value::from(b"CoffeeShop".to_vec())
    );
    assert_eq!(
        psk["802-11-wireless"]["mode"],
        Value::from("infrastructure")
    );
    assert_eq!(key_mgmt(&psk).as_deref(), Some("wpa-psk"));
    // No secret anywhere in it: NM asks the agent.
    assert_eq!(psk["802-11-wireless-security"].len(), 1);
    assert_eq!(psk.len(), 3);

    let sae = profile(&Join {
        security: Security::Sae,
        ..join.clone()
    });
    assert_eq!(key_mgmt(&sae).as_deref(), Some("sae"));
    let owe = profile(&Join {
        security: Security::Owe,
        ..join.clone()
    });
    assert_eq!(key_mgmt(&owe).as_deref(), Some("owe"));
    let open = profile(&Join {
        security: Security::Open,
        ..join
    });
    assert_eq!(key_mgmt(&open), None);
    assert_eq!(open.len(), 2, "an open network has no security setting");
}

#[test]
fn an_access_points_flags_say_what_it_asks_for() {
    use caustics_network::model::security;
    const PRIVACY: u32 = 0x1;
    const CCMP: u32 = 0x8 | 0x80;
    assert_eq!(security(0, 0, 0), Security::Open);
    assert_eq!(security(PRIVACY, 0, 0), Security::Wep);
    assert_eq!(security(PRIVACY, 0, CCMP | 0x100), Security::Psk);
    assert_eq!(security(PRIVACY, 0x4 | 0x40 | 0x100, 0), Security::Psk);
    // WPA2/WPA3 transition takes the passphrase every client has.
    assert_eq!(security(PRIVACY, 0, CCMP | 0x100 | 0x400), Security::Psk);
    // The fixture walk's access point: SAE alone.
    assert_eq!(security(PRIVACY, 0, 1160), Security::Sae);
    assert_eq!(security(0, 0, CCMP | 0x800), Security::Owe);
    assert_eq!(security(PRIVACY, 0, CCMP | 0x200), Security::Enterprise);
    assert_eq!(
        security(PRIVACY, 0, CCMP | 0x200 | 0x100),
        Security::Enterprise
    );
    assert_eq!(security(PRIVACY, 0, 0x2000), Security::Enterprise);
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
