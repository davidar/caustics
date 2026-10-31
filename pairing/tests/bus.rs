//! The agent object on a private `dbus-daemon`: a peer owns `org.bluez`,
//! serves a device whose `Alias` is the title, and calls the agent's methods
//! as BlueZ would; the frames come out as the host would read them, and the
//! method returns what BlueZ would see. No system bus is involved.

use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, Frame, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_INFO, PROMPT_INFO_TITLE, Prompt,
    Secret,
};
use caustics_pairing::agent::SERVICE;
use caustics_pairing::ask;
use caustics_pairing::conversation::{Event, Sink};
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zbus::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::ObjectPath;

const WAIT: Duration = Duration::from_secs(10);
const BUDS: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_20";
const KEYBOARD: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_30";

#[zbus::proxy(interface = "org.bluez.Agent1", default_path = "/caustics/pairing")]
trait Agent {
    fn request_confirmation(&self, device: &ObjectPath<'_>, passkey: u32) -> zbus::Result<()>;
    fn request_authorization(&self, device: &ObjectPath<'_>) -> zbus::Result<()>;
    fn authorize_service(&self, device: &ObjectPath<'_>, uuid: &str) -> zbus::Result<()>;
    fn request_pin_code(&self, device: &ObjectPath<'_>) -> zbus::Result<String>;
    fn request_passkey(&self, device: &ObjectPath<'_>) -> zbus::Result<u32>;
    fn display_passkey(
        &self,
        device: &ObjectPath<'_>,
        passkey: u32,
        entered: u16,
    ) -> zbus::Result<()>;
    fn cancel(&self) -> zbus::Result<()>;
    fn release(&self) -> zbus::Result<()>;
}

/// The device BlueZ would serve: an alias, and `Paired`.
struct Device {
    alias: String,
    paired: bool,
}

#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn alias(&self) -> String {
        self.alias.clone()
    }

    #[zbus(property)]
    fn paired(&self) -> bool {
        self.paired
    }
}

async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a connection to the private bus")
}

/// A connection that owns `org.bluez` and serves the buds.
async fn bluez(bus: &Bus) -> Connection {
    let conn = connect(bus).await;
    conn.object_server()
        .at(
            BUDS,
            Device {
                alias: "Example Buds".to_owned(),
                paired: false,
            },
        )
        .await
        .expect("the device is served");
    conn.request_name(SERVICE).await.expect("the name is ours");
    conn
}

struct Served {
    name: String,
    events: UnboundedSender<Event>,
    frames: UnboundedReceiver<Frame>,
    task: JoinHandle<i32>,
}

async fn serve(bus: &Bus, check_caller: bool) -> Served {
    let conn = connect(bus).await;
    let name = conn.unique_name().expect("a unique name").to_string();
    let (events, inbox) = unbounded_channel();
    let (frames_tx, frames) = unbounded_channel();
    let sink: Sink = Box::new(move |frame| {
        let _ = frames_tx.send(frame.clone());
        Ok(())
    });
    let for_task = events.clone();
    let task = tokio::spawn(async move {
        caustics_pairing::serve(&conn, check_caller, for_task, sink, inbox).await
    });
    Served {
        name,
        events,
        frames,
        task,
    }
}

impl Served {
    async fn prompt(&mut self) -> Prompt {
        match tokio::time::timeout(WAIT, self.frames.recv()).await {
            Ok(Some(Frame::Prompt(prompt))) => prompt,
            other => panic!("no prompt: {other:?}"),
        }
    }

    async fn expect(&mut self, kind: u32, arg: u32, message: &str) -> u32 {
        let prompt = self.prompt().await;
        assert_eq!(
            (prompt.kind, prompt.arg, prompt.message.as_str()),
            (kind, arg, message),
            "{prompt:?}"
        );
        prompt.id
    }

    fn answer(&self, id: u32, text: &str) {
        self.events
            .send(Event::Answer(Answer::Text {
                id,
                bytes: Secret::new(text.as_bytes().to_vec()),
            }))
            .unwrap();
    }

    fn dismiss(&self, id: u32) {
        self.events
            .send(Event::Answer(Answer::Cancelled { id }))
            .unwrap();
    }

    async fn close(self) {
        let _ = self.events.send(Event::Closed);
        let _ = tokio::time::timeout(WAIT, self.task).await;
    }
}

async fn agent(conn: &Connection, served: &Served) -> AgentProxy<'static> {
    AgentProxy::builder(conn)
        .destination(served.name.clone())
        .expect("a destination")
        .build()
        .await
        .expect("a proxy")
}

fn path(device: &str) -> ObjectPath<'static> {
    ObjectPath::try_from(device.to_owned()).expect("a device path")
}

fn error_name<T: std::fmt::Debug>(result: zbus::Result<T>) -> String {
    match result {
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        other => panic!("not a method error: {other:?}"),
    }
}

async fn done<T>(call: JoinHandle<T>) -> T {
    tokio::time::timeout(WAIT, call)
        .await
        .expect("the call returns in time")
        .expect("the call's task")
}

#[tokio::test]
async fn bluez_s_questions_are_conversations_and_their_answers_its_replies() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipped");
        return;
    };
    let bluez = bluez(&bus).await;
    let mut served = serve(&bus, true).await;
    let agent = agent(&bluez, &served).await;

    // A confirmation, titled by the device's alias, confirmed.
    let call = {
        let agent = agent.clone();
        tokio::spawn(async move { agent.request_confirmation(&path(BUDS), 42).await })
    };
    let id = served
        .expect(PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    served
        .expect(PROMPT_INFO, 0, "Confirm passkey 000042")
        .await;
    served.expect(PROMPT_ECHO, 0, ask::CONFIRM).await;
    served.answer(id, "");
    served.expect(PROMPT_DONE, 1, "").await;
    done(call).await.expect("a confirmation confirmed returns");

    // Dismissed: Rejected.
    let call = {
        let agent = agent.clone();
        tokio::spawn(async move {
            agent
                .authorize_service(&path(BUDS), "0000110b-0000-1000-8000-00805f9b34fb")
                .await
        })
    };
    let id = served
        .expect(PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    served.expect(PROMPT_INFO, 0, "Allow Audio Sink").await;
    served.expect(PROMPT_ECHO, 0, ask::CONFIRM).await;
    served.dismiss(id);
    served.expect(PROMPT_DONE, 0, "").await;
    assert_eq!(error_name(done(call).await), "org.bluez.Error.Rejected");

    // A PIN, for a device whose alias cannot be read: the address is the
    // title, and the PIN is the reply.
    let call = {
        let agent = agent.clone();
        tokio::spawn(async move { agent.request_pin_code(&path(KEYBOARD)).await })
    };
    let id = served
        .expect(PROMPT_INFO, PROMPT_INFO_TITLE, "02:5E:00:00:0B:30")
        .await;
    served.expect(PROMPT_ECHO, 0, ask::PIN).await;
    served.answer(id, "8086");
    served.expect(PROMPT_DONE, 1, "").await;
    assert_eq!(done(call).await.expect("the PIN"), "8086");

    // A passkey, then BlueZ cancelling it mid-conversation.
    let call = {
        let agent = agent.clone();
        tokio::spawn(async move { agent.request_passkey(&path(BUDS)).await })
    };
    served
        .expect(PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    served.expect(PROMPT_ECHO, 0, ask::PASSKEY).await;
    agent.cancel().await.expect("Cancel returns");
    served.expect(PROMPT_CANCEL, 0, "").await;
    assert_eq!(error_name(done(call).await), "org.bluez.Error.Canceled");

    // A passkey answered.
    let call = {
        let agent = agent.clone();
        tokio::spawn(async move { agent.request_passkey(&path(BUDS)).await })
    };
    let id = served
        .expect(PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    served.expect(PROMPT_ECHO, 0, ask::PASSKEY).await;
    served.answer(id, "000123");
    served.expect(PROMPT_DONE, 1, "").await;
    assert_eq!(done(call).await.expect("the passkey"), 123);

    served.close().await;
}

#[tokio::test]
async fn a_shown_passkey_ends_when_bluez_says_the_device_paired() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipped");
        return;
    };
    let bluez = bluez(&bus).await;
    let mut served = serve(&bus, true).await;
    let agent = agent(&bluez, &served).await;

    agent
        .display_passkey(&path(BUDS), 42, 0)
        .await
        .expect("a display returns at once");
    served
        .expect(PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    served
        .expect(PROMPT_INFO, 0, "Type 000042 on the device")
        .await;
    agent
        .display_passkey(&path(BUDS), 42, 4)
        .await
        .expect("a display returns at once");
    served
        .expect(PROMPT_INFO, 0, "Type 000042 on the device (4 typed)")
        .await;

    let device = bluez
        .object_server()
        .interface::<_, Device>(BUDS)
        .await
        .expect("the device is served");
    device.get_mut().await.paired = true;
    let emitter: &SignalEmitter<'_> = device.signal_emitter();
    device
        .get()
        .await
        .paired_changed(emitter)
        .await
        .expect("PropertiesChanged goes out");
    served.expect(PROMPT_DONE, 1, "").await;
    served.close().await;
}

#[tokio::test]
async fn in_earnest_only_bluez_may_call() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipped");
        return;
    };
    let _bluez = bluez(&bus).await;
    let mut served = serve(&bus, true).await;
    let stranger = connect(&bus).await;
    let agent = agent(&stranger, &served).await;
    assert_eq!(
        error_name(agent.request_confirmation(&path(BUDS), 1).await),
        "org.bluez.Error.Rejected"
    );
    assert_eq!(error_name(agent.cancel().await), "org.bluez.Error.Rejected");
    assert!(
        served.frames.try_recv().is_err(),
        "a stranger's call put nothing on screen"
    );
    served.close().await;
}
