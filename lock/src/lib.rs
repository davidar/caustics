//! `caustics-lock` — the canvas's lock sidecar (docs/canvas.md, *Sinks*, *How
//! each job is wired*).
//!
//! It owns the two things a lock screen needs besides its surfaces: logind
//! (this session's `Lock` signal, and its `LockedHint`, [`logind`]) and the
//! PAM conversation ([`pam`]). The canvas host owns the lock surfaces and the
//! prompt drawn on them, and this process asks for both over the wire: a
//! `Lock` to lock and, after a conversation PAM said succeeded, to unlock;
//! `Prompt` frames for the conversation, whose `Answer`s come back on stdin
//! (docs/abi.md, *Lock*, *Prompt*, *Answer*). The host's
//! `CANVAS_EVENT_LOCKED` and `CANVAS_EVENT_UNLOCKED` say when the compositor
//! confirmed each. [`conductor`] is the state between them.
//!
//! The patterns are every sidecar's (docs/abi.md, *Writing a sidecar*): no
//! arguments, a `Hello` first, one witness a line on stderr, and an exit when
//! the host closes stdin.
//!
//! # Behaviour
//!
//! - **Locking**: logind's `Lock` (`loginctl lock-session`, an idle timer,
//!   the lid), or `lock` on the socket ([`socket`]), writes `LOCK_LOCK`. When
//!   the host says locked, `LockedHint` is set and a conversation begins.
//! - **On start**, a set `LockedHint` writes `LOCK_LOCK` before anything
//!   else: a restarted host re-locks before it paints (docs/canvas.md,
//!   *Failure model*).
//! - **A conversation** is one PAM transaction as this process's user, under
//!   a `PROMPT_INFO` title (the user's name). A failure is counted, shown as
//!   PAM's message, and followed at once by a fresh conversation; so is a
//!   dismissal (Escape). **Nothing unlocks but a success**: logind's
//!   `Unlock` signal is witnessed and ignored.
//! - **The fingerprint reader** is spoken to directly, on fprintd's D-Bus
//!   interface ([`fingerprint`]), while the conversation runs: PAM asks one
//!   thing at a time, so the reader is not in the PAM stack. It is held in
//!   windows while someone is at the lock (the lock going up, input on a
//!   lock surface, a resume) and let go otherwise ([`conductor::Windows`]),
//!   and never claimed while the machine is going to sleep. A match is a
//!   success; the password keeps working throughout; the reader is released
//!   when a window closes and when the lock is over. A claim polkit refuses
//!   ends its window only: the next trigger tries again.
//! - **Nothing typed is ever written anywhere but to PAM**, and no witness
//!   carries an answer. The host's stdin is read unbuffered, so no read
//!   buffer keeps a copy of one.
//!
//! # Settings
//!
//! From this process's own environment, never the host's (docs/architecture.md,
//! *Configuration is the caustic*):
//!
//! - `CAUSTICS_LOCK_PAM_SERVICE`: the PAM service, default `swaylock` (the
//!   service a session built on the sway tools has). The reader is spoken to
//!   directly, so the service should keep the fingerprint module off the
//!   unlock path, or PAM waits on the reader too.
//! - `CAUSTICS_LOCK_FINGERPRINT`: `0` is no reader; unset or anything else
//!   is the reader verifying while locked.
//! - `CAUSTICS_LOCK_FINGERPRINT_WINDOW`: the reader's windows, `idle,cap` in
//!   seconds, default `30,90`; a malformed value is one witness and the
//!   default.
//! - `CAUSTICS_LOCK_DRY_RUN_SECRET`: the **dry run**. PAM is not touched: a
//!   conversation is one echo-off ask, `Password: `, compared with these
//!   bytes. logind is still spoken to if a system bus answers, so a test
//!   points `DBUS_SYSTEM_BUS_ADDRESS` at nothing and the lock runs with the
//!   socket as its only trigger. The reader is spoken to on the same bus, so
//!   a test that runs a fake fprintd there runs it beside the dry run.
//!
//! # Exit codes
//!
//! - **0**: the host closed stdin. A refusal (no logind and no socket, so
//!   nothing could ever ask for a lock; no user to authenticate) is one
//!   witness and then a wait for that, so the host does not restart this
//!   process into the same answer every second.
//! - **1**: it could not run at all: no runtime, no stdin to read.

pub mod conductor;
pub mod fingerprint;
pub mod logind;
pub mod pam;
pub mod socket;

use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd as _;
use std::sync::Arc;

use caustics_bar_abi::wire::{self, Frame, Secret};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::conductor::{Conductor, Event, Sensor, Sink, Windows};
use crate::fingerprint::{Fprintd, Reader};
use crate::pam::{DryRun, LinuxPam, Pam};

/// What this sidecar calls itself.
pub const NAME: &str = "caustics-lock";

/// This sidecar's witness line: `caustics-lock: …` on stderr, one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new(NAME);

pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The PAM service when `CAUSTICS_LOCK_PAM_SERVICE` names none.
pub const DEFAULT_SERVICE: &str = "swaylock";

/// The title when the user database has no name for this uid.
pub const UNNAMED_TITLE: &str = "Locked";

/// The sidecar's settings, from its environment.
#[derive(Debug)]
pub struct Settings {
    /// `CAUSTICS_LOCK_PAM_SERVICE`, or [`DEFAULT_SERVICE`].
    pub service: String,
    /// `CAUSTICS_LOCK_DRY_RUN_SECRET`, when set and not empty.
    pub dry_run: Option<Secret>,
    /// `CAUSTICS_LOCK_FINGERPRINT` is not `0`.
    pub fingerprint: bool,
    /// `CAUSTICS_LOCK_FINGERPRINT_WINDOW`, or the default; `Err` holds a
    /// value that is not `idle,cap`, for the witness.
    pub windows: Result<Windows, String>,
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings::from_vars(
            std::env::var_os("CAUSTICS_LOCK_PAM_SERVICE"),
            std::env::var_os("CAUSTICS_LOCK_DRY_RUN_SECRET"),
            std::env::var_os("CAUSTICS_LOCK_FINGERPRINT"),
            std::env::var_os("CAUSTICS_LOCK_FINGERPRINT_WINDOW"),
        )
    }

    /// The rule, apart from the environment: an empty service is the
    /// default, an empty secret is no dry run (it would unlock on an empty
    /// answer), the reader is on unless its variable is `0`, and its windows
    /// are the default when unset or empty.
    pub fn from_vars(
        service: Option<OsString>,
        secret: Option<OsString>,
        fingerprint: Option<OsString>,
        windows: Option<OsString>,
    ) -> Settings {
        Settings {
            service: service
                .and_then(|s| s.into_string().ok())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_SERVICE.to_owned()),
            dry_run: secret
                .filter(|s| !s.is_empty())
                .map(|s| Secret::new(s.into_encoded_bytes())),
            fingerprint: fingerprint.is_none_or(|f| f != "0"),
            windows: match windows.filter(|w| !w.is_empty()) {
                None => Ok(Windows::default()),
                Some(text) => text
                    .to_str()
                    .and_then(Windows::parse)
                    .ok_or_else(|| text.to_string_lossy().into_owned()),
            },
        }
    }
}

/// The whole program. Returns the exit status.
pub fn run() -> i32 {
    let settings = Settings::from_env();
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
    runtime.block_on(async {
        let (events, inbox) = unbounded_channel();
        if let Err(e) = watch_stdin(events.clone()) {
            witness(&format!("could not read stdin: {e}"));
            return 1;
        }
        let sink: Sink = Box::new(|frame| wire::write(&mut io::stdout().lock(), frame));
        serve(settings, events, inbox, sink).await
    })
}

/// The host's frames, read on a thread of their own: an `Answer` and a
/// `CanvasEvent` go to the conductor, EOF is [`Event::Closed`]. stdin is read
/// through a duplicate of its descriptor, **unbuffered**: `std::io::Stdin`
/// keeps a buffer of what it read, and an answer must leave no copy anywhere
/// that is not zeroed (docs/abi.md, *Answer*).
fn watch_stdin(into: UnboundedSender<Event>) -> io::Result<()> {
    let mut input = std::fs::File::from(io::stdin().as_fd().try_clone_to_owned()?);
    std::thread::Builder::new()
        .name("stdin".to_owned())
        .spawn(move || {
            loop {
                let event = match wire::read(&mut input) {
                    Ok(Some(Frame::Answer(answer))) => Event::Answer(answer),
                    Ok(Some(Frame::CanvasEvent(event))) => Event::Canvas(event),
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

/// Wait for the host to leave, answering any client that asks: a refusal's
/// wait (the crate's note, *Exit codes*).
async fn idle(mut inbox: UnboundedReceiver<Event>) -> i32 {
    while let Some(event) = inbox.recv().await {
        match event {
            Event::Closed => break,
            Event::Command(_, reply) => {
                let _ = reply.send("error the lock refused to run; see its witness".to_owned());
            }
            _ => {}
        }
    }
    0
}

/// Everything after the runtime: logind, the socket, the conductor. The
/// tests' entry too, with their own sink.
pub async fn serve(
    settings: Settings,
    events: UnboundedSender<Event>,
    inbox: UnboundedReceiver<Event>,
    sink: Sink,
) -> i32 {
    let uid = caustics_sidecar::session::own_uid();
    let name = caustics_sidecar::session::user_name(uid);
    let pam: Arc<dyn Pam> = match settings.dry_run {
        Some(secret) => {
            witness(
                "dry run: PAM is not touched; one ask, compared with $CAUSTICS_LOCK_DRY_RUN_SECRET",
            );
            Arc::new(DryRun::new(secret))
        }
        None => {
            let Some(pam) = LinuxPam::new(&settings.service) else {
                witness("CAUSTICS_LOCK_PAM_SERVICE has a NUL in it; refusing");
                return idle(inbox).await;
            };
            if name.is_none() {
                witness(&format!(
                    "uid {uid} has no name in the user database, so PAM has nobody to \
                     authenticate; refusing"
                ));
                return idle(inbox).await;
            }
            witness(&format!("PAM service {}", settings.service));
            Arc::new(pam)
        }
    };
    let logind = logind::connect(&events).await;
    let socket = match socket::runtime_dir() {
        None => {
            witness("no XDG_RUNTIME_DIR; no socket");
            None
        }
        Some(dir) => {
            let path = dir.join(socket::SOCKET);
            match socket::bind(&path, socket::TAKEOVER_WINDOW) {
                Ok(listener) => match socket::serve(listener, events.clone()) {
                    Ok(()) => {
                        witness(&format!("listening on {}", path.display()));
                        Some(path)
                    }
                    Err(e) => {
                        witness(&format!("no thread for the socket: {e}"));
                        let _ = std::fs::remove_file(&path);
                        None
                    }
                },
                Err(e) => {
                    witness(&format!("{e}; no socket"));
                    None
                }
            }
        }
    };
    if logind.is_none() && socket.is_none() {
        witness("neither logind nor the socket can ask for a lock; refusing");
        return idle(inbox).await;
    }
    let user = name.clone().unwrap_or_default();
    let title = name.unwrap_or_else(|| UNNAMED_TITLE.to_owned());
    let (hint, locked) = match &logind {
        Some(logind) => (Some(logind.hint.clone()), logind.locked),
        None => (None, false),
    };
    let sensor = if settings.fingerprint {
        let windows = settings.windows.unwrap_or_else(|text| {
            let windows = Windows::default();
            witness(&format!(
                "fingerprint: CAUSTICS_LOCK_FINGERPRINT_WINDOW={text} is not idle,cap in \
                 seconds; {},{}",
                windows.idle.as_secs(),
                windows.cap.as_secs()
            ));
            windows
        });
        let reader: Box<dyn Reader> = Box::new(Fprintd::system());
        Some(Sensor { reader, windows })
    } else {
        witness("fingerprint: off (CAUSTICS_LOCK_FINGERPRINT=0)");
        None
    };
    let code = Conductor::new(pam, user, title, sink, events, hint, sensor)
        .run(inbox, locked)
        .await;
    if let Some(path) = socket {
        let _ = std::fs::remove_file(path);
    }
    if let Some(logind) = logind {
        logind.finish().await;
    }
    code
}

#[cfg(test)]
mod tests;
