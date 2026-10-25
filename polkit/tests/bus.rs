//! The agent object on a private `dbus-daemon`: a peer plays polkitd and
//! calls `BeginAuthentication` and `CancelAuthentication` as polkitd would,
//! the frames come out as the host would read them, and the method returns
//! what polkitd would see. No system bus is involved.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, Frame, PROMPT_CANCEL, PROMPT_DONE, PROMPT_INFO, PROMPT_INFO_TITLE, PROMPT_SECRET,
    Prompt, Secret,
};
use caustics_polkit::agent::{AUTHORITY, own_uid};
use caustics_polkit::conversation::{Conductor, Event, Sink};
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zbus::Connection;
use zbus::zvariant::Value;

const SECRET: &str = "the bus test's secret";
const WAIT: Duration = Duration::from_secs(10);

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.AuthenticationAgent",
    default_path = "/org/freedesktop/PolicyKit1/AuthenticationAgent"
)]
trait Agent {
    fn begin_authentication(
        &self,
        action_id: &str,
        message: &str,
        icon_name: &str,
        details: HashMap<&str, &str>,
        cookie: &str,
        identities: &[(&str, HashMap<&str, Value<'_>>)],
    ) -> zbus::Result<()>;

    fn cancel_authentication(&self, cookie: &str) -> zbus::Result<()>;
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
    record: std::path::PathBuf,
}

async fn serve(bus: &Bus, check_caller: bool, name: &str) -> Served {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("polkit-bus-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test's directory");
    let record = dir.join("record");
    let conn = connect(bus).await;
    let unique = conn.unique_name().expect("a unique name").to_string();
    let (events, inbox) = unbounded_channel();
    let (frames_tx, frames) = unbounded_channel();
    let sink: Sink = Box::new(move |frame| {
        let _ = frames_tx.send(frame.clone());
        Ok(())
    });
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-helper.sh");
    let conductor = Conductor::new(helper, sink, events.clone()).with_helper_env(vec![
        (OsString::from("FAKE_HELPER_SECRET"), OsString::from(SECRET)),
        (OsString::from("FAKE_HELPER_RECORD"), record.clone().into()),
    ]);
    let events_for_task = events.clone();
    let task = tokio::spawn(async move {
        caustics_polkit::serve(&conn, check_caller, events_for_task, conductor, inbox).await
    });
    Served {
        name: unique,
        events,
        frames,
        task,
        record,
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

fn me() -> (&'static str, HashMap<&'static str, Value<'static>>) {
    ("unix-user", HashMap::from([("uid", Value::U32(own_uid()))]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_is_prompted_answered_and_returns() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipping");
        return;
    };
    let mut served = serve(&bus, false, "answered").await;
    let polkitd = connect(&bus).await;
    let proxy = agent(&polkitd, &served).await;

    // Answered with the secret: the call returns, the helper read it.
    let call = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            proxy
                .begin_authentication(
                    "org.example.test",
                    "Authentication is needed to test",
                    "",
                    HashMap::new(),
                    "cookie-1",
                    &[me()],
                )
                .await
        })
    };
    let title = served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    assert_eq!(title.message, "Authentication is needed to test");
    let ask = served.expect(PROMPT_SECRET, 0).await;
    assert_eq!(ask.message, "Password: ");
    served
        .events
        .send(Event::Answer(Answer::Text {
            id: ask.id,
            bytes: Secret::new(SECRET.as_bytes().to_vec()),
        }))
        .unwrap();
    served.expect(PROMPT_INFO, 0).await;
    served.expect(PROMPT_DONE, 1).await;
    tokio::time::timeout(WAIT, call)
        .await
        .expect("the call returns")
        .unwrap()
        .expect("authorized");
    assert_eq!(
        std::fs::read_to_string(&served.record).unwrap(),
        format!("{SECRET}\n")
    );

    // Dismissed: the call fails as cancelled.
    let call = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            proxy
                .begin_authentication(
                    "org.example.test",
                    "again",
                    "",
                    HashMap::new(),
                    "cookie-2",
                    &[me()],
                )
                .await
        })
    };
    served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    let ask = served.expect(PROMPT_SECRET, 0).await;
    served
        .events
        .send(Event::Answer(Answer::Cancelled { id: ask.id }))
        .unwrap();
    served.expect(PROMPT_DONE, 0).await;
    let err = tokio::time::timeout(WAIT, call)
        .await
        .expect("the call returns")
        .unwrap()
        .expect_err("dismissed");
    assert!(
        matches!(&err, zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.PolicyKit1.Error.Cancelled"),
        "{err:?}"
    );

    // Cancelled by the requester while it asks: the host is told to
    // withdraw it, and the call fails as cancelled.
    let call = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            proxy
                .begin_authentication(
                    "org.example.test",
                    "once more",
                    "",
                    HashMap::new(),
                    "cookie-3",
                    &[me()],
                )
                .await
        })
    };
    served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    served.expect(PROMPT_SECRET, 0).await;
    proxy
        .cancel_authentication("cookie-3")
        .await
        .expect("cancels");
    served.expect(PROMPT_CANCEL, 0).await;
    let err = tokio::time::timeout(WAIT, call)
        .await
        .expect("the call returns")
        .unwrap()
        .expect_err("cancelled");
    assert!(
        matches!(&err, zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.PolicyKit1.Error.Cancelled"),
        "{err:?}"
    );

    // A request with no user to authenticate as fails at once.
    let err = proxy
        .begin_authentication(
            "org.example.test",
            "nobody",
            "",
            HashMap::new(),
            "cookie-4",
            &[("unix-group", HashMap::from([("gid", Value::U32(10))]))],
        )
        .await
        .expect_err("no identity");
    assert!(
        matches!(&err, zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.PolicyKit1.Error.Failed"),
        "{err:?}"
    );
    served.close().await;
}

/// In earnest, only the owner of polkitd's name may call: a stranger is
/// refused before anything reaches the host, and the owner is heard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_earnest_only_polkitd_may_call() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon here; skipping");
        return;
    };
    let mut served = serve(&bus, true, "earnest").await;
    let stranger = connect(&bus).await;
    let err = agent(&stranger, &served)
        .await
        .begin_authentication(
            "org.example.test",
            "from a stranger",
            "",
            HashMap::new(),
            "cookie-1",
            &[me()],
        )
        .await
        .expect_err("refused");
    assert!(
        matches!(&err, zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.PolicyKit1.Error.Failed"),
        "{err:?}"
    );
    assert!(
        served.frames.try_recv().is_err(),
        "the host heard a stranger"
    );

    let polkitd = connect(&bus).await;
    polkitd.request_name(AUTHORITY).await.expect("the name");
    let proxy = agent(&polkitd, &served).await;
    let call = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            proxy
                .begin_authentication(
                    "org.example.test",
                    "from polkitd",
                    "",
                    HashMap::new(),
                    "cookie-2",
                    &[me()],
                )
                .await
        })
    };
    served.expect(PROMPT_INFO, PROMPT_INFO_TITLE).await;
    served.expect(PROMPT_SECRET, 0).await;
    proxy
        .cancel_authentication("cookie-2")
        .await
        .expect("cancels");
    served.expect(PROMPT_CANCEL, 0).await;
    let _ = tokio::time::timeout(WAIT, call).await;
    served.close().await;
}
