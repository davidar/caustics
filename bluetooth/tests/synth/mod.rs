//! The fixtures under `bluetooth/tests/data/`, written from invented data.
//!
//! Each body is a BlueZ 5 `GetManagedObjects` reply in its real layout: the
//! same objects, interfaces, property names and variant types a machine's
//! `hci0` with paired devices answers with, serialised through zvariant
//! exactly as zbus hands a reply body to the parser. Only the values are made
//! up: the adapter is named after an invented host, every address is locally
//! administered (`02:…`), and the device names are plainly examples.
//!
//! - `managed-objects.bin`: one powered adapter and one paired Bluetooth LE
//!   device, not connected, that carries **no `Icon` property at all**, which
//!   is what BlueZ does for a device whose class it does not know;
//! - `managed-objects-two-devices.bin`: the same, plus a second device (the
//!   first one's properties at another address, with an alias of its own,
//!   `Icon = audio-headset` and `Connected = true`), so two rows can be seen to
//!   keep their own ids;
//! - `managed-objects-discovering.bin`: the first body with the adapter
//!   `Discovering`, and three unpaired devices in range as a discovery finds
//!   them: a speaker with a `Name`, an `Icon` and a strong `RSSI`, a phone
//!   with a `Name` and a weaker one, and a nameless advertiser whose `Alias`
//!   is BlueZ's dashed address, which the list leaves out.
//!
//! The bodies are deterministic (every map here is ordered), so `frames.rs`
//! checks that the committed files are this module's output, and `live.rs`'s
//! `synthesise_fixtures` rewrites them after an edit here:
//!
//! ```text
//! cargo test -p caustics-bluetooth --test live synthesise_fixtures -- --ignored
//! ```

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};

use zbus::zvariant::serialized::Context;
use zbus::zvariant::{LE, ObjectPath, Value, to_bytes};

/// The adapter's `Name` and `Alias`: BlueZ names an adapter after the host.
pub const ADAPTER_NAME: &str = "workstation";
pub const ADAPTER_ADDRESS: &str = "02:5E:00:00:0B:01";
/// The paired device in both bodies.
pub const TAG_ADDRESS: &str = "02:5E:00:00:0B:10";
pub const TAG_PATH: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_10";
pub const TAG_NAME: &str = "Example Tag";
/// The second device, in the two-device body only.
pub const BUDS_ADDRESS: &str = "02:5E:00:00:0B:20";
pub const BUDS_PATH: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_20";
pub const BUDS_ALIAS: &str = "Example Buds";
/// The devices in range, in the discovering body only.
pub const SPEAKER_ADDRESS: &str = "02:5E:00:00:0B:40";
pub const SPEAKER_PATH: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_40";
pub const SPEAKER_NAME: &str = "Example Speaker";
pub const PHONE_ADDRESS: &str = "02:5E:00:00:0B:50";
pub const PHONE_PATH: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_50";
pub const PHONE_NAME: &str = "example phone";
pub const BEACON_ADDRESS: &str = "02:5E:00:00:0B:60";
pub const BEACON_PATH: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_60";

const HCI0: &str = "/org/bluez/hci0";

type Props = BTreeMap<&'static str, Value<'static>>;
type Interfaces = BTreeMap<&'static str, Props>;

fn props<const N: usize>(entries: [(&'static str, Value<'static>); N]) -> Props {
    entries.into_iter().collect()
}

fn s(text: &str) -> Value<'static> {
    Value::from(text.to_owned())
}

fn strings(items: &[&str]) -> Value<'static> {
    Value::from(items.iter().map(|i| i.to_string()).collect::<Vec<_>>())
}

fn interfaces<const N: usize>(entries: [(&'static str, Props); N]) -> Interfaces {
    entries.into_iter().collect()
}

fn adapter(discovering: bool) -> Interfaces {
    interfaces([
        (
            "org.bluez.Adapter1",
            props([
                ("Address", s(ADAPTER_ADDRESS)),
                ("AddressType", s("public")),
                ("Alias", s(ADAPTER_NAME)),
                ("Class", Value::from(7078156u32)),
                ("Connectable", Value::from(false)),
                ("Discoverable", Value::from(false)),
                ("DiscoverableTimeout", Value::from(180u32)),
                ("Discovering", Value::from(discovering)),
                ("Manufacturer", Value::from(70u16)),
                ("Modalias", s("usb:v1D6Bp0246d0557")),
                ("Name", s(ADAPTER_NAME)),
                ("Pairable", Value::from(true)),
                ("PairableTimeout", Value::from(0u32)),
                ("PowerState", s("on")),
                ("Powered", Value::from(true)),
                ("Roles", strings(&["central", "peripheral"])),
                (
                    "UUIDs",
                    strings(&[
                        "0000110e-0000-1000-8000-00805f9b34fb",
                        "0000111f-0000-1000-8000-00805f9b34fb",
                        "00001200-0000-1000-8000-00805f9b34fb",
                        "0000110b-0000-1000-8000-00805f9b34fb",
                        "0000110a-0000-1000-8000-00805f9b34fb",
                        "0000110c-0000-1000-8000-00805f9b34fb",
                        "00001800-0000-1000-8000-00805f9b34fb",
                        "00001801-0000-1000-8000-00805f9b34fb",
                        "0000180a-0000-1000-8000-00805f9b34fb",
                        "03b80e5a-ede8-4b33-a751-6ce34ec4c700",
                        "0000111e-0000-1000-8000-00805f9b34fb",
                    ]),
                ),
                ("Version", Value::from(12u8)),
            ]),
        ),
        ("org.bluez.BatteryProviderManager1", Props::new()),
        ("org.bluez.GattManager1", Props::new()),
        (
            "org.bluez.LEAdvertisingManager1",
            props([
                ("ActiveInstances", Value::from(0u8)),
                (
                    "SupportedCapabilities",
                    Value::from(
                        [
                            ("MaxAdvLen".to_owned(), Value::from(251u8)),
                            ("MaxScnRspLen".to_owned(), Value::from(251u8)),
                            ("MaxTxPower".to_owned(), Value::from(13i16)),
                            ("MinTxPower".to_owned(), Value::from(-14i16)),
                        ]
                        .into_iter()
                        .collect::<HashMap<String, Value<'static>>>(),
                    ),
                ),
                (
                    "SupportedFeatures",
                    strings(&["CanSetTxPower", "HardwareOffload"]),
                ),
                (
                    "SupportedIncludes",
                    strings(&["tx-power", "appearance", "local-name"]),
                ),
                ("SupportedInstances", Value::from(20u8)),
                (
                    "SupportedSecondaryChannels",
                    strings(&["1M", "2M", "Coded"]),
                ),
            ]),
        ),
        (
            "org.bluez.Media1",
            props([
                ("SupportedFeatures", strings(&["tx-timestamping"])),
                (
                    "SupportedUUIDs",
                    strings(&[
                        "0000110a-0000-1000-8000-00805f9b34fb",
                        "0000110b-0000-1000-8000-00805f9b34fb",
                    ]),
                ),
            ]),
        ),
        ("org.bluez.NetworkServer1", Props::new()),
        ("org.freedesktop.DBus.Introspectable", Props::new()),
        ("org.freedesktop.DBus.Properties", Props::new()),
    ])
}

/// A paired LE device as BlueZ lists it: no `Icon`, because BlueZ derives one
/// from the device class or appearance and this device advertises neither.
fn tag() -> Props {
    props([
        (
            "Adapter",
            Value::from(ObjectPath::try_from(HCI0).expect("an object path")),
        ),
        ("Address", s(TAG_ADDRESS)),
        ("AddressType", s("random")),
        ("Alias", s(TAG_NAME)),
        ("Blocked", Value::from(false)),
        ("Bonded", Value::from(true)),
        ("CablePairing", Value::from(false)),
        ("Connected", Value::from(false)),
        ("LegacyPairing", Value::from(false)),
        ("Name", s(TAG_NAME)),
        ("Paired", Value::from(true)),
        ("ServicesResolved", Value::from(false)),
        ("Trusted", Value::from(false)),
        (
            "UUIDs",
            strings(&[
                "00001530-1212-efde-1523-785feabcd123",
                "00001800-0000-1000-8000-00805f9b34fb",
                "00001801-0000-1000-8000-00805f9b34fb",
                "0000180a-0000-1000-8000-00805f9b34fb",
                "0000180f-0000-1000-8000-00805f9b34fb",
                "6ba1b218-15a8-461f-9fa8-5dcae273eafd",
            ]),
        ),
    ])
}

/// An unpaired device as a discovery lists it: `RSSI` while it is heard, a
/// `Name` when it advertises one, and `Alias` BlueZ's fallback.
fn in_range(address: &'static str, name: Option<&str>, icon: Option<&str>, rssi: i16) -> Props {
    let mut device = props([
        (
            "Adapter",
            Value::from(ObjectPath::try_from(HCI0).expect("an object path")),
        ),
        ("Address", s(address)),
        ("AddressType", s("public")),
        (
            "Alias",
            s(&name.map_or_else(|| address.replace(':', "-"), str::to_owned)),
        ),
        ("Blocked", Value::from(false)),
        ("Bonded", Value::from(false)),
        ("Connected", Value::from(false)),
        ("LegacyPairing", Value::from(false)),
        ("Paired", Value::from(false)),
        ("RSSI", Value::from(rssi)),
        ("ServicesResolved", Value::from(false)),
        ("Trusted", Value::from(false)),
    ]);
    if let Some(name) = name {
        device.insert("Name", s(name));
    }
    if let Some(icon) = icon {
        device.insert("Icon", s(icon));
    }
    device
}

fn body(two_devices: bool) -> Vec<u8> {
    body_of(two_devices, false, false)
}

/// The discovering body's devices in range under an adapter that says
/// `discovering`: `false` is BlueZ's picture shortly after a discovery
/// stopped, when the devices it found are still listed.
pub fn in_range_body(discovering: bool) -> Vec<u8> {
    body_of(false, discovering, true)
}

fn body_of(two_devices: bool, discovering: bool, in_range_devices: bool) -> Vec<u8> {
    let mut objects: BTreeMap<ObjectPath<'static>, Interfaces> = BTreeMap::new();
    let mut put = |path: &'static str, interfaces: Interfaces| {
        objects.insert(
            ObjectPath::try_from(path).expect("an object path"),
            interfaces,
        );
    };
    put(
        "/org/bluez",
        interfaces([
            ("org.bluez.AgentManager1", Props::new()),
            ("org.bluez.ProfileManager1", Props::new()),
            ("org.freedesktop.DBus.Introspectable", Props::new()),
        ]),
    );
    put(HCI0, adapter(discovering));
    put(
        TAG_PATH,
        interfaces([
            ("org.bluez.Bearer.LE1", Props::new()),
            ("org.bluez.Device1", tag()),
            ("org.freedesktop.DBus.Introspectable", Props::new()),
            ("org.freedesktop.DBus.Properties", Props::new()),
        ]),
    );
    if two_devices {
        let mut buds = tag();
        buds.insert("Address", s(BUDS_ADDRESS));
        buds.insert("Alias", s(BUDS_ALIAS));
        buds.insert("Icon", s("audio-headset"));
        buds.insert("Connected", Value::from(true));
        put(BUDS_PATH, interfaces([("org.bluez.Device1", buds)]));
    }
    if in_range_devices {
        put(
            SPEAKER_PATH,
            interfaces([(
                "org.bluez.Device1",
                in_range(SPEAKER_ADDRESS, Some(SPEAKER_NAME), Some("audio-card"), -48),
            )]),
        );
        put(
            PHONE_PATH,
            interfaces([(
                "org.bluez.Device1",
                in_range(PHONE_ADDRESS, Some(PHONE_NAME), None, -71),
            )]),
        );
        put(
            BEACON_PATH,
            interfaces([(
                "org.bluez.Device1",
                in_range(BEACON_ADDRESS, None, None, -30),
            )]),
        );
    }
    to_bytes(Context::new_dbus(LE, 0), &objects)
        .expect("the body serialises")
        .bytes()
        .to_vec()
}

/// Every fixture this module writes, by file name under `tests/data/`.
pub fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("managed-objects.bin", body(false)),
        ("managed-objects-two-devices.bin", body(true)),
        ("managed-objects-discovering.bin", in_range_body(true)),
    ]
}
