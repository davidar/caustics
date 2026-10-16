//! NetworkManager on the **system** bus: the one `GetManagedObjects` walk that
//! is the whole picture, plus `GetSettings` on each known connection for the
//! two facts the walk does not carry.
//!
//! NM implements `org.freedesktop.DBus.ObjectManager` on `/org/freedesktop`,
//! and — as of NM 1.56 — that one reply
//! carries every object this sidecar needs: the manager's own properties,
//! `Settings.Connections`, every `Device` (with `Device.Wireless` beside it),
//! every `AccessPoint` with its `Ssid` and `Strength`, and every
//! `Connection.Active` with its `Uuid`, `Type` and `State`. Nothing here walks
//! object paths one at a time.
//!
//! What the walk does *not* carry is a stored connection's `connection.id`
//! and its `802-11-wireless.ssid` — `Settings.Connection` only publishes
//! `Unsaved`, `Flags`, `Filename` and `VersionId` as properties — so those
//! come from `GetSettings()`, cached per connection path and re-walked only
//! when a Settings signal says the stored connections moved.
//!
//! **Bytes in, model out.** [`parse_managed_objects`] and [`parse_settings`]
//! take a raw D-Bus **reply body** and nothing else, exactly as
//! `tray::menu::parse_reply` does, so `network/tests/` drives the real
//! deserialiser against bus bytes in NM's own layout, synthesised from
//! invented data (`network/tests/synth/`). A value rebuilt
//! in the test would only prove the test agrees with itself.

use std::collections::HashMap;

use zbus::zvariant::serialized::{Context, Data};
use zbus::zvariant::{LE, OwnedObjectPath, OwnedValue, Value};

/// NM's well-known name: the sender of every signal this sidecar follows, and
/// the name whose arrival ends the `absent:` wait.
pub const SERVICE: &str = "org.freedesktop.NetworkManager";
/// The `ObjectManager` the whole walk goes through.
pub const OBJECT_MANAGER_PATH: &str = "/org/freedesktop";
/// The manager object itself: `WirelessEnabled` is set here.
pub const MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
/// The `Settings` object: a signal on this path or under it means the stored
/// connections changed and the [`Settings`] cache is stale.
pub const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";

const IF_MANAGER: &str = "org.freedesktop.NetworkManager";
const IF_SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const IF_DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const IF_WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const IF_ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
const IF_ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";

/// `NM_DEVICE_TYPE_ETHERNET`.
pub const DEVICE_ETHERNET: u32 = 1;
/// `NM_DEVICE_TYPE_WIFI`.
pub const DEVICE_WIFI: u32 = 2;

/// `NM_ACTIVE_CONNECTION_STATE_ACTIVATING`.
pub const ACTIVE_ACTIVATING: u32 = 1;
/// `NM_ACTIVE_CONNECTION_STATE_ACTIVATED`.
pub const ACTIVE_ACTIVATED: u32 = 2;

/// `connection.type` of a Wi-Fi profile.
pub const TYPE_WIFI: &str = "802-11-wireless";
/// `connection.type` of a wired profile — here only so the wired fixture reads
/// as deliberate.
pub const TYPE_ETHERNET: &str = "802-3-ethernet";

/// The object path D-Bus spells "none".
pub const NO_PATH: &str = "/";

// ---------------------------------------------------------------------------
// The walk, as a value
// ---------------------------------------------------------------------------

/// One `GetManagedObjects` reply, reduced to what the picture is made of.
/// Paths are kept as strings: this sidecar compares them and hands two of them
/// back to NM, and never builds one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Managed {
    pub wireless_enabled: bool,
    /// `PrimaryConnection`, or [`NO_PATH`] when there is no default route.
    pub primary: String,
    /// `Settings.Connections`: every stored profile, in NM's order.
    pub connections: Vec<String>,
    pub devices: Vec<Device>,
    pub access_points: Vec<AccessPoint>,
    pub active: Vec<Active>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub path: String,
    /// `DeviceType`: [`DEVICE_ETHERNET`], [`DEVICE_WIFI`], or one of the many
    /// others (32 loopback, 16 tun — both common beside a Wi-Fi device).
    pub kind: u32,
    /// `Device.Wireless.AccessPoints`, empty for anything but Wi-Fi.
    pub access_points: Vec<String>,
    /// `Device.Wireless.ActiveAccessPoint`, [`NO_PATH`] when not associated.
    pub active_access_point: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessPoint {
    pub path: String,
    /// `Ssid`, an `ay`: raw bytes, because an SSID is not required to be text.
    pub ssid: Vec<u8>,
    /// `Strength`, a `y` in 0..=100.
    pub strength: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Active {
    pub path: String,
    /// The `Settings.Connection` path behind this activation.
    pub connection: String,
    pub uuid: String,
    /// `Type`, e.g. [`TYPE_WIFI`].
    pub kind: String,
    /// `NM_ACTIVE_CONNECTION_STATE_*`.
    pub state: u32,
    /// The devices carrying it, which is how the primary's kind is decided.
    pub devices: Vec<String>,
}

/// One `GetSettings` reply, reduced to the three facts a row needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// `connection.id` — the label, and what NM itself calls this network.
    pub id: String,
    pub uuid: String,
    /// `connection.type`.
    pub kind: String,
    /// `802-11-wireless.ssid`, absent for a profile that has no SSID (a wired
    /// one, or a Wi-Fi one matching by BSSID only).
    pub ssid: Option<Vec<u8>>,
}

/// The `GetSettings` replies, by connection object path.
pub type SettingsByPath = HashMap<String, Settings>;

// ---------------------------------------------------------------------------
// The parsers — the one door the tests come through
// ---------------------------------------------------------------------------

/// `a{oa{sa{sv}}}`: a `GetManagedObjects` reply body.
type ManagedWire = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;
/// `a{sa{sv}}`: a `GetSettings` reply body.
type SettingsWire = HashMap<String, HashMap<String, OwnedValue>>;

/// A raw `ObjectManager.GetManagedObjects()` **reply body** as the picture it
/// means.
pub fn parse_managed_objects(body: &[u8]) -> Result<Managed, String> {
    let data = Data::new(body, Context::new_dbus(LE, 0));
    let (objects, _) = data
        .deserialize::<ManagedWire>()
        .map_err(|e| format!("{e}"))?;
    Ok(managed(&objects))
}

/// A raw `Settings.Connection.GetSettings()` **reply body** as the three facts
/// a row needs. Secrets are not in this reply by NM's contract — which
/// [`secret_keys`] checks rather than trusts.
pub fn parse_settings(body: &[u8]) -> Result<Settings, String> {
    let data = Data::new(body, Context::new_dbus(LE, 0));
    let (groups, _) = data
        .deserialize::<SettingsWire>()
        .map_err(|e| format!("{e}"))?;
    Ok(settings(&groups))
}

/// Every key in a `GetSettings` body that would be a secret if NM ever put one
/// there: `psk`, anything spelling `password`, `secret` or `wep-key`, and
/// `pin`. Empty is the expected answer, and both the recorder and a test over
/// the committed fixtures assert it.
///
/// The check is on **keys**, not on a substring of the body, because `wpa-psk`
/// is a perfectly ordinary *value* of `key-mgmt` — every WPA-PSK profile has
/// it — and a byte search for `psk` would fail on a body that is
/// exactly right.
pub fn secret_keys(body: &[u8]) -> Result<Vec<String>, String> {
    let data = Data::new(body, Context::new_dbus(LE, 0));
    let (groups, _) = data
        .deserialize::<SettingsWire>()
        .map_err(|e| format!("{e}"))?;
    let mut found = Vec::new();
    for (group, entries) in &groups {
        for key in entries.keys() {
            let k = key.to_ascii_lowercase();
            if k == "psk"
                || k == "pin"
                || k.contains("password")
                || k.contains("secret")
                || k.contains("wep-key")
            {
                found.push(format!("{group}.{key}"));
            }
        }
    }
    found.sort();
    Ok(found)
}

// ---------------------------------------------------------------------------
// Wire values to the structs above
// ---------------------------------------------------------------------------

fn managed(objects: &ManagedWire) -> Managed {
    let mut out = Managed {
        primary: NO_PATH.to_owned(),
        ..Managed::default()
    };
    for (path, interfaces) in objects {
        let path = path.as_str();
        if let Some(props) = interfaces.get(IF_MANAGER) {
            out.wireless_enabled = boolean(props, "WirelessEnabled").unwrap_or(false);
            if let Some(primary) = path_of(props, "PrimaryConnection") {
                out.primary = primary;
            }
        }
        if let Some(props) = interfaces.get(IF_SETTINGS) {
            out.connections = paths(props, "Connections");
        }
        if let Some(props) = interfaces.get(IF_DEVICE) {
            let wireless = interfaces.get(IF_WIRELESS);
            out.devices.push(Device {
                path: path.to_owned(),
                kind: integer(props, "DeviceType").unwrap_or(0),
                access_points: wireless
                    .map(|w| paths(w, "AccessPoints"))
                    .unwrap_or_default(),
                active_access_point: wireless
                    .and_then(|w| path_of(w, "ActiveAccessPoint"))
                    .unwrap_or_else(|| NO_PATH.to_owned()),
            });
        }
        if let Some(props) = interfaces.get(IF_ACCESS_POINT) {
            out.access_points.push(AccessPoint {
                path: path.to_owned(),
                ssid: bytes(props, "Ssid").unwrap_or_default(),
                strength: byte(props, "Strength").unwrap_or(0),
            });
        }
        if let Some(props) = interfaces.get(IF_ACTIVE) {
            out.active.push(Active {
                path: path.to_owned(),
                connection: path_of(props, "Connection").unwrap_or_else(|| NO_PATH.to_owned()),
                uuid: string(props, "Uuid").unwrap_or_default(),
                kind: string(props, "Type").unwrap_or_default(),
                state: integer(props, "State").unwrap_or(0),
                devices: paths(props, "Devices"),
            });
        }
    }
    // A `HashMap` hands its entries back in no order at all, and every one of
    // these lists becomes rows or is searched by path, so sort them: the same
    // bytes must always make the same frame.
    out.devices.sort_by(|a, b| a.path.cmp(&b.path));
    out.access_points.sort_by(|a, b| a.path.cmp(&b.path));
    out.active.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn settings(groups: &SettingsWire) -> Settings {
    let connection = groups.get("connection");
    Settings {
        id: connection.and_then(|c| string(c, "id")).unwrap_or_default(),
        uuid: connection
            .and_then(|c| string(c, "uuid"))
            .unwrap_or_default(),
        kind: connection
            .and_then(|c| string(c, "type"))
            .unwrap_or_default(),
        ssid: groups.get(TYPE_WIFI).and_then(|w| bytes(w, "ssid")),
    }
}

// ---------------------------------------------------------------------------
// `OwnedValue` readers. Every one of them is "the right type or nothing": a
// property NM renamed or retyped is a missing fact, never a panic.
// ---------------------------------------------------------------------------

/// `OwnedValue` derefs to the `Value` it wraps; every reader below goes through
/// this so none of them clones a variant to look inside it.
fn value<'a>(props: &'a HashMap<String, OwnedValue>, key: &str) -> Option<&'a Value<'static>> {
    props.get(key).map(|owned| &**owned)
}

fn boolean(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match value(props, key) {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn integer(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    match value(props, key) {
        Some(Value::U32(n)) => Some(*n),
        _ => None,
    }
}

fn byte(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    match value(props, key) {
        Some(Value::U8(n)) => Some(*n),
        _ => None,
    }
}

fn string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match value(props, key) {
        Some(Value::Str(s)) => Some(s.as_str().to_owned()),
        _ => None,
    }
}

fn path_of(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match value(props, key) {
        Some(Value::ObjectPath(p)) => Some(p.as_str().to_owned()),
        _ => None,
    }
}

fn paths(props: &HashMap<String, OwnedValue>, key: &str) -> Vec<String> {
    match value(props, key) {
        Some(Value::Array(array)) => array
            .iter()
            .filter_map(|v| match v {
                Value::ObjectPath(p) => Some(p.as_str().to_owned()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// An `ay`. zvariant hands a byte array back as an `Array` of `U8` when it came
/// out of a variant, so read it that way rather than asking for a `Vec<u8>`.
fn bytes(props: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<u8>> {
    match value(props, key) {
        Some(Value::Array(array)) => Some(
            array
                .iter()
                .filter_map(|v| match v {
                    Value::U8(b) => Some(*b),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}
