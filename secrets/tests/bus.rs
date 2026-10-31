//! The agent object on a private `dbus-daemon`: a peer plays NetworkManager
//! and calls `GetSecrets` and `CancelGetSecrets` as NetworkManager would,
//! the frames come out as the host would read them, and the method returns
//! what NetworkManager would see. No system bus is involved.

use std::collections::HashMap;
use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, Frame, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ERROR, PROMPT_INFO, PROMPT_INFO_TITLE,
    PROMPT_SECRET, Prompt, Secret,
};
use caustics_secrets::agent::SERVICE;
use caustics_secrets::ask::{ALLOW_INTERACTION, REQUEST_NEW, WIRELESS_SECURITY, WRONG};
use caustics_secrets::conversation::{BAD_PSK, Event, Sink};
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const SECRET: &str = "the bus test's secret";
const WAIT: Duration = Duration::from_secs(10);
const PROFILE: &str = "/org/freedesktop/NetworkManager/Settings/7";

type Reply = HashMap<String, HashMap<String, OwnedValue>>;

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.SecretAgent",
    default_path = "/org/freedesktop/NetworkManager/SecretAgent"
)]
trait Agent {
    fn get_secrets(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        connection_path: &ObjectPath<'_>,
        setting_name: &str,
        hints: &[&str],
        flags: u32,
    ) -> zbus::Result<Reply>;

    fn cancel_get_secrets(
        &self,
        connection_path: &ObjectPath<'_>,
        setting_name: &str,
    ) -> zbus::Result<()>;

    fn save_secrets(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        connection_path: &ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a connection to the private bus")
}

/// The agent, served on its own connection.
struct Served {
    name: String,
    events: UnboundedSender<Event>,
    frames: UnboundedReceiver<Frame>,
    task: JoinHandle<i32>,
}

async fn serve(bus: &Bus, check_caller: bool) -> Served {
    let conn = connect(bus).await;
    let unique = conn.unique_name().expect("a unique name").to_string();
    let (events, inbox) = unbounded_channel();
    let (frames_tx, frames) = unbounded_channel();
    let sink: Sink = Box::new(move |frame| {
        let _ = frames_tx.send(frame.clone());
        Ok(())
    });
    let events_for_task = events.clone();
    let task = tokio::spawn(async move {
        caustics_secrets::serve(&conn, check_caller, events_for_task, sink, inbox).await
    });
    Served {
        name: unique,
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

    async fn expect(&mut self, kind: u32, arg: u32) -> Prompt {
        let prompt = self.prompt().await;
        assert_eq!((prompt.kind, prompt.arg), (kind, arg), "{prompt:?}");
        prompt
    }

    fn answer(&self, id: u32, text: &str) {
        self.events
            .send(Event::Answer(Answer::Text {
                id,
                bytes: Secret::new(text.as_bytes().to_vec()),
            }))
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

/// `GetSecrets` for a WPA-PSK network named `id`, spawned so the test can
/// play the host meanwhile.
fn get(proxy: &AgentProxy<'static>, id: &str, flags: u32) -> JoinHandle<zbus::Result<Reply>> {
    let proxy = proxy.clone();
    let id = id.to_owned();
    tokio::spawn(async move {
        let connection = HashMap::from([
            (
                "connection",
                HashMap::from([
                    ("id", Value::from(id.as_str())),
                    ("type", Value::from("802-11-wireless")),
                ]),
            ),
            (
                WIRELESS_SECURITY,
                HashMap::from([("key-mgmt", Value::from("wpa-psk"))]),
            ),
        ]);
        proxy
            .get_secrets(
                connection,
                &ObjectPath::try_from(PROFILE).unwrap(),
                WIRELESS_SECURITY,
                &[],
                flags,
            )
            .await
    })
}

async fn returned(call: JoinHandle<zbus::Result<Reply>>) -> zbus::Result<Reply> {
    tokio::time::timeout(WAIT, call)
        .await
        .expect("the call returns")
        .expect("the task")
}

fn error_name(result: zbus::Result<Reply>) -> String {
    match result {
        Err(zbus::Error::MethodError(name, _, _)) => name.as_str().to_owned(),
        other => panic!("not a method error: {other:?}"),
    }
}

fn psk(reply: &Reply) -> Option<String> {
    match reply.get(WIRELESS_SECURITY)?.get("psk").map(|v| &**v) {
        Some(Value::Str(s)) => Some(s.as_str().to_owned()),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_is_prompted_answered_and_returns_the_secret() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipping");
        return;
    };
    let mut served = serve(&bus, false).await;
    let nm = connect(&bus).await;
    let proxy = agent(&nm, &served).await;

    // Answered: the reply carries the secret under the setting and key, and
    // nothing else.
    let call = get(&proxy, "CoffeeShop", ALLOW_INTERACTION);
    let title = served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    assert_eq!(title.message, "CoffeeShop");
    let ask = served.expect(PROMPT_SECRET, 0).await;
    assert_eq!(ask.message, "Password");
    served.answer(ask.id, SECRET);
    served.expect(PROMPT_DONE, 1).await;
    let reply = returned(call).await.expect("the secret");
    assert_eq!(psk(&reply).as_deref(), Some(SECRET));
    assert_eq!(reply.len(), 1, "{:?}", reply.keys());
    assert_eq!(reply[WIRELESS_SECURITY].len(), 1);

    // A passphrase NetworkManager would refuse is an error line and the ask
    // again, in the same conversation.
    let call = get(&proxy, "CoffeeShop", ALLOW_INTERACTION);
    served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    let ask = served.expect(PROMPT_SECRET, 0).await;
    served.answer(ask.id, "short");
    let error = served.expect(PROMPT_ERROR, 0).await;
    assert_eq!((error.id, error.message.as_str()), (ask.id, BAD_PSK));
    let again = served.expect(PROMPT_SECRET, 0).await;
    assert_eq!(again.id, ask.id);
    served.answer(again.id, SECRET);
    served.expect(PROMPT_DONE, 1).await;
    assert_eq!(
        psk(&returned(call).await.expect("the secret")).as_deref(),
        Some(SECRET)
    );

    // The last answer refused: the conversation leads with the error line.
    let call = get(&proxy, "CoffeeShop", ALLOW_INTERACTION | REQUEST_NEW);
    served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    let wrong = served.expect(PROMPT_ERROR, 0).await;
    assert_eq!(wrong.message, WRONG);
    let ask = served.expect(PROMPT_SECRET, 0).await;

    // Dismissed: UserCanceled.
    served
        .events
        .send(Event::Answer(Answer::Cancelled { id: ask.id }))
        .unwrap();
    served.expect(PROMPT_DONE, 0).await;
    assert_eq!(
        error_name(returned(call).await),
        "org.freedesktop.NetworkManager.SecretAgent.Error.UserCanceled"
    );

    // Cancelled by NetworkManager while it asks: the host is told to
    // withdraw it, and the call returns AgentCanceled.
    let call = get(&proxy, "CoffeeShop", ALLOW_INTERACTION);
    served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    served.expect(PROMPT_SECRET, 0).await;
    proxy
        .cancel_get_secrets(&ObjectPath::try_from(PROFILE).unwrap(), WIRELESS_SECURITY)
        .await
        .expect("cancels");
    served.expect(PROMPT_CANCEL, 0).await;
    assert_eq!(
        error_name(returned(call).await),
        "org.freedesktop.NetworkManager.SecretAgent.Error.AgentCanceled"
    );

    // No interaction allowed: NoSecrets at once, and the host hears nothing.
    let result = returned(get(&proxy, "CoffeeShop", 0)).await;
    assert_eq!(
        error_name(result),
        "org.freedesktop.NetworkManager.SecretAgent.Error.NoSecrets"
    );
    assert!(served.frames.try_recv().is_err(), "a frame for no question");

    // Saving stores nothing and returns at once.
    proxy
        .save_secrets(HashMap::new(), &ObjectPath::try_from(PROFILE).unwrap())
        .await
        .expect("saves nothing");
    served.close().await;
}

/// One conversation at a time: a second request waits, unseen by the host,
/// and NetworkManager cancelling it takes it out with no frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_request_queues_and_its_cancel_is_silent() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipping");
        return;
    };
    let mut served = serve(&bus, false).await;
    let nm = connect(&bus).await;
    let proxy = agent(&nm, &served).await;

    let first = get(&proxy, "First", ALLOW_INTERACTION);
    let title = served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    assert_eq!(title.message, "First");
    let ask = served.expect(PROMPT_SECRET, 0).await;

    // The second, for another profile: queued, nothing on the wire.
    let second = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            proxy
                .get_secrets(
                    HashMap::from([("connection", HashMap::from([("id", Value::from("Second"))]))]),
                    &ObjectPath::try_from("/org/freedesktop/NetworkManager/Settings/8").unwrap(),
                    WIRELESS_SECURITY,
                    &[],
                    ALLOW_INTERACTION,
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        served.frames.try_recv().is_err(),
        "the queued one was shown"
    );
    proxy
        .cancel_get_secrets(
            &ObjectPath::try_from("/org/freedesktop/NetworkManager/Settings/8").unwrap(),
            WIRELESS_SECURITY,
        )
        .await
        .expect("cancels");
    assert_eq!(
        error_name(returned(second).await),
        "org.freedesktop.NetworkManager.SecretAgent.Error.AgentCanceled"
    );
    assert!(
        served.frames.try_recv().is_err(),
        "a frame for a queued cancel"
    );

    served.answer(ask.id, SECRET);
    served.expect(PROMPT_DONE, 1).await;
    assert_eq!(
        psk(&returned(first).await.expect("the secret")).as_deref(),
        Some(SECRET)
    );
    served.close().await;
}

/// In earnest, only the owner of NetworkManager's name may call: a stranger
/// is refused before anything reaches the host, and the owner is heard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_earnest_only_networkmanager_may_call() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipping");
        return;
    };
    let mut served = serve(&bus, true).await;
    let stranger = connect(&bus).await;
    let result = returned(get(
        &agent(&stranger, &served).await,
        "Stranger",
        ALLOW_INTERACTION,
    ))
    .await;
    assert_eq!(
        error_name(result),
        "org.freedesktop.NetworkManager.SecretAgent.Error.PermissionDenied"
    );
    assert!(
        served.frames.try_recv().is_err(),
        "the host heard a stranger"
    );

    let nm = connect(&bus).await;
    nm.request_name(SERVICE).await.expect("the name");
    let proxy = agent(&nm, &served).await;
    let call = get(&proxy, "Home", ALLOW_INTERACTION);
    served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    let ask = served.expect(PROMPT_SECRET, 0).await;
    served.answer(ask.id, SECRET);
    served.expect(PROMPT_DONE, 1).await;
    assert_eq!(
        psk(&returned(call).await.expect("the secret")).as_deref(),
        Some(SECRET)
    );
    served.close().await;
}
