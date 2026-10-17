use super::*;

use crate::abi::wire::{Rows, RowsBuilder, Samples};
use crate::model::{Update, Window};

/// The sidecars the fixture's frames come from.
const TRAY: &str = "caustics-tray";
const DEMO: &str = "rows_demo";

fn facts() -> Facts {
    Facts::new(vec![TRAY.to_owned(), DEMO.to_owned()])
}

fn window(id: u64, app_id: &str, title: &str, focused: bool, stamp: u64) -> Update {
    Update::Window(Window {
        id,
        app_id: app_id.to_owned(),
        title: title.to_owned(),
        focused,
        urgent: false,
        focus_stamp: stamp,
    })
}

/// The four windows, the two favourites and the clock the oracle's corpus
/// opens with.
fn fixture() -> Model {
    let mut model = Model::default();
    model.favourites = vec!["kitty".to_owned(), "org.mozilla.firefox".to_owned()];
    model.apply(window(1, "kitty", "one", false, 1));
    model.apply(window(2, "kitty", "two", true, 9));
    model.apply(window(3, "org.mozilla.firefox", "web", false, 4));
    model.apply(window(4, "stray", "a stray", false, 2));
    model.apply(Update::Samples {
        sidecar: "caustics-sysmon".into(),
        body: Box::new(Samples::ZERO),
    });
    model
}

/// The bytes at one range of the strings buffer.
fn text(facts: &Facts, off: u32, len: u32) -> String {
    facts.text_at(off, len).unwrap_or_default()
}

/// Every dock entry's app id, in the order the table carries them — which is
/// dock order (docs/abi.md, *The facts*).
fn dock(facts: &Facts) -> Vec<String> {
    let buffer = facts.buffer();
    buffer
        .apps
        .iter()
        .take(buffer.header.app_count as usize)
        .map(|app| text(facts, app.id_off, app.id_len))
        .collect()
}

#[test]
fn the_slot_table_is_append_only_and_capped() {
    let mut facts = facts();
    assert_eq!(facts.slot_of(TRAY), Some(0));
    assert_eq!(facts.slot_of(DEMO), Some(1));
    assert_eq!(facts.slot_of("caustics-power"), Some(2));
    assert_eq!(facts.slot_of(TRAY), Some(0), "a slot never moves");
    for n in 0..abi::MAX_SLOTS {
        facts.slot_of(&format!("filler-{n}"));
    }
    assert_eq!(facts.slot_of("one-too-many"), None);
    assert_eq!(facts.sidecar(0), Some(TRAY));
    assert_eq!(facts.sidecar(99), None);
}

#[test]
fn every_window_carries_its_app_row_its_stamp_and_its_title() {
    let mut facts = facts();
    let mut images = Images::new();
    facts.compose(&fixture(), &Strings::new(), &mut images);
    let buffer = facts.buffer().clone();
    assert_eq!(buffer.header.window_count, 4);
    let entries = dock(&facts);
    for window in buffer.windows.iter().take(4) {
        // The one join the embassy makes, because it is the one that knows
        // both lists: the row is an index into the table it just wrote.
        assert!(
            (window.app_row as usize) < entries.len(),
            "app_row names a row"
        );
        assert!(
            !text(&facts, window.title_off, window.title_len).is_empty(),
            "every window's title is in the buffer"
        );
    }
    let two = buffer
        .windows
        .iter()
        .find(|w| w.id_lo == 2)
        .expect("window 2 is published");
    assert_eq!(two.focus_stamp, 9);
    assert_eq!(two.flags, abi::WFLAG_FOCUSED);
    assert_eq!(entries[two.app_row as usize], "kitty");
}

#[test]
fn the_dock_is_the_favourites_in_file_order_then_the_rest_first_seen() {
    let mut facts = facts();
    let mut images = Images::new();
    facts.compose(&fixture(), &Strings::new(), &mut images);
    assert_eq!(dock(&facts), vec!["kitty", "org.mozilla.firefox", "stray"]);
    let buffer = facts.buffer();
    assert_eq!(buffer.apps[0].flags, abi::AFLAG_FAVOURITE);
    assert_eq!(buffer.apps[2].flags, 0, "a running stray is not pinned");

    // A second running app appears after the first, and the favourites keep
    // the file's own order however they were seen.
    let mut later = fixture();
    later.apply(window(9, "late.example", "another", false, 3));
    later.favourites = vec!["org.mozilla.firefox".to_owned(), "kitty".to_owned()];
    facts.compose(&later, &Strings::new(), &mut images);
    assert_eq!(
        dock(&facts),
        vec!["org.mozilla.firefox", "kitty", "stray", "late.example"]
    );
}

#[test]
fn a_name_a_witness_quotes_a_frame_later_never_moves() {
    let mut facts = facts();
    let mut images = Images::new();
    let mut model = fixture();
    model.apply(Update::Rows {
        sidecar: DEMO.to_owned(),
        rows: demo(0.25),
    });
    facts.compose(&model, &Strings::new(), &mut images);

    let range_of = |facts: &Facts, id: &str| -> (u32, u32) {
        let buffer = facts.buffer();
        let at = buffer
            .apps
            .iter()
            .take(buffer.header.app_count as usize)
            .position(|app| text(facts, app.id_off, app.id_len) == id)
            .unwrap_or_else(|| panic!("{id} is in the table"));
        (buffer.apps[at].id_off, buffer.apps[at].id_len)
    };
    let kitty = range_of(&facts, "kitty");
    let slot = facts.slot_of(DEMO).expect("a slot") as usize;
    let sidecar = {
        let fact = facts.buffer().slots[slot];
        (fact.name_off, fact.name_len)
    };
    assert_eq!(text(&facts, kitty.0, kitty.1), "kitty");
    assert_eq!(text(&facts, sidecar.0, sidecar.1), DEMO);

    // Everything else moves: an app leaves the dock, a title grows. The whole
    // buffer is rewritten.
    let mut moved = Model::default();
    moved.favourites = vec!["kitty".to_owned()];
    moved.apply(window(
        1,
        "kitty",
        "a very much longer title than before",
        true,
        1,
    ));
    moved.apply(window(9, "late.example", "another", false, 2));
    facts.compose(&moved, &Strings::new(), &mut images);

    assert_eq!(range_of(&facts, "kitty"), kitty, "the range did not move");
    assert_eq!(
        text(&facts, kitty.0, kitty.1),
        "kitty",
        "and it still spells it"
    );
    assert_eq!(
        text(&facts, sidecar.0, sidecar.1),
        DEMO,
        "and a departed chip is still nameable"
    );
}

#[test]
fn the_compose_region_is_the_tail_word_aligned_and_zeroed() {
    let mut facts = facts();
    let mut images = Images::new();
    facts.compose(&fixture(), &Strings::new(), &mut images);
    let base = facts.buffer().header.compose_base as usize;
    assert_eq!(base % 4, 0, "the caustic writes whole words");
    assert_eq!(facts.strings().len(), base + COMPOSE_BYTES);
    assert!(
        facts.strings()[base..].iter().all(|byte| *byte == 0),
        "the caustic's own region arrives zeroed"
    );
    assert!(facts.strings().len() <= STRING_BYTES);
    assert_eq!(
        facts.buffer().header.string_words as usize,
        facts.strings().len() / 4,
        "and the header bounds the caustic's decode"
    );
}

// ---------------------------------------------------------------------------
// The rows arena (docs/abi.md, *The arenas*): what a sidecar sent, byte for byte
// ---------------------------------------------------------------------------

/// A sidecar's frame: a chip with a menu, a checkable row and a slider.
fn demo(value: f32) -> Rows {
    let mut builder = RowsBuilder::new(abi::APPLET_TRAY, 7);
    let chip = builder.item(
        1,
        Some("chip"),
        Some("audio-volume-high-symbolic"),
        -1.0,
        wire::RFLAG_HAS_MENU,
    );
    builder.menu(chip, 2, Some("Mute"), None, wire::RFLAG_CHECKABLE);
    builder.slider(chip, 6, Some("Volume"), value, 0.0, 1.0, 0);
    builder.build()
}

/// One frame's body as the caller would write it.
fn region(facts: &mut Facts, rows: &Rows) -> Vec<u8> {
    facts
        .rows_region(1, rows)
        .expect("the demo's body fits one region")
}

#[test]
fn a_rows_body_lands_in_its_region_byte_for_byte() {
    let rows = demo(0.25);
    let mut facts = facts();
    let body = region(&mut facts, &rows);

    // The header the writer gave, at the front.
    assert_eq!(&body[0..4], &abi::APPLET_TRAY.to_le_bytes());
    assert_eq!(&body[4..8], &(rows.rows.len() as u32).to_le_bytes());
    assert_eq!(&body[8..12], &(rows.bytes.len() as u32).to_le_bytes());
    assert_eq!(&body[12..16], &7u32.to_le_bytes(), "the revision");
    // Then the rows, at the wire's own stride, and then the blob — every byte
    // of it, because nothing here is a themed name the embassy resolves.
    assert_eq!(
        &body[16..16 + rows.rows.len() * 48],
        abi::slice_as_bytes(&rows.rows)
    );
    assert_eq!(&body[16 + rows.rows.len() * 48..], &rows.bytes[..]);
    assert_eq!(body.len(), 16 + rows.rows.len() * 48 + rows.bytes.len());
}

/// A row whose icon is a minted pixmap key goes into the region like any
/// other name — verbatim, key bytes and all (docs/abi.md, *Pixmap icons*).
#[test]
fn a_rows_pixmap_key_is_a_name_in_the_region() {
    let px = abi::IMAGE_SLOT_PX as usize;
    let key = wire::pixmap_key(TRAY, &vec![0x7fu8; px * px * 4]);
    let mut builder = RowsBuilder::new(abi::APPLET_TRAY, 1);
    builder.item(4, Some("dropbox"), Some(&key), -1.0, 0);
    let rows = builder.build();

    let mut facts = facts();
    let body = region(&mut facts, &rows);
    let blob = 16 + rows.rows.len() * 48;
    let row = rows.rows[0];
    let start = blob + row.icon_off as usize;
    assert_eq!(&body[start..start + row.icon_len as usize], key.as_bytes());
    assert_eq!(
        &body[blob..],
        &rows.bytes[..],
        "the blob is the writer's, byte for byte"
    );
}

#[test]
fn a_body_past_the_arena_is_refused_and_the_region_kept() {
    let mut facts = facts();
    let good = demo(0.25);
    let body = region(&mut facts, &good);

    // A frame whose blob alone is bigger than a region.
    let mut builder = RowsBuilder::new(abi::APPLET_TRAY, 2);
    let label = "x".repeat(abi::ROWS_ARENA_BYTES);
    builder.item(1, Some(&label), None, -1.0, 0);
    let huge = builder.build();
    let refused = facts.rows_region(1, &huge);
    assert!(refused.is_err(), "a body past the region is refused");
    assert!(
        refused.unwrap_err() > abi::ROWS_ARENA_BYTES,
        "and the witness carries the number"
    );

    // The region is what it was: the embassy wrote nothing.
    assert_eq!(facts.region(1), &body[..], "the region is left as it was");
}

#[test]
fn a_sidecar_that_goes_leaves_no_region_and_no_pixmaps() {
    let rgba = slot_rgba(9);
    let mut facts = facts();
    let mut images = Images::new();
    region(&mut facts, &demo(0.25));
    assert!(!facts.region(1).is_empty());
    // One key from the departing sidecar, one from another.
    facts.learn_pixmap(&mut images, DEMO, "rows_demo:0000000000000009", &rgba);
    facts.learn_pixmap(&mut images, TRAY, "caustics-tray:0000000000000009", &rgba);

    facts.forget_slot(1);
    assert!(
        facts.region(1).is_empty(),
        "the region is gone with the child"
    );
    assert!(
        !facts.pixmaps.contains_key("rows_demo:0000000000000009"),
        "and its pixmaps"
    );
    assert!(
        facts.pixmaps.contains_key("caustics-tray:0000000000000009"),
        "and nobody else's"
    );
    assert_eq!(facts.pixmap_order.len(), 1, "the order forgets it too");
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    // The slot's *name* stays: a departure witness quotes it a frame later.
    assert_ne!(facts.buffer().slots[1].name_len, 0);
}

/// A slot's worth of one flat colour.
fn slot_rgba(v: u8) -> Vec<u8> {
    vec![v; Images::slot_bytes()]
}

/// The keys the image directory names, as the caustic would read them.
fn directory(facts: &Facts) -> Vec<String> {
    let buffer = facts.buffer();
    buffer
        .images
        .iter()
        .take(buffer.header.image_count as usize)
        .map(|fact| text(facts, fact.name_off, fact.name_len))
        .collect()
}

/// A want for `name`, placed where the caustic would find it: a row's icon in
/// the demo sidecar's region.
fn want_row_icon(facts: &mut Facts, images: &mut Images, name: &str, kind: u32) -> Wanted {
    let mut builder = RowsBuilder::new(abi::APPLET_TRAY, 1);
    builder.item(1, None, Some(name), -1.0, 0);
    let rows = builder.build();
    region(facts, &rows);
    let blob = 16 + rows.rows.len() * 48;
    let row = rows.rows[0];
    facts.want_icon(
        images,
        abi::WantIcon {
            off: blob as u32 + row.icon_off,
            len: row.icon_len,
            kind,
            source: 1,
        },
    )
}

/// The tray's case (docs/abi.md, *Pixmap*): its `Pixmap` arrives before any build
/// has named the key, is kept, and the `REQ_ICON` the caustic's icon stitch (`caustics/bar_table.slang`) sends after the
/// glyph sidecar's `GLYPH_MISSING` lands it — with no request out.
#[test]
fn a_pixmap_nobody_asked_for_is_kept_and_a_want_lands_it_without_asking() {
    let key = "caustics-tray:00000000000000aa";
    let mut facts = facts();
    let mut images = Images::new();

    assert_eq!(
        facts.learn_pixmap(&mut images, TRAY, key, &slot_rgba(0xaa)),
        None,
        "no slot yet"
    );
    assert!(facts.pixmaps.contains_key(key), "but kept");
    assert_eq!(images.len(), 0);

    assert_eq!(
        want_row_icon(&mut facts, &mut images, key, wire::REQ_ICON),
        Wanted::Landed
    );
    assert!(
        facts.take_requests().is_empty(),
        "the store answered; no one was asked"
    );
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    assert_eq!(
        directory(&facts),
        vec![key.to_owned()],
        "listed under the bare key"
    );

    // Asked once: a second want is nothing.
    assert_eq!(
        want_row_icon(&mut facts, &mut images, key, wire::REQ_ICON),
        Wanted::Nothing
    );
}

#[test]
fn a_want_the_store_lacks_is_asked_of_a_sidecar() {
    let mut facts = facts();
    let mut images = Images::new();
    let key = "caustics-tray:00000000000000bb";
    assert_eq!(
        want_row_icon(&mut facts, &mut images, key, wire::REQ_ICON),
        Wanted::Asked
    );
    assert_eq!(
        facts.take_requests(),
        vec![(APPS_SIDECAR, wire::REQ_ICON, key.to_owned())]
    );
    // ...and the answer, when it comes, lands by the asked path.
    assert!(
        facts
            .learn_pixmap(&mut images, APPS_SIDECAR, key, &slot_rgba(0xbb))
            .is_some()
    );
}

/// An eviction un-asks a themed name (`compose`, `take_evicted`), and the next
/// want finds the store again: the tray is never asked twice for anything.
#[test]
fn an_evicted_icon_comes_back_from_the_store() {
    let mut facts = facts();
    let mut images = Images::new();
    let key = "caustics-tray:00000000000000cc";
    facts.learn_pixmap(&mut images, TRAY, key, &slot_rgba(0xcc));
    assert_eq!(
        want_row_icon(&mut facts, &mut images, key, wire::REQ_ICON),
        Wanted::Landed
    );
    facts.compose(&Model::default(), &Strings::new(), &mut images);

    // Fill every slot with newer pictures in later generations, so the key's
    // slot is the oldest and goes.
    for n in 0..abi::MAX_IMAGE_SLOTS {
        images.begin_generation();
        images.insert(&format!("filler-{n}"), &slot_rgba(1));
    }
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    assert!(!directory(&facts).contains(&key.to_owned()), "evicted");

    assert_eq!(
        want_row_icon(&mut facts, &mut images, key, wire::REQ_ICON),
        Wanted::Landed
    );
    assert!(
        facts.take_requests().is_empty(),
        "back from the store, not from a sidecar"
    );
}

/// The store is bounded: the 257th key lets the first go.
#[test]
fn the_store_lets_the_oldest_key_go_past_its_bound() {
    let mut facts = facts();
    let mut images = Images::new();
    let rgba = slot_rgba(5);
    for n in 0..=MAX_STORED_PIXMAPS {
        facts.learn_pixmap(&mut images, TRAY, &format!("caustics-tray:{n:016x}"), &rgba);
    }
    assert_eq!(facts.pixmaps.len(), MAX_STORED_PIXMAPS);
    assert_eq!(facts.pixmaps_evicted(), 1, "one key went, with its witness");
    assert!(
        !facts
            .pixmaps
            .contains_key(&format!("caustics-tray:{:016x}", 0)),
        "the oldest"
    );
    assert!(
        facts
            .pixmaps
            .contains_key(&format!("caustics-tray:{:016x}", MAX_STORED_PIXMAPS))
    );
    assert!(facts.stored_bytes() <= MAX_STORED_BYTES);

    // A key sent again is the newest, not the oldest.
    facts.learn_pixmap(
        &mut images,
        TRAY,
        &format!("caustics-tray:{:016x}", 1),
        &rgba,
    );
    facts.learn_pixmap(&mut images, TRAY, "caustics-tray:ffffffffffffffff", &rgba);
    assert!(
        facts
            .pixmaps
            .contains_key(&format!("caustics-tray:{:016x}", 1)),
        "refreshed, so kept"
    );
    assert!(
        !facts
            .pixmaps
            .contains_key(&format!("caustics-tray:{:016x}", 2)),
        "the next oldest went"
    );
}

/// An app id's picture evicted from its slot is un-asked by the next compose
/// — the listing does not refill it — and the caustic's next want
/// lands it from the store with no request out, because the `App` answer is
/// already here.
#[test]
fn an_evicted_app_icon_is_unasked_and_the_next_want_lands_it_from_the_store() {
    let mut facts = facts();
    let mut images = Images::new();
    facts.set_app(app_frame("kitty", "Kitty", 0));
    assert_eq!(
        facts.learn_pixmap(&mut images, APPS_SIDECAR, "kitty", &slot_rgba(7)),
        None,
        "kept"
    );
    assert_eq!(
        want_row_icon(&mut facts, &mut images, "kitty", wire::REQ_APP),
        Wanted::Landed
    );
    assert!(
        facts.take_requests().is_empty(),
        "the answer and the pixels are both here"
    );
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    assert!(directory(&facts).contains(&"kitty".to_owned()));

    // Newer pictures in later generations, none of them drawn: kitty's slot
    // is the oldest and goes.
    for n in 0..abi::MAX_IMAGE_SLOTS {
        images.begin_generation();
        images.insert(&format!("filler-{n}"), &slot_rgba(1));
    }
    assert!(images.slot_of("kitty").is_none(), "evicted");
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    assert!(
        !directory(&facts).contains(&"kitty".to_owned()),
        "not refilled by the listing"
    );
    assert!(!facts.apps_asked.contains("kitty"), "un-asked");

    assert_eq!(
        want_row_icon(&mut facts, &mut images, "kitty", wire::REQ_APP),
        Wanted::Landed
    );
    assert!(
        facts.take_requests().is_empty(),
        "back from the store, not from caustics-apps"
    );
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    assert!(
        directory(&facts).contains(&"kitty".to_owned()),
        "and named again"
    );
}

/// Every slot holds a key the bar asked about and every compose lists, and
/// nothing draws any of them. A listing that stamped every slot with the
/// current generation would refuse a 65th want ("all 64 slots are drawn this
/// frame") for the life of the process; the oldest undrawn picture must go
/// and the new one land.
#[test]
fn a_full_array_nothing_draws_gives_way_to_a_new_want() {
    let mut facts = facts();
    let mut images = Images::new();
    for n in 0..abi::MAX_IMAGE_SLOTS {
        let key = format!("caustics-tray:{n:016x}");
        facts.learn_pixmap(&mut images, TRAY, &key, &slot_rgba(n as u8));
        assert_eq!(
            want_row_icon(&mut facts, &mut images, &key, wire::REQ_ICON),
            Wanted::Landed
        );
        facts.compose(&Model::default(), &Strings::new(), &mut images);
    }
    assert_eq!(directory(&facts).len(), abi::MAX_IMAGE_SLOTS, "all listed");

    let key = "caustics-tray:00000000000000ff";
    facts.learn_pixmap(&mut images, TRAY, key, &slot_rgba(0xff));
    assert_eq!(
        want_row_icon(&mut facts, &mut images, key, wire::REQ_ICON),
        Wanted::Landed
    );
    assert_eq!(images.refusals(), 0, "the listing kept nothing young");
    assert_eq!(images.evictions(), 1);
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    let listed = directory(&facts);
    assert!(listed.contains(&key.to_owned()), "the new picture is named");
    assert!(
        !listed.contains(&format!("caustics-tray:{:016x}", 0)),
        "the oldest went"
    );
    assert_eq!(listed.len(), abi::MAX_IMAGE_SLOTS);

    // And when the caustic says it draws every slot, a want is refused rather
    // than taking a picture off the bar.
    images.touch_drawn(&[u32::MAX; abi::IMAGE_DRAWN_WORDS]);
    let key = "caustics-tray:00000000000000fe";
    facts.learn_pixmap(&mut images, TRAY, key, &slot_rgba(0xfe));
    assert_eq!(
        want_row_icon(&mut facts, &mut images, key, wire::REQ_ICON),
        Wanted::Nothing
    );
    assert_eq!(images.refusals(), 1);
}

/// The idempotence the arena relies on is the model's: an identical frame is
/// not applied, so `publish_rows` is never reached and nothing is written.
#[test]
fn an_identical_frame_is_no_write_at_all() {
    let mut model = Model::default();
    assert!(model.apply(Update::Rows {
        sidecar: DEMO.to_owned(),
        rows: demo(0.25)
    }));
    assert!(
        !model.apply(Update::Rows {
            sidecar: DEMO.to_owned(),
            rows: demo(0.25)
        }),
        "the same frame changes nothing, so no region is written and no command sent"
    );
    assert!(
        model.apply(Update::Rows {
            sidecar: DEMO.to_owned(),
            rows: demo(0.5)
        }),
        "a slider that moved is a write — of the region, and nothing else"
    );
}

// ---------------------------------------------------------------------------
// The answers arena (docs/abi.md, *The arenas*)
// ---------------------------------------------------------------------------

/// An `App` frame as `caustics-apps` writes one.
fn app_frame(app_id: &str, name: &str, flags: u32) -> wire::App {
    wire::App {
        flags,
        app_id: app_id.to_owned(),
        name: name.to_owned(),
        icon: String::new(),
        path: format!("/usr/share/applications/{app_id}.desktop"),
        exec: vec![app_id.to_owned(), "--flag".to_owned()],
    }
}

/// The names the arena carries, glyph half then app half, read the way the
/// caustic reads them.
fn arena_keys(facts: &Facts) -> (Vec<String>, Vec<String>) {
    let bytes = facts.answers();
    let split = facts.buffer().header.glyph_bytes as usize;
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let mut glyphs = Vec::new();
    let mut at = 0;
    while at + 16 <= split {
        let len = word(at + 8);
        glyphs.push(String::from_utf8_lossy(&bytes[at + 16..at + 16 + len]).into_owned());
        at += 16 + len.next_multiple_of(4);
    }
    let mut apps = Vec::new();
    at = split;
    while at + 32 <= bytes.len() {
        let app_len = word(at + 4);
        let fields = app_len + word(at + 8) + word(at + 12) + word(at + 16) + word(at + 20);
        apps.push(String::from_utf8_lossy(&bytes[at + 32..at + 32 + app_len]).into_owned());
        at += 32 + fields.next_multiple_of(4);
    }
    (glyphs, apps)
}

/// A miss the sidecar later takes back (its negative answers expire, so a
/// theme or app installed later is found) reaches the
/// caustic with **nothing here forgetting anything**: the asked-sets keep one
/// question per name, and the answer that arrives unasked replaces the one
/// held — a `GLYPH_MISSING` becomes an id, a `REQ_ICON` that was answered
/// with silence lands its pixels in a slot, an `APP_MISSING` becomes an entry.
#[test]
fn an_answer_that_arrives_unasked_replaces_a_miss() {
    let mut facts = facts();
    let mut images = Images::new();
    let name = "steam_tray_mono";

    assert!(facts.learn_glyph(name, None));
    assert_eq!(
        want_row_icon(&mut facts, &mut images, name, wire::REQ_ICON),
        Wanted::Asked
    );
    assert_eq!(
        facts.take_requests(),
        vec![(APPS_SIDECAR, wire::REQ_ICON, name.to_owned())]
    );
    // `caustics-apps` answered silence. Half a minute later the file exists.
    assert!(
        facts
            .learn_pixmap(&mut images, APPS_SIDECAR, name, &slot_rgba(0x5a))
            .is_some()
    );
    facts.compose(&Model::default(), &Strings::new(), &mut images);
    assert_eq!(
        directory(&facts),
        vec![name.to_owned()],
        "the pixels are where the caustic's icon stitch looks"
    );
    assert_eq!(
        want_row_icon(&mut facts, &mut images, name, wire::REQ_ICON),
        Wanted::Nothing
    );
    assert!(
        facts.take_requests().is_empty(),
        "and no one is asked again"
    );

    // `caustics-glyphs` found curves for it after all: the id replaces the miss.
    assert!(
        facts.learn_glyph(name, Some(4242)),
        "a changed answer is a rebuild"
    );
    let bytes = facts.answers();
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    assert_eq!((word(0), word(4)), (4242, 0), "gid, and no GLYPH_MISSING");
    assert_eq!(
        arena_keys(&facts).0,
        vec![name.to_owned()],
        "one answer per name, replaced"
    );

    facts.set_app(app_frame(
        "steam_app_4831170",
        "steam_app_4831170",
        wire::APP_MISSING,
    ));
    facts.set_app(app_frame("steam_app_4831170", "Dwarf Fortress", 0));
    let app = facts.app("steam_app_4831170").expect("an answer");
    assert_eq!((app.name.as_str(), app.flags), ("Dwarf Fortress", 0));
}

#[test]
fn answers_append_in_order_and_a_font_drops_the_glyphs() {
    let mut facts = facts();
    assert!(facts.learn_glyph("audio-volume-high-symbolic", Some(700)));
    assert!(
        !facts.learn_glyph("audio-volume-high-symbolic", Some(700)),
        "the same answer"
    );
    assert!(
        facts.learn_glyph("no-such-icon", None),
        "and a refusal is an answer too"
    );
    facts.set_app(app_frame("kitty", "Kitty", 0));
    facts.set_app(app_frame("org.mozilla.firefox", "Firefox", 0));

    let (glyphs, apps) = arena_keys(&facts);
    assert_eq!(glyphs, vec!["audio-volume-high-symbolic", "no-such-icon"]);
    assert_eq!(apps, vec!["kitty", "org.mozilla.firefox"]);
    assert_eq!(
        facts.buffer().header.answer_bytes as usize,
        facts.answers().len(),
        "and the header says how much of the arena is live"
    );

    // Whole-state: a second answer for the same app id replaces the first.
    facts.set_app(app_frame("kitty", "Kitty Terminal", 0));
    assert_eq!(arena_keys(&facts).1, vec!["kitty", "org.mozilla.firefox"]);
    assert_eq!(
        facts.app("kitty").expect("an answer").name,
        "Kitty Terminal"
    );

    // A `Font` frame is a different glyph space: every id learned against the
    // last one is meaningless and leaves the arena. The app answers stay.
    facts.forget_glyphs();
    let (glyphs, apps) = arena_keys(&facts);
    assert!(glyphs.is_empty(), "every glyph answer went with the font");
    assert_eq!(apps, vec!["kitty", "org.mozilla.firefox"]);
}

#[test]
fn the_arena_drops_the_oldest_when_it_would_overflow() {
    let mut facts = facts();
    // Each answer is 16 B of head plus a name padded to a word; a name of
    // 1000 bytes plus a four-digit ordinal is a round 1020 an entry.
    let long = "n".repeat(996);
    let per = 16 + 1000;
    let fits = abi::ANSWERS_BYTES / per;
    for at in 0..fits {
        facts.learn_glyph(&format!("{at:04}{long}"), Some(at as u32));
    }
    assert_eq!(arena_keys(&facts).0.len(), fits);
    assert!(facts.answers().len() <= abi::ANSWERS_BYTES);

    facts.learn_glyph(&format!("{fits:04}{long}"), Some(0));
    let (glyphs, _) = arena_keys(&facts);
    assert!(
        facts.answers().len() <= abi::ANSWERS_BYTES,
        "the arena never overflows"
    );
    assert_eq!(glyphs.len(), fits, "the oldest went");
    assert!(glyphs[0].starts_with("0001"), "and it was the first one in");
    assert!(
        glyphs
            .last()
            .expect("one")
            .starts_with(&format!("{fits:04}"))
    );
}

// ---------------------------------------------------------------------------
// The questions, and where a want's bytes are read from
// ---------------------------------------------------------------------------

#[test]
fn a_want_is_read_out_of_the_buffer_it_names() {
    let mut facts = facts();
    let mut images = Images::new();
    let mut model = fixture();
    model.apply(Update::Rows {
        sidecar: DEMO.to_owned(),
        rows: demo(0.25),
    });
    let rows = model.applets[DEMO].clone();
    facts.rows_region(1, &rows).expect("it fits");
    facts.set_app(app_frame("kitty", "Kitty", 0));
    facts.compose(&model, &Strings::new(), &mut images);

    // A row's icon name, in the slot's own region: the offset is relative to
    // the region, which is what the caustic sends back.
    let blob = 16 + rows.rows.len() * 48;
    let icon = rows.rows[0];
    let mut want = |facts: &mut Facts, want: abi::WantIcon| facts.want_icon(&mut images, want);
    assert_eq!(
        want(
            &mut facts,
            abi::WantIcon {
                off: blob as u32 + icon.icon_off,
                len: icon.icon_len,
                kind: wire::REQ_GLYPH,
                source: 1,
            }
        ),
        Wanted::Asked
    );
    // An app id, in the strings buffer. Its `App` answer is already here and
    // the store has no pixels, so this one is asked: the pixmap is what it
    // wants.
    let entry = facts.buffer().apps[0];
    let app_want = abi::WantIcon {
        off: entry.id_off,
        len: entry.id_len,
        kind: wire::REQ_APP,
        source: abi::SRC_STRINGS,
    };
    assert_eq!(want(&mut facts, app_want), Wanted::Asked);
    // And an `AppBody.icon`, in the answers arena — here, the app id itself,
    // which is at a known offset in the one answer.
    assert_eq!(
        want(
            &mut facts,
            abi::WantIcon {
                off: 32,
                len: 5,
                kind: wire::REQ_ICON,
                source: abi::SRC_ANSWERS,
            }
        ),
        Wanted::Asked
    );

    let asked = facts.take_requests();
    assert_eq!(
        asked,
        vec![
            (
                GLYPHS_SIDECAR,
                wire::REQ_GLYPH,
                "audio-volume-high-symbolic".to_owned()
            ),
            (APPS_SIDECAR, wire::REQ_ICON, "kitty".to_owned()),
        ],
        "an app id whose `App` answer is held is not asked for it again"
    );

    // One question per name for the life of the process.
    assert_eq!(want(&mut facts, app_want), Wanted::Nothing);
    // A range that names nothing this side wrote is dropped.
    assert_eq!(
        want(
            &mut facts,
            abi::WantIcon {
                off: 9_000_000,
                len: 4,
                kind: wire::REQ_GLYPH,
                source: abi::SRC_STRINGS,
            }
        ),
        Wanted::Nothing
    );
    assert!(facts.take_requests().is_empty());
}

#[test]
fn a_pixmap_is_filed_under_the_name_it_was_asked_by() {
    let px = abi::IMAGE_SLOT_PX as usize;
    let rgba = vec![3u8; px * px * 4];
    let mut facts = facts();
    let mut images = Images::new();

    // Nothing asked: no slot, because no build will look under the name yet
    // — but kept, for the want that will.
    assert_eq!(
        facts.learn_pixmap(&mut images, APPS_SIDECAR, "stranger", &rgba),
        None
    );
    assert!(facts.pixmaps.contains_key("stranger"));

    // A `REQ_ICON` name is filed under the themed key and published bare —
    // the rule `Facts::learn_pixmap` states, which is why a colour tray icon draws at all.
    assert_eq!(
        facts.want_icon(
            &mut images,
            abi::WantIcon {
                off: 0,
                len: 0,
                kind: wire::REQ_ICON,
                source: abi::SRC_STRINGS,
            }
        ),
        Wanted::Nothing
    );
    facts.icons_asked.insert("dropbox".to_owned());
    assert!(
        facts
            .learn_pixmap(&mut images, APPS_SIDECAR, "dropbox", &rgba)
            .is_some()
    );
    // ...and a `REQ_APP` one bare, as *Pixmap* keys it.
    facts.apps_asked.insert("kitty".to_owned());
    assert!(
        facts
            .learn_pixmap(&mut images, APPS_SIDECAR, "kitty", &rgba)
            .is_some()
    );

    facts.compose(&Model::default(), &Strings::new(), &mut images);
    let buffer = facts.buffer().clone();
    let keys: Vec<String> = buffer
        .images
        .iter()
        .take(buffer.header.image_count as usize)
        .map(|fact| text(&facts, fact.name_off, fact.name_len))
        .collect();
    assert!(
        keys.contains(&"dropbox".to_owned()),
        "published under the bare name"
    );
    assert!(keys.contains(&"kitty".to_owned()));
}

#[test]
fn a_flagged_string_is_one_cover_request_and_not_two() {
    let mut facts = facts();
    assert!(
        facts.ask_cover("日本語"),
        "a first flagged run is a question"
    );
    assert!(
        !facts.ask_cover("日本語"),
        "and the same run is never asked about twice"
    );
    assert!(!facts.ask_cover(""), "an empty run is nothing to cover");
    let asked = facts.take_requests();
    assert_eq!(
        asked,
        vec![(GLYPHS_SIDECAR, wire::REQ_COVER, "日本語".to_owned())]
    );
}

/// A VPN's exit-node row: two regional indicators and a name. The font has
/// neither, Noto Emoji ligates the pair to a flag, and without the source
/// `ACT_COVER` carries no row label would ever be asked about.
#[test]
fn a_row_label_is_covered_out_of_its_slots_region() {
    const FLAG: &str = "🇦🇺 Australia";
    let mut facts = facts();
    let mut builder = RowsBuilder::new(abi::APPLET_OTHER, 1);
    let chip = builder.item(1, Some("tailscale"), None, -1.0, wire::RFLAG_HAS_MENU);
    builder.menu(chip, 2, Some("Exit node"), None, 0);
    builder.menu(chip, 3, Some(FLAG), None, 0);
    let rows = builder.build();
    let slot = facts.slot_of(DEMO).expect("the demo has a slot");
    facts.rows_region(slot, &rows).expect("it fits");
    facts.compose(&fixture(), &Strings::new(), &mut Images::new());

    // The caustic's range is relative to the region, as `ACT_WANT_ICON`'s is:
    // past the header and the rows, into the blob, at the row's own offset.
    let blob = (16 + rows.rows.len() * size_of::<wire::Row>()) as u32;
    let row = rows.rows[2];
    let range = Cover {
        off: blob + row.label_off,
        len: row.label_len,
        source: slot,
    };
    assert_eq!(facts.cover(range), Some(FLAG.to_owned()));
    assert_eq!(
        facts.take_requests(),
        vec![(GLYPHS_SIDECAR, wire::REQ_COVER, FLAG.to_owned())],
        "one REQ_COVER to caustics-glyphs, with exactly the label's bytes"
    );
    // Once per string for the life of the process, whichever buffer it was in.
    assert_eq!(facts.cover(range), None);
    // The same range against another buffer is another buffer's bytes: the
    // source word is the whole of the difference.
    assert_ne!(
        facts
            .bytes_at(abi::SRC_STRINGS, range.off, range.len)
            .as_deref(),
        Some(FLAG)
    );
    // A slot that has written nothing, and a source that is no buffer, name
    // nothing.
    assert_eq!(facts.cover(Cover { source: 0, ..range }), None);
    assert_eq!(
        facts.cover(Cover {
            source: abi::SRC_ANSWERS + 1,
            ..range
        }),
        None
    );
    assert!(facts.take_requests().is_empty());
}

/// A title is still a range of the strings buffer, and still one question.
#[test]
fn a_title_is_covered_out_of_the_strings_buffer() {
    let mut facts = facts();
    let mut model = fixture();
    model.apply(window(5, "kitty", "日本語のタイトル", false, 3));
    facts.compose(&model, &Strings::new(), &mut Images::new());
    let buffer = facts.buffer().clone();
    let title = buffer
        .windows
        .iter()
        .take(buffer.header.window_count as usize)
        .find(|fact| text(&facts, fact.title_off, fact.title_len) == "日本語のタイトル")
        .expect("the window is in the facts");
    let range = Cover {
        off: title.title_off,
        len: title.title_len,
        source: abi::SRC_STRINGS,
    };
    assert_eq!(facts.cover(range).as_deref(), Some("日本語のタイトル"));
    assert_eq!(
        facts.take_requests(),
        vec![(
            GLYPHS_SIDECAR,
            wire::REQ_COVER,
            "日本語のタイトル".to_owned()
        )]
    );
}

/// A string past the wire's name cap loses its tail, on a character boundary.
#[test]
fn a_cover_past_the_wires_cap_is_cut_on_a_character() {
    let mut facts = facts();
    let long = "é".repeat(wire::MAX_NAME_LEN);
    let mut model = fixture();
    model.apply(window(5, "kitty", &long, false, 3));
    facts.compose(&model, &Strings::new(), &mut Images::new());
    let buffer = facts.buffer().clone();
    let title = buffer
        .windows
        .iter()
        .take(buffer.header.window_count as usize)
        .find(|fact| text(&facts, fact.title_off, 2) == "é")
        .expect("the window is in the facts");
    assert!(
        title.title_len as usize > wire::MAX_NAME_LEN,
        "the facts carry the whole title"
    );
    let sent = facts
        .cover(Cover {
            off: title.title_off,
            len: title.title_len,
            source: abi::SRC_STRINGS,
        })
        .expect("a first question");
    assert!(sent.len() <= wire::MAX_NAME_LEN);
    assert!(
        sent.chars().all(|c| c == 'é'),
        "cut on a character boundary"
    );
}

#[test]
fn the_cover_questions_are_bounded() {
    let mut facts = facts();
    for at in 0..MAX_COVERED {
        assert!(facts.ask_cover(&format!("run-{at}")));
    }
    assert!(
        !facts.ask_cover("one too many"),
        "a bar whose titles produced a thousand unshapeable runs is looking at a font"
    );
    assert_eq!(facts.take_requests().len(), MAX_COVERED);
}
