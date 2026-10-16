//! BlueZ on the **system** bus: one `GetManagedObjects` walk is the whole
//! picture.
//!
//! `org.freedesktop.DBus.ObjectManager` on `/` of `org.bluez` answers
//! `a{oa{sa{sv}}}` — every adapter, every device, every interface, every
//! property, in one round trip — so nothing here enumerates paths and nothing
//! here caches a tree. Every signal from BlueZ (a `PropertiesChanged`, an
//! `InterfacesAdded`, an `InterfacesRemoved`) is the same instruction: read it
//! again and rebuild. At this scale — one adapter, a handful of paired devices
//! — a full re-read is cheaper than tracking, and it cannot go stale.
//!
//! Properties are read as a plain `a{sv}` rather than a typed dict, for the
//! reason `caustics-tray` reads dbusmenu properties that way: BlueZ sends what the
//! device has. A random-address LE device (a tracker tag, say) has **no
//! `Icon` and no `Class`**, so [`Device::icon`] is routinely empty and the
//! fallback in [`crate::rows`] is the normal case, not the edge.
//!
//! [`parse_managed_objects`] is the one door `bluetooth/tests/` comes through:
//! it deserialises a **reply body** (in the tests, synthesised in BlueZ's own
//! layout) with the same zvariant types the live path uses and hands it to the
//! same [`state_of`], so what the tests exercise is the real deserialiser
//! against real bytes.

use std::collections::HashMap;
use std::sync::Arc;

use caustics_sidecar::{Drain, drain};
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use zbus::zvariant::serialized::{Context, Data};
use zbus::zvariant::{LE, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

use crate::witness;

/// The service this sidecar is about. Absent on a machine without bluetoothd.
pub const SERVICE: &str = "org.bluez";
/// The adapter interface: `Powered` (read/write) and `Name`.
pub const ADAPTER: &str = "org.bluez.Adapter1";
/// The device interface: `Alias`/`Name`/`Address`, `Paired`, `Connected`,
/// `Icon`, and the two methods a row click makes.
pub const DEVICE: &str = "org.bluez.Device1";

const ROOT: &str = "/";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const DBUS: &str = "org.freedesktop.DBus";

/// `Connect` and `Disconnect`, as [`crate::actions::Call`] names them.
pub const CONNECT: &str = "Connect";
pub const DISCONNECT: &str = "Disconnect";

/// `GetManagedObjects`'s reply: `a{oa{sa{sv}}}`, object path to interface to
/// properties. The type the live path and the test fixtures share.
pub type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// The adapter the chip is about. One machine, one adapter in practice; when
/// there are several this is the lowest path, so `hci0` beats `hci1` and the
/// choice does not move between frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adapter {
    /// Its object path, which is what an action needs.
    pub path: String,
    /// `Name` — the witness's, not the chip's; the chip is an icon.
    pub name: String,
    pub powered: bool,
}

/// A **paired** device of that adapter. Unpaired ones are not rows: pairing is
/// the settings program's job and a discovery list is not a bar menu.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    /// Its object path, which is what an action needs.
    pub path: String,
    /// `Address` — the MAC, and the stable key the row id is derived from.
    pub address: String,
    /// `Alias`, else `Name`, else the address: the row's label.
    pub alias: String,
    /// `Icon` — a themed name BlueZ hands out (`audio-headset`, `input-mouse`).
    /// Empty for a device whose class BlueZ does not know, which is an
    /// ordinary case; [`crate::rows`] falls back.
    pub icon: String,
    pub connected: bool,
}

/// The whole picture, as this sidecar sees it. [`Default`] is "no adapter",
/// which is the empty frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub adapter: Option<Adapter>,
    /// The adapter's paired devices, by alias then address — a user-visible
    /// order that does not depend on a hash map's iteration.
    pub devices: Vec<Device>,
}

impl State {
    /// Whether the adapter is on. No adapter is not on.
    pub fn powered(&self) -> bool {
        self.adapter.as_ref().is_some_and(|adapter| adapter.powered)
    }

    /// How many of the devices are connected — the publish witness's second
    /// fact.
    pub fn connected(&self) -> usize {
        self.devices
            .iter()
            .filter(|device| device.connected)
            .count()
    }
}

// The two proxies, hand-written as `caustics-power`'s are: the only methods and
// properties declared are the ones an action makes, because everything *read*
// comes from the ObjectManager walk above.

#[zbus::proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
trait Adapter1 {
    #[zbus(property)]
    fn set_powered(&self, on: bool) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait Device1 {
    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
}

/// The whole picture, in one round trip.
pub async fn read(conn: &Connection) -> Result<State, String> {
    let reply = conn
        .call_method(
            Some(SERVICE),
            ROOT,
            Some(OBJECT_MANAGER),
            "GetManagedObjects",
            &(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let objects: Managed = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(state_of(&objects))
}

/// A raw `GetManagedObjects` **reply body** as the state it means.
///
/// The tests' one door: the bytes are in BlueZ's own layout, the deserialiser and the
/// translation are the live path's, and a value rebuilt in a test would only
/// prove the test agrees with itself.
pub fn parse_managed_objects(body: &[u8]) -> Result<State, String> {
    // A reply body from a little-endian peer, read from its first byte — what
    // `Message::body().data().bytes()` wrote down.
    let data = Data::new(body, Context::new_dbus(LE, 0));
    let (objects, _) = data.deserialize::<Managed>().map_err(|e| format!("{e}"))?;
    Ok(state_of(&objects))
}

/// The walk's reply as [`State`]: one adapter, its paired devices.
pub fn state_of(objects: &Managed) -> State {
    let mut adapters: Vec<(&OwnedObjectPath, &HashMap<String, OwnedValue>)> = objects
        .iter()
        .filter_map(|(path, interfaces)| interfaces.get(ADAPTER).map(|props| (path, props)))
        .collect();
    // By path, so a second adapter cannot make the chip flip between frames.
    adapters.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
    let adapter = adapters.first().map(|(path, props)| Adapter {
        path: path.as_str().to_owned(),
        name: string(props, "Name").unwrap_or_default().to_owned(),
        powered: boolean(props, "Powered").unwrap_or(false),
    });

    let mut devices: Vec<Device> = objects
        .iter()
        .filter_map(|(path, interfaces)| interfaces.get(DEVICE).map(|props| (path, props)))
        // Paired only: an unpaired device is a discovery result, not a row.
        .filter(|(_, props)| boolean(props, "Paired").unwrap_or(false))
        // And of *this* adapter, when both the device and the adapter say so.
        .filter(
            |(_, props)| match (adapter.as_ref(), string(props, "Adapter")) {
                (Some(adapter), Some(owner)) => owner == adapter.path,
                _ => true,
            },
        )
        .map(|(path, props)| {
            let address = string(props, "Address").unwrap_or_default().to_owned();
            let alias = string(props, "Alias")
                .filter(|alias| !alias.is_empty())
                .or_else(|| string(props, "Name").filter(|name| !name.is_empty()))
                .map(|alias| alias.to_owned())
                .unwrap_or_else(|| address.clone());
            Device {
                path: path.as_str().to_owned(),
                address,
                alias,
                icon: string(props, "Icon").unwrap_or_default().to_owned(),
                connected: boolean(props, "Connected").unwrap_or(false),
            }
        })
        .collect();
    devices.sort_by(|a, b| {
        a.alias
            .to_lowercase()
            .cmp(&b.alias.to_lowercase())
            .then_with(|| a.address.cmp(&b.address))
    });

    State { adapter, devices }
}

/// Set the adapter's `Powered`. One property write; BlueZ answers before the
/// radio is up, and the `PropertiesChanged` that follows is what republishes.
pub async fn set_powered(conn: &Connection, path: &str, on: bool) -> Result<(), String> {
    let adapter = Adapter1Proxy::builder(conn)
        .path(path.to_owned())
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    adapter.set_powered(on).await.map_err(|e| e.to_string())
}

/// `Connect()` or `Disconnect()` on one device. Either can take seconds, which
/// is why the caller spawns this rather than awaiting it in the select loop.
pub async fn device_method(conn: &Connection, path: &str, member: &str) -> Result<(), String> {
    let device = Device1Proxy::builder(conn)
        .path(path.to_owned())
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    let result = match member {
        CONNECT => device.connect().await,
        DISCONNECT => device.disconnect().await,
        other => {
            return Err(format!(
                "{other} is not a Device1 method this sidecar makes"
            ));
        }
    };
    result.map_err(|e| e.to_string())
}

/// Watch BlueZ, sending the whole state whenever a signal says something moved.
///
/// Ends only when its signal stream does; a BlueZ that is not there is an
/// `absent:` witness, the empty state, and a `NameOwnerChanged` to wait on —
/// exiting would only make the bar restart this into its backoff and fill the
/// journal.
pub async fn watch(conn: Connection, states: UnboundedSender<State>) {
    let changed = Arc::new(Notify::new());
    let mut drains = match signals(&conn, &changed).await {
        Ok(drains) => drains,
        Err(e) => {
            // No match rule is no wake-up: publish what is there once and stop
            // watching rather than spinning.
            witness(&format!("bluez: no signal subscription ({e})"));
            let _ = states.send(read(&conn).await.unwrap_or_default());
            return;
        }
    };

    // The three streams end together, when the connection does.
    let ended = async {
        for drain in &mut drains {
            drain.ended().await;
        }
    };
    tokio::pin!(ended);

    // `absent:` is a transition, not a heartbeat: BlueZ emits plenty of signals
    // and a missing adapter must not write a line for each of them.
    let mut reported: Option<&'static str> = None;
    loop {
        let state = match read(&conn).await {
            Ok(state) => {
                if state.adapter.is_none() {
                    if reported != Some("adapter") {
                        witness("absent: adapter");
                        reported = Some("adapter");
                    }
                } else if reported.is_some() {
                    witness(&format!(
                        "bluez: adapter {} is back",
                        state
                            .adapter
                            .as_ref()
                            .map(|a| a.name.as_str())
                            .unwrap_or_default()
                    ));
                    reported = None;
                }
                state
            }
            Err(e) => {
                if reported != Some("bluez") {
                    witness("absent: bluez");
                    witness(&format!("bluez: {e}"));
                    reported = Some("bluez");
                }
                State::default()
            }
        };
        if states.send(state).is_err() {
            return;
        }
        tokio::select! {
            // A change that arrived before the end is read first.
            biased;
            () = changed.notified() => {}
            () = &mut ended => {
                witness("bluez: the signal stream ended");
                return;
            }
        }
    }
}

/// Every signal that means "read it again", as one stream.
///
/// Three rules: BlueZ's `PropertiesChanged` on any path (a device connects, the
/// adapter powers), BlueZ's `InterfacesAdded`/`InterfacesRemoved` (a device is
/// paired or removed, an adapter is plugged), and the bus daemon's
/// `NameOwnerChanged` for `org.bluez` itself (bluetoothd stops and starts).
/// None of their contents is read: the re-read is the whole reaction, so all
/// three fold into one `Notify`.
///
/// **Each stream is handed to a [`drain`] the moment it exists** (docs/architecture.md,
/// *Rules*). Their queues are 16, 16 and 4, and `watch`
/// spends most of its life inside `read`'s `GetManagedObjects`: a stream held unpolled across that
/// call — or across the next subscription, which is a bus call too — fills
/// after a handful of signals and parks zbus's socket reader with the reply
/// behind it. The drain awaits nothing but its stream.
async fn signals(conn: &Connection, changed: &Arc<Notify>) -> zbus::Result<[Drain; 3]> {
    let properties = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(SERVICE)?
        .interface(PROPERTIES)?
        .member("PropertiesChanged")?
        .build();
    // Interface without member: `InterfacesAdded` and `InterfacesRemoved` in
    // one rule, since both mean the same thing here.
    let objects = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(SERVICE)?
        .interface(OBJECT_MANAGER)?
        .build();
    let names = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DBUS)?
        .interface(DBUS)?
        .member("NameOwnerChanged")?
        .arg(0, SERVICE)?
        .build();

    let properties = drain(
        MessageStream::for_match_rule(properties, conn, Some(16)).await?,
        changed.clone(),
    );
    let objects = drain(
        MessageStream::for_match_rule(objects, conn, Some(16)).await?,
        changed.clone(),
    );
    let names = drain(
        MessageStream::for_match_rule(names, conn, Some(4)).await?,
        changed.clone(),
    );
    Ok([properties, objects, names])
}

fn string<'a>(props: &'a HashMap<String, OwnedValue>, key: &str) -> Option<&'a str> {
    match &**props.get(key)? {
        Value::Str(s) => Some(s.as_str()),
        // `Adapter` is an `o`, not an `s`, and is read by the same helper.
        Value::ObjectPath(path) => Some(path.as_str()),
        _ => None,
    }
}

fn boolean(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match &**props.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// The recorder: a diagnosis tool, not part of the sidecar. It is here rather
/// than under `bluetooth/tests/` because recording needs this crate's own
/// zbus types.
///
/// **A recording is for diagnosis on the developer's own machine and is never
/// committed.** It holds that machine's adapter name, its paired devices'
/// addresses and aliases. The committed fixtures are synthesised from invented
/// data by `bluetooth/tests/synth/` (rewrite them with
/// `cargo test -p caustics-bluetooth --test live synthesise_fixtures -- --ignored`),
/// and `bluetooth/tests/frames.rs` fails while a recording sits in their
/// place, so restore them before committing.
#[cfg(test)]
mod record {
    use super::*;

    /// Overwrite `bluetooth/tests/data/` with a recording of the live system
    /// bus. **Read-only on the bus**: one `GetManagedObjects` call and nothing
    /// else — no `Connect`, no `Disconnect`, no `Powered` write.
    ///
    /// Two files: `managed-objects.bin` is BlueZ's own reply body, verbatim.
    /// `managed-objects-two-devices.bin` is that body **edited** — the first
    /// paired device cloned to a second, locally administered address, given
    /// an `Icon` and `Connected = true` — and re-serialised through zvariant,
    /// so a machine with one paired device still shows two rows keeping their
    /// own ids.
    #[test]
    #[ignore = "rewrites bluetooth/tests/data from the live system bus"]
    fn record_the_managed_objects_fixtures() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("a tokio runtime");
        runtime.block_on(async {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
            let conn = Connection::system().await.expect("the system bus");
            let reply = conn
                .call_method(
                    Some(SERVICE),
                    ROOT,
                    Some(OBJECT_MANAGER),
                    "GetManagedObjects",
                    &(),
                )
                .await
                .expect("GetManagedObjects");
            let recorded = reply.body().data().bytes().to_vec();
            std::fs::write(format!("{dir}/managed-objects.bin"), &recorded)
                .expect("the fixture writes");
            println!("recorded managed-objects.bin ({} bytes)", recorded.len());

            let mut objects: Managed = reply.body().deserialize().expect("the reply deserialises");
            let (path, mut props) = objects
                .iter()
                .find_map(|(path, interfaces)| {
                    interfaces
                        .get(DEVICE)
                        .map(|props| (path.clone(), props.clone()))
                })
                .expect("a device on the bus to edit");
            println!("editing {path} into a second, connected device");
            props.insert(
                "Address".to_owned(),
                OwnedValue::try_from(Value::from("02:5E:00:00:0B:20")).expect("an address"),
            );
            props.insert(
                "Alias".to_owned(),
                OwnedValue::try_from(Value::from("Example Buds")).expect("an alias"),
            );
            props.insert(
                "Icon".to_owned(),
                OwnedValue::try_from(Value::from("audio-headset")).expect("an icon"),
            );
            props.insert(
                "Connected".to_owned(),
                OwnedValue::try_from(Value::from(true)).expect("connected"),
            );
            let second = OwnedObjectPath::try_from("/org/bluez/hci0/dev_02_5E_00_00_0B_20")
                .expect("a device path");
            let mut interfaces = HashMap::new();
            interfaces.insert(DEVICE.to_owned(), props);
            objects.insert(second, interfaces);

            let derived = zbus::zvariant::to_bytes(Context::new_dbus(LE, 0), &objects)
                .expect("the edited body re-serialises");
            std::fs::write(
                format!("{dir}/managed-objects-two-devices.bin"),
                derived.bytes(),
            )
            .expect("the fixture writes");
            println!(
                "derived managed-objects-two-devices.bin ({} bytes)",
                derived.bytes().len()
            );
        });
    }
}
