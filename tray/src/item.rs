//! One StatusNotifierItem, one tokio task.
//!
//! The task owns that item's proxy and its dbusmenu, and it is the only thing
//! that talks to that service — so an item that hangs or errors costs exactly
//! itself, which is the reason the tray is its own sidecar rather than part of another sidecar. It is aborted when the item unregisters or its bus name
//! vanishes.
//!
//! **It never holds a signal stream.** The six `New*` streams and the menu's
//! two are each read by a [`drain`] task (the one in `caustics-sidecar`) that
//! awaits nothing else and folds what it reads into a `Notify`; the item task
//! awaits the `Notify`s. zbus's socket reader blocks on any stream whose
//! channel is full, so a stream left unpolled across a bus call is a
//! connection that can stop reading: an item that rewrites a long menu in a
//! burst while the task waits on a reply freezes the whole tray
//! (docs/architecture.md, *Rules*).

// `Pixmap` stays qualified: the SNI struct below owns that name here.
use std::sync::Arc;
use tiny_skia::{ColorU8, FilterQuality, PixmapPaint, Transform};

use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

use caustics_sidecar::{Drain, drain};

use crate::icon::{self, Lookup, Source};
use crate::menu::{self, MenuWatch};
use crate::model::{Icon, TrayAddress, TrayItem, TrayStatus};
use crate::rows::Call;
use crate::{Msg, witness};

/// The square every pixmap crosses the wire at, in a `Pixmap` frame of its own
/// (docs/abi.md, *Pixmap*): the bar's image slot.
///
/// SNI items export whatever size they felt like — tailscale's only icon is
/// 250×250, a quarter of a megabyte as it arrives — and the bar
/// draws them at `ICON_LOGICAL` (20 logical px in `caustics/bar_tunables.slang`)
/// times an output scale of at most 3, so 60 px is the most it can ever want.
///
/// It is the whole square, not "the longest side": the bar copies a pixmap
/// into a fixed slot and fits nothing, so a pixmap that is not the slot's own
/// square is one the bar cannot draw. A square is 16 KiB whatever is in it,
/// which is what the bar holds anyway, and the resample happens once, here.
pub const WIRE_PIXMAP: u32 = caustics_bar_abi::IMAGE_SLOT_PX;

/// One `a(iiay)` entry: ARGB32 in network byte order, as the SNI spec has it.
#[derive(Clone, Debug, zbus::zvariant::Value)]
pub struct Pixmap {
    pub width: i32,
    pub height: i32,
    pub argb: Vec<u8>,
}

#[zbus::proxy(interface = "org.kde.StatusNotifierItem")]
trait Item {
    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;
    fn secondary_activate(&self, x: i32, y: i32) -> zbus::Result<()>;

    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn title(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_theme_path(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_pixmap(&self) -> zbus::Result<Vec<Pixmap>>;
    #[zbus(property)]
    fn attention_icon_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> zbus::Result<Vec<Pixmap>>;
    #[zbus(property)]
    fn menu(&self) -> zbus::Result<OwnedObjectPath>;

    #[zbus(signal)]
    fn new_title(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_icon(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_attention_icon(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_status(&self, status: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_icon_theme_path(&self, path: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_menu(&self) -> zbus::Result<()>;
}

pub async fn run(
    conn: Connection,
    address: TrayAddress,
    out: UnboundedSender<Msg>,
    mut calls: UnboundedReceiver<Call>,
) {
    let key = address.key();
    let proxy = match proxy_for(&conn, &address).await {
        Ok(proxy) => proxy,
        Err(e) => return fail(&out, &key, e),
    };

    // Subscribe before the first read: a change during startup then leaves a
    // permit rather than being missed. Any of the six `New*` signals means
    // the same thing — re-read the item — so they fold into one `Notify`.
    let item_changed = Arc::new(Notify::new());
    let _signals = match signals(&proxy, &item_changed).await {
        Ok(drains) => drains,
        Err(e) => return fail(&out, &key, e),
    };
    // The menu's two streams fold into this one, whichever menu path the item
    // has at the time; a permit left by a menu that was replaced costs one
    // spare `GetLayout`.
    let menu_changed = Arc::new(Notify::new());

    let mut menu_path = String::new();
    let mut menu: Option<MenuWatch> = None;
    // The item's `IconThemePath`, as the last refresh read it: a menu row's
    // icon may live there too.
    let mut own_dir: Option<String> = None;
    refresh(
        &proxy,
        &key,
        &out,
        &mut menu_path,
        &mut menu,
        &menu_changed,
        &mut own_dir,
    )
    .await;

    loop {
        tokio::select! {
            call = calls.recv() => match call {
                None => break,
                Some(call) => act(&proxy, &key, &out, menu.as_ref(), call, own_dir.as_deref()).await,
            },
            _ = item_changed.notified() => {
                refresh(&proxy, &key, &out, &mut menu_path, &mut menu, &menu_changed, &mut own_dir)
                    .await;
            }
            _ = menu_changed.notified() => {
                if let Some(menu) = menu.as_ref() {
                    fetch_menu(menu, &key, &out, own_dir.as_deref()).await;
                }
            }
        }
    }
    witness(&format!("{key}: item task finished"));
}

async fn proxy_for(conn: &Connection, address: &TrayAddress) -> zbus::Result<ItemProxy<'static>> {
    ItemProxy::builder(conn)
        .destination(address.bus_name.clone())?
        .path(address.path.clone())?
        // Explicit reads, never a cache: SNI items announce changes with their
        // own `New*` signals and are not obliged to emit `PropertiesChanged`,
        // so a cache would go stale without ever saying so.
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

/// The six `New*` streams, each handed to a [`drain`] **as soon as it
/// exists** — a subscription is a bus call, and a stream waiting unpolled for
/// the next one to be made is the hazard this module is shaped against.
/// `NewAttentionIcon` is the sixth.
async fn signals(proxy: &ItemProxy<'_>, changed: &Arc<Notify>) -> zbus::Result<[Drain; 6]> {
    Ok([
        drain(proxy.receive_new_title().await?, changed.clone()),
        drain(proxy.receive_new_icon().await?, changed.clone()),
        drain(proxy.receive_new_attention_icon().await?, changed.clone()),
        drain(proxy.receive_new_status().await?, changed.clone()),
        drain(proxy.receive_new_icon_theme_path().await?, changed.clone()),
        drain(proxy.receive_new_menu().await?, changed.clone()),
    ])
}

fn fail(out: &UnboundedSender<Msg>, key: &str, e: impl std::fmt::Display) {
    witness(&format!("{key}: could not be proxied: {e}"));
    let _ = out.send(Msg::Gone(key.to_owned()));
}

/// Re-read every property and send the whole item. The frame is whole-state
/// and republished only when it changed, so an over-eager signal costs the
/// round trips and nothing else.
async fn refresh(
    proxy: &ItemProxy<'_>,
    key: &str,
    out: &UnboundedSender<Msg>,
    menu_path: &mut String,
    menu: &mut Option<MenuWatch>,
    menu_changed: &Arc<Notify>,
    own_dir: &mut Option<String>,
) {
    // `Title` is often empty on ayatana items; `Id` is what those show.
    let title = match proxy.title().await {
        Ok(title) if !title.is_empty() => title,
        _ => proxy.id().await.unwrap_or_default(),
    };
    let status = match proxy.status().await.as_deref() {
        Ok("Active") => TrayStatus::Active,
        Ok("NeedsAttention") => TrayStatus::NeedsAttention,
        _ => TrayStatus::Passive,
    };
    *own_dir = proxy.icon_theme_path().await.ok().filter(|p| !p.is_empty());
    let lookup = Lookup::live(Lookup::dirs_of(own_dir.as_deref()));
    // `NeedsAttention` with an attention icon that resolves: that is the icon
    // ([`icon::attention`]). One that resolves to nothing does not replace
    // a normal icon that draws.
    let attention = match status {
        TrayStatus::NeedsAttention => attention_of(proxy, &lookup).await,
        _ => None,
    };
    let attending = if attention.is_some() {
        "attention "
    } else {
        ""
    };
    let (icon, source) = match attention {
        Some(found) => found,
        None => icon_of(proxy, &lookup).await,
    };
    let path = proxy
        .menu()
        .await
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    let has_menu = !path.is_empty() && path != "/";

    witness(&format!(
        "{key} is {title:?} {status:?} {attending}{} from {} menu={}",
        describe(&icon),
        source.describe(),
        if has_menu { path.as_str() } else { "none" },
    ));
    let sent = out.send(Msg::Item {
        key: key.to_owned(),
        item: Box::new(TrayItem {
            address: TrayAddress::parse(key),
            title,
            icon,
            status,
            has_menu,
        }),
    });
    if sent.is_err() {
        return;
    }

    if path == *menu_path {
        return;
    }
    *menu_path = path.clone();
    *menu = None;
    if !has_menu {
        let _ = out.send(Msg::MenuGone(key.to_owned()));
        return;
    }
    let address = TrayAddress::parse(key);
    match menu::watch(
        proxy.inner().connection(),
        &address.bus_name,
        &path,
        menu_changed,
    )
    .await
    {
        Ok(watch) => {
            fetch_menu(&watch, key, out, own_dir.as_deref()).await;
            *menu = Some(watch);
        }
        Err(e) => witness(&format!("{key}: has no usable menu: {e}")),
    }
}

async fn fetch_menu(
    watch: &MenuWatch,
    key: &str,
    out: &UnboundedSender<Msg>,
    own_dir: Option<&str>,
) {
    match watch.layout(own_dir).await {
        Ok((menu, tally)) => {
            witness(&format!(
                "{key}: menu revision {} with {} rows; {}",
                menu.revision,
                menu.items.len(),
                tally.describe(),
            ));
            let _ = out.send(Msg::Menu {
                key: key.to_owned(),
                menu: Box::new(menu),
            });
        }
        Err(e) => witness(&format!("{key}: GetLayout failed: {e}")),
    }
}

/// The one place a decision the caustic already made becomes a method call.
async fn act(
    proxy: &ItemProxy<'_>,
    key: &str,
    out: &UnboundedSender<Msg>,
    menu: Option<&MenuWatch>,
    call: Call,
    own_dir: Option<&str>,
) {
    match call {
        Call::Activate { x, y } => match proxy.activate(x, y).await {
            Ok(()) => witness(&format!("{key}: Activate({x}, {y}): ok")),
            Err(e) => witness(&format!("{key}: Activate({x}, {y}): {e}")),
        },
        Call::SecondaryActivate { x, y } => match proxy.secondary_activate(x, y).await {
            Ok(()) => witness(&format!("{key}: SecondaryActivate({x}, {y}): ok")),
            Err(e) => witness(&format!("{key}: SecondaryActivate({x}, {y}): {e}")),
        },
        Call::MenuClick { id, parent } => {
            let Some(menu) = menu else {
                witness(&format!("{key}: has no menu for row {id}"));
                return;
            };
            // `AboutToShow(parent)` first, as dbusmenu clients do before showing a menu: the server takes it as notice that this submenu is being
            // used, and `true` means it moved the layout under us.
            let moved = match menu.about_to_show(parent).await {
                Ok(moved) => moved,
                Err(e) => {
                    witness(&format!("{key}: AboutToShow({parent}): {e}"));
                    false
                }
            };
            match menu.clicked(id).await {
                Ok(()) => witness(&format!("{key}: Event({id}, clicked): ok")),
                Err(e) => witness(&format!("{key}: Event({id}, clicked): {e}")),
            }
            if moved {
                witness(&format!(
                    "{key}: AboutToShow({parent}) says the layout moved"
                ));
                fetch_menu(menu, key, out, own_dir).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

/// The item's icon, down the chain in [`crate::icon`]:
/// the name if a theme or the item's own directory has it, else the pixmap
/// it exports (tailscale ships only this; Steam both), else the name anyway.
/// The pixmap is a bus call, so it is read only when the name missed.
async fn icon_of(proxy: &ItemProxy<'_>, lookup: &Lookup) -> (Icon, Source) {
    let name = proxy.icon_name().await.unwrap_or_default();
    if let Some(found) = icon::by_name(lookup, &name) {
        return found;
    }
    let pixels = match proxy.icon_pixmap().await {
        Ok(pixmaps) => best_pixmap(&pixmaps).map(convert_pixmap),
        Err(_) => None,
    };
    icon::after_name(lookup, &name, pixels)
}

/// The item's attention icon, down the same chain (`AttentionIconName`, then
/// `AttentionIconPixmap` beside it), or `None` when it offers none or none
/// that resolves — [`icon::attention`] says which.
async fn attention_of(proxy: &ItemProxy<'_>, lookup: &Lookup) -> Option<(Icon, Source)> {
    let name = proxy.attention_icon_name().await.unwrap_or_default();
    if let Some(found) = icon::by_name(lookup, &name) {
        return Some(found);
    }
    let pixels = match proxy.attention_icon_pixmap().await {
        Ok(pixmaps) => best_pixmap(&pixmaps).map(convert_pixmap),
        Err(_) => None,
    };
    icon::attention(icon::after_name(lookup, &name, pixels))
}

/// The largest pixmap on offer.
///
/// The largest is the best source: [`fit_to_slot`] resamples it to the slot
/// before the wire, and resampling down from a 512 px icon beats resampling
/// up from a 16 px one.
pub fn best_pixmap(pixmaps: &[Pixmap]) -> Option<&Pixmap> {
    let usable = |p: &&Pixmap| {
        p.width > 0 && p.height > 0 && p.argb.len() as i64 >= p.width as i64 * p.height as i64 * 4
    };
    pixmaps
        .iter()
        .filter(usable)
        .max_by_key(|p| p.width.max(p.height))
}

/// Fit a straight-RGBA8 pixmap into [`WIRE_PIXMAP`] squared, aspect preserved
/// and the remainder transparent. `None` when it is already that square or is
/// malformed, so the common case allocates nothing and the caller keeps what
/// it had.
///
/// The result is a **padded square**: the bar does not centre what arrives,
/// so the writer is the one that has to hand over something a slot holds.
/// Small icons are scaled *up* into it, which is what the caustic's sampler
/// would have done anyway.
///
/// tiny-skia's `Pixmap` is premultiplied and a `Pixmap` frame's texels are straight (docs/abi.md, *Pixmap*),
/// so this premultiplies on the way in and demultiplies on the way out. Both
/// conversions lose the low bits of a partly transparent texel; an icon is not
/// a photograph and the alternative is a hand-rolled resampler.
pub fn fit_to_slot(width: u32, height: u32, rgba: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let px = WIRE_PIXMAP;
    if width == 0 || height == 0 || rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    if width == px && height == px {
        return None;
    }
    let scale = (px as f32 / width as f32).min(px as f32 / height as f32);
    let tx = (px as f32 - width as f32 * scale) / 2.0;
    let ty = (px as f32 - height as f32 * scale) / 2.0;

    let mut source = tiny_skia::Pixmap::new(width, height)?;
    for (texel, texel_bytes) in source.pixels_mut().iter_mut().zip(rgba.chunks_exact(4)) {
        *texel = ColorU8::from_rgba(
            texel_bytes[0],
            texel_bytes[1],
            texel_bytes[2],
            texel_bytes[3],
        )
        .premultiply();
    }
    let mut out = tiny_skia::Pixmap::new(px, px)?;
    out.draw_pixmap(
        0,
        0,
        source.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        },
        Transform::from_translate(tx, ty).pre_scale(scale, scale),
        None,
    );

    let mut straight = Vec::with_capacity((px as usize) * (px as usize) * 4);
    for texel in out.pixels() {
        let c = texel.demultiply();
        straight.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((px, px, straight))
}

/// ARGB32 in network byte order — `[A, R, G, B]` per pixel — to the straight RGBA8 a `Pixmap` frame carries.
pub fn convert_pixmap(pixmap: &Pixmap) -> Icon {
    let (width, height) = (pixmap.width as u32, pixmap.height as u32);
    let mut rgba = Vec::with_capacity(pixmap.argb.len());
    for px in pixmap.argb.chunks_exact(4).take((width * height) as usize) {
        rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
    }
    Icon::Pixmap {
        width,
        height,
        rgba,
    }
}

fn describe(icon: &Icon) -> String {
    match icon {
        Icon::None => "no icon".to_owned(),
        Icon::Themed { name, .. } => format!("icon {name:?}"),
        Icon::Pixmap { width, height, .. } => format!("pixmap {width}x{height}"),
    }
}
