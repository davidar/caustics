//! What `caustics-network` publishes, built from bytes and read back through the
//! ABI's own reader — no bus, no bar.
//!
//! The fixtures under `network/tests/data/` are **raw D-Bus reply bodies** in
//! NetworkManager 1.56's layout, synthesised from invented data by `synth/`
//! (the module note says what is invented and how to rewrite them). They have
//! the shape of a real walk: every object, interface, property and variant
//! type a machine on Wi-Fi with a tunnel up answers with. Deserialising those
//! bytes is the point: `a{oa{sa{sv}}}` with byte-array SSIDs and object paths
//! inside variants is exactly where a hand-written reader goes wrong.
//!
//! What is in them:
//!
//! - `managed-objects.bin` — 20 064 B, the whole `GetManagedObjects` walk: 25
//!   objects, `WirelessEnabled = true`, four devices (loopback, `wlo1` Wi-Fi,
//!   `tailscale0` tun, a Wi-Fi-P2P one), **one** access point, three active
//!   connections, five stored profiles.
//! - `settings-wifi-active.bin` — 872 B, `GetSettings` for `ExampleNet`, the
//!   profile that is joined.
//! - `settings-wifi-other.bin` — 912 B, `GetSettings` for `hotspot-example`, a
//!   known profile whose access point is not in range — and a WPA-PSK one, so
//!   it is also the fixture that proves `GetSettings` omits secrets.
//!
//! **One access point is not an accident**: NM ages its scan list down to the
//! associated AP when nothing asks it to scan (twenty APs one moment, one
//! twenty minutes later). So the fixture walk is the steady state, and the
//! cases it cannot show — Wi-Fi off, a wired primary, a second AP in range —
//! are **derived** below by editing the parsed picture, which is said at every
//! use.

use caustics_bar_abi::buffer::{APPLET_NETWORK, NO_INDEX};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_RADIO,
    ROW_ITEM, ROW_MENU, ROW_SEPARATOR, Rows,
};
use caustics_network::model::{self, MAX_VISIBLE, Network, Primary, Snapshot};
use caustics_network::nm::{self, Settings, SettingsByPath};
use caustics_network::rows::{self, ITEM, SEP_BOTTOM, SEP_TOP, WIFI};

mod synth;

const MANAGED: &[u8] = include_bytes!("data/managed-objects.bin");
const WIFI_ACTIVE: &[u8] = include_bytes!("data/settings-wifi-active.bin");
const WIFI_OTHER: &[u8] = include_bytes!("data/settings-wifi-other.bin");

/// The committed bytes are exactly what `synth/` writes: nothing in them that
/// is not in the invented data, and a fixture that drifted from its source (a
/// recording left in place, a hand edit) fails here rather than in review.
#[test]
fn the_fixtures_are_the_synthesisers_output() {
    for (name, committed) in [
        ("managed-objects.bin", MANAGED),
        ("settings-wifi-active.bin", WIFI_ACTIVE),
        ("settings-wifi-other.bin", WIFI_OTHER),
    ] {
        let written = synth::fixtures()
            .into_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, body)| body)
            .expect("synth writes every fixture");
        assert!(
            written.as_slice() == committed,
            "tests/data/{name} is not synth's output; run `cargo test -p caustics-network \
             --test live synthesise_fixtures -- --ignored`"
        );
    }
}

/// The paths those two `GetSettings` bodies came from, which is the key the
/// cache is under and the path `ActivateConnection` would be handed.
const PATH_ACTIVE: &str = "/org/freedesktop/NetworkManager/Settings/1";
const PATH_OTHER: &str = "/org/freedesktop/NetworkManager/Settings/4";

// ---------------------------------------------------------------------------
// The fixture bytes to a picture
// ---------------------------------------------------------------------------

/// The fixture walk, parsed and built — the whole live path bar the two method
/// calls.
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

/// The fixture walk with extra access points put on the Wi-Fi device's own
/// list — **derived, not in the fixture**, for the reason the module note gives: the fixture models NM's idle state, whose scan list holds only the associated access point. This is
/// what a walk taken seconds after a `RequestScan` looks like.
fn with_access_points(extra: &[(&[u8], u8)]) -> Snapshot {
    let mut managed = nm::parse_managed_objects(MANAGED).expect("the fixture walk deserialises");
    let wifi = managed
        .devices
        .iter()
        .position(|d| d.kind == nm::DEVICE_WIFI)
        .expect("the walk has a Wi-Fi device");
    for (i, (ssid, strength)) in extra.iter().enumerate() {
        let path = format!("/org/freedesktop/NetworkManager/AccessPoint/9{i:03}");
        managed.devices[wifi].access_points.push(path.clone());
        managed.access_points.push(nm::AccessPoint {
            path,
            ssid: (*ssid).to_vec(),
            strength: *strength,
            ..Default::default()
        });
    }
    let mut cache = SettingsByPath::new();
    for (path, body) in [(PATH_ACTIVE, WIFI_ACTIVE), (PATH_OTHER, WIFI_OTHER)] {
        cache.insert(
            path.to_owned(),
            nm::parse_settings(body).expect("the reply deserialises"),
        );
    }
    model::build(&managed, &cache)
}

/// The SSID of the profile that was joined in the fixture, as
/// bytes. An access point broadcasting this is a *known* network and must never
/// appear in the in-range section.
fn joined_ssid() -> Vec<u8> {
    nm::parse_settings(WIFI_ACTIVE)
        .expect("the reply deserialises")
        .ssid
        .expect("the active Wi-Fi profile has an SSID")
}

/// Round-trip through `wire::write`/`wire::read`, so every assertion below is
/// about what the *bar* would decode rather than about `RowsBuilder`'s memory.
fn decoded(revision: u32, state: &Snapshot) -> Rows {
    let frame = Frame::Rows(rows::picture(revision, state));
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &frame).expect("writing to a Vec");
    match wire::read(&mut bytes.as_slice()) {
        Ok(Some(Frame::Rows(rows))) => rows,
        other => panic!("expected a rows frame, got {other:?}"),
    }
}

fn index_of(rows: &Rows, id: u32) -> usize {
    rows.rows
        .iter()
        .position(|row| row.id == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

#[test]
fn the_fixture_walk_is_one_chip_a_switch_and_two_networks() {
    let state = fixture();
    assert!(state.wireless_enabled);
    assert!(state.wifi_device());
    // 79 at recording time: the `good` bucket, one point under `excellent`.
    assert_eq!(state.primary, Primary::WiFi { strength: 79 });
    assert_eq!(
        state
            .networks
            .iter()
            .map(|n| n.label.as_str())
            .collect::<Vec<_>>(),
        vec!["ExampleNet", "hotspot-example"],
        "the joined one first, then what is out of range"
    );

    let rows = decoded(7, &state);
    assert_eq!(rows.applet, APPLET_NETWORK);
    assert_eq!(rows.revision, 7);

    // The chip, the switch, a separator and two networks: nothing in range
    // follows them, so no separator ends the menu.
    let ids: Vec<u32> = rows.rows.iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![
            ITEM,
            WIFI,
            SEP_TOP,
            state.networks[0].row,
            state.networks[1].row,
        ]
    );

    let chip = &rows.rows[0];
    assert_eq!(chip.role, ROW_ITEM);
    assert_eq!(chip.parent, NO_INDEX);
    assert_eq!(chip.flags, RFLAG_HAS_MENU);
    assert_eq!(chip.value, -1.0, "the chip draws no level");
    assert_eq!(rows.icon(0), Some(model::GOOD));
    assert_eq!(rows.label(0), None, "the chip is an icon and nothing else");

    // Every other row hangs off the chip, and nothing is a bar item twice.
    for i in 1..rows.rows.len() {
        assert_eq!(
            rows.rows[i].parent, 0,
            "row {} hangs off the chip",
            rows.rows[i].id
        );
    }
    assert_eq!(
        rows.rows.iter().filter(|row| row.role == ROW_ITEM).count(),
        1
    );

    let i = index_of(&rows, WIFI);
    assert_eq!(rows.rows[i].role, ROW_MENU);
    assert_eq!(rows.label(i).as_deref(), Some(rows::WIFI_LABEL));
    assert_eq!(rows.rows[i].flags, RFLAG_CHECKABLE | RFLAG_CHECKED);
    assert_eq!(rows.icon(i), None, "the switch is a label and a check");

    let i = index_of(&rows, state.networks[0].row);
    assert_eq!(rows.rows[i].role, ROW_MENU);
    assert_eq!(rows.label(i).as_deref(), Some("ExampleNet"));
    assert_eq!(
        rows.rows[i].flags,
        RFLAG_RADIO | RFLAG_CHECKABLE | RFLAG_CHECKED
    );
    assert_eq!(rows.icon(i), Some(model::GOOD));

    // The one out of NM's aged scan list: the no-signal icon, and still a
    // row that can be clicked — activation scans for it (`Snapshot::joinable`).
    let i = index_of(&rows, state.networks[1].row);
    assert_eq!(rows.label(i).as_deref(), Some("hotspot-example"));
    assert_eq!(rows.rows[i].flags, RFLAG_RADIO | RFLAG_CHECKABLE);
    assert_eq!(rows.icon(i), Some(model::NONE));

    let i = index_of(&rows, SEP_TOP);
    assert_eq!(rows.rows[i].role, ROW_SEPARATOR);
    assert_eq!(rows.label(i), None);
    assert_eq!(rows.icon(i), None);
}

/// With networks in range, the last of them ends the menu: no separator
/// follows it.
#[test]
fn the_last_network_in_range_ends_the_menu() {
    let ranged = with_access_points(&[(b"Example Cafe", 70)]);
    let rows = decoded(1, &ranged);
    let last = rows.rows.last().expect("rows");
    assert_eq!(Some(last.id), ranged.visible.last().map(|v| v.row));
    assert_ne!(last.role, ROW_SEPARATOR);
}

#[test]
fn only_the_wifi_profiles_become_rows() {
    // The fixture walk lists five stored profiles; three of them are a
    // loopback, a tun and an ethernet one, and none of those is a Wi-Fi network
    // to join. Putting them in the cache must change nothing.
    let managed = nm::parse_managed_objects(MANAGED).expect("the fixture walk deserialises");
    assert_eq!(managed.connections.len(), 5);

    let mut cache = SettingsByPath::new();
    for (path, body) in [(PATH_ACTIVE, WIFI_ACTIVE), (PATH_OTHER, WIFI_OTHER)] {
        cache.insert(
            path.to_owned(),
            nm::parse_settings(body).expect("the reply deserialises"),
        );
    }
    // Derived, not in the fixture: the three non-Wi-Fi profiles as the walk names
    // them, with the types NM gave them.
    for (path, id, kind) in [
        (
            "/org/freedesktop/NetworkManager/Settings/2",
            "lo",
            "loopback",
        ),
        (
            "/org/freedesktop/NetworkManager/Settings/3",
            "tailscale0",
            "tun",
        ),
        (
            "/org/freedesktop/NetworkManager/Settings/5",
            "usb-tether",
            nm::TYPE_ETHERNET,
        ),
    ] {
        cache.insert(
            path.to_owned(),
            Settings {
                id: id.to_owned(),
                uuid: format!("uuid-of-{id}"),
                kind: kind.to_owned(),
                ssid: None,
            },
        );
    }
    let state = model::build(&managed, &cache);
    assert_eq!(state.networks.len(), 2, "{:?}", state.networks);
}

#[test]
fn the_fixture_settings_carry_no_secret() {
    // `GetSettings` omits secrets by NM's contract, and `settings-wifi-other`
    // is a WPA-PSK profile — so this is the fixture that would have caught it.
    // `wpa-psk` appears in the body as a *value* of `key-mgmt`, which is why
    // the check is on keys and a byte search would be wrong.
    for (name, body) in [
        ("settings-wifi-active", WIFI_ACTIVE),
        ("settings-wifi-other", WIFI_OTHER),
    ] {
        let keys = nm::secret_keys(body).expect("the reply deserialises");
        assert!(keys.is_empty(), "{name}.bin carries {keys:?}");
    }
    assert!(
        WIFI_OTHER.windows(7).any(|w| w == b"wpa-psk"),
        "the fixture really is a PSK profile, so the check above means something"
    );
}

// ---------------------------------------------------------------------------
// The five icon states
// ---------------------------------------------------------------------------

#[test]
fn the_chip_has_one_icon_for_each_state_the_machine_can_be_in() {
    let fixture = fixture();
    // The fixture: a Wi-Fi primary at 79, which is the `good` bucket.
    assert_eq!(fixture.icon(), model::GOOD);
    assert_eq!(decoded(1, &fixture).icon(0), Some(model::GOOD));

    // Derived: every bucket edge, by moving the one number.
    for (strength, icon) in [
        (100, model::EXCELLENT),
        (80, model::EXCELLENT),
        (79, model::GOOD),
        (55, model::GOOD),
        (54, model::OK),
        (30, model::OK),
        (29, model::WEAK),
        (5, model::WEAK),
        (4, model::NONE),
        (0, model::NONE),
    ] {
        let state = Snapshot {
            primary: Primary::WiFi { strength },
            ..fixture.clone()
        };
        assert_eq!(state.icon(), icon, "strength {strength}");
    }

    // Derived: a wired primary — the USB-tether profile activated.
    let wired = Snapshot {
        primary: Primary::Wired,
        ..fixture.clone()
    };
    assert_eq!(wired.icon(), model::WIRED);
    assert_eq!(decoded(1, &wired).icon(0), Some(model::WIRED));

    // Derived: the radio on, nothing joined.
    let idle = Snapshot {
        primary: Primary::None,
        ..fixture.clone()
    };
    assert_eq!(idle.icon(), model::WIRELESS_OFFLINE);

    // Derived: the radio off. Every network goes out of range with it, which
    // is NM's own behaviour and the reason both fields move together.
    let off = Snapshot {
        wireless_enabled: false,
        primary: Primary::None,
        networks: fixture
            .networks
            .iter()
            .map(|n| Network {
                strength: None,
                active: false,
                ..n.clone()
            })
            .collect(),
        ..fixture.clone()
    };
    assert_eq!(off.icon(), model::WIRELESS_DISABLED);
    let rows = decoded(1, &off);
    assert_eq!(rows.icon(0), Some(model::WIRELESS_DISABLED));
    let i = index_of(&rows, WIFI);
    assert_eq!(
        rows.rows[i].flags, RFLAG_CHECKABLE,
        "the switch is unchecked, not gone"
    );
    for network in &off.networks {
        let i = index_of(&rows, network.row);
        assert_eq!(rows.rows[i].flags & RFLAG_DISABLED, RFLAG_DISABLED);
        assert_eq!(rows.rows[i].flags & RFLAG_CHECKED, 0);
    }

    // Derived: a machine with no Wi-Fi device at all and no default route —
    // a desktop with one unplugged ethernet port. Not "radio off": there is no
    // radio to have switched off.
    let wireless_nowhere = Snapshot {
        wifi: Vec::new(),
        wireless_enabled: false,
        primary: Primary::None,
        networks: Vec::new(),
        visible: Vec::new(),
    };
    assert_eq!(wireless_nowhere.icon(), model::OFFLINE);
    let rows = decoded(1, &wireless_nowhere);
    assert_eq!(rows.icon(0), Some(model::OFFLINE));
    // No networks, so no separator at all: the chip and the switch.
    assert_eq!(
        rows.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![ITEM, WIFI]
    );
}

// ---------------------------------------------------------------------------
// Stability — the property the bar's rebuild-vs-upload distinction needs
// ---------------------------------------------------------------------------

fn shape(rows: &Rows) -> Vec<(u32, u32, u32, u32)> {
    rows.rows
        .iter()
        .map(|r| (r.id, r.parent, r.role, r.flags))
        .collect()
}

#[test]
fn ids_roles_and_parents_hold_still_when_only_a_strength_moved() {
    let state = fixture();
    let a = decoded(1, &state);

    // Derived: the same picture with the associated AP two points weaker —
    // still the `good` bucket, so this is the case the bar must *upload* and
    // never rebuild for. In fact it is the case the sidecar never publishes at
    // all, which the next assertion says.
    let drifted = Snapshot {
        primary: Primary::WiFi { strength: 77 },
        ..state.clone()
    };
    let b = decoded(2, &drifted);
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(
        a.bytes, b.bytes,
        "the bucket did not move, so no label or icon did either"
    );

    // Derived: across a bucket edge. The ids, roles and parents still hold; the
    // icons are what changed, which is a rebuild and is correct.
    let weaker = Snapshot {
        primary: Primary::WiFi { strength: 40 },
        networks: state
            .networks
            .iter()
            .map(|n| Network {
                strength: n.strength.map(|_| 40),
                ..n.clone()
            })
            .collect(),
        ..state.clone()
    };
    let c = decoded(3, &weaker);
    assert_eq!(shape(&a), shape(&c), "only the icons moved");
    assert_ne!(a.bytes, c.bytes);
    assert_eq!(c.icon(0), Some(model::OK));

    // Derived: the known profile comes into range. Its id is unchanged — which
    // is the whole reason the id is the UUID's hash and not an index — and only
    // its flags and icon move.
    let in_range = Snapshot {
        networks: state
            .networks
            .iter()
            .map(|n| Network {
                strength: Some(n.strength.unwrap_or(62)),
                ..n.clone()
            })
            .collect(),
        ..state.clone()
    };
    let d = decoded(4, &in_range);
    assert_eq!(
        a.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        d.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        "every id survived"
    );
    let i = index_of(&d, state.networks[1].row);
    assert_eq!(
        d.rows[i].flags,
        RFLAG_RADIO | RFLAG_CHECKABLE,
        "clickable before and after"
    );
    assert_eq!(d.icon(i), Some(model::GOOD));
}

#[test]
fn a_row_id_is_the_uuid_and_never_an_index() {
    let state = fixture();
    for network in &state.networks {
        assert_eq!(network.row, model::row_id(&network.uuid));
        assert_eq!(
            network.row & model::DERIVED,
            model::DERIVED,
            "every derived id carries the bit that keeps it off the fixed ids 1..=6"
        );
        // The fixed ids and the two retired ones.
        for fixed in 1..=6 {
            assert_ne!(network.row, fixed);
        }
    }
    // The order the profiles happen to be listed in is not in the id.
    assert_eq!(
        model::row_id("5e7e0000-0000-4000-8000-000000000002"),
        state.networks[0].row
    );
    assert_ne!(model::row_id("a"), model::row_id("b"));
}

#[test]
fn two_profiles_that_would_share_a_row_id_make_one_row() {
    // The wire says an id is unique in a frame. NM will not hand out the same
    // UUID twice, and two FNV-1a hashes colliding over ten profiles is a
    // one-in-a-hundred-million event — but the frame has to stay well formed
    // either way, so the second is dropped rather than sent.
    let managed = nm::parse_managed_objects(MANAGED).expect("the fixture walk deserialises");
    let one = nm::parse_settings(WIFI_ACTIVE).expect("the reply deserialises");
    let mut cache = SettingsByPath::new();
    cache.insert(PATH_ACTIVE.to_owned(), one.clone());
    cache.insert(
        PATH_OTHER.to_owned(),
        Settings {
            id: "a twin".to_owned(),
            ..one
        },
    );
    let state = model::build(&managed, &cache);
    assert_eq!(state.networks.len(), 1);
}

// ---------------------------------------------------------------------------
// SSIDs are bytes
// ---------------------------------------------------------------------------

#[test]
fn an_ssid_is_matched_as_bytes_and_never_as_text() {
    // The label is the profile's `connection.id`, which D-Bus guarantees is
    // text; the SSID is an `ay` and is not guaranteed anything. So the match
    // between a profile and a visible access point is on raw bytes — this
    // builds a walk whose one access point has an SSID that is not UTF-8 and a
    // profile that stores the same bytes.
    let mut managed = nm::parse_managed_objects(MANAGED).expect("the fixture walk deserialises");
    let ragged = vec![0x54, 0x50, 0xff, 0xfe, 0x4f];
    let wifi = managed
        .devices
        .iter()
        .position(|d| d.kind == nm::DEVICE_WIFI)
        .expect("the walk has a Wi-Fi device");
    let ap = managed.devices[wifi].access_points[0].clone();
    managed.access_points = vec![nm::AccessPoint {
        path: ap,
        ssid: ragged.clone(),
        strength: 91,
        ..Default::default()
    }];

    let mut cache = SettingsByPath::new();
    let mut settings = nm::parse_settings(WIFI_ACTIVE).expect("the reply deserialises");
    settings.ssid = Some(ragged);
    cache.insert(PATH_ACTIVE.to_owned(), settings);

    let state = model::build(&managed, &cache);
    assert_eq!(state.networks.len(), 1);
    assert_eq!(state.networks[0].strength, Some(91), "the bytes matched");
    let rows = decoded(1, &state);
    let i = index_of(&rows, state.networks[0].row);
    assert_eq!(rows.icon(i), Some(model::EXCELLENT));
    assert_eq!(
        rows.label(i).as_deref(),
        Some("ExampleNet"),
        "the label is still the id"
    );
}

// ---------------------------------------------------------------------------
// Absence
// ---------------------------------------------------------------------------

#[test]
fn no_networkmanager_is_a_frame_with_no_rows() {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &Frame::Rows(rows::empty(1))).expect("writing to a Vec");
    let rows = match wire::read(&mut bytes.as_slice()) {
        Ok(Some(Frame::Rows(rows))) => rows,
        other => panic!("expected a rows frame, got {other:?}"),
    };
    // The chip disappears; the applet is still named, so the bar's health line
    // still says this sidecar is running.
    assert_eq!(rows.applet, APPLET_NETWORK);
    assert_eq!(rows.revision, 1);
    assert!(rows.rows.is_empty());
    assert!(rows.bytes.is_empty());
}

#[test]
fn a_body_that_is_not_a_walk_is_an_error_and_not_a_panic() {
    assert!(nm::parse_managed_objects(b"").is_err());
    assert!(nm::parse_managed_objects(&MANAGED[..64]).is_err());
    assert!(
        nm::parse_settings(MANAGED).is_err(),
        "a walk is not a settings reply"
    );
    assert!(nm::secret_keys(b"\x01\x02\x03").is_err());
}

// ---------------------------------------------------------------------------
// The networks in range
// ---------------------------------------------------------------------------

#[test]
fn the_networks_in_range_are_a_section_of_their_own_below_the_known_ones() {
    // Five access points the device can see: two unknown names, one of those
    // twice at two strengths, a hidden one, and the profile that is joined.
    let state = with_access_points(&[
        (b"CoffeeShop", 70),
        (b"Neighbour", 40),
        (b"Neighbour", 55),
        (b"", 90),
        (&joined_ssid(), 66),
    ]);

    assert_eq!(
        state
            .visible
            .iter()
            .map(|v| (v.label(), v.strength))
            .collect::<Vec<_>>(),
        vec![("CoffeeShop".to_owned(), 70), ("Neighbour".to_owned(), 55)],
        "deduplicated by SSID keeping the strongest, hidden skipped, the known \
         one skipped, strongest first"
    );

    // The whole menu, in order: the switch, the known networks and their
    // separator, then what is in range, last.
    let rows = decoded(11, &state);
    assert_eq!(
        rows.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![
            ITEM,
            WIFI,
            SEP_TOP,
            state.networks[0].row,
            state.networks[1].row,
            SEP_BOTTOM,
            state.visible[0].row,
            state.visible[1].row,
        ]
    );

    // **No mark.** That is the whole difference between one of these and a known
    // network: no radio, no check, because none of them is a thing this applet
    // can join.
    for network in &state.visible {
        let i = index_of(&rows, network.row);
        assert_eq!(rows.rows[i].role, ROW_MENU);
        assert_eq!(
            rows.rows[i].parent, 0,
            "it hangs off the chip like every other row"
        );
        assert_eq!(rows.rows[i].flags, 0, "{} is marked", network.label());
        assert_eq!(rows.label(i).as_deref(), Some(network.label().as_str()));
        assert_eq!(rows.icon(i), Some(model::bucket(Some(network.strength))));
    }
    // The strength buckets, which is what the icon says: 70 is good, 55 is good.
    assert_eq!(
        rows.icon(index_of(&rows, state.visible[0].row)),
        Some(model::GOOD)
    );

    let i = index_of(&rows, SEP_BOTTOM);
    assert_eq!(rows.rows[i].role, ROW_SEPARATOR);
}

#[test]
fn nothing_unknown_in_range_is_no_section_and_no_separator() {
    // The steady state: the only access point in range is the joined
    // one, so the section and the separator above it are absent and the
    // frame is the menu of known networks alone.
    let quiet = fixture();
    assert!(quiet.visible.is_empty(), "{:?}", quiet.visible);
    let rows = decoded(3, &quiet);
    assert!(!rows.rows.iter().any(|row| row.id == SEP_BOTTOM));
    assert_eq!(
        rows.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![
            ITEM,
            WIFI,
            SEP_TOP,
            quiet.networks[0].row,
            quiet.networks[1].row,
        ]
    );
}

#[test]
fn an_access_point_no_device_lists_is_not_in_range() {
    // The same rule a known network's strength already follows: an AccessPoint
    // object that the Wi-Fi device does not list is one NM is about to forget,
    // and a menu built on it would offer a network that is not there.
    let mut managed = nm::parse_managed_objects(MANAGED).expect("the fixture walk deserialises");
    let wifi = managed
        .devices
        .iter()
        .position(|d| d.kind == nm::DEVICE_WIFI)
        .expect("the walk has a Wi-Fi device");
    managed.devices[wifi].access_points.clear();
    managed.access_points.push(nm::AccessPoint {
        path: "/org/freedesktop/NetworkManager/AccessPoint/8000".to_owned(),
        ssid: b"CoffeeShop".to_vec(),
        strength: 70,
        ..Default::default()
    });

    let mut cache = SettingsByPath::new();
    for (path, body) in [(PATH_ACTIVE, WIFI_ACTIVE), (PATH_OTHER, WIFI_OTHER)] {
        cache.insert(
            path.to_owned(),
            nm::parse_settings(body).expect("the reply deserialises"),
        );
    }
    let state = model::build(&managed, &cache);
    assert!(
        state.wifi_device(),
        "the device is still there; its list is empty"
    );
    assert!(state.visible.is_empty(), "{:?}", state.visible);
    assert!(
        state.networks.iter().all(|n| n.strength.is_none()),
        "and so is every strength"
    );
}

#[test]
fn the_section_is_capped_at_ten_and_they_are_the_ten_strongest() {
    // Twenty access points is an ordinary reading in a block of flats, so the
    // cap is reached in everyday use.
    let names: Vec<String> = (0..14).map(|i| format!("ap-{i:02}")).collect();
    let extra: Vec<(&[u8], u8)> = names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_bytes(), (20 + i * 5) as u8))
        .collect();
    let state = with_access_points(&extra);

    assert_eq!(state.visible.len(), MAX_VISIBLE);
    assert_eq!(
        state.visible.iter().map(|v| v.label()).collect::<Vec<_>>(),
        (4..14)
            .rev()
            .map(|i| format!("ap-{i:02}"))
            .collect::<Vec<_>>(),
        "the ten strongest, strongest first"
    );
    // The weakest four are simply not rows, and the frame is still well formed:
    // the ten unmarked rows are the ten in range.
    let rows = decoded(4, &state);
    assert_eq!(
        rows.rows
            .iter()
            .filter(|row| row.flags == 0 && row.role == ROW_MENU)
            .count(),
        MAX_VISIBLE
    );
}

#[test]
fn an_in_range_row_id_is_the_ssid_and_holds_still_while_the_strength_moves() {
    let weak = with_access_points(&[(b"CoffeeShop", 22)]);
    let strong = with_access_points(&[(b"CoffeeShop", 88)]);
    assert_eq!(weak.visible.len(), 1);
    assert_eq!(
        weak.visible[0].row, strong.visible[0].row,
        "the same network, the same row"
    );
    assert_eq!(weak.visible[0].row, model::visible_row_id(b"CoffeeShop"));

    // Only the icon moved, which is what the bar rebuilds for and nothing else.
    let (a, b) = (decoded(1, &weak), decoded(2, &strong));
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(a.icon(index_of(&a, weak.visible[0].row)), Some(model::WEAK));
    assert_eq!(
        b.icon(index_of(&b, weak.visible[0].row)),
        Some(model::EXCELLENT)
    );

    // The two derived id spaces are apart, and neither can be a fixed id.
    for network in &weak.networks {
        assert_eq!(
            network.row & model::VISIBLE,
            0,
            "a known network is not in the visible half"
        );
    }
    assert_eq!(weak.visible[0].row & model::VISIBLE, model::VISIBLE);
    for fixed in 1..=6 {
        assert_ne!(weak.visible[0].row, fixed);
    }

    // An SSID that is not text is still a row, because the id is over the bytes.
    let ragged = with_access_points(&[(&[0x54, 0x50, 0xff, 0xfe, 0x4f], 61)]);
    assert_eq!(ragged.visible.len(), 1);
    assert_eq!(
        ragged.visible[0].row,
        model::visible_row_id(&[0x54, 0x50, 0xff, 0xfe, 0x4f])
    );
    assert!(
        ragged.visible[0].label().contains('\u{fffd}'),
        "{}",
        ragged.visible[0].label()
    );
}

/// The walk reads an access point's security flags from NM's own layout, and
/// a network in range carries what a join needs: its access point, its device
/// and its security.
#[test]
fn a_network_in_range_carries_what_a_join_names() {
    let mut managed = nm::parse_managed_objects(MANAGED).expect("the fixture walk deserialises");
    let ap = &managed.access_points[0];
    assert_eq!((ap.flags, ap.wpa_flags, ap.rsn_flags), (1, 0, 1160));
    assert_eq!(
        model::security(ap.flags, ap.wpa_flags, ap.rsn_flags),
        model::Security::Sae
    );

    let wifi = managed
        .devices
        .iter()
        .position(|d| d.kind == nm::DEVICE_WIFI)
        .expect("the walk has a Wi-Fi device");
    let device = managed.devices[wifi].path.clone();
    for (n, strength, rsn) in [(1u32, 40u8, 0x188u32), (2, 75, 0x188)] {
        let path = format!("/org/freedesktop/NetworkManager/AccessPoint/95{n}");
        managed.devices[wifi].access_points.push(path.clone());
        managed.access_points.push(nm::AccessPoint {
            path,
            ssid: b"Mesh".to_vec(),
            strength,
            flags: nm::AP_PRIVACY,
            rsn_flags: rsn,
            ..Default::default()
        });
    }
    let state = model::build(&managed, &SettingsByPath::new());
    let mesh = state
        .visible
        .iter()
        .find(|v| v.ssid == b"Mesh")
        .expect("the mesh is in range");
    // One row for the two, naming the stronger.
    assert_eq!(mesh.strength, 75);
    assert_eq!(mesh.ap, "/org/freedesktop/NetworkManager/AccessPoint/952");
    assert_eq!(mesh.device, device);
    assert_eq!(mesh.security, model::Security::Psk);
}
