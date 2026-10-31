//! `caustics-bluetooth` — the bar's bluetooth sidecar.
//!
//! BlueZ's adapter and its paired devices as **one `WIRE_ROWS` frame** with
//! `applet = APPLET_BLUETOOTH`: a chip, an on/off row, a row per paired device,
//! and while the menu is open a row per device in range that is not paired
//! yet, with actions back by row id on stdin (docs/abi.md, *Rows*,
//! *Action*). Nothing here knows about Wayland, the GPU, or the bar's
//! internals.
//!
//! **Pairing** is a click on a device in range: `Pair()`. While the menu is
//! open the adapter is also discoverable, so the other device can begin the
//! pairing instead. Either way **this sidecar is BlueZ's agent**
//! ([`agent`]): it registers as the default, and routes each question
//! ([`pairing`]). A yes or a no (compare a passkey, allow a pairing or a
//! service) is asked by the chip's own rows, shown as a toast; a code to type
//! on the other device is a toast too; and only a PIN or a passkey that must
//! be typed here goes to the canvas's pairing sink over its socket
//! ([`sink`], pairing/README.md), whose answer goes to BlueZ and nowhere
//! else.
//!
//! Why a separate process is the bar's membership test (docs/architecture.md,
//! *Sidecars*): this owns no Wayland object and none of the bar's fds — it owns
//! one system-bus connection and a menu — and the bar copies its bytes without
//! parsing them.
//!
//! The split into a library and a two-line `main.rs` is so `bluetooth/tests/`
//! can drive the frame and the dispatch from synthesised bus bytes; no test ever
//! connects, disconnects, pairs, discovers, or touches `Powered`.
//!
//! # Settings
//!
//! From this process's own environment, never the bar's
//! (docs/architecture.md, *Configuration is the caustic*):
//!
//! - `CAUSTICS_BLUETOOTH_AGENT_DRY_RUN=1`: the agent's **dry run**. The agent
//!   object is served on the *session* bus, takes a call from any
//!   connection, and is registered with nothing, so a test can play BlueZ
//!   with `busctl --user call`. The rest of the sidecar is as ever: BlueZ's
//!   picture is still read from the system bus.

pub mod actions;
pub mod agent;
pub mod ask;
pub mod bluez;
pub mod pairing;
pub mod reply;
pub mod rows;
pub mod sink;

use std::path::PathBuf;
use std::time::Instant;

use caustics_bar_abi::wire::{self, ACTION_TOAST_SHOWN, ActionBody, Frame};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use zbus::Connection;

use bluez::State;
use pairing::{Effect, Pairing, View};

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

/// The sidecar's settings, from its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `CAUSTICS_BLUETOOTH_AGENT_DRY_RUN=1`.
    pub agent_dry_run: bool,
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings::from_vars(std::env::var_os("CAUSTICS_BLUETOOTH_AGENT_DRY_RUN"))
    }

    /// The rule, apart from the environment: only `1` is a dry run.
    pub fn from_vars(dry_run: Option<std::ffi::OsString>) -> Settings {
        Settings {
            agent_dry_run: dry_run.is_some_and(|v| v == "1"),
        }
    }
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
    let settings = Settings::from_env();
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

    // The agent: its events, the sink's replies, BlueZ changing hands.
    let (agent_tx, mut agent_events) = unbounded_channel();
    let (typed_tx, mut typed_replies) = unbounded_channel();
    let (owner_tx, mut owners) = unbounded_channel();
    let mut agent = AgentHost::start(&settings, conn.as_ref(), agent_tx, owner_tx).await;
    let mut asking = Pairing::new();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut revision = 1u32;
    let mut state = State::default();
    if !publish(&mut out, revision, &state, &asking.view()) {
        return 0;
    }
    // Which adapters are discovering for this menu (`actions::Discovery`).
    let mut discovery = actions::Discovery::default();

    loop {
        let shown = asking.view();
        let deadline = asking.deadline();
        let mut moved = false;
        tokio::select! {
            // `Some(..)`: the watcher's end closing simply disables this arm.
            Some(next) = states.recv() => {
                // Publish on change only: BlueZ emits a signal for plenty that
                // does not move this picture.
                if next == state {
                    continue;
                }
                state = next;
                moved = true;
                let paired: Vec<&str> = state.devices.iter().map(|d| d.path.as_str()).collect();
                asking.paired(&paired);
                // An adapter powered while the menu is open looks too.
                for op in discovery.changed(&state) {
                    actions::perform(conn.as_ref(), op).await;
                }
            }
            Some(event) = agent_events.recv() => {
                let effects = asking.event(event, Instant::now());
                agent.effects(effects, &typed_tx);
            }
            Some((id, reply)) = typed_replies.recv() => {
                let effects = asking.typed(id, reply);
                agent.effects(effects, &typed_tx);
            }
            Some(owner) = owners.recv() => {
                witness(&format!("agent: BlueZ is {owner} now; registering again"));
                agent.register().await;
            }
            () = sleep_until(deadline), if deadline.is_some() => {
                asking.expire(Instant::now());
            }
            action = incoming.recv() => match action {
                None => {
                    witness("stdin closed; exiting");
                    asking.leave();
                    leave(conn.as_ref(), &mut discovery).await;
                    agent.leave().await;
                    return 0;
                }
                // The bar's two menu notices: discovery while it is open, and
                // a look at whether the pairing sink is up.
                Some(action) if actions::is_menu_notice(action.kind) => {
                    for op in discovery.notice(action.kind, &state).unwrap_or_default() {
                        actions::perform(conn.as_ref(), op).await;
                    }
                    if discovery.is_open() {
                        agent.recheck().await;
                    }
                }
                // A toast of the agent's rows ended; nothing is kept about it.
                Some(action) if action.kind == ACTION_TOAST_SHOWN => {}
                Some(action) => match asking.click(action.row) {
                    Some(effects) => agent.effects(effects, &typed_tx),
                    None => match actions::op_for(&state, action.row, action.kind) {
                        Some(op) => actions::perform(conn.as_ref(), op).await,
                        None => witness(&format!(
                            "action row={} kind={}: nothing to do",
                            action.row, action.kind
                        )),
                    },
                },
            },
        }
        // Publish whatever moved: the picture, or what the agent asks.
        if moved || asking.view() != shown {
            revision += 1;
            if !publish(&mut out, revision, &state, &asking.view()) {
                asking.leave();
                leave(conn.as_ref(), &mut discovery).await;
                agent.leave().await;
                return 0;
            }
        }
    }
}

/// `deadline`, or never.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// The agent as the loop holds it: where it is served, which capability it
/// registered with, and where the pairing sink's socket is.
struct AgentHost {
    /// The connection BlueZ calls the agent on, in earnest; `None` in the
    /// dry run (registered with nothing) and without a system bus.
    earnest: Option<Connection>,
    /// The capability registered, once registered.
    capability: Option<&'static str>,
    /// `$XDG_RUNTIME_DIR/caustics-pairing.sock`, when there is a runtime
    /// directory.
    socket: Option<PathBuf>,
    /// The dry run's session-bus connection, kept so the object stays served.
    _dry: Option<Connection>,
    _owners: Option<tokio::task::JoinHandle<()>>,
}

impl AgentHost {
    /// Serve the agent and register it: on the system bus in earnest, on the
    /// session bus registered with nothing in the dry run.
    async fn start(
        settings: &Settings,
        system: Option<&Connection>,
        events: UnboundedSender<agent::Event>,
        owners: UnboundedSender<String>,
    ) -> AgentHost {
        let mut host = AgentHost {
            earnest: None,
            capability: None,
            socket: sink::socket_path(),
            _dry: None,
            _owners: None,
        };
        if host.socket.is_none() {
            witness("agent: no XDG_RUNTIME_DIR, so no pairing sink can be asked for a typed PIN");
        }
        if settings.agent_dry_run {
            match Connection::session().await {
                Ok(conn) => match agent::serve(&conn, false, events).await {
                    Ok(_) => {
                        witness(&format!(
                            "agent: dry run: the agent is at {} {} on the session bus; \
                             nothing is registered with BlueZ",
                            conn.unique_name().map_or("?", |n| n.as_str()),
                            agent::PATH
                        ));
                        host._dry = Some(conn);
                    }
                    Err(e) => witness(&format!("agent: dry run: serving {}: {e}", agent::PATH)),
                },
                Err(e) => witness(&format!("agent: dry run: no session bus: {e}")),
            }
            return host;
        }
        let Some(conn) = system else {
            return host;
        };
        if let Err(e) = agent::serve(conn, true, events).await {
            witness(&format!("agent: serving {}: {e}", agent::PATH));
            return host;
        }
        // Watched before the first registration, so BlueZ starting between
        // the two is not missed.
        host._owners = Some(agent::owners(conn, owners));
        host.earnest = Some(conn.clone());
        host.register().await;
        host
    }

    /// Whether the pairing sink answers now.
    async fn sink_answers(&self) -> bool {
        match &self.socket {
            Some(path) => sink::answers(path).await,
            None => false,
        }
    }

    /// Register with the capability the sink's presence calls for.
    async fn register(&mut self) {
        let Some(conn) = self.earnest.clone() else {
            return;
        };
        let answers = self.sink_answers().await;
        let capability = agent::capability(answers);
        let why = if answers {
            "the canvas's pairing sink answers, so a typed PIN can be asked"
        } else {
            "no pairing sink answers, so a typed PIN cannot be asked"
        };
        match agent::register(&conn, capability, why).await {
            Ok(()) => self.capability = Some(capability),
            // BlueZ not running yet: not a refusal. The owner watch registers
            // when it comes.
            Err(e) if agent::absent(&e) => {
                self.capability = None;
                witness(&format!(
                    "agent: registering waits for {}: {}",
                    bluez::SERVICE,
                    agent::error_name(&e)
                ));
            }
            Err(e) => {
                self.capability = None;
                witness(&format!(
                    "agent: BlueZ refused the registration: {}",
                    agent::error_name(&e)
                ));
            }
        }
    }

    /// The menu opened: register again when the sink came or went since.
    async fn recheck(&mut self) {
        let (Some(conn), Some(registered)) = (self.earnest.clone(), self.capability) else {
            return;
        };
        let now = agent::capability(self.sink_answers().await);
        if now == registered {
            return;
        }
        witness(&format!(
            "agent: the pairing sink {}; registering as {now} instead of {registered}",
            if now == agent::KEYBOARD_DISPLAY {
                "answers now"
            } else {
                "is gone"
            }
        ));
        if let Err(e) = agent::unregister(&conn).await {
            witness(&format!("agent: unregistering: {}", agent::error_name(&e)));
        }
        self.register().await;
    }

    /// The socket calls the conductor asked for, each on a task of its own:
    /// a person answers a typed question in their own time.
    fn effects(&self, effects: Vec<Effect>, replies: &UnboundedSender<(u32, sink::Reply)>) {
        for effect in effects {
            let Some(path) = self.socket.clone() else {
                if let Effect::Ask { id, .. } = effect {
                    let _ = replies.send((
                        id,
                        sink::Reply::Unreachable("there is no XDG_RUNTIME_DIR".to_owned()),
                    ));
                }
                continue;
            };
            match effect {
                Effect::Ask { id, typed, title } => {
                    let replies = replies.clone();
                    tokio::spawn(async move {
                        let reply = sink::ask(&path, typed, &title).await;
                        let _ = replies.send((id, reply));
                    });
                }
                Effect::Cancel => {
                    tokio::spawn(async move {
                        if let Err(why) = sink::cancel(&path).await {
                            witness(&format!("agent: cancelling the sink's prompt: {why}"));
                        }
                    });
                }
            }
        }
    }

    /// Unregister before going, so BlueZ does not call a name that is gone.
    async fn leave(&mut self) {
        let Some(conn) = self.earnest.take() else {
            return;
        };
        if self.capability.take().is_none() {
            return;
        }
        match agent::unregister(&conn).await {
            Ok(()) => witness("agent: unregistered"),
            Err(e) => witness(&format!("agent: unregistering: {}", agent::error_name(&e))),
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
fn publish(out: &mut impl std::io::Write, revision: u32, state: &State, asking: &View) -> bool {
    let frame = rows::picture_with(revision, state, asking);
    if let Err(err) = wire::write(out, &frame) {
        if err.kind() == std::io::ErrorKind::BrokenPipe {
            witness("stdout closed; exiting");
        } else {
            witness(&format!("stdout write failed: {err}"));
        }
        return false;
    }
    // `asking=` names which of the agent's rows are up, never what they say.
    let asking = match (asking.question.is_some(), asking.display.is_some()) {
        (true, true) => " asking=question,display",
        (true, false) => " asking=question",
        (false, true) => " asking=display",
        (false, false) => "",
    };
    match state.adapter.as_ref() {
        Some(adapter) => witness(&format!(
            "published rev={revision} powered={} devices={} connected={} in-range={}{asking}",
            adapter.powered,
            state.devices.len(),
            state.connected(),
            state.nearby.len()
        )),
        None => witness(&format!("published rev={revision} no adapter{asking}")),
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
