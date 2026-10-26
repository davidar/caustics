//! The control socket, `$XDG_RUNTIME_DIR/caustics-lock.sock`: one command a
//! line, one reply a line, as the wallpaper's socket is.
//!
//! ```text
//! lock          → ok          the lock is asked for (or already held)
//! lock wait     → locked      once the host has said CANVAS_EVENT_LOCKED;
//!                 error …     if the host goes first
//! status        → ok locked=yes|no attempts=N
//! ```
//!
//! `lock wait` is the before-sleep hold (docs/canvas.md, *How each job is
//! wired*): a sleep hook runs `caustics-canvasctl lock --wait` and the machine
//! goes down only once the screen is locked. `attempts` counts the failed
//! conversations of the lock now held, or of the last one: a count, never a
//! description.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;

use crate::conductor::Event;

/// The socket's file name in `$XDG_RUNTIME_DIR`.
pub const SOCKET: &str = "caustics-lock.sock";

/// The longest line the socket reads.
pub const MAX_LINE: usize = 256;

/// The reply to a `lock wait` once the session is locked.
pub const LOCKED: &str = "locked";

/// A command a client sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `lock`, or with `wait` `lock wait`.
    Lock { wait: bool },
    /// `status`.
    Status,
}

/// One line of the socket's language.
pub fn parse(line: &str) -> Result<Command, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["lock"] => Ok(Command::Lock { wait: false }),
        ["lock", "wait"] => Ok(Command::Lock { wait: true }),
        ["status"] => Ok(Command::Status),
        [] => Err("an empty line".to_owned()),
        ["lock", ..] => Err("lock takes nothing but wait".to_owned()),
        ["status", ..] => Err("status takes nothing".to_owned()),
        [other, ..] => Err(format!(
            "{other:?} is not a command (lock, lock wait, status)"
        )),
    }
}

/// The session's runtime directory, or `None` without one.
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// How long a socket that answers is given to stop answering before it is
/// another process's. The previous lock of a restarted set is still being
/// torn down when its successor starts, and its listener can answer a probe
/// for some milliseconds after the unit has reported it gone; read as
/// "another lock's" at that instant, the new lock would run without a socket
/// until the next restart (the wallpaper's socket has the same window).
pub const TAKEOVER_WINDOW: Duration = Duration::from_secs(2);

/// The socket, bound: a stale one (nothing answers on it) is removed first.
/// One that answers is probed again across `window`, and one still answering
/// at its end is another lock's, which this one does not take.
pub fn bind(path: &Path, window: Duration) -> io::Result<UnixListener> {
    let started = Instant::now();
    loop {
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                if started.elapsed() >= window {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "{} is another lock's (it answered for {} ms)",
                            path.display(),
                            window.as_millis()
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            match std::fs::remove_file(path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        return UnixListener::bind(path);
    }
}

/// Accept connections on a thread of their own, one thread a client, every
/// command to the conductor with a channel for its reply.
pub fn serve(listener: UnixListener, into: UnboundedSender<Event>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("socket".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let into = into.clone();
                let _ = std::thread::Builder::new()
                    .name("client".to_owned())
                    .spawn(move || client(stream, into));
            }
        })
        .map(drop)
}

fn client(stream: UnixStream, into: UnboundedSender<Event>) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut lines = BufReader::new(stream.take(64 << 10));
    let mut line = String::new();
    loop {
        line.clear();
        match lines.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.len() > MAX_LINE {
            let _ = out.write_all(b"error a line longer than the socket reads\n");
            return;
        }
        let reply = match parse(&line) {
            Ok(command) => {
                let (tx, rx) = mpsc::channel();
                if into.send(Event::Command(command, tx)).is_err() {
                    return;
                }
                rx.recv()
                    .unwrap_or_else(|_| "error the lock is leaving".to_owned())
            }
            Err(why) => format!("error {why}"),
        };
        if out.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
