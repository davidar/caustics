//! The translation: what the bus said, as one `Rows` frame (docs/abi.md
//! *Rows*).
//!
//! Pure, and the reason the crate is a library: [`build`] takes the items in
//! registration order and an [`Ids`] allocator and returns the frame plus the
//! table that says what each row id *does*. Nothing in here connects to
//! anything, so `tray/tests/` drives it with a tree it built by hand or one it
//! deserialised from a recorded `GetLayout` reply.
//!
//! Three decisions live here and nowhere else:
//!
//! - **Ids.** A row id must be unique in the frame and stable across frames
//!   while the item lives (docs/abi.md, *Rows*). [`Ids`] is a registry keyed by the thing
//!   itself — an item's address, or an item's address and a dbusmenu id — and
//!   allocates a fresh number the first time it sees one and never again. An action that arrives a frame late must still name something.
//! - **What does not fit.** The wire bounds a frame at [`MAX_ROWS`] rows and
//!   the bar's region of the rows arena at [`ROWS_ARENA_BYTES`], and a frame
//!   past either is refused whole — so a frame that would break one is
//!   trimmed *here*, with a witness, rather than published and dropped.
//! - **Pixels are not rows.** A pixmap icon is fitted into
//!   [`crate::item::WIRE_PIXMAP`] squared by [`crate::item::fit_to_slot`] and
//!   goes into a `Pixmap` frame of its own, keyed `caustics-tray:<hash of the
//!   texels>` (docs/abi.md, *Pixmap*); the row carries the key. [`Built::pixmaps`] is what
//!   this build named, and `lib.rs` sends each one once, before the frame
//!   that names it. A full tray frame is names and labels — about 10 KB.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SECONDARY,
    APPLET_TRAY, ROWS_ARENA_BYTES,
};
use caustics_bar_abi::wire::{
    ActionBody, Frame, MAX_ROWS, Pixmap, RFLAG_ATTENTION, RFLAG_CHECKABLE, RFLAG_CHECKED,
    RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_LABEL_TITLE, RFLAG_RADIO, RFLAG_SUBMENU, Row, RowRef,
    Rows, RowsBuilder, WIRE_MAX_BODY, pixmap_key,
};

use crate::item::{WIRE_PIXMAP, fit_to_slot};
use crate::model::{Icon, Menu, MenuItem, Toggle, TrayItem, TrayStatus};

/// What this sidecar's minted `Pixmap` keys start with (docs/abi.md, *Pixmap*): its own name,
/// which is what keeps them apart from every other sidecar's.
pub const KEY_SIDECAR: &str = "caustics-tray";

/// How many blob bytes a frame can carry once every row is accounted for. The
/// worst case is what has to fit, because a frame is trimmed while it is being
/// built and the row count is not known until the end.
///
/// Bounded by the bar's region of the rows arena rather than by the wire's
/// body cap: the region is the smaller of the two (64 KiB against the cap's
/// 256 MiB, which is sized for a canvas picture), and
/// a frame past it is one the bar refuses whole and keeps the last one that
/// fitted: a menu that silently stops updating.
const BLOB_BUDGET: usize = {
    let cap = if ROWS_ARENA_BYTES < WIRE_MAX_BODY {
        ROWS_ARENA_BYTES
    } else {
        WIRE_MAX_BODY
    };
    cap - 16 - MAX_ROWS * size_of::<Row>()
};

/// What a row id names. The routing table [`build`] returns, and the whole of
/// what the sidecar remembers about a row between frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A bar item: SNI `Activate` / `SecondaryActivate` on this address key.
    Item(String),
    /// A menu row: dbusmenu `AboutToShow(parent)` then `Event(id, …)` on the
    /// item at this address key. `parent` is the row's parent dbusmenu id, 0
    /// for a row at the root.
    MenuRow { key: String, id: i32, parent: i32 },
}

/// One frame, and what its ids mean.
#[derive(Clone, Debug, Default)]
pub struct Built {
    pub rows: Rows,
    pub targets: HashMap<u32, Target>,
    /// Every pixmap a row of this frame names, as `(key, rgba)` — fitted to
    /// [`WIRE_PIXMAP`] squared, straight RGBA8, in the order first named and
    /// once each however many rows share it. `lib.rs` sends the ones it has
    /// not sent before the `Rows` frame goes out (docs/abi.md, *Pixmap*).
    pub pixmaps: Vec<(String, Vec<u8>)>,
}

/// A stable row id per thing, for the life of the process.
///
/// Never reused, including after an item unregisters: an id that came back
/// meaning something else would turn a click that raced a frame into a click
/// on the wrong row, and ids are cheap.
#[derive(Debug, Default)]
pub struct Ids {
    next: u32,
    map: HashMap<Key, u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Item(String),
    Row(String, i32),
}

impl Ids {
    /// The id for `key`, allocating on first sight. Starts at 1: 0 is a fine
    /// id on the wire, but a zero in a witness reads like "none".
    fn get(&mut self, key: Key) -> u32 {
        if let Some(id) = self.map.get(&key) {
            return *id;
        }
        self.next += 1;
        self.map.insert(key, self.next);
        self.next
    }

    pub fn item(&mut self, key: &str) -> u32 {
        self.get(Key::Item(key.to_owned()))
    }

    pub fn row(&mut self, key: &str, id: i32) -> u32 {
        self.get(Key::Row(key.to_owned(), id))
    }

    /// How many ids have been handed out — the tests' way of asking whether a
    /// second frame allocated anything.
    pub fn allocated(&self) -> u32 {
        self.next
    }
}

/// One item as the frame builder needs it: its key, what its task last read,
/// and its menu if it has fetched one.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub key: &'a str,
    pub item: &'a TrayItem,
    pub menu: Option<&'a Menu>,
}

/// The whole picture, whole-state, every time.
///
/// `note` takes one witness line per thing that did not fit; it is called at
/// most a handful of times per frame and never once per row.
pub fn build(
    entries: &[Entry<'_>],
    ids: &mut Ids,
    revision: u32,
    note: &mut dyn FnMut(&str),
) -> Built {
    let mut out = Builder {
        rows: RowsBuilder::new(APPLET_TRAY, revision),
        targets: HashMap::new(),
        pixmaps: Vec::new(),
        count: 0,
        blob: 0,
        full: false,
        note,
    };

    for entry in entries {
        // `Passive` is SNI's "I exist but have nothing to say"; this process leaves it out of the frame because it is the one that knows what Passive means.
        if entry.item.status == TrayStatus::Passive {
            continue;
        }
        let Some(parent) = out.item(entry, ids) else {
            break;
        };
        if let Some(menu) = entry.menu {
            if !out.menu(&menu.items, parent, entry.key, 0, ids) {
                break;
            }
        }
    }

    Built {
        rows: out.rows.build(),
        targets: out.targets,
        pixmaps: out.pixmaps,
    }
}

struct Builder<'n> {
    rows: RowsBuilder,
    targets: HashMap<u32, Target>,
    pixmaps: Vec<(String, Vec<u8>)>,
    count: usize,
    blob: usize,
    full: bool,
    note: &'n mut dyn FnMut(&str),
}

impl Builder<'_> {
    /// Room for one more row, or the one witness that says there is not.
    fn room(&mut self) -> bool {
        if self.count < MAX_ROWS {
            self.count += 1;
            return true;
        }
        if !self.full {
            self.full = true;
            (self.note)(&format!(
                "frame full at {MAX_ROWS} rows; the rest is not published"
            ));
        }
        false
    }

    /// The blob bytes a label and an icon would cost, if they fit. An icon
    /// that does not is dropped and the row keeps its label; a label that does
    /// not is dropped too, which is a frame so far past reasonable that the
    /// witness is the point.
    fn fits(&mut self, bytes: usize, what: &str) -> bool {
        if self.blob + bytes <= BLOB_BUDGET {
            self.blob += bytes;
            return true;
        }
        (self.note)(&format!("{what} dropped: the frame's blob is full"));
        false
    }

    /// A pixmap as the texels a slot holds — [`WIRE_PIXMAP`] squared, fitted
    /// by [`fit_to_slot`] with one witness when it had to be, and borrowed as
    /// it is when it already was — or `None` with a witness when the bytes
    /// are not the size they say.
    fn fitted<'a>(
        &mut self,
        width: u32,
        height: u32,
        rgba: &'a [u8],
        what: &str,
    ) -> Option<Cow<'a, [u8]>> {
        if width == 0 || height == 0 || rgba.len() != (width as usize) * (height as usize) * 4 {
            (self.note)(&format!(
                "{what}: a {width}x{height} pixmap is {} bytes",
                rgba.len()
            ));
            return None;
        }
        if width == WIRE_PIXMAP && height == WIRE_PIXMAP {
            return Some(Cow::Borrowed(rgba));
        }
        let (w, h, fitted) = fit_to_slot(width, height, rgba)?;
        (self.note)(&format!("{what}: pixmap {width}x{height} → {w}x{h}"));
        Some(Cow::Owned(fitted))
    }

    /// A model icon as the name the row carries, or `None`.
    ///
    /// A themed name crosses as it is. A pixmap crosses as a **key** —
    /// `caustics-tray:<fnv1a64 of the fitted texels>` — and its texels go into
    /// [`Built::pixmaps`] for a `Pixmap` frame of their own (docs/abi.md, *Pixmap*), once per
    /// key per frame however many rows share it.
    fn icon<'a>(&mut self, icon: &'a Icon, what: &str) -> Option<Cow<'a, str>> {
        match icon {
            Icon::None => None,
            Icon::Themed { name, theme_path } => {
                if let Some(dir) = theme_path {
                    // Step 4 of the chain (`crate::icon`): no theme, not in
                    // the item's own directory, no pixels beside it. The name
                    // crosses anyway so the bar's placeholder stands in for
                    // it; this is the witness that says why.
                    (self.note)(&format!(
                        "{what}: nothing has icon {name:?} (looked in {dir} too)"
                    ));
                }
                self.fits(name.len(), &format!("{what} icon name"))
                    .then_some(Cow::Borrowed(name.as_str()))
            }
            Icon::Pixmap {
                width,
                height,
                rgba,
            } => {
                let texels = self.fitted(*width, *height, rgba, what)?;
                let key = pixmap_key(KEY_SIDECAR, &texels);
                if !self.fits(key.len(), &format!("{what} pixmap key")) {
                    return None;
                }
                if !self.pixmaps.iter().any(|(held, _)| *held == key) {
                    self.pixmaps.push((key.clone(), texels.into_owned()));
                }
                Some(Cow::Owned(key))
            }
        }
    }

    /// A bar item. Its label is the item's **title** under
    /// `RFLAG_LABEL_TITLE`: the caustic never draws it
    /// beside the icon — a tray item is an icon chip — but wears its first
    /// character while the icon is unresolved, so an item whose icon nothing
    /// answers for is a letter and not a blank, hoverable gap. First line
    /// only, as every label here is.
    fn item(&mut self, entry: &Entry<'_>, ids: &mut Ids) -> Option<RowRef> {
        if !self.room() {
            return None;
        }
        let id = ids.item(entry.key);
        let icon = self.icon(&entry.item.icon, entry.key);
        let title = first_line(&entry.item.title);
        let title = (!title.is_empty() && self.fits(title.len(), &format!("{} title", entry.key)))
            .then_some(title);
        let mut flags = RFLAG_LABEL_TITLE;
        if entry.item.has_menu {
            flags |= RFLAG_HAS_MENU;
        }
        if entry.item.status == TrayStatus::NeedsAttention {
            flags |= RFLAG_ATTENTION;
        }
        self.targets.insert(id, Target::Item(entry.key.to_owned()));
        Some(self.rows.item(id, title, icon.as_deref(), -1.0, flags))
    }

    /// One level of the menu, then its children — parents before children,
    /// which is the reader's own invariant and what makes the tree acyclic by
    /// construction. Returns `false` once the frame is full.
    fn menu(
        &mut self,
        rows: &[MenuItem],
        parent: RowRef,
        key: &str,
        parent_id: i32,
        ids: &mut Ids,
    ) -> bool {
        let mut descend: Vec<(RowRef, &MenuItem)> = Vec::new();
        for row in rows {
            // `visible = false` is a row the server is hiding, not a row it
            // is disabling; it has no place in the frame at all.
            if !row.visible {
                continue;
            }
            if !self.room() {
                return false;
            }
            let id = ids.row(key, row.id);
            self.targets.insert(
                id,
                Target::MenuRow {
                    key: key.to_owned(),
                    id: row.id,
                    parent: parent_id,
                },
            );

            if row.separator {
                self.rows.separator(parent, id);
                continue;
            }

            let mut flags = 0;
            if !row.enabled {
                flags |= RFLAG_DISABLED;
            }
            match row.toggle {
                Toggle::None => {}
                Toggle::Checkmark(on) => {
                    flags |= RFLAG_CHECKABLE;
                    if on {
                        flags |= RFLAG_CHECKED;
                    }
                }
                // dbusmenu's `toggle-type` says which of the two a row is, and
                // the wire carries the difference through: a radio row draws
                // as a radio rather than as a check.
                Toggle::Radio(on) => {
                    flags |= RFLAG_CHECKABLE | RFLAG_RADIO;
                    if on {
                        flags |= RFLAG_CHECKED;
                    }
                }
            }
            if !row.children.is_empty() {
                flags |= RFLAG_SUBMENU;
            }

            // Multi-line labels exist on the live bus — tailscale's account
            // row is the account and then the tailnet, as in
            // "user@example.com\nexample.com" — and a menu row is one line.
            // A label is text and cutting it is the writer's job, not the reader's.
            let label = first_line(&row.label);
            let label = self.fits(label.len(), "a menu label").then_some(label);
            let what = format!("{key}:{}", row.id);
            let icon = self.icon(&row.icon, &what);
            let handle = self.rows.menu(parent, id, label, icon.as_deref(), flags);
            if !row.children.is_empty() {
                descend.push((handle, row));
            }
        }
        for (handle, row) in descend {
            if !self.menu(&row.children, handle, key, row.id, ids) {
                return false;
            }
        }
        true
    }
}

/// The `Pixmap` keys this process has written (docs/abi.md, *Pixmap*), and the frames a build
/// still owes the bar.
///
/// A key is its texels' hash, so one sent is never sent again: an icon that
/// comes back unchanged costs nothing, and one that changed is a new key and
/// one 16 KiB frame. A restarted tray starts from an empty set and resends,
/// which is right — the bar forgets what a sidecar sent when it exits.
#[derive(Debug, Default)]
pub struct Sent {
    keys: HashSet<String>,
}

impl Sent {
    /// One `Pixmap` frame per key of `pixmaps` not yet sent, in order, each
    /// marked sent by the call. The caller writes them **before** the `Rows`
    /// frame that names them, so the bar holds the texels before it reads the
    /// name.
    pub fn owed(&mut self, pixmaps: Vec<(String, Vec<u8>)>) -> Vec<Frame> {
        let mut frames = Vec::new();
        for (key, rgba) in pixmaps {
            if !self.keys.insert(key.clone()) {
                continue;
            }
            frames.push(Frame::Pixmap(Pixmap {
                key,
                width: WIRE_PIXMAP,
                height: WIRE_PIXMAP,
                rgba,
            }));
        }
        frames
    }
}

fn first_line(text: &str) -> &str {
    text.split(['\n', '\r']).next().unwrap_or(text)
}

// ---------------------------------------------------------------------------
// Actions back
// ---------------------------------------------------------------------------

/// The one call an action turns into. A description rather than a method call,
/// so the mapping is testable without a bus: `tray/tests/` asks what an id
/// *would* do, and [`crate::item`] is the only thing that does it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// SNI `Activate(x, y)`.
    Activate { x: i32, y: i32 },
    /// SNI `SecondaryActivate(x, y)`.
    SecondaryActivate { x: i32, y: i32 },
    /// dbusmenu `AboutToShow(parent)` then `Event(id, "clicked", …)`.
    MenuClick { id: i32, parent: i32 },
}

impl Call {
    /// The witness form, which is also what the tests read.
    pub fn describe(&self, key: &str) -> String {
        match self {
            Call::Activate { x, y } => format!("{key}: Activate({x}, {y})"),
            Call::SecondaryActivate { x, y } => format!("{key}: SecondaryActivate({x}, {y})"),
            Call::MenuClick { id, parent } => {
                format!("{key}: AboutToShow({parent}) then Event({id}, clicked)")
            }
        }
    }
}

/// `ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`: the bar telling the sidecar that owns
/// a menu that it opened or closed (docs/abi.md, *Action*).
///
/// This sidecar has nothing to do with either. A dbusmenu's tree is published
/// whether or not anything has opened it, so there is nothing to
/// fetch on the open — and a witness per menu open, for every tray item, would
/// be journal noise for nothing. [`dispatch`] is not reached for these: the
/// caller drops them first, because its only way to say "nothing" is a witness.
pub fn is_menu_notice(kind: u32) -> bool {
    kind == ACTION_MENU_OPEN || kind == ACTION_MENU_CLOSE
}

/// One `Action` body as the item it names and the call it means, or the
/// witness line that says why it is nothing.
///
/// A click on a bar item is `Activate` only for an item **without**
/// `RFLAG_HAS_MENU`: an item with one has its rows open instead, and the bar
/// never sends the activate (docs/abi.md, *Rows*). Secondary and middle both
/// reach `SecondaryActivate`: the caustic (`applets.slang`) pushes ACTION_SECONDARY for a right
/// click on an applet and ACTION_MIDDLE for a middle one.
pub fn dispatch(
    targets: &HashMap<u32, Target>,
    action: &ActionBody,
) -> Result<(String, Call), String> {
    let x = action.pos[0].round() as i32;
    let y = action.pos[1].round() as i32;
    let Some(target) = targets.get(&action.row) else {
        return Err(format!("action row={} names no row of ours", action.row));
    };
    match (target, action.kind) {
        (Target::Item(key), ACTION_ACTIVATE) => Ok((key.clone(), Call::Activate { x, y })),
        (Target::Item(key), ACTION_SECONDARY | ACTION_MIDDLE) => {
            Ok((key.clone(), Call::SecondaryActivate { x, y }))
        }
        (Target::MenuRow { key, id, parent }, ACTION_ACTIVATE) => Ok((
            key.clone(),
            Call::MenuClick {
                id: *id,
                parent: *parent,
            },
        )),
        (_, kind) => Err(format!(
            "action row={} kind={kind} is not one this row takes",
            action.row
        )),
    }
}
