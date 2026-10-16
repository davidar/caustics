//! `caustics-notify` — the session's notification daemon, as the bar's sidecar
//! (docs/architecture.md, *Notifications*).
//!
//! It owns `org.freedesktop.Notifications` on the session bus (Desktop
//! Notifications Specification 1.2), keeps the history, and publishes **one
//! `WIRE_ROWS` frame** with `applet = APPLET_NOTIFY`: a bell chip with the
//! unread count, and a menu of do-not-disturb, the notifications newest first,
//! and clear-all (docs/abi.md, *Rows*). Clicks come back as `Action` frames
//! on stdin (docs/abi.md, *Action*); an `image-data` hint goes out as a `Pixmap` (docs/abi.md, *Pixmap*) before
//! the frame that names it. It draws no toast itself: the caustic does, and
//! [`RFLAG_SHOW`](caustics_bar_abi::wire::RFLAG_SHOW) on the chip is how this
//! daemon asks for one (docs/architecture.md, *Menus, toasts and popups*).
//!
//! Why a sidecar is the bar's membership test (docs/architecture.md, *Sidecars*): this
//! owns no Wayland object and none of the bar's fds — one session-bus
//! connection and a list — and the bar copies its bytes without parsing them.
//!
//! **The name is queued.** `RequestName` with `AllowReplacement |
//! ReplaceExisting` and no `DoNotQueue`: while another daemon holds the name
//! (cosmic-notifications under cosmic-session, for one) this process says
//! so once, publishes no chip, and serves from the `NameAcquired` on. The two
//! name signals are the only streams it subscribes to, and both go to a drain
//! (`caustics_sidecar::forward`) the moment they exist: a zbus stream nobody polls
//! can stall the whole connection (docs/architecture.md, *Rules*).
//!
//! **A click hands the app focus.** The daemon flags the rows whose click
//! invokes an action `RFLAG_ACTIVATION`, so the bar asks the compositor for an
//! `xdg_activation_v1` token for an activation of one of those rows — never for
//! do-not-disturb, clear-all, settings or a dismissal — and writes it back as a
//! `REQ_ACTIVATION` request naming the row. A click that
//! invokes an action is held until its token arrives — then
//! `ActivationToken(id, token)` goes out *before* `ActionInvoked(id, key)`, as
//! the specification orders — or until [`Config::activation_grace`] passes, and
//! then `ActionInvoked` goes out alone: a compositor that grants nothing must
//! not cost the click.
//!
//! **The click's signals go to the notification's sender**, not to the bus at
//! large. Every notifying app subscribes to the
//! interface's signals and matches `ActionInvoked` by id alone — kitty checks
//! no sender — and ids repeat across daemons: each daemon numbers its own notifications, and one that takes the name over inherits apps that still hold the old daemon's ids. A broadcast `ActionInvoked(31, "default")` meant for one app then reaches every app that holds an id 31, and kitty, which checks no sender, takes the focus. So
//! `ActivationToken` and `ActionInvoked` carry the sender's unique name as
//! their destination; `NotificationClosed` stays a broadcast, as the
//! specification shapes it and nothing acts on it but bookkeeping.
//!
//! **The daemon remembers across restarts.** Every restart (an upgrade, a crash, a new session) is one;
//! starting the ids from 1 again would hand an app's old id to a new
//! notification (a `CloseNotification` or `replaces_id` from the last
//! generation then acts on it) and wipe the unread list. So the id
//! counter and every entry that is not transient live in
//! `$XDG_STATE_HOME/caustics/notify/history.json`, beside the do-not-disturb
//! file: read in [`serve`] before the name is requested — every restored
//! entry shown, so nothing toasts again and nothing is emitted, `unread` as it
//! was — and written from `Daemon::publish` whenever its bytes change, to a
//! `.tmp` renamed over it. A file that does not parse, or of another
//! `version`, is witnessed, ignored and replaced by the first write.

pub mod actions;
pub mod history;
pub mod image;
pub mod markup;
pub mod rows;
pub mod server;

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{self, ActionBody, Frame, Pixmap, REQ_ACTIVATION, RFLAG_SHOW, Rows};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zbus::Connection;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::names::{BusName, UniqueName};
use zbus::object_server::SignalEmitter;

use actions::Op;
use history::{CLOSED_DISMISSED, History, SNAPSHOT_VERSION, Snapshot};
use rows::View;
use server::Server;

/// What this sidecar calls itself: the witness prefix and the pixmap keys'.
pub const NAME: &str = "caustics-notify";

/// The usage text, on stderr — stdout carries frames and nothing else.
pub const USAGE: &str = "\
usage: caustics-notify

  No options. Serves org.freedesktop.Notifications on the session bus (queued
  behind any daemon that holds it), writes one WIRE_ROWS frame (APPLET_NOTIFY)
  to stdout whenever the picture changes, reads WIRE_ACTION frames from stdin,
  and exits when stdin closes. Do-not-disturb is kept in
  $XDG_STATE_HOME/caustics/notify/dnd, the id counter and the notifications in
  $XDG_STATE_HOME/caustics/notify/history.json. Every witness is one line on
  stderr.";

/// This sidecar's witness line: `caustics-notify: …` on stderr, in one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new(NAME);

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// Where frames go: stdout in the binary, a channel in a test.
pub type Sink = Box<dyn FnMut(&Frame) -> std::io::Result<()> + Send + Sync>;

/// What the bar writes on stdin that this daemon acts on.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A click, a menu opening or closing (docs/abi.md, *Action*).
    Action(ActionBody),
    /// The compositor's token for the click that activated `row`
    /// (`REQ_ACTIVATION`, docs/abi.md, *Request*).
    Activation { row: u32, token: String },
}

/// How long a click that invokes an action waits for its activation token
/// before `ActionInvoked` goes out without one. The token
/// is one compositor round trip behind the action on the same pipe, so it is
/// milliseconds when it comes at all.
pub const ACTIVATION_GRACE: Duration = Duration::from_millis(100);

/// What the daemon is told from outside the bus.
#[derive(Clone, Debug)]
pub struct Config {
    /// The do-not-disturb file.
    pub dnd_path: PathBuf,
    /// The history file: the id counter and the kept notifications
    /// (the module note says why they persist).
    pub history_path: PathBuf,
    /// The length of a minute: the open menu's re-send period and the unit of
    /// "5 min ago". A minute, except in a test.
    pub minute: Duration,
    /// How long an invoked action waits for its token: [`ACTIVATION_GRACE`].
    pub activation_grace: Duration,
}

impl Config {
    /// The binary's: the state files under `$XDG_STATE_HOME`, a real minute.
    pub fn from_env() -> Config {
        let (state_home, home) = (std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"));
        Config {
            dnd_path: dnd_path(state_home.clone(), home.clone()),
            history_path: history_path(state_home, home),
            minute: Duration::from_secs(60),
            activation_grace: ACTIVATION_GRACE,
        }
    }
}

/// `$XDG_STATE_HOME/caustics/notify/dnd`, `~/.local/state` when the variable is
/// unset or relative (the XDG spec says a relative one is ignored) — the
/// favourites file is the precedent (`bar/src/favourites.rs`).
pub fn dnd_path(state_home: Option<OsString>, home: Option<OsString>) -> PathBuf {
    state_dir(state_home, home).join("dnd")
}

/// `$XDG_STATE_HOME/caustics/notify/history.json`, by the same rule as
/// [`dnd_path`].
pub fn history_path(state_home: Option<OsString>, home: Option<OsString>) -> PathBuf {
    state_dir(state_home, home).join("history.json")
}

fn state_dir(state_home: Option<OsString>, home: Option<OsString>) -> PathBuf {
    let base = state_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    base.join("caustics/notify")
}

/// Do-not-disturb as the file says: `1` is on, anything else (or no file) off.
pub fn read_dnd(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.trim() == "1")
}

/// Write it, making the directory.
pub fn write_dnd(path: &Path, on: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, if on { "1\n" } else { "0\n" })
}

/// The history file read: `Ok(None)` when
/// there is no file, an error when it does not parse or its `version` is not
/// [`SNAPSHOT_VERSION`]. The version is read first, so a file from a later
/// format says so rather than failing on its first unknown field.
pub fn read_history(path: &Path) -> Result<Option<Snapshot>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    #[derive(serde::Deserialize)]
    struct Version {
        version: u32,
    }
    let version = serde_json::from_slice::<Version>(&bytes)
        .map_err(|e| e.to_string())?
        .version;
    if version != SNAPSHOT_VERSION {
        return Err(format!("version {version}, not {SNAPSHOT_VERSION}"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// The history the daemon starts with: the file's, as of `now`, or an empty
/// one — when there is no file, and when it cannot be read, which is
/// witnessed and left for the first write to replace. Also the bytes the
/// restored history serialises to when the file was good, so a restart does
/// not rewrite what is already there; `None` when it must be written.
pub fn load_history(path: &Path, now: Instant) -> (History, Option<Vec<u8>>) {
    match read_history(path) {
        Ok(Some(snapshot)) => {
            let history = History::restore(snapshot, now);
            let again = history.snapshot();
            witness(&format!(
                "history.json: restored {} entries, {} unread, next id {}",
                again.entries.len(),
                history.unread(),
                again.next_id,
            ));
            (history, serde_json::to_vec(&again).ok())
        }
        Ok(None) => (History::new(), None),
        Err(e) => {
            witness(&format!("history.json: {e}; starting empty"));
            (History::new(), None)
        }
    }
}

/// Write the history file: `history.json.tmp` beside it, renamed over it,
/// so a reader never sees half a file. Makes the directory.
pub fn write_history(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
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
        let (act_tx, mut incoming) = unbounded_channel();
        watch_stdin(act_tx);
        let conn = match Connection::session().await {
            Ok(conn) => conn,
            Err(e) => {
                // No session bus is no notifications: stay up for stdin, as
                // every sidecar does, and publish nothing.
                witness("absent: session-bus");
                witness(&format!("session-bus: {e}"));
                while incoming.recv().await.is_some() {}
                return 0;
            }
        };
        let sink: Sink = Box::new(|frame| wire::write(&mut std::io::stdout().lock(), frame));
        serve(conn, Config::from_env(), incoming, sink).await
    })
}

/// A name event, from the two drained streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Name {
    Acquired,
    Lost,
}

/// Serve on `conn` until `actions` closes (the bar is gone) or the sink fails.
pub async fn serve(
    conn: Connection,
    config: Config,
    mut actions: UnboundedReceiver<Input>,
    sink: Sink,
) -> i32 {
    // The last process's history, before the object is served and the name
    // requested: no `Notify` lands in an empty history that the file
    // then overwrites, and nothing is emitted for what comes back.
    let (history, written) = load_history(&config.history_path, Instant::now());
    let history = Arc::new(Mutex::new(history));
    let wake = Arc::new(Notify::new());
    let server = Server {
        history: history.clone(),
        wake: wake.clone(),
    };
    if let Err(e) = conn.object_server().at(server::PATH, server).await {
        witness(&format!("serving {}: {e}", server::PATH));
        return 1;
    }

    // Both name streams exist before the request, so an acquisition that
    // happens between the reply and the first poll is not missed; both are
    // drained from the moment they exist.
    let (names_tx, mut names) = unbounded_channel();
    let dbus = match DBusProxy::new(&conn).await {
        Ok(dbus) => dbus,
        Err(e) => {
            witness(&format!("org.freedesktop.DBus: {e}"));
            return 1;
        }
    };
    let _acquired = watch_name(&dbus, &names_tx, Name::Acquired).await;
    let _lost = watch_name(&dbus, &names_tx, Name::Lost).await;
    drop(names_tx);

    let flags = RequestNameFlags::AllowReplacement | RequestNameFlags::ReplaceExisting;
    let owned = match conn.request_name_with_flags(server::NAME, flags).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            witness(&format!("serving {}", server::NAME));
            true
        }
        Ok(reply) => {
            let holder = dbus
                .get_name_owner(server::NAME.try_into().expect("a well-known name"))
                .await
                .map(|owner| owner.to_string())
                .unwrap_or_else(|_| "nobody".to_owned());
            witness(&format!(
                "waiting for {} (held by {holder}; {reply:?})",
                server::NAME
            ));
            false
        }
        Err(e) => {
            witness(&format!("requesting {}: {e}; waiting", server::NAME));
            false
        }
    };

    let dnd = read_dnd(&config.dnd_path);
    let mut daemon = Daemon {
        conn: conn.clone(),
        history,
        config,
        sink,
        dnd,
        owned,
        open: false,
        revision: 0,
        last: None,
        sent: HashSet::new(),
        held: Vec::new(),
        written,
    };
    if !daemon.publish().await {
        return 0;
    }

    let mut tick = tokio::time::interval(daemon.config.minute);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let deadline = daemon
            .history
            .lock()
            .expect("the history")
            .next_deadline(Instant::now());
        let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(
            deadline.unwrap_or_else(Instant::now),
        ));
        let grace = daemon.held.iter().map(|held| held.until).min();
        let graced = tokio::time::sleep_until(tokio::time::Instant::from_std(
            grace.unwrap_or_else(Instant::now),
        ));
        tokio::select! {
            _ = wake.notified() => {}
            Some(name) = names.recv() => {
                let owned = name == Name::Acquired;
                if owned != daemon.owned {
                    daemon.owned = owned;
                    witness(&match name {
                        Name::Acquired => format!("acquired {0}; serving {0}", server::NAME),
                        Name::Lost => format!("lost {}; waiting", server::NAME),
                    });
                }
            }
            action = actions.recv() => match action {
                None => {
                    witness("stdin closed; exiting");
                    return 0;
                }
                Some(Input::Activation { row, token }) => {
                    daemon.token(row, token).await;
                }
                Some(Input::Action(action)) => {
                    let op = {
                        let history = daemon.history.lock().expect("the history");
                        actions::op_for(&history, action.row, action.kind)
                    };
                    match op {
                        Some(Op::MenuOpen) => tick.reset(),
                        Some(_) => {}
                        None => {
                            witness(&format!("action row={:#x} kind={}: nothing to do", action.row, action.kind));
                            continue;
                        }
                    }
                    daemon.perform(action.row, op.expect("matched above")).await;
                }
            },
            _ = tick.tick(), if daemon.open => {}
            _ = sleep, if deadline.is_some() => {}
            _ = graced, if grace.is_some() => daemon.expire_held().await,
        }
        if !daemon.publish().await {
            return 0;
        }
    }
}

/// One name signal, forwarded into `sink` by a drain.
async fn watch_name(
    dbus: &DBusProxy<'_>,
    sink: &UnboundedSender<Name>,
    which: Name,
) -> Option<caustics_sidecar::Drain> {
    let ours = |name: &str| name == server::NAME;
    let drain = match which {
        Name::Acquired => dbus.receive_name_acquired().await.map(|stream| {
            caustics_sidecar::forward(stream, sink.clone(), move |signal| {
                signal
                    .args()
                    .ok()
                    .filter(|a| ours(a.name().as_str()))
                    .map(|_| Name::Acquired)
            })
        }),
        Name::Lost => dbus.receive_name_lost().await.map(|stream| {
            caustics_sidecar::forward(stream, sink.clone(), move |signal| {
                signal
                    .args()
                    .ok()
                    .filter(|a| ours(a.name().as_str()))
                    .map(|_| Name::Lost)
            })
        }),
    };
    match drain {
        Ok(drain) => Some(drain),
        Err(e) => {
            witness(&format!("subscribing to {which:?}: {e}"));
            None
        }
    }
}

/// The loop's state.
struct Daemon {
    conn: Connection,
    history: Arc<Mutex<History>>,
    config: Config,
    sink: Sink,
    dnd: bool,
    owned: bool,
    /// The bar has this applet's menu open (`ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`, docs/abi.md, *Action*).
    open: bool,
    revision: u32,
    /// The last frame written and its ageless twin (`rows::worth_sending`).
    last: Option<(Rows, Rows)>,
    /// `Pixmap` keys written by this process (docs/abi.md, *Pixmap*): each once.
    sent: HashSet<String>,
    /// Invoked actions waiting for their activation token, oldest first.
    held: Vec<Held>,
    /// The history file's bytes as last written — or tried, so a failing
    /// write costs one witness line per change, not one per publish.
    written: Option<Vec<u8>>,
}

/// One click whose `ActionInvoked` waits for a token, or for the grace.
struct Held {
    /// The row the click was on: what the bar's `REQ_ACTIVATION` names.
    row: u32,
    id: u32,
    key: String,
    /// The notification's sender: the signals' destination (the module note says why).
    sender: Option<String>,
    /// The notification left the history at the click, and its
    /// `NotificationClosed` follows the `ActionInvoked`.
    closed: bool,
    until: Instant,
}

impl Daemon {
    fn emitter(&self) -> Option<SignalEmitter<'_>> {
        SignalEmitter::new(&self.conn, server::PATH).ok()
    }

    async fn closed(&self, closed: &[history::Closed]) {
        if let Some(emitter) = self.emitter() {
            server::emit_closed(&emitter, closed).await;
        }
    }

    async fn perform(&mut self, row: u32, op: Op) {
        match op {
            Op::MenuOpen => {
                self.open = true;
                self.history.lock().expect("the history").mark_read();
            }
            Op::MenuClose => self.open = false,
            Op::ToggleDnd => {
                self.dnd = !self.dnd;
                witness(&format!("dnd {}", if self.dnd { "on" } else { "off" }));
                if let Err(e) = write_dnd(&self.config.dnd_path, self.dnd) {
                    witness(&format!("{}: {e}", self.config.dnd_path.display()));
                }
            }
            Op::ClearAll => {
                let ids = self.history.lock().expect("the history").clear();
                let closed: Vec<_> = ids.into_iter().map(|id| (id, CLOSED_DISMISSED)).collect();
                self.closed(&closed).await;
            }
            Op::Settings => actions::spawn_settings(),
            Op::ToastShown { id } => {
                // The bar says which rows a toast actually showed, so exactly
                // these are marked: a row held behind the toast is still owed one.
                self.history.lock().expect("the history").mark_shown(id);
            }
            Op::Invoke { id, key, keep } => {
                // The row leaves the picture at the click; the signals wait
                // for the token when there are signals to send. Which rows the ended toast showed, the bar reports separately with `ACTION_TOAST_SHOWN`; a row held behind it is still
                // owed its toast. A resident one that stays is read.
                let (sender, closed) = {
                    let mut history = self.history.lock().expect("the history");
                    let sender = history.get(id).and_then(|entry| entry.sender.clone());
                    if keep {
                        history.read(id);
                    }
                    (sender, !keep && history.remove(id))
                };
                match key {
                    Some(key) if !self.config.activation_grace.is_zero() => {
                        self.held.push(Held {
                            row,
                            id,
                            key,
                            sender,
                            closed,
                            until: Instant::now() + self.config.activation_grace,
                        });
                    }
                    Some(key) => self.invoke(id, &key, sender.as_deref(), None, closed).await,
                    None if closed => self.closed(&[(id, CLOSED_DISMISSED)]).await,
                    None => {}
                }
            }
        }
    }

    /// `ActivationToken` when there is one, then `ActionInvoked`, then
    /// `NotificationClosed` when the click took it out of the history — the
    /// specification's order (Desktop Notifications 1.2, *Signals*). The
    /// first two go to `sender` alone; without one (a notification
    /// that came from no connection, which the bus never produces) they go
    /// out as broadcasts.
    async fn invoke(
        &self,
        id: u32,
        key: &str,
        sender: Option<&str>,
        token: Option<&str>,
        closed: bool,
    ) {
        let destination = sender.and_then(|name| UniqueName::try_from(name.to_owned()).ok());
        witness(&format!(
            "action id={id} key={key:?} {} to {}",
            match token {
                Some(token) => format!("token ({} bytes)", token.len()),
                None => "no token".to_owned(),
            },
            match &destination {
                Some(name) => name.to_string(),
                None => "everyone".to_owned(),
            }
        ));
        if let Some(emitter) = self.emitter() {
            let emitter = match destination {
                Some(name) => emitter.set_destination(BusName::Unique(name)),
                None => emitter,
            };
            if let Some(token) = token
                && let Err(e) = Server::activation_token(&emitter, id, token).await
            {
                witness(&format!("ActivationToken({id}): {e}"));
            }
            if let Err(e) = Server::action_invoked(&emitter, id, key).await {
                witness(&format!("ActionInvoked({id}): {e}"));
            }
        }
        if closed {
            self.closed(&[(id, CLOSED_DISMISSED)]).await;
        }
    }

    /// The bar's token for the click on `row`: the oldest click held for that
    /// row goes out with it.
    async fn token(&mut self, row: u32, token: String) {
        let Some(at) = self.held.iter().position(|held| held.row == row) else {
            witness(&format!(
                "activation: a token for row={row:#x} with no click waiting; dropped"
            ));
            return;
        };
        let held = self.held.remove(at);
        self.invoke(
            held.id,
            &held.key,
            held.sender.as_deref(),
            Some(&token),
            held.closed,
        )
        .await;
    }

    /// Every held click whose grace has passed goes out without a token.
    async fn expire_held(&mut self) {
        let now = Instant::now();
        let (late, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|held| held.until <= now);
        self.held = waiting;
        for held in late {
            self.invoke(
                held.id,
                &held.key,
                held.sender.as_deref(),
                None,
                held.closed,
            )
            .await;
        }
    }

    /// Build the picture and write it when it differs from the last one.
    /// `false` means the sink is gone, and so are we.
    async fn publish(&mut self) -> bool {
        let now = Instant::now();
        let expired = {
            let mut history = self.history.lock().expect("the history");
            // `shown` by the clock, and by do-not-disturb: a toast it kept
            // from the screen is not owed when it is turned off again.
            history.mark_past_shown(now);
            if self.dnd {
                history.mark_toasts_shown(now);
            }
            history.expire(now, self.dnd)
        };
        self.closed(&expired).await;

        let view = View {
            owned: self.owned,
            dnd: self.dnd,
            now,
            minute: self.config.minute,
            ages: true,
            settings: actions::settings_available(),
        };
        let (rows, ageless, pixmaps, unread, entries, snapshot) = {
            let history = self.history.lock().expect("the history");
            let (rows, pixmaps) = rows::picture(self.revision + 1, &history, view);
            let (ageless, _) = rows::picture(
                self.revision + 1,
                &history,
                View {
                    ages: false,
                    ..view
                },
            );
            (
                rows,
                ageless,
                pixmaps,
                history.unread(),
                history.entries().len(),
                history.snapshot(),
            )
        };
        // The file follows the history, not the frame: `worth_sending` holds
        // back age-only frames, and a change it holds back is still a change.
        self.save(&snapshot);
        let next = (rows, ageless);
        if !rows::worth_sending(self.last.as_ref(), &next) {
            return true;
        }
        let rows = &next.0;
        self.revision += 1;

        // The texels first, so the bar holds them before it reads the name.
        let mut owed = 0;
        for (key, rgba) in pixmaps {
            if !self.sent.insert(key.clone()) {
                continue;
            }
            let frame = Frame::Pixmap(Pixmap {
                key,
                width: caustics_bar_abi::IMAGE_SLOT_PX,
                height: caustics_bar_abi::IMAGE_SLOT_PX,
                rgba,
            });
            if !self.write(&frame) {
                return false;
            }
            owed += 1;
        }
        let show = rows
            .rows
            .first()
            .is_some_and(|chip| chip.flags & RFLAG_SHOW != 0);
        if !self.write(&Frame::Rows(rows.clone())) {
            return false;
        }
        if self.owned {
            witness(&format!(
                "published rev={} unread={unread} entries={entries} dnd={} show={} pixmaps={owed}",
                self.revision,
                u8::from(self.dnd),
                u8::from(show),
            ));
        } else {
            witness(&format!(
                "published rev={} no chip (the name is another's)",
                self.revision
            ));
        }
        self.last = Some(next);
        true
    }

    /// Write the history file when its bytes differ from the last written:
    /// once per change, never per tick. An error is one witness line
    /// and the daemon carries on.
    fn save(&mut self, snapshot: &Snapshot) {
        let bytes = match serde_json::to_vec(snapshot) {
            Ok(bytes) => bytes,
            Err(e) => {
                witness(&format!("history.json: {e}"));
                return;
            }
        };
        if self.written.as_ref() == Some(&bytes) {
            return;
        }
        if let Err(e) = write_history(&self.config.history_path, &bytes) {
            witness(&format!("{}: {e}", self.config.history_path.display()));
        }
        self.written = Some(bytes);
    }

    fn write(&mut self, frame: &Frame) -> bool {
        match (self.sink)(frame) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                witness("stdout closed; exiting");
                false
            }
            Err(e) => {
                witness(&format!("stdout write failed: {e}"));
                false
            }
        }
    }
}

/// Read `Action` frames on a thread and hand them to the loop. Dropping the
/// sender at EOF is what tells the loop the bar is gone.
fn watch_stdin(actions: UnboundedSender<Input>) {
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read(&mut input) {
                    Ok(None) => return,
                    Ok(Some(Frame::Action(action))) => {
                        if actions.send(Input::Action(action)).is_err() {
                            return;
                        }
                    }
                    Ok(Some(Frame::Request(request))) if request.kind == REQ_ACTIVATION => {
                        let input = Input::Activation {
                            row: request.arg,
                            token: request.name,
                        };
                        if actions.send(input).is_err() {
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
