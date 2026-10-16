use super::*;

const WINDOWS: &str = include_str!("../../../tests/fixtures/niri-windows.json");
const EVENTS: &str = include_str!("../../../tests/fixtures/niri-events.jsonl");
const VERSION: &str = include_str!("../../../tests/fixtures/niri-version.json");

/// The four action requests, byte for byte, as niri's `Request`/`Action`
/// serde shapes produce them.
#[test]
fn action_requests_match_the_wire_shape() {
    assert_eq!(
        json!({ "Action": { "FocusWindow": { "id": 4u64 } } }).to_string(),
        r#"{"Action":{"FocusWindow":{"id":4}}}"#
    );
    assert_eq!(
        json!({ "Action": { "CloseWindow": { "id": 4u64 } } }).to_string(),
        r#"{"Action":{"CloseWindow":{"id":4}}}"#
    );
    assert_eq!(
        json!({ "Action": { "ToggleOverview": {} } }).to_string(),
        r#"{"Action":{"ToggleOverview":{}}}"#
    );
    assert_eq!(json!("EventStream").to_string(), r#""EventStream""#);
}

/// The launch request, byte for byte.
///
/// niri's `Action::Spawn` is `{ command: Vec<String> }` (its `niri-ipc` crate, consulted as documentation and not linked), so the argv goes across whole
/// and niri does the starting — no shell, which is why the sibling `SpawnSh`
/// is not the one used. Both arms of the embassy's resolution are pinned:
/// `gio launch <path>` where the machine has gio, and the entry's own
/// `Exec=` where it does not.
#[test]
fn a_launch_is_niris_spawn_with_the_argv_whole() {
    let gio = vec![
        "/usr/bin/gio".to_owned(),
        "launch".to_owned(),
        "/usr/share/applications/kitty.desktop".to_owned(),
    ];
    assert_eq!(
        json!({ "Action": { "Spawn": { "command": gio } } }).to_string(),
        r#"{"Action":{"Spawn":{"command":["/usr/bin/gio","launch","/usr/share/applications/kitty.desktop"]}}}"#
    );

    let exec = vec!["kitty".to_owned(), "--single-instance".to_owned()];
    assert_eq!(
        json!({ "Action": { "Spawn": { "command": exec } } }).to_string(),
        r#"{"Action":{"Spawn":{"command":["kitty","--single-instance"]}}}"#
    );
}

/// The feeder claims a `Launch` and builds that request from the action
/// itself — the join between `model.rs` and the wire, which the shape test
/// above cannot see because it writes the JSON by hand.
///
/// With no socket the round trip is skipped and the action is still claimed:
/// a niri that is not running is one witness line, never a second owner
/// hunting for the same click.
#[test]
fn the_feeder_claims_a_launch_even_with_no_socket() {
    // Built rather than `start`ed: `start` reads `$NIRI_SOCKET`, and a test
    // that writes the process environment is a test that fights every other
    // test in the binary. This is the same struct `start` returns when the
    // variable is unset.
    let mut niri = NiriIpc {
        socket: None,
        stop: Arc::new(AtomicBool::new(false)),
    };
    assert!(!niri.is_bound());
    let argv = vec![
        "/usr/bin/gio".to_owned(),
        "launch".to_owned(),
        "/x/kitty.desktop".to_owned(),
    ];
    assert!(
        niri.perform(&Action::Launch { argv }),
        "a launch is this feeder's"
    );
    assert!(
        !niri.perform(&Action::Pin {
            app_id: "kitty".to_owned()
        }),
        "a pin is not"
    );
}

/// A `Reply` is `Result<Response, String>`, which serde writes as a
/// one-key object, which is the shape the fixtures are written in.
#[test]
fn a_reply_is_an_ok_or_err_object() {
    let v: Value = serde_json::from_str(VERSION.trim()).unwrap();
    assert_eq!(v["Ok"]["Version"].as_str().unwrap(), "26.04 (d12bd42)");
    let v: Value = serde_json::from_str(WINDOWS.trim()).unwrap();
    assert!(v["Ok"]["Windows"].is_array());
}

/// The opening line of the event stream is a `Reply`, not an `Event`; it
/// must fall through the translator without producing anything.
#[test]
fn the_event_stream_handshake_is_not_an_event() {
    let set = WindowSet::default();
    assert!(translate(r#"{"Ok":"Handled"}"#, &set).is_empty());
    assert!(translate(r#"{"WorkspacesChanged":{"workspaces":[]}}"#, &set).is_empty());
    assert!(translate("not json at all", &set).is_empty());
}

/// The fixture's event stream, line by line, against the shared window set.
#[test]
fn the_recorded_event_stream_builds_the_window_set() {
    let set = WindowSet::default();
    let mut updates = Vec::new();
    for line in EVENTS.lines().filter(|l| !l.trim().is_empty()) {
        updates.extend(translate(line, &set));
    }

    // The fixture's `WindowsChanged` lists five windows; then window 3 is
    // focused, window 6 turns urgent, window 9 opens focused, and window 4
    // closes.
    assert_eq!(set.ids(), vec![2, 3, 6, 9, 34]);
    let focused: Vec<u64> = set
        .ids()
        .into_iter()
        .filter(|id| set.get(*id).unwrap().focused)
        .collect();
    assert_eq!(focused, vec![9], "exactly one window is focused");
    assert_eq!(set.get(9).unwrap().app_id, "kitty");
    assert!(set.get(6).unwrap().urgent, "the urgency event applied");
    assert!(updates.contains(&Update::WindowClosed(4)));
    // Focus stamps order the dock's "most recently focused" pick.
    assert!(set.get(9).unwrap().focus_stamp > set.get(3).unwrap().focus_stamp);
}

/// `WindowsChanged` replaces the whole set, so a window it omits closed.
#[test]
fn windows_changed_closes_what_it_omits() {
    let set = WindowSet::default();
    translate(
        r#"{"WindowsChanged":{"windows":[{"id":1,"title":"a","app_id":"kitty","is_focused":false,"is_urgent":false}]}}"#,
        &set,
    );
    assert_eq!(set.ids(), vec![1]);
    let out = translate(r#"{"WindowsChanged":{"windows":[]}}"#, &set);
    assert_eq!(out, vec![Update::WindowClosed(1)]);
    assert!(set.ids().is_empty());
}

/// A null title or app id is an empty string, not a dropped window.
#[test]
fn a_window_without_a_title_still_counts() {
    let set = WindowSet::default();
    let out = translate(
        r#"{"WindowOpenedOrChanged":{"window":{"id":7,"title":null,"app_id":null,"is_focused":false,"is_urgent":false}}}"#,
        &set,
    );
    assert_eq!(out.len(), 1);
    let w = set.get(7).unwrap();
    assert_eq!((w.title.as_str(), w.app_id.as_str()), ("", ""));
}
