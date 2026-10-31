//! `caustics-displays` — the bar's displays sidecar.
//!
//! niri's outputs, outside the bar process: **one `WIRE_ROWS` frame** with
//! `applet = APPLET_DISPLAYS`, a chip whose menu lists every output with its
//! on/off, scale, mode and rotation, and actions back by row id on stdin
//! (docs/abi.md, *Rows*, *Action*) that become niri's `Output` requests over
//! `$NIRI_SOCKET`. Nothing here knows about Wayland, the GPU, or the bar's
//! internals, and nothing here is kept: a change lasts until niri restarts,
//! and the layout is niri's config file's.
//!
//! No async runtime: the stdin reader and the niri worker are threads, and
//! the loop waits on one channel both write to. Every niri round trip is
//! blocking socket I/O on the worker, never the loop's thread, so a niri that
//! does not answer never holds the menu.
//!
//! The split into a library and a two-line `main.rs` is so `displays/tests/`
//! can drive the frame, the table and the replies with fakes.

pub mod actions;
pub mod niri;
pub mod rows;

use std::ffi::OsString;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::ACTION_MENU_OPEN;
use caustics_bar_abi::wire::{self, ActionBody, Frame};

use actions::Decision;
use niri::{Change, Output};
use rows::Slots;

/// The usage text, on stderr — stdout carries frames and nothing else.
pub const USAGE: &str = "\
usage: caustics-displays

  No options. Reads niri's outputs over $NIRI_SOCKET and writes one WIRE_ROWS
  frame (APPLET_DISPLAYS) to stdout whenever they change, reads WIRE_ACTION
  frames from stdin and asks niri for each change, and exits when stdin
  closes. Every witness is one line on stderr.";

/// This sidecar's witness line: `caustics-displays: …` on stderr, in **one
/// write** (docs/abi.md, *Writing a sidecar*).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-displays");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// How often the outputs are re-read while the menu is open. niri's IPC has
/// no event for an output changing, so a monitor plugged in while the menu
/// is up shows within this; while the menu is closed nothing is read, and the
/// menu re-reads on opening.
pub const POLL: Duration = Duration::from_secs(2);

/// What the loop wakes up to.
#[derive(Debug)]
pub enum Msg {
    /// An action from the bar.
    Action(ActionBody),
    /// The bar closed stdin, or it broke.
    Closed,
    /// An `Outputs` reply, read.
    Outputs(Result<Vec<Output>, String>),
}

/// What the worker is asked to do, in order.
#[derive(Debug)]
pub enum Job {
    /// Read the outputs.
    Query,
    /// Ask for a change, witness niri's answer, then read the outputs.
    Change { output: String, change: Change },
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
    let (tx, rx) = channel();
    watch_stdin(tx.clone());
    let Some(socket) = std::env::var_os(niri::SOCKET_ENV) else {
        // A compositor that is not niri, or none: no chip, as a missing
        // sidecar has none, and the one thing left to do is leave with the bar.
        witness(&format!("no ${}; nothing to show", niri::SOCKET_ENV));
        loop {
            match rx.recv() {
                Ok(Msg::Action(_)) => {}
                Ok(Msg::Closed) | Err(_) => {
                    witness("stdin closed; exiting");
                    return 0;
                }
                Ok(Msg::Outputs(_)) => {}
            }
        }
    };
    let jobs = spawn_worker(socket, tx);
    serve(rx, jobs)
}

/// The loop: the picture, the menu's open state, and what each message does
/// to them.
pub fn serve(rx: Receiver<Msg>, jobs: Sender<Job>) -> i32 {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut outputs: Vec<Output> = Vec::new();
    let mut slots = Slots::default();
    let mut published: Option<Vec<Output>> = None;
    let mut revision = 0u32;
    let mut last_error: Option<String> = None;
    // `Some(next read)` while the menu is open.
    let mut open: Option<Instant> = None;

    // The first read, so the chip is on the bar before the menu is ever opened.
    let _ = jobs.send(Job::Query);
    loop {
        let msg = match open {
            None => rx.recv().unwrap_or(Msg::Closed),
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => {
                    open = Some(Instant::now() + POLL);
                    let _ = jobs.send(Job::Query);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => Msg::Closed,
            },
        };
        match msg {
            Msg::Closed => {
                witness("stdin closed; exiting");
                return 0;
            }
            Msg::Outputs(Ok(list)) => {
                last_error = None;
                outputs = list;
            }
            Msg::Outputs(Err(why)) => {
                // Said once per distinct failure; the picture stays as it was
                // last read, which is the honest thing a menu can show.
                if last_error.as_deref() != Some(why.as_str()) {
                    witness(&format!("outputs: {why}"));
                    last_error = Some(why);
                }
                continue;
            }
            Msg::Action(action) => {
                if actions::is_menu_notice(action.kind) {
                    if action.kind == ACTION_MENU_OPEN {
                        open = Some(Instant::now() + POLL);
                        let _ = jobs.send(Job::Query);
                    } else {
                        open = None;
                    }
                    continue;
                }
                match actions::decide(action.row, action.kind, &outputs, &slots) {
                    // Its outcome is the worker's one witness.
                    Decision::Change { output, change } => {
                        let _ = jobs.send(Job::Change { output, change });
                    }
                    Decision::LastOn { output } => witness(&format!(
                        "{output} off: refused, it is the only output on and the session would have no screen"
                    )),
                    Decision::Nothing => witness(&format!(
                        "action row={} kind={}: nothing to do",
                        action.row, action.kind
                    )),
                }
                continue;
            }
        }
        if published.as_ref() == Some(&outputs) {
            continue;
        }
        slots.assign(&outputs);
        revision += 1;
        let frame = rows::picture(revision, &outputs, &slots);
        if let Err(err) = wire::write(&mut out, &frame) {
            return write_failed(&err);
        }
        let extra = outputs.len().saturating_sub(rows::MAX_OUTPUTS);
        witness(&format!(
            "published rev={revision} outputs={}{}",
            rows::summary(&outputs),
            if extra > 0 {
                format!(" ({extra} past the menu's {})", rows::MAX_OUTPUTS)
            } else {
                String::new()
            }
        ));
        published = Some(outputs.clone());
    }
}

/// The worker: one thread, one request at a time, in the order asked. A
/// burst of reads waiting behind a slow one is one read.
fn spawn_worker(socket: OsString, tx: Sender<Msg>) -> Sender<Job> {
    let (jobs, inbox) = channel::<Job>();
    std::thread::Builder::new()
        .name("niri".into())
        .spawn(move || {
            while let Ok(job) = inbox.recv() {
                let mut query = false;
                let mut next = Some(job);
                while let Some(job) = next.take() {
                    match job {
                        Job::Query => query = true,
                        Job::Change { output, change } => {
                            change_output(&socket, &output, &change);
                            query = true;
                        }
                    }
                    next = inbox.try_recv().ok();
                }
                if query && tx.send(Msg::Outputs(read_outputs(&socket))).is_err() {
                    return;
                }
            }
        })
        .expect("spawning the niri worker");
    jobs
}

/// The `Outputs` request, sent and read.
pub fn read_outputs(socket: &std::ffi::OsStr) -> Result<Vec<Output>, String> {
    let reply = niri::round_trip(socket, niri::OUTPUTS).map_err(|e| e.to_string())?;
    niri::parse_outputs(&reply)
}

/// One `Output` request, and its outcome as one witness: `ok`, or niri's
/// words.
fn change_output(socket: &std::ffi::OsStr, output: &str, change: &Change) {
    let name = actions::change_name(change);
    let outcome = niri::round_trip(socket, &niri::request(output, change))
        .map_err(|e| e.to_string())
        .and_then(|reply| niri::outcome(&reply));
    match outcome {
        Ok(()) => witness(&format!("{output} {name}: ok")),
        Err(why) => witness(&format!("{output} {name}: {why}")),
    }
}

/// A failed write to stdout, witnessed, as the exit status. A broken pipe is
/// the bar gone, the ordinary end, 0; anything else is a failure, 1.
pub fn write_failed(err: &std::io::Error) -> i32 {
    if err.kind() == std::io::ErrorKind::BrokenPipe {
        witness("stdout closed; exiting");
        0
    } else {
        witness(&format!("stdout write failed: {err}; exiting"));
        1
    }
}

/// Read `Action` frames on a thread and hand them to the loop; EOF or a
/// broken stream is [`Msg::Closed`].
fn watch_stdin(tx: Sender<Msg>) {
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read(&mut input) {
                    Ok(None) => break,
                    Ok(Some(Frame::Action(action))) => {
                        if tx.send(Msg::Action(action)).is_err() {
                            return;
                        }
                    }
                    Ok(Some(other)) => {
                        witness(&format!("ignoring a frame of kind {}", other.kind()));
                    }
                    Err(err) => {
                        witness(&format!("stdin: {err}"));
                        break;
                    }
                }
            }
            let _ = tx.send(Msg::Closed);
        })
        .expect("spawning the stdin watcher");
}
