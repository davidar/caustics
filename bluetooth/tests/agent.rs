//! The agent object on a private `dbus-daemon`: a peer owns `org.bluez`,
//! serves a device whose `Alias` is the title, and calls the agent's methods
//! as BlueZ would; the test plays the sidecar's loop, handing each event to
//! the conductor and clicking its rows, and a socket plays the canvas's
//! pairing sink. No system bus is involved, and nothing pairs.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use caustics_bluetooth::agent::{self, Event};
use caustics_bluetooth::bluez::SERVICE;
use caustics_bluetooth::pairing::{Effect, Pairing};
use caustics_bluetooth::rows::{ROW_CONFIRM, ROW_REJECT};
use caustics_bluetooth::sink;
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use zbus::Connection;
use zbus::zvariant::ObjectPath;

const WAIT: Duration = Duration::from_secs(10);
const PHONE: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_40";

#[zbus::proxy(
    interface = "org.bluez.Agent1",
    default_path = "/caustics/bluetooth/agent"
)]
trait Agent {
    fn request_confirmation(&self, device: &ObjectPath<'_>, passkey: u32) -> zbus::Result<()>;
    fn request_pin_code(&self, device: &ObjectPath<'_>) -> zbus::Result<String>;
    fn cancel(&self) -> zbus::Result<()>;
}

/// The device BlueZ would serve: an alias.
struct Device;

#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn alias(&self) -> String {
        "Pixel 9".to_owned()
    }
}

async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a connection to the private bus")
}

struct Rig {
    _bus: Bus,
    bluez: Connection,
    agent_name: String,
    events: UnboundedReceiver<Event>,
    _agent: Connection,
}

async fn rig() -> Option<Rig> {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon on this machine; skipping");
        return None;
    };
    let bluez = connect(&bus).await;
    bluez.object_server().at(PHONE, Device).await.unwrap();
    bluez.request_name(SERVICE).await.unwrap();
    let conn = connect(&bus).await;
    let (tx, events) = unbounded_channel();
    agent::serve(&conn, true, tx).await.unwrap();
    Some(Rig {
        agent_name: conn.unique_name().unwrap().to_string(),
        _bus: bus,
        bluez,
        events,
        _agent: conn,
    })
}

impl Rig {
    async fn proxy(&self, from: &Connection) -> AgentProxy<'static> {
        AgentProxy::builder(from)
            .destination(self.agent_name.clone())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    async fn event(&mut self) -> Event {
        tokio::time::timeout(WAIT, self.events.recv())
            .await
            .expect("an event in time")
            .expect("the agent is up")
    }
}

fn phone() -> ObjectPath<'static> {
    ObjectPath::from_static_str_unchecked(PHONE)
}

fn error_name(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_owned(),
        other => other.to_string(),
    }
}

#[tokio::test]
async fn confirm_returns_reject_is_rejected_and_cancel_is_canceled() {
    let Some(mut rig) = rig().await else { return };
    let proxy = rig.proxy(&rig.bluez).await;
    let mut pairing = Pairing::new();

    for (click, want) in [
        (Some(ROW_CONFIRM), None),
        (Some(ROW_REJECT), Some("org.bluez.Error.Rejected")),
        (None, Some("org.bluez.Error.Canceled")),
    ] {
        let call = {
            let proxy = proxy.clone();
            tokio::spawn(async move { proxy.request_confirmation(&phone(), 123_456).await })
        };
        let event = rig.event().await;
        match &event {
            Event::Ask { title, .. } => assert_eq!(title, "Pixel 9", "the title is BlueZ's alias"),
            other => panic!("{other:?}"),
        }
        assert!(pairing.event(event, Instant::now()).is_empty());
        assert_eq!(
            pairing.view().question.as_deref(),
            Some("Confirm passkey 123 456 for Pixel 9")
        );
        match click {
            Some(row) => {
                pairing.click(row);
            }
            None => {
                proxy.cancel().await.expect("Cancel returns");
                let cancel = rig.event().await;
                assert!(matches!(cancel, Event::Cancel));
                pairing.event(cancel, Instant::now());
            }
        }
        let result = tokio::time::timeout(WAIT, call).await.unwrap().unwrap();
        match want {
            None => result.expect("a confirmation returns"),
            Some(name) => assert_eq!(error_name(&result.unwrap_err()), name),
        }
        assert!(!pairing.view().showing());
    }
}

#[tokio::test]
async fn only_bluez_may_ask() {
    let Some(mut rig) = rig().await else { return };
    let other = connect(&rig._bus).await;
    let proxy = rig.proxy(&other).await;
    let err = proxy
        .request_confirmation(&phone(), 1)
        .await
        .expect_err("a caller that is not BlueZ is refused");
    assert_eq!(error_name(&err), "org.bluez.Error.Rejected");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rig.events.recv())
            .await
            .is_err(),
        "and the loop never heard of it"
    );
}

fn scratch(name: &str) -> PathBuf {
    // A socket path must fit `sun_path` (108 bytes): the system's temporary
    // directory, as the lock's and the pairing sink's tests use it.
    let dir = std::env::temp_dir().join(format!("caustics-bt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(sink::SOCKET)
}

/// A sink that answers `status` with `ok asking=no` and each `pin` or
/// `passkey` with `reply`, and records the lines it read.
fn fake_sink(path: &PathBuf, reply: &'static str) -> std::sync::mpsc::Receiver<String> {
    let listener = UnixListener::bind(path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let answer = if line.starts_with("status") {
                "ok asking=no\n"
            } else if line.starts_with("cancel") {
                "ok\n"
            } else {
                reply
            };
            let _ = tx.send(line);
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    rx
}

#[tokio::test]
async fn a_pin_is_asked_of_the_sink_and_its_text_is_bluez_s_reply() {
    let Some(mut rig) = rig().await else { return };
    let path = scratch("pin");
    let lines = fake_sink(&path, "text 0000\n");
    assert!(sink::answers(&path).await, "the sink answers status");
    assert_eq!(lines.recv().unwrap(), "status\n");
    assert_eq!(agent::capability(true), agent::KEYBOARD_DISPLAY);

    let proxy = rig.proxy(&rig.bluez).await;
    let call = tokio::spawn(async move { proxy.request_pin_code(&phone()).await });
    let mut pairing = Pairing::new();
    let effects = pairing.event(rig.event().await, Instant::now());
    let Some(Effect::Ask { id, typed, title }) = effects.into_iter().next() else {
        panic!("a typed question asks the sink");
    };
    let reply = sink::ask(&path, typed, &title).await;
    assert_eq!(lines.recv().unwrap(), "pin Pixel 9\n");
    pairing.typed(id, reply);
    let pin = tokio::time::timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(pin.expect("the PIN"), "0000");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn no_sink_is_display_yes_no_and_a_pin_is_rejected() {
    let Some(mut rig) = rig().await else { return };
    let path = scratch("none").with_file_name("nobody.sock");
    assert!(!sink::answers(&path).await);
    assert_eq!(agent::capability(false), agent::DISPLAY_YES_NO);

    let proxy = rig.proxy(&rig.bluez).await;
    let call = tokio::spawn(async move { proxy.request_pin_code(&phone()).await });
    let mut pairing = Pairing::new();
    let effects = pairing.event(rig.event().await, Instant::now());
    let Some(Effect::Ask { id, typed, title }) = effects.into_iter().next() else {
        panic!("a typed question asks the sink");
    };
    let reply = sink::ask(&path, typed, &title).await;
    assert!(matches!(reply, sink::Reply::Unreachable(_)), "{reply:?}");
    pairing.typed(id, reply);
    let err = tokio::time::timeout(WAIT, call)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error_name(&err), "org.bluez.Error.Rejected");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_title_is_one_line_and_fits() {
    assert_eq!(sink::line_title("Pixel\n9\0"), "Pixel 9");
    assert_eq!(sink::line_title("\n"), "a device");
    let long = "é".repeat(400);
    let cut = sink::line_title(&long);
    assert!(cut.len() + "passkey \n".len() <= sink::MAX_LINE);
}
