//! The one test that needs a session bus: the built binary, spawned the way
//! the bar spawns it, read the way the bar reads it.
//!
//! `cargo test -p caustics-tray -- --ignored --nocapture`
//!
//! It **writes nothing to the child's stdin**: no `Activate`, no dbusmenu
//! `Event`, nothing that could reach a real application. It reads frames until
//! one carries an item with a menu, prints what it found, then closes stdin
//! and waits for the child to exit 0 — which is also the assertion of docs/abi.md, *Lifecycle*.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::{APPLET_TRAY, NO_INDEX};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_ATTENTION, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU,
    RFLAG_SUBMENU, ROW_ITEM, ROW_SEPARATOR, Rows,
};

/// The longest the whole read lasts.
const WINDOW: Duration = Duration::from_secs(10);
/// A gap with no frame in it that counts as the picture having settled.
const QUIET: Duration = Duration::from_millis(1500);

#[test]
#[ignore = "spawns the built binary against the live session bus"]
fn the_live_bus_publishes_items_with_their_menus() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-tray"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("caustics-tray spawns");

    // Witnesses on one thread, frames on another: exactly the two fds the bar
    // gives a sidecar, read exactly as the bar reads them.
    let stderr = BufReader::new(child.stderr.take().expect("stderr is piped"));
    let (says, witnesses) = mpsc::channel();
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            if says.send(line).is_err() {
                break;
            }
        }
    });

    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (sends, frames) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            match wire::read(&mut stdout) {
                Ok(Some(frame)) => {
                    if sends.send(Ok(frame)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    let _ = sends.send(Err(err.to_string()));
                    break;
                }
            }
        }
    });

    // Read until the picture settles — every item task has answered and
    // nothing has changed for `QUIET` — or the window runs out. Not "the first
    // frame with a menu in it": the whole point is what the real items and their menus come to, and the first of them arrives in milliseconds.
    let deadline = Instant::now() + WINDOW;
    let mut best: Option<Rows> = None;
    let mut seen = 0;
    // Every `Pixmap` key the child sent, in the order it sent them: a row may
    // name one only after it arrived (docs/abi.md, *Pixmap*).
    let mut pixmaps: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        let wait = QUIET.min(deadline.saturating_duration_since(Instant::now()));
        let Ok(frame) = frames.recv_timeout(wait) else {
            break;
        };
        let rows = match frame.expect("the stream is well formed") {
            Frame::Rows(rows) => rows,
            Frame::Pixmap(pixmap) => {
                assert!(
                    pixmap.key.starts_with("caustics-tray:"),
                    "a minted key: {}",
                    pixmap.key
                );
                assert!(
                    !pixmaps.contains(&pixmap.key),
                    "{} was sent twice",
                    pixmap.key
                );
                println!("pixmap {} ({}x{})", pixmap.key, pixmap.width, pixmap.height);
                pixmaps.push(pixmap.key);
                continue;
            }
            other => panic!(
                "a tray sidecar writes rows and pixmaps, not a {} frame",
                other.kind()
            ),
        };
        for i in 0..rows.rows.len() {
            if let Some(name) = rows.icon(i).filter(|n| n.starts_with("caustics-tray:")) {
                assert!(
                    pixmaps.iter().any(|k| k == name),
                    "row {i} names {name} before its texels"
                );
            }
        }
        seen += 1;
        assert_eq!(rows.applet, APPLET_TRAY);
        let items = rows.rows.iter().filter(|r| r.role == ROW_ITEM).count();
        // The body's size is the bar's other bound (`ROWS_ARENA_BYTES`, 64
        // KiB, a region of the rows arena); a frame is names and
        // labels, and the pixels go by `Pixmap`.
        println!(
            "frame {seen}: rev={} items={items} rows={} body={} B",
            rows.revision,
            rows.rows.len(),
            rows.body_len(),
        );
        best = Some(rows);
    }

    // Closing stdin is the bar going away (docs/abi.md, *Lifecycle*); the child owes us an exit 0.
    drop(child.stdin.take());
    let status = child.wait().expect("the child is waited on");

    println!("\n--- stderr ---");
    while let Ok(line) = witnesses.recv_timeout(Duration::from_millis(200)) {
        println!("{line}");
    }

    let rows = best.expect("at least one frame in the window");
    println!("\n--- the last frame ---");
    print_rows(&rows);
    println!("\nexit: {status}");

    assert!(status.success(), "stdin's EOF is a clean exit");
    let items: Vec<usize> = (0..rows.rows.len())
        .filter(|i| rows.rows[*i].role == ROW_ITEM)
        .collect();
    assert!(
        !items.is_empty(),
        "the session bus should have at least one tray item"
    );
    assert!(
        items
            .iter()
            .any(|i| rows.rows[*i].flags & RFLAG_HAS_MENU != 0),
        "at least one item should export a menu",
    );
    assert!(
        rows.rows.len() > items.len(),
        "an item with a menu should have published its rows",
    );
}

fn print_rows(rows: &Rows) {
    for (i, row) in rows.rows.iter().enumerate() {
        let depth = depth_of(rows, i);
        let label = rows.label(i).map(|l| l.into_owned()).unwrap_or_default();
        let icon = match rows.icon(i) {
            None => String::new(),
            Some(name) if name.starts_with("caustics-tray:") => format!("  [pixmap {name}]"),
            Some(name) => format!("  [icon {name}]"),
        };
        let flags = describe(row.flags);
        let what = if row.role == ROW_SEPARATOR {
            "----".to_owned()
        } else {
            label
        };
        println!("{:width$}{what}{icon}{flags}", "", width = depth * 2 + 2);
    }
}

fn depth_of(rows: &Rows, i: usize) -> usize {
    let mut depth = 0;
    let mut at = rows.rows[i].parent;
    while at != NO_INDEX {
        depth += 1;
        at = rows.rows[at as usize].parent;
    }
    depth
}

fn describe(flags: u32) -> String {
    let mut out = String::new();
    for (bit, name) in [
        (RFLAG_HAS_MENU, "has-menu"),
        (RFLAG_SUBMENU, "submenu"),
        (RFLAG_DISABLED, "disabled"),
        (RFLAG_CHECKABLE, "checkable"),
        (RFLAG_CHECKED, "checked"),
        (RFLAG_ATTENTION, "attention"),
    ] {
        if flags & bit != 0 {
            out.push_str(&format!(" {name}"));
        }
    }
    if out.is_empty() {
        out
    } else {
        format!("  ({})", out.trim())
    }
}
