//! `caustics-power` must never stop reading its signals (docs/architecture.md, *Rules*).
//!
//! zbus's socket reader hands every message to every stream's channel in turn
//! and **awaits** room in each (64 slots, overflow off). A watch that holds
//! its `PropertiesChanged` stream unpolled while it reads properties lets
//! a UPower that emits more than 64 changes during one read park the
//! reader with the property reply behind it — and with it every call on the
//! connection, logind's Lock and Suspend included.
//!
//! The peer here plays UPower on a private `dbus-daemon`: the first read of
//! `IsPresent` emits 200 `PropertiesChanged` on the display device *before*
//! it answers. Both streams exist before the first read, which is
//! one `GetAll` — the server calls every getter for it — so the first read is
//! the one the burst lands in. Without the drain the first battery
//! never arrives and the test times out at 5 s.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use caustics_power::{profiles, upower};
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::unbounded_channel;
use zbus::Connection;
use zbus::zvariant::Value;

/// More than the 64 zbus queues per stream, by a margin.
const BURST: u32 = 200;
const SERVICE: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE: &str = "org.freedesktop.UPower.Device";

/// UPower's display device, whose `IsPresent` bursts once.
struct Device {
    conn: Connection,
    reads: Arc<AtomicU32>,
}

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl Device {
    #[zbus(property)]
    async fn is_present(&self) -> bool {
        // Read 0 is the loop's first `GetAll`, with both streams subscribed.
        if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            let changed: HashMap<&str, Value<'_>> =
                HashMap::from([("Percentage", Value::F64(42.0))]);
            for _ in 0..BURST {
                let _ = self
                    .conn
                    .emit_signal(
                        None::<&str>,
                        DISPLAY_DEVICE,
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                        &(DEVICE, &changed, Vec::<&str>::new()),
                    )
                    .await;
            }
        }
        true
    }
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        42.0
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        "battery-good-symbolic".to_owned()
    }
}

#[tokio::test]
async fn a_properties_burst_during_a_read_does_not_stop_the_reply() {
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
    let reads = Arc::new(AtomicU32::new(0));
    peer.object_server()
        .at(
            DISPLAY_DEVICE,
            Device {
                conn: peer.clone(),
                reads: reads.clone(),
            },
        )
        .await
        .expect("the display device");
    peer.request_name(SERVICE)
        .await
        .expect("the peer owns UPower's name");
    let client = connect().await;

    let (batteries, mut seen) = unbounded_channel();
    let task = tokio::spawn(upower::watch(client, batteries));

    let battery = tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the GetAll reply arrived despite 200 signals ahead of it")
        .expect("the watch sent a battery");
    // The fake has no State or times, which read as unknown and none.
    assert_eq!(
        battery,
        Some(upower::Battery {
            percent: 42.0,
            icon: "battery-good-symbolic".to_owned(),
            state: upower::STATE_UNKNOWN,
            time_to_empty: 0,
            time_to_full: 0,
        })
    );
    assert!(reads.load(Ordering::SeqCst) >= 1, "the burst was emitted");
    task.abort();
}

/// The power-profiles daemon, whose `Profiles` bursts on
/// its first read. `profiles::watch` subscribes all four of its streams —
/// `PropertiesChanged` on each name's object and `NameOwnerChanged` for each name —
/// before that read, so an undrained stream would park the reader with the
/// reply behind 200 signals.
struct Profiles {
    conn: Connection,
    reads: Arc<AtomicU32>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl Profiles {
    #[zbus(property)]
    async fn profiles(&self) -> Vec<HashMap<String, zbus::zvariant::OwnedValue>> {
        if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            let changed: HashMap<&str, Value<'_>> =
                HashMap::from([("ActiveProfile", Value::from("balanced"))]);
            for _ in 0..BURST {
                let _ = self
                    .conn
                    .emit_signal(
                        None::<&str>,
                        profiles::HADESS.path,
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                        &(profiles::HADESS.service, &changed, Vec::<&str>::new()),
                    )
                    .await;
            }
        }
        let name = zbus::zvariant::OwnedValue::try_from(Value::from("balanced")).unwrap();
        vec![HashMap::from([("Profile".to_owned(), name)])]
    }
    #[zbus(property)]
    fn active_profile(&self) -> String {
        "balanced".to_owned()
    }
}

#[tokio::test]
async fn a_profiles_burst_during_a_read_does_not_stop_the_reply() {
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
    let reads = Arc::new(AtomicU32::new(0));
    peer.object_server()
        .at(
            profiles::HADESS.path,
            Profiles {
                conn: peer.clone(),
                reads: reads.clone(),
            },
        )
        .await
        .expect("the daemon's object");
    peer.request_name(profiles::HADESS.service)
        .await
        .expect("the peer owns the daemon's name");
    let client = connect().await;

    let (sink, mut seen) = unbounded_channel();
    let task = tokio::spawn(profiles::watch(client, sink));

    let first = tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the Profiles reply arrived despite 200 signals ahead of it")
        .expect("the watch sent the profiles");
    assert_eq!(
        first,
        Some(profiles::Profiles {
            names: vec!["balanced".to_owned()],
            active: "balanced".to_owned()
        })
    );
    assert!(reads.load(Ordering::SeqCst) >= 1, "the burst was emitted");
    task.abort();
}
