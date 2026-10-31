//! The one frame this sidecar publishes: `APPLET_NOTIFY`, a bell chip with the
//! unread count, and a menu of do-not-disturb, the history newest first, and
//! clear-all.
//!
//! Whole-state, like every rows frame. **Ids are stable**: the fixed rows have
//! constants, a notification's row is its own notification id — which
//! `replaces_id` keeps — and an action row is derived from that id and the
//! action's index, so a row that changes under an open menu keeps its id and
//! the caustic, which holds an open menu level by id, keeps its place.

use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::{APPLET_NOTIFY, ROWS_ARENA_BYTES};
use caustics_bar_abi::wire::{
    MAX_ROWS, RFLAG_ACTIVATION, RFLAG_ATTENTION, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED,
    RFLAG_HAS_MENU, RFLAG_SHOW, RFLAG_SUBMENU, Row, Rows, RowsBuilder,
};

use crate::history::{DEFAULT_ACTION, Entry, History, Icon};

/// The chip.
pub const ROW_CHIP: u32 = 0xFFFF_0000;
/// "Do Not Disturb", checkable.
pub const ROW_DND: u32 = 0xFFFF_0001;
/// "Clear all".
pub const ROW_CLEAR: u32 = 0xFFFF_0002;
// 0xFFFF_0003 was the settings row; ids are never reused.
/// Below do-not-disturb.
pub const ROW_SEP_TOP: u32 = 0xFFFF_0004;
/// Above clear-all.
pub const ROW_SEP_BOTTOM: u32 = 0xFFFF_0005;
/// "No notifications", disabled, while the history is empty.
pub const ROW_EMPTY: u32 = 0xFFFF_0006;

/// The bit every action row carries, and the index that is "Dismiss" and the
/// one that is the separator above it.
///
/// `(id << 8) | index` alone would collide with a *notification* row:
/// notification 256's row id is action row 0 of notification 1, and a
/// resident notification 1 can still be in the menu when 256 arrives. With
/// bit 31 set the two ranges are disjoint for every id below 2²³ (eight million notifications, counted across restarts since the counter persists), and the fixed `0xFFFF_00xx` ids are
/// reached only at id `0x7FFF00`.
pub const ACTION_ROW_BIT: u32 = 0x8000_0000;
pub const INDEX_SEPARATOR: u32 = 0xFE;
pub const INDEX_DISMISS: u32 = 0xFF;

/// Notification rows a menu shows; the rest stay in the history (the menu
/// scrolls, about 36 rows at a time on a 1000-px output at the shipped row height: forty is a
/// screenful and a little more).
pub const MENU_MAX: usize = 40;
/// Actions one notification's submenu lists; a client with more is cut.
pub const ACTIONS_MAX: usize = 16;
/// A notification row's label — its summary — before its age, in chars.
pub const LABEL_MAX: usize = 96;
/// The body bytes one frame carries across all its rows:
/// each row's body is cut to this shared out over the rows shown, and never
/// past [`markup::BODY_MAX`](crate::markup::BODY_MAX). Forty rows get about
/// 800 bytes each — more than the bar shapes of one — and a few rows get
/// their whole body; the region keeps room for the labels, icons and rows.
pub const BODIES_BUDGET: usize = 32 * 1024;
/// An action row's label, in chars.
pub const ACTION_LABEL_MAX: usize = 64;
/// An icon name or path longer than this is dropped: a path is the client's,
/// and the frame has a region of [`ROWS_ARENA_BYTES`] to fit in.
pub const ICON_MAX: usize = 512;
/// The frame body bytes (header, rows and blob) a frame may reach before
/// notifications are left out: the region less a margin for the rows that
/// always come after them.
pub const BODY_BUDGET: usize = ROWS_ARENA_BYTES - 4096;

/// The chip's icons, both in Adwaita (`symbolic/legacy/preferences-system-notifications-symbolic.svg` and
/// `symbolic/status/notifications-disabled-symbolic.svg`; Adwaita has no
/// `notifications-symbolic`).
pub const ICON_BELL: &str = "preferences-system-notifications-symbolic";
pub const ICON_DND: &str = "notifications-disabled-symbolic";

pub const DND_LABEL: &str = "Do Not Disturb";
pub const EMPTY_LABEL: &str = "No notifications";
pub const CLEAR_LABEL: &str = "Clear all";
pub const DISMISS_LABEL: &str = "Dismiss";
/// A `default` action whose label is empty.
pub const OPEN_LABEL: &str = "Open";

/// An action row's id.
pub fn action_row(id: u32, index: u32) -> u32 {
    ACTION_ROW_BIT | (id << 8) | (index & 0xFF)
}

/// An action row's notification id and index, when `row` is one.
pub fn split_action_row(row: u32) -> Option<(u32, u32)> {
    (row & ACTION_ROW_BIT != 0 && row & 0xFFFF_0000 != 0xFFFF_0000)
        .then_some(((row & !ACTION_ROW_BIT) >> 8, row & 0xFF))
}

/// What a frame is made of beyond the history.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// The daemon owns `org.freedesktop.Notifications`. Without it there is
    /// no chip: the frame has no rows.
    pub owned: bool,
    pub dnd: bool,
    pub now: Instant,
    /// The length of a minute for "5 min ago" — a minute, except in a test.
    pub minute: Duration,
    /// Whether the labels carry their age. `false` builds the frame the
    /// daemon compares against to tell an age-only change from a real one
    /// ([`worth_sending`]).
    pub ages: bool,
}

/// Whether `next` — the frame and its ageless twin (`View::ages` false) — is
/// worth writing after `prev`.
///
/// A frame identical to the last is never written. A frame that differs from
/// the last **only in its age labels** ("now" becoming "1 min ago") is not
/// written while a toast is live (the chip carries `RFLAG_SHOW`): every frame
/// the bar takes while a toast is up is a rebuild under the pointer, and the
/// ages are not news. It goes out with the next real change, or when the toast
/// ends — which is itself a change, so no age is ever held for long.
pub fn worth_sending(prev: Option<&(Rows, Rows)>, next: &(Rows, Rows)) -> bool {
    let (rows, ageless) = next;
    let Some((last, last_ageless)) = prev else {
        // Nothing published yet, and nothing to publish: no chip is what a
        // slot with no frame already shows.
        return !rows.rows.is_empty();
    };
    if last.rows == rows.rows && last.bytes == rows.bytes {
        return false;
    }
    let toasting = rows
        .rows
        .first()
        .is_some_and(|chip| chip.flags & RFLAG_SHOW != 0);
    let ages_only = last_ageless.rows == ageless.rows && last_ageless.bytes == ageless.bytes;
    !(toasting && ages_only)
}

/// The picture, and the minted pixmaps its rows name, in row order.
pub fn picture(revision: u32, history: &History, view: View) -> (Rows, Vec<(String, Vec<u8>)>) {
    let mut rows = RowsBuilder::new(APPLET_NOTIFY, revision);
    let mut pixmaps = Vec::new();
    if !view.owned {
        return (rows.build(), pixmaps);
    }
    let mut budget = Budget::default();

    let unread = history.unread();
    let count = (unread > 0).then(|| unread.to_string());
    let mut flags = RFLAG_HAS_MENU;
    if history.critical_unread() {
        flags |= RFLAG_ATTENTION;
    }
    if !view.dnd && history.toasting(view.now) {
        flags |= RFLAG_SHOW;
    }
    let icon = if view.dnd { ICON_DND } else { ICON_BELL };
    let chip = rows.item(ROW_CHIP, count.as_deref(), Some(icon), -1.0, flags);
    budget.take(1, count.as_deref().map_or(0, str::len) + icon.len());

    let dnd = RFLAG_CHECKABLE | if view.dnd { RFLAG_CHECKED } else { 0 };
    rows.menu(chip, ROW_DND, Some(DND_LABEL), None, dnd);
    rows.separator(chip, ROW_SEP_TOP);
    budget.take(2, DND_LABEL.len());
    // What always comes after the notifications, reserved now.
    budget.take(2, CLEAR_LABEL.len() + EMPTY_LABEL.len());

    let rows_shown = history.entries().len().min(MENU_MAX).max(1);
    let body_max = (BODIES_BUDGET / rows_shown).min(crate::markup::BODY_MAX);
    let mut shown = 0;
    for entry in history.entries().iter().take(MENU_MAX) {
        let label = label(entry, view);
        let body = crate::markup::cut_bytes(&entry.body, body_max);
        let icon = entry.icon.name().filter(|name| name.len() <= ICON_MAX);
        let actions = submenu(entry);
        let need_rows = 1 + if actions.is_empty() {
            0
        } else {
            actions.len() + 2
        };
        let need_bytes = label.len()
            + body.len()
            + icon.map_or(0, str::len)
            + actions.iter().map(|(_, l)| l.len()).sum::<usize>()
            + DISMISS_LABEL.len();
        if !budget.fits(need_rows, need_bytes) {
            break;
        }
        budget.take(need_rows, need_bytes);

        // Attention is the toast's pick: exactly the
        // entries toasting now — unread, never shown, within their time —
        // not every unread one, or each new toast re-shows the old.
        let mut flags = 0;
        if entry.toasting(view.now) {
            flags |= RFLAG_ATTENTION;
        }
        if !actions.is_empty() {
            flags |= RFLAG_SUBMENU;
        }
        // A click that invokes the `default` action hands the app focus with
        // a token; one that only dismisses asks for none.
        if entry.has_default() {
            flags |= RFLAG_ACTIVATION;
        }
        let row = rows.menu(chip, entry.id, Some(&label), icon, flags);
        // The whole body beside the summary, for the caustic to wrap under
        // it. The budget keeps the blob far below the 64 KiB a row's 16-bit body offset and length can address (`BODY_FIELD_MAX`), so this always attaches.
        rows.body(row, &body);
        if let Icon::Pixels(fitted) = &entry.icon
            && icon.is_some()
        {
            pixmaps.push((fitted.key.clone(), fitted.rgba.clone()));
        }
        if !actions.is_empty() {
            for (index, label) in &actions {
                let id = action_row(entry.id, *index);
                rows.menu(row, id, Some(label), None, RFLAG_ACTIVATION);
            }
            rows.separator(row, action_row(entry.id, INDEX_SEPARATOR));
            rows.menu(
                row,
                action_row(entry.id, INDEX_DISMISS),
                Some(DISMISS_LABEL),
                None,
                0,
            );
        }
        shown += 1;
    }
    if shown == 0 {
        rows.menu(chip, ROW_EMPTY, Some(EMPTY_LABEL), None, RFLAG_DISABLED);
    }

    rows.separator(chip, ROW_SEP_BOTTOM);
    let clear = if history.entries().is_empty() {
        RFLAG_DISABLED
    } else {
        0
    };
    rows.menu(chip, ROW_CLEAR, Some(CLEAR_LABEL), None, clear);
    (rows.build(), pixmaps)
}

/// The summary's first line cut to [`LABEL_MAX`] chars on a char boundary,
/// then `" · 5 min ago"`: the row's title. The summary is the app's name when
/// it is empty. The body is not in it: it travels as the row's second string
/// and the caustic draws it wrapped under this. With `view.ages` false, the
/// summary alone: the twin [`worth_sending`] compares to tell an age-only
/// change.
pub fn label(entry: &Entry, view: View) -> String {
    let summary = if entry.summary.trim().is_empty() {
        &entry.app_name
    } else {
        &entry.summary
    };
    let summary = crate::markup::first_line(summary);
    if !view.ages {
        return cut(summary, LABEL_MAX);
    }
    let age = view.now.saturating_duration_since(entry.received);
    format!(
        "{} · {}",
        cut(summary, LABEL_MAX),
        age_label(age, view.minute)
    )
}

/// How long ago, in the words the row ends with.
pub fn age_label(age: Duration, minute: Duration) -> String {
    let minutes = (age.as_nanos() / minute.as_nanos().max(1)) as u64;
    match minutes {
        0 => "now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

/// `text` cut to `max` chars, an ellipsis standing in for the rest.
pub fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some(_) => {
            let keep = text
                .char_indices()
                .nth(max - 1)
                .map_or(text.len(), |(i, _)| i);
            format!("{}…", &text[..keep])
        }
    }
}

/// The submenu's action rows, `(index, label)`: every action — `default`
/// first under its own label, or "Open" — when there is any action other than
/// `default`; none otherwise.
fn submenu(entry: &Entry) -> Vec<(u32, String)> {
    if !entry.has_named_actions() {
        return Vec::new();
    }
    let mut out: Vec<(u32, String)> = entry
        .actions
        .iter()
        .take(ACTIONS_MAX)
        .enumerate()
        .map(|(index, (key, label))| {
            let label = if label.trim().is_empty() {
                if key == DEFAULT_ACTION {
                    OPEN_LABEL
                } else {
                    key.as_str()
                }
            } else {
                label.as_str()
            };
            (index as u32, cut(label, ACTION_LABEL_MAX))
        })
        .collect();
    out.sort_by_key(|(index, _)| entry.actions[*index as usize].0 != DEFAULT_ACTION);
    out
}

/// Rows and blob bytes spent so far, against [`MAX_ROWS`] and
/// [`BODY_BUDGET`].
#[derive(Default)]
struct Budget {
    rows: usize,
    bytes: usize,
}

impl Budget {
    fn fits(&self, rows: usize, bytes: usize) -> bool {
        self.rows + rows <= MAX_ROWS
            && 16 + (self.rows + rows) * size_of::<Row>() + self.bytes + bytes <= BODY_BUDGET
    }

    fn take(&mut self, rows: usize, bytes: usize) {
        self.rows += rows;
        self.bytes += bytes;
    }
}
