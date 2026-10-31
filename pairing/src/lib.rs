//! `caustics-pairing` — BlueZ's pairing agent, as a sink sidecar of the
//! canvas (docs/canvas.md, *Sinks*).
//!
//! It registers with BlueZ's agent manager as the default agent, and each
//! question BlueZ puts while a device pairs (confirm a passkey, type a PIN
//! or a passkey, allow a pairing or a service) becomes one **conversation**
//! with the canvas host over the wire: a `Prompt` with the device's alias as
//! its title, the line and the ask, the host's `Answer`, and a `PROMPT_DONE`
//! or a `PROMPT_CANCEL` at the end (docs/abi.md, *Prompt*, *Answer*). A code
//! BlueZ shows rather than asks for (`DisplayPinCode`, `DisplayPasskey`) is
//! a conversation with a line and no ask. The answer goes to BlueZ in the
//! method's reply and nowhere else.
//!
//! The patterns are every sidecar's (docs/abi.md, *Writing a sidecar*) and
//! the secret agent's, whose twin this is: no arguments, a `Hello` first,
//! one witness a line on stderr, and an exit when the host closes stdin,
//! after unregistering.
//!
//! # Behaviour
//!
//! - **One conversation at a time**; requests that arrive meanwhile queue in
//!   arrival order, and an ask supersedes a code on screen
//!   ([`conversation`] has the life of each).
//! - A `Cancel` or a `Release` ends what this agent holds: the conversation
//!   on screen with a `PROMPT_CANCEL`, a pending method with
//!   `org.bluez.Error.Canceled`.
//! - An `ANSWER_CANCELLED` (the user dismissed the prompt) returns
//!   `org.bluez.Error.Rejected`.
//! - **No PIN or passkey is ever written anywhere but the reply**, and none
//!   is in a witness; a passkey BlueZ shows (it is on the other device's
//!   screen too) is in the prompt's line, and still in no witness. The PIN's
//!   copy in the reply is zeroed once the reply is sent ([`reply`]). The
//!   host's stdin is read unbuffered, so no copy of an answer sits in a read
//!   buffer.
//! - BlueZ restarting (its name changing hands) is answered by registering
//!   again.
//!
//! # Settings
//!
//! From this process's own environment, never the host's (docs/architecture.md,
//! *Configuration is the caustic*):
//!
//! - `CAUSTICS_PAIRING_DRY_RUN=1`: the **dry run**. The system bus is not
//!   touched at all: nothing registers with BlueZ, and the agent object is
//!   served on the *session* bus instead, taking a call from any connection,
//!   so a test can play BlueZ and call `RequestConfirmation` itself.
//!
//! # Exit codes
//!
//! - **0**: the host closed stdin. A refusal (no system bus, a registration
//!   BlueZ refused) is one witness and then a wait for that, so the host does
//!   not restart this process into the same answer every second.
//! - **1**: it could not run at all: no runtime, no stdin to read.

pub mod agent;
pub mod ask;
pub mod conversation;
pub mod reply;

use std::collections::HashMap;
use std::io;
use std::os::fd::AsFd as _;

use caustics_bar_abi::wire::{self, Frame};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zbus::fdo::DBusProxy;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

use crate::agent::{Agent, DEVICE, PATH, SERVICE};
use crate::conversation::{
    Conductor, DISPLAY_FOR, Event, Outcome, PAIRED, Sink, TIMED_OUT, Watch, Watching,
};

/// What this sidecar calls itself.
pub const NAME: &str = "caustics-pairing";

/// The capability this agent registers with: it can show a code and take
/// one typed, so BlueZ picks the strongest pairing both devices allow.
pub const CAPABILITY: &str = "KeyboardDisplay";

/// This sidecar's witness line: `caustics-pairing: …` on stderr, one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new(NAME);

pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The sidecar's settings, from its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `CAUSTICS_PAIRING_DRY_RUN=1`.
    pub dry_run: bool,
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings::from_vars(std::env::var_os("CAUSTICS_PAIRING_DRY_RUN"))
    }

    /// The rule, apart from the environment: only `1` is a dry run.
    pub fn from_vars(dry_run: Option<std::ffi::OsString>) -> Settings {
        Settings {
            dry_run: dry_run.is_some_and(|v| v == "1"),
        }
    }
}

/// `org.bluez.AgentManager1` at `/org/bluez`, where an agent registers.
#[zbus::proxy(
    interface = "org.bluez.AgentManager1",
    default_service = "org.bluez",
    default_path = "/org/bluez"
)]
trait AgentManager {
    fn register_agent(&self, agent: &ObjectPath<'_>, capability: &str) -> zbus::Result<()>;
    fn request_default_agent(&self, agent: &ObjectPath<'_>) -> zbus::Result<()>;
    fn unregister_agent(&self, agent: &ObjectPath<'_>) -> zbus::Result<()>;
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
/// (docs/abi.md, *Answer*). The secret agent's reader, copied rather than
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
            Event::Ask(request) => {
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
/// `check_caller` is earnest's: only BlueZ may call, and a shown code's
/// device is watched for BlueZ's signals alone.
pub async fn serve(
    conn: &Connection,
    check_caller: bool,
    events: UnboundedSender<Event>,
    sink: Sink,
    inbox: UnboundedReceiver<Event>,
) -> i32 {
    let watch = watcher(conn.clone(), events.clone(), check_caller);
    let agent = Agent {
        events,
        check_caller,
    };
    if let Err(e) = conn.object_server().at(PATH, agent).await {
        witness(&format!("serving {PATH}: {e}"));
        return idle(inbox).await;
    }
    Conductor::with_watch(sink, watch).run(inbox).await
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
    let watch = watcher(conn.clone(), events.clone(), true);
    let agent = Agent {
        events,
        check_caller: true,
    };
    if let Err(e) = conn.object_server().at(PATH, agent).await {
        witness(&format!("serving {PATH}: {e}"));
        return idle(inbox).await;
    }
    // Watched before the first registration, so BlueZ starting between the
    // two is not missed.
    let _again = reregister(&conn);
    match register(&conn).await {
        Ok(()) => {}
        // BlueZ not running yet: not a refusal. The watch above registers
        // when it comes.
        Err(e) if absent(&e) => {
            witness("absent: bluez");
            witness(&format!("registering waits for {SERVICE}: {e}"));
        }
        Err(e) => {
            witness(&format!(
                "BlueZ refused the registration: {}",
                error_name(&e)
            ));
            return idle(inbox).await;
        }
    }
    let code = Conductor::with_watch(sink, watch).run(inbox).await;
    match unregister(&conn).await {
        Ok(()) => witness("unregistered"),
        Err(e) => witness(&format!("unregistering: {}", error_name(&e))),
    }
    code
}

fn path() -> ObjectPath<'static> {
    ObjectPath::from_static_str_unchecked(PATH)
}

/// `RegisterAgent(PATH, "KeyboardDisplay")`, then `RequestDefaultAgent(PATH)`
/// so the pairings BlueZ starts for a connection with no agent of its own
/// (the bar's Bluetooth sidecar's `Pair`) ask this one. The second refused is
/// one witness and not a failure: the agent still answers its own requests.
pub async fn register(conn: &Connection) -> zbus::Result<()> {
    let manager = AgentManagerProxy::new(conn).await?;
    manager.register_agent(&path(), CAPABILITY).await?;
    match manager.request_default_agent(&path()).await {
        Ok(()) => witness(&format!(
            "the pairing agent {PATH} ({CAPABILITY}), registered with {SERVICE} as the default"
        )),
        Err(e) => witness(&format!(
            "the pairing agent {PATH} ({CAPABILITY}), registered with {SERVICE}; \
             not the default: {}",
            error_name(&e)
        )),
    }
    Ok(())
}

pub async fn unregister(conn: &Connection) -> zbus::Result<()> {
    AgentManagerProxy::new(conn)
        .await?
        .unregister_agent(&path())
        .await
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

/// An error as BlueZ named it (`org.bluez.Error.AlreadyExists`), with its
/// words; anything else as zbus says it.
pub fn error_name(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, Some(words), _) => format!("{} ({words})", name.as_str()),
        zbus::Error::MethodError(name, None, _) => name.as_str().to_owned(),
        other => other.to_string(),
    }
}

/// Register again whenever BlueZ's name gets a new owner: a restarted
/// bluetoothd has forgotten every agent (and released this one on its way
/// down). The stream is forwarded the moment it exists (docs/architecture.md,
/// *Rules*), and a task of its own makes the call, so the conductor never
/// waits on it.
fn reregister(conn: &Connection) -> tokio::task::JoinHandle<()> {
    let conn = conn.clone();
    tokio::spawn(async move {
        let Ok(dbus) = DBusProxy::new(&conn).await else {
            witness("org.freedesktop.DBus: no proxy; BlueZ restarting will not be noticed");
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
            witness(&format!("BlueZ is {owner} now; registering again"));
            if let Err(e) = register(&conn).await {
                witness(&format!(
                    "BlueZ is {owner} now; registering again: {}",
                    error_name(&e)
                ));
            }
        }
    })
}

/// The watch a shown code starts: the device's `Paired` turning true ends
/// it as [`PAIRED`], and [`DISPLAY_FOR`] passing as [`TIMED_OUT`]. In
/// earnest only BlueZ's own signals count.
fn watcher(conn: Connection, events: UnboundedSender<Event>, from_bluez: bool) -> Watch {
    Box::new(move |id, device| {
        let conn = conn.clone();
        let events = events.clone();
        let device = device.to_owned();
        Some(Watching(tokio::spawn(async move {
            let why = paired_or_timeout(&conn, &device, from_bluez).await;
            let _ = events.send(Event::Settled { id, why });
        })))
    })
}

async fn paired_or_timeout(conn: &Connection, device: &str, from_bluez: bool) -> &'static str {
    let rule = || -> zbus::Result<MatchRule<'_>> {
        let mut rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .path(device)?
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .arg(0, DEVICE)?;
        if from_bluez {
            rule = rule.sender(SERVICE)?;
        }
        Ok(rule.build())
    };
    let stream = match rule() {
        Ok(rule) => MessageStream::for_match_rule(rule, conn, Some(8)).await,
        Err(e) => Err(e),
    };
    let stream = match stream {
        Ok(stream) => stream,
        Err(e) => {
            witness(&format!(
                "watching {device}: {}; the code is held until the timeout",
                error_name(&e)
            ));
            tokio::time::sleep(DISPLAY_FOR).await;
            return TIMED_OUT;
        }
    };
    let (tx, mut paired) = unbounded_channel();
    let _drain = caustics_sidecar::forward(stream, tx, |message| {
        let message = message.ok()?;
        let (_, changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
            message.body().deserialize().ok()?;
        matches!(changed.get("Paired").map(|v| &**v), Some(Value::Bool(true))).then_some(())
    });
    tokio::select! {
        Some(()) = paired.recv() => PAIRED,
        () = tokio::time::sleep(DISPLAY_FOR) => TIMED_OUT,
    }
}
