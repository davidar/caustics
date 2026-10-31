//! `caustics-secrets` — the session's NetworkManager secret agent, as a sink
//! sidecar of the canvas (docs/canvas.md, *Sinks*).
//!
//! It registers with NetworkManager's agent manager, and each request
//! NetworkManager sends for a secret it has not got (`GetSecrets`, most often
//! the passphrase of a Wi-Fi network being joined for the first time, or one
//! whose password changed) becomes one **conversation** with the canvas
//! host over the wire: a `Prompt` with the connection's name as its title,
//! one `PROMPT_SECRET`, the host's `Answer`, and a `PROMPT_DONE` or a
//! `PROMPT_CANCEL` at the end (docs/abi.md, *Prompt*, *Answer*). The answer
//! goes to NetworkManager in the method's reply and nowhere else;
//! NetworkManager keeps it in the profile, as it keeps every system-owned
//! secret, so this agent stores nothing.
//!
//! The patterns are every sidecar's (docs/abi.md, *Writing a sidecar*) and
//! the polkit sink's: no arguments, a `Hello` first, one witness a line on
//! stderr, and an exit when the host closes stdin, after unregistering.
//!
//! # Behaviour
//!
//! - **One conversation at a time**; requests that arrive meanwhile queue in
//!   arrival order. A `CancelGetSecrets` takes a queued one out with no frame
//!   (the host never saw it), and ends the one on screen with a
//!   `PROMPT_CANCEL`; its call returns `….Error.AgentCanceled`.
//! - **Interaction only when allowed**: a call without
//!   `ALLOW_INTERACTION` returns `….Error.NoSecrets` at once and shows
//!   nothing, as does a setting this agent has no question for
//!   ([`ask`] has the table).
//! - `REQUEST_NEW` (the last answer was refused) leads the conversation with
//!   a "wrong password" error line.
//! - An `ANSWER_CANCELLED` (the user dismissed the prompt) returns
//!   `….Error.UserCanceled`.
//! - **Nothing typed is ever written anywhere but the reply.** No witness
//!   carries an answer; the copy in the reply is zeroed once the reply is
//!   sent ([`reply`]). The host's stdin is read unbuffered, so no copy of an
//!   answer sits in a read buffer.
//! - NetworkManager restarting (its name changing hands) is answered by
//!   registering again.
//!
//! # Settings
//!
//! From this process's own environment, never the host's (docs/architecture.md,
//! *Configuration is the caustic*):
//!
//! - `CAUSTICS_SECRETS_DRY_RUN=1`: the **dry run**. The system bus is not
//!   touched at all: nothing registers with NetworkManager, and the agent
//!   object is served on the *session* bus instead, taking a call from any
//!   connection, so a test can play NetworkManager and call `GetSecrets`
//!   itself.
//!
//! # Exit codes
//!
//! - **0**: the host closed stdin. A refusal (no system bus, a registration
//!   NetworkManager refused) is one witness and then a wait for that, so the
//!   host does not restart this process into the same answer every second.
//! - **1**: it could not run at all: no runtime, no stdin to read.

pub mod agent;
pub mod ask;
pub mod conversation;
pub mod reply;

use std::io;
use std::os::fd::AsFd as _;

use caustics_bar_abi::wire::{self, Frame};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zbus::Connection;
use zbus::fdo::DBusProxy;

use crate::agent::{Agent, PATH, SERVICE};
use crate::conversation::{Conductor, Event, Outcome, Sink};

/// What this sidecar calls itself.
pub const NAME: &str = "caustics-secrets";

/// The identifier this agent registers under: NetworkManager wants a dotted
/// name, and it is the same for every user.
pub const IDENTIFIER: &str = "caustics.secrets";

/// This sidecar's witness line: `caustics-secrets: …` on stderr, one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new(NAME);

pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The sidecar's settings, from its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `CAUSTICS_SECRETS_DRY_RUN=1`.
    pub dry_run: bool,
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings::from_vars(std::env::var_os("CAUSTICS_SECRETS_DRY_RUN"))
    }

    /// The rule, apart from the environment: only `1` is a dry run.
    pub fn from_vars(dry_run: Option<std::ffi::OsString>) -> Settings {
        Settings {
            dry_run: dry_run.is_some_and(|v| v == "1"),
        }
    }
}

/// `org.freedesktop.NetworkManager.AgentManager`, where an agent registers.
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AgentManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/AgentManager"
)]
trait AgentManager {
    fn register_with_capabilities(&self, identifier: &str, capabilities: u32) -> zbus::Result<()>;
    fn unregister(&self) -> zbus::Result<()>;
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
            dry_run(events, inbox, sink).await
        } else {
            earnest(events, inbox, sink).await
        }
    })
}

/// The host's frames, read on a thread of their own: an `Answer` goes to the
/// conductor, EOF is [`Event::Closed`]. stdin is read through a duplicate of
/// its descriptor, **unbuffered**: `std::io::Stdin` keeps a buffer of what it
/// read, and an answer must leave no copy anywhere that is not zeroed
/// (docs/abi.md, *Answer*). The polkit sink's reader, copied rather than
/// shared, because the event it builds is each sink's own.
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
            Event::Get(request) => {
                let _ = request.reply.send(Outcome::Failed);
            }
            _ => {}
        }
    }
    0
}

/// Serve the agent object on `conn` and run the conductor until the host
/// leaves: the dry run's whole program, and the tests'. `events` is the
/// conductor's inbox's sender, which the object sends requests on.
pub async fn serve(
    conn: &Connection,
    check_caller: bool,
    events: UnboundedSender<Event>,
    sink: Sink,
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
    Conductor::new(sink).run(inbox).await
}

async fn dry_run(
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
        "dry run: the agent is at {} {PATH} on the session bus; the system bus is not touched",
        conn.unique_name().map_or("?", |n| n.as_str()),
    ));
    serve(&conn, false, events, sink, inbox).await
}

async fn earnest(
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
        events,
        check_caller: true,
    };
    if let Err(e) = conn.object_server().at(PATH, agent).await {
        witness(&format!("serving {PATH}: {e}"));
        return idle(inbox).await;
    }
    // Watched before the first registration, so NetworkManager starting
    // between the two is not missed.
    let _again = reregister(&conn);
    match register(&conn).await {
        Ok(()) => witness(&format!(
            "the secret agent {IDENTIFIER} of this user, registered with {SERVICE}"
        )),
        // NetworkManager not running yet: not a refusal. The watch above
        // registers when it comes.
        Err(e) if absent(&e) => {
            witness("absent: networkmanager");
            witness(&format!("registering waits for {SERVICE}: {e}"));
        }
        Err(e) => {
            witness(&format!("NetworkManager refused the registration: {e}"));
            return idle(inbox).await;
        }
    }
    let code = Conductor::new(sink).run(inbox).await;
    match unregister(&conn).await {
        Ok(()) => witness("unregistered"),
        Err(e) => witness(&format!("unregistering: {e}")),
    }
    code
}

/// `RegisterWithCapabilities(IDENTIFIER, 0)`: no VPN hints, no capabilities.
pub async fn register(conn: &Connection) -> zbus::Result<()> {
    AgentManagerProxy::new(conn)
        .await?
        .register_with_capabilities(IDENTIFIER, 0)
        .await
}

pub async fn unregister(conn: &Connection) -> zbus::Result<()> {
    AgentManagerProxy::new(conn).await?.unregister().await
}

/// The error a call to a name nobody owns gets.
fn absent(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ),
        _ => false,
    }
}

/// Register again whenever NetworkManager's name gets a new owner: a
/// restarted NetworkManager has forgotten every agent. The stream is
/// forwarded the moment it exists (docs/architecture.md, *Rules*), and a
/// task of its own makes the call, so the conductor never waits on it.
fn reregister(conn: &Connection) -> tokio::task::JoinHandle<()> {
    let conn = conn.clone();
    tokio::spawn(async move {
        let Ok(dbus) = DBusProxy::new(&conn).await else {
            witness(
                "org.freedesktop.DBus: no proxy; NetworkManager restarting will not be noticed",
            );
            return;
        };
        let stream = match dbus
            .receive_name_owner_changed_with_args(&[(0, SERVICE)])
            .await
        {
            Ok(stream) => stream,
            Err(e) => {
                witness(&format!("watching {SERVICE}: {e}"));
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
            match register(&conn).await {
                Ok(()) => witness(&format!("NetworkManager is {owner} now; registered again")),
                Err(e) => witness(&format!(
                    "NetworkManager is {owner} now; registering again: {e}"
                )),
            }
        }
    })
}
