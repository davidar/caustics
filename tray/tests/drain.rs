//! The tray must never stop reading its signals.
//!
//! zbus's socket reader hands every message to every `MessageStream`'s
//! channel in turn and **awaits** room in each (capacity 64, overflow off).
//! A stream nobody polls fills after 64 matching signals and the reader parks
//! on it — reading nothing further for anyone, method replies included. When an item rewrites a long menu in a burst while the item task is inside a `GetLayout`, the menu's streams fill, the reply sits unread in the kernel, and the tray never changes again.
//!
//! The first test drives the real item task against a peer whose `GetLayout`
//! emits 200 `LayoutUpdated` signals *before* it replies, over a **private
//! `dbus-daemon`** the test starts and kills. Not a peer-to-peer pair:
//! zbus's `p2p` feature is off in this workspace and
//! turning it on brings `uuid` in, and a p2p message carries no sender, so the
//! item's match rule on the peer's unique name would never match a signal
//! anyway. With no `dbus-daemon` on the machine the test says so and passes.
//! The bus is `caustics_sidecar::test_bus`, and the drain's own unit test
//! is in `caustics-sidecar` with the drain.

use std::collections::HashMap;
use std::time::Duration;

use caustics_sidecar::test_bus::Bus;
use caustics_tray::Msg;
use caustics_tray::item;
use caustics_tray::model::TrayAddress;
use tokio::sync::mpsc::unbounded_channel;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};

/// More than the 64 zbus queues per stream, by a margin.
const BURST: u32 = 200;

/// The SNI half of the peer: a themed icon (so no pixmap is read) and a menu.
struct Sni;

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Sni {
    #[zbus(property)]
    fn id(&self) -> String {
        "burst".to_owned()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        "A menu that rewrites itself".to_owned()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        "Active".to_owned()
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        "network-vpn-symbolic".to_owned()
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::try_from("/Menu")
            .unwrap()
            .into()
    }
}

/// The dbusmenu half: tailscale's burst, compressed into one call.
struct BurstyMenu;

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl BurstyMenu {
    async fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> (u32, Layout) {
        // Every one of these matches the item's `LayoutUpdated` stream, and
        // every one reaches the client before the reply below does.
        for revision in 0..BURST {
            let _ = Self::layout_updated(&emitter, revision, 0).await;
        }
        let mut child = HashMap::new();
        child.insert(
            "label".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from("Exit node")),
        );
        let child: Value<'_> = StructureBuilder::new()
            .add_field(1i32)
            .add_field(child)
            .add_field(Vec::<OwnedValue>::new())
            .build()
            .expect("a layout row")
            .into();
        (
            BURST,
            (
                0,
                HashMap::new(),
                vec![child.try_to_owned().expect("owned")],
            ),
        )
    }

    async fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    async fn event(&self, _id: i32, _event_id: String, _data: OwnedValue, _timestamp: u32) {}

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;
}

/// The item task, against the bursty peer, must receive the `GetLayout`
/// reply and send its menu on. If a stream were held unpolled in the task this would time out:
/// the reader parks on the menu's full `LayoutUpdated` stream with the reply behind it.
#[tokio::test]
async fn a_signal_burst_during_get_layout_does_not_stop_the_reply() {
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
    peer.object_server()
        .at("/StatusNotifierItem", Sni)
        .await
        .expect("the SNI object");
    peer.object_server()
        .at("/Menu", BurstyMenu)
        .await
        .expect("the menu object");
    let client = connect().await;

    let (out, mut msgs) = unbounded_channel();
    let (_calls, calls) = unbounded_channel();
    // Addressed the way the watcher hands tailscale out: its unique name.
    let unique = peer.unique_name().expect("a unique name").to_string();
    let address = TrayAddress::parse(&format!("{unique}/StatusNotifierItem"));
    let task = tokio::spawn(item::run(client, address, out, calls));

    let menu = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = msgs.recv().await {
            if let Msg::Menu { menu, .. } = msg {
                return Some(menu);
            }
        }
        None
    })
    .await
    .expect("the GetLayout reply arrived despite 200 signals ahead of it")
    .expect("the item task sent its menu");
    assert_eq!(menu.revision, BURST);
    assert_eq!(menu.items.len(), 1);
    assert_eq!(menu.items[0].label, "Exit node");
    task.abort();
}
