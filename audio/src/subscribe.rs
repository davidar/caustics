//! The wake-up: one long-lived `pactl subscribe` child, whose lines say that
//! something about the server changed.
//!
//! It is a child rather than a library call for the reason `lib.rs` gives:
//! no build-time dependency on libpulse or libpipewire. The child
//! prints one unbuffered line per event — `Event 'change' on sink #52` — and
//! every line this module accepts means the same thing: *re-read*. Nothing is
//! parsed out of it beyond which kind of object moved, because `wpctl` is
//! where the facts come from.
//!
//! **Client events are not wake-ups.** Measured: five minutes of
//! normal use was 15 events, 14 of them `client` `new`/`remove` pairs and one
//! `sink-input` change — and every `wpctl get-volume` this sidecar runs is
//! itself one of those pairs. Waking on a client event would be the re-read
//! waking the re-read, forever. `sink-input`/`source-output` are out for a
//! milder reason: a stream appearing on a sink cannot change the sink's volume
//! or its mute, so the re-read would publish nothing.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;

use crate::witness;

/// The binary. On `PATH` wherever pipewire-pulse is (its `pactl` 17.0 tested).
pub const PACTL: &str = "pactl";

/// The backoff on a child that exited, doubling — the same 1 s → 30 s the bar
/// uses for its own children (docs/abi.md, *Lifecycle*).
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Which objects are worth a re-read. The object word is matched whole, so
/// `sink-input` is not a `sink`.
const WAKES: [&str; 3] = ["sink", "source", "server"];

/// Is this line of `pactl subscribe` a reason to re-read?
///
/// `Event 'change' on sink #52` → yes; `Event 'new' on client #2240` → no;
/// `Got SIGINT, exiting.`, which the child prints on its way out, → no.
pub fn is_wakeup(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix("Event ") else {
        return false;
    };
    // `'change' on sink #52`: the third word is the object.
    rest.split_whitespace()
        .nth(2)
        .is_some_and(|what| WAKES.contains(&what))
}

/// Run the child, forever, handing one `()` to the loop per interesting line.
///
/// Returns when the loop's receiver is gone, which is when the process is
/// leaving anyway.
pub async fn watch(wakeups: UnboundedSender<()>) {
    let mut backoff = BACKOFF_MIN;
    let mut said_absent = false;
    loop {
        let started = tokio::time::Instant::now();
        match run(&wakeups).await {
            Ok(status) => {
                witness(&format!("{PACTL} subscribe exited ({status}); respawning"));
                // The bar resets a child's backoff when the child delivered
                // something; for this one, having stayed up longer than the cap
                // is the same statement and needs no bookkeeping.
                if started.elapsed() >= BACKOFF_MAX {
                    backoff = BACKOFF_MIN;
                }
            }
            Err(Gone) => return,
            Err(NotStarted(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                // One line, not one every 30 s: a binary that is not installed
                // does not appear on its own, and the journal is the bar's.
                if !said_absent {
                    witness(&format!("absent: {PACTL}"));
                    said_absent = true;
                }
                backoff = BACKOFF_MAX;
            }
            Err(NotStarted(err)) => {
                witness(&format!("{PACTL} subscribe: {err}"));
                backoff = BACKOFF_MAX;
            }
        }
        if wakeups.is_closed() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Why a run of the child ended other than by exiting.
enum Ended {
    /// The loop is gone; so is the reason to be here.
    Gone,
    /// The child would not start.
    NotStarted(std::io::Error),
}

use Ended::{Gone, NotStarted};

/// One child, from spawn to exit.
async fn run(wakeups: &UnboundedSender<()>) -> Result<std::process::ExitStatus, Ended> {
    let mut child = Command::new(PACTL)
        .arg("subscribe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Its complaints are not ours to forward: a sound server that comes and
        // goes would otherwise write the bar's journal for it.
        .stderr(Stdio::null())
        // A sidecar never leaves a child behind: this kills it if the runtime
        // is dropped out from under us, and `finish` kills it on every other
        // path.
        .kill_on_drop(true)
        .spawn()
        .map_err(NotStarted)?;
    witness(&format!(
        "{PACTL} subscribe: pid={}",
        child.id().unwrap_or(0)
    ));
    // A fresh subscription has missed whatever happened while it was down.
    if wakeups.send(()).is_err() {
        return Err(finish(child).await);
    }
    let Some(stdout) = child.stdout.take() else {
        return Err(finish(child).await);
    };
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if is_wakeup(&line) && wakeups.send(()).is_err() {
                    return Err(finish(child).await);
                }
            }
            // EOF, or a read that failed: either way this child is done.
            Ok(None) => break,
            Err(err) => {
                witness(&format!("{PACTL} subscribe: {err}"));
                break;
            }
        }
    }
    match child.wait().await {
        Ok(status) => Ok(status),
        Err(err) => Err(NotStarted(err)),
    }
}

/// Kill the child and reap it, for the paths that are leaving rather than
/// respawning. Always [`Gone`]: the only caller is the one whose receiver went
/// away.
async fn finish(mut child: Child) -> Ended {
    let _ = child.start_kill();
    let _ = child.wait().await;
    Gone
}
