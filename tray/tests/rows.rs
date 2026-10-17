//! What the tray does, without a bus.
//!
//! The fixtures under `tray/tests/data/` are the **raw D-Bus reply bodies** of
//! `GetLayout(0, -1, …)` as tailscale and kdeconnect answer it on a session
//! bus, recorded and then scrubbed: the rows that named an account, a machine
//! or a phone (the account row's text and avatar, "This Device", the
//! recommended exit node, the phone's name) were rewritten with invented
//! values of the same shape and re-serialised through zvariant, and
//! `icon-data.jpg` (the avatar row's `icon-data`) is a generated 96x96
//! gradient. Everything else is as the applications sent it, including the
//! public list of exit-node locations. Deserialising those bytes is the
//! point: the recursive `(ia{sv}av)` shape is exactly where a hand-written
//! deserialiser goes wrong, and a value rebuilt in the test would only prove
//! the test agrees with itself.
//!
//! Everything past the deserialiser is the pure translation in
//! `caustics_tray::rows`, so these tests are the frame the bar would have been
//! sent, asserted row by row.

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MIDDLE, ACTION_SECONDARY, ACTION_SET_VALUE, APPLET_TRAY, NO_INDEX,
    ROWS_ARENA_BYTES,
};
use caustics_bar_abi::wire::{
    self, ActionBody, Frame, MAX_ROWS, RFLAG_ATTENTION, RFLAG_CHECKABLE, RFLAG_CHECKED,
    RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_LABEL_TITLE, RFLAG_RADIO, RFLAG_SUBMENU, ROW_ITEM,
    ROW_MENU, ROW_SEPARATOR, Rows, pixmap_key,
};
use caustics_tray::icon::{self, Lookup, Source, Tally};
use caustics_tray::item::WIRE_PIXMAP;
use caustics_tray::menu;
use caustics_tray::model::{Icon, Menu, MenuItem, Toggle, TrayAddress, TrayItem, TrayStatus};
use caustics_tray::rows::{self, Built, Call, Entry, Ids, Sent, Target};

const TAILSCALE: &[u8] = include_bytes!("data/dbusmenu-tailscale.bin");
const KDECONNECT: &[u8] = include_bytes!("data/dbusmenu-kdeconnect.bin");
const ICON_PNG: &[u8] = include_bytes!("data/icon-data.png");
const ICON_JPEG: &[u8] = include_bytes!("data/icon-data.jpg");

/// A theme that has every name: the chain's first step always wins, which is
/// what every test about the *frame* wants — the machine's themes are not
/// what they are about.
fn every_theme(_: &str) -> bool {
    true
}

/// A machine with no icon theme at all.
fn no_theme(_: &str) -> bool {
    false
}

fn layout(body: &[u8]) -> Menu {
    let lookup = Lookup {
        themed: every_theme,
        dirs: Vec::new(),
    };
    menu::parse_reply(body, &lookup)
        .expect("the GetLayout body deserialises")
        .0
}

/// A scratch directory for the chain's step 2, removed at the end.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("caustics-tray-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn put(&self, rel: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// The deserialiser
// ---------------------------------------------------------------------------

#[test]
fn dbusmenu_layout_deserialises() {
    let menu = layout(TAILSCALE);
    let rows = &menu.items;
    assert_eq!(menu.revision, 28);
    assert_eq!(rows.len(), 10);

    // Defaults applied for what the server left out, `enabled` honoured.
    assert_eq!(rows[0].label, "Connected");
    assert!(!rows[0].enabled);
    assert!(rows[0].visible);
    assert_eq!(rows[1].label, "Disconnect");
    assert!(rows[1].enabled);

    // `type = "separator"`, which carries no label.
    assert!(rows[2].separator);
    assert_eq!(rows[2].label, "");

    // A row whose only icon is inline `icon-data` — JPEG, on this item.
    assert!(
        matches!(rows[3].icon, Icon::Pixmap { .. }),
        "row 3 icon: {:?}",
        rows[3].icon
    );

    // Nesting, and the checkmark that says which exit node is selected.
    let exit_nodes = &rows[6];
    assert_eq!(exit_nodes.label, "Exit Nodes");
    assert_eq!(exit_nodes.children.len(), 6);
    assert_eq!(exit_nodes.children[0].label, "None");
    assert_eq!(exit_nodes.children[0].toggle, Toggle::Checkmark(true));
    assert_eq!(exit_nodes.children[2].toggle, Toggle::Checkmark(false));

    // Three levels down: Mullvad VPN -> Australia -> Sydney.
    let mullvad = &exit_nodes.children[5];
    assert_eq!(mullvad.label, "Mullvad VPN");
    let australia = mullvad
        .children
        .iter()
        .find(|r| r.label.ends_with("Australia"))
        .unwrap();
    assert!(australia.children.iter().any(|r| r.label == "Sydney"));
}

#[test]
fn dbusmenu_layout_prefers_a_themed_name_over_inline_data() {
    let menu = layout(KDECONNECT);
    let rows = &menu.items;
    assert_eq!(menu.revision, 47);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].label, "Open app");

    // Every kdeconnect row carries both `icon-name` and `icon-data`; the
    // themed name wins so the bar follows the user's icon theme.
    let device = &rows[1];
    assert_eq!(device.label, "Example Phone");
    assert!(
        matches!(&device.icon, Icon::Themed { name, theme_path: None }
            if name == "smartphonedisconnected"),
        "device icon: {:?}",
        device.icon,
    );
    // A disabled status row. Its number moves with the phone, so match the
    // shape, not the reading.
    assert!(
        device.children[0].label.starts_with("Battery: "),
        "{:?}",
        device.children[0].label
    );
    assert!(!device.children[0].enabled);

    // `visible = false` survives the deserialiser; the frame builder is what
    // drops it (below).
    let hidden = device
        .children
        .iter()
        .find(|r| r.label == "Run command")
        .unwrap();
    assert!(!hidden.visible);
}

#[test]
fn icon_data_decodes_both_encodings_on_the_bus() {
    // kdeconnect sends PNG, tailscale sends JPEG; the spec mentions only PNG.
    let (width, height, rgba) = menu::decode(ICON_PNG).expect("PNG icon-data decodes");
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    assert!(width > 0 && height > 0);

    let (width, height, rgba) = menu::decode(ICON_JPEG).expect("JPEG icon-data decodes");
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    assert!(width > 0 && height > 0);

    assert!(menu::decode(b"not an image at all").is_none());
    assert!(menu::decode(&[]).is_none());
}

#[test]
fn an_item_s_own_icon_directory_is_read_for_the_file() {
    // Steam: `IconName "steam_tray_mono"`, `IconThemePath` a flat directory
    // holding `steam_tray_mono.png`, and no theme that knows the name.
    let scratch = Scratch::new("own-icon");
    scratch.put("steam_tray_mono.png", ICON_PNG);
    let lookup = Lookup {
        themed: no_theme,
        dirs: vec![scratch.0.clone()],
    };

    let (width, height, rgba) = menu::decode(ICON_PNG).unwrap();
    let (found, source) = icon::by_name(&lookup, "steam_tray_mono").expect("the file is there");
    assert_eq!(
        found,
        Icon::Pixmap {
            width,
            height,
            rgba
        }
    );
    assert_eq!(
        source,
        Source::OwnPng(scratch.0.join("steam_tray_mono.png"))
    );
    // A name the directory lacks is not found there, and a name is never a path.
    assert_eq!(icon::by_name(&lookup, "dropboxstatus-idle"), None);
    let elsewhere = Lookup {
        themed: no_theme,
        dirs: vec!["/nonexistent".into()],
    };
    assert_eq!(icon::by_name(&elsewhere, "steam_tray_mono"), None);
    let root = Lookup {
        themed: no_theme,
        dirs: vec!["/".into()],
    };
    let as_path = format!("{}/steam_tray_mono", scratch.0.display());
    assert_eq!(icon::by_name(&root, &as_path), None);
}

/// The chain, step by step: a theme's name beats the
/// item's own file; the own directory is read flat or theme-shaped, a PNG as
/// pixels and an SVG as its absolute path; the pixels beside the name come
/// next; and a name nothing has is sent anyway, carrying the directory it was
/// looked for in.
#[test]
fn the_tray_s_icon_chain_is_theme_then_own_directory_then_pixels_then_the_name() {
    let scratch = Scratch::new("chain");
    let (width, height, rgba) = menu::decode(ICON_PNG).unwrap();
    let png = Icon::Pixmap {
        width,
        height,
        rgba,
    };
    let (jw, jh, jrgba) = menu::decode(ICON_JPEG).unwrap();
    let beside = Icon::Pixmap {
        width: jw,
        height: jh,
        rgba: jrgba,
    };
    scratch.put("hicolor/16x16/apps/kde-app.png", ICON_JPEG);
    let big = scratch.put("hicolor/48x48/apps/kde-app.png", ICON_PNG);
    let svg = scratch.put("hicolor/scalable/status/only-svg.svg", b"<svg/>");

    // 1. A theme has it: the name, even with the file and pixels at hand.
    let themed = Lookup {
        themed: every_theme,
        dirs: vec![scratch.0.clone()],
    };
    assert_eq!(
        icon::choose(&themed, "kde-app", Some(beside.clone())),
        (
            Icon::Themed {
                name: "kde-app".into(),
                theme_path: None
            },
            Source::Theme
        )
    );

    let own = Lookup {
        themed: no_theme,
        dirs: vec![scratch.0.clone()],
    };
    // 2. Theme-shaped, the largest size: decoded here, pixels on the wire.
    assert_eq!(
        icon::choose(&own, "kde-app", Some(beside.clone())),
        (png.clone(), Source::OwnPng(big.clone()))
    );
    // 2. An SVG crosses as its absolute path, which `caustics-glyphs` and
    // `caustics-apps` both take as its own answer.
    let path = svg.to_str().unwrap().to_owned();
    assert_eq!(
        icon::choose(&own, "only-svg", None),
        (
            Icon::Themed {
                name: path,
                theme_path: None
            },
            Source::OwnSvg(svg.clone())
        )
    );
    // 3. The pixels beside a name nothing has.
    assert_eq!(
        icon::choose(&own, "nobody-has-this", Some(beside.clone())),
        (beside, Source::Pixels)
    );
    // 4. The name anyway, with where it was looked for.
    let dir = scratch.0.display().to_string();
    assert_eq!(
        icon::choose(&own, "nobody-has-this", None),
        (
            Icon::Themed {
                name: "nobody-has-this".into(),
                theme_path: Some(dir)
            },
            Source::Lost
        )
    );
    // No name and no pixels is no icon.
    assert_eq!(icon::choose(&own, "", None), (Icon::None, Source::Nothing));
    assert_eq!(
        icon::choose(&own, "", Some(png.clone())),
        (png, Source::Pixels)
    );

    // The item's and the menu's `IconThemePath`s are one list, blanks,
    // relative paths and repeats dropped.
    assert_eq!(
        Lookup::dirs_of(["/a", "", "rel", "/b", "/a"]),
        vec![std::path::PathBuf::from("/a"), "/b".into()]
    );
}

/// kdeconnect's rows on a machine whose themes have none of its Breeze names
/// — a desktop without KDE's themes logs `smartphonedisconnected -> nothing
/// (no theme has it)` — draw the PNG each row sends beside the name, and the
/// fetch's witness names what fell back.
#[test]
fn a_kdeconnect_row_whose_name_no_theme_has_draws_its_icon_data() {
    let lookup = Lookup {
        themed: no_theme,
        dirs: Vec::new(),
    };
    let (menu, tally) = menu::parse_reply(KDECONNECT, &lookup).expect("it deserialises");
    let device = &menu.items[1];
    assert_eq!(device.label, "Example Phone");
    assert!(
        matches!(device.icon, Icon::Pixmap { .. }),
        "device icon: {:?}",
        device.icon
    );
    assert!(
        tally.instead.iter().any(|n| n == "smartphonedisconnected"),
        "{tally:?}"
    );
    assert!(
        tally.lost.is_empty(),
        "every kdeconnect row sends its pixels: {tally:?}"
    );
    assert_eq!(tally.theme, 0);
    println!("{}", tally.describe());

    // And on a machine whose theme does have them, the names, as before.
    let (themed, tally) = menu::parse_reply(
        KDECONNECT,
        &Lookup {
            themed: every_theme,
            dirs: Vec::new(),
        },
    )
    .unwrap();
    assert!(
        matches!(&themed.items[1].icon, Icon::Themed { name, .. } if name == "smartphonedisconnected")
    );
    assert_eq!(
        tally,
        Tally {
            theme: tally.theme,
            ..Tally::default()
        }
    );
    assert!(tally.theme > 0);
}

#[test]
fn mnemonics_are_stripped() {
    assert_eq!(menu::strip_mnemonic("_Quit"), "Quit");
    assert_eq!(menu::strip_mnemonic("Save _As"), "Save As");
    assert_eq!(menu::strip_mnemonic("a __b"), "a _b");
    assert_eq!(menu::strip_mnemonic("no marker"), "no marker");
    assert_eq!(menu::strip_mnemonic("trailing_"), "trailing");
}

#[test]
fn tray_addresses_resolve() {
    // The three forms watchers hand out on a live session bus.
    let cases = [
        (":1.137/StatusNotifierItem", ":1.137", "/StatusNotifierItem"),
        (
            ":1.1177/org/ayatana/NotificationItem/dropbox_client_3",
            ":1.1177",
            "/org/ayatana/NotificationItem/dropbox_client_3",
        ),
        (":1.1280", ":1.1280", "/StatusNotifierItem"),
    ];
    for (given, bus_name, path) in cases {
        let address = TrayAddress::parse(given);
        assert_eq!(address.bus_name, bus_name, "bus name of {given}");
        assert_eq!(address.path, path, "path of {given}");
        // The key is what the items are held under, so parsing it back has to
        // land on the same address — that is what makes an unregistration
        // match the registration.
        assert_eq!(TrayAddress::parse(&address.key()), address);
    }
}

#[test]
fn sni_pixmaps_become_straight_rgba8() {
    // ARGB32 in network byte order: [A, R, G, B] per pixel.
    let pixmap = caustics_tray::item::Pixmap {
        width: 2,
        height: 1,
        argb: vec![0xff, 0x10, 0x20, 0x30, 0x80, 0x40, 0x50, 0x60],
    };
    let Icon::Pixmap {
        width,
        height,
        rgba,
    } = caustics_tray::item::convert_pixmap(&pixmap)
    else {
        panic!("a pixmap converts to a pixmap");
    };
    assert_eq!((width, height), (2, 1));
    assert_eq!(rgba, vec![0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0x80]);
}

#[test]
fn the_largest_pixmap_offered_wins() {
    let px = |w: i32| caustics_tray::item::Pixmap {
        width: w,
        height: w,
        argb: vec![0; (w * w * 4) as usize],
    };
    // The largest is the best *source*, whatever its size.
    let offered = [px(16), px(48), px(128)];
    assert_eq!(
        caustics_tray::item::best_pixmap(&offered).unwrap().width,
        128
    );

    // A 512 px icon is usable.
    let huge = [px(512), px(250)];
    assert_eq!(caustics_tray::item::best_pixmap(&huge).unwrap().width, 512);

    // A truncated pixmap is not a pixmap.
    let short = [caustics_tray::item::Pixmap {
        width: 16,
        height: 16,
        argb: vec![0; 4],
    }];
    assert!(caustics_tray::item::best_pixmap(&short).is_none());
    assert!(caustics_tray::item::best_pixmap(&[]).is_none());
}

#[test]
fn a_pixmap_the_bar_cannot_use_is_resampled_on_the_way_to_the_wire() {
    // Two sizes off the live bus's shape: tailscale's square 250, and a
    // wide one to prove the aspect is kept rather than squashed to a square.
    let square = item(
        ":1.9",
        Icon::Pixmap {
            width: 250,
            height: 250,
            rgba: vec![0x40; 250 * 250 * 4],
        },
        TrayStatus::Active,
        false,
    );
    let wide = item(
        ":1.10",
        Icon::Pixmap {
            width: 512,
            height: 300,
            rgba: vec![0x40; 512 * 300 * 4],
        },
        TrayStatus::Active,
        false,
    );
    // Already the slot's own square: untouched, and no witness about it.
    let small = item(
        ":1.11",
        Icon::Pixmap {
            width: WIRE_PIXMAP,
            height: WIRE_PIXMAP,
            rgba: vec![0x40; (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize],
        },
        TrayStatus::Active,
        false,
    );
    let mut ids = Ids::default();
    let (built, notes) = build_noting(
        &[
            Entry {
                key: ":1.9",
                item: &square,
                menu: None,
            },
            Entry {
                key: ":1.10",
                item: &wide,
                menu: None,
            },
            Entry {
                key: ":1.11",
                item: &small,
                menu: None,
            },
        ],
        &mut ids,
    );
    let rows = round_trip(&built.rows);

    // Every one of them is the slot's square: a 512x300 keeps its aspect
    // inside it and the remainder is transparent. The row names it; the texels
    // are in `Built::pixmaps` under that name.
    for i in 0..3 {
        let texels = texels_of(&built, &rows, i);
        assert_eq!(
            texels.len(),
            (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize,
            "row {i}"
        );
    }
    let small_rgba = vec![0x40; (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize];
    assert_eq!(
        texels_of(&built, &rows, 2),
        &small_rgba[..],
        "a pixmap already that square is left alone"
    );
    // The 512x300 is centred: its top row is padding, its middle row is not.
    let wide = texels_of(&built, &rows, 1);
    assert_eq!(wide[3], 0, "the band above a wide icon is transparent");
    let mid = (32 * WIRE_PIXMAP as usize + 32) * 4;
    assert_eq!(wide[mid + 3], 0x40, "the icon itself is in the middle");

    assert!(
        notes.iter().any(|n| n == ":1.9: pixmap 250x250 → 64x64"),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n == ":1.10: pixmap 512x300 → 64x64"),
        "{notes:?}"
    );
    assert!(
        !notes.iter().any(|n| n.contains(":1.11")),
        "nothing to say about it: {notes:?}"
    );
}

#[test]
fn the_tailscale_frame_carries_keys_not_pixels() {
    // tailscale's only icon is 250x250 and one of its menu rows carries a
    // 96x96 `icon-data`, which inline would be about 290 KB in every frame
    // the item appears in; as pixmap keys the pixels leave the frame entirely.
    let tailscale = layout(TAILSCALE);
    let sni = item(
        ":1.137/StatusNotifierItem",
        Icon::Pixmap {
            width: 250,
            height: 250,
            rgba: vec![0x40; 250 * 250 * 4],
        },
        TrayStatus::Active,
        true,
    );
    let mut ids = Ids::default();
    let built = build(
        &[Entry {
            key: ":1.137/StatusNotifierItem",
            item: &sni,
            menu: Some(&tailscale),
        }],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);

    let mut bytes = Vec::new();
    wire::write(&mut bytes, &Frame::Rows(rows.clone())).expect("the frame writes");
    println!(
        "the tailscale frame is {} bytes over {} rows",
        bytes.len(),
        rows.rows.len()
    );
    assert!(
        bytes.len() < 16 * 1024,
        "the tailscale frame is {} bytes, which should be well under 16 KiB",
        bytes.len(),
    );
    // Every pixmap icon in it is a name the tray minted, none was dropped, and
    // the texels are beside the frame at the slot's square — one frame each.
    let named: Vec<&str> = (0..rows.rows.len())
        .filter_map(|i| rows.icon(i))
        .filter(|name| name.starts_with("caustics-tray:"))
        .collect();
    assert_eq!(
        named.len(),
        2,
        "the item's icon and the menu row's: {named:?}"
    );
    assert_eq!(built.pixmaps.len(), 2);
    for (key, rgba) in &built.pixmaps {
        assert!(named.contains(&key.as_str()), "{key} is named by a row");
        assert_eq!(rgba.len(), (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize);
        assert_eq!(
            *key,
            pixmap_key(rows::KEY_SIDECAR, rgba),
            "content-addressed"
        );
    }
}

/// A row's icon key, and the texels `Built::pixmaps` carries under it.
fn texels_of<'a>(built: &'a Built, rows: &Rows, i: usize) -> &'a [u8] {
    let Some(key) = rows.icon(i) else {
        panic!("row {i} should name a pixmap");
    };
    assert!(key.starts_with("caustics-tray:"), "row {i} names {key:?}");
    built
        .pixmaps
        .iter()
        .find(|(held, _)| held == key)
        .map(|(_, rgba)| rgba.as_slice())
        .unwrap_or_else(|| panic!("row {i}'s key {key} has no texels beside the frame"))
}

#[test]
fn a_pixmap_goes_out_once_per_key_and_never_again() {
    // Two items with the same picture and one with another: two keys, and
    // the frame names the first one twice.
    let flat = |v: u8| Icon::Pixmap {
        width: WIRE_PIXMAP,
        height: WIRE_PIXMAP,
        rgba: vec![v; (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize],
    };
    let a = item(":1.20", flat(0x10), TrayStatus::Active, false);
    let b = item(":1.21", flat(0x10), TrayStatus::Active, false);
    let c = item(":1.22", flat(0x20), TrayStatus::Active, false);
    let entries = [
        Entry {
            key: ":1.20",
            item: &a,
            menu: None,
        },
        Entry {
            key: ":1.21",
            item: &b,
            menu: None,
        },
        Entry {
            key: ":1.22",
            item: &c,
            menu: None,
        },
    ];
    let mut ids = Ids::default();
    let mut sent = Sent::default();

    let first = build(&entries, &mut ids, 1);
    let rows = round_trip(&first.rows);
    assert_eq!(
        rows.icon(0),
        rows.icon(1),
        "the same texels are the same key"
    );
    assert_ne!(rows.icon(0), rows.icon(2));
    assert_eq!(first.pixmaps.len(), 2, "deduplicated within the frame");

    let owed = sent.owed(first.pixmaps);
    assert_eq!(owed.len(), 2, "one Pixmap per distinct icon");
    for frame in &owed {
        let Frame::Pixmap(pixmap) = frame else {
            panic!("{frame:?}")
        };
        assert_eq!((pixmap.width, pixmap.height), (WIRE_PIXMAP, WIRE_PIXMAP));
        // The frame is one the bar's reader takes.
        let mut bytes = Vec::new();
        wire::write(&mut bytes, frame).expect("the pixmap writes");
        assert_eq!(
            wire::read(&mut bytes.as_slice()).unwrap().as_ref(),
            Some(frame)
        );
    }

    // The next build names the same keys, and nothing is owed.
    let second = build(&entries, &mut ids, 2);
    assert!(
        sent.owed(second.pixmaps).is_empty(),
        "none on the second build"
    );

    // An icon that changed is a new key, and only it goes out.
    let d = item(":1.22", flat(0x30), TrayStatus::Active, false);
    let changed = build(
        &[
            Entry {
                key: ":1.20",
                item: &a,
                menu: None,
            },
            Entry {
                key: ":1.22",
                item: &d,
                menu: None,
            },
        ],
        &mut ids,
        3,
    );
    assert_eq!(sent.owed(changed.pixmaps).len(), 1);
}

/// A real tray (two items with pixmap icons, two menu rows with pixmap icons
/// and 141 labelled rows) would be about 66 KB with its pixels inline, past
/// the bar's 65,536-byte region, and the bar would refuse every frame. As
/// names it is a small fraction of that.
#[test]
fn a_full_tray_with_four_pixmaps_fits_the_bars_region_with_room_to_spare() {
    let slot = (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize;
    let tailscale_icon = Icon::Pixmap {
        width: 250,
        height: 250,
        rgba: vec![0x40; 250 * 250 * 4],
    };
    let discord_icon = Icon::Pixmap {
        width: 24,
        height: 24,
        rgba: vec![0x70; 24 * 24 * 4],
    };
    let mut rows_left = 141;
    let menu = |seed: u8, pixmap_rows: usize, count: usize| -> Menu {
        let items = (0..count)
            .map(|i| MenuItem {
                id: i as i32 + 1,
                // The length of a real exit-node row, and then some.
                label: format!("Mullvad VPN / Exit node {i:03} au-syd-wg-{i:03}.mullvad.ts.net"),
                enabled: true,
                visible: true,
                icon: if i < pixmap_rows {
                    Icon::Pixmap {
                        width: 96,
                        height: 96,
                        rgba: vec![seed + i as u8; 96 * 96 * 4],
                    }
                } else {
                    Icon::None
                },
                ..Default::default()
            })
            .collect();
        Menu { revision: 1, items }
    };
    let tailscale_menu = menu(0x50, 1, 120);
    rows_left -= 120;
    let discord_menu = menu(0x90, 1, rows_left);
    let tailscale = item(
        ":1.137/StatusNotifierItem",
        tailscale_icon,
        TrayStatus::Active,
        true,
    );
    let discord = item(
        ":1.200/StatusNotifierItem",
        discord_icon,
        TrayStatus::Active,
        true,
    );
    let mut ids = Ids::default();
    let built = build(
        &[
            Entry {
                key: ":1.137/StatusNotifierItem",
                item: &tailscale,
                menu: Some(&tailscale_menu),
            },
            Entry {
                key: ":1.200/StatusNotifierItem",
                item: &discord,
                menu: Some(&discord_menu),
            },
        ],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);
    assert_eq!(rows.rows.len(), 2 + 141);
    assert_eq!(built.pixmaps.len(), 4, "four icons, four keys");
    assert!(built.pixmaps.iter().all(|(_, rgba)| rgba.len() == slot));
    let body = rows.body_len();
    println!(
        "a full tray is a {body}-byte Rows body beside {} Pixmap frames",
        built.pixmaps.len()
    );
    assert!(body < 16 * 1024, "the body is {body} bytes");
    assert!(body < ROWS_ARENA_BYTES / 4);
}

// ---------------------------------------------------------------------------
// The frame
// ---------------------------------------------------------------------------

fn item(key: &str, icon: Icon, status: TrayStatus, has_menu: bool) -> TrayItem {
    TrayItem {
        address: TrayAddress::parse(key),
        title: "Item title".to_owned(),
        icon,
        status,
        has_menu,
    }
}

/// Build one frame and say nothing about the witnesses.
fn build(entries: &[Entry<'_>], ids: &mut Ids, revision: u32) -> Built {
    rows::build(entries, ids, revision, &mut |_| {})
}

/// Build one frame and keep the witnesses.
fn build_noting(entries: &[Entry<'_>], ids: &mut Ids) -> (Built, Vec<String>) {
    let mut notes = Vec::new();
    let built = rows::build(entries, ids, 1, &mut |line| notes.push(line.to_owned()));
    (built, notes)
}

/// A frame is only a frame if the bar's own reader takes it: header, offsets,
/// parents before children, every bound in *Rows*. Round-tripping through
/// `wire::write` and `wire::read` is that assertion, and it is made of every
/// frame these tests build.
fn round_trip(rows: &Rows) -> Rows {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &Frame::Rows(rows.clone())).expect("the frame writes");
    match wire::read(&mut bytes.as_slice()) {
        Ok(Some(Frame::Rows(read))) => {
            assert_eq!(&read, rows, "what the reader took is what was written");
            read
        }
        other => panic!("the reader rejected the frame: {other:?}"),
    }
}

/// Row `i`'s label, for readable assertions.
fn label(rows: &Rows, i: usize) -> String {
    rows.label(i).map(|l| l.into_owned()).unwrap_or_default()
}

/// The index of the row labelled `text`.
fn find(rows: &Rows, text: &str) -> usize {
    (0..rows.rows.len())
        .find(|i| label(rows, *i) == text)
        .unwrap_or_else(|| panic!("{text}"))
}

#[test]
fn a_live_menu_becomes_the_rows_the_bar_draws() {
    let tailscale = layout(TAILSCALE);
    let sni = item(
        ":1.137/StatusNotifierItem",
        Icon::None,
        TrayStatus::Active,
        true,
    );
    let mut ids = Ids::default();
    let built = build(
        &[Entry {
            key: ":1.137/StatusNotifierItem",
            item: &sni,
            menu: Some(&tailscale),
        }],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);

    assert_eq!(rows.applet, APPLET_TRAY);
    assert_eq!(rows.revision, 1);

    // The bar item, its label the item's title under `RFLAG_LABEL_TITLE`:
    // never drawn beside the icon, the placeholder's letter while the icon is
    // unresolved.
    assert_eq!(rows.rows[0].role, ROW_ITEM);
    assert_eq!(rows.rows[0].parent, NO_INDEX);
    assert_eq!(rows.rows[0].flags, RFLAG_HAS_MENU | RFLAG_LABEL_TITLE);
    assert_eq!(label(&rows, 0), "Item title");

    // Root rows hang off the item; every one of them precedes its children.
    assert_eq!(label(&rows, 1), "Connected");
    assert_eq!(rows.rows[1].parent, 0);
    assert_eq!(
        rows.rows[1].flags, RFLAG_DISABLED,
        "`enabled = false` is DISABLED"
    );

    assert_eq!(
        rows.rows[3].role, ROW_SEPARATOR,
        "`type = separator` is its own role"
    );
    assert_eq!(rows.rows[3].parent, 0);

    // The submenu row carries SUBMENU, and its children name *it*.
    let exit = find(&rows, "Exit Nodes");
    assert_eq!(rows.rows[exit].role, ROW_MENU);
    assert_eq!(rows.rows[exit].flags & RFLAG_SUBMENU, RFLAG_SUBMENU);
    assert_eq!(rows.rows[exit].parent, 0);

    let none = (0..rows.rows.len())
        .find(|i| rows.rows[*i].parent == exit as u32 && label(&rows, *i) == "None")
        .expect("the selected exit node");
    assert_eq!(
        rows.rows[none].flags & (RFLAG_CHECKABLE | RFLAG_CHECKED),
        RFLAG_CHECKABLE | RFLAG_CHECKED,
        "`toggle-type = checkmark`, `toggle-state = 1`",
    );
    assert!(
        none > exit,
        "a parent precedes its children — the reader's own rule"
    );

    // Three levels down still hangs together: Mullvad VPN -> Australia -> Sydney.
    let mullvad = find(&rows, "Mullvad VPN");
    assert_eq!(rows.rows[mullvad].parent, exit as u32);
    let australia = (0..rows.rows.len())
        .find(|i| rows.rows[*i].parent == mullvad as u32 && label(&rows, *i).ends_with("Australia"))
        .expect("Australia");
    assert!(
        (0..rows.rows.len())
            .any(|i| rows.rows[i].parent == australia as u32 && label(&rows, i) == "Sydney"),
        "Sydney under Australia under Mullvad VPN",
    );

    // Every row of the tree is in the frame, and nothing else is.
    fn visible(items: &[MenuItem]) -> usize {
        items
            .iter()
            .filter(|r| r.visible)
            .map(|r| 1 + visible(&r.children))
            .sum()
    }
    assert_eq!(rows.rows.len(), 1 + visible(&tailscale.items));
}

#[test]
fn hidden_rows_are_left_out_and_radios_come_through() {
    let kdeconnect = layout(KDECONNECT);
    let sni = item(":1.1280", Icon::None, TrayStatus::Active, true);
    let mut ids = Ids::default();
    let built = build(
        &[Entry {
            key: ":1.1280",
            item: &sni,
            menu: Some(&kdeconnect),
        }],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);

    let labels: Vec<String> = (0..rows.rows.len()).map(|i| label(&rows, i)).collect();
    assert!(labels.contains(&"Open app".to_owned()));
    assert!(
        !labels.contains(&"Run command".to_owned()),
        "`visible = false` has no place in the frame: {labels:?}",
    );
    // The hidden row's siblings are still there, so nothing collapsed with it.
    assert!(labels.iter().any(|l| l.starts_with("Battery: ")));

    // A radio row from a tree built by hand, since kdeconnect's has none.
    let radio = Menu {
        revision: 1,
        items: vec![MenuItem {
            id: 9,
            label: "Radio".into(),
            enabled: true,
            visible: true,
            toggle: Toggle::Radio(true),
            ..Default::default()
        }],
    };
    let built = build(
        &[Entry {
            key: ":1.1280",
            item: &sni,
            menu: Some(&radio),
        }],
        &mut ids,
        2,
    );
    let rows = round_trip(&built.rows);
    assert_eq!(
        rows.rows[1].flags,
        RFLAG_CHECKABLE | RFLAG_CHECKED | RFLAG_RADIO,
        "`toggle-type = radio` is a radio, not a check",
    );
}

#[test]
fn an_items_icon_is_a_themed_name_or_a_pixmap() {
    let named = item(
        ":1.1",
        Icon::Themed {
            name: "dropbox".into(),
            theme_path: None,
        },
        TrayStatus::NeedsAttention,
        false,
    );
    let rgba = vec![0x11; 16 * 16 * 4];
    let pixmap = item(
        ":1.2",
        Icon::Pixmap {
            width: 16,
            height: 16,
            rgba: rgba.clone(),
        },
        TrayStatus::Active,
        false,
    );
    let mut ids = Ids::default();
    let built = build(
        &[
            Entry {
                key: ":1.1",
                item: &named,
                menu: None,
            },
            Entry {
                key: ":1.2",
                item: &pixmap,
                menu: None,
            },
        ],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);

    assert_eq!(rows.rows.len(), 2, "two items, in registration order");
    assert_eq!(rows.icon(0), Some("dropbox"));
    assert_eq!(
        rows.rows[0].flags,
        RFLAG_ATTENTION | RFLAG_LABEL_TITLE,
        "NeedsAttention is ATTENTION, and no menu"
    );

    // 16x16 is not the slot's square, so it leaves here scaled into one — the
    // bar fits nothing — and as a key, with the
    // texels beside the frame.
    let sent = texels_of(&built, &rows, 1);
    assert_eq!(sent.len(), (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize);
    assert_eq!(
        built.pixmaps.len(),
        1,
        "a themed name has no texels to send"
    );
    // Still one flat colour, within the bit the premultiply-and-back costs at
    // an alpha of 17 (`item::fit_to_slot` says so).
    assert_eq!(sent[3], rgba[3], "the alpha is exact");
    assert!(
        sent[0].abs_diff(rgba[0]) <= 2,
        "{:?} vs {:?}",
        &sent[..4],
        &rgba[..4]
    );
    assert!(
        sent.chunks_exact(4).all(|t| t[..4] == sent[..4]),
        "and it is still flat"
    );
    assert_eq!(
        rows.rows[1].flags, RFLAG_LABEL_TITLE,
        "an icon adds no flag"
    );
    assert_eq!(
        rows.rows[1].value, -1.0,
        "a tray item draws no level on its icon"
    );
}

#[test]
fn a_passive_item_is_not_on_the_bar() {
    let passive = item(":1.1", Icon::None, TrayStatus::Passive, true);
    let active = item(":1.2", Icon::None, TrayStatus::Active, false);
    let mut ids = Ids::default();
    let built = build(
        &[
            Entry {
                key: ":1.1",
                item: &passive,
                menu: Some(&layout(KDECONNECT)),
            },
            Entry {
                key: ":1.2",
                item: &active,
                menu: None,
            },
        ],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);
    assert_eq!(rows.rows.len(), 1, "Passive means `I have nothing to say`");
    assert_eq!(
        built.targets.get(&rows.rows[0].id),
        Some(&Target::Item(":1.2".to_owned()))
    );
}

#[test]
fn a_menu_rows_icon_data_becomes_a_pixmap_in_both_encodings() {
    let (png_w, png_h, png_rgba) = menu::decode(ICON_PNG).unwrap();
    let (jpeg_w, jpeg_h, jpeg_rgba) = menu::decode(ICON_JPEG).unwrap();
    let tree = Menu {
        revision: 1,
        items: vec![
            MenuItem {
                id: 1,
                label: "PNG".into(),
                enabled: true,
                visible: true,
                icon: Icon::Pixmap {
                    width: png_w,
                    height: png_h,
                    rgba: png_rgba.clone(),
                },
                ..Default::default()
            },
            MenuItem {
                id: 2,
                label: "JPEG".into(),
                enabled: true,
                visible: true,
                icon: Icon::Pixmap {
                    width: jpeg_w,
                    height: jpeg_h,
                    rgba: jpeg_rgba.clone(),
                },
                ..Default::default()
            },
        ],
    };
    let sni = item(":1.3", Icon::None, TrayStatus::Active, true);
    let mut ids = Ids::default();
    let built = build(
        &[Entry {
            key: ":1.3",
            item: &sni,
            menu: Some(&tree),
        }],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);

    // Both encodings reach the wire as a pixmap key, and every one
    // of them is fitted to the slot's own square whatever the bus offered
    // (both fixtures are 96x96). What is asserted here is the shape;
    // `a_pixmap_the_bar_cannot_use_is_resampled_on_the_way_to_the_wire` is
    // where the resampling itself is pinned.
    for (row, what) in [(1, "PNG"), (2, "JPEG")] {
        assert_eq!(label(&rows, row), what);
        let rgba = texels_of(&built, &rows, row);
        assert_eq!(
            rgba.len(),
            (WIRE_PIXMAP * WIRE_PIXMAP * 4) as usize,
            "{what}"
        );
    }
    assert_eq!(built.pixmaps.len(), 2);
}

#[test]
fn an_oversized_pixmap_that_is_not_a_pixmap_is_a_witness_and_no_icon() {
    // A well-formed pixmap of any size is resampled to the slot's square;
    // one whose bytes do not make a 512x512 image cannot be, and that is what
    // still drops — a witness, no key, and no texels beside the frame.
    let broken = item(
        ":1.4",
        Icon::Pixmap {
            width: 512,
            height: 512,
            rgba: vec![0; 512 * 512],
        },
        TrayStatus::Active,
        false,
    );
    let mut ids = Ids::default();
    let (built, notes) = build_noting(
        &[Entry {
            key: ":1.4",
            item: &broken,
            menu: None,
        }],
        &mut ids,
    );
    let rows = round_trip(&built.rows);
    assert_eq!(rows.rows.len(), 1, "the item is still on the bar");
    assert_eq!(rows.icon(0), None);
    assert!(built.pixmaps.is_empty());
    assert!(notes.iter().any(|n| n.contains("512x512")), "{notes:?}");
    assert!(
        !notes.iter().any(|n| n.contains("→")),
        "and nothing claimed to have resampled it: {notes:?}",
    );
}

#[test]
fn a_frame_stops_at_the_wires_row_limit() {
    let items: Vec<MenuItem> = (0..400)
        .map(|i| MenuItem {
            id: i,
            label: format!("row {i}"),
            enabled: true,
            visible: true,
            ..Default::default()
        })
        .collect();
    let tree = Menu { revision: 1, items };
    let sni = item(":1.5", Icon::None, TrayStatus::Active, true);
    let mut ids = Ids::default();
    let (built, notes) = build_noting(
        &[Entry {
            key: ":1.5",
            item: &sni,
            menu: Some(&tree),
        }],
        &mut ids,
    );
    let rows = round_trip(&built.rows);
    assert_eq!(
        rows.rows.len(),
        MAX_ROWS,
        "a frame over MAX_ROWS is rejected whole"
    );
    assert_eq!(notes.len(), 1, "one witness, not one per row: {notes:?}");
    assert!(notes[0].contains("frame full"), "{notes:?}");
}

#[test]
fn ids_are_stable_across_frames_when_the_menu_changes() {
    let sni = item(":1.6", Icon::None, TrayStatus::Active, true);
    let before = Menu {
        revision: 1,
        items: vec![
            MenuItem {
                id: 1,
                label: "Keep".into(),
                enabled: true,
                visible: true,
                ..Default::default()
            },
            MenuItem {
                id: 2,
                label: "Go".into(),
                enabled: true,
                visible: true,
                ..Default::default()
            },
        ],
    };
    let after = Menu {
        revision: 2,
        items: vec![
            MenuItem {
                id: 3,
                label: "New".into(),
                enabled: true,
                visible: true,
                ..Default::default()
            },
            MenuItem {
                id: 1,
                label: "Keep, renamed".into(),
                enabled: true,
                visible: true,
                ..Default::default()
            },
        ],
    };

    let mut ids = Ids::default();
    let first = build(
        &[Entry {
            key: ":1.6",
            item: &sni,
            menu: Some(&before),
        }],
        &mut ids,
        1,
    );
    round_trip(&first.rows);
    let allocated = ids.allocated();
    let second = build(
        &[Entry {
            key: ":1.6",
            item: &sni,
            menu: Some(&after),
        }],
        &mut ids,
        2,
    );
    round_trip(&second.rows);

    assert_eq!(
        first.rows.rows[0].id, second.rows.rows[0].id,
        "the bar item keeps its id"
    );
    // dbusmenu id 1 moved from the first row to the second and was renamed;
    // its row id is the same number, because that is what an action names.
    assert_eq!(first.rows.rows[1].id, second.rows.rows[2].id);
    assert_eq!(
        second.targets.get(&second.rows.rows[2].id),
        Some(&Target::MenuRow {
            key: ":1.6".into(),
            id: 1,
            parent: 0
        }),
    );
    // Exactly one new id: the row that is new. The one that went does not give
    // its number back.
    assert_eq!(ids.allocated(), allocated + 1);
    // And every id in a frame is distinct — the other half of *Rows*'s contract.
    let mut seen: Vec<u32> = second.rows.rows.iter().map(|r| r.id).collect();
    seen.sort_unstable();
    let count = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), count, "ids are unique within a frame");
}

// ---------------------------------------------------------------------------
// Actions back
// ---------------------------------------------------------------------------

fn action(row: u32, kind: u32) -> ActionBody {
    ActionBody {
        row,
        kind,
        pos: [12.4, 30.6],
        ..ActionBody::default()
    }
}

#[test]
fn an_action_id_becomes_the_call_it_would_make() {
    let plain = item(":1.7", Icon::None, TrayStatus::Active, false);
    let with_menu = item(":1.8", Icon::None, TrayStatus::Active, true);
    let tree = Menu {
        revision: 1,
        items: vec![MenuItem {
            id: 40,
            label: "Exit nodes".into(),
            enabled: true,
            visible: true,
            children: vec![MenuItem {
                id: 41,
                label: "Sydney".into(),
                enabled: true,
                visible: true,
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let mut ids = Ids::default();
    let built = build(
        &[
            Entry {
                key: ":1.7",
                item: &plain,
                menu: None,
            },
            Entry {
                key: ":1.8",
                item: &with_menu,
                menu: Some(&tree),
            },
        ],
        &mut ids,
        1,
    );
    round_trip(&built.rows);
    let (plain_id, submenu_id, leaf_id) = (
        built.rows.rows[0].id,
        built.rows.rows[2].id,
        built.rows.rows[3].id,
    );

    // A bar item without a menu: SNI Activate, at the click's own position.
    assert_eq!(
        rows::dispatch(&built.targets, &action(plain_id, ACTION_ACTIVATE)),
        Ok((":1.7".to_owned(), Call::Activate { x: 12, y: 31 })),
    );
    // Right and middle both mean SecondaryActivate, as the caustic pushes them.
    for kind in [ACTION_SECONDARY, ACTION_MIDDLE] {
        assert_eq!(
            rows::dispatch(&built.targets, &action(plain_id, kind)),
            Ok((":1.7".to_owned(), Call::SecondaryActivate { x: 12, y: 31 })),
        );
    }

    // A menu row: AboutToShow on its *parent* — 0 for a root row, the submenu's
    // own dbusmenu id for a nested one — then Event on the row itself.
    assert_eq!(
        rows::dispatch(&built.targets, &action(submenu_id, ACTION_ACTIVATE)),
        Ok((":1.8".to_owned(), Call::MenuClick { id: 40, parent: 0 })),
    );
    let (key, call) = rows::dispatch(&built.targets, &action(leaf_id, ACTION_ACTIVATE)).unwrap();
    assert_eq!(
        (key.as_str(), call),
        (":1.8", Call::MenuClick { id: 41, parent: 40 })
    );
    assert_eq!(
        call.describe(&key),
        ":1.8: AboutToShow(40) then Event(41, clicked)"
    );

    // An id that names nothing, and a kind no row of ours takes: a witness
    // each, and no call.
    assert!(rows::dispatch(&built.targets, &action(9999, ACTION_ACTIVATE)).is_err());
    assert!(rows::dispatch(&built.targets, &action(leaf_id, ACTION_SET_VALUE)).is_err());
}

/// An item's title crosses as its row's label, first line only, under
/// `RFLAG_LABEL_TITLE` — whatever its icon is, and with no icon at all — so
/// the caustic has a letter to draw when the icon never resolves. An item
/// with no title carries no label and the
/// caustic wears `?`.
#[test]
fn an_items_title_is_its_placeholder_and_crosses_whatever_the_icon() {
    let mut steam = item(
        ":1.5",
        Icon::Themed {
            name: "steam_tray_mono".into(),
            theme_path: None,
        },
        TrayStatus::Active,
        true,
    );
    steam.title = "Steam\nsecond line".to_owned();
    let mut bare = item(":1.6", Icon::None, TrayStatus::Active, false);
    bare.title = String::new();
    let mut ids = Ids::default();
    let built = build(
        &[
            Entry {
                key: ":1.5",
                item: &steam,
                menu: None,
            },
            Entry {
                key: ":1.6",
                item: &bare,
                menu: None,
            },
        ],
        &mut ids,
        1,
    );
    let rows = round_trip(&built.rows);
    assert_eq!(label(&rows, 0), "Steam");
    assert_eq!(rows.icon(0), Some("steam_tray_mono"));
    assert_eq!(rows.rows[0].flags & RFLAG_LABEL_TITLE, RFLAG_LABEL_TITLE);
    assert_eq!(rows.label(1), None, "no title, no label");
    assert_eq!(rows.icon(1), None);
    assert_eq!(
        rows.rows[1].flags, RFLAG_LABEL_TITLE,
        "still a title chip: the caustic wears `?`"
    );
}
