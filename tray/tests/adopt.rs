//! Items the watcher does not list, against a private `dbus-daemon`
//! (`caustics_sidecar::test_bus`, as `tests/drain.rs`).
//!
//! A watcher that took the name from the queue starts with an empty registry,
//! and an item that only re-registers when the name was free in between is
//! then known to nobody (kdeconnect is one such item). A host finds it by asking
//! the bus; and a watcher replaced owner to owner is a watcher gone, so the
//! host decides again and asks again.

use std::time::Duration;

use caustics_sidecar::test_bus::Bus;
use caustics_tray::watcher::{self, WATCHER_NAME, WatcherEvent};
use tokio::sync::mpsc::UnboundedReceiver;
use zbus::fdo::RequestNameFlags;

/// A watcher that knows no item and never will.
struct EmptyWatcher;

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl EmptyWatcher {
    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Sni;

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Sni {
    #[zbus(property)]
    fn id(&self) -> String {
        "unlisted".to_owned()
    }
}

async fn connect(bus: &Bus) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a connection to the private bus")
}

/// A connection serving an empty watcher, asking for the name with `flags`.
async fn empty_watcher(bus: &Bus, flags: RequestNameFlags) -> zbus::Connection {
    let conn = connect(bus).await;
    conn.object_server()
        .at("/StatusNotifierWatcher", EmptyWatcher)
        .await
        .expect("the watcher");
    conn.request_name_with_flags(WATCHER_NAME, flags.into())
        .await
        .expect("the name, or its queue");
    conn
}

async fn next(events: &mut UnboundedReceiver<WatcherEvent>) -> WatcherEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("an event in time")
        .expect("the stream is open")
}

#[tokio::test]
async fn a_host_adopts_an_item_its_watcher_does_not_list() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; adoption is untested here");
        return;
    };
    let _watcher = empty_watcher(&bus, RequestNameFlags::DoNotQueue).await;
    let peer = connect(&bus).await;
    peer.object_server()
        .at("/StatusNotifierItem", Sni)
        .await
        .expect("the SNI object");
    // A bystander with no item: it must not be adopted.
    let _bystander = connect(&bus).await;

    let host = connect(&bus).await;
    let (mut events, _tasks) = watcher::establish(&host).await.expect("a host");

    let unique = peer.unique_name().expect("a unique name");
    match next(&mut events).await {
        WatcherEvent::Registered(address) => {
            assert_eq!(address, format!("{unique}/StatusNotifierItem"));
        }
        other => panic!("the unlisted item, not {other:?}"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .is_err(),
        "and nothing else on the bus is an item"
    );
}

#[tokio::test]
async fn a_watcher_replaced_from_the_queue_is_a_watcher_gone() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the handover is untested here");
        return;
    };
    let first = empty_watcher(&bus, RequestNameFlags::DoNotQueue).await;
    let host = connect(&bus).await;
    let (mut events, _tasks) = watcher::establish(&host).await.expect("a host");

    // Queued behind the first: the name passes owner to owner when it leaves.
    let _second = empty_watcher(&bus, RequestNameFlags::AllowReplacement).await;
    drop(first);

    assert!(matches!(next(&mut events).await, WatcherEvent::WatcherGone));
}
