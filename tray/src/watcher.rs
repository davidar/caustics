//! `org.kde.StatusNotifierWatcher`: own it, or host on whoever has it
//! (the StatusNotifierItem specification's registry).
//!
//! This sidecar is a **Host** when someone else already owns the name — which
//! is the case beside another panel, such as cosmic-panel's status area —
//! and the **owner** when the name is free. Both modes reduce to the same
//! thing for everything downstream: a stream of [`WatcherEvent`] saying which
//! item addresses exist. A host that loses its watcher retries the whole
//! decision after [`RETRY`], so the tray survives that panel restarting
//! under it.
//!
//! A host does not take the watcher's registry as the whole truth. A watcher
//! that received the name from a queue (the name passed owner to owner, never
//! free) starts with an empty list, and Qt's items re-register only when the
//! name goes from free to owned — so they sit on the bus, alive, known to
//! nobody (kdeconnect does this after every restart of a bar that had been
//! the watcher). [`adopt`] asks every connection the watcher does not list
//! whether it has an item at the conventional path.
//!
//! Which mode it is in is **entirely this process's business**: nothing about
//! it crosses the wire, and the bar cannot tell.

use std::time::Duration;

use futures_lite::StreamExt as _;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::names::{BusName, WellKnownName};
use zbus::object_server::SignalEmitter;
use zbus::{Connection, message::Header};

use crate::witness;

pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// Where an item lives when it registers by bus name alone.
const ITEM_PATH: &str = "/StatusNotifierItem";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";

/// How long a connection has to say whether it is an item. Most answer
/// `UnknownObject` at once; one that reads nothing never answers.
const PROBE: Duration = Duration::from_secs(2);

/// How long to wait before re-running the own-or-host decision after the
/// watcher we were hosting on went away.
pub const RETRY: Duration = Duration::from_secs(1);

#[zbus::proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait Watcher {
    fn register_status_notifier_host(&self, service: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn status_notifier_item_registered(&self, service: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn status_notifier_item_unregistered(&self, service: String) -> zbus::Result<()>;
}

/// The watcher we serve when the name is free. It keeps the registry the
/// interface is contractually required to expose; this process's own view of
/// the items comes from the same events, through `sink`.
struct StatusNotifierWatcher {
    items: Vec<String>,
    sink: UnboundedSender<WatcherEvent>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl StatusNotifierWatcher {
    async fn register_status_notifier_item(
        &mut self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        // Two calling conventions in the wild: the service name, or the object
        // path with the sender implied. Normalise to the address form the
        // watcher's property uses.
        let address = match (service.starts_with('/'), header.sender()) {
            (true, Some(sender)) => format!("{sender}{service}"),
            (true, None) => return,
            (false, _) => service.to_owned(),
        };
        if self.items.iter().any(|s| *s == address) {
            return;
        }
        witness(&format!("item registered with our watcher: {address}"));
        self.items.push(address.clone());
        let _ = Self::status_notifier_item_registered(&emitter, &address).await;
        let _ = self.sink.send(WatcherEvent::Registered(address));
    }

    async fn register_status_notifier_host(
        &mut self,
        service: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        witness(&format!("host registered with our watcher: {service}"));
        let _ = Self::status_notifier_host_registered(&emitter).await;
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[derive(Debug)]
pub enum WatcherEvent {
    Registered(String),
    Unregistered(String),
    /// A unique bus name lost its owner: every item it published is gone.
    OwnerGone(String),
    /// Only in host mode: the watcher we registered with died.
    WatcherGone,
}

/// Everything one own-or-host attempt spawned. Dropping it stops those tasks
/// and leaves the item tasks — which outlive a watcher restart — alone.
pub struct Tasks(Vec<JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Own the watcher name or register as a host on the one that has it, and
/// return the event stream either decision produces.
pub async fn establish(
    conn: &Connection,
) -> zbus::Result<(UnboundedReceiver<WatcherEvent>, Tasks)> {
    let (sink, events) = unbounded_channel();
    let dbus = DBusProxy::new(conn).await?;
    let name = WellKnownName::try_from(WATCHER_NAME)?;

    // DoNotQueue alone: never steal the name from a watcher that has it.
    let reply = dbus
        .request_name(name.clone(), RequestNameFlags::DoNotQueue.into())
        .await?;
    let mut tasks = Vec::new();

    if reply == RequestNameReply::PrimaryOwner {
        witness(&format!("{WATCHER_NAME} was free; serving it"));
        own(conn, &dbus, sink.clone()).await?;
    } else {
        witness(&format!(
            "{WATCHER_NAME} is taken ({reply:?}); registering as a host"
        ));
        tasks.extend(host(conn, sink.clone()).await?);
    }

    tasks.push(watch_owners(&dbus, sink).await?);
    Ok((events, Tasks(tasks)))
}

/// Serve the watcher interface, then sweep the bus for items that registered
/// with whatever watcher held the name before us.
async fn own(
    conn: &Connection,
    dbus: &DBusProxy<'_>,
    sink: UnboundedSender<WatcherEvent>,
) -> zbus::Result<()> {
    let watcher = StatusNotifierWatcher {
        items: Vec::new(),
        sink: sink.clone(),
    };
    conn.object_server().at(WATCHER_PATH, watcher).await?;

    for name in dbus.list_names().await? {
        let name = name.as_str();
        if !name.contains("StatusNotifierItem") {
            continue;
        }
        let Ok(owner) = dbus
            .get_name_owner(BusName::try_from(name.to_owned())?)
            .await
        else {
            continue;
        };
        let address = format!("{owner}/StatusNotifierItem");
        witness(&format!("found {address} on the bus ({name})"));
        let _ = sink.send(WatcherEvent::Registered(address));
    }
    Ok(())
}

/// Register as a host on someone else's watcher and mirror its registry.
///
/// Each signal stream is forwarded by a task of its own the moment it is
/// subscribed, **before** the bus calls that follow:
/// a stream held unpolled across a call can fill and stop zbus's reader with
/// that call's reply behind it.
async fn host(
    conn: &Connection,
    sink: UnboundedSender<WatcherEvent>,
) -> zbus::Result<[JoinHandle<()>; 2]> {
    let watcher = WatcherProxy::new(conn).await?;

    // The spec wants a host to own a `org.kde.StatusNotifierHost-<pid>` name
    // and hand *that* to the watcher; some watchers check. cosmic-panel's does
    // not, so failing to own it is a witness and the unique name is the
    // fallback.
    let host_name = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    // zbus warns when a name is requested before the object server exists,
    // since method calls could arrive with nothing to answer them. A host
    // serves no interface — the name is the whole registration — but the
    // warning is a line in every start's journal, and the cure is to have the
    // (empty) object server exist first.
    let _ = conn.object_server();
    let service = match conn.request_name(host_name.as_str()).await {
        Ok(()) => host_name,
        Err(e) => {
            witness(&format!(
                "could not own {host_name} ({e}); using the unique name"
            ));
            conn.unique_name()
                .map(|n| n.to_string())
                .unwrap_or_default()
        }
    };

    // Subscribe before reading the property, so an item that appears between
    // the two is not lost. A registration seen twice — by signal and by the
    // property — is one item: `State::watcher_event` keys tasks by address.
    let registered = forward(
        watcher.receive_status_notifier_item_registered().await?,
        sink.clone(),
        |signal| {
            signal
                .args()
                .ok()
                .map(|args| WatcherEvent::Registered(args.service.clone()))
        },
    );
    let unregistered = forward(
        watcher.receive_status_notifier_item_unregistered().await?,
        sink.clone(),
        |signal| {
            signal
                .args()
                .ok()
                .map(|args| WatcherEvent::Unregistered(args.service.clone()))
        },
    );

    watcher.register_status_notifier_host(&service).await?;
    witness(&format!("registered as host {service}"));

    let known = watcher.registered_status_notifier_items().await?;
    for address in &known {
        witness(&format!("watcher knows {address}"));
        let _ = sink.send(WatcherEvent::Registered(address.clone()));
    }
    tokio::spawn(adopt(conn.clone(), known, sink));

    Ok([registered, unregistered])
}

/// Find the items the watcher does not list: every connection on the bus the
/// registry does not name is asked for the `Id` of an item at [`ITEM_PATH`],
/// all at once, and one that answers is an item. Its end is `OwnerGone`, like
/// any other's; one that registers later as well is the same address and so
/// the same task.
async fn adopt(conn: Connection, known: Vec<String>, sink: UnboundedSender<WatcherEvent>) {
    let Ok(dbus) = DBusProxy::new(&conn).await else {
        return;
    };
    let Ok(names) = dbus.list_names().await else {
        return;
    };
    let ours = conn.unique_name().map(|n| n.to_string());
    let listed = |name: &str| {
        known
            .iter()
            .any(|address| address.split('/').next() == Some(name))
    };

    let probes: Vec<_> = names
        .iter()
        .map(|name| name.as_str().to_owned())
        .filter(|name| name.starts_with(':') && ours.as_deref() != Some(name) && !listed(name))
        .map(|name| {
            let conn = conn.clone();
            tokio::spawn(async move {
                let call = conn.call_method(
                    Some(name.as_str()),
                    ITEM_PATH,
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(ITEM_INTERFACE, "Id"),
                );
                matches!(tokio::time::timeout(PROBE, call).await, Ok(Ok(_))).then_some(name)
            })
        })
        .collect();

    for probe in probes {
        if let Ok(Some(name)) = probe.await {
            let address = format!("{name}{ITEM_PATH}");
            witness(&format!(
                "found {address} on the bus, unknown to the watcher"
            ));
            let _ = sink.send(WatcherEvent::Registered(address));
        }
    }
}

/// Read one watcher signal stream to its end, translating each signal into an
/// event on the unbounded sink — awaiting nothing but the stream, so its
/// channel never fills. The stream ending is the watcher going away.
fn forward<S, F>(mut stream: S, sink: UnboundedSender<WatcherEvent>, event: F) -> JoinHandle<()>
where
    S: futures_lite::Stream + Send + Unpin + 'static,
    S::Item: Send,
    F: Fn(S::Item) -> Option<WatcherEvent> + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(signal) = stream.next().await {
            if let Some(event) = event(signal) {
                let _ = sink.send(event);
            }
        }
        let _ = sink.send(WatcherEvent::WatcherGone);
    })
}

/// `NameOwnerChanged`, which is how an item that crashed is noticed and how a
/// host learns its watcher died — or was replaced by a queued owner: the name never becomes free, so items do not re-register, and the new watcher starts with an empty registry. Spawned in the same breath as the
/// subscription, with no await between: the stream is never unpolled.
async fn watch_owners(
    dbus: &DBusProxy<'_>,
    sink: UnboundedSender<WatcherEvent>,
) -> zbus::Result<JoinHandle<()>> {
    let mut changes = dbus.receive_name_owner_changed().await?;
    let ours = dbus
        .inner()
        .connection()
        .unique_name()
        .map(|n| n.to_string());
    Ok(tokio::spawn(async move {
        while let Some(signal) = changes.next().await {
            let Ok(args) = signal.args() else { continue };
            let name = args.name.as_str();
            if let Some(new) = args.new_owner.as_ref() {
                if name == WATCHER_NAME
                    && args.old_owner.is_some()
                    && ours.as_deref() != Some(new.as_str())
                {
                    let _ = sink.send(WatcherEvent::WatcherGone);
                }
                continue;
            }
            if name == WATCHER_NAME {
                let _ = sink.send(WatcherEvent::WatcherGone);
            } else if name.starts_with(':') {
                let _ = sink.send(WatcherEvent::OwnerGone(name.to_owned()));
            }
        }
    }))
}
