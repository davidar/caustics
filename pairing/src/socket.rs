//! The socket, `$XDG_RUNTIME_DIR/caustics-pairing.sock`: one command a
//! line, one reply a line, as the lock's and the wallpaper's sockets are.
//!
//! ```text
//! pin <title>        → text <PIN>        | cancelled | error <why>
//! passkey <title>    → text <six digits> | cancelled | error <why>
//! cancel             → ok                 (BlueZ's Cancel reached the agent)
//! status             → ok asking=yes|no
//! ```
//!
//! The Bluetooth sidecar, BlueZ's agent, is the one client in earnest
//! (`bluetooth/src/sink.rs`): it asks here only for a PIN or a passkey that
//! must be typed. The title is the device's name and is shown as the
//! prompt's title; a NUL or a carriage return in it is refused, as is an
//! empty one. A `pin` or a `passkey` waits for the person; the reply is
//! written from a buffer zeroed when it drops.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use crate::conversation::{Command, Event, LEAVING, Question, line};

/// The socket's file name in `$XDG_RUNTIME_DIR`.
pub const SOCKET: &str = "caustics-pairing.sock";

/// The longest line the socket reads.
pub const MAX_LINE: usize = 256;

/// One line of the socket's language.
pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    if line.contains(['\0', '\r', '\n']) {
        return Err("a NUL or a line break inside the line".to_owned());
    }
    let (word, rest) = match line.trim_start().split_once(' ') {
        Some((word, rest)) => (word, rest.trim()),
        None => (line.trim(), ""),
    };
    let ask = |question| {
        if rest.is_empty() {
            Err(format!("{word} takes the device's name"))
        } else {
            Ok(Command::Ask {
                question,
                title: rest.to_owned(),
            })
        }
    };
    match word {
        "pin" => ask(Question::Pin),
        "passkey" => ask(Question::Passkey),
        "cancel" if rest.is_empty() => Ok(Command::Cancel),
        "status" if rest.is_empty() => Ok(Command::Status),
        "cancel" | "status" => Err(format!("{word} takes nothing")),
        "" => Err("an empty line".to_owned()),
        other => Err(format!(
            "{other:?} is not a command (pin, passkey, cancel, status)"
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
/// another process's: the lock's window, for the same reason. The sink of
/// a restarted set is still being torn down when its successor starts, and
/// its listener can answer a probe for some milliseconds after it is gone.
pub const TAKEOVER_WINDOW: Duration = Duration::from_secs(2);

/// The socket, bound: a stale one (nothing answers on it) is removed first.
/// One that answers is probed again across `window`, and one still answering
/// at its end is another sink's, which this one does not take.
pub fn bind(path: &Path, window: Duration) -> io::Result<UnixListener> {
    let started = Instant::now();
    loop {
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                if started.elapsed() >= window {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "{} is another pairing sink's (it answered for {} ms)",
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
pub fn serve(listener: UnixListener, into: Sender<Event>) -> io::Result<()> {
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

fn client(stream: UnixStream, into: Sender<Event>) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut lines = BufReader::new(stream.take(64 << 10));
    let mut text = String::new();
    loop {
        text.clear();
        match lines.read_line(&mut text) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if text.len() > MAX_LINE {
            let _ = out.write_all(b"error a line longer than the socket reads\n");
            return;
        }
        let reply = match parse(&text) {
            Ok(command) => {
                let (tx, rx) = mpsc::channel();
                if into.send(Event::Command(command, tx)).is_err() {
                    return;
                }
                rx.recv().unwrap_or_else(|_| line(LEAVING))
            }
            Err(why) => line(&format!("error {why}")),
        };
        if out.write_all(&reply).is_err() {
            return;
        }
    }
}

/// `caustics-pairing --ask <command…>`: the words as one line to a running
/// sink, and its reply on stdout. For a person trying the sink by hand and
/// for the nested gate; the Bluetooth sidecar has its own client.
pub fn ask(words: &[String]) -> io::Result<Vec<u8>> {
    let dir = runtime_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no XDG_RUNTIME_DIR, so no socket to ask",
        )
    })?;
    let path = dir.join(SOCKET);
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    stream.write_all(format!("{}\n", words.join(" ")).as_bytes())?;
    let mut reply = Vec::new();
    BufReader::new(stream).read_until(b'\n', &mut reply)?;
    Ok(reply)
}
