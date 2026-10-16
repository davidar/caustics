//! `caustics-network` must never stop reading its signals (docs/architecture.md, *Rules*).
//!
//! zbus's socket reader hands every message to every stream's channel in turn
//! and **awaits** room in each (64 slots, overflow off). A follower that held
//! its all-of-NM `MessageStream` unpolled across `snapshot`'s
//! `GetManagedObjects` and `GetSettings` calls would let a NetworkManager that
//! sends more than 64 signals during one snapshot — joining a network is
//! "dozens in under a second" by `bus.rs`'s own count — park the reader with
//! the reply behind it, and the scan and activate calls on the same
//! connection with it.
//!
//! The peer here plays NM on a private `dbus-daemon`: its first
//! `GetManagedObjects` emits 200 `PropertiesChanged` under the `Settings`
//! object *before* it answers. Without the forwarder no snapshot ever arrives
//! and the test times out at 5 s. With it, the burst is also what makes the
//! second snapshot:
//! a signal from under `Settings` drops the profile cache and re-reads.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use caustics_network::{bus, nm};
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::unbounded_channel;
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// More than the 64 zbus queues per stream, by a margin.
const BURST: u32 = 200;

type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// NM's `ObjectManager`: nothing managed, and a first walk that bursts.
struct Objects {
    conn: Connection,
    calls: Arc<AtomicU32>,
}

#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl Objects {
    async fn get_managed_objects(&self) -> Managed {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let changed: HashMap<&str, Value<'_>> = HashMap::from([("VersionId", Value::U64(1))]);
            for _ in 0..BURST {
                let _ = self
                    .conn
                    .emit_signal(
                        None::<&str>,
                        nm::SETTINGS_PATH,
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                        &(
                            "org.freedesktop.NetworkManager.Settings",
                            &changed,
                            Vec::<&str>::new(),
                        ),
                    )
                    .await;
            }
        }
        Managed::new()
    }
}

#[tokio::test]
async fn a_signal_burst_during_a_snapshot_does_not_stop_the_reply() {
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
            nm::OBJECT_MANAGER_PATH,
            Objects {
                conn: peer.clone(),
                calls: calls.clone(),
            },
        )
        .await
        .expect("NM's object manager");
    peer.request_name(nm::SERVICE)
        .await
        .expect("the peer owns NM's name");
    let client = connect().await;

    let (snapshots, mut seen) = unbounded_channel();
    let task = tokio::spawn(bus::watch(client, snapshots));

    let first = tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the GetManagedObjects reply arrived despite 200 signals ahead of it")
        .expect("the watch sent something");
    assert!(first.is_some(), "a snapshot, not NM's absence");

    // The burst itself is one re-read after the debounce, not two hundred.
    let second = tokio::time::timeout(bus::DEBOUNCE * 8, seen.recv())
        .await
        .expect("the burst woke one re-read")
        .expect("the watch sent something");
    assert!(second.is_some(), "a snapshot, not NM's absence");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one walk, then one re-read for the whole burst"
    );
    task.abort();
}
