//! What `caustics-bluetooth` publishes, built from **bus bytes** and read back
//! through the ABI's own reader — no bus, no bar.
//!
//! The two fixtures under `bluetooth/tests/data/` are raw `GetManagedObjects`
//! **reply bodies**, in `a{oa{sa{sv}}}`, in BlueZ's real layout and
//! synthesised from invented data by `synth/` (the module note says how to
//! rewrite them):
//!
//! - `managed-objects.bin`: one adapter (`hci0`, `workstation`,
//!   `Powered = true`) and one paired device, `Example Tag`, not connected and
//!   carrying **no `Icon` property at all**, which is what BlueZ does for a
//!   device whose class it does not know.
//! - `managed-objects-two-devices.bin`: the same body with that device cloned
//!   to a second address, given an alias of its own, `Icon = audio-headset`
//!   and `Connected = true`. One paired device is too thin to show that two
//!   rows keep their own ids.
//! - `managed-objects-discovering.bin`: the first body with the adapter
//!   discovering and three unpaired devices in range, one of them nameless.
//!
//! Deserialising bytes is the point: the reply's nested `a{oa{sa{sv}}}` is
//! where a hand-written reader goes wrong. Everything past the deserialiser is
//! the pure translation in `caustics_bluetooth::rows`, so these are the frames the
//! bar would have been sent, asserted row by row.

use caustics_bar_abi::buffer::{APPLET_BLUETOOTH, NO_INDEX};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, ROW_ITEM,
    ROW_MENU, ROW_SEPARATOR, Rows,
};
use caustics_bluetooth::bluez::{Device, State, parse_managed_objects};
use caustics_bluetooth::rows::{
    DEVICE_ID_BIT, ICON_DEVICE, ICON_OFF, ICON_ON, NEARBY_ID_BIT, POWER_LABEL, ROW_CHIP, ROW_POWER,
    ROW_SEP_DEVICES, ROW_SEP_NEARBY, device_row, nearby_row, picture,
};

mod synth;

/// A reply body with one paired device.
const ONE_DEVICE: &[u8] = include_bytes!("data/managed-objects.bin");
/// The same body with a second, connected device in it.
const TWO_DEVICES: &[u8] = include_bytes!("data/managed-objects-two-devices.bin");
/// The first body with the adapter discovering and devices in range.
const DISCOVERING: &[u8] = include_bytes!("data/managed-objects-discovering.bin");

const TAG_DEVICE: &str = synth::TAG_ADDRESS;
const BUDS_DEVICE: &str = synth::BUDS_ADDRESS;

/// The committed bytes are exactly what `synth/` writes: nothing in them that
/// is not in the invented data, and a fixture that drifted from its source (a
/// recording left in place, a hand edit) fails here rather than in review.
#[test]
fn the_fixtures_are_the_synthesisers_output() {
    for (name, committed) in [
        ("managed-objects.bin", ONE_DEVICE),
        ("managed-objects-two-devices.bin", TWO_DEVICES),
        ("managed-objects-discovering.bin", DISCOVERING),
    ] {
        let written = synth::fixtures()
            .into_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, body)| body)
            .expect("synth writes every fixture");
        assert!(
            written.as_slice() == committed,
            "tests/data/{name} is not synth's output; run `cargo test -p caustics-bluetooth \
             --test live synthesise_fixtures -- --ignored`"
        );
    }
}

fn state(body: &[u8]) -> State {
    parse_managed_objects(body).expect("the GetManagedObjects body deserialises")
}

/// Round-trip through `wire::write`/`wire::read`, so every assertion below is
/// about what the *bar* would decode rather than about `RowsBuilder`'s memory.
fn decoded(revision: u32, state: &State) -> Rows {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &picture(revision, state)).expect("writing to a Vec");
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

/// Ids, parents and roles: what must not move when only a value did
/// (docs/abi.md, *Rows*).
fn skeleton(rows: &Rows) -> Vec<(u32, u32, u32)> {
    rows.rows
        .iter()
        .map(|row| (row.id, row.parent, row.role))
        .collect()
}

// ---------------------------------------------------------------------------
// The deserialiser, against the fixture bytes
// ---------------------------------------------------------------------------

#[test]
fn the_first_body_is_one_powered_adapter_and_one_paired_device() {
    let state = state(ONE_DEVICE);

    let adapter = state.adapter.as_ref().expect("the body has an adapter");
    assert_eq!(adapter.path, "/org/bluez/hci0");
    assert_eq!(adapter.name, "workstation");
    assert!(adapter.powered, "the adapter is on in the fixture");
    assert!(state.powered());

    assert_eq!(
        state.devices.len(),
        1,
        "one paired device: {:?}",
        state.devices
    );
    let device = &state.devices[0];
    assert_eq!(device.address, TAG_DEVICE);
    assert_eq!(device.alias, "Example Tag");
    assert_eq!(device.path, synth::TAG_PATH);
    assert!(!device.connected);
    assert_eq!(state.connected(), 0);
    assert_eq!(
        device.icon, "",
        "BlueZ sends no Icon for this device, so the row falls back to {ICON_DEVICE}"
    );
}

#[test]
fn the_derived_body_is_two_devices_one_connected() {
    let state = state(TWO_DEVICES);

    assert!(state.powered());
    // Sorted by alias: "Example Buds" before "Example Tag", not the map's order.
    let aliases: Vec<&str> = state.devices.iter().map(|d| d.alias.as_str()).collect();
    assert_eq!(aliases, vec!["Example Buds", "Example Tag"]);
    assert_eq!(state.connected(), 1);

    let buds = &state.devices[0];
    assert_eq!(buds.address, BUDS_DEVICE);
    assert!(buds.connected);
    assert_eq!(buds.icon, "audio-headset", "BlueZ hands out themed names");
}

// ---------------------------------------------------------------------------
// The frame
// ---------------------------------------------------------------------------

#[test]
fn the_first_state_is_a_chip_a_toggle_and_one_device() {
    let rows = decoded(7, &state(ONE_DEVICE));

    assert_eq!(rows.applet, APPLET_BLUETOOTH);
    assert_eq!(rows.revision, 7);
    // The chip, the toggle, a separator, the device.
    assert_eq!(rows.rows.len(), 4);

    let chip = &rows.rows[0];
    assert_eq!(chip.id, ROW_CHIP);
    assert_eq!(chip.role, ROW_ITEM);
    assert_eq!(chip.parent, NO_INDEX);
    assert_eq!(chip.flags, RFLAG_HAS_MENU);
    assert_eq!(rows.icon(0), Some(ICON_ON), "the adapter is powered");
    assert_eq!(rows.label(0), None, "the chip is an icon");
    assert_eq!(chip.value, -1.0, "a radio has no level to draw");

    let ids: Vec<u32> = rows.rows[1..].iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![ROW_POWER, ROW_SEP_DEVICES, device_row(TAG_DEVICE),]
    );
    for i in 1..rows.rows.len() {
        let row = &rows.rows[i];
        assert_eq!(row.parent, 0, "row {} hangs off the chip", row.id);
        let expected = match row.id {
            ROW_SEP_DEVICES => ROW_SEPARATOR,
            _ => ROW_MENU,
        };
        assert_eq!(row.role, expected, "row {}", row.id);
    }

    let toggle = index_of(&rows, ROW_POWER);
    assert_eq!(rows.label(toggle).as_deref(), Some(POWER_LABEL));
    assert_eq!(rows.rows[toggle].flags, RFLAG_CHECKABLE | RFLAG_CHECKED);
    assert_eq!(rows.icon(toggle), None, "the toggle is a label and a tick");

    let device = index_of(&rows, device_row(TAG_DEVICE));
    assert_eq!(rows.label(device).as_deref(), Some("Example Tag"));
    assert_eq!(
        rows.rows[device].flags, RFLAG_CHECKABLE,
        "paired, not connected, radio on"
    );
    assert_eq!(
        rows.icon(device),
        Some(ICON_DEVICE),
        "no Icon on the bus means the fallback"
    );
    assert_eq!(device, rows.rows.len() - 1, "the last device ends the menu");
}

#[test]
fn a_connected_device_is_checked_and_wears_bluez_s_own_icon() {
    let rows = decoded(1, &state(TWO_DEVICES));
    assert_eq!(
        rows.rows.len(),
        5,
        "one more device row than the first body"
    );

    let buds = index_of(&rows, device_row(BUDS_DEVICE));
    assert_eq!(rows.label(buds).as_deref(), Some("Example Buds"));
    assert_eq!(rows.rows[buds].flags, RFLAG_CHECKABLE | RFLAG_CHECKED);
    assert_eq!(rows.icon(buds), Some("audio-headset"));

    let other = index_of(&rows, device_row(TAG_DEVICE));
    assert!(buds < other, "the device rows are in the state's order");
    assert_eq!(rows.rows[other].flags, RFLAG_CHECKABLE);
}

#[test]
fn an_adapter_that_is_off_swaps_the_icon_and_disables_the_device_rows() {
    let mut state = state(TWO_DEVICES);
    state.adapter.as_mut().expect("an adapter").powered = false;
    // A radio that is off reports its devices as disconnected; this one says
    // otherwise, so the `DISABLED` assertion below is not a coincidence of it.
    let rows = decoded(2, &state);

    assert_eq!(rows.icon(0), Some(ICON_OFF));
    let toggle = index_of(&rows, ROW_POWER);
    assert_eq!(
        rows.rows[toggle].flags, RFLAG_CHECKABLE,
        "checkable, not checked"
    );
    for device in [BUDS_DEVICE, TAG_DEVICE] {
        let i = index_of(&rows, device_row(device));
        assert_eq!(
            rows.rows[i].flags & RFLAG_DISABLED,
            RFLAG_DISABLED,
            "{device}: BlueZ cannot connect through a radio that is off"
        );
    }
}

#[test]
fn no_adapter_is_a_frame_with_no_rows() {
    // No `org.bluez`, no adapter on it, and no system bus are the same empty
    // `State` and the same frame: the chip disappears and the bar's health line
    // still says `running`.
    let rows = decoded(3, &State::default());
    assert_eq!(rows.applet, APPLET_BLUETOOTH);
    assert_eq!(rows.revision, 3);
    assert!(rows.rows.is_empty(), "{:?}", rows.rows);
}

/// No separator ever ends the menu: the devices end it, or with none the
/// toggle does.
#[test]
fn the_menu_ends_at_the_last_device_or_the_toggle() {
    let two = state(TWO_DEVICES);
    let rows = decoded(1, &two);
    let ids: Vec<u32> = rows.rows.iter().map(|row| row.id).collect();
    let mut expected = vec![ROW_CHIP, ROW_POWER, ROW_SEP_DEVICES];
    expected.extend(two.devices.iter().map(|device| device_row(&device.address)));
    assert_eq!(ids, expected, "the menu ends at the last device");
    for device in [TAG_DEVICE, BUDS_DEVICE] {
        assert!(ids.contains(&device_row(device)), "{device} is listed");
    }

    let mut none = state(ONE_DEVICE);
    none.devices.clear();
    let rows = decoded(1, &none);
    let ids: Vec<u32> = rows.rows.iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![ROW_CHIP, ROW_POWER],
        "no device is the toggle alone"
    );
}

// ---------------------------------------------------------------------------
// Stability, which is what the bar's rebuild-vs-upload rests on
// ---------------------------------------------------------------------------

#[test]
fn ids_roles_and_parents_are_stable_when_only_a_value_moved() {
    let connected = state(TWO_DEVICES);
    let mut idle = connected.clone();
    for device in &mut idle.devices {
        device.connected = false;
    }

    let a = decoded(1, &connected);
    let b = decoded(2, &idle);
    assert_eq!(skeleton(&a), skeleton(&b), "only CHECKED moved");

    // …and it did move: the same row id, a different tick.
    let i = index_of(&a, device_row(BUDS_DEVICE));
    let j = index_of(&b, device_row(BUDS_DEVICE));
    assert_eq!(i, j);
    assert_eq!(a.rows[i].flags & RFLAG_CHECKED, RFLAG_CHECKED);
    assert_eq!(b.rows[j].flags & RFLAG_CHECKED, 0);

    // A rename moves the label and nothing else — the id is the address.
    let mut renamed = connected.clone();
    renamed.devices[0].alias = "Buds Pro".to_owned();
    let c = decoded(3, &renamed);
    assert_eq!(skeleton(&a), skeleton(&c));
    assert_eq!(c.label(i).as_deref(), Some("Buds Pro"));

    // And a device going away leaves every other id where it was.
    let mut gone = connected.clone();
    gone.devices.remove(0);
    let d = decoded(4, &gone);
    assert!(d.rows.iter().all(|row| row.id != device_row(BUDS_DEVICE)));
    let kept = index_of(&d, device_row(TAG_DEVICE));
    assert_eq!(d.rows[kept].parent, 0);
}

#[test]
fn a_device_row_id_comes_from_the_address_and_never_hits_a_fixed_id() {
    // The fixed ids and the two retired ones.
    let fixed = [ROW_CHIP, ROW_POWER, ROW_SEP_DEVICES, 4, 5, ROW_SEP_NEARBY];
    for address in [
        TAG_DEVICE,
        BUDS_DEVICE,
        "",
        "00:00:00:00:00:00",
        "02:AA:BB:CC:DD:EE",
    ] {
        let id = device_row(address);
        assert_eq!(id & DEVICE_ID_BIT, DEVICE_ID_BIT, "{address:?}");
        assert!(id >= DEVICE_ID_BIT, "{address:?} gave {id}");
        assert!(!fixed.contains(&id), "{address:?} collided with a fixed id");
        let near = nearby_row(address);
        assert!(
            !fixed.contains(&near),
            "{address:?} collided with a fixed id"
        );
        assert_eq!(near & NEARBY_ID_BIT, NEARBY_ID_BIT);
        assert_eq!(
            id & NEARBY_ID_BIT,
            0,
            "a paired device's id never has the bit"
        );
        assert_ne!(
            id, near,
            "a device in range never shares a paired device's id"
        );
    }
    // The same address is the same id, a different one is a different id: what
    // makes a row survive a device connecting and a frame being rebuilt.
    assert_eq!(device_row(TAG_DEVICE), device_row(TAG_DEVICE));
    assert_ne!(device_row(TAG_DEVICE), device_row(BUDS_DEVICE));
}

#[test]
fn a_device_with_no_alias_falls_back_to_its_address() {
    // `Alias` is BlueZ's own fallback to `Name`, and `Name` is absent for a
    // device that never advertised one; the row still has to say something.
    let mut state = State {
        adapter: state(ONE_DEVICE).adapter,
        devices: vec![Device {
            path: "/org/bluez/hci0/dev_02_11_22_33_44_55".to_owned(),
            address: "02:11:22:33:44:55".to_owned(),
            alias: "02:11:22:33:44:55".to_owned(),
            ..Device::default()
        }],
        ..State::default()
    };
    let rows = decoded(1, &state);
    let i = index_of(&rows, device_row("02:11:22:33:44:55"));
    assert_eq!(rows.label(i).as_deref(), Some("02:11:22:33:44:55"));

    // A label that is not UTF-8 cannot reach here — `Alias` arrives as a D-Bus
    // string — but a lone surrogate-ish byte sequence would be replaced, not
    // rejected, on the way out (docs/abi.md, *Rows*).
    state.devices[0].alias = "Bo\u{fffd}se".to_owned();
    let rows = decoded(2, &state);
    let i = index_of(&rows, device_row("02:11:22:33:44:55"));
    assert_eq!(rows.label(i).as_deref(), Some("Bo\u{fffd}se"));
}

// ---------------------------------------------------------------------------
// Devices in range
// ---------------------------------------------------------------------------

#[test]
fn a_discovering_adapter_lists_the_named_devices_in_range_strongest_first() {
    let state = state(DISCOVERING);
    let adapter = state.adapter.as_ref().expect("an adapter");
    assert!(adapter.discovering);
    assert_eq!(state.radios, vec!["/org/bluez/hci0".to_owned()]);
    assert_eq!(
        state.devices.len(),
        1,
        "the paired tag is still the one paired"
    );

    let nearby: Vec<(&str, Option<i16>)> = state
        .nearby
        .iter()
        .map(|d| (d.alias.as_str(), d.rssi))
        .collect();
    assert_eq!(
        nearby,
        vec![
            (synth::SPEAKER_NAME, Some(-48)),
            (synth::PHONE_NAME, Some(-71)),
        ],
        "the nameless advertiser is left out though it is the loudest"
    );
    assert_eq!(state.nearby[0].icon, "audio-card");
    assert_eq!(state.nearby[0].path, synth::SPEAKER_PATH);
}

#[test]
fn an_adapter_that_is_not_discovering_lists_none() {
    // The devices a discovery found stay on the bus for a while after it
    // stopped; the list is only for while it runs, so they are not rows.
    let stopped = state(&synth::in_range_body(false));
    assert!(!stopped.adapter.as_ref().expect("an adapter").discovering);
    assert!(stopped.nearby.is_empty(), "{:?}", stopped.nearby);
    assert_eq!(stopped.devices.len(), 1, "the paired tag stays");
    let rows = decoded(1, &stopped);
    assert!(rows.rows.iter().all(|row| row.id != ROW_SEP_NEARBY));
}

#[test]
fn the_devices_in_range_follow_the_paired_ones_under_their_own_separator() {
    let state = state(DISCOVERING);
    let rows = decoded(1, &state);
    let ids: Vec<u32> = rows.rows.iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![
            ROW_CHIP,
            ROW_POWER,
            ROW_SEP_DEVICES,
            device_row(TAG_DEVICE),
            ROW_SEP_NEARBY,
            nearby_row(synth::SPEAKER_ADDRESS),
            nearby_row(synth::PHONE_ADDRESS),
        ]
    );
    let speaker = index_of(&rows, nearby_row(synth::SPEAKER_ADDRESS));
    assert_eq!(rows.label(speaker).as_deref(), Some(synth::SPEAKER_NAME));
    assert_eq!(rows.icon(speaker), Some("audio-card"));
    assert_eq!(rows.rows[speaker].flags, 0, "no tick: it is not paired");
    assert_eq!(rows.rows[speaker].role, ROW_MENU);
    let phone = index_of(&rows, nearby_row(synth::PHONE_ADDRESS));
    assert_eq!(rows.icon(phone), Some(ICON_DEVICE));

    // No paired device: the devices in range come straight after the first
    // separator, with no second one.
    let mut alone = state.clone();
    alone.devices.clear();
    let rows = decoded(2, &alone);
    let ids: Vec<u32> = rows.rows.iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![
            ROW_CHIP,
            ROW_POWER,
            ROW_SEP_DEVICES,
            nearby_row(synth::SPEAKER_ADDRESS),
            nearby_row(synth::PHONE_ADDRESS),
        ]
    );
}
