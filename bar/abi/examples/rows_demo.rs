//! `rows_demo` — the worked example for the rows frame (docs/abi.md, *Writing a sidecar*),
//! and the harness's proof that the rows path works with no real applet.
//!
//! It is a whole sidecar: one applet on the bar with an icon and a level, a
//! menu of three rows under it — one of them a slider — and an echo to stderr
//! of every action the bar sends back. Nothing here knows about Wayland, the
//! GPU, or the bar's internals; it depends on `caustics-bar-abi` and writes bytes.
//!
//! It is also the worked example for the two menu notices (docs/abi.md
//! *Action*). On `ACTION_MENU_OPEN` it appends a fourth menu row, `Opened N
//! times`, and republishes **while the menu is up**; on `ACTION_MENU_CLOSE` the
//! row goes again. `bar/bar-test.sh` drives this to check that a row arriving under an open menu leaves it open: the caustic
//! holds its menu level as an item id and re-lays the menu out in place.
//!
//! Run it by hand to see the wire:
//!
//! ```text
//! cargo run -p caustics-bar-abi --example rows_demo | xxd | head
//! ```
//!
//! or hand it to a bar:
//!
//! ```text
//! CAUSTICS_SIDECARS="caustics-sysmon target/debug/examples/rows_demo" caustics-bar
//! ```

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SECONDARY,
    ACTION_SET_VALUE, APPLET_OTHER,
};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_HAS_MENU, RowsBuilder,
};

/// The row ids this applet uses. Stable across frames, which is the contract:
/// an action comes back naming one of these.
const ROW_BAR: u32 = 1;
const ROW_HELLO: u32 = 2;
const ROW_TOGGLE: u32 = 3;
const ROW_LEVEL: u32 = 4;
/// The row that exists only while the menu is open — the proof that a sidecar's
/// rows may change under its own open menu.
const ROW_OPENS: u32 = 5;

/// The slider's own units — deliberately not 0..1, so the caustic's normalising on the way in and de-normalising on the way out are both visible in the echo.
const LEVEL_MIN: f32 = 0.0;
const LEVEL_MAX: f32 = 100.0;

/// The whole picture, whenever anything changes: a rows frame is whole-state,
/// so there is never a delta to track and a restart of either end needs no
/// replay.
/// `opens` is `Some(n)` while the bar says this applet's menu is up, and it is
/// what puts the fourth row in: the menu gains a row *while it is open*, which
/// is the whole point of the two notices.
fn picture(revision: u32, level: f32, checked: bool, opens: Option<u32>) -> Frame {
    let label = format!("{:.0}%", level);
    let mut rows = RowsBuilder::new(APPLET_OTHER, revision);
    let bar = rows.item(
        ROW_BAR,
        Some(&label),
        Some("applications-system-symbolic"),
        (level - LEVEL_MIN) / (LEVEL_MAX - LEVEL_MIN),
        RFLAG_HAS_MENU,
    );
    rows.menu(
        bar,
        ROW_HELLO,
        Some("Hello"),
        Some("dialog-information-symbolic"),
        0,
    );
    rows.menu(
        bar,
        ROW_TOGGLE,
        Some("Toggle"),
        None,
        RFLAG_CHECKABLE | if checked { RFLAG_CHECKED } else { 0 },
    );
    rows.slider(
        bar,
        ROW_LEVEL,
        Some("Level"),
        level,
        LEVEL_MIN,
        LEVEL_MAX,
        0,
    );
    if let Some(opens) = opens {
        let times = if opens == 1 { "time" } else { "times" };
        rows.menu(
            bar,
            ROW_OPENS,
            Some(&format!("Opened {opens} {times}")),
            None,
            0,
        );
    }
    rows.frame()
}

/// One line on stderr, in **one write**.
///
/// Not `eprintln!`: `Stderr` is unbuffered and `write_fmt` issues a write per
/// piece of the format string. A sidecar's stderr is the bar's, inherited, so
/// those pieces interleave with the bar's own tracing — and journald makes an
/// entry of whatever arrives between two newlines, which turns one witness
/// into three unreadable ones. Seen in the VM: `rows_demo: action row=` and
/// `2 kind=1 (activate) …` as two journal lines with a bar log between them.
fn say(line: &str) {
    use std::io::Write;

    let _ = std::io::stderr().write_all(format!("{line}\n").as_bytes());
}

fn kind_name(kind: u32) -> &'static str {
    match kind {
        ACTION_ACTIVATE => "activate",
        ACTION_SECONDARY => "secondary",
        ACTION_MIDDLE => "middle",
        ACTION_SET_VALUE => "set-value",
        ACTION_MENU_OPEN => "menu-open",
        ACTION_MENU_CLOSE => "menu-close",
        _ => "?",
    }
}

fn main() {
    let mut level = 40.0f32;
    let mut checked = false;
    let mut revision = 1u32;
    // How many times the bar has told us our menu opened, and whether it is
    // open now. The first is in a row's label; the second is whether that row
    // is in the frame at all.
    let mut opens = 0u32;
    let mut menu_open = false;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // The first frame, before the picture: which contract this was built
    // against (docs/abi.md, *Hello*). A worked example owes it like any other
    // sidecar — it is how the bar knows this demo is its own tree's.
    if let Err(err) = wire::hello(&mut out, "rows_demo") {
        say(&format!("rows_demo: stdout: {err}"));
        std::process::exit(1);
    }
    if let Err(err) = wire::write(&mut out, &picture(revision, level, checked, None)) {
        say(&format!("rows_demo: stdout: {err}"));
        std::process::exit(1);
    }
    say(&format!(
        "rows_demo: published rev={revision} level={level:.0}"
    ));

    // The bar holds the write end of this pipe, so its EOF is this process's
    // notice to leave — the one thing every sidecar must read.
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    loop {
        match wire::read(&mut input) {
            Ok(None) => {
                say("rows_demo: stdin closed; exiting");
                return;
            }
            Ok(Some(Frame::Action(action))) => {
                say(&format!(
                    "rows_demo: action row={} kind={} ({}) button={} value={:.2}",
                    action.row,
                    action.kind,
                    kind_name(action.kind),
                    action.button,
                    action.value
                ));
                match (action.row, action.kind) {
                    (ROW_LEVEL, ACTION_SET_VALUE) => {
                        level = action.value.clamp(LEVEL_MIN, LEVEL_MAX)
                    }
                    (ROW_TOGGLE, ACTION_ACTIVATE) => checked = !checked,
                    // The menu opened: count it, and publish the extra row
                    // into the menu that is *already up*.
                    (_, ACTION_MENU_OPEN) => {
                        opens = opens.saturating_add(1);
                        menu_open = true;
                    }
                    (_, ACTION_MENU_CLOSE) => menu_open = false,
                    _ => continue, // nothing changed, so nothing to publish
                }
                revision += 1;
                let shown = menu_open.then_some(opens);
                if let Err(err) = wire::write(&mut out, &picture(revision, level, checked, shown)) {
                    say(&format!("rows_demo: stdout: {err}"));
                    return;
                }
                say(&format!(
                    "rows_demo: published rev={revision} level={level:.0} opens={opens} menu={}",
                    if menu_open { "open" } else { "closed" }
                ));
            }
            Ok(Some(other)) => {
                say(&format!("rows_demo: ignoring a {} frame", other.kind()));
            }
            Err(err) => {
                say(&format!("rows_demo: stdin: {err}"));
                return;
            }
        }
    }
}
