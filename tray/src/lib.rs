//! `caustics-tray` — the bar's tray sidecar.
//!
//! The StatusNotifierWatcher decision, one tokio task per item, and each
//! item's dbusmenu — all of it outside the bar process, which
//! knows only Wayland, the compositor's IPC socket and pipes. What crosses to
//! the bar is **one `WIRE_ROWS` frame** (docs/abi.md, *Rows*): one `ROW_ITEM`
//! per item in registration order, each item's whole menu tree beneath it, and
//! actions back by row id on stdin. Whole-state, rewritten on every change,
//! and once at startup even when there is nothing on the bus. A pixmap icon is
//! a **name** in that frame, and its texels a `Pixmap` frame of their own,
//! sent once per key before the first frame that names it (docs/abi.md, *Pixmap*).
//!
//! ```text
//!   run ── stdin thread ─ wire::read ─────────────────► Msg::Action ─┐
//!       └─ serve ─ session bus ─ watcher::establish              State ─ stdout
//!                    └─ item::run(one task per item) ─► Msg::Item/Menu/Gone ┘
//! ```
//!
//! Nothing in here decides what the bar looks like: the frame says what
//! exists, and placement, shaping and the open/closed state of a menu are the
//! bar's and the caustic's. What *is* decided here is what the session bus
//! means — that a `Passive` item is not shown, that a `NewIcon` signal is a
//! re-read, that own-or-host is retried after a second — because that is the
//! knowledge this process exists to keep out of the bar.
//!
//! Menus are published **eagerly**: the wire does tell a sidecar when its menu opens (`ACTION_MENU_OPEN`), but fetching then would put a D-Bus round trip between the click and the menu, and the bar shows an applet's rows only while its popup is open (docs/abi.md, *Rows*), so the
//! whole tree is in every frame and a click never waits for a round trip.
//!
//! The split into a library and a two-line `main.rs` is so `tray/tests/` can
//! drive the translation ([`rows`]) and the deserialiser ([`menu`]) with
//! recorded bytes and fakes; nothing else depends on this crate.

pub mod icon;
pub mod item;
pub mod menu;
pub mod model;
pub mod rows;
pub mod watcher;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;

use caustics_bar_abi::wire::{self, ActionBody, Frame, Rows};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zbus::Connection;

use crate::model::{Menu, TrayAddress, TrayItem};
use crate::rows::{Call, Entry, Ids, Sent, Target};
use crate::watcher::{RETRY, WatcherEvent};

/// The usage text, on stderr — stdout carries frames and nothing else.
pub const USAGE: &str = "\
usage: caustics-tray

The bar's tray as a sidecar: one WIRE_ROWS frame on stdout whenever the
session bus's StatusNotifierItems or their dbusmenus change, WIRE_ACTION
frames read from stdin, witnesses on stderr; exits 0 when stdin reaches EOF.
It takes no arguments and reads no configuration.";

/// This sidecar's witness line: `caustics-tray: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-tray");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// What reaches the one loop that owns the picture. Every sender is either an
/// item's task or the stdin thread; nothing else exists.
#[derive(Debug)]
pub enum Msg {
    /// An item's properties, freshly read.
    Item { key: String, item: Box<TrayItem> },
    /// An item's dbusmenu, freshly fetched.
    Menu { key: String, menu: Box<Menu> },
    /// The item stopped exporting a menu path.
    MenuGone(String),
    /// The item is gone, or was never reachable.
    Gone(String),
    /// The bar decided something about one of our rows.
    Action(ActionBody),
}

// ---------------------------------------------------------------------------
// The picture
// ---------------------------------------------------------------------------

/// One item's task and the channel its calls go down. Dropping it aborts the
/// task, which is how an item that hangs stops costing anything.
struct ItemTask {
    bus_name: String,
    calls: UnboundedSender<Call>,
    task: JoinHandle<()>,
}

impl Drop for ItemTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Everything the frame is made of: which items exist, in what order, what
/// each last said, and the ids that have been handed out.
#[derive(Default)]
struct State {
    /// Registration order — the order the rows come out in, which is the order
    /// the bar publishes them and the caustic lays them out.
    order: Vec<String>,
    items: HashMap<String, TrayItem>,
    menus: HashMap<String, Menu>,
    tasks: HashMap<String, ItemTask>,
    ids: Ids,
    revision: u32,
    last: Option<Rows>,
    targets: HashMap<u32, Target>,
    /// Witnesses already written, so that a thing that does not fit is one
    /// line rather than one line per frame.
    said: HashSet<String>,
    /// `Pixmap` keys written to stdout by this process (docs/abi.md, *Pixmap*).
    sent: Sent,
}

impl State {
    /// Build the picture and write it — but only when it is a different
    /// picture. A `New*` signal storm (some items rewrite their layout dozens of times a day) is a re-read either way, and this is what keeps it from also being that many frames.
    fn publish(&mut self) {
        let entries: Vec<Entry<'_>> = self
            .order
            .iter()
            .filter_map(|key| {
                self.items.get(key).map(|item| Entry {
                    key,
                    item,
                    menu: self.menus.get(key),
                })
            })
            .collect();
        // What did not fit is a fact about an item, not about a frame, so it
        // is said once and not once a second: an item's icon theme path is in every frame it appears in, and its menu may change many times a day.
        let mut said = std::mem::take(&mut self.said);
        let built = rows::build(&entries, &mut self.ids, self.revision + 1, &mut |line| {
            if said.insert(line.to_owned()) {
                witness(line);
            }
        });
        self.said = said;
        self.targets = built.targets;

        let same = self
            .last
            .as_ref()
            .is_some_and(|prev| prev.rows == built.rows.rows && prev.bytes == built.rows.bytes);
        if same {
            return;
        }
        self.revision += 1;

        let items = built
            .rows
            .rows
            .iter()
            .filter(|r| r.parent == caustics_bar_abi::NO_INDEX)
            .count();
        let rows_total = built.rows.rows.len();
        // The texels first, so the bar holds them before it reads the name —
        // what `caustics-apps` does with the `Pixmap` before the `App` that names
        // it, for the same reason.
        let owed = self.sent.owed(built.pixmaps);
        let pixmaps = owed.len();
        for frame in &owed {
            emit(frame);
        }
        emit(&Frame::Rows(built.rows.clone()));
        witness(&format!(
            "published rev={} items={items} rows={rows_total} bytes={} pixmaps={pixmaps}",
            self.revision,
            built.rows.body_len(),
        ));
        self.last = Some(built.rows);
    }

    /// One watcher event. The only place an item task is spawned or dropped.
    fn watcher_event(
        &mut self,
        event: WatcherEvent,
        conn: &Connection,
        out: &UnboundedSender<Msg>,
    ) {
        match event {
            WatcherEvent::Registered(service) => {
                let address = TrayAddress::parse(&service);
                let key = address.key();
                if self.tasks.contains_key(&key) {
                    return;
                }
                let (calls, rx) = unbounded_channel();
                let task = tokio::spawn(item::run(conn.clone(), address.clone(), out.clone(), rx));
                self.tasks.insert(
                    key.clone(),
                    ItemTask {
                        bus_name: address.bus_name,
                        calls,
                        task,
                    },
                );
                self.order.push(key);
            }
            WatcherEvent::Unregistered(service) => {
                let key = TrayAddress::parse(&service).key();
                if self.tasks.remove(&key).is_some() {
                    witness(&format!("{key} unregistered"));
                    self.forget(&key);
                }
            }
            WatcherEvent::OwnerGone(name) => {
                let gone: Vec<String> = self
                    .tasks
                    .iter()
                    .filter(|(_, task)| task.bus_name == name)
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in gone {
                    self.tasks.remove(&key);
                    witness(&format!("{key} vanished with its bus name"));
                    self.forget(&key);
                }
            }
            WatcherEvent::WatcherGone => {}
        }
    }

    /// One message from an item's task or from stdin.
    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Item { key, item } => {
                self.items.insert(key, *item);
                self.publish();
            }
            Msg::Menu { key, menu } => {
                self.menus.insert(key, *menu);
                self.publish();
            }
            Msg::MenuGone(key) => {
                if self.menus.remove(&key).is_some() {
                    self.publish();
                }
            }
            Msg::Gone(key) => {
                self.tasks.remove(&key);
                self.forget(&key);
            }
            Msg::Action(action) => self.act(&action),
        }
    }

    /// An item that is no longer there: it leaves the frame, and its task goes
    /// with it. Its **ids do not come back** — a reused id would turn a click
    /// that raced a frame into a click on some other item's row.
    fn forget(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.items.remove(key);
        self.menus.remove(key);
        self.publish();
    }

    /// The bar's decision, routed to the task that owns the item. The call
    /// never happens here: this loop must not wait on a service that hangs.
    fn act(&mut self, action: &ActionBody) {
        if rows::is_menu_notice(action.kind) {
            return;
        }
        match rows::dispatch(&self.targets, action) {
            Ok((key, call)) => match self.tasks.get(&key) {
                Some(task) => {
                    witness(&call.describe(&key));
                    if task.calls.send(call).is_err() {
                        witness(&format!("{key}: its task is gone; the action is dropped"));
                    }
                }
                None => witness(&format!("{key}: no task for it; the action is dropped")),
            },
            Err(why) => witness(&why),
        }
    }
}

/// One frame to stdout, or the exit that a closed or broken stdout means.
fn emit(frame: &Frame) {
    let stdout = std::io::stdout();
    if let Err(err) = wire::write(&mut stdout.lock(), frame) {
        if err.kind() == std::io::ErrorKind::BrokenPipe {
            witness("stdout closed; exiting");
            std::process::exit(0);
        }
        witness(&format!("stdout write failed: {err}"));
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// The bar holds the write end of our stdin, so its EOF is this process's
/// notice to leave — the one thing every sidecar must read (docs/abi.md, *Lifecycle*). Exiting
/// from the reading thread is honest: every frame left in one `write_all`, so
/// there is nothing to flush, and the loop below may be parked on a retry.
fn watch_stdin(out: UnboundedSender<Msg>) {
    let spawned = std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read(&mut input) {
                    Ok(None) => break,
                    Ok(Some(Frame::Action(action))) => {
                        if out.send(Msg::Action(action)).is_err() {
                            break;
                        }
                    }
                    Ok(Some(other)) => witness(&format!("ignoring a {} frame", other.kind())),
                    Err(err) => {
                        witness(&format!("stdin: {err}"));
                        break;
                    }
                }
            }
            witness("stdin closed; exiting");
            std::process::exit(0);
        });
    if let Err(e) = spawned {
        witness(&format!(
            "no stdin thread ({e}); the bar's exit will not be noticed"
        ));
    }
}

/// Wait, but keep taking messages: a retry must not stall the actions or the
/// item tasks that are still running.
async fn idle(state: &mut State, rx: &mut UnboundedReceiver<Msg>) {
    let deadline = tokio::time::sleep(RETRY);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return,
            msg = rx.recv() => match msg {
                Some(msg) => state.message(msg),
                None => return,
            },
        }
    }
}

async fn serve(tx: UnboundedSender<Msg>, mut rx: UnboundedReceiver<Msg>) {
    let mut state = State::default();
    // The startup frame, even with nothing on the bus: the bar learns the
    // applet exists and is empty, rather than learning nothing.
    state.publish();

    let mut connection: Option<Connection> = None;
    let mut said_absent = false;

    loop {
        // A session bus, retried until there is one. `absent:` is said once —
        // the bar's own witness vocabulary for an optional thing that is not
        // there — and the process keeps running, because a bus that appears
        // later is a tray that appears later.
        let conn = match connection.clone() {
            Some(conn) => conn,
            None => match Connection::session().await {
                Ok(conn) => {
                    witness("connected to the session bus");
                    said_absent = false;
                    connection = Some(conn.clone());
                    conn
                }
                Err(e) => {
                    if !said_absent {
                        witness("absent: session-bus");
                        witness(&format!("no session bus: {e}"));
                        said_absent = true;
                    }
                    idle(&mut state, &mut rx).await;
                    continue;
                }
            },
        };

        let (mut events, _tasks) = match watcher::establish(&conn).await {
            Ok(pair) => pair,
            Err(e) => {
                witness(&format!("no watcher ({e}); retrying in {RETRY:?}"));
                // The connection itself may be what broke; the next pass makes
                // a new one rather than retrying on a dead socket forever.
                connection = None;
                idle(&mut state, &mut rx).await;
                continue;
            }
        };

        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(WatcherEvent::WatcherGone) | None => break,
                    Some(event) => state.watcher_event(event, &conn, &tx),
                },
                msg = rx.recv() => match msg {
                    Some(msg) => state.message(msg),
                    // Every sender is gone: nothing can change the picture
                    // again, so there is nothing left to do.
                    None => return,
                },
            }
        }

        witness(&format!("the watcher went away; re-deciding in {RETRY:?}"));
        idle(&mut state, &mut rx).await;
    }
}

/// The whole program. Returns the process's exit status.
pub fn run<I: IntoIterator<Item = String>>(argv: I) -> i32 {
    for arg in argv {
        match arg.as_str() {
            "-h" | "--help" => {
                let _ = std::io::stderr().write_all(format!("{USAGE}\n").as_bytes());
                return 0;
            }
            other => {
                witness(&format!("`{other}` is not an option this takes"));
                let _ = std::io::stderr().write_all(format!("{USAGE}\n").as_bytes());
                return 2;
            }
        }
    }

    // Current-thread: one tray's worth of D-Bus traffic does not want a thread
    // pool, and a single thread keeps the ordering of the whole sidecar
    // obvious. Per-item tasks still isolate an item that hangs, because what
    // they isolate is a *service*, not a core.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            witness(&format!("no tokio runtime ({e}); there can be no tray"));
            witness("absent: session-bus");
            return 1;
        }
    };

    let (tx, rx) = unbounded_channel();
    watch_stdin(tx.clone());
    runtime.block_on(serve(tx, rx));
    witness("nothing left to publish; exiting");
    0
}
