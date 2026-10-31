//! `caustics-pairing` — the canvas's pairing sink: a PIN or a passkey that
//! must be typed while a Bluetooth device pairs, asked on the canvas's
//! prompt (docs/canvas.md, *Sinks*).
//!
//! **It is not BlueZ's agent.** The bar's Bluetooth sidecar is
//! (`bluetooth/src/agent.rs`): it asks a yes or a no
//! (compare a passkey, allow a pairing or a service) on the bar's own rows,
//! shows a code to type on the other device as a toast, and only a code
//! that must be typed *here* comes to this sink, over its socket
//! ([`socket`]). Each becomes one **conversation** with the canvas host over
//! the wire: a `Prompt` with the device's name as its title and the ask, the
//! host's `Answer`, and a `PROMPT_DONE` or a `PROMPT_CANCEL` at the end
//! (docs/abi.md, *Prompt*, *Answer*). The answer goes back over the socket
//! as the reply line and nowhere else.
//!
//! The patterns are every sink's (docs/abi.md, *Writing a sidecar*): no
//! arguments, a `Hello` first, one witness a line on stderr, and an exit
//! when the host closes stdin.
//!
//! # Behaviour
//!
//! - **One conversation at a time**; a second question while one is asked
//!   is answered `error busy` ([`conversation`] has the life of each).
//! - `cancel` on the socket (BlueZ withdrew the question) ends the
//!   conversation on screen with a `PROMPT_CANCEL`, and its asker hears
//!   `cancelled`.
//! - An `ANSWER_CANCELLED` (the person dismissed the prompt) is `cancelled`.
//! - **No PIN or passkey is ever written anywhere but the reply line**, and
//!   none is in a witness. The reply is built once in a buffer zeroed when
//!   it drops, and the host's stdin is read unbuffered, so no copy of an
//!   answer sits in a read buffer.
//!
//! # Exit codes
//!
//! - **0**: the host closed stdin. A refusal (no runtime directory, a socket
//!   another sink holds) is one witness and then a wait for that, so the
//!   host does not restart this process into the same answer every second.
//! - **1**: it could not run at all: no stdin to read.
//!
//! `caustics-pairing --ask <command…>` is a client instead: the words as
//! one line to a running sink's socket and its reply on stdout, for a
//! person trying it by hand and for the nested gate.

pub mod ask;
pub mod conversation;
pub mod socket;

use std::io::{self, Write as _};
use std::os::fd::AsFd as _;
use std::sync::mpsc::{Receiver, Sender, channel};

use caustics_bar_abi::wire::{self, Frame};

use crate::conversation::{Conductor, Event, Sink};

/// What this sidecar calls itself.
pub const NAME: &str = "caustics-pairing";

/// This sidecar's witness line: `caustics-pairing: …` on stderr, one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new(NAME);

pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The whole program. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    for arg in args {
        witness(&format!("ignoring the argument {arg:?}"));
    }
    let (events, inbox) = channel();
    if let Err(e) = watch_stdin(events.clone()) {
        witness(&format!("could not read stdin: {e}"));
        return 1;
    }
    let Some(dir) = socket::runtime_dir() else {
        witness("absent: runtime-dir");
        witness("no XDG_RUNTIME_DIR, so no socket for the Bluetooth sidecar to ask on");
        return idle(inbox);
    };
    let path = dir.join(socket::SOCKET);
    let listener = match socket::bind(&path, socket::TAKEOVER_WINDOW) {
        Ok(listener) => listener,
        Err(e) => {
            witness(&format!("{}: {e}", path.display()));
            return idle(inbox);
        }
    };
    if let Err(e) = socket::serve(listener, events) {
        witness(&format!("serving {}: {e}", path.display()));
        return idle(inbox);
    }
    witness(&format!(
        "serving {}; asked only for a PIN or a passkey to type",
        path.display()
    ));
    let sink: Sink = Box::new(|frame| wire::write(&mut io::stdout().lock(), frame));
    let code = Conductor::new(sink).run(inbox);
    // The socket file goes with the sink, so the Bluetooth sidecar's next
    // look finds nothing rather than a name nobody answers on.
    let _ = std::fs::remove_file(&path);
    code
}

/// `--ask <words…>`: one line to the socket, the reply on stdout. 0 when a
/// reply came, 1 when nothing answered.
pub fn ask_sink(words: &[String]) -> i32 {
    match socket::ask(words) {
        Ok(reply) => {
            let mut out = io::stdout().lock();
            let _ = out.write_all(&reply);
            let _ = out.flush();
            0
        }
        Err(e) => {
            witness(&format!("--ask: {e}"));
            1
        }
    }
}

/// The host's frames, read on a thread of their own: an `Answer` goes to the
/// conductor, EOF is [`Event::Closed`]. stdin is read through a duplicate of
/// its descriptor, **unbuffered**: `std::io::Stdin` keeps a buffer of what it
/// read, and an answer must leave no copy anywhere that is not zeroed
/// (docs/abi.md, *Answer*).
fn watch_stdin(into: Sender<Event>) -> io::Result<()> {
    let mut input = std::fs::File::from(io::stdin().as_fd().try_clone_to_owned()?);
    std::thread::Builder::new()
        .name("stdin".to_owned())
        .spawn(move || {
            loop {
                let event = match wire::read(&mut input) {
                    Ok(Some(Frame::Answer(answer))) => Event::Answer(answer),
                    Ok(Some(other)) => {
                        witness(&format!(
                            "stdin: frame kind {} has no meaning here",
                            other.kind()
                        ));
                        continue;
                    }
                    Ok(None) | Err(_) => Event::Closed,
                };
                let closed = matches!(event, Event::Closed);
                if into.send(event).is_err() || closed {
                    return;
                }
            }
        })?;
    Ok(())
}

/// Wait for the host to leave, doing nothing else: a refusal's wait (the
/// crate's note, *Exit codes*).
fn idle(inbox: Receiver<Event>) -> i32 {
    while let Ok(event) = inbox.recv() {
        if matches!(event, Event::Closed) {
            break;
        }
    }
    0
}
