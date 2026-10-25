//! The conductor against `tests/fake-helper.sh`: requests go in as the agent
//! object hands them over, frames come out on a channel as the host would
//! read them, and answers go in as the host writes them. Each test has its
//! own directory and its own wrapper around the fake helper, so the helper's
//! environment is the test's and not the process's.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, Frame, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR, PROMPT_INFO,
    PROMPT_INFO_TITLE, PROMPT_SECRET, Prompt, Secret,
};
use caustics_polkit::conversation::{Begin, Conductor, Event, Outcome, Sink};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const SECRET: &str = "correct horse battery";
const WAIT: Duration = Duration::from_secs(10);

/// One test's scratch directory, and the fake helper's settings for it.
struct Scene {
    dir: PathBuf,
    helper: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl Scene {
    /// A fresh directory named for the test, and the checked-in fake helper
    /// told `mode` and this directory's record files. Nothing executable is
    /// written at run time: a script written by one test and spawned while
    /// another test forks is `ETXTBSY`.
    fn new(name: &str, mode: &str) -> Scene {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("polkit-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test's directory");
        let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-helper.sh");
        let env = [
            ("FAKE_HELPER_SECRET", OsString::from(SECRET)),
            ("FAKE_HELPER_MODE", OsString::from(mode)),
            ("FAKE_HELPER_RECORD", dir.join("record").into()),
            ("FAKE_HELPER_COOKIES", dir.join("cookies").into()),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), v))
        .collect();
        Scene { dir, helper, env }
    }

    /// What the helper read as answers, one line each.
    fn record(&self) -> String {
        std::fs::read_to_string(self.dir.join("record")).unwrap_or_default()
    }

    fn cookies(&self) -> String {
        std::fs::read_to_string(self.dir.join("cookies")).unwrap_or_default()
    }
}

/// A conductor running on its own task.
struct Agent {
    events: UnboundedSender<Event>,
    frames: UnboundedReceiver<Frame>,
    task: JoinHandle<i32>,
}

impl Agent {
    fn start(scene: &Scene) -> Agent {
        let (events, inbox) = unbounded_channel();
        let (frames_tx, frames) = unbounded_channel();
        let sink: Sink = Box::new(move |frame| {
            let _ = frames_tx.send(frame.clone());
            Ok(())
        });
        let conductor = Conductor::new(scene.helper.clone(), sink, events.clone())
            .with_helper_env(scene.env.clone());
        let task = tokio::spawn(conductor.run(inbox));
        Agent {
            events,
            frames,
            task,
        }
    }

    /// A request as `BeginAuthentication` hands it over.
    fn begin(&self, cookie: &str) -> oneshot::Receiver<Outcome> {
        let (reply, outcome) = oneshot::channel();
        let begin = Begin {
            action_id: "org.example.test".to_owned(),
            message: format!("Authentication is needed for {cookie}"),
            user: "tester".to_owned(),
            uid: 1000,
            cookie: cookie.to_owned(),
            reply,
        };
        self.events
            .send(Event::Begin(begin))
            .expect("the conductor");
        outcome
    }

    fn cancel(&self, cookie: &str) {
        self.events
            .send(Event::Cancel {
                cookie: cookie.to_owned(),
            })
            .expect("the conductor");
    }

    fn type_in(&self, id: u32, text: &str) {
        self.events
            .send(Event::Answer(Answer::Text {
                id,
                bytes: Secret::new(text.as_bytes().to_vec()),
            }))
            .expect("the conductor");
    }

    fn dismiss(&self, id: u32) {
        self.events
            .send(Event::Answer(Answer::Cancelled { id }))
            .expect("the conductor");
    }

    async fn prompt(&mut self) -> Prompt {
        match tokio::time::timeout(WAIT, self.frames.recv()).await {
            Ok(Some(Frame::Prompt(prompt))) => prompt,
            other => panic!("no prompt: {other:?}"),
        }
    }

    /// The next prompt, which must be of `kind`; its id.
    async fn expect(&mut self, kind: u32, arg: u32, message: &str) -> u32 {
        let prompt = self.prompt().await;
        assert_eq!(
            (prompt.kind, prompt.arg, prompt.message.as_str()),
            (kind, arg, message),
            "{prompt:?}"
        );
        prompt.id
    }

    /// The title and the password ask of a fresh conversation; its id.
    async fn asked(&mut self, cookie: &str) -> u32 {
        let title = format!("Authentication is needed for {cookie}");
        let id = self.expect(PROMPT_INFO, PROMPT_INFO_TITLE, &title).await;
        assert_eq!(self.expect(PROMPT_SECRET, 0, "Password: ").await, id);
        id
    }

    async fn close(self) {
        self.events.send(Event::Closed).expect("the conductor");
        assert_eq!(
            tokio::time::timeout(WAIT, self.task)
                .await
                .expect("it exits")
                .expect("it ran"),
            0
        );
    }
}

async fn outcome(rx: oneshot::Receiver<Outcome>) -> Outcome {
    tokio::time::timeout(WAIT, rx)
        .await
        .expect("an outcome in time")
        .expect("an outcome")
}

#[tokio::test]
async fn the_right_answer_reaches_the_helper_and_authorizes() {
    let scene = Scene::new("success", "ask");
    let mut agent = Agent::start(&scene);
    let done = agent.begin("cookie-1");
    let id = agent.asked("cookie-1").await;
    agent.type_in(id, SECRET);
    assert_eq!(agent.expect(PROMPT_INFO, 0, "ok").await, id);
    assert_eq!(agent.expect(PROMPT_DONE, 1, "").await, id);
    assert_eq!(outcome(done).await, Outcome::Authorized);
    assert_eq!(scene.record(), format!("{SECRET}\n"));
    assert_eq!(scene.cookies(), "cookie-1\n");
    agent.close().await;
}

#[tokio::test]
async fn a_wrong_answer_is_a_failure_with_the_helper_s_error() {
    let scene = Scene::new("failure", "ask");
    let mut agent = Agent::start(&scene);
    let done = agent.begin("cookie-1");
    let id = agent.asked("cookie-1").await;
    agent.type_in(id, "wrong horse");
    assert_eq!(agent.expect(PROMPT_ERROR, 0, "wrong").await, id);
    assert_eq!(agent.expect(PROMPT_DONE, 0, "").await, id);
    assert_eq!(outcome(done).await, Outcome::Failed);
    assert_eq!(scene.record(), "wrong horse\n");
    agent.close().await;
}

/// The user dismisses the prompt mid-ask: the helper is killed before it
/// reads anything, and the request fails as dismissed.
#[tokio::test]
async fn a_dismissal_mid_ask_kills_the_helper() {
    let scene = Scene::new("dismissed", "ask");
    let mut agent = Agent::start(&scene);
    let done = agent.begin("cookie-1");
    let id = agent.asked("cookie-1").await;
    agent.dismiss(id);
    assert_eq!(agent.expect(PROMPT_DONE, 0, "").await, id);
    assert_eq!(outcome(done).await, Outcome::Dismissed);
    assert_eq!(scene.record(), "");
    agent.close().await;
}

/// A helper that exits mid-ask, without a word: a failure.
#[tokio::test]
async fn a_helper_that_dies_mid_ask_is_a_failure() {
    let scene = Scene::new("dies", "die");
    let mut agent = Agent::start(&scene);
    let done = agent.begin("cookie-1");
    let id = agent.asked("cookie-1").await;
    assert_eq!(agent.expect(PROMPT_DONE, 0, "").await, id);
    assert_eq!(outcome(done).await, Outcome::Failed);
    agent.close().await;
}

/// A helper that cannot be spawned at all: the title went, then the end.
#[tokio::test]
async fn a_helper_that_will_not_start_is_a_failure() {
    let mut scene = Scene::new("absent", "ask");
    scene.helper = scene.dir.join("no-such-helper");
    let mut agent = Agent::start(&scene);
    let done = agent.begin("cookie-1");
    let id = agent
        .expect(
            PROMPT_INFO,
            PROMPT_INFO_TITLE,
            "Authentication is needed for cookie-1",
        )
        .await;
    assert_eq!(agent.expect(PROMPT_DONE, 0, "").await, id);
    assert_eq!(outcome(done).await, Outcome::Failed);
    agent.close().await;
}

/// PAM may ask twice: a name with the echo on, then the password, both
/// answered under one id.
#[tokio::test]
async fn two_asks_go_under_one_conversation() {
    let scene = Scene::new("twice", "twice");
    let mut agent = Agent::start(&scene);
    let done = agent.begin("cookie-1");
    let id = agent
        .expect(
            PROMPT_INFO,
            PROMPT_INFO_TITLE,
            "Authentication is needed for cookie-1",
        )
        .await;
    assert_eq!(agent.expect(PROMPT_ECHO, 0, "Login as tester: ").await, id);
    agent.type_in(id, "tester");
    assert_eq!(agent.expect(PROMPT_SECRET, 0, "Password: ").await, id);
    agent.type_in(id, SECRET);
    assert_eq!(agent.expect(PROMPT_INFO, 0, "ok").await, id);
    assert_eq!(agent.expect(PROMPT_DONE, 1, "").await, id);
    assert_eq!(outcome(done).await, Outcome::Authorized);
    assert_eq!(scene.record(), format!("tester\n{SECRET}\n"));
    agent.close().await;
}

/// An answer for nothing that is asking is dropped, and one the helper
/// would split in two ends the conversation without reaching it.
#[tokio::test]
async fn an_answer_out_of_turn_is_dropped_and_an_unfit_one_ends_it() {
    let scene = Scene::new("unfit", "ask");
    let mut agent = Agent::start(&scene);
    let done = agent.begin("cookie-1");
    let id = agent.asked("cookie-1").await;
    agent.type_in(id + 1, SECRET);
    agent.type_in(id, "two\nlines");
    assert_eq!(agent.expect(PROMPT_DONE, 0, "").await, id);
    assert_eq!(outcome(done).await, Outcome::Failed);
    assert_eq!(scene.record(), "");
    agent.close().await;
}

/// Two requests: the second waits, then gets its own id once the first is
/// done; a cancel of a queued one takes it out without a frame.
#[tokio::test]
async fn requests_queue_in_arrival_order_and_a_queued_one_can_be_cancelled() {
    let scene = Scene::new("queue", "ask");
    let mut agent = Agent::start(&scene);
    let first = agent.begin("cookie-1");
    let second = agent.begin("cookie-2");
    let third = agent.begin("cookie-3");
    let one = agent.asked("cookie-1").await;
    agent.cancel("cookie-2");
    assert_eq!(outcome(second).await, Outcome::Cancelled);
    agent.type_in(one, SECRET);
    assert_eq!(agent.expect(PROMPT_INFO, 0, "ok").await, one);
    assert_eq!(agent.expect(PROMPT_DONE, 1, "").await, one);
    assert_eq!(outcome(first).await, Outcome::Authorized);
    // The cancelled one never reached the host; the third is next.
    let three = agent.asked("cookie-3").await;
    assert_ne!(three, one);
    agent.type_in(three, SECRET);
    assert_eq!(agent.expect(PROMPT_INFO, 0, "ok").await, three);
    assert_eq!(agent.expect(PROMPT_DONE, 1, "").await, three);
    assert_eq!(outcome(third).await, Outcome::Authorized);
    assert_eq!(scene.cookies(), "cookie-1\ncookie-3\n");
    agent.close().await;
}

/// A cancel of the one on screen kills its helper, withdraws it from the
/// host, and lets the next one through.
#[tokio::test]
async fn a_cancel_of_the_active_one_withdraws_it_and_starts_the_next() {
    let scene = Scene::new("cancel-active", "ask");
    let mut agent = Agent::start(&scene);
    let first = agent.begin("cookie-1");
    let second = agent.begin("cookie-2");
    let one = agent.asked("cookie-1").await;
    agent.cancel("cookie-1");
    assert_eq!(agent.expect(PROMPT_CANCEL, 0, "").await, one);
    assert_eq!(outcome(first).await, Outcome::Cancelled);
    let two = agent.asked("cookie-2").await;
    // An answer to the withdrawn one is dropped, not sent to the new helper.
    agent.type_in(one, "stale");
    agent.type_in(two, SECRET);
    assert_eq!(agent.expect(PROMPT_INFO, 0, "ok").await, two);
    assert_eq!(agent.expect(PROMPT_DONE, 1, "").await, two);
    assert_eq!(outcome(second).await, Outcome::Authorized);
    assert_eq!(scene.record(), format!("{SECRET}\n"));
    agent.close().await;
}

/// The host leaving ends everything open as a failure.
#[tokio::test]
async fn the_host_leaving_fails_what_is_open() {
    let scene = Scene::new("host-gone", "ask");
    let mut agent = Agent::start(&scene);
    let first = agent.begin("cookie-1");
    let second = agent.begin("cookie-2");
    agent.asked("cookie-1").await;
    agent.close().await;
    assert_eq!(outcome(first).await, Outcome::Failed);
    assert_eq!(outcome(second).await, Outcome::Failed);
}
