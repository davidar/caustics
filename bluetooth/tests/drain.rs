//! `caustics-bluetooth` must never stop reading its signals (docs/architecture.md,
//! *Rules*).
//!
//! zbus's socket reader hands every message to every stream's channel in turn
//! and **awaits** room in each. A watch that subscribes three streams with
//! queues of 16, 16 and 4 and holds them unpolled across `read`'s
//! `GetManagedObjects` lets seventeen `PropertiesChanged` from BlueZ during one
//! read — a device connecting is about that many — park the reader with the
//! reply behind it.
//!
//! The peer here plays BlueZ on a private `dbus-daemon`: its first
//! `GetManagedObjects` emits 200 `PropertiesChanged` and 200 `InterfacesAdded`
//! *before* it answers. Without the drains no state ever arrives and the test
//! times out at 5 s.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use caustics_bluetooth::bluez;
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::unbounded_channel;
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// More than any of the three queues, and than zbus's default 64, by a margin.
const BURST: u32 = 200;
const ADAPTER_PATH: &str = "/org/bluez/hci0";

/// BlueZ's root object: one adapter, and a `GetManagedObjects` that bursts
/// once.
struct Root {
    conn: Connection,
    calls: Arc<AtomicU32>,
}

#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl Root {
    async fn get_managed_objects(&self) -> bluez::Managed {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let changed: HashMap<&str, Value<'_>> = HashMap::from([("Powered", Value::Bool(true))]);
            let added: HashMap<&str, HashMap<&str, Value<'_>>> =
                HashMap::from([(bluez::DEVICE, HashMap::new())]);
            for n in 0..BURST {
                let _ = self
                    .conn
                    .emit_signal(
                        None::<&str>,
                        ADAPTER_PATH,
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                        &(bluez::ADAPTER, &changed, Vec::<&str>::new()),
                    )
                    .await;
                let device = zbus::zvariant::ObjectPath::try_from(format!(
                    "{ADAPTER_PATH}/dev_00_00_00_00_{:02X}_{:02X}",
                    n >> 8,
                    n & 0xff
                ))
                .expect("a device path");
                let _ = self
                    .conn
                    .emit_signal(
                        None::<&str>,
                        "/",
                        "org.freedesktop.DBus.ObjectManager",
                        "InterfacesAdded",
                        &(device, &added),
                    )
                    .await;
            }
        }
        let adapter: HashMap<String, OwnedValue> = HashMap::from([
            (
                "Name".to_owned(),
                OwnedValue::try_from(Value::from("test-adapter")).unwrap(),
            ),
            ("Powered".to_owned(), OwnedValue::from(true)),
        ]);
        HashMap::from([(
            OwnedObjectPath::try_from(ADAPTER_PATH).unwrap(),
            HashMap::from([(bluez::ADAPTER.to_owned(), adapter)]),
        )])
    }
}

#[tokio::test]
async fn a_signal_burst_during_get_managed_objects_does_not_stop_the_reply() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the burst is untested here");
        return;
    };
    let connect = || async {
        zbus::connection::Builder::address(bus.address.as_str())
            .expect("the bus address parses")
            .build()
            .await
            .expect("a connection to the private bus")
    };
    let peer = connect().await;
    let calls = Arc::new(AtomicU32::new(0));
    peer.object_server()
        .at(
            "/",
            Root {
                conn: peer.clone(),
                calls: calls.clone(),
            },
        )
        .await
        .expect("BlueZ's root object");
    peer.request_name(bluez::SERVICE)
        .await
        .expect("the peer owns org.bluez");
    let client = connect().await;

    let (states, mut seen) = unbounded_channel();
    let task = tokio::spawn(bluez::watch(client, states));

    let state = tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the GetManagedObjects reply arrived despite 400 signals ahead of it")
        .expect("the watch sent a state");
    assert!(state.powered());
    let adapter = state.adapter.expect("the adapter the reply named");
    assert_eq!(adapter.name, "test-adapter");
    assert!(calls.load(Ordering::SeqCst) >= 1, "the burst was emitted");
    task.abort();
}
