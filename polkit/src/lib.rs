//! `caustics-polkit` — the session's polkit authentication agent, as a sink
//! sidecar of the canvas (docs/canvas.md, *Sinks*).
//!
//! It registers with polkitd as the agent of this session, and each request
//! polkitd sends (`BeginAuthentication`) becomes one **conversation** with
//! the canvas host over the wire: a `Prompt` with the action's message as
//! its title, then one `Prompt` per line the helper writes, the host's
//! `Answer` written to the helper, and a `PROMPT_DONE` or a `PROMPT_CANCEL`
//! at the end (docs/abi.md, *Prompt*, *Answer*). PAM runs in
//! `polkit-agent-helper-1`, which tells polkitd the outcome itself; this
//! process learns only success or failure ([`helper`] has the line protocol
//! as it was verified).
//!
//! The patterns are every sidecar's (docs/abi.md, *Writing a sidecar*): no
//! arguments, a `Hello` first, one witness a line on stderr, and an exit
//! when the host closes stdin, after unregistering.
//!
//! # Behaviour
//!
//! - **One conversation at a time**; requests that arrive meanwhile queue in
//!   arrival order. A `CancelAuthentication` takes a queued one out with no
//!   frame (the host never saw it), and ends the one on screen: its helper
//!   is killed and a `PROMPT_CANCEL` goes to the host.
//! - **Identity**: the first `unix-user` identity whose uid is this
//!   process's, else the first `unix-user` offered (docs/canvas.md, *How
//!   each job is wired*). There is no picker.
//! - An `ANSWER_CANCELLED` (the user dismissed the prompt) kills the helper
//!   and ends the conversation as a failure; the method returns
//!   `org.freedesktop.PolicyKit1.Error.Cancelled`. Any other failure returns
//!   `….Error.Failed`. A wrong password is an error line and a fresh ask in
//!   the same conversation (the helper runs again with the same cookie), up
//!   to `conversation::MAX_ATTEMPTS`; only then does the request fail.
//! - **Nothing typed is ever written anywhere but the helper's stdin.** No
//!   witness carries an answer, a cookie, or a line from the helper; failed
//!   conversations are counted, never described. The host's stdin is read
//!   unbuffered, so no copy of an answer sits in a read buffer.
//! - polkitd restarting (its name changing hands) is answered by
//!   registering again.
//! - A locked session will hold requests in the queue until it unlocks; the
//!   lock does not exist yet, and `Conductor::start_next` is where it will
//!   be read.
//!
//! # Settings
//!
//! From this process's own environment, never the host's (docs/architecture.md,
//! *Configuration is the caustic*):
//!
//! - `CAUSTICS_POLKIT_HELPER`: the helper to spawn, default
//!   `/usr/lib/polkit-1/polkit-agent-helper-1`.
//! - `CAUSTICS_POLKIT_DRY_RUN=1`: the **dry run**. The system bus is not
//!   touched at all: nothing registers with polkitd, and the agent object is
//!   served on the *session* bus instead, taking a call from any
//!   connection, so a test can play polkitd and call `BeginAuthentication`
//!   itself. The helper is whatever `CAUSTICS_POLKIT_HELPER` names
//!   (`polkit/tests/fake-helper.sh` in the tests).
//!
//! # Exit codes
//!
//! - **0**: the host closed stdin. A refusal (no system bus, no session to
//!   speak for, another agent already registered for the session) is one
//!   witness and then a wait for that, so the host does not restart this
//!   process into the same answer every second.
//! - **1**: it could not run at all: no runtime, no stdin to read.

pub mod agent;
pub mod authority;
pub mod conversation;
pub mod helper;

use std::io;
use std::os::fd::AsFd as _;
use std::path::PathBuf;

use caustics_bar_abi::wire::{self, Frame};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zbus::Connection;
use zbus::fdo::DBusProxy;

use crate::agent::{AUTHORITY, Agent, PATH};
use crate::conversation::{Conductor, Event, Sink};

/// What this sidecar calls itself.
pub const NAME: &str = "caustics-polkit";

/// This sidecar's witness line: `caustics-polkit: …` on stderr, one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new(NAME);

pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The sidecar's settings, from its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `CAUSTICS_POLKIT_HELPER`, or [`helper::DEFAULT_PATH`].
    pub helper: PathBuf,
    /// `CAUSTICS_POLKIT_DRY_RUN=1`.
    pub dry_run: bool,
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings::from_vars(
            std::env::var_os("CAUSTICS_POLKIT_HELPER"),
            std::env::var_os("CAUSTICS_POLKIT_DRY_RUN"),
        )
    }

    /// The rule, apart from the environment: an empty helper is the default,
    /// and only `1` is a dry run.
    pub fn from_vars(
        helper: Option<std::ffi::OsString>,
        dry_run: Option<std::ffi::OsString>,
    ) -> Settings {
        Settings {
            helper: helper
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(helper::DEFAULT_PATH)),
            dry_run: dry_run.is_some_and(|v| v == "1"),
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
        if settings.dry_run {
            dry_run(settings, events, inbox, sink).await
        } else {
            earnest(settings, events, inbox, sink).await
        }
    })
}

/// The host's frames, read on a thread of their own: an `Answer` goes to the
/// conductor, EOF is [`Event::Closed`]. stdin is read through a duplicate of
/// its descriptor, **unbuffered**: `std::io::Stdin` keeps a buffer of what it
/// read, and an answer must leave no copy anywhere that is not zeroed
/// (docs/abi.md, *Answer*).
fn watch_stdin(into: UnboundedSender<Event>) -> io::Result<()> {
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

/// Handle events doing nothing but wait for the host to leave: a refusal's
/// wait (the crate's note, *Exit codes*).
async fn idle(mut inbox: UnboundedReceiver<Event>) -> i32 {
    while let Some(event) = inbox.recv().await {
        match event {
            Event::Closed => break,
            Event::Begin(begin) => {
                let _ = begin.reply.send(conversation::Outcome::Failed);
            }
            _ => {}
        }
    }
    0
}

/// Serve the agent object on `conn` and run `conductor` until the host
/// leaves: the dry run's whole program, and the tests'. `events` is the
/// conductor's inbox's sender, which the object sends requests on.
pub async fn serve(
    conn: &Connection,
    check_caller: bool,
    events: UnboundedSender<Event>,
    conductor: Conductor,
    inbox: UnboundedReceiver<Event>,
) -> i32 {
    let agent = Agent {
        events,
        check_caller,
    };
    if let Err(e) = conn.object_server().at(PATH, agent).await {
        witness(&format!("serving {PATH}: {e}"));
        return idle(inbox).await;
    }
    conductor.run(inbox).await
}

async fn dry_run(
    settings: Settings,
    events: UnboundedSender<Event>,
    inbox: UnboundedReceiver<Event>,
    sink: Sink,
) -> i32 {
    let conn = match Connection::session().await {
        Ok(conn) => conn,
        Err(e) => {
            witness("absent: session-bus");
            witness(&format!("session-bus: {e}"));
            return idle(inbox).await;
        }
    };
    witness(&format!(
        "dry run: the agent is at {} {PATH} on the session bus; the system bus is not touched; \
         the helper is {}",
        conn.unique_name().map_or("?", |n| n.as_str()),
        settings.helper.display()
    ));
    let conductor = Conductor::new(settings.helper, sink, events.clone());
    serve(&conn, false, events, conductor, inbox).await
}

async fn earnest(
    settings: Settings,
    events: UnboundedSender<Event>,
    inbox: UnboundedReceiver<Event>,
    sink: Sink,
) -> i32 {
    let conn = match Connection::system().await {
        Ok(conn) => conn,
        Err(e) => {
            witness("absent: system-bus");
            witness(&format!("system-bus: {e}"));
            return idle(inbox).await;
        }
    };
    let agent = Agent {
        events: events.clone(),
        check_caller: true,
    };
    if let Err(e) = conn.object_server().at(PATH, agent).await {
        witness(&format!("serving {PATH}: {e}"));
        return idle(inbox).await;
    }
    let (session, how) = match authority::session_id(&conn).await {
        Ok(found) => found,
        Err(e) => {
            witness(&format!("no session to be the agent of: {e}"));
            return idle(inbox).await;
        }
    };
    let locale = authority::locale();
    if let Err(e) = authority::register(&conn, &session, &locale).await {
        witness(&format!(
            "polkitd refused the registration for session {session}: {e}"
        ));
        return idle(inbox).await;
    }
    witness(&format!(
        "the authentication agent of session {session} ({how}), locale {locale}; the helper is {}",
        settings.helper.display()
    ));
    let _again = reregister(&conn, session.clone(), locale);
    let code = Conductor::new(settings.helper, sink, events)
        .run(inbox)
        .await;
    match authority::unregister(&conn, &session).await {
        Ok(()) => witness(&format!("unregistered from session {session}")),
        Err(e) => witness(&format!("unregistering: {e}")),
    }
    code
}

/// Register again whenever polkitd's name gets a new owner: a restarted
/// polkitd has forgotten every agent. The stream is forwarded the moment it
/// exists (docs/architecture.md, *Rules*), and a task of its own makes the
/// call, so the conductor never waits on it.
fn reregister(conn: &Connection, session: String, locale: String) -> tokio::task::JoinHandle<()> {
    let conn = conn.clone();
    tokio::spawn(async move {
        let Ok(dbus) = DBusProxy::new(&conn).await else {
            witness("org.freedesktop.DBus: no proxy; polkitd restarting will not be noticed");
            return;
        };
        let stream = match dbus
            .receive_name_owner_changed_with_args(&[(0, AUTHORITY)])
            .await
        {
            Ok(stream) => stream,
            Err(e) => {
                witness(&format!("watching {AUTHORITY}: {e}"));
                return;
            }
        };
        let (tx, mut owners) = unbounded_channel();
        let _drain = caustics_sidecar::forward(stream, tx, |signal| {
            signal
                .args()
                .ok()
                .and_then(|args| args.new_owner().as_ref().map(|o| o.to_string()))
        });
        while let Some(owner) = owners.recv().await {
            match authority::register(&conn, &session, &locale).await {
                Ok(()) => witness(&format!(
                    "polkitd is {owner} now; registered again for session {session}"
                )),
                Err(e) => witness(&format!("polkitd is {owner} now; registering again: {e}")),
            }
        }
    })
}
