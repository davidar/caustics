//! The frame and the tables behind it, without a bus.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::ROWS_ARENA_BYTES;
use caustics_bar_abi::wire::{
    self, ACTION_ACTIVATE, ACTION_MIDDLE, ACTION_TOAST_SHOWN, Frame, MAX_ROWS, RFLAG_DISABLED,
};
use caustics_notify::actions::{Op, op_for};
use caustics_notify::history::{HISTORY_MAX, History, Icon, Incoming};
use caustics_notify::rows::{
    self, INDEX_DISMISS, MENU_MAX, ROW_CHIP, ROW_CLEAR, ROW_DND, ROW_EMPTY, View, action_row,
    split_action_row,
};
use caustics_notify::{image, markup, server};
use zbus::zvariant::{OwnedValue, Value};

fn incoming(summary: &str) -> Incoming {
    Incoming {
        app_name: "app".into(),
        replaces_id: 0,
        icon: Icon::None,
        summary: summary.into(),
        body: String::new(),
        actions: Vec::new(),
        urgency: 1,
        transient: false,
        resident: false,
        expire_timeout: -1,
        sender: None,
    }
}

fn view(now: Instant) -> View {
    View {
        owned: true,
        dnd: false,
        now,
        minute: Duration::from_secs(60),
        ages: true,
    }
}

#[test]
fn the_named_tags_go_and_other_angle_brackets_stay() {
    assert_eq!(markup::strip("<b>Bold</b> and <i>it</i>"), "Bold and it");
    assert_eq!(markup::strip("<a href=\"https://x\">link</a>"), "link");
    assert_eq!(
        markup::strip("pic <img src=\"x.png\" alt=\"y\"/> done"),
        "pic  done"
    );
    assert_eq!(markup::strip("<U>under</U>"), "under");
    assert_eq!(markup::strip("a <3 b"), "a <3 b");
    assert_eq!(markup::strip("<span>kept</span>"), "<span>kept</span>");
    assert_eq!(markup::strip("one<br/>two"), "one\ntwo");
}

#[test]
fn entities_are_unescaped_after_the_tags() {
    assert_eq!(markup::strip("Tom &amp; Jerry &lt;3"), "Tom & Jerry <3");
    assert_eq!(
        markup::strip("&lt;b&gt;literal&lt;/b&gt;"),
        "<b>literal</b>"
    );
    assert_eq!(
        markup::strip("&#65;&#x42; &bogus; & &quot;q&apos;"),
        "AB &bogus; & \"q'"
    );
    assert_eq!(markup::strip("&amp;é"), "&é");
}

#[test]
fn the_first_line_skips_blank_ones() {
    assert_eq!(markup::first_line("\n  \nsecond\nthird"), "second");
    assert_eq!(markup::first_line(""), "");
}

#[test]
fn ages_and_cuts() {
    let m = Duration::from_secs(60);
    assert_eq!(rows::age_label(Duration::from_secs(5), m), "now");
    assert_eq!(
        rows::age_label(Duration::from_secs(5 * 60 + 3), m),
        "5 min ago"
    );
    assert_eq!(rows::age_label(Duration::from_secs(3 * 3600), m), "3 h ago");
    assert_eq!(
        rows::age_label(Duration::from_secs(2 * 86400), m),
        "2 d ago"
    );

    assert_eq!(rows::cut("short", 96), "short");
    let long = "é".repeat(100);
    let cut = rows::cut(&long, 96);
    assert_eq!(cut.chars().count(), 96);
    assert!(cut.ends_with('…'));
}

#[test]
fn a_label_is_the_summary_dot_age_and_the_body_is_apart() {
    let now = Instant::now();
    let mut history = History::new();
    let mut n = incoming("Summary");
    n.body = "the body".into();
    history.notify(n, now);
    let entry = &history.entries()[0];
    let later = View {
        now: now + Duration::from_secs(5 * 60),
        ..view(now)
    };
    assert_eq!(rows::label(entry, later), "Summary · 5 min ago");

    // No summary: the app's name stands in.
    let mut history = History::new();
    history.notify(incoming(""), now);
    assert_eq!(rows::label(&history.entries()[0], view(now)), "app · now");
}

/// The frame as the bar's reader sees it: written, read back through the
/// wire's own decode (which validates every range against the blob), and
/// the row for notification `id`'s label and body.
fn read_back(rows: &wire::Rows, id: u32) -> (String, Option<String>) {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &Frame::Rows(rows.clone())).unwrap();
    let Some(Frame::Rows(back)) = wire::read(&mut bytes.as_slice()).unwrap() else {
        panic!("the reader refused the frame");
    };
    let i = back
        .rows
        .iter()
        .position(|r| r.id == id)
        .expect("the notification's row");
    (
        back.label(i).expect("a label").into_owned(),
        back.body(i).map(|body| body.into_owned()),
    )
}

/// The summary is the label and the whole body, its own newlines kept, is the row's second range; only the summary is cut to `LABEL_MAX`.
#[test]
fn a_long_body_goes_whole_beside_the_summary() {
    let now = Instant::now();
    let mut history = History::new();
    let sentence = "The quick brown fox jumps over the lazy dog again. ";
    let raw: String = sentence.repeat(6);
    let raw = &raw[..300];
    assert_eq!(raw.chars().count(), 300);
    let mut n = incoming("Build finished");
    n.body = server::decode(
        "app".into(),
        0,
        "",
        "Build finished".into(),
        raw,
        Vec::new(),
        &HashMap::new(),
        -1,
    )
    .body;
    let (id, _) = history.notify(n, now);
    let (rows, _) = rows::picture(1, &history, view(now));
    let (label, body) = read_back(&rows, id);
    assert_eq!(label, "Build finished · now");
    let body = body.expect("the row carries its body");
    assert_eq!(body, raw.trim_end(), "the whole body");
    assert!(body.chars().count() > rows::LABEL_MAX);

    // The packed range is the body's own bytes in the blob.
    let row = rows.rows.iter().find(|r| r.id == id).unwrap();
    let (off, len) = row.body_range();
    assert_eq!(
        &rows.bytes[off as usize..(off + len) as usize],
        body.as_bytes()
    );
    assert!(rows.body_len() <= ROWS_ARENA_BYTES);
}

#[test]
fn a_bodys_newlines_are_kept_and_its_markup_is_not() {
    let decode = |body: &str| {
        server::decode(
            "app".into(),
            0,
            "",
            "S".into(),
            body,
            Vec::new(),
            &HashMap::new(),
            -1,
        )
        .body
    };
    assert_eq!(decode("one\ntwo\r\nthree\rfour"), "one\ntwo\nthree\nfour");
    assert_eq!(decode("\n\n  first  \n\nlast  \n\n"), "  first\n\nlast");
    assert_eq!(
        decode("<b>Tom</b> &amp; <i>Jerry</i><br/>on <a href=\"x\">two</a> lines"),
        "Tom & Jerry\non two lines"
    );

    let now = Instant::now();
    let mut history = History::new();
    let mut n = incoming("Two lines");
    n.body = decode("<b>first</b> line\nsecond line");
    let (id, _) = history.notify(n, now);
    let (rows, _) = rows::picture(1, &history, view(now));
    let (label, body) = read_back(&rows, id);
    assert_eq!(label, "Two lines · now");
    assert_eq!(body.as_deref(), Some("first line\nsecond line"));

    // No body, no range: the row draws its summary alone.
    let (id, _) = history.notify(incoming("Bare"), now);
    let (rows, _) = rows::picture(2, &history, view(now));
    assert_eq!(read_back(&rows, id).1, None);
    assert_eq!(rows.rows.iter().find(|r| r.id == id).unwrap().body, 0);
}

#[test]
fn a_body_is_capped_on_a_char_boundary() {
    let long = "é".repeat(2000); // 4000 bytes
    let body = markup::body(&long);
    assert!(body.len() <= markup::BODY_MAX, "{} bytes", body.len());
    assert!(body.ends_with('…'));
    assert!(body.trim_end_matches('…').chars().all(|c| c == 'é'));

    // Forty long bodies share the frame and every one still has one.
    let now = Instant::now();
    let mut history = History::new();
    for _ in 0..60 {
        let mut n = incoming("x");
        n.body = markup::body(&"word ".repeat(600));
        history.notify(n, now);
    }
    let (rows, _) = rows::picture(1, &history, view(now));
    assert!(
        rows.body_len() <= ROWS_ARENA_BYTES,
        "{} bytes",
        rows.body_len()
    );
    let bodies = rows
        .rows
        .iter()
        .filter(|r| r.id < 0x8000_0000 && r.body_range().1 > 0)
        .count();
    assert_eq!(bodies, MENU_MAX, "every notification shown has its body");
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &Frame::Rows(rows.clone())).unwrap();
    assert!(matches!(
        wire::read(&mut bytes.as_slice()).unwrap(),
        Some(Frame::Rows(_))
    ));
}

#[test]
fn action_row_ids_never_meet_a_notification_or_a_fixed_row() {
    for id in [1u32, 2, 255, 256, 257, 65_536, 0x7F_FEFF] {
        for index in [0u32, 1, 15, INDEX_DISMISS] {
            let row = action_row(id, index);
            assert_eq!(split_action_row(row), Some((id, index)));
            // A notification row is its id, which has bit 31 clear.
            assert!(row >= 0x8000_0000);
            assert!(row & 0xFFFF_0000 != 0xFFFF_0000);
        }
        assert_eq!(split_action_row(id), None);
    }
    for fixed in [ROW_CHIP, ROW_DND, ROW_CLEAR, ROW_EMPTY] {
        assert_eq!(split_action_row(fixed), None);
    }
}

#[test]
fn no_name_is_an_empty_frame_and_an_empty_history_says_so() {
    let now = Instant::now();
    let history = History::new();
    let (rows, _) = rows::picture(
        1,
        &history,
        View {
            owned: false,
            ..view(now)
        },
    );
    assert!(rows.rows.is_empty(), "no chip while the name is another's");

    let (rows, _) = rows::picture(1, &history, view(now));
    let ids: Vec<u32> = rows.rows.iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        [
            ROW_CHIP,
            ROW_DND,
            rows::ROW_SEP_TOP,
            ROW_EMPTY,
            rows::ROW_SEP_BOTTOM,
            ROW_CLEAR
        ]
    );
    let flags = |id| rows.rows.iter().find(|r| r.id == id).unwrap().flags;
    assert_ne!(flags(ROW_EMPTY) & RFLAG_DISABLED, 0);
    assert_ne!(flags(ROW_CLEAR) & RFLAG_DISABLED, 0);
}

/// With a notification, the menu still ends at "Clear all".
#[test]
fn the_menu_ends_at_clear_all() {
    let now = Instant::now();
    let mut history = History::new();
    history.notify(incoming("Summary"), now);
    let (rows, _) = rows::picture(1, &history, view(now));
    assert_eq!(rows.rows.last().unwrap().id, ROW_CLEAR);
}

#[test]
fn a_full_history_fits_the_rows_cap_and_the_region() {
    let now = Instant::now();
    let mut history = History::new();
    for n in 0..HISTORY_MAX + 10 {
        let mut i = incoming(&"S".repeat(200));
        i.body = "B".repeat(400);
        i.icon = Icon::Name(format!("/{}", "p".repeat(500)));
        i.actions = (0..20)
            .map(|k| (format!("k{k}"), "L".repeat(100)))
            .collect();
        let (_, closed) = history.notify(i, now);
        if n >= HISTORY_MAX {
            assert_eq!(closed.len(), 1, "the oldest falls off");
        }
    }
    assert_eq!(history.entries().len(), HISTORY_MAX);
    let (rows, _) = rows::picture(1, &history, view(now));
    assert!(rows.rows.len() <= MAX_ROWS);
    assert!(
        rows.body_len() <= ROWS_ARENA_BYTES,
        "{} bytes",
        rows.body_len()
    );
    // It round-trips through the reader the bar uses.
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &Frame::Rows(rows.clone())).unwrap();
    let Some(Frame::Rows(back)) = wire::read(&mut bytes.as_slice()).unwrap() else {
        panic!("not a rows frame");
    };
    assert_eq!(back.rows.len(), rows.rows.len());
    // And the tail is always there.
    assert_eq!(rows.rows.last().unwrap().id, ROW_CLEAR);

    // Without actions, the menu stops at MENU_MAX.
    let mut history = History::new();
    for _ in 0..100 {
        history.notify(incoming("x"), now);
    }
    let (rows, _) = rows::picture(1, &history, view(now));
    let notifications = rows.rows.iter().filter(|r| r.id < 0x8000_0000).count();
    assert_eq!(notifications, MENU_MAX);
}

#[test]
fn the_click_table() {
    let now = Instant::now();
    let mut history = History::new();
    let (plain, _) = history.notify(incoming("plain"), now);
    let mut d = incoming("default");
    d.actions = vec![("default".into(), "".into())];
    d.resident = true;
    let (with_default, _) = history.notify(d, now);
    let mut a = incoming("named");
    a.actions = vec![("reply".into(), "Reply".into())];
    let (named, _) = history.notify(a, now);

    assert_eq!(
        op_for(&history, ROW_DND, ACTION_ACTIVATE),
        Some(Op::ToggleDnd)
    );
    assert_eq!(
        op_for(&history, ROW_CHIP, ACTION_MIDDLE),
        Some(Op::ToggleDnd)
    );
    assert_eq!(
        op_for(&history, ROW_CLEAR, ACTION_ACTIVATE),
        Some(Op::ClearAll)
    );
    assert_eq!(
        op_for(&history, plain, ACTION_ACTIVATE),
        Some(Op::Invoke {
            id: plain,
            key: None,
            keep: false
        })
    );
    assert_eq!(
        op_for(&history, with_default, ACTION_ACTIVATE),
        Some(Op::Invoke {
            id: with_default,
            key: Some("default".into()),
            keep: true
        })
    );
    assert_eq!(
        op_for(&history, action_row(named, 0), ACTION_ACTIVATE),
        Some(Op::Invoke {
            id: named,
            key: Some("reply".into()),
            keep: false
        })
    );
    assert_eq!(
        op_for(&history, action_row(named, INDEX_DISMISS), ACTION_ACTIVATE),
        Some(Op::Invoke {
            id: named,
            key: None,
            keep: false
        })
    );
    assert_eq!(
        op_for(&history, action_row(named, 7), ACTION_ACTIVATE),
        None
    );
    assert_eq!(op_for(&history, 9999, ACTION_ACTIVATE), None);
}

#[test]
fn the_icon_precedence_and_the_image_unpacker() {
    let hints = |pairs: &[(&str, Value<'_>)]| -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_owned(),
                    OwnedValue::try_from(v.try_clone().unwrap()).unwrap(),
                )
            })
            .collect()
    };
    let rgba = (
        2i32,
        1i32,
        8i32,
        true,
        8i32,
        4i32,
        vec![1u8, 2, 3, 4, 5, 6, 7, 8],
    );
    let all = hints(&[
        ("image-data", Value::from(rgba.clone())),
        ("image-path", Value::from("/img.png")),
        ("desktop-entry", Value::from("org.app")),
    ]);
    assert!(matches!(server::icon("app-icon", &all), Icon::Pixels(_)));
    let no_data = hints(&[
        ("image-path", Value::from("file:///img.png")),
        ("desktop-entry", Value::from("org.app")),
    ]);
    assert_eq!(
        server::icon("app-icon", &no_data),
        Icon::Name("/img.png".into())
    );
    let entry_only = hints(&[("desktop-entry", Value::from("org.app"))]);
    assert_eq!(
        server::icon("app-icon", &entry_only),
        Icon::Name("app-icon".into())
    );
    // A desktop id is not an icon name, and `caustics-apps` resolves a row's
    // icon as a themed name alone — so the hint is no icon at all.
    assert_eq!(server::icon("", &entry_only), Icon::None);
    assert_eq!(
        server::icon("relative/path.png", &HashMap::new()),
        Icon::None
    );
    // The older spelling.
    let old = hints(&[("icon_data", Value::from(rgba.clone()))]);
    assert!(matches!(server::icon("", &old), Icon::Pixels(_)));

    let (w, h, px) = image::unpack(&rgba).unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(px, [1, 2, 3, 4, 5, 6, 7, 8]);
    // RGB at a padded stride.
    let rgb = (
        2i32,
        2i32,
        8i32,
        false,
        8i32,
        3i32,
        vec![1u8, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12],
    );
    let (_, _, px) = image::unpack(&rgb).unwrap();
    assert_eq!(
        px,
        [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
    );
    // Lies about its size.
    assert!(image::unpack(&(100, 100, 400, true, 8, 4, vec![0u8; 16])).is_none());
    assert!(image::unpack(&(1, 1, 4, true, 16, 4, vec![0u8; 8])).is_none());
}

#[test]
fn dnd_lives_under_the_state_home() {
    use std::ffi::OsString;
    use std::path::PathBuf;
    let p = caustics_notify::dnd_path(Some(OsString::from("/s")), Some(OsString::from("/h")));
    assert_eq!(p, PathBuf::from("/s/caustics/notify/dnd"));
    let p = caustics_notify::dnd_path(Some(OsString::from("rel")), Some(OsString::from("/h")));
    assert_eq!(p, PathBuf::from("/h/.local/state/caustics/notify/dnd"));
}

// --- The toast tells the truth ----------------------------------------------

fn attention(rows: &wire::Rows, id: u32) -> bool {
    rows.rows
        .iter()
        .find(|r| r.id == id)
        .expect("the row")
        .flags
        & wire::RFLAG_ATTENTION
        != 0
}

fn show(rows: &wire::Rows) -> bool {
    rows.rows[0].flags & wire::RFLAG_SHOW != 0
}

fn timed(summary: &str, expire_timeout: i32) -> Incoming {
    Incoming {
        expire_timeout,
        ..incoming(summary)
    }
}

/// A row wears `RFLAG_ATTENTION` exactly while it toasts —
/// unread, never shown, within its time — not while it is merely unread. The
/// other rule (attention = unread) would re-toast every unopened notification with
/// each new one.
#[test]
fn attention_is_toasting_not_unread() {
    let t0 = Instant::now();
    let mut history = History::new();
    let (a, _) = history.notify(timed("A", 10_000), t0);
    let (rows, _) = rows::picture(1, &history, view(t0));
    assert!(attention(&rows, a) && show(&rows));

    // Its time is up: still unread (the count says 1), no longer a toast.
    let late = t0 + Duration::from_secs(11);
    assert_eq!(
        history.next_deadline(t0),
        Some(t0 + Duration::from_secs(10))
    );
    let (rows, _) = rows::picture(2, &history, view(late));
    assert!(
        !attention(&rows, a),
        "a toast whose time is up leaves the toast"
    );
    assert!(!show(&rows));
    assert_eq!(
        rows.label(0).as_deref(),
        Some("1"),
        "and the count still counts it"
    );

    // A new one arrives: it toasts alone. A is never re-shown.
    history.mark_past_shown(late);
    assert!(history.get(a).unwrap().shown, "the clock marks it shown");
    let (b, _) = history.notify(timed("B", 10_000), late);
    let (rows, _) = rows::picture(3, &history, view(late));
    assert!(attention(&rows, b));
    assert!(
        !attention(&rows, a),
        "a shown notification never toasts again"
    );
    assert_eq!(rows.label(0).as_deref(), Some("2"));
    assert_eq!(
        history.next_deadline(late),
        Some(late + Duration::from_secs(10))
    );
}

/// The specification's 0 is "never expires": the toast stays until the
/// notification is clicked or dismissed — no deadline, attention an hour on.
#[test]
fn an_expire_timeout_of_zero_stays() {
    let t0 = Instant::now();
    let mut history = History::new();
    let (id, _) = history.notify(timed("Stays", 0), t0);
    let entry = history.get(id).unwrap();
    assert_eq!(entry.toast_until(), None);
    let hour = t0 + Duration::from_secs(3600);
    assert!(entry.toasting(hour));
    assert_eq!(history.next_deadline(t0), None, "nothing counts down");
    history.mark_past_shown(hour);
    let (rows, _) = rows::picture(1, &history, view(hour));
    assert!(
        attention(&rows, id) && show(&rows),
        "still a toast an hour later"
    );

    // −1 is still the server's choice: five seconds for a normal one.
    let (normal, _) = history.notify(timed("Normal", -1), t0);
    assert_eq!(
        history.get(normal).unwrap().toast_until(),
        Some(t0 + caustics_notify::history::TOAST_NORMAL)
    );
}

/// A toast that ended marks the rows it showed, and only those: the bar
/// sends `ACTION_TOAST_SHOWN` for each, so the toast does not come back with
/// "the rest" — and a row held behind
/// it, never on screen, is not among them.
#[test]
fn a_toast_that_ended_marks_its_rows_shown() {
    let t0 = Instant::now();
    let mut history = History::new();
    let (a, _) = history.notify(timed("A", 10_000), t0);
    let (b, _) = history.notify(timed("B", 10_000), t0 + Duration::from_secs(3));
    let (c, _) = history.notify(timed("C", 0), t0 + Duration::from_secs(4));
    let now = t0 + Duration::from_secs(5);
    // The click on A: A goes (no default action: a dismissal); the toast that
    // showed A, B and C ends and says so, one row at a time.
    assert_eq!(
        op_for(&history, a, ACTION_ACTIVATE),
        Some(Op::Invoke {
            id: a,
            key: None,
            keep: false
        })
    );
    history.remove(a);
    assert_eq!(
        op_for(&history, a, ACTION_TOAST_SHOWN),
        None,
        "a row that went is nobody's"
    );
    for id in [b, c] {
        assert_eq!(
            op_for(&history, id, ACTION_TOAST_SHOWN),
            Some(Op::ToastShown { id })
        );
        assert!(history.mark_shown(id));
        assert!(!history.mark_shown(id), "once");
    }
    let (rows, _) = rows::picture(1, &history, view(now));
    assert!(
        !attention(&rows, b) && !attention(&rows, c),
        "nothing reopens"
    );
    assert!(!show(&rows), "the chip stops asking");
    assert_eq!(rows.label(0).as_deref(), Some("2"), "both still unread");
    // Something new toasts on its own.
    let (d, _) = history.notify(timed("D", 10_000), now);
    let (rows, _) = rows::picture(2, &history, view(now));
    assert!(attention(&rows, d) && show(&rows));
    assert!(!attention(&rows, b) && !attention(&rows, c));

    // A replacement is news: it toasts again.
    let mut again = timed("B, again", 10_000);
    again.replaces_id = b;
    history.notify(again, now);
    let (rows, _) = rows::picture(3, &history, view(now));
    assert!(attention(&rows, b), "a replaced notification toasts again");
}

/// A right click on a notification's row is a dismissal: closed, no action
/// invoked, whatever actions it has.
#[test]
fn a_right_click_dismisses() {
    let now = Instant::now();
    let mut history = History::new();
    let mut n = incoming("with actions");
    n.actions = vec![
        ("default".into(), "Open".into()),
        ("reply".into(), "Reply".into()),
    ];
    n.resident = true;
    let (id, _) = history.notify(n, now);
    let dismiss = Some(Op::Invoke {
        id,
        key: None,
        keep: false,
    });
    assert_eq!(op_for(&history, id, wire::ACTION_SECONDARY), dismiss);
    assert_eq!(
        op_for(&history, action_row(id, 1), wire::ACTION_SECONDARY),
        dismiss
    );
    assert_eq!(op_for(&history, ROW_DND, wire::ACTION_SECONDARY), None);
    assert_eq!(op_for(&history, 9999, wire::ACTION_SECONDARY), None);
}

/// A frame whose only change is an age label ("now" → "2 min ago") is not
/// written while a toast is live: under a toast every frame is a rebuild, and
/// an age is not news. Without a toast it is written at once.
#[test]
fn an_age_alone_is_not_sent_while_a_toast_is_up() {
    let t0 = Instant::now();
    let mut history = History::new();
    history.notify(timed("Live", 600_000), t0);
    let frame = |history: &History, now: Instant| {
        let (rows, _) = rows::picture(1, history, view(now));
        let (ageless, _) = rows::picture(
            1,
            history,
            View {
                ages: false,
                ..view(now)
            },
        );
        (rows, ageless)
    };
    let first = frame(&history, t0);
    assert!(rows::worth_sending(None, &first));
    assert!(
        !rows::worth_sending(Some(&first), &first),
        "the same frame twice"
    );
    let later = frame(&history, t0 + Duration::from_secs(120));
    assert_ne!(first.0.bytes, later.0.bytes, "the label aged");
    assert!(show(&later.0));
    assert!(
        !rows::worth_sending(Some(&first), &later),
        "an age alone under a live toast is held"
    );
    // A real change goes out at once, ages and all.
    let (b, _) = history.notify(timed("Another", 600_000), t0 + Duration::from_secs(120));
    let news = frame(&history, t0 + Duration::from_secs(120));
    assert!(rows::worth_sending(Some(&first), &news));
    assert!(news.0.rows.iter().any(|r| r.id == b));

    // No toast: an age change is the frame's only news, and it is sent.
    let mut quiet = History::new();
    quiet.notify(timed("Old", 1000), t0);
    quiet.mark_past_shown(t0 + Duration::from_secs(60));
    let one = frame(&quiet, t0 + Duration::from_secs(60));
    let two = frame(&quiet, t0 + Duration::from_secs(180));
    assert!(!show(&one.0) && !show(&two.0));
    assert!(
        rows::worth_sending(Some(&one), &two),
        "no toast: the age goes out"
    );
}

/// The token is asked for only where a click invokes an action: a
/// notification with a `default` action and a named action's row carry
/// `RFLAG_ACTIVATION`; a plain notification (whose click dismisses), Dismiss,
/// do-not-disturb, clear-all and the chip do not.
#[test]
fn the_token_is_asked_for_only_where_an_action_runs() {
    let now = Instant::now();
    let mut history = History::new();
    let (plain, _) = history.notify(incoming("plain"), now);
    let mut d = incoming("default");
    d.actions = vec![("default".into(), "".into())];
    let (with_default, _) = history.notify(d, now);
    let mut a = incoming("named");
    a.actions = vec![
        ("default".into(), "".into()),
        ("reply".into(), "Reply".into()),
    ];
    let (named, _) = history.notify(a, now);
    let (rows, _) = rows::picture(1, &history, view(now));
    let wants = |id: u32| {
        rows.rows
            .iter()
            .find(|r| r.id == id)
            .expect("the row")
            .flags
            & wire::RFLAG_ACTIVATION
            != 0
    };
    assert!(wants(with_default) && wants(named));
    assert!(wants(action_row(named, 0)) && wants(action_row(named, 1)));
    assert!(!wants(plain), "a click that only dismisses wants no token");
    assert!(!wants(action_row(named, INDEX_DISMISS)));
    for fixed in [ROW_CHIP, ROW_DND, ROW_CLEAR] {
        assert!(!wants(fixed), "row {fixed:#x}");
    }
}

/// `image-data` is bounded by its area, 4 Mpx, not by a side: a wide banner
/// is unpacked, a claim past the area is not even looked at.
#[test]
fn an_image_is_bounded_by_its_area() {
    let banner = (
        8192i32,
        1i32,
        8192 * 3,
        false,
        8i32,
        3i32,
        vec![7u8; 8192 * 3],
    );
    let (w, h, px) = image::unpack(&banner).expect("a wide banner is fine");
    assert_eq!((w, h, px.len()), (8192, 1, 8192 * 4));
    // 2049 × 2048 is just past 4 Mpx, with every byte it claims present.
    let (w, h) = (2049i32, 2048i32);
    let big = (
        w,
        h,
        w * 3,
        false,
        8i32,
        3i32,
        vec![0u8; (w * h * 3) as usize],
    );
    assert!(
        image::unpack(&big).is_none(),
        "past {} px",
        image::MAX_PIXELS
    );
    // 2048² is exactly the bound.
    let (w, h) = (2048i32, 2048i32);
    let edge = (
        w,
        h,
        w * 3,
        false,
        8i32,
        3i32,
        vec![0u8; (w * h * 3) as usize],
    );
    assert!(image::unpack(&edge).is_some());
    // The 2³¹ claim costs nothing.
    assert!(image::unpack(&(65_536, 65_536, 196_608, false, 8, 3, Vec::new())).is_none());
}
