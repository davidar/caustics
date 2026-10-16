//! UPower coming and going under a running watch, and the battery read out of
//! one `GetAll`.
//!
//! A watch that returns for good when UPower is absent at start, or wakes
//! only on `PropertiesChanged`, leaves the chip on a dead instance's reading
//! after an upowerd restart until the new one happens to emit. The fakes here never emit `PropertiesChanged` at all: every reading the test
//! waits for is one that `NameOwnerChanged` alone brought.

use std::collections::HashMap;
use std::time::Duration;

use caustics_power::upower::{
    self, Battery, DISPLAY_DEVICE, FALLBACK_ICON, SERVICE, STATE_UNKNOWN,
};
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

/// UPower's display device at one fixed percentage, discharging.
struct Device(f64);

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl Device {
    #[zbus(property)]
    fn is_present(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.0
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        "battery-good-symbolic".to_owned()
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        upower::STATE_DISCHARGING
    }
    #[zbus(property)]
    fn time_to_empty(&self) -> i64 {
        7980
    }
    #[zbus(property)]
    fn time_to_full(&self) -> i64 {
        0
    }
}

async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a connection to the private bus")
}

/// A UPower at `percent`, owning the name.
async fn upowerd(bus: &Bus, percent: f64) -> Connection {
    let peer = connect(bus).await;
    peer.object_server()
        .at(DISPLAY_DEVICE, Device(percent))
        .await
        .expect("the display device");
    peer.request_name(SERVICE)
        .await
        .expect("the peer owns UPower's name");
    peer
}

async fn next(seen: &mut UnboundedReceiver<Option<Battery>>) -> Option<Battery> {
    tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the watch said something within 5 s")
        .expect("the watch is still running")
}

/// Read until the battery's percentage is `want` (or there is none).
async fn until(seen: &mut UnboundedReceiver<Option<Battery>>, want: Option<f32>) {
    loop {
        if next(seen).await.map(|b| b.percent) == want {
            return;
        }
    }
}

#[tokio::test]
async fn a_upower_absent_at_start_arrives_and_a_restarted_one_is_read_at_once() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; UPower's comings and goings are untested here");
        return;
    };
    let client = connect(&bus).await;
    let (tx, mut seen) = unbounded_channel();
    let task = tokio::spawn(upower::watch(client, tx));

    // No UPower: no chip, and the watch stays.
    assert_eq!(next(&mut seen).await, None);

    // It arrives: the whole battery, in one read.
    let first = upowerd(&bus, 81.0).await;
    loop {
        if let Some(battery) = next(&mut seen).await {
            assert_eq!(
                battery,
                Battery {
                    percent: 81.0,
                    icon: "battery-good-symbolic".to_owned(),
                    state: upower::STATE_DISCHARGING,
                    time_to_empty: 7980,
                    time_to_full: 0,
                }
            );
            break;
        }
    }

    // It leaves: no chip.
    drop(first);
    until(&mut seen, None).await;

    // A restarted upowerd with another reading, which it never announces.
    let _second = upowerd(&bus, 64.0).await;
    until(&mut seen, Some(64.0)).await;
    task.abort();
}

fn value(v: impl Into<Value<'static>>) -> OwnedValue {
    OwnedValue::try_from(v.into()).unwrap()
}

fn props(percent: f64) -> HashMap<String, OwnedValue> {
    HashMap::from([
        ("IsPresent".to_owned(), value(true)),
        ("Percentage".to_owned(), value(percent)),
    ])
}

#[test]
fn a_percentage_that_is_not_a_number_reads_as_zero() {
    // A NaN survives a clamp and would reach the label.
    assert_eq!(upower::battery(&props(f64::NAN)).unwrap().percent, 0.0);
    assert_eq!(upower::battery(&props(f64::INFINITY)).unwrap().percent, 0.0);
    assert_eq!(upower::battery(&props(140.0)).unwrap().percent, 100.0);
    assert_eq!(upower::battery(&props(-3.0)).unwrap().percent, 0.0);
    assert_eq!(upower::battery(&props(77.4)).unwrap().percent, 77.4);
}

#[test]
fn what_get_all_leaves_out_reads_as_unknown() {
    // Only the two that matter: no icon, state or times.
    let battery = upower::battery(&props(50.0)).unwrap();
    assert_eq!(battery.icon, FALLBACK_ICON);
    assert_eq!(
        (battery.state, battery.time_to_empty, battery.time_to_full),
        (STATE_UNKNOWN, 0, 0)
    );
    // A property of another type is as good as absent.
    let mut odd = props(50.0);
    odd.insert("State".to_owned(), value("discharging"));
    assert_eq!(upower::battery(&odd).unwrap().state, STATE_UNKNOWN);
    // No battery: `IsPresent` false, or missing.
    let mut none = props(50.0);
    none.insert("IsPresent".to_owned(), value(false));
    assert_eq!(upower::battery(&none), None);
    assert_eq!(upower::battery(&HashMap::new()), None);
}
