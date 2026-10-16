//! `com.canonical.dbusmenu`: the layout behind a tray item's `Menu` path.
//!
//! `GetLayout` answers `(u, (ia{sv}av))` — a revision and a tree whose
//! children arrive as *variants*, so the type is recursive through a
//! `VARIANT` and serde cannot derive it. The hand-written [`Layout`]
//! deserialiser reads each child as `(signature, Layout)`, which is exactly
//! what a variant is on the wire, and throws the signature away.
//!
//! Properties are read as a plain `a{sv}` rather than a typed dict: the spec
//! lets a server return properties nobody asked for, real items disagree
//! about which of `enabled` / `visible` / `toggle-state` they bother to send, and every one of them has a documented default. So
//! the map is the wire and [`row`] applies the defaults.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;
use zbus::export::serde;
use zbus::zvariant::serialized::{Context, Data};
use zbus::zvariant::{LE, OwnedValue, Signature, Value};
use zbus::{Connection, zvariant::Type};

use crate::icon::{self, Lookup, Tally};
use crate::model::{Icon, Menu, MenuItem, Toggle};
use crate::witness;
use caustics_sidecar::{Drain, drain};

/// What the sidecar asks each row for. Anything outside this list is a
/// property nothing here has a use for and would only pay to transfer.
pub const WANTED: &[&str] = &[
    "label",
    "enabled",
    "visible",
    "type",
    "toggle-type",
    "toggle-state",
    "icon-name",
    "icon-data",
    "children-display",
];

#[zbus::proxy(interface = "com.canonical.dbusmenu")]
trait DBusMenu {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, Layout)>;

    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;

    fn event(&self, id: i32, event_id: &str, data: &Value<'_>, timestamp: u32) -> zbus::Result<()>;

    /// The menu's own icon directories (the dbusmenu spec's `as`), which a
    /// row's `icon-name` may live in.
    #[zbus(property)]
    fn icon_theme_path(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn layout_updated(&self, revision: u32, parent: i32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn items_properties_updated(
        &self,
        updated: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;
}

/// One tray item's menu: the proxy, and the two drains that read the streams
/// saying the layout moved. Held by the item's task, which is the only thing
/// that touches it; dropping it stops both drains.
///
/// The streams are **not** here: the item task may await
/// a large `GetLayout` reply while an item rewrites a long menu in a burst, and a stream it
/// held across that call fills at 64 signals and parks zbus's socket reader
/// with the reply behind it.
pub struct MenuWatch {
    proxy: DBusMenuProxy<'static>,
    _changes: [Drain; 2],
}

/// Watch the menu at `path`: `changed` is notified whenever the server says
/// the layout or a row's properties moved.
pub async fn watch(
    conn: &Connection,
    bus_name: &str,
    path: &str,
    changed: &Arc<Notify>,
) -> zbus::Result<MenuWatch> {
    let proxy = DBusMenuProxy::builder(conn)
        .destination(bus_name.to_owned())?
        .path(path.to_owned())?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    // Each drained the moment it exists: the second subscription is a bus
    // call, and the first stream must not wait unpolled through it.
    let layout = drain(proxy.receive_layout_updated().await?, changed.clone());
    let properties = drain(
        proxy.receive_items_properties_updated().await?,
        changed.clone(),
    );
    Ok(MenuWatch {
        proxy,
        _changes: [layout, properties],
    })
}

impl MenuWatch {
    /// The whole tree, root's children first. Depth -1 is all of it: menus
    /// this size (tailscale's is 143 rows) arrive in one round trip, and a
    /// lazy per-level fetch would put a D-Bus call on the popup's open path —
    /// and the bar opens the menu from rows it already holds, so a lazy fetch would add a round trip to every open (docs/abi.md, *Rows*).
    ///
    /// Each row's icon goes down the chain in [`crate::icon`], against the
    /// item's own directory (`item_dir`, its `IconThemePath`) and the menu's
    /// own `IconThemePath` — read here, on every fetch, because nothing
    /// announces that it changed. The [`Tally`] is for the fetch's witness.
    pub async fn layout(&self, item_dir: Option<&str>) -> zbus::Result<(Menu, Tally)> {
        let (revision, root) = self.proxy.get_layout(0, -1, WANTED).await?;
        let own = self.proxy.icon_theme_path().await.unwrap_or_default();
        let dirs = Lookup::dirs_of(item_dir.into_iter().chain(own.iter().map(String::as_str)));
        let lookup = Lookup::live(dirs);
        let mut tally = Tally::default();
        let items = rows(&root, &lookup, &mut tally);
        Ok((Menu { revision, items }, tally))
    }

    /// `AboutToShow(id)` — the row's **parent**, which for a root-level row is
    /// the root itself, 0. `true` means the server changed the layout for us
    /// and the caller should refetch.
    pub async fn about_to_show(&self, id: i32) -> zbus::Result<bool> {
        self.proxy.about_to_show(id).await
    }

    pub async fn clicked(&self, id: i32) -> zbus::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        self.proxy.event(id, "clicked", &Value::I32(0), now).await
    }
}

// ---------------------------------------------------------------------------
// The recursive layout
// ---------------------------------------------------------------------------

#[derive(Debug, Type)]
#[zvariant(signature = "(ia{sv}av)")]
pub struct Layout {
    id: i32,
    props: HashMap<String, OwnedValue>,
    children: Vec<Layout>,
}

impl<'de> serde::Deserialize<'de> for Layout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A child arrives as a variant: its signature, then its value. Both
        // halves have to be read for the cursor to land in the right place;
        // only the second is worth keeping.
        type Wire = (i32, HashMap<String, OwnedValue>, Vec<(Signature, Layout)>);
        let (id, props, children) = <Wire as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Layout {
            id,
            props,
            children: children.into_iter().map(|(_, c)| c).collect(),
        })
    }
}

/// A raw `GetLayout(0, -1, WANTED)` **reply body** as the menu it means, its
/// icons resolved against `lookup`.
///
/// The one door the tests come through, so that nothing under `tray/tests/`
/// needs a zbus type — and so that what they exercise is the real
/// deserialiser against real recorded bytes, which is where a hand-written
/// `(ia{sv}av)` reader goes wrong.
pub fn parse_reply(body: &[u8], lookup: &Lookup) -> Result<(Menu, Tally), String> {
    // A reply body from a little-endian peer, read from its first byte.
    let data = Data::new(body, Context::new_dbus(LE, 0));
    let ((revision, root), _) = data
        .deserialize::<(u32, Layout)>()
        .map_err(|e| format!("{e}"))?;
    let mut tally = Tally::default();
    let items = rows(&root, lookup, &mut tally);
    Ok((Menu { revision, items }, tally))
}

/// A layout's children as the model's rows. The root itself is a container
/// nothing draws, so only its children become items.
pub fn rows(root: &Layout, lookup: &Lookup, tally: &mut Tally) -> Vec<MenuItem> {
    root.children
        .iter()
        .map(|child| row(child, lookup, tally))
        .collect()
}

/// One wire row to one [`MenuItem`], with the dbusmenu spec's defaults for
/// everything the server left out (`enabled` and `visible` default to true,
/// `type` to "standard", `toggle-state` to -1 meaning indeterminate).
fn row(layout: &Layout, lookup: &Lookup, tally: &mut Tally) -> MenuItem {
    let props = &layout.props;
    let separator = string(props, "type").is_some_and(|t| t == "separator");
    let toggle = match string(props, "toggle-type") {
        Some("checkmark") => Toggle::Checkmark(integer(props, "toggle-state") == Some(1)),
        Some("radio") => Toggle::Radio(integer(props, "toggle-state") == Some(1)),
        _ => Toggle::None,
    };
    MenuItem {
        id: layout.id,
        label: string(props, "label")
            .map(strip_mnemonic)
            .unwrap_or_default(),
        enabled: boolean(props, "enabled").unwrap_or(true),
        visible: boolean(props, "visible").unwrap_or(true),
        separator,
        toggle,
        icon: icon(props, lookup, tally),
        children: layout
            .children
            .iter()
            .map(|child| row(child, lookup, tally))
            .collect(),
    }
}

/// A row's icon, down the chain in [`crate::icon`]: the name if a theme or the
/// item's own directory has it, else the inline image — decoded here so
/// nothing downstream needs an image decoder — else the name anyway.
///
/// kdeconnect sends both on every row, and names Breeze icons
/// (`smartphonedisconnected`, `klipper`) that few installed themes have; were
/// the name to win regardless, the row would draw nothing.
fn icon(props: &HashMap<String, OwnedValue>, lookup: &Lookup, tally: &mut Tally) -> Icon {
    let name = string(props, "icon-name").unwrap_or_default();
    if let Some((icon, source)) = icon::by_name(lookup, name) {
        tally.count(name, &source);
        return icon;
    }
    let (icon, source) = icon::after_name(lookup, name, inline(props));
    tally.count(name, &source);
    icon
}

/// The row's `icon-data`, decoded, or `None` with a witness when it is there
/// and is no format this knows.
fn inline(props: &HashMap<String, OwnedValue>) -> Option<Icon> {
    let data = bytes(props, "icon-data")?;
    match decode(&data) {
        Some((width, height, rgba)) => Some(Icon::Pixmap {
            width,
            height,
            rgba,
        }),
        None => {
            witness(&format!(
                "icon-data in an unknown format ({} bytes, starts {:02x?})",
                data.len(),
                &data[..data.len().min(4)],
            ));
            None
        }
    }
}

/// `icon-data` is a bare encoded image with no format field. The spec says
/// PNG; the live bus disagrees — kdeconnect sends PNG, tailscale sends JPEG —
/// so sniff the magic and decode both. Anything else is a witness and no icon.
pub fn decode(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        decode_png(data)
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        decode_jpeg(data)
    } else {
        None
    }
}

fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    // Palettes expanded, 16-bit channels dropped to 8, tRNS turned into a real
    // alpha channel: what is left is one of the four cases below.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let pixels = (info.width as usize).checked_mul(info.height as usize)?;
    let rgba = match info.color_type {
        png::ColorType::Rgba => {
            buf.truncate(pixels * 4);
            buf
        }
        png::ColorType::Rgb => widen(&buf, pixels, 3, |px| [px[0], px[1], px[2], 0xff]),
        png::ColorType::GrayscaleAlpha => widen(&buf, pixels, 2, |px| [px[0], px[0], px[0], px[1]]),
        png::ColorType::Grayscale => widen(&buf, pixels, 1, |px| [px[0], px[0], px[0], 0xff]),
        png::ColorType::Indexed => return None,
    };
    (rgba.len() == pixels * 4).then_some((info.width, info.height, rgba))
}

fn widen(buf: &[u8], pixels: usize, stride: usize, to_rgba: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels * 4);
    for px in buf.chunks_exact(stride).take(pixels) {
        out.extend_from_slice(&to_rgba(px));
    }
    out
}

fn decode_jpeg(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(data), options);
    let rgba = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (width, height) = (info.width as u32, info.height as u32);
    let pixels = (width as usize).checked_mul(height as usize)?;
    (rgba.len() == pixels * 4).then_some((width, height, rgba))
}

/// GTK's underscore mnemonics: `_x` marks `x` as the accelerator and `__` is a
/// literal underscore. Neither belongs in a label the bar draws.
pub fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        match c {
            '_' => match chars.next() {
                Some(next) => out.push(next),
                None => break,
            },
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reading one property out of the wire map
// ---------------------------------------------------------------------------

fn string<'a>(props: &'a HashMap<String, OwnedValue>, key: &str) -> Option<&'a str> {
    match &**props.get(key)? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

fn boolean(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match &**props.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn integer(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    match &**props.get(key)? {
        Value::I32(i) => Some(*i),
        _ => None,
    }
}

fn bytes(props: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<u8>> {
    match &**props.get(key)? {
        Value::Array(array) => array
            .inner()
            .iter()
            .map(|v| match v {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}
