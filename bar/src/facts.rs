//! The model becomes **facts**, and a sidecar's bytes go in whole
//! (docs/abi.md, *The facts*, *The arenas*).
//!
//! The embassy writes down what it **originates** — the windows the compositor reports,
//! the dock's app ids in dock order, which image slot holds which pixmap,
//! which binary sits in which sidecar slot, the literal pool and the UTF-8 all
//! of those address — and it **copies** what sidecars send, byte for byte,
//! into two arenas the caustic reads through the wire's own layout:
//!
//! * the **rows arena**, [`abi::MAX_SLOTS`] regions of
//!   [`abi::ROWS_ARENA_BYTES`], one `Rows` body each ([`Facts::rows_region`]);
//! * the **answers arena**, every `Glyph` and `App` frame that has arrived,
//!   concatenated ([`Facts::answers`]).
//!
//! There is no transcription. The embassy writes the body and the caustic
//! reads it: the caustic compares a region's shape against its own copy and pushes
//! `ACT_REBUILD` (docs/abi.md, *The arenas*), and nothing on this side hashes anything at all.
//!
//! Nothing in a `Rows` body is read: a row's icon is a **name**, and
//! a sidecar whose icon is pixels sends them first as a `Pixmap` frame under a
//! key it minted (docs/abi.md, *Pixmap icons*, *Pixmap*). The rows arena is header, rows and blob,
//! verbatim.
//!
//! # The pixmap store
//!
//! Every `Pixmap` frame is kept by its key, asked for or not
//! ([`Facts::learn_pixmap`]): an image slot is a cache the generation evicts
//! from, and the store is what an evicted or not-yet-wanted icon comes back
//! from without a round trip. A want for a key the store holds lands a slot
//! and asks no one ([`Facts::want_icon`]). Bounded at [`MAX_STORED_PIXMAPS`]
//! and [`MAX_STORED_BYTES`], oldest inserted out first; a sidecar that exits
//! takes the keys it sent with it ([`Facts::forget_slot`]). The store keeps
//! bytes under the name they came with and reads none of them — the two membership tests it passes (docs/architecture.md, *The bar process*) are the pipe read (a frame in, filed under its key)
//! and `ACT_WANT_ICON` (a name in, a slot or a request out).
//!
//! # What is left here for its own reasons
//!
//! `slots` — a sidecar slot back to the sidecar's name, which is what an
//! `ACT_SIDECAR` is addressed by; the typed `App` answers, because `launch`
//! reads `path`/`exec` off one and that is I/O and the embassy's; the asked
//! sets, because the embassy asks once per name and remembers (docs/abi.md, *Request*); and
//! the stable head region of the strings buffer, where a name a witness may
//! quote a frame later is placed once.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::abi::wire::{self, Rows};
use crate::abi::{self, AppFact, Cover, ImageFact, SlotFact, WantIcon, WindowFact};
use crate::feeders::pipe::{APPS_SIDECAR, GLYPHS_SIDECAR};
use crate::images::Images;
use crate::model::Model;
use crate::strings::Strings;

/// The dock's default pinned app ids, in the order they appear, compiled in
/// because the bar has no configuration file (docs/architecture.md, *Configuration is
/// the caustic*).
///
/// The one string list the caustic cannot hold, and the one that is not really
/// config either. It is only the **seed**: the dock's favourites
/// are [`Model::favourites`], read once at startup from
/// `$XDG_STATE_HOME/caustics/bar/favourites` and written by the Pin and Unpin rows
/// of an entry's right-click menu (`crate::favourites`). A machine with no file
/// yet gets this list; after the first pin the file is the truth.
pub(crate) const DEFAULT_FAVOURITES: [&str; 7] = [
    "org.mozilla.firefox",
    "com.system76.CosmicFiles",
    "com.system76.CosmicEdit",
    "com.system76.CosmicTerm",
    "com.system76.CosmicStore",
    "com.system76.CosmicSettings",
    "kitty",
];

/// What an image slot holding a themed icon is keyed by, ahead of the name.
///
/// The prefix keeps a theme name from colliding with an app id, which is keyed
/// bare because *Pixmap* says a `REQ_APP` pixmap is keyed by the app id. It is an
/// [`Images`] key and never leaves this file: an [`abi::ImageFact`] publishes
/// the bare name, which is the key the pixmap answered.
const THEMED_KEY: &str = "themed:";

/// The bytes of the strings buffer, which is [`abi::MAX_STRING_WORDS`] words.
const STRING_BYTES: usize = abi::MAX_STRING_WORDS * 4;

/// The bytes the caustic reserves compositions and letters from: the tail of
/// the strings buffer, zeroed by this side and written by `bar_build` and
/// `bar_compose` (docs/abi.md, *The facts*).
const COMPOSE_BYTES: usize = abi::MAX_COMPOSED * abi::COMPOSE_MAX_BYTES as usize;

/// The head of the strings buffer, where a name a witness may **quote a frame
/// later** is placed once and never moved.
///
/// The rest of the buffer is rewritten on every compose, and that is fine for
/// a range the caustic pushes in the frame it read it in (*The three rules the buffer adds*, rule 2). A
/// `layout: <name> target=none rect=gone` line is the exception: the caustic
/// records the range when it lays an element out and quotes it on the frame
/// the element *leaves*, which is by definition a frame after the thing it
/// names has gone from the tables. Without this the line read
/// `layout: applet-udiocaustics-blu target=none rect=gone` — two names' bytes, at
/// an offset that had slid under the range.
///
/// Two kinds of name go here, and they are the two a departure can carry: an
/// app id and a sidecar's name. 4 KiB is a hundred-odd of them; past that a
/// name takes an ordinary place in the buffer and one witness says so.
const STABLE_BYTES: usize = 4096;

/// `Pixmap` frames the store keeps: 256 keys at one
/// 64² slot each is [`MAX_STORED_BYTES`] exactly, four times the image array,
/// and far past any bar's worth of distinct icons.
pub const MAX_STORED_PIXMAPS: usize = 256;

/// Texel bytes the store keeps, 4 MiB.
pub const MAX_STORED_BYTES: usize = 4 << 20;

/// One `Pixmap` frame as the store keeps it: who sent it, and its texels.
#[derive(Clone, Debug)]
struct Stored {
    sidecar: String,
    rgba: Vec<u8>,
}

/// What a want came to ([`Facts::want_icon`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wanted {
    /// Asked before, or nothing this side can act on.
    Nothing,
    /// A request is queued; the embassy writes it.
    Asked,
    /// The store had the pixels and they are in a slot now: the facts owe a
    /// compose, which is what names the slot (and writes any request queued
    /// beside it).
    Landed,
}

/// One answer a sidecar gave, as the arena carries it (docs/abi.md, *The arenas*).
///
/// Kept in **arrival order**, so "drop the oldest" is `remove(0)`; rendered
/// glyphs-first, because a body carries no kind word of its own and the two
/// heads are different shapes.
#[derive(Clone, Debug)]
enum Answer {
    /// A `Glyph` frame (docs/abi.md, *Glyph*): a themed name became a glyph id, or did not.
    Glyph { name: String, gid: Option<u32> },
    /// An `App` frame (docs/abi.md, *App*): what the desktop entry says.
    App(Box<wire::App>),
}

/// What the embassy knows, as the tables the caustic builds from, and what
/// sidecars sent, as the arenas it reads.
pub struct Facts {
    /// Sidecar name by slot: the position in `pipe::commands()`'s resolved
    /// list, and anything else appended on first sight. A slot never comes to
    /// mean a different sidecar, which is what makes a stale slot number
    /// resolve to nothing rather than to someone else's row.
    slots: Vec<String>,
    /// Sidecars past [`abi::MAX_SLOTS`], witnessed once each.
    slots_full: HashSet<String>,
    /// An app id's first-seen stamp, never reused: what orders the unpinned
    /// tail of the dock (docs/abi.md, *The facts*).
    appearance: HashMap<String, u32>,
    next_appearance: u32,
    /// The head region's bytes, [`STABLE_BYTES`] of them, and where each name
    /// in it sits. Written once per name, for the life of the process, so a
    /// range a witness quotes a frame later still spells what it spelled.
    stable_bytes: Vec<u8>,
    stable_at: HashMap<String, (u32, u32)>,
    stable_used: usize,
    /// Whether the region filled, so the witness is one per process.
    stable_full: bool,

    /// The facts buffer, rewritten by every [`Facts::compose`].
    buffer: Box<abi::Facts>,
    /// The bytes every table above points into, as the GPU will hold them.
    strings: Vec<u8>,
    /// The literal pool that indexes the head of those bytes.
    literals: Vec<abi::Literal>,
    /// Whether the strings buffer filled, so the witness is one per compose.
    strings_full: bool,

    /// Each sidecar slot's region of the rows arena, exactly as it was written
    /// to the GPU. Kept because the embassy wrote it and an `ACT_WANT_ICON`
    /// may name a range of it (docs/abi.md, *The action ring*, `SRC_*`).
    regions: Vec<Vec<u8>>,

    /// Every `Pixmap` frame by key (docs/abi.md, *Pixmap*), and the keys oldest first.
    pixmaps: HashMap<String, Stored>,
    pixmap_order: VecDeque<String>,
    /// Keys the bound pushed out, for the tests and the one witness.
    pixmaps_evicted: u64,

    /// Every `Glyph` and `App` answer, in arrival order (docs/abi.md, *The arenas*).
    answers: Vec<Answer>,
    /// Those rendered into the arena, glyphs first, each entry padded to a
    /// word; and where the app half begins.
    arena: Vec<u8>,
    glyph_bytes: usize,

    /// App ids a `REQ_APP` has gone out for. Presence *is* the asked mark: a
    /// compose happens every time any window's title changes — which for a
    /// browser is every tab switch — and one `Request` per app id for the life
    /// of the process is the budget.
    apps_asked: HashSet<String>,
    /// Themed names a `REQ_GLYPH` has gone out for.
    glyphs_asked: HashSet<String>,
    /// Names a `REQ_ICON` has gone out for: a themed name `caustics-glyphs`
    /// answered `GLYPH_MISSING` for, whose pixels `caustics-apps` may have. A name
    /// no theme has is answered with **silence**, so this is the only thing
    /// that stops the same dead name being asked about on every build.
    icons_asked: HashSet<String>,
    /// The strings a `REQ_COVER` has gone out for (docs/abi.md, *Request*). One
    /// question per distinct string for the life of the process, bounded by
    /// [`MAX_COVERED`] because a title is whatever an application puts there.
    covered: HashSet<String>,
    /// Whether that bound has been reached and witnessed.
    cover_capped: bool,
    /// What the caustic wants resolved: `(sidecar, REQ_*, name)`, drained by
    /// the embassy through `Feeders::request`.
    requests: Vec<(&'static str, u32, String)>,
}

/// Distinct strings [`Facts::ask_cover`] will ever ask about — the bar's own
/// bound on a question whose answer is another process's memory. The pipe caps
/// what it remembers at 4096 (docs/abi.md, *Request*); this is well inside that.
pub const MAX_COVERED: usize = 1024;

impl Default for Facts {
    fn default() -> Self {
        Facts::new(Vec::new())
    }
}

impl Facts {
    /// A fresh set of tables, with the sidecar slots seeded in the order the
    /// pipe feeder resolved them.
    pub fn new(slots: Vec<String>) -> Facts {
        let mut facts = Facts {
            slots: Vec::new(),
            slots_full: HashSet::new(),
            appearance: HashMap::new(),
            next_appearance: 0,
            stable_bytes: vec![0; STABLE_BYTES],
            stable_at: HashMap::new(),
            stable_used: 0,
            stable_full: false,
            buffer: Box::new(abi::Facts::ZERO),
            strings: Vec::new(),
            literals: Vec::new(),
            strings_full: false,
            regions: vec![Vec::new(); abi::MAX_SLOTS],
            pixmaps: HashMap::new(),
            pixmap_order: VecDeque::new(),
            pixmaps_evicted: 0,
            answers: Vec::new(),
            arena: Vec::new(),
            glyph_bytes: 0,
            apps_asked: HashSet::new(),
            glyphs_asked: HashSet::new(),
            icons_asked: HashSet::new(),
            covered: HashSet::new(),
            cover_capped: false,
            requests: Vec::new(),
        };
        for name in slots {
            facts.slot_of(&name);
        }
        facts
    }

    /// The facts buffer as the GPU holds it.
    pub fn buffer(&self) -> &abi::Facts {
        &self.buffer
    }

    /// The bytes every offset in it points into.
    pub fn strings(&self) -> &[u8] {
        &self.strings
    }

    /// The literal pool.
    pub fn literals(&self) -> &[abi::Literal] {
        &self.literals
    }

    /// The answers arena, as the GPU holds it (docs/abi.md, *The arenas*).
    pub fn answers(&self) -> &[u8] {
        &self.arena
    }

    /// The sidecar in `slot`, if this bar has ever seen one there.
    pub fn sidecar(&self, slot: u32) -> Option<&str> {
        self.slots.get(slot as usize).map(String::as_str)
    }

    /// The slot a sidecar's name has, allocating one the first time.
    ///
    /// Append-on-first-sight rather than the position in the live list: the
    /// list is `pipe::commands()`'s and is read once, so the two agree for
    /// every sidecar the bar started — and a name that arrives from somewhere
    /// else still gets a slot that never becomes another sidecar's.
    pub fn slot_of(&mut self, name: &str) -> Option<u32> {
        if let Some(at) = self.slots.iter().position(|slot| slot == name) {
            return Some(at as u32);
        }
        if self.slots.len() >= abi::MAX_SLOTS {
            if self.slots_full.insert(name.to_owned()) {
                tracing::warn!(
                    "facts: {} sidecar slots are taken; {name:?} has none and publishes no applet",
                    abi::MAX_SLOTS
                );
            }
            return None;
        }
        self.slots.push(name.to_owned());
        Some(self.slots.len() as u32 - 1)
    }

    /// The first-seen stamp of an app id, allocating one the first time.
    fn appearance(&mut self, app_id: &str) -> u32 {
        if let Some(stamp) = self.appearance.get(app_id) {
            return *stamp;
        }
        self.next_appearance += 1;
        self.appearance
            .insert(app_id.to_owned(), self.next_appearance);
        self.next_appearance
    }

    // -- the compose -------------------------------------------------------

    /// Write every table, and the bytes they point into.
    ///
    /// One pass over the model, in this order: the stable head region (app ids and sidecar names, never moved), the literal pool, then the dock in dock order, the windows, the image directory and the slots'
    /// names, then the zeroed region the caustic reserves its compositions
    /// from. Nothing decides anything; the caustic builds.
    pub fn compose(&mut self, model: &Model, strings: &Strings, images: &mut Images) {
        // One compose, one generation. What keeps a slot young is the
        // caustic's `ACT_IMAGES_DRAWN` (stamped with this generation) or a
        // landing in it, never this pass's listing (`images.rs`).
        images.begin_generation();
        // A slot taken from someone else takes that name's "asked" mark with
        // it — a themed name's under its prefix, an app id's bare — so the
        // next want lands the pixels again from the store rather than naming a
        // slot that now holds someone else's picture.
        for key in images.take_evicted() {
            match key.strip_prefix(THEMED_KEY) {
                Some(name) => self.icons_asked.remove(name),
                None => self.apps_asked.remove(&key),
            };
        }
        self.buffer = Box::new(abi::Facts::ZERO);
        self.strings.clear();
        self.literals.clear();
        self.strings_full = false;

        // The head region first, whole and at its own fixed size: every app id
        // and sidecar name this process has ever placed, each at the offset it
        // was placed at. A name minted later in this pass writes into it in
        // place, because the bytes are already here.
        self.strings.extend_from_slice(&self.stable_bytes);

        self.literal_pool(strings);
        self.apps(model);
        self.windows(model);
        self.image_directory(images);
        self.slot_names();

        // The caustic's own region, last and word-aligned: `bar_build` hands
        // composition k the bytes at `compose_base + k * COMPOSE_MAX_BYTES`
        // and this side never writes them again (docs/abi.md, *The facts*).
        self.pad_to_word();
        if self.strings.len() + COMPOSE_BYTES > STRING_BYTES {
            // Nothing else can be given up for it, so the titles go and the
            // clock keeps its bytes: a bar whose strings buffer is that full
            // has already witnessed why.
            self.strings
                .truncate(STRING_BYTES.saturating_sub(COMPOSE_BYTES));
            self.pad_to_word();
        }
        self.buffer.header.compose_base = self.strings.len() as u32;
        self.strings.resize(self.strings.len() + COMPOSE_BYTES, 0);
        self.buffer.header.string_words = (self.strings.len() as u32).div_ceil(4);
        self.buffer.header.answer_bytes = self.arena.len() as u32;
        self.buffer.header.glyph_bytes = self.glyph_bytes as u32;
    }

    /// Every literal of the running caustic's string table, appended and
    /// indexed (docs/abi.md, *The facts*).
    fn literal_pool(&mut self, strings: &Strings) {
        for (hash, text) in strings.iter() {
            if self.literals.len() >= abi::MAX_LITERALS {
                tracing::warn!(
                    "facts: the literal pool is full at {} entries; \
                     the caustic cannot spell every literal it names",
                    self.literals.len()
                );
                break;
            }
            let Some((off, len)) = self.put(text.as_bytes(), text) else {
                break;
            };
            self.literals.push(abi::Literal {
                hash,
                off,
                len,
                _pad0: 0,
            });
        }
        self.buffer.header.literal_count = self.literals.len() as u32;
    }

    /// The dock's entries, **in dock order** (docs/abi.md, *The facts*): the favourites file's own
    /// order first, then every running app id not among them, in the order
    /// first seen.
    ///
    /// The order is the embassy's because both sources are its; what an entry
    /// is called and what its icon is are the apps sidecar's answer, joined by
    /// the app id string in the arena.
    fn apps(&mut self, model: &Model) {
        let mut ids: Vec<(bool, u32, String)> = Vec::new();
        for (at, app_id) in model.favourites.iter().enumerate() {
            ids.push((true, at as u32, app_id.clone()));
        }
        for app_id in model.running_app_ids() {
            if ids.iter().any(|(_, _, held)| held == app_id) {
                continue;
            }
            let stamp = self.appearance(app_id);
            ids.push((false, stamp, app_id.to_owned()));
        }
        // Favourites lead in file order; the rest follow in first-seen order,
        // which is what the caustic lays out.
        ids.sort_by(|a, b| (!a.0, a.1).cmp(&(!b.0, b.1)));

        let mut count = 0usize;
        for (favourite, _, app_id) in ids {
            if count >= abi::MAX_APPS {
                tracing::warn!(
                    "facts: {} app ids is all the table holds; {app_id:?} has no dock entry",
                    abi::MAX_APPS
                );
                break;
            }
            // The app id goes in the stable region: a dock entry's
            // `layout: <app id> target=none rect=gone` line quotes this range
            // on the frame the entry leaves, which is after the compose that
            // dropped it.
            let Some((id_off, id_len)) = self.put_stable(&app_id) else {
                continue;
            };
            self.buffer.apps[count] = AppFact {
                id_off,
                id_len,
                flags: if favourite { abi::AFLAG_FAVOURITE } else { 0 },
                _pad0: 0,
            };
            count += 1;
        }
        self.buffer.header.app_count = count as u32;
    }

    /// Every window, with the row of the app it hangs under.
    fn windows(&mut self, model: &Model) {
        let mut count = 0usize;
        for window in model.windows.values() {
            if count >= abi::MAX_WINDOWS {
                tracing::warn!(
                    "facts: {} windows is all the table holds; window {} is not published",
                    abi::MAX_WINDOWS,
                    window.id
                );
                break;
            }
            // 56 bits, because the caustic's own `Id` puts an 8-bit tag in the
            // high word and names the window by the rest. A larger id is
            // refused rather than truncated into a different window — the
            // *operands* of an `ACT_ACTIVATE_WINDOW` are the full 64 bits, but
            // what the caustic looks a window up *by* is still the narrower
            // one.
            if window.id >= (1u64 << 56) {
                tracing::warn!(
                    "facts: window id {} needs more than 56 bits; it is not published",
                    window.id
                );
                continue;
            }
            let app_row = self.app_row(&window.app_id);
            let (title_off, title_len) = self
                .put(window.title.as_bytes(), &window.title)
                .unwrap_or((0, 0));
            let mut flags = 0;
            if window.focused {
                flags |= abi::WFLAG_FOCUSED;
            }
            if window.urgent {
                flags |= abi::WFLAG_URGENT;
            }
            self.buffer.windows[count] = WindowFact {
                id_lo: window.id as u32,
                id_hi: (window.id >> 32) as u32,
                app_row,
                title_off,
                title_len,
                flags,
                focus_stamp: window.focus_stamp as u32,
                _pad0: 0,
            };
            count += 1;
        }
        self.buffer.header.window_count = count as u32;
    }

    /// The dock row an app id has, by its bytes — the one join the embassy
    /// makes, because it is the one that knows both lists (docs/abi.md, *The facts*).
    fn app_row(&self, app_id: &str) -> u32 {
        let live = self.buffer.header.app_count as usize;
        for (at, app) in self.buffer.apps.iter().take(live).enumerate() {
            let start = app.id_off as usize;
            let end = start + app.id_len as usize;
            if self.strings.get(start..end) == Some(app_id.as_bytes()) {
                return at as u32;
            }
        }
        abi::NO_INDEX
    }

    /// Which image slot holds which pixmap (docs/abi.md, *The facts*): every key this bar asked
    /// about whose pixels landed.
    ///
    /// A listing, not a draw: it looks slots up without keeping them young,
    /// and fills none. A listing that kept slots young would count every icon ever named as drawn, and new icons would be refused once the slots ran out (docs/architecture.md,
    /// *Images and icons*).
    fn image_directory(&mut self, images: &Images) {
        let mut rows: Vec<ImageFact> = Vec::new();
        // An app id's own pixels, keyed bare, as *Pixmap* keys a `REQ_APP`
        // answer's pixmap. An evicted app id is un-asked by the compose, so
        // the caustic's next want lands it from the store.
        let mut app_ids: Vec<String> = self.apps_asked.iter().cloned().collect();
        app_ids.sort();
        for app_id in app_ids {
            let Some(slot) = images.slot_of(&app_id) else {
                continue;
            };
            let Some((name_off, name_len)) = self.put(app_id.as_bytes(), &app_id) else {
                continue;
            };
            rows.push(ImageFact {
                name_off,
                name_len,
                slot,
                _pad0: 0,
            });
        }
        // A themed name whose pixels came back from `REQ_ICON`. Published
        // under the **bare** name, which is the key it answered; the prefix is
        // this file's own and never crosses.
        let mut names: Vec<String> = self.icons_asked.iter().cloned().collect();
        names.sort();
        for name in names {
            let Some(slot) = images.slot_of(&Facts::themed_key(&name)) else {
                continue;
            };
            let Some((name_off, name_len)) = self.put(name.as_bytes(), &name) else {
                continue;
            };
            rows.push(ImageFact {
                name_off,
                name_len,
                slot,
                _pad0: 0,
            });
        }
        let mut count = 0usize;
        for fact in rows {
            if count >= abi::MAX_IMAGE_SLOTS {
                tracing::warn!(
                    "facts: {} image slots is all the directory holds",
                    abi::MAX_IMAGE_SLOTS
                );
                break;
            }
            self.buffer.images[count] = fact;
            count += 1;
        }
        self.buffer.header.image_count = count as u32;
    }

    /// The sidecar slots, by name: what a witness spells `applet-caustics-power`
    /// out of (docs/abi.md, *The facts*).
    fn slot_names(&mut self) {
        for slot in 0..self.slots.len().min(abi::MAX_SLOTS) {
            let name = self.slots[slot].clone();
            let (name_off, name_len) = self.put_stable(&name).unwrap_or((0, 0));
            self.buffer.slots[slot] = SlotFact {
                name_off,
                name_len,
                _pad0: 0,
                _pad1: 0,
            };
        }
    }

    // -- the rows arena (docs/abi.md, *The arenas*) -----------------------------

    /// One `Rows` frame as the bytes that go into its slot's region: header,
    /// rows and blob at the writer's own offsets, and nothing else.
    ///
    /// `Err(len)` when the body does not fit [`abi::ROWS_ARENA_BYTES`]; the
    /// caller witnesses the number and leaves the region as it was.
    pub fn rows_region(&mut self, slot: u32, rows: &Rows) -> Result<Vec<u8>, usize> {
        let head = wire::RowsHeader {
            applet: rows.applet,
            row_count: rows.rows.len() as u32,
            bytes_len: rows.bytes.len() as u32,
            revision: rows.revision,
        };
        let len = 16 + rows.rows.len() * size_of::<wire::Row>() + rows.bytes.len();
        if len > abi::ROWS_ARENA_BYTES {
            return Err(len);
        }
        let mut body = Vec::with_capacity(len);
        body.extend_from_slice(abi::as_bytes(&head));
        body.extend_from_slice(abi::slice_as_bytes(&rows.rows));
        body.extend_from_slice(&rows.bytes);
        self.regions[slot as usize] = body.clone();
        Ok(body)
    }

    /// A sidecar that exited or was replaced: its region is gone, and so are
    /// the pixmaps it sent — a restarted one sends them again, and a key no
    /// one will send again is not worth the room.
    pub fn forget_slot(&mut self, slot: u32) {
        self.regions[slot as usize].clear();
        let Some(sidecar) = self.slots.get(slot as usize) else {
            return;
        };
        self.pixmaps.retain(|_, held| held.sidecar != *sidecar);
        let pixmaps = &self.pixmaps;
        self.pixmap_order.retain(|key| pixmaps.contains_key(key));
    }

    /// One slot's region, as the embassy wrote it — what an `ACT_WANT_ICON`
    /// or an `ACT_COVER` whose source is a slot index reads its bytes out of.
    fn region(&self, slot: u32) -> &[u8] {
        self.regions
            .get(slot as usize)
            .map_or(&[][..], Vec::as_slice)
    }

    // -- the answers arena (docs/abi.md, *The arenas*) --------------------------

    /// A `Glyph` frame: one name answered (docs/abi.md, *Glyph*). `true` when the answer is
    /// new, which is a reason to upload the arena again.
    pub fn learn_glyph(&mut self, name: &str, gid: Option<u32>) -> bool {
        self.glyphs_asked.remove(name);
        for answer in &mut self.answers {
            if let Answer::Glyph {
                name: held,
                gid: was,
            } = answer
                && held == name
            {
                if *was == gid {
                    return false;
                }
                *was = gid;
                self.rebuild_arena();
                return true;
            }
        }
        self.answers.push(Answer::Glyph {
            name: name.to_owned(),
            gid,
        });
        self.rebuild_arena();
        true
    }

    /// A `Font` frame: a *different* glyph space. Every id learned against the
    /// last one is meaningless — the sidecar restarted, or the font changed —
    /// so every glyph answer leaves the arena and the names are asked again as
    /// they come up.
    pub fn forget_glyphs(&mut self) {
        self.answers
            .retain(|answer| !matches!(answer, Answer::Glyph { .. }));
        self.glyphs_asked.clear();
        // A `Font` frame means `caustics-glyphs` restarted or switched fonts, so every
        // codepoint it had covered must be asked for again.
        self.covered.clear();
        self.rebuild_arena();
    }

    /// One sidecar's answer to a `REQ_APP` (docs/abi.md, *App*), keyed by the app id it
    /// echoes back. Whole-state, like every frame on this wire.
    pub fn set_app(&mut self, app: wire::App) {
        for answer in &mut self.answers {
            if let Answer::App(held) = answer
                && held.app_id == app.app_id
            {
                **held = app;
                self.rebuild_arena();
                return;
            }
        }
        self.answers.push(Answer::App(Box::new(app)));
        self.rebuild_arena();
    }

    /// What `caustics-apps` said about an app id, if it has answered — what a dock
    /// click has to become an argv through (`wayland::Bar::launch`).
    ///
    /// The one answer this side still *reads*: `path` and `exec` become a
    /// process, which is I/O and the embassy's.
    pub fn app(&self, app_id: &str) -> Option<&wire::App> {
        self.answers.iter().find_map(|answer| match answer {
            Answer::App(app) if app.app_id == app_id => Some(&**app),
            _ => None,
        })
    }

    /// The arena, rendered: the glyph bodies first and the app bodies after,
    /// each head followed by its bytes and padded to a word (docs/abi.md, *The arenas*).
    ///
    /// Rewritten whole, which is what makes "drop the oldest" one line. A want
    /// the caustic pushes again for a dropped answer is asked again.
    fn rebuild_arena(&mut self) {
        while self.arena_len() > abi::ANSWERS_BYTES && !self.answers.is_empty() {
            let dropped = self.answers.remove(0);
            match dropped {
                Answer::Glyph { name, .. } => {
                    self.glyphs_asked.remove(&name);
                }
                Answer::App(app) => {
                    self.apps_asked.remove(&app.app_id);
                }
            }
        }
        let mut out: Vec<u8> = Vec::with_capacity(self.arena_len());
        for answer in &self.answers {
            if let Answer::Glyph { name, gid } = answer {
                let head = wire::GlyphBody {
                    gid: gid.unwrap_or(0),
                    flags: if gid.is_some() {
                        0
                    } else {
                        wire::GLYPH_MISSING
                    },
                    name_len: name.len() as u32,
                    _rsvd0: 0,
                };
                out.extend_from_slice(abi::as_bytes(&head));
                out.extend_from_slice(name.as_bytes());
                out.resize(out.len().next_multiple_of(4), 0);
            }
        }
        self.glyph_bytes = out.len();
        for answer in &self.answers {
            if let Answer::App(app) = answer {
                let exec = app.exec.join("\0");
                let head = wire::AppBody {
                    flags: app.flags,
                    app_len: app.app_id.len() as u32,
                    name_len: app.name.len() as u32,
                    icon_len: app.icon.len() as u32,
                    path_len: app.path.len() as u32,
                    exec_len: exec.len() as u32,
                    _rsvd0: 0,
                    _rsvd1: 0,
                };
                out.extend_from_slice(abi::as_bytes(&head));
                out.extend_from_slice(app.app_id.as_bytes());
                out.extend_from_slice(app.name.as_bytes());
                out.extend_from_slice(app.icon.as_bytes());
                out.extend_from_slice(app.path.as_bytes());
                out.extend_from_slice(exec.as_bytes());
                out.resize(out.len().next_multiple_of(4), 0);
            }
        }
        self.arena = out;
        self.buffer.header.answer_bytes = self.arena.len() as u32;
        self.buffer.header.glyph_bytes = self.glyph_bytes as u32;
    }

    /// What the arena would be, in bytes, without rendering it.
    fn arena_len(&self) -> usize {
        self.answers
            .iter()
            .map(|answer| match answer {
                Answer::Glyph { name, .. } => 16 + name.len().next_multiple_of(4),
                Answer::App(app) => {
                    let fields = app.app_id.len()
                        + app.name.len()
                        + app.icon.len()
                        + app.path.len()
                        + app.exec.join("\0").len();
                    32 + fields.next_multiple_of(4)
                }
            })
            .sum()
    }

    // -- the pixels, and the questions -------------------------------------

    /// A `Pixmap` frame (docs/abi.md, *Pixmap*): kept in the store under its key whatever it
    /// answers, and — when it answers something this bar asked — into the
    /// slot the next compose will name.
    ///
    /// The wire keys the pixmap by the name it was asked under, and this side
    /// asks two kinds of name: an app id (`REQ_APP`, whose slot is keyed bare)
    /// and a themed icon that is not curves (`REQ_ICON`, keyed under the
    /// themed prefix so it can never shadow an app id). The frame does not say
    /// which request it answers, so the name is read against what was asked;
    /// a pixmap filed under the wrong key would never be drawn. A key nobody
    /// asked for yet is the normal case: the tray sends its pixels before the frame that names them.
    pub fn learn_pixmap(
        &mut self,
        images: &mut Images,
        sidecar: &str,
        name: &str,
        rgba: &[u8],
    ) -> Option<u32> {
        self.keep_pixmap(sidecar, name, rgba);
        let mut landed = None;
        if self.icons_asked.contains(name) {
            landed = images.insert(&Facts::themed_key(name), rgba).or(landed);
        }
        if self.apps_asked.contains(name) {
            landed = images.insert(name, rgba).or(landed);
        }
        if landed.is_none() {
            tracing::debug!(
                "facts: a pixmap for {name:?} from {sidecar} is kept until it is wanted"
            );
        }
        landed
    }

    /// File a `Pixmap` frame in the store, newest last, and let the oldest go
    /// past the bound.
    fn keep_pixmap(&mut self, sidecar: &str, key: &str, rgba: &[u8]) {
        if self.pixmaps.contains_key(key) {
            self.pixmap_order.retain(|held| held != key);
        }
        self.pixmaps.insert(
            key.to_owned(),
            Stored {
                sidecar: sidecar.to_owned(),
                rgba: rgba.to_vec(),
            },
        );
        self.pixmap_order.push_back(key.to_owned());
        while self.pixmaps.len() > MAX_STORED_PIXMAPS || self.stored_bytes() > MAX_STORED_BYTES {
            let Some(oldest) = self.pixmap_order.pop_front() else {
                break;
            };
            let Some(gone) = self.pixmaps.remove(&oldest) else {
                continue;
            };
            self.pixmaps_evicted += 1;
            if self.pixmaps_evicted == 1 {
                tracing::warn!(
                    "facts: the pixmap store is full at {MAX_STORED_PIXMAPS} keys; {oldest:?} \
                     from {} goes, and every key past it goes the same way",
                    gone.sidecar
                );
            } else {
                tracing::debug!(
                    "facts: the pixmap store lets {oldest:?} from {} go",
                    gone.sidecar
                );
            }
        }
    }

    /// The store's texel bytes.
    fn stored_bytes(&self) -> usize {
        self.pixmaps.values().map(|held| held.rgba.len()).sum()
    }

    /// Keys the store holds, for the health line.
    pub fn pixmaps_stored(&self) -> usize {
        self.pixmaps.len()
    }

    /// Keys the store's bound has let go since the process began.
    pub fn pixmaps_evicted(&self) -> u64 {
        self.pixmaps_evicted
    }

    /// The names the caustic wants resolved, and the sidecar and `REQ_*` each
    /// goes to. Cleared by the call; the embassy writes them.
    pub fn take_requests(&mut self) -> Vec<(&'static str, u32, String)> {
        std::mem::take(&mut self.requests)
    }

    /// One `REQ_COVER` for the bytes of a text block the caustic flagged
    /// `SHAPE_RUN_NOTDEF` (docs/abi.md, *Request*). `true` when this string
    /// had not been asked about before.
    ///
    /// The embassy has the bytes — they are in its own strings buffer — and
    /// sends them as they are. It decodes nothing: which codepoints are
    /// missing, which face has them and whether any face does is entirely
    /// `caustics-glyphs`' business.
    pub fn ask_cover(&mut self, text: &str) -> bool {
        if text.is_empty() || self.covered.contains(text) {
            return false;
        }
        if self.covered.len() >= MAX_COVERED {
            // A bar whose titles have produced a thousand distinct unshapeable
            // strings is looking at a font that is missing a script, not at a
            // title: one witness, and it stops asking.
            if !self.cover_capped {
                self.cover_capped = true;
                tracing::info!(
                    "text: {MAX_COVERED} strings have been sent for cover; not asking again"
                );
            }
            return false;
        }
        self.covered.insert(text.to_owned());
        self.requests
            .push((GLYPHS_SIDECAR, wire::REQ_COVER, text.to_owned()));
        true
    }

    /// An `ACT_COVER` (docs/abi.md, *The action ring*): the bytes of the block the caustic flagged, read
    /// out of whichever of the three buffers its `source` names, and asked
    /// about through [`Facts::ask_cover`]. `Some(text)` when a question went
    /// out, for the embassy's one line.
    ///
    /// A title, a literal, a sidecar's row label and an app's name are all
    /// bytes this side wrote (a tray menu's `🇦🇺 Australia` is a row label,
    /// and without the source word it would never be asked about). The wire carries a name as UTF-8 capped at `MAX_NAME_LEN`; a
    /// string past that loses its tail's glyphs and nothing else.
    pub fn cover(&mut self, cover: Cover) -> Option<String> {
        let mut text = self.bytes_at(cover.source, cover.off, cover.len)?;
        if text.len() > wire::MAX_NAME_LEN {
            let cut = (0..=wire::MAX_NAME_LEN)
                .rev()
                .find(|at| text.is_char_boundary(*at))
                .unwrap_or(0);
            text.truncate(cut);
        }
        self.ask_cover(&text).then_some(text)
    }

    /// One name the caustic wants resolved, as the `(range, REQ_*, source)` an
    /// `ACT_WANT_ICON` carries (docs/abi.md, *The action ring*).
    ///
    /// A forward, with **the store first**: the range names bytes of a buffer
    /// this side wrote, the kind names the sidecar, and what is left here is
    /// the asked-set that keeps one question per name for the life of the
    /// process — and, for a pixmap the store already holds, the slot it goes
    /// into instead of the question.
    pub fn want_icon(&mut self, images: &mut Images, want: WantIcon) -> Wanted {
        let Some(name) = self.bytes_at(want.source, want.off, want.len) else {
            return Wanted::Nothing;
        };
        match want.kind {
            wire::REQ_APP => {
                if !self.apps_asked.insert(name.clone()) {
                    return Wanted::Nothing;
                }
                // The pixels may be here; the name and the argv are an `App`
                // answer, and only `caustics-apps` has those.
                let landed = self
                    .pixmaps
                    .get(&name)
                    .and_then(|held| images.insert(&name, &held.rgba));
                if self.app(&name).is_none() {
                    self.requests.push((APPS_SIDECAR, wire::REQ_APP, name));
                }
                if landed.is_some() {
                    Wanted::Landed
                } else {
                    Wanted::Asked
                }
            }
            wire::REQ_GLYPH => {
                if !self.glyphs_asked.insert(name.clone()) {
                    return Wanted::Nothing;
                }
                self.requests.push((GLYPHS_SIDECAR, wire::REQ_GLYPH, name));
                Wanted::Asked
            }
            wire::REQ_ICON => {
                if !self.icons_asked.insert(name.clone()) {
                    return Wanted::Nothing;
                }
                if let Some(held) = self.pixmaps.get(&name) {
                    // A key a sidecar sent before the frame that names it — the
                    // tray's every colour icon — or pixels an eviction took.
                    return match images.insert(&Facts::themed_key(&name), &held.rgba) {
                        Some(_) => Wanted::Landed,
                        None => Wanted::Nothing,
                    };
                }
                self.requests.push((APPS_SIDECAR, wire::REQ_ICON, name));
                Wanted::Asked
            }
            other => {
                tracing::info!(
                    "facts: the caustic wants {name:?} as kind {other}, which no sidecar answers"
                );
                Wanted::Nothing
            }
        }
    }

    /// The UTF-8 at a range of one of the three buffers the embassy wrote
    /// (docs/abi.md, *The action ring*): a sidecar slot's rows region, the strings buffer, or the
    /// answers arena.
    ///
    /// The one decode in the bar, and it is a decode of the bar's *own* bytes:
    /// a caustic's range is read against the buffer that was live when it was
    /// pushed (*The three rules the buffer adds*, rule 2), so this is the name the caustic saw. A range past
    /// the end is a caustic from another generation and is dropped.
    pub fn bytes_at(&self, source: u32, off: u32, len: u32) -> Option<String> {
        let buffer = match source {
            abi::SRC_STRINGS => &self.strings[..],
            abi::SRC_ANSWERS => &self.arena[..],
            slot if (slot as usize) < abi::MAX_SLOTS => self.region(slot),
            _ => return None,
        };
        Facts::slice_at(buffer, off, len)
    }

    /// The UTF-8 at a range of the strings buffer — what an `ACT_SPAWN`,
    /// `ACT_PIN` or `ACT_UNPIN` names.
    pub fn text_at(&self, off: u32, len: u32) -> Option<String> {
        Facts::slice_at(&self.strings, off, len)
    }

    fn slice_at(buffer: &[u8], off: u32, len: u32) -> Option<String> {
        if len == 0 {
            return None;
        }
        let start = off as usize;
        let end = start.checked_add(len as usize)?;
        let bytes = buffer.get(start..end)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    // -- the bytes ---------------------------------------------------------

    /// A name a witness may quote **after** the compose that wrote it: an app
    /// id, a sidecar's name (see [`STABLE_BYTES`]).
    ///
    /// Placed once for the life of the process, so the range stays true. Past
    /// the region's size it falls back to [`Facts::put`], which is a range
    /// that is true for the frame it is read in and no longer.
    fn put_stable(&mut self, text: &str) -> Option<(u32, u32)> {
        if text.is_empty() {
            return None;
        }
        if let Some(range) = self.stable_at.get(text) {
            return Some(*range);
        }
        let len = text.len();
        if self.stable_used + len > STABLE_BYTES {
            if !self.stable_full {
                self.stable_full = true;
                tracing::warn!(
                    "facts: the stable-name region is full at {} bytes; {text:?} and every \
                     name after it can go stale in a `rect=gone` witness",
                    self.stable_used
                );
            }
            return self.put(text.as_bytes(), text);
        }
        let offset = self.stable_used;
        self.stable_bytes[offset..offset + len].copy_from_slice(text.as_bytes());
        self.stable_used += len;
        let range = (offset as u32, len as u32);
        self.stable_at.insert(text.to_owned(), range);
        // This compose has already copied the region in, so patch what it
        // copied rather than waiting for the next one.
        if self.strings.len() >= STABLE_BYTES {
            self.strings[offset..offset + len].copy_from_slice(text.as_bytes());
        }
        Some(range)
    }

    /// Append `bytes` to the strings buffer and give back their range, or
    /// `None` when it is full.
    ///
    /// Full is a witness and a missing label, never a panic and never a
    /// truncated string drawn as if it were whole. `what` names the string in
    /// the witness.
    fn put(&mut self, bytes: &[u8], what: &str) -> Option<(u32, u32)> {
        if bytes.is_empty() {
            return None;
        }
        if self.strings.len() + bytes.len() + COMPOSE_BYTES > STRING_BYTES {
            if !self.strings_full {
                self.strings_full = true;
                tracing::warn!(
                    "facts: the strings buffer is full at {} bytes; {what:?} is not drawn",
                    self.strings.len()
                );
            }
            return None;
        }
        let offset = self.strings.len() as u32;
        self.strings.extend_from_slice(bytes);
        Some((offset, bytes.len() as u32))
    }

    fn pad_to_word(&mut self) {
        let pad = self.strings.len().next_multiple_of(4) - self.strings.len();
        self.strings.resize(self.strings.len() + pad, 0);
    }

    /// The key a themed icon's image slot is filed under.
    pub fn themed_key(name: &str) -> String {
        format!("{THEMED_KEY}{name}")
    }
}

#[cfg(test)]
mod tests;
