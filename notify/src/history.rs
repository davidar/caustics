//! What the daemon remembers: the notifications,
//! newest first, bounded; which are unread; which have been shown as a toast;
//! whose toast is still live.
//!
//! **A toast is shown once.** An entry toasts while it is
//! unread, not yet [`shown`](Entry::shown), and within its toast time. `shown`
//! is set by the bar — when a toast ends, it names the rows it showed with an
//! `ACTION_TOAST_SHOWN` each, and exactly those are shown — and by the
//! clock, for a toast the bar never opened (do-not-disturb, no room, a
//! compositor that refused it): its toast time has passed. A row that arrived
//! behind an open toast was never on screen and is still owed its own.
//! `unread` still counts every one of them.
//!
//! **It outlives the process** (`lib.rs` says why): [`Snapshot`] is
//! what the history file holds — the id counter and the entries that are not
//! transient — and [`History::restore`] turns one back into a history whose
//! entries are all shown. The file itself is `lib.rs`'s.
//!
//! Pure: no bus, no clock of its own. Every call that depends on the time is
//! handed `now`, and every call that ends a notification returns the
//! `NotificationClosed` it owes, so the server and the loop emit them and the
//! tests read them.

use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::image::Fitted;

/// Notifications kept, newest first. The 201st pushes the oldest out.
pub const HISTORY_MAX: usize = 200;

/// The specification's urgencies (`urgency` hint, a byte).
pub const URGENCY_LOW: u8 = 0;
pub const URGENCY_NORMAL: u8 = 1;
pub const URGENCY_CRITICAL: u8 = 2;

/// How long a toast lives when the client said "server's choice"
/// (`expire_timeout` −1, or any negative), by urgency. A critical one's never ends on its
/// own: it lives until it is read.
pub const TOAST_NORMAL: Duration = Duration::from_secs(5);
pub const TOAST_LOW: Duration = Duration::from_secs(10);

/// `NotificationClosed`'s reasons, as the specification numbers them.
pub const CLOSED_EXPIRED: u32 = 1;
pub const CLOSED_DISMISSED: u32 = 2;
pub const CLOSED_BY_CALL: u32 = 3;
pub const CLOSED_UNDEFINED: u32 = 4;

/// The action key a click on the row itself invokes.
pub const DEFAULT_ACTION: &str = "default";

/// What a notification's row wears as its icon (the specification's precedence is applied in `server::icon` before one of these is made).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    None,
    /// A themed name or an absolute path; `caustics-glyphs` and `caustics-apps`
    /// resolve both.
    Name(String),
    /// An `image-data` hint, fitted and keyed (`caustics-notify:<fnv1a64>`).
    Pixels(Fitted),
}

impl Icon {
    /// The name the row carries.
    pub fn name(&self) -> Option<&str> {
        match self {
            Icon::None => None,
            Icon::Name(name) => Some(name),
            Icon::Pixels(fitted) => Some(&fitted.key),
        }
    }
}

/// A `Notify` call, decoded: what the server hands the history.
#[derive(Clone, Debug)]
pub struct Incoming {
    pub app_name: String,
    pub replaces_id: u32,
    pub icon: Icon,
    pub summary: String,
    /// The stripped body, its own newlines kept, at most `markup::BODY_MAX`
    /// bytes: the row draws it wrapped under the summary.
    pub body: String,
    /// `(key, label)` pairs, in the client's order.
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub transient: bool,
    pub resident: bool,
    /// Milliseconds. The specification: −1 (any negative) is the server's
    /// choice, 0 is "never expires", more is that long. This daemon advertises
    /// `persistence`, so nothing leaves the history on a timer whatever this
    /// says; what it times is the **toast**: the server's
    /// choice is [`TOAST_NORMAL`] or [`TOAST_LOW`] (a critical one until it is
    /// read), 0 is a toast that stays until it is clicked or dismissed, and a
    /// positive value is the toast's life.
    pub expire_timeout: i32,
    /// The unique bus name of the connection that sent the `Notify`: the
    /// client the click's `ActivationToken` and `ActionInvoked` are sent to,
    /// and no one else (`lib.rs` says why). `None` off the bus (tests).
    pub sender: Option<String>,
}

/// One notification the daemon holds.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u32,
    pub app_name: String,
    pub icon: Icon,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub transient: bool,
    pub resident: bool,
    pub expire_timeout: i32,
    /// Who sent it (`Incoming::sender`); a replacement's sender takes over.
    pub sender: Option<String>,
    /// When it arrived (or was last replaced): the toast's clock and the
    /// row's "5 min ago".
    pub received: Instant,
    /// The same moment on the wall: what the history file keeps, since an
    /// `Instant` means nothing to another process.
    pub wall: SystemTime,
    /// Set on arrival and on replacement; cleared when the menu opens.
    pub unread: bool,
    /// Its toast has been seen and is over: its toast time passed, or the bar
    /// said a toast showed it ([`History::mark_shown`]). Cleared on
    /// replacement, which is news. A shown entry never toasts again; `unread`
    /// still counts it.
    pub shown: bool,
}

impl Entry {
    /// When its toast ends; `None` when it never does on its own — an
    /// `expire_timeout` of 0 (the specification's "never expires") and a
    /// critical one at the server's choice. Either stays until it is read,
    /// clicked or dismissed.
    pub fn toast_until(&self) -> Option<Instant> {
        if self.expire_timeout > 0 {
            return Some(self.received + Duration::from_millis(self.expire_timeout as u64));
        }
        if self.expire_timeout == 0 || self.urgency >= URGENCY_CRITICAL {
            return None;
        }
        Some(
            self.received
                + if self.urgency == URGENCY_LOW {
                    TOAST_LOW
                } else {
                    TOAST_NORMAL
                },
        )
    }

    /// Whether its toast is live at `now`: unread, not shown, and either never
    /// ending or not ended yet. A row wears `RFLAG_ATTENTION` exactly while
    /// this holds, so the toast and the menu agree on what is new.
    pub fn toasting(&self, now: Instant) -> bool {
        self.unread && !self.shown && self.toast_until().is_none_or(|until| now < until)
    }

    /// The `default` action, when the client gave one.
    pub fn has_default(&self) -> bool {
        self.actions.iter().any(|(key, _)| key == DEFAULT_ACTION)
    }

    /// Whether the row gets a submenu: any action other than `default`.
    pub fn has_named_actions(&self) -> bool {
        self.actions.iter().any(|(key, _)| key != DEFAULT_ACTION)
    }
}

/// The daemon's memory.
#[derive(Debug)]
pub struct History {
    /// Newest first.
    entries: Vec<Entry>,
    /// The next id handed out: from 1, and carried across restarts by the
    /// history file, so a new process does not hand out an id an app
    /// still holds from the last.
    next_id: u32,
}

impl Default for History {
    fn default() -> Self {
        History {
            entries: Vec::new(),
            next_id: 1,
        }
    }
}

/// A `NotificationClosed(id, reason)` owed to the bus.
pub type Closed = (u32, u32);

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// `Notify`: a new id, or `replaces_id` updated in place when it is one
    /// this daemon holds (same id, same place, unread again, its toast
    /// restarted). A `replaces_id` it does not hold is a new notification, as
    /// the specification says. Returns the id and whatever fell off the end.
    pub fn notify(&mut self, n: Incoming, now: Instant) -> (u32, Vec<Closed>) {
        let wall = SystemTime::now();
        if n.replaces_id != 0
            && let Some(entry) = self.entries.iter_mut().find(|e| e.id == n.replaces_id)
        {
            let id = entry.id;
            *entry = Entry::from_incoming(id, n, now, wall);
            return (id, Vec::new());
        }
        let id = self.next_id;
        // Wrapping past u32 is four billion notifications; 0 is never an id.
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.entries
            .insert(0, Entry::from_incoming(id, n, now, wall));
        let mut closed = Vec::new();
        while self.entries.len() > HISTORY_MAX {
            if let Some(old) = self.entries.pop() {
                closed.push((old.id, CLOSED_UNDEFINED));
            }
        }
        (id, closed)
    }

    /// Remove one; `true` when it was there.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() != before
    }

    /// Remove every one; their ids, newest first.
    pub fn clear(&mut self) -> Vec<u32> {
        self.entries.drain(..).map(|e| e.id).collect()
    }

    /// One entry read: a resident notification whose action ran stays, read.
    pub fn read(&mut self, id: u32) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.unread = false;
        }
    }

    /// Notification `id` was in a toast that has ended, by the bar's own
    /// account: shown, and it toasts no more. `true` when anything changed.
    pub fn mark_shown(&mut self, id: u32) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(entry) if !entry.shown => {
                entry.shown = true;
                true
            }
            _ => false,
        }
    }

    /// Every entry toasting at `now` is shown: do-not-disturb kept it from a
    /// toast. `true` when anything changed.
    pub fn mark_toasts_shown(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            if entry.toasting(now) {
                entry.shown = true;
                changed = true;
            }
        }
        changed
    }

    /// The clock's half of `shown`: every entry whose toast time has passed.
    /// `true` when anything changed.
    pub fn mark_past_shown(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            if !entry.shown && entry.toast_until().is_some_and(|until| until <= now) {
                entry.shown = true;
                changed = true;
            }
        }
        changed
    }

    /// The menu opened: everything is read. `true` when anything changed.
    pub fn mark_read(&mut self) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            changed |= std::mem::replace(&mut entry.unread, false);
        }
        changed
    }

    pub fn unread(&self) -> usize {
        self.entries.iter().filter(|e| e.unread).count()
    }

    /// An unread critical one: the chip's `RFLAG_ATTENTION`.
    pub fn critical_unread(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.unread && e.urgency >= URGENCY_CRITICAL)
    }

    /// Any toast live: the chip's `RFLAG_SHOW`, unless do-not-disturb.
    pub fn toasting(&self, now: Instant) -> bool {
        self.entries.iter().any(|e| e.toasting(now))
    }

    /// The transient ones whose toast is over leave, `expired` (a transient
    /// notification is shown and not kept); with do-not-disturb on there is
    /// no toast, so they leave at once.
    pub fn expire(&mut self, now: Instant, dnd: bool) -> Vec<Closed> {
        let mut closed = Vec::new();
        self.entries.retain(|e| {
            let keep = !e.transient || (!dnd && e.toasting(now));
            if !keep {
                closed.push((e.id, CLOSED_EXPIRED));
            }
            keep
        });
        closed
    }

    /// The next moment the picture changes by the clock alone — a toast
    /// ending, which takes `RFLAG_ATTENTION` off its row and, with the last,
    /// `RFLAG_SHOW` off the chip — or `None` when no toast is counting down.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.entries
            .iter()
            .filter(|e| e.toasting(now))
            .filter_map(Entry::toast_until)
            .filter(|until| *until > now)
            .min()
    }
}

// --- The daemon remembers across restarts ------------------------------------

/// The history file's format version: what [`Snapshot::version`] must say
/// for a file to be read.
pub const SNAPSHOT_VERSION: u32 = 1;

/// What the history file holds: the id counter and every entry that is not
/// transient, newest first. Serialised as JSON
/// by `lib.rs`, which owns the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub next_id: u32,
    pub entries: Vec<Saved>,
}

/// One entry as the file keeps it. Not `transient` (a transient entry is not
/// written), not `shown` (every restored entry is shown), not `received` (an
/// `Instant` means nothing to another process; it is rebuilt from `wall`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub id: u32,
    pub app_name: String,
    pub icon: SavedIcon,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub resident: bool,
    pub expire_timeout: i32,
    /// Seconds since the epoch.
    pub wall: u64,
    pub unread: bool,
    pub sender: Option<String>,
}

/// [`Icon`] as the file keeps it: the pixels as their key and their texels,
/// hex (a JSON array of numbers would be three times the bytes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedIcon {
    None,
    Name(String),
    Pixels {
        key: String,
        #[serde(with = "hex")]
        rgba: Vec<u8>,
    },
}

impl History {
    /// What the history file holds now (pure; `lib.rs` writes it).
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            version: SNAPSHOT_VERSION,
            next_id: self.next_id,
            entries: self
                .entries
                .iter()
                .filter(|e| !e.transient)
                .map(Saved::from)
                .collect(),
        }
    }

    /// The history a file held, as of `now` (pure; `lib.rs` reads the file
    /// and checks the version). Every entry comes back **shown** — a restart
    /// re-toasts nothing — with `unread` as it was; `received` is `now` less
    /// its age on the wall, so "5 min ago" holds across the restart (a `wall`
    /// in the future is "now"). At most [`HISTORY_MAX`], the newest; an id
    /// that appears twice keeps its first (newest) entry; a pixmap of the
    /// wrong size comes back as no icon.
    pub fn restore(snapshot: Snapshot, now: Instant) -> History {
        let wall_now = SystemTime::now();
        let mut seen = std::collections::HashSet::new();
        let entries = snapshot
            .entries
            .into_iter()
            .filter(|saved| saved.id != 0 && seen.insert(saved.id))
            .take(HISTORY_MAX)
            .map(|saved| saved.into_entry(now, wall_now))
            .collect();
        History {
            entries,
            next_id: snapshot.next_id.max(1),
        }
    }
}

impl From<&Entry> for Saved {
    fn from(entry: &Entry) -> Saved {
        Saved {
            id: entry.id,
            app_name: entry.app_name.clone(),
            icon: match &entry.icon {
                Icon::None => SavedIcon::None,
                Icon::Name(name) => SavedIcon::Name(name.clone()),
                Icon::Pixels(fitted) => SavedIcon::Pixels {
                    key: fitted.key.clone(),
                    rgba: fitted.rgba.clone(),
                },
            },
            summary: entry.summary.clone(),
            body: entry.body.clone(),
            actions: entry.actions.clone(),
            urgency: entry.urgency,
            resident: entry.resident,
            expire_timeout: entry.expire_timeout,
            wall: entry
                .wall
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            unread: entry.unread,
            sender: entry.sender.clone(),
        }
    }
}

impl Saved {
    fn into_entry(self, now: Instant, wall_now: SystemTime) -> Entry {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(self.wall);
        let age = wall_now.duration_since(wall).unwrap_or(Duration::ZERO);
        // On Linux an `Instant` is signed seconds on the monotonic clock, so
        // an age older than the boot still subtracts; `now` only if it
        // cannot be represented at all.
        let received = now.checked_sub(age).unwrap_or(now);
        let slot = (caustics_bar_abi::IMAGE_SLOT_PX as usize).pow(2) * 4;
        let icon = match self.icon {
            SavedIcon::None => Icon::None,
            SavedIcon::Name(name) => Icon::Name(name),
            SavedIcon::Pixels { key, rgba } if rgba.len() == slot => {
                Icon::Pixels(Fitted { key, rgba })
            }
            SavedIcon::Pixels { .. } => Icon::None,
        };
        Entry {
            id: self.id,
            app_name: self.app_name,
            icon,
            summary: self.summary,
            body: self.body,
            actions: self.actions,
            urgency: self.urgency,
            transient: false,
            resident: self.resident,
            expire_timeout: self.expire_timeout,
            sender: self.sender,
            received,
            wall,
            unread: self.unread,
            shown: true,
        }
    }
}

/// Bytes as a lowercase hex string, for [`SavedIcon::Pixels`].
mod hex {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(DIGITS[usize::from(b >> 4)] as char);
            out.push(DIGITS[usize::from(b & 0xf)] as char);
        }
        s.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        if text.len() % 2 != 0 {
            return Err(D::Error::custom("hex of odd length"));
        }
        text.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or_else(|| D::Error::custom("not hex"))
            })
            .collect()
    }
}

impl Entry {
    fn from_incoming(id: u32, n: Incoming, now: Instant, wall: SystemTime) -> Entry {
        Entry {
            id,
            app_name: n.app_name,
            icon: n.icon,
            summary: n.summary,
            body: n.body,
            actions: n.actions,
            urgency: n.urgency,
            transient: n.transient,
            resident: n.resident,
            expire_timeout: n.expire_timeout,
            sender: n.sender,
            received: now,
            wall,
            unread: true,
            shown: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming(summary: &str) -> Incoming {
        Incoming {
            app_name: "app".to_owned(),
            replaces_id: 0,
            icon: Icon::Name("dialog-information".to_owned()),
            summary: summary.to_owned(),
            body: "body".to_owned(),
            actions: vec![("default".to_owned(), String::new())],
            urgency: URGENCY_NORMAL,
            transient: false,
            resident: false,
            expire_timeout: -1,
            sender: Some(":1.42".to_owned()),
        }
    }

    fn pixels() -> Fitted {
        let rgba = vec![0x7f; (caustics_bar_abi::IMAGE_SLOT_PX as usize).pow(2) * 4];
        Fitted {
            key: "caustics-notify:0123456789abcdef".to_owned(),
            rgba,
        }
    }

    fn saved(id: u32, wall: u64) -> Saved {
        Saved {
            id,
            app_name: "app".to_owned(),
            icon: SavedIcon::None,
            summary: "s".to_owned(),
            body: String::new(),
            actions: Vec::new(),
            urgency: URGENCY_NORMAL,
            resident: false,
            expire_timeout: -1,
            wall,
            unread: true,
            sender: None,
        }
    }

    /// Through the file's bytes, as `lib.rs` writes and reads them.
    fn through_json(snapshot: &Snapshot) -> Snapshot {
        let bytes = serde_json::to_vec(snapshot).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn a_snapshot_round_trips_and_ids_continue() {
        let start = Instant::now();
        let mut history = History::new();
        let (a, _) = history.notify(incoming("First"), start);
        let mut with_pixels = incoming("Avatar");
        with_pixels.icon = Icon::Pixels(pixels());
        let (b, _) = history.notify(with_pixels, start);
        let mut transient = incoming("Blip");
        transient.transient = true;
        let (t, _) = history.notify(transient, start);
        history.read(a);

        let snapshot = history.snapshot();
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(snapshot.next_id, 4);
        let ids: Vec<u32> = snapshot.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, [b, a], "newest first, the transient one absent");
        let json = through_json(&snapshot);
        assert_eq!(json, snapshot);

        let now = Instant::now();
        let mut restored = History::restore(json, now);
        let ids: Vec<u32> = restored.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, [b, a]);
        assert!(restored.get(t).is_none());
        assert!(
            restored.entries().iter().all(|e| e.shown),
            "a restart re-toasts nothing"
        );
        assert!(!restored.toasting(now));
        assert_eq!(restored.unread(), 1, "unread is kept: b, not a");
        let entry = restored.get(b).unwrap();
        assert_eq!(entry.icon, Icon::Pixels(pixels()));
        assert_eq!(entry.sender.as_deref(), Some(":1.42"));
        assert_eq!(entry.summary, "Avatar");
        assert_eq!(entry.actions, [("default".to_owned(), String::new())]);
        // The same history makes the same snapshot: no write after a restore.
        assert_eq!(restored.snapshot(), snapshot);

        let (c, _) = restored.notify(incoming("Third"), now);
        assert_eq!(c, 4, "ids continue from next_id");
    }

    #[test]
    fn received_is_rebuilt_from_the_wall() {
        let now = Instant::now();
        let wall_now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            next_id: 10,
            entries: vec![
                saved(3, wall_now - 300),
                saved(2, wall_now + 3600),
                saved(1, wall_now - 3 * 86_400),
            ],
        };
        let history = History::restore(snapshot, now);
        let age = |id| now.saturating_duration_since(history.get(id).unwrap().received);
        let five = age(3).as_secs();
        assert!((299..=302).contains(&five), "five minutes ago: {five} s");
        assert_eq!(age(2), Duration::ZERO, "a wall in the future is now");
        let days = age(1).as_secs();
        assert!(
            days >= 3 * 86_400 - 1,
            "three days, older than the boot or not: {days} s"
        );
    }

    #[test]
    fn restore_caps_and_drops_what_it_cannot_use() {
        let now = Instant::now();
        let mut history = History::new();
        for i in 0..HISTORY_MAX + 5 {
            history.notify(incoming(&format!("n{i}")), now);
        }
        let mut snapshot = history.snapshot();
        assert_eq!(snapshot.entries.len(), HISTORY_MAX);
        // A hand-edited file: more than the cap, a repeated id, an id of 0,
        // a pixmap of the wrong size, and a next_id of 0.
        let mut extra = saved(9999, 0);
        extra.icon = SavedIcon::Pixels {
            key: "caustics-notify:short".to_owned(),
            rgba: vec![1, 2, 3],
        };
        let zero = saved(0, 0);
        let duplicate = snapshot.entries[1].clone();
        snapshot.entries.insert(0, extra);
        snapshot.entries.insert(1, zero);
        snapshot.entries.insert(2, duplicate.clone());
        snapshot.next_id = 0;

        let mut restored = History::restore(snapshot, now);
        assert_eq!(restored.entries().len(), HISTORY_MAX, "the cap applies");
        assert_eq!(restored.get(9999).unwrap().icon, Icon::None);
        assert!(restored.get(0).is_none());
        assert_eq!(
            restored
                .entries()
                .iter()
                .filter(|e| e.id == duplicate.id)
                .count(),
            1
        );
        let (id, _) = restored.notify(incoming("after"), now);
        assert_eq!(id, 1, "a next_id of 0 is 1");
    }

    #[test]
    fn hex_is_lowercase_and_odd_or_foreign_digits_fail() {
        let mut one = saved(1, 0);
        one.icon = SavedIcon::Pixels {
            key: "k".to_owned(),
            rgba: vec![0x00, 0xab, 0xff],
        };
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            next_id: 2,
            entries: vec![one],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains(r#""rgba":"00abff""#), "{json}");
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), snapshot);
        assert!(serde_json::from_str::<Snapshot>(&json.replace("00abff", "00abf")).is_err());
        assert!(serde_json::from_str::<Snapshot>(&json.replace("00abff", "00abzz")).is_err());
    }
}
