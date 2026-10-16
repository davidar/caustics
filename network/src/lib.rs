//! `caustics-network` — the bar's network sidecar.
//!
//! NetworkManager's Wi-Fi state as **one `WIRE_ROWS` frame** with
//! `applet = APPLET_NETWORK`: one bar item whose icon is the default route, a
//! Wi-Fi switch, one radio row per known Wi-Fi profile, the access points in
//! range that no profile claims, and the settings row when there is a
//! settings program — with actions back by row id on stdin (docs/abi.md
//! *Rows*, *Action*). Nothing here knows about Wayland, the GPU, or the bar's
//! internals.
//!
//! **The radio is only asked while the menu is open.** NM ages its idle scan
//! list down to the associated access point, so the networks in range are only
//! known if something asked recently — and a scan takes the radio off-channel,
//! which during a video call is not something to do in the background. So the
//! bar's `ACTION_MENU_OPEN` starts it and `ACTION_MENU_CLOSE` stops it, with
//! one re-scan every [`SCAN_EVERY`] in between. Nothing scans at startup.
//!
//! Why a separate process is the bar's membership test (docs/architecture.md,
//! *Sidecars*): this owns no Wayland object and none of the bar's fds — it owns
//! one system-bus connection and a menu — so the bar's own sockets stay
//! Wayland's and the compositor's.
//!
//! **There is no password entry here, ever.** A network NM does not already
//! know is not a row the bar can join; joining a new one is the settings
//! program's job, and its row opens it.
//!
//! The split into a library and a two-line `main.rs` is so `network/tests/` can
//! drive the frame and the dispatch from synthesised bus bytes; no test ever
//! activates, deactivates, scans or switches a radio.

pub mod actions;
pub mod bus;
pub mod model;
pub mod nm;
pub mod rows;

use std::time::Duration;

use caustics_bar_abi::wire::{self, ActionBody, Frame, Rows};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::Instant;
use zbus::Connection;

use model::Snapshot;

/// How often to re-scan while the menu is open — GNOME's own cadence, and slow
/// enough that a menu somebody left up is not a radio held off-channel.
pub const SCAN_EVERY: Duration = Duration::from_secs(15);

/// The usage text, on stderr — stdout carries frames and nothing else. The bar
/// spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*), so this is for a
/// human running it by hand.
pub const USAGE: &str = "\
usage: caustics-network

  No options. Writes one WIRE_ROWS frame (APPLET_NETWORK) to stdout whenever the
  picture changes, reads WIRE_ACTION frames from stdin, and exits when stdin
  closes. Every witness is one line on stderr.";

/// This sidecar's witness line: `caustics-network: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-network");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The whole program. Returns the process's exit status.
pub fn run<I: IntoIterator<Item = String>>(argv: I) -> i32 {
    for arg in argv {
        match arg.as_str() {
            "-h" | "--help" => {
                witness(USAGE);
                return 0;
            }
            other => witness(&format!("ignoring the argument {other:?}")),
        }
    }

    // Current-thread: one bus connection and one menu's worth of traffic does
    // not want a thread pool, and a single thread keeps the ordering obvious.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build();
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(e) => {
            witness(&format!("no tokio runtime ({e})"));
            return 1;
        }
    };
    runtime.block_on(serve())
}

async fn serve() -> i32 {
    // The bar holds the write end of stdin, so its EOF is this process's notice
    // to leave — the one thing every sidecar must read.
    let (act_tx, mut incoming) = unbounded_channel();
    watch_stdin(act_tx);

    // Read once at startup: an environment that changes under a running process
    // must not change whether a click joins a network.
    let dry = actions::dry_run(std::env::var(actions::DRY_RUN_ENV).ok().as_deref());
    if dry {
        witness(&format!(
            "{}=1: every action is a witness and nothing else",
            actions::DRY_RUN_ENV
        ));
    }

    let conn = match Connection::system().await {
        Ok(conn) => Some(conn),
        Err(e) => {
            witness("absent: system-bus");
            witness(&format!("system-bus: {e}"));
            None
        }
    };

    let (snap_tx, mut snapshots) = unbounded_channel();
    match conn.as_ref() {
        Some(conn) => {
            tokio::spawn(bus::watch(conn.clone(), snap_tx));
        }
        // No bus at all is also no NetworkManager: the empty frame and this
        // one line.
        None => witness("absent: networkmanager"),
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut revision = 1u32;
    let mut state = Snapshot::default();
    // The empty picture first: no rows until something is known, so the chip is
    // absent rather than wrong, and the bar's health line still says
    // `running`.
    let mut published = rows::empty(revision);
    if !publish(&mut out, &published, &state) {
        return 0;
    }

    // When the next scan is due, or `None` while the menu is closed — which is
    // also how the select arm below is switched off.
    let mut next_scan: Option<Instant> = None;

    loop {
        tokio::select! {
            // Armed only while the menu is open: `pending()` never completes, so
            // with no deadline this arm simply is not there.
            () = async {
                match next_scan {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                next_scan = Some(Instant::now() + SCAN_EVERY);
                actions::perform(
                    conn.as_ref(),
                    actions::Op::Scan { devices: state.wifi.clone() },
                    dry,
                )
                .await;
            }
            // `Some(..)`: the watcher's end closing simply disables this arm.
            Some(next) = snapshots.recv() => {
                // `None` is "no NetworkManager": the empty frame, and the
                // default snapshot so an action that arrives anyway decides
                // nothing.
                let candidate = match &next {
                    Some(state) => {
                        rows::picture(revision + 1, state, actions::settings_available())
                    }
                    None => rows::empty(revision + 1),
                };
                state = next.unwrap_or_default();
                // Publish on change only: the bar's rebuild-vs-upload
                // distinction, and NM's six-second `Strength` signal, both
                // depend on this.
                if same(&candidate, &published) {
                    continue;
                }
                revision += 1;
                published = candidate;
                if !publish(&mut out, &published, &state) {
                    return 0;
                }
            }
            action = incoming.recv() => match action {
                None => {
                    witness("stdin closed; exiting");
                    return 0;
                }
                Some(action) => match actions::op_for(&state, action.row, action.kind) {
                    Some(op) => {
                        // The scan cadence is the loop's own — the timer lives
                        // here — so these two ops arm and disarm it on the way
                        // past. `perform` still does the call itself.
                        match &op {
                            actions::Op::Scan { .. } => {
                                next_scan = Some(Instant::now() + SCAN_EVERY);
                            }
                            actions::Op::ScanStop => next_scan = None,
                            _ => {}
                        }
                        actions::perform(conn.as_ref(), op, dry).await;
                    }
                    None => witness(&format!(
                        "action row={} kind={}: nothing to do",
                        action.row, action.kind
                    )),
                },
            },
        }
    }
}

/// Two pictures that differ only in their revision are the same picture.
fn same(a: &Rows, b: &Rows) -> bool {
    a.applet == b.applet && a.rows == b.rows && a.bytes == b.bytes
}

/// Write the whole picture. `false` means stdout is gone and so are we.
fn publish(out: &mut impl std::io::Write, rows: &Rows, state: &Snapshot) -> bool {
    if let Err(err) = wire::write(out, &Frame::Rows(rows.clone())) {
        if err.kind() == std::io::ErrorKind::BrokenPipe {
            witness("stdout closed; exiting");
        } else {
            witness(&format!("stdout write failed: {err}"));
        }
        return false;
    }
    // One or two facts that describe the picture, per publish
    // for a reader of the journal.
    if rows.rows.is_empty() {
        witness(&format!("published rev={} no rows", rows.revision));
    } else {
        witness(&format!(
            "published rev={} icon={} networks={} in-range={}",
            rows.revision,
            state.icon(),
            state.networks.len(),
            state.visible.len()
        ));
    }
    true
}

/// Read `Action` frames on a thread and hand them to the loop. Dropping the
/// sender at EOF is what tells the loop the bar is gone.
fn watch_stdin(actions: UnboundedSender<ActionBody>) {
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read(&mut input) {
                    Ok(None) => return,
                    Ok(Some(Frame::Action(action))) => {
                        if actions.send(action).is_err() {
                            return;
                        }
                    }
                    Ok(Some(other)) => {
                        witness(&format!("ignoring a frame of kind {}", other.kind()));
                    }
                    Err(err) => {
                        witness(&format!("stdin: {err}"));
                        return;
                    }
                }
            }
        })
        .expect("spawning the stdin watcher");
}
