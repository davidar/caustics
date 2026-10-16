//! `caustics-notify` against a private `dbus-daemon`:
//! a client on the bus calls the daemon the way `notify-send` does, the
//! daemon's frames are written with `wire::write` and read back with
//! `wire::read` — the wire's own structs, as the bar reads them — and actions
//! go in as the bar would send them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use caustics_bar_abi::buffer::APPLET_NOTIFY;
use caustics_bar_abi::wire::{
    self, ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_TOAST_SHOWN, ActionBody,
    Frame, RFLAG_ATTENTION, RFLAG_CHECKED, RFLAG_SHOW, RFLAG_SUBMENU, ROW_ITEM, Rows,
};
use caustics_notify::rows::{ICON_BELL, ICON_DND, ROW_CHIP, ROW_DND};
use caustics_notify::{ACTIVATION_GRACE, Config, Input, Sink, serve};
use caustics_sidecar::test_bus::Bus;
use futures_lite::StreamExt as _;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zbus::Connection;
use zbus::zvariant::Value;

const NAME: &str = "org.freedesktop.Notifications";

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    fn get_server_information(&self) -> zbus::Result<(String, String, String, String)>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

/// How long a test waits for something that should happen.
const WAIT: Duration = Duration::from_secs(5);

async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a connection to the private bus")
}

/// One daemon on the private bus, its frames decoded on a channel.
struct Daemon {
    frames: UnboundedReceiver<Frame>,
    actions: UnboundedSender<Input>,
    task: JoinHandle<i32>,
}

impl Daemon {
    async fn start(bus: &Bus, config: Config) -> Daemon {
        let conn = connect(bus).await;
        let (frames_tx, frames) = unbounded_channel();
        // Through the wire both ways: what the bar would read off the pipe.
        let sink: Sink = Box::new(move |frame| {
            let mut bytes = Vec::new();
            wire::write(&mut bytes, frame)?;
            let decoded = wire::read(&mut bytes.as_slice())?.expect("a whole frame");
            let _ = frames_tx.send(decoded);
            Ok(())
        });
        let (actions, rx) = unbounded_channel();
        let task = tokio::spawn(serve(conn, config, rx, sink));
        Daemon {
            frames,
            actions,
            task,
        }
    }

    /// Started, and serving: its first frame with a chip in it is the sign
    /// that it owns the name, so a `Notify` sent after this reaches it.
    async fn serving(bus: &Bus, config: Config) -> Daemon {
        let mut daemon = Daemon::start(bus, config).await;
        daemon.rows_until(|r| !r.rows.is_empty()).await;
        daemon
    }

    fn act(&self, row: u32, kind: u32) {
        let body = ActionBody {
            row,
            kind,
            ..ActionBody::default()
        };
        self.actions
            .send(Input::Action(body))
            .expect("the daemon is reading actions");
    }

    /// The bar's `REQ_ACTIVATION` for a click on `row` (docs/abi.md, *Request*).
    fn token(&self, row: u32, token: &str) {
        self.actions
            .send(Input::Activation {
                row,
                token: token.to_owned(),
            })
            .expect("the daemon is reading its stdin");
    }

    /// The next frame of any kind.
    async fn next(&mut self) -> Frame {
        tokio::time::timeout(WAIT, self.frames.recv())
            .await
            .expect("a frame in time")
            .expect("the daemon is running")
    }

    /// Rows frames until one satisfies `want`.
    async fn rows_until(&mut self, want: impl Fn(&Rows) -> bool) -> Rows {
        loop {
            if let Frame::Rows(rows) = self.next().await
                && want(&rows)
            {
                return rows;
            }
        }
    }

    /// Every frame that arrives within `period`.
    async fn quiet(&mut self, period: Duration) -> Vec<Frame> {
        let mut seen = Vec::new();
        let end = tokio::time::Instant::now() + period;
        while let Ok(Some(frame)) = tokio::time::timeout_at(end, self.frames.recv()).await {
            seen.push(frame);
        }
        seen
    }

    /// Close its stdin and wait for it to leave.
    async fn stop(self) -> i32 {
        drop(self.actions);
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("it exits on EOF")
            .expect("no panic")
    }
}

fn config(test: &str, minute: Duration) -> Config {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("caustics-notify-{test}"));
    let _ = std::fs::remove_dir_all(&dir);
    Config {
        dnd_path: dir.join("caustics/notify/dnd"),
        history_path: dir.join("caustics/notify/history.json"),
        minute,
        activation_grace: ACTIVATION_GRACE,
    }
}

fn minute() -> Duration {
    Duration::from_secs(60)
}

fn chip(rows: &Rows) -> &caustics_bar_abi::wire::Row {
    let chip = &rows.rows[0];
    assert_eq!(chip.role, ROW_ITEM);
    assert_eq!(chip.id, ROW_CHIP);
    chip
}

fn chip_label(rows: &Rows) -> Option<String> {
    chip(rows);
    rows.label(0).map(|l| l.into_owned())
}

fn row_index(rows: &Rows, id: u32) -> Option<usize> {
    rows.rows.iter().position(|r| r.id == id)
}

fn has_row(rows: &Rows, id: u32) -> bool {
    row_index(rows, id).is_some()
}

/// `notify-send Hello World`, near enough: its hints are `urgency` (a byte)
/// and `sender-pid`.
fn notify_send_hints() -> HashMap<&'static str, Value<'static>> {
    HashMap::from([("urgency", Value::U8(1)), ("sender-pid", Value::I64(4242))])
}

macro_rules! bus_or_skip {
    () => {
        match Bus::start() {
            Some(bus) => bus,
            None => {
                eprintln!("no dbus-daemon to start; untested here");
                return;
            }
        }
    };
}

#[tokio::test]
async fn a_notify_send_is_one_frame_with_a_count_and_a_row() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::start(&bus, config("notify-send", minute())).await;
    // Owning the name with nothing in the history: the chip, no count.
    let first = daemon.rows_until(|_| true).await;
    assert_eq!(first.applet, APPLET_NOTIFY);
    assert_eq!(chip_label(&first), None);
    assert_eq!(first.icon(0), Some(ICON_BELL));

    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let caps = proxy.get_capabilities().await.unwrap();
    assert_eq!(caps, ["body", "actions", "icon-static", "persistence"]);
    let info = proxy.get_server_information().await.unwrap();
    assert_eq!(
        (info.0.as_str(), info.1.as_str(), info.3.as_str()),
        ("caustics-notify", "caustics", "1.2")
    );

    let id = proxy
        .notify(
            "notify-send",
            0,
            "dialog-information",
            "Hello",
            "World",
            &[],
            notify_send_hints(),
            -1,
        )
        .await
        .unwrap();
    assert_eq!(id, 1, "ids start at 1");

    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    assert_eq!(chip_label(&rows).as_deref(), Some("1"));
    let i = row_index(&rows, id).unwrap();
    assert_eq!(rows.label(i).as_deref(), Some("Hello · now"));
    assert_eq!(
        rows.body(i).as_deref(),
        Some("World"),
        "the body is the row's second string"
    );
    assert_eq!(rows.icon(i), Some("dialog-information"));
    assert_ne!(
        rows.rows[i].flags & RFLAG_ATTENTION,
        0,
        "a fresh notification's row wears attention while it toasts"
    );
    // A fresh notification is a toast, and DND is off.
    assert_ne!(chip(&rows).flags & RFLAG_SHOW, 0);
    assert_eq!(
        chip(&rows).flags & RFLAG_ATTENTION,
        0,
        "attention on the chip is for critical"
    );

    assert_eq!(daemon.stop().await, 0);
}

#[tokio::test]
async fn replaces_id_keeps_the_row_id_and_close_removes_it() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("replace", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let mut closed = proxy.receive_notification_closed().await.unwrap();

    let a = proxy
        .notify("app", 0, "", "First", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    let b = proxy
        .notify("app", 0, "", "Second", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, a) && has_row(r, b)).await;
    assert_eq!(chip_label(&rows).as_deref(), Some("2"));
    // Newest first: b, then a.
    assert!(row_index(&rows, b) < row_index(&rows, a));

    let again = proxy
        .notify("app", a, "", "First, again", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    assert_eq!(again, a, "a replacement keeps its id");
    let rows = daemon
        .rows_until(|r| {
            row_index(r, a)
                .and_then(|i| r.label(i))
                .is_some_and(|l| l.starts_with("First, again"))
        })
        .await;
    assert!(row_index(&rows, b) < row_index(&rows, a), "and its place");

    // An id this daemon never gave is a new notification, not an error.
    let c = proxy
        .notify("app", 999, "", "Third", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    assert!(c != 999 && c > b);

    proxy.close_notification(a).await.unwrap();
    let rows = daemon.rows_until(|r| !has_row(r, a) && has_row(r, c)).await;
    assert_eq!(chip_label(&rows).as_deref(), Some("2"));
    let signal = tokio::time::timeout(WAIT, closed.next())
        .await
        .unwrap()
        .unwrap();
    let args = signal.args().unwrap();
    assert_eq!((args.id, args.reason), (a, 3), "closed by the caller");
}

#[tokio::test]
async fn image_data_is_a_pixmap_before_the_frame_that_names_it() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("image", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();

    // 3×2 RGB, rows padded to a stride of 12: the "any stride" case.
    let (w, h, stride) = (3, 2, 12);
    let mut data = vec![0u8; stride as usize * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            data[y * stride as usize + x * 3] = 0xff;
        }
    }
    let image = Value::from((w, h, stride, false, 8i32, 3i32, data));
    let hints = HashMap::from([("image-data", image)]);
    let id = proxy
        .notify("chat", 0, "chat-icon", "Avatar", "", &[], hints, -1)
        .await
        .unwrap();

    let mut key = None;
    loop {
        match daemon.next().await {
            Frame::Pixmap(pixmap) => {
                assert!(
                    pixmap.key.starts_with("caustics-notify:"),
                    "a minted key: {}",
                    pixmap.key
                );
                assert_eq!((pixmap.width, pixmap.height), (64, 64));
                assert_eq!(pixmap.rgba.len(), 64 * 64 * 4);
                key = Some(pixmap.key);
            }
            Frame::Rows(rows) if has_row(&rows, id) => {
                let key = key.expect("the Pixmap came first");
                // image-data beats app_icon.
                assert_eq!(rows.icon(row_index(&rows, id).unwrap()), Some(key.as_str()));
                break;
            }
            _ => {}
        }
    }
    // The same pixels again are the same key, and not sent twice.
    let hints = HashMap::from([(
        "image-data",
        Value::from((w, h, stride, false, 8i32, 3i32, {
            let mut d = vec![0u8; stride as usize * h as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    d[y * stride as usize + x * 3] = 0xff;
                }
            }
            d
        })),
    )]);
    let id2 = proxy
        .notify("chat", 0, "", "Again", "", &[], hints, -1)
        .await
        .unwrap();
    loop {
        match daemon.next().await {
            Frame::Pixmap(_) => panic!("a key already sent was sent again"),
            Frame::Rows(rows) if has_row(&rows, id2) => break,
            _ => {}
        }
    }
}

#[tokio::test]
async fn markup_is_stripped_and_the_body_keeps_its_lines() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("markup", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let id = proxy
        .notify(
            "mail",
            0,
            "file:///usr/share/icons/x.svg",
            "<b>Bold</b> news",
            "<i>Tom</i> &amp; <a href=\"x\">Jerry</a><br/>second line",
            &[],
            HashMap::new(),
            -1,
        )
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    let i = row_index(&rows, id).unwrap();
    assert_eq!(rows.label(i).as_deref(), Some("Bold news · now"));
    assert_eq!(
        rows.body(i).as_deref(),
        Some("Tom & Jerry\nsecond line"),
        "tags and entities gone, <br/> a line break"
    );
    assert_eq!(
        rows.icon(i),
        Some("/usr/share/icons/x.svg"),
        "file:// is stripped"
    );
}

#[tokio::test]
async fn an_action_row_invokes_and_closes() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("actions", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let mut invoked = proxy.receive_action_invoked().await.unwrap();
    let mut closed = proxy.receive_notification_closed().await.unwrap();

    let actions = ["default", "", "reply", "Reply", "mute", "Mute"];
    let id = proxy
        .notify("chat", 0, "", "Ping", "hi", &actions, HashMap::new(), -1)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    let i = row_index(&rows, id).unwrap();
    assert_ne!(
        rows.rows[i].flags & RFLAG_SUBMENU,
        0,
        "named actions make a submenu"
    );
    let children: Vec<(u32, String)> = rows
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.parent == i as u32)
        .map(|(j, r)| {
            (
                r.id,
                rows.label(j).map(|l| l.into_owned()).unwrap_or_default(),
            )
        })
        .collect();
    let labels: Vec<&str> = children.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(labels, ["Open", "Reply", "Mute", "", "Dismiss"]);
    let reply = children.iter().find(|(_, l)| l == "Reply").unwrap().0;

    daemon.act(reply, ACTION_ACTIVATE);
    let signal = tokio::time::timeout(WAIT, invoked.next())
        .await
        .unwrap()
        .unwrap();
    let args = signal.args().unwrap();
    assert_eq!((args.id, args.action_key.as_str()), (id, "reply"));
    let signal = tokio::time::timeout(WAIT, closed.next())
        .await
        .unwrap()
        .unwrap();
    let args = signal.args().unwrap();
    assert_eq!((args.id, args.reason), (id, 2), "dismissed by the user");
    let rows = daemon.rows_until(|r| !has_row(r, id)).await;
    assert_eq!(chip_label(&rows), None, "and the count is gone");

    // A row with only a default action: the click is the default, then closed.
    let id = proxy
        .notify(
            "chat",
            0,
            "",
            "Pong",
            "",
            &["default", "Open"],
            HashMap::new(),
            -1,
        )
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    assert_eq!(
        rows.rows[row_index(&rows, id).unwrap()].flags & RFLAG_SUBMENU,
        0
    );
    daemon.act(id, ACTION_ACTIVATE);
    let signal = tokio::time::timeout(WAIT, invoked.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(signal.args().unwrap().action_key, "default");
    let signal = tokio::time::timeout(WAIT, closed.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(signal.args().unwrap().id, id);

    // Resident: the default runs and the row stays.
    let hints = HashMap::from([("resident", Value::Bool(true))]);
    let id = proxy
        .notify(
            "music",
            0,
            "",
            "Playing",
            "",
            &["default", "Show"],
            hints,
            -1,
        )
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;
    daemon.act(id, ACTION_ACTIVATE);
    let signal = tokio::time::timeout(WAIT, invoked.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(signal.args().unwrap().id, id);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), closed.next())
            .await
            .is_err(),
        "a resident notification is not closed by its action"
    );

    // Clear all: every one closed, reason 2, and the menu says so.
    daemon.act(caustics_notify::rows::ROW_CLEAR, ACTION_ACTIVATE);
    let signal = tokio::time::timeout(WAIT, closed.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (signal.args().unwrap().id, signal.args().unwrap().reason),
        (id, 2)
    );
    let rows = daemon
        .rows_until(|r| has_row(r, caustics_notify::rows::ROW_EMPTY))
        .await;
    assert_eq!(chip_label(&rows), None);
}

#[tokio::test]
async fn do_not_disturb_persists_across_a_restart() {
    let bus = bus_or_skip!();
    let config = config("dnd", minute());
    let mut daemon = Daemon::start(&bus, config.clone()).await;
    let rows = daemon.rows_until(|_| true).await;
    let dnd = row_index(&rows, ROW_DND).unwrap();
    assert_eq!(rows.rows[dnd].flags & RFLAG_CHECKED, 0);

    daemon.act(ROW_DND, ACTION_ACTIVATE);
    let rows = daemon
        .rows_until(|r| row_index(r, ROW_DND).is_some_and(|i| r.rows[i].flags & RFLAG_CHECKED != 0))
        .await;
    assert_eq!(rows.icon(0), Some(ICON_DND));
    assert_eq!(
        std::fs::read_to_string(&config.dnd_path).unwrap().trim(),
        "1"
    );

    // DND suppresses the toast and nothing else: the count still counts.
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let id = proxy
        .notify("app", 0, "", "Quiet", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    assert_eq!(chip_label(&rows).as_deref(), Some("1"));
    assert_eq!(chip(&rows).flags & RFLAG_SHOW, 0);
    assert_eq!(daemon.stop().await, 0);

    let mut daemon = Daemon::start(&bus, config).await;
    let rows = daemon.rows_until(|_| true).await;
    let dnd = row_index(&rows, ROW_DND).unwrap();
    assert_ne!(
        rows.rows[dnd].flags & RFLAG_CHECKED,
        0,
        "DND came back from the file"
    );
    assert_eq!(rows.icon(0), Some(ICON_DND));
}

#[tokio::test]
async fn the_minute_resend_happens_only_while_the_menu_is_open() {
    let bus = bus_or_skip!();
    // A "minute" of a quarter second, so three of them fit in a test.
    let minute = Duration::from_millis(250);
    let mut daemon = Daemon::serving(&bus, config("tick", minute)).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    // A long toast, so no frame comes from its expiry inside the test.
    let id = proxy
        .notify("app", 0, "", "Tick", "", &[], HashMap::new(), 600_000)
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;

    let closed = daemon.quiet(minute * 4).await;
    assert!(
        closed.is_empty(),
        "frames while the menu was closed: {closed:?}"
    );

    daemon.act(ROW_CHIP, ACTION_MENU_OPEN);
    // Opening reads everything: the count goes.
    let rows = daemon.rows_until(|r| chip_label(r).is_none()).await;
    assert!(has_row(&rows, id));
    let open = daemon.quiet(minute * 4).await;
    let ages: Vec<String> = open
        .iter()
        .filter_map(|f| match f {
            Frame::Rows(rows) => row_index(rows, id)
                .and_then(|i| rows.label(i))
                .map(|l| l.into_owned()),
            _ => None,
        })
        .collect();
    assert!(
        ages.len() >= 2,
        "a re-send every minute while open: {ages:?}"
    );
    assert!(ages.iter().any(|l| l.ends_with("min ago")), "{ages:?}");

    daemon.act(ROW_CHIP, ACTION_MENU_CLOSE);
    // Whatever was in flight at the close, then silence.
    let _ = daemon.quiet(minute / 2).await;
    let after = daemon.quiet(minute * 4).await;
    assert!(after.is_empty(), "frames after the menu closed: {after:?}");
}

#[tokio::test]
async fn the_chip_shows_while_a_toast_lives_and_critical_is_attention() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("toast", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();

    let id = proxy
        .notify("app", 0, "", "Brief", "", &[], HashMap::new(), 300)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    assert_ne!(chip(&rows).flags & RFLAG_SHOW, 0);
    // The toast ends: a fresh frame without the bit, the count unchanged.
    let rows = daemon.rows_until(|r| chip(r).flags & RFLAG_SHOW == 0).await;
    assert_eq!(chip_label(&rows).as_deref(), Some("1"));

    let hints = HashMap::from([("urgency", Value::U8(2))]);
    let critical = proxy
        .notify("app", 0, "", "Battery", "", &[], hints, -1)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, critical)).await;
    assert_ne!(chip(&rows).flags & RFLAG_ATTENTION, 0);
    assert_ne!(
        chip(&rows).flags & RFLAG_SHOW,
        0,
        "a critical toast does not time out"
    );
    daemon.act(ROW_CHIP, ACTION_MENU_OPEN);
    let rows = daemon
        .rows_until(|r| chip(r).flags & RFLAG_ATTENTION == 0)
        .await;
    assert_eq!(
        chip(&rows).flags & RFLAG_SHOW,
        0,
        "read is the end of a critical toast"
    );

    // A transient one is shown and not kept.
    let mut closed = proxy.receive_notification_closed().await.unwrap();
    let hints = HashMap::from([("transient", Value::Bool(true))]);
    let transient = proxy
        .notify("app", 0, "", "Blip", "", &[], hints, 200)
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, transient)).await;
    daemon.rows_until(|r| !has_row(r, transient)).await;
    let signal = tokio::time::timeout(WAIT, closed.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (signal.args().unwrap().id, signal.args().unwrap().reason),
        (transient, 1)
    );
}

#[tokio::test]
async fn it_waits_for_the_name_and_publishes_when_it_is_released() {
    let bus = bus_or_skip!();
    // Another daemon holds the name without allowing replacement, as
    // cosmic-notifications does.
    let other = connect(&bus).await;
    other
        .request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
        .expect("the other daemon owns the name");

    let mut daemon = Daemon::start(&bus, config("wait", minute())).await;
    let early = daemon.quiet(Duration::from_millis(500)).await;
    assert!(
        early.is_empty(),
        "no chip while the name is another's: {early:?}"
    );

    other
        .release_name(NAME)
        .await
        .expect("the other daemon lets go");
    let rows = daemon.rows_until(|r| !r.rows.is_empty()).await;
    assert_eq!(rows.applet, APPLET_NOTIFY);
    assert_eq!(chip(&rows).id, ROW_CHIP);

    // And it serves now.
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let id = proxy
        .notify("app", 0, "", "Mine now", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;
}

// --- A click hands the app focus ---------------------------------------------

/// Every signal the daemon emits, in the order the bus delivered them, as
/// `(member, id, the second argument as text)`.
struct Signals(zbus::MessageStream);

impl Signals {
    async fn watch(client: &Connection) -> Signals {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(NAME)
            .unwrap()
            .build();
        Signals(
            zbus::MessageStream::for_match_rule(rule, client, None)
                .await
                .unwrap(),
        )
    }

    async fn next_within(&mut self, period: Duration) -> Option<(String, u32, String)> {
        let message = tokio::time::timeout(period, self.0.next())
            .await
            .ok()??
            .unwrap();
        let member = message.header().member().unwrap().to_string();
        let body = message.body();
        let decoded = match member.as_str() {
            "NotificationClosed" => {
                let (id, reason): (u32, u32) = body.deserialize().unwrap();
                (member, id, reason.to_string())
            }
            _ => {
                let (id, text): (u32, String) = body.deserialize().unwrap();
                (member, id, text)
            }
        };
        Some(decoded)
    }

    async fn next(&mut self) -> (String, u32, String) {
        self.next_within(WAIT).await.expect("a signal in time")
    }
}

fn signal(member: &str, id: u32, text: &str) -> (String, u32, String) {
    (member.to_owned(), id, text.to_owned())
}

/// A click whose token arrives: `ActivationToken` first, then
/// `ActionInvoked`, then `NotificationClosed` — the specification's order — on
/// the row the click was on, the notification's own or an action's.
#[tokio::test]
async fn a_click_with_a_token_emits_activation_token_before_action_invoked() {
    let bus = bus_or_skip!();
    // A grace long enough that the token below is never late on a busy host.
    let config = Config {
        activation_grace: Duration::from_secs(3),
        ..config("activation-token", minute())
    };
    let mut daemon = Daemon::serving(&bus, config).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let mut signals = Signals::watch(&client).await;

    let id = proxy
        .notify(
            "chat",
            0,
            "",
            "Ping",
            "",
            &["default", "Open"],
            HashMap::new(),
            -1,
        )
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;
    daemon.act(id, ACTION_ACTIVATE);
    // Nothing is said until the token comes: the click is held.
    assert_eq!(
        signals.next_within(Duration::from_millis(300)).await,
        None,
        "the action waits for its token"
    );
    // The row leaves the picture at the click, not at the token.
    let rows = daemon.rows_until(|r| !has_row(r, id)).await;
    assert_eq!(chip_label(&rows), None);
    daemon.token(id, "token-for-the-click");
    assert_eq!(
        signals.next().await,
        signal("ActivationToken", id, "token-for-the-click")
    );
    assert_eq!(signals.next().await, signal("ActionInvoked", id, "default"));
    assert_eq!(signals.next().await, signal("NotificationClosed", id, "2"));

    // An action row in the submenu: the token names that row, `(id << 8) | i`.
    let actions = ["default", "", "reply", "Reply"];
    let id = proxy
        .notify("chat", 0, "", "Pong", "", &actions, HashMap::new(), -1)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    let i = row_index(&rows, id).unwrap() as u32;
    let reply = rows
        .rows
        .iter()
        .enumerate()
        .find(|(j, r)| r.parent == i && rows.label(*j).as_deref() == Some("Reply"))
        .map(|(_, r)| r.id)
        .expect("a Reply row");
    daemon.act(reply, ACTION_ACTIVATE);
    // A token for another row is not this click's.
    daemon.token(id, "a-token-for-the-row-itself");
    daemon.token(reply, "token-for-reply");
    assert_eq!(
        signals.next().await,
        signal("ActivationToken", id, "token-for-reply")
    );
    assert_eq!(signals.next().await, signal("ActionInvoked", id, "reply"));
    assert_eq!(signals.next().await, signal("NotificationClosed", id, "2"));
    assert_eq!(
        signals.next_within(Duration::from_millis(300)).await,
        None,
        "nothing more"
    );
    assert_eq!(daemon.stop().await, 0);
}

/// A click whose token never comes — a compositor without the global, or one
/// that is slow — still invokes, once the grace has passed and no later; a
/// token that turns up after that is dropped, not emitted.
///
/// On a 3 s grace rather than the binary's 100 ms: the
/// test measures real time on a bus, and a bound a tenth of a second wide
/// flakes on a loaded host. Three seconds either side of nothing is a wide
/// margin, and the rule under test does not depend on the length.
#[tokio::test]
async fn a_click_with_no_token_still_invokes_after_the_grace() {
    const GRACE: Duration = Duration::from_secs(3);
    let bus = bus_or_skip!();
    let config = Config {
        activation_grace: GRACE,
        ..config("activation-none", minute())
    };
    let mut daemon = Daemon::serving(&bus, config).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let mut signals = Signals::watch(&client).await;

    let id = proxy
        .notify(
            "chat",
            0,
            "",
            "Ping",
            "",
            &["default", "Open"],
            HashMap::new(),
            -1,
        )
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;
    let clicked = std::time::Instant::now();
    daemon.act(id, ACTION_ACTIVATE);
    assert_eq!(
        signals.next_within(GRACE * 3).await,
        Some(signal("ActionInvoked", id, "default"))
    );
    let waited = clicked.elapsed();
    assert!(
        waited >= GRACE - Duration::from_millis(10),
        "invoked after {waited:?}, before the grace"
    );
    assert!(
        waited < GRACE * 2,
        "invoked {waited:?} after the click; the grace is {GRACE:?}"
    );
    assert_eq!(signals.next().await, signal("NotificationClosed", id, "2"));
    daemon.token(id, "too-late");
    assert_eq!(
        signals.next_within(Duration::from_millis(300)).await,
        None,
        "a late token is dropped"
    );

    // A dismissal invokes nothing, so it waits for nothing.
    let id = proxy
        .notify("chat", 0, "", "Gone", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;
    let clicked = std::time::Instant::now();
    daemon.act(id, ACTION_ACTIVATE);
    assert_eq!(signals.next().await, signal("NotificationClosed", id, "2"));
    assert!(
        clicked.elapsed() < GRACE,
        "a dismissal is not held ({:?})",
        clicked.elapsed()
    );
    assert_eq!(daemon.stop().await, 0);
}

// --- The toast tells the truth ----------------------------------------------

fn attention(rows: &Rows, id: u32) -> bool {
    row_index(rows, id).is_some_and(|i| rows.rows[i].flags & RFLAG_ATTENTION != 0)
}

/// A row's attention is its toast: on while the notification toasts, and off
/// in a frame the daemon sends **on the deadline**, with the menu never
/// opened — the count still counting it.
#[tokio::test]
async fn attention_follows_the_toast_and_leaves_on_its_deadline() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("attention-deadline", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let id = proxy
        .notify("app", 0, "", "Brief", "", &[], HashMap::new(), 400)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, id)).await;
    assert!(attention(&rows, id), "a fresh toast wears attention");
    assert_ne!(chip(&rows).flags & RFLAG_SHOW, 0);
    let rows = daemon.rows_until(|r| !attention(r, id)).await;
    assert_eq!(
        chip(&rows).flags & RFLAG_SHOW,
        0,
        "the chip stops asking in the same frame"
    );
    assert_eq!(chip_label(&rows).as_deref(), Some("1"), "unread still");
    // Nothing more: no frame re-toasts it.
    let after = daemon.quiet(Duration::from_millis(600)).await;
    assert!(after.is_empty(), "frames after the deadline: {after:?}");

    // A second one toasts alone; the first is never attention again.
    let next = proxy
        .notify("app", 0, "", "Next", "", &[], HashMap::new(), 10_000)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, next)).await;
    assert!(attention(&rows, next));
    assert!(
        !attention(&rows, id),
        "a shown notification never toasts again"
    );
    assert_eq!(chip_label(&rows).as_deref(), Some("2"));
}

/// A toast that ended marks **only its rows** shown. A click on one row
/// of a toast acts on that one and marks nothing else; the bar's
/// `ACTION_TOAST_SHOWN` for each row the toast showed is what retires them,
/// and a notification that was toasting but not in the toast keeps asking.
#[tokio::test]
async fn a_toast_that_ended_marks_only_its_rows_shown() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("toast-click", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let mut ids = Vec::new();
    for summary in ["First", "Second", "Third"] {
        ids.push(
            proxy
                .notify("app", 0, "", summary, "", &[], HashMap::new(), 10_000)
                .await
                .unwrap(),
        );
    }
    let rows = daemon
        .rows_until(|r| ids.iter().all(|id| has_row(r, *id)))
        .await;
    assert!(ids.iter().all(|id| attention(&rows, *id)), "three toasting");

    // The toast showed the first two; the click is on the second.
    daemon.act(ids[1], ACTION_ACTIVATE);
    let rows = daemon.rows_until(|r| !has_row(r, ids[1])).await;
    assert!(
        attention(&rows, ids[0]) && attention(&rows, ids[2]),
        "a click marks nothing but its own row"
    );
    assert_ne!(chip(&rows).flags & RFLAG_SHOW, 0, "the chip still asks");

    // The toast ends, and the bar names what it showed: the first, and the
    // second, which the click already took out of the history.
    daemon.act(ids[0], ACTION_TOAST_SHOWN);
    daemon.act(ids[1], ACTION_TOAST_SHOWN);
    let rows = daemon.rows_until(|r| !attention(r, ids[0])).await;
    assert!(
        attention(&rows, ids[2]),
        "the one the toast did not show still asks"
    );
    assert_ne!(chip(&rows).flags & RFLAG_SHOW, 0, "and the chip with it");
    assert_eq!(chip_label(&rows).as_deref(), Some("2"), "both still unread");

    // Its own toast ends: nothing asks, and nothing else is sent.
    daemon.act(ids[2], ACTION_TOAST_SHOWN);
    let rows = daemon.rows_until(|r| !attention(r, ids[2])).await;
    assert_eq!(
        chip(&rows).flags & RFLAG_SHOW,
        0,
        "the chip asks for no toast"
    );
    assert_eq!(chip_label(&rows).as_deref(), Some("2"), "shown is not read");
    let after = daemon.quiet(Duration::from_millis(500)).await;
    assert!(after.is_empty(), "frames after the last toast: {after:?}");

    // A right click dismisses without invoking anything.
    let mut invoked = proxy.receive_action_invoked().await.unwrap();
    let with_default = proxy
        .notify(
            "app",
            0,
            "",
            "Has a default",
            "",
            &["default", "Open"],
            HashMap::new(),
            -1,
        )
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, with_default)).await;
    daemon.act(with_default, wire::ACTION_SECONDARY);
    daemon.rows_until(|r| !has_row(r, with_default)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), invoked.next())
            .await
            .is_err(),
        "a dismissal invokes no action"
    );
}

/// A notification that arrives while a toast is up is held behind it by
/// the bar and was never on screen. When that toast ends, the bar names the
/// rows it showed; the late one is not among them, so it still asks and is
/// the next toast. (Were a click to mark every toasting entry shown, a chat
/// burst clicked within seconds would lose the second message's toast.)
#[tokio::test]
async fn a_row_that_arrived_after_the_toast_opened_is_toasted_next_time() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("toast-late", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let first = proxy
        .notify("chat", 0, "", "First", "", &[], HashMap::new(), 10_000)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, first)).await;
    assert!(attention(&rows, first), "the toast opens on the first");

    // The second arrives while that toast is up.
    let late = proxy
        .notify("chat", 0, "", "Second", "", &[], HashMap::new(), 10_000)
        .await
        .unwrap();
    let rows = daemon.rows_until(|r| has_row(r, late)).await;
    assert!(attention(&rows, late) && attention(&rows, first));

    // A click on the toast's row, and the toast's end naming only that row.
    daemon.act(first, ACTION_ACTIVATE);
    daemon.act(first, ACTION_TOAST_SHOWN);
    let rows = daemon.rows_until(|r| !has_row(r, first)).await;
    assert!(
        attention(&rows, late),
        "the late one was never shown and still asks"
    );
    assert_ne!(
        chip(&rows).flags & RFLAG_SHOW,
        0,
        "so the chip asks for its toast"
    );

    // Its own toast, and its end.
    daemon.act(late, ACTION_TOAST_SHOWN);
    let rows = daemon.rows_until(|r| !attention(r, late)).await;
    assert_eq!(chip(&rows).flags & RFLAG_SHOW, 0, "nothing left to toast");
    assert!(has_row(&rows, late), "shown, and still in the menu");
}

/// `CloseNotification` of an id this daemon does not hold answers with the
/// specification's error, and one it holds still closes with reason 3.
#[tokio::test]
async fn closing_an_unknown_id_is_an_error() {
    let bus = bus_or_skip!();
    let mut daemon = Daemon::serving(&bus, config("close-unknown", minute())).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let err = proxy
        .close_notification(4242)
        .await
        .expect_err("an unknown id");
    eprintln!("CloseNotification(4242): {err}");
    assert!(
        matches!(&err, zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.Failed"),
        "{err:?}"
    );
    let id = proxy
        .notify("app", 0, "", "Mine", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;
    proxy
        .close_notification(id)
        .await
        .expect("a held id closes");
    daemon.rows_until(|r| !has_row(r, id)).await;
    // Closed already: an error again.
    assert!(proxy.close_notification(id).await.is_err());
}

// --- The click's signals go to the sender ----------------------------------

/// `ActivationToken` and `ActionInvoked` reach the
/// connection that sent the `Notify` and no other. A second client with the
/// same match rule — every notifying app has one, and matches by id alone —
/// sees only the broadcast `NotificationClosed`.
#[tokio::test]
async fn a_clicks_signals_reach_its_sender_alone() {
    let bus = bus_or_skip!();
    let config = Config {
        activation_grace: Duration::from_secs(3),
        ..config("directed-signals", minute())
    };
    let mut daemon = Daemon::serving(&bus, config).await;
    let sender = connect(&bus).await;
    let bystander = connect(&bus).await;
    let proxy = NotificationsProxy::new(&sender).await.unwrap();
    let mut own = Signals::watch(&sender).await;
    let mut others = Signals::watch(&bystander).await;

    let id = proxy
        .notify(
            "chat",
            0,
            "",
            "Ping",
            "",
            &["default", "Open"],
            HashMap::new(),
            -1,
        )
        .await
        .unwrap();
    daemon.rows_until(|r| has_row(r, id)).await;
    daemon.act(id, ACTION_ACTIVATE);
    daemon.rows_until(|r| !has_row(r, id)).await;
    daemon.token(id, "token-for-the-sender");
    assert_eq!(
        own.next().await,
        signal("ActivationToken", id, "token-for-the-sender")
    );
    assert_eq!(own.next().await, signal("ActionInvoked", id, "default"));
    assert_eq!(own.next().await, signal("NotificationClosed", id, "2"));
    // The bystander holds the same match rule and sees only the close: a
    // directed signal never reaches it, whatever id it remembers.
    assert_eq!(others.next().await, signal("NotificationClosed", id, "2"));
    assert_eq!(others.next_within(Duration::from_millis(300)).await, None);
}

// --- The daemon remembers across restarts ------------------------------------

/// A second daemon on the same state file takes
/// up where the first left off — the same notifications, unread and counted
/// but toasting nothing, the image's `Pixmap` sent again to the bar, and ids
/// that continue rather than start at 1 again.
#[tokio::test]
async fn a_restarted_daemon_keeps_its_history_and_its_ids() {
    let bus = bus_or_skip!();
    let config = config("restart", minute());
    let mut daemon = Daemon::serving(&bus, config.clone()).await;
    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();

    let first = proxy
        .notify("app", 0, "", "First", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    let image = Value::from((2i32, 2i32, 8i32, true, 8i32, 4i32, vec![0x40u8; 16]));
    let second = proxy
        .notify(
            "chat",
            0,
            "",
            "Avatar",
            "hi",
            &["default", "Open"],
            HashMap::from([("image-data", image)]),
            -1,
        )
        .await
        .unwrap();
    assert_eq!((first, second), (1, 2));
    let mut key = None;
    loop {
        match daemon.next().await {
            Frame::Pixmap(pixmap) => key = Some(pixmap.key),
            Frame::Rows(rows) if has_row(&rows, first) && has_row(&rows, second) => break,
            _ => {}
        }
    }
    let key = key.expect("the first daemon sent the image");
    assert_eq!(daemon.stop().await, 0);
    assert!(config.history_path.exists(), "the history was written");

    let mut daemon = Daemon::start(&bus, config.clone()).await;
    let mut pixmaps = Vec::new();
    let rows = loop {
        match daemon.next().await {
            Frame::Pixmap(pixmap) => pixmaps.push(pixmap.key),
            Frame::Rows(rows) if !rows.rows.is_empty() => break rows,
            _ => {}
        }
    };
    assert_eq!(pixmaps, [key.as_str()], "the image goes to the bar again");
    assert_eq!(chip_label(&rows).as_deref(), Some("2"), "both still unread");
    assert_eq!(
        chip(&rows).flags & RFLAG_SHOW,
        0,
        "a restart toasts nothing"
    );
    assert!(
        !attention(&rows, first) && !attention(&rows, second),
        "no row wears attention"
    );
    assert!(
        row_index(&rows, second) < row_index(&rows, first),
        "newest first"
    );
    let i = row_index(&rows, second).unwrap();
    assert_eq!(rows.icon(i), Some(key.as_str()));
    assert_eq!(rows.body(i).as_deref(), Some("hi"));

    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let third = proxy
        .notify("app", 0, "", "Third", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    assert_eq!(third, 3, "the ids continue");
    let rows = daemon.rows_until(|r| has_row(r, third)).await;
    assert!(attention(&rows, third), "a new one toasts as ever");
    proxy
        .close_notification(first)
        .await
        .expect("a restored id closes");
    daemon.rows_until(|r| !has_row(r, first)).await;
    assert_eq!(daemon.stop().await, 0);

    let saved = caustics_notify::read_history(&config.history_path)
        .unwrap()
        .expect("a history file");
    let ids: Vec<u32> = saved.entries.iter().map(|e| e.id).collect();
    assert_eq!((saved.next_id, ids), (4, vec![third, second]));
}

/// A history file that does not parse starts an empty daemon, and the first
/// write replaces it.
#[tokio::test]
async fn a_garbled_history_file_starts_empty_and_is_replaced() {
    let bus = bus_or_skip!();
    let config = config("garbled", minute());
    std::fs::create_dir_all(config.history_path.parent().unwrap()).unwrap();
    std::fs::write(&config.history_path, b"{\"version\": 1, \"next_id\": ").unwrap();

    let mut daemon = Daemon::start(&bus, config.clone()).await;
    let rows = daemon.rows_until(|r| !r.rows.is_empty()).await;
    assert_eq!(chip_label(&rows), None);
    assert!(has_row(&rows, caustics_notify::rows::ROW_EMPTY));
    let saved = caustics_notify::read_history(&config.history_path)
        .expect("the garbage is gone")
        .expect("a history file");
    assert_eq!((saved.next_id, saved.entries.len()), (1, 0));

    let client = connect(&bus).await;
    let proxy = NotificationsProxy::new(&client).await.unwrap();
    let id = proxy
        .notify("app", 0, "", "Fresh", "", &[], HashMap::new(), -1)
        .await
        .unwrap();
    assert_eq!(id, 1);
    daemon.rows_until(|r| has_row(r, id)).await;
    let saved = caustics_notify::read_history(&config.history_path)
        .unwrap()
        .unwrap();
    assert_eq!(saved.entries.len(), 1);
    assert_eq!(daemon.stop().await, 0);

    // A file of another version is likewise ignored and replaced.
    std::fs::write(&config.history_path, b"{\"version\": 2}").unwrap();
    assert!(caustics_notify::read_history(&config.history_path).is_err());
    let mut daemon = Daemon::start(&bus, config.clone()).await;
    let rows = daemon.rows_until(|r| !r.rows.is_empty()).await;
    assert_eq!(chip_label(&rows), None);
    assert_eq!(daemon.stop().await, 0);
    assert!(caustics_notify::read_history(&config.history_path).is_ok());
}
