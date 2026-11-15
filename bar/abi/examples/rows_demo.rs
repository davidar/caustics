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
//! And, with `ROWS_DEMO_BLINK_MS=<ms>` in its environment, the worked example
//! for a sidecar that mints `Pixmap` keys (docs/abi.md, *Pixmap*): the chip's
//! icon alternates between two pictures of its own every `<ms>`, a sync
//! animation's shape, each sent once before the first frame that names it
//! and again on a `REQ_ICON` for it. `bar/bar-test.sh` drives this to check
//! that an alternating icon neither blinks out of its slot nor is sent again.
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

use caustics_bar_abi::IMAGE_SLOT_PX;
use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SECONDARY,
    ACTION_SET_VALUE, APPLET_OTHER,
};
use caustics_bar_abi::wire::{
    self, Frame, Pixmap, REQ_ICON, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_HAS_MENU, RowsBuilder,
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
/// `icon` is the chip's icon: the themed name, or in blink mode one of the
/// two keys this demo minted.
fn picture(revision: u32, level: f32, checked: bool, opens: Option<u32>, icon: &str) -> Frame {
    let label = format!("{:.0}%", level);
    let mut rows = RowsBuilder::new(APPLET_OTHER, revision);
    let bar = rows.item(
        ROW_BAR,
        Some(&label),
        Some(icon),
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

/// The chip's themed icon, outside blink mode.
const ICON: &str = "applications-system-symbolic";

/// One of the blink's two pictures: a flat square of one colour, straight
/// RGBA at the slot's size, under the key its texels hash to.
fn blink_picture(rgba: [u8; 4]) -> (String, Vec<u8>) {
    let texels: Vec<u8> = rgba
        .iter()
        .copied()
        .cycle()
        .take((IMAGE_SLOT_PX * IMAGE_SLOT_PX * 4) as usize)
        .collect();
    (wire::pixmap_key("rows_demo", &texels), texels)
}

fn pixmap_frame(key: &str, rgba: &[u8]) -> Frame {
    Frame::Pixmap(Pixmap {
        key: key.to_owned(),
        width: IMAGE_SLOT_PX,
        height: IMAGE_SLOT_PX,
        rgba: rgba.to_vec(),
    })
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
    // Blink mode: two pictures, both sent before the first frame (each is
    // named by every other frame, so both stay live and neither is sent
    // again), the chip alternating between them.
    let blink = std::env::var("ROWS_DEMO_BLINK_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(std::time::Duration::from_millis);
    let pictures = [
        blink_picture([220, 60, 40, 255]),
        blink_picture([40, 120, 220, 255]),
    ];
    let mut showing = 0usize;
    let icon = |showing: usize| -> &str {
        match blink {
            Some(_) => &pictures[showing].0,
            None => ICON,
        }
    };
    if blink.is_some() {
        for (key, rgba) in &pictures {
            if let Err(err) = wire::write(&mut out, &pixmap_frame(key, rgba)) {
                say(&format!("rows_demo: stdout: {err}"));
                std::process::exit(1);
            }
        }
        say(&format!(
            "rows_demo: blinking between {} and {} every {} ms",
            pictures[0].0,
            pictures[1].0,
            blink.map_or(0, |period| period.as_millis())
        ));
    }
    if let Err(err) = wire::write(
        &mut out,
        &picture(revision, level, checked, None, icon(showing)),
    ) {
        say(&format!("rows_demo: stdout: {err}"));
        std::process::exit(1);
    }
    say(&format!(
        "rows_demo: published rev={revision} level={level:.0}"
    ));

    // The bar holds the write end of this pipe, so its EOF is this process's
    // notice to leave — the one thing every sidecar must read. On a thread,
    // so the blink's clock can wake the loop between frames.
    let (frames, inbox) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        loop {
            let read = wire::read(&mut input);
            let done = !matches!(read, Ok(Some(_)));
            if frames.send(read).is_err() || done {
                return;
            }
        }
    });
    let period = blink.unwrap_or(std::time::Duration::from_secs(3600));
    loop {
        let read = match inbox.recv_timeout(period) {
            Ok(read) => read,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if blink.is_none() {
                    continue;
                }
                showing = 1 - showing;
                revision += 1;
                let shown = menu_open.then_some(opens);
                if let Err(err) = wire::write(
                    &mut out,
                    &picture(revision, level, checked, shown, icon(showing)),
                ) {
                    say(&format!("rows_demo: stdout: {err}"));
                    return;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(None),
        };
        match read {
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
                if let Err(err) = wire::write(
                    &mut out,
                    &picture(revision, level, checked, shown, icon(showing)),
                ) {
                    say(&format!("rows_demo: stdout: {err}"));
                    return;
                }
                say(&format!(
                    "rows_demo: published rev={revision} level={level:.0} opens={opens} menu={}",
                    if menu_open { "open" } else { "closed" }
                ));
            }
            // The bar asks a chip's own sidecar for the pixels of an icon
            // name its store lacks (docs/abi.md, *Request*): one of the two
            // keys minted here is its `Pixmap` again; anything else is
            // silence. Said on stderr only because echoing what it is sent is
            // what the demo is for.
            Ok(Some(Frame::Request(request))) => {
                let held = pictures.iter().find(|(key, _)| {
                    blink.is_some() && request.kind == REQ_ICON && *key == request.name
                });
                match held {
                    Some((key, rgba)) => {
                        say(&format!("rows_demo: request for {key}; sent again"));
                        if let Err(err) = wire::write(&mut out, &pixmap_frame(key, rgba)) {
                            say(&format!("rows_demo: stdout: {err}"));
                            return;
                        }
                    }
                    None => say(&format!(
                        "rows_demo: request kind {} for {:?}; not minted here, so silence",
                        request.kind, request.name
                    )),
                }
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
