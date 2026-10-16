//! The attention icon, against the real item task on a
//! private `dbus-daemon` (`caustics_sidecar::test_bus`, as `tests/drain.rs`).
//!
//! An item that says `NeedsAttention` and offers an attention icon wears it;
//! one whose attention icon resolves to nothing keeps its normal icon; the
//! same item `Active` wears its normal icon. The names are ones no theme has
//! and the pixmaps are the colour that says which was chosen, so the machine's
//! themes cannot move the answer.

use std::time::Duration;

use caustics_sidecar::test_bus::Bus;
use caustics_tray::Msg;
use caustics_tray::item;
use caustics_tray::model::{Icon, TrayAddress, TrayItem, TrayStatus};
use tokio::sync::mpsc::unbounded_channel;

/// No icon theme has this.
const NOBODY: &str = "caustics-test-nobody-has-this-icon";

/// A 2x2 pixmap of one colour, as `a(iiay)` ARGB32 in network byte order.
fn pixmap(argb: [u8; 4]) -> Vec<(i32, i32, Vec<u8>)> {
    vec![(2, 2, argb.repeat(4))]
}

struct Sni {
    status: &'static str,
    attention_name: &'static str,
    attention: Vec<(i32, i32, Vec<u8>)>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Sni {
    #[zbus(property)]
    fn id(&self) -> String {
        "attention".to_owned()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        "Attention".to_owned()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        self.status.to_owned()
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        NOBODY.to_owned()
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        String::new()
    }
    /// Blue: the normal icon.
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        pixmap([0xff, 0, 0, 0xff])
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> String {
        self.attention_name.to_owned()
    }
    /// Red, when offered: the attention icon.
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        self.attention.clone()
    }
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::try_from("/").unwrap().into()
    }
}

/// The item as the task first reads it, from a peer serving `sni`.
async fn first_read(bus: &Bus, sni: Sni) -> TrayItem {
    let connect = || async {
        zbus::connection::Builder::address(bus.address.as_str())
            .expect("the bus address parses")
            .build()
            .await
            .expect("a connection to the private bus")
    };
    let peer = connect().await;
    peer.object_server()
        .at("/StatusNotifierItem", sni)
        .await
        .expect("the SNI object");
    let client = connect().await;
    let (out, mut msgs) = unbounded_channel();
    let (_calls, calls) = unbounded_channel();
    let unique = peer.unique_name().expect("a unique name").to_string();
    let address = TrayAddress::parse(&format!("{unique}/StatusNotifierItem"));
    let task = tokio::spawn(item::run(client, address, out, calls));
    let item = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = msgs.recv().await {
            if let Msg::Item { item, .. } = msg {
                return Some(*item);
            }
        }
        None
    })
    .await
    .expect("the item task read the item")
    .expect("and sent it");
    task.abort();
    item
}

/// The first texel's RGBA, which is the whole pixmap's.
fn colour(icon: &Icon) -> [u8; 4] {
    match icon {
        Icon::Pixmap { rgba, .. } => [rgba[0], rgba[1], rgba[2], rgba[3]],
        other => panic!("a pixmap, not {other:?}"),
    }
}

const RED: [u8; 4] = [0xff, 0, 0, 0xff];
const BLUE: [u8; 4] = [0, 0, 0xff, 0xff];

#[tokio::test]
async fn needs_attention_wears_the_attention_icon_when_it_resolves() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the attention icon is untested here");
        return;
    };
    // NeedsAttention, an attention pixmap: red.
    let item = first_read(
        &bus,
        Sni {
            status: "NeedsAttention",
            attention_name: NOBODY,
            attention: pixmap([0xff, 0xff, 0, 0]),
        },
    )
    .await;
    assert_eq!(item.status, TrayStatus::NeedsAttention);
    assert_eq!(colour(&item.icon), RED, "the attention icon");

    // NeedsAttention, an attention name nothing has and no pixels: the normal
    // icon stays rather than a placeholder.
    let item = first_read(
        &bus,
        Sni {
            status: "NeedsAttention",
            attention_name: NOBODY,
            attention: Vec::new(),
        },
    )
    .await;
    assert_eq!(
        colour(&item.icon),
        BLUE,
        "an attention icon that resolves to nothing"
    );

    // Active: the attention icon is not read.
    let item = first_read(
        &bus,
        Sni {
            status: "Active",
            attention_name: NOBODY,
            attention: pixmap([0xff, 0xff, 0, 0]),
        },
    )
    .await;
    assert_eq!(colour(&item.icon), BLUE, "Active is the normal icon");
}
