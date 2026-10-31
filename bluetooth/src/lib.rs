//! `caustics-bluetooth` — the bar's bluetooth sidecar.
//!
//! BlueZ's adapter and its paired devices as **one `WIRE_ROWS` frame** with
//! `applet = APPLET_BLUETOOTH`: a chip, an on/off row, a row per paired device,
//! and while the menu is open a row per device in range that is not paired
//! yet, with actions back by row id on stdin (docs/abi.md, *Rows*,
//! *Action*). Nothing here knows about Wayland, the GPU, or the bar's
//! internals.
//!
//! **Pairing** is a click on a device in range: `Pair()`, and BlueZ asks the
//! session's pairing agent whatever the pairing needs. While the menu is
//! open the adapter is also discoverable, so the other device can begin the
//! pairing instead, and the same agent is asked. That agent is
//! `caustics-pairing`, a sink of the canvas (pairing/README.md), so a passkey
//! to confirm or a PIN to type is asked on the canvas's prompt and never
//! reaches this process or the bar.
//!
//! Why a separate process is the bar's membership test (docs/architecture.md,
//! *Sidecars*): this owns no Wayland object and none of the bar's fds — it owns
//! one system-bus connection and a menu — and the bar copies its bytes without
//! parsing them.
//!
//! The split into a library and a two-line `main.rs` is so `bluetooth/tests/`
//! can drive the frame and the dispatch from synthesised bus bytes; no test ever
//! connects, disconnects, pairs, discovers, or touches `Powered`.

pub mod actions;
pub mod bluez;
pub mod rows;

use caustics_bar_abi::wire::{self, ActionBody, Frame};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use zbus::Connection;

use bluez::State;

/// The usage text, on stderr — stdout carries frames and nothing else. The bar
/// spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*), so this is for a
/// human running it by hand.
pub const USAGE: &str = "\
usage: caustics-bluetooth

  No options. Writes one WIRE_ROWS frame (APPLET_BLUETOOTH) to stdout whenever
  the picture changes, reads WIRE_ACTION frames from stdin, and exits when stdin
  closes. Every witness is one line on stderr.";

/// This sidecar's witness line: `caustics-bluetooth: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-bluetooth");

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

    // Current-thread: one adapter's worth of D-Bus traffic does not want a
    // thread pool, and a single thread keeps the ordering obvious.
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

    let conn = match Connection::system().await {
        Ok(conn) => Some(conn),
        Err(e) => {
            witness("absent: system-bus");
            witness(&format!("system-bus: {e}"));
            None
        }
    };

    let (state_tx, mut states) = unbounded_channel();
    match conn.as_ref() {
        Some(conn) => {
            tokio::spawn(bluez::watch(conn.clone(), state_tx));
        }
        // No bus at all is also no BlueZ; there is nothing to watch for, so the
        // process stays up for its stdin and publishes the empty frame below.
        None => witness("absent: bluez"),
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut revision = 1u32;
    let mut state = State::default();
    if !publish(&mut out, revision, &state) {
        return 0;
    }
    // Which adapters are discovering for this menu (`actions::Discovery`).
    let mut discovery = actions::Discovery::default();

    loop {
        tokio::select! {
            // `Some(..)`: the watcher's end closing simply disables this arm.
            Some(next) = states.recv() => {
                // Publish on change only: BlueZ emits a signal for plenty that
                // does not move this picture.
                if next == state {
                    continue;
                }
                state = next;
                // An adapter powered while the menu is open looks too.
                for op in discovery.changed(&state) {
                    actions::perform(conn.as_ref(), op).await;
                }
                revision += 1;
                if !publish(&mut out, revision, &state) {
                    leave(conn.as_ref(), &mut discovery).await;
                    return 0;
                }
            }
            action = incoming.recv() => match action {
                None => {
                    witness("stdin closed; exiting");
                    leave(conn.as_ref(), &mut discovery).await;
                    return 0;
                }
                // The bar's two menu notices: discovery while it is open.
                Some(action) if actions::is_menu_notice(action.kind) => {
                    for op in discovery.notice(action.kind, &state).unwrap_or_default() {
                        actions::perform(conn.as_ref(), op).await;
                    }
                }
                Some(action) => match actions::op_for(&state, action.row, action.kind) {
                    Some(op) => actions::perform(conn.as_ref(), op).await,
                    None => witness(&format!(
                        "action row={} kind={}: nothing to do",
                        action.row, action.kind
                    )),
                },
            },
        }
    }
}

/// Stop the discovery this sidecar started before it goes. BlueZ would stop
/// it anyway when this connection closes; asking says so in the journal.
async fn leave(conn: Option<&Connection>, discovery: &mut actions::Discovery) {
    for op in discovery.close() {
        actions::perform(conn, op).await;
    }
}

/// Write the whole picture. `false` means stdout is gone and so are we.
fn publish(out: &mut impl std::io::Write, revision: u32, state: &State) -> bool {
    let frame = rows::picture(revision, state);
    if let Err(err) = wire::write(out, &frame) {
        if err.kind() == std::io::ErrorKind::BrokenPipe {
            witness("stdout closed; exiting");
        } else {
            witness(&format!("stdout write failed: {err}"));
        }
        return false;
    }
    match state.adapter.as_ref() {
        Some(adapter) => witness(&format!(
            "published rev={revision} powered={} devices={} connected={} in-range={}",
            adapter.powered,
            state.devices.len(),
            state.connected(),
            state.nearby.len()
        )),
        None => witness(&format!("published rev={revision} no adapter")),
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
