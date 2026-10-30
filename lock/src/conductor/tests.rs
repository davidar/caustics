//! The conductor against fake PAMs: events go in as logind, the socket and
//! the host would send them, frames come out on a channel as the host would
//! read them, and the hint logind would be told comes out on another.

use std::sync::Mutex;
use std::time::Duration;

use caustics_bar_abi::wire::{CANVAS_EVENT_DIMMED, Secret};
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;

use super::*;
use crate::fingerprint::Run;
use crate::pam::{DRY_RUN_ASK, DRY_RUN_FAILURE, DryRun};

const RIGHT: &[u8] = b"correct horse battery";
const WAIT: Duration = Duration::from_secs(10);

/// A PAM that runs its closure against the conversation.
struct Scripted<F>(F);

impl<F> Pam for Scripted<F>
where
    F: Fn(&mut Conversation) -> Verdict + Send + Sync + 'static,
{
    fn authenticate(&self, _user: &str, conv: &mut Conversation) -> Verdict {
        (self.0)(conv)
    }
}

/// A conductor running on its own task.
struct Lock {
    events: UnboundedSender<Event>,
    frames: UnboundedReceiver<Frame>,
    hints: UnboundedReceiver<bool>,
    task: JoinHandle<i32>,
}

impl Lock {
    fn start(pam: Arc<dyn Pam>, locked_at_start: bool) -> Lock {
        Lock::with_reader(pam, locked_at_start, None)
    }

    fn with_reader(pam: Arc<dyn Pam>, locked_at_start: bool, sensor: Option<Sensor>) -> Lock {
        let (events, inbox) = unbounded_channel();
        let (frames_tx, frames) = unbounded_channel();
        let (hint, hints) = unbounded_channel();
        let sink: Sink = Box::new(move |frame| {
            let _ = frames_tx.send(frame.clone());
            Ok(())
        });
        let conductor = Conductor::new(
            pam,
            "tester".to_owned(),
            "tester".to_owned(),
            sink,
            events.clone(),
            Some(hint),
            sensor,
        );
        let task = tokio::spawn(conductor.run(inbox, locked_at_start));
        Lock {
            events,
            frames,
            hints,
            task,
        }
    }

    fn dry_run() -> Lock {
        Lock::start(Arc::new(DryRun::new(Secret::new(RIGHT.to_vec()))), false)
    }

    fn send(&self, event: Event) {
        self.events.send(event).expect("the conductor");
    }

    fn canvas(&self, event: u32) {
        self.send(Event::Canvas(CanvasEventBody {
            event,
            ..CanvasEventBody::default()
        }));
    }

    /// A socket command; the channel its reply comes back on.
    fn command(&self, command: Command) -> mpsc::Receiver<String> {
        let (reply, answer) = mpsc::channel();
        self.send(Event::Command(command, reply));
        answer
    }

    async fn reply(answer: mpsc::Receiver<String>) -> String {
        tokio::task::spawn_blocking(move || answer.recv_timeout(WAIT))
            .await
            .expect("the waiting task")
            .expect("a reply")
    }

    async fn status(&self) -> String {
        Lock::reply(self.command(Command::Status)).await
    }

    fn type_in(&self, id: u32, text: &[u8]) {
        self.send(Event::Answer(Answer::Text {
            id,
            bytes: Secret::new(text.to_vec()),
        }));
    }

    fn dismiss(&self, id: u32) {
        self.send(Event::Answer(Answer::Cancelled { id }));
    }

    async fn frame(&mut self) -> Frame {
        match tokio::time::timeout(WAIT, self.frames.recv()).await {
            Ok(Some(frame)) => frame,
            other => panic!("no frame: {other:?}"),
        }
    }

    /// The next frame, which must be a `Lock` with `command`.
    async fn expect_lock(&mut self, command: u32) {
        match self.frame().await {
            Frame::Lock(lock) => assert_eq!(lock.command, command, "{lock:?}"),
            other => panic!("not a lock: {other:?}"),
        }
    }

    /// The next frame, which must be a prompt of `kind`; its id.
    async fn expect(&mut self, kind: u32, arg: u32, message: &str) -> u32 {
        match self.frame().await {
            Frame::Prompt(prompt) => {
                assert_eq!(
                    (prompt.kind, prompt.arg, prompt.message.as_str()),
                    (kind, arg, message),
                    "{prompt:?}"
                );
                prompt.id
            }
            other => panic!("not a prompt: {other:?}"),
        }
    }

    /// A title and the dry run's ask; the conversation's id.
    async fn expect_ask(&mut self) -> u32 {
        let id = self.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
        assert_eq!(self.expect(PROMPT_SECRET, 0, DRY_RUN_ASK).await, id);
        id
    }

    /// Nothing more within `quiet`.
    async fn silent(&mut self, quiet: Duration) {
        if let Ok(Some(frame)) = tokio::time::timeout(quiet, self.frames.recv()).await {
            panic!("an unexpected frame: {frame:?}");
        }
    }

    async fn hint(&mut self) -> bool {
        tokio::time::timeout(WAIT, self.hints.recv())
            .await
            .expect("a hint in time")
            .expect("the hint channel")
    }

    /// Lock through the socket and confirm it as the host would.
    async fn locked(&mut self) {
        assert_eq!(
            Lock::reply(self.command(Command::Lock { wait: false })).await,
            "ok"
        );
        self.expect_lock(LOCK_LOCK).await;
        self.canvas(CANVAS_EVENT_LOCKED);
        assert!(
            self.hint().await,
            "the hint is set when the host says locked"
        );
    }

    async fn close(self) -> i32 {
        self.send(Event::Closed);
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("the conductor ends")
            .expect("the conductor's task")
    }
}

/// A restarted lock whose session logind says is locked asks for the lock
/// before it does anything else, and starts no conversation until the host
/// says locked.
#[tokio::test]
async fn a_set_hint_at_start_locks_before_anything_else() {
    let mut lock = Lock::start(Arc::new(DryRun::new(Secret::new(RIGHT.to_vec()))), true);
    lock.expect_lock(LOCK_LOCK).await;
    lock.silent(Duration::from_millis(200)).await;
    assert_eq!(lock.status().await, "ok locked=no attempts=0");
    lock.canvas(CANVAS_EVENT_LOCKED);
    assert!(lock.hint().await);
    lock.expect_ask().await;
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    assert_eq!(lock.close().await, 0);
}

/// The whole of a wrong-then-right unlock: lock, locked, ask, a wrong answer
/// (PAM's message, a failed end, a fresh ask), the right one (a success and
/// the unlock), and the host's unlocked clearing the hint.
#[tokio::test]
async fn a_wrong_answer_is_counted_and_a_fresh_ask_follows_until_the_right_one() {
    let mut lock = Lock::dry_run();
    lock.locked().await;
    let first = lock.expect_ask().await;
    lock.type_in(first, b"wrong");
    lock.expect(PROMPT_ERROR, 0, DRY_RUN_FAILURE).await;
    lock.expect(PROMPT_DONE, 0, "").await;
    let second = lock.expect_ask().await;
    assert_ne!(first, second, "a fresh conversation has a fresh id");
    assert_eq!(lock.status().await, "ok locked=yes attempts=1");
    lock.type_in(second, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    // Still locked until the host says it is not.
    assert_eq!(lock.status().await, "ok locked=yes attempts=1");
    lock.canvas(CANVAS_EVENT_UNLOCKED);
    assert!(
        !lock.hint().await,
        "the hint is cleared when the host says unlocked"
    );
    assert_eq!(lock.status().await, "ok locked=no attempts=1");
    lock.silent(Duration::from_millis(200)).await;
    assert_eq!(lock.close().await, 0);
}

/// Escape on the lock screen ends the attempt and a fresh one begins: a lock
/// screen is never dismissed, and a dismissal is not a failure.
#[tokio::test]
async fn a_dismissal_starts_a_fresh_conversation_and_counts_nothing() {
    let mut lock = Lock::dry_run();
    lock.locked().await;
    let first = lock.expect_ask().await;
    lock.dismiss(first);
    lock.expect(PROMPT_DONE, 0, "").await;
    let second = lock.expect_ask().await;
    assert_ne!(first, second);
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    lock.type_in(second, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    assert_eq!(lock.close().await, 0);
}

/// logind's Unlock signal unlocks nothing; neither does a canvas event that
/// is the dim's, nor an answer for a conversation that is not asking.
#[tokio::test]
async fn nothing_but_a_success_unlocks() {
    let mut lock = Lock::dry_run();
    lock.locked().await;
    let id = lock.expect_ask().await;
    lock.send(Event::LogindUnlock);
    lock.canvas(CANVAS_EVENT_DIMMED);
    lock.type_in(id + 7, RIGHT);
    lock.silent(Duration::from_millis(300)).await;
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    // The ask is still open, and still the one that unlocks.
    lock.type_in(id, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    assert_eq!(lock.close().await, 0);
}

/// logind's Lock locks; a second lock while locking or locked writes
/// nothing more.
#[tokio::test]
async fn logind_s_lock_locks_once() {
    let mut lock = Lock::dry_run();
    lock.send(Event::LogindLock);
    lock.expect_lock(LOCK_LOCK).await;
    lock.send(Event::LogindLock);
    assert_eq!(
        Lock::reply(lock.command(Command::Lock { wait: false })).await,
        "ok"
    );
    lock.silent(Duration::from_millis(200)).await;
    lock.canvas(CANVAS_EVENT_LOCKED);
    assert!(lock.hint().await);
    lock.expect_ask().await;
    lock.send(Event::LogindLock);
    lock.silent(Duration::from_millis(200)).await;
    assert_eq!(lock.close().await, 0);
}

/// `lock wait` is answered `locked` when the host says so, not before; one
/// asked while locked is answered at once.
#[tokio::test]
async fn lock_wait_is_answered_when_the_host_says_locked() {
    let mut lock = Lock::dry_run();
    let waiting = lock.command(Command::Lock { wait: true });
    lock.expect_lock(LOCK_LOCK).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        waiting.try_recv().is_err(),
        "answered before the host locked"
    );
    lock.canvas(CANVAS_EVENT_LOCKED);
    assert_eq!(Lock::reply(waiting).await, LOCKED);
    assert_eq!(
        Lock::reply(lock.command(Command::Lock { wait: true })).await,
        LOCKED
    );
    assert_eq!(lock.close().await, 0);
}

/// A host that goes before it locked answers the waiting client with an
/// error, which `caustics-canvasctl lock --wait` turns into exit 1.
#[tokio::test]
async fn lock_wait_is_an_error_when_the_host_goes() {
    let mut lock = Lock::dry_run();
    let waiting = lock.command(Command::Lock { wait: true });
    lock.expect_lock(LOCK_LOCK).await;
    assert_eq!(lock.close().await, 0);
    let reply = Lock::reply(waiting).await;
    assert!(reply.starts_with("error "), "{reply}");
}

/// The host going mid-conversation ends the conductor, and the PAM thread
/// blocked on the ask reads `Gone`: nobody is left to answer it.
#[tokio::test]
async fn the_host_going_mid_conversation_leaves_pam_with_nobody_to_answer() {
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let seen_tx = Mutex::new(seen_tx);
    let pam = Scripted(move |conv: &mut Conversation| {
        let gone = matches!(conv.ask(false, DRY_RUN_ASK), Reply::Gone);
        let _ = seen_tx.lock().unwrap().send(gone);
        Verdict::Dismissed
    });
    let mut lock = Lock::start(Arc::new(pam), false);
    lock.locked().await;
    lock.expect_ask().await;
    assert_eq!(lock.close().await, 0);
    let gone = tokio::task::spawn_blocking(move || seen.recv_timeout(WAIT))
        .await
        .unwrap()
        .expect("the PAM thread saw its reply");
    assert!(gone, "the ask read something other than Gone");
}

/// Every kind of PAM message becomes its prompt kind, in order, under one
/// id: an info line, an echoed ask, an error line, an ask with the echo off.
#[tokio::test]
async fn pam_s_four_message_kinds_become_the_four_prompt_kinds() {
    let pam = Scripted(|conv: &mut Conversation| {
        conv.show(false, "Place your finger");
        let Reply::Text(code) = conv.ask(true, "Code: ") else {
            return Verdict::Dismissed;
        };
        conv.show(true, "That code is old");
        let Reply::Text(password) = conv.ask(false, "Password: ") else {
            return Verdict::Dismissed;
        };
        if code.as_bytes() == b"42" && password.as_bytes() == RIGHT {
            Verdict::Success
        } else {
            Verdict::Failed("no".to_owned())
        }
    });
    let mut lock = Lock::start(Arc::new(pam), false);
    lock.locked().await;
    let id = lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    assert_eq!(lock.expect(PROMPT_INFO, 0, "Place your finger").await, id);
    assert_eq!(lock.expect(PROMPT_ECHO, 0, "Code: ").await, id);
    lock.type_in(id, b"42");
    assert_eq!(lock.expect(PROMPT_ERROR, 0, "That code is old").await, id);
    assert_eq!(lock.expect(PROMPT_SECRET, 0, "Password: ").await, id);
    lock.type_in(id, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    assert_eq!(lock.close().await, 0);
}

/// A PAM that fails without asking anything (a service that denies
/// outright) is not counted against the user, and the next conversation
/// waits out a pause instead of spinning.
#[tokio::test]
async fn a_pam_that_never_asks_is_retried_after_a_pause() {
    let pam = Scripted(|_conv: &mut Conversation| Verdict::Failed("Permission denied".to_owned()));
    let mut lock = Lock::start(Arc::new(pam), false);
    lock.locked().await;
    lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    lock.expect(PROMPT_ERROR, 0, "Permission denied").await;
    lock.expect(PROMPT_DONE, 0, "").await;
    lock.silent(FIRST_PAUSE / 2).await;
    lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    assert_eq!(lock.close().await, 0);
}

#[test]
fn a_long_message_is_cut_at_a_character_boundary() {
    let long = "é".repeat(MAX_NAME_LEN);
    let cut_to = cut(&long, MAX_NAME_LEN);
    assert_eq!(cut_to.len(), MAX_NAME_LEN);
    assert!(cut(&format!("a{long}"), MAX_NAME_LEN).len() < MAX_NAME_LEN);
    assert_eq!(cut("short", MAX_NAME_LEN), "short");
}

/// A reader that does nothing on its own: each run is announced on a
/// channel, the test sends its findings, and its stop is reported.
struct FakeReader {
    runs: UnboundedSender<FakeRun>,
}

/// One run of the [`FakeReader`]: its id, and a channel that says, when it
/// was stopped, what the stop said.
struct FakeRun {
    id: u32,
    user: String,
    stopped: tokio::sync::oneshot::Receiver<Option<String>>,
}

impl Reader for FakeReader {
    fn start(&mut self, user: &str, run: u32, _events: UnboundedSender<Event>) -> Run {
        let (stop, stop_rx) = tokio::sync::oneshot::channel::<Option<String>>();
        let (stopped_tx, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let why = stop_rx.await.unwrap_or(None);
            let _ = stopped_tx.send(why);
        });
        let _ = self.runs.send(FakeRun {
            id: run,
            user: user.to_owned(),
            stopped,
        });
        Run::new(stop, task)
    }
}

const READY: &str = "Touch the fingerprint reader";

/// A conductor with a fake reader, and where its runs are announced. Its
/// windows are the default: none closes within a test.
fn with_fake_reader(pam: Arc<dyn Pam>) -> (Lock, UnboundedReceiver<FakeRun>) {
    with_windows(pam, Windows::default())
}

/// A conductor with a fake reader held in `windows`.
fn with_windows(pam: Arc<dyn Pam>, windows: Windows) -> (Lock, UnboundedReceiver<FakeRun>) {
    let (runs, announced) = unbounded_channel();
    let reader: Box<dyn Reader> = Box::new(FakeReader { runs });
    let lock = Lock::with_reader(pam, false, Some(Sensor { reader, windows }));
    (lock, announced)
}

async fn next_run(runs: &mut UnboundedReceiver<FakeRun>) -> FakeRun {
    tokio::time::timeout(WAIT, runs.recv())
        .await
        .expect("a run in time")
        .expect("the reader's channel")
}

/// Wait for `run` to be stopped; what its stop said.
async fn stopped(run: FakeRun) -> Option<String> {
    tokio::time::timeout(WAIT, run.stopped)
        .await
        .expect("the run is stopped in time")
        .expect("the run's task said so")
}

impl Lock {
    fn finger(&self, run: u32, finger: Finger) {
        self.send(Event::Finger { run, finger });
    }
}

/// A match while the password is being asked for ends the conversation as
/// a success and unlocks; the ask PAM is blocked on reads a dismissal, and
/// the end the thread sends for it changes nothing. The run stops with the
/// lock.
#[tokio::test]
async fn a_match_ends_the_conversation_unlocks_and_cancels_the_ask() {
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let seen_tx = Mutex::new(seen_tx);
    let pam = Scripted(move |conv: &mut Conversation| {
        let cancelled = matches!(conv.ask(false, DRY_RUN_ASK), Reply::Cancelled);
        let _ = seen_tx.lock().unwrap().send(cancelled);
        Verdict::Dismissed
    });
    let (mut lock, mut runs) = with_fake_reader(Arc::new(pam));
    lock.locked().await;
    let run = next_run(&mut runs).await;
    assert_eq!(run.user, "tester");
    let id = lock.expect_ask().await;
    lock.finger(
        run.id,
        Finger::Ready {
            how: READY.to_owned(),
        },
    );
    assert_eq!(lock.expect(PROMPT_INFO, 0, READY).await, id);
    lock.finger(run.id, Finger::Match);
    assert_eq!(lock.expect(PROMPT_DONE, 1, "").await, id);
    lock.expect_lock(LOCK_UNLOCK).await;
    let cancelled = tokio::task::spawn_blocking(move || seen.recv_timeout(WAIT))
        .await
        .unwrap()
        .expect("the PAM thread saw its reply");
    assert!(cancelled, "the pending ask was not cancelled");
    // The thread's Dismissed is for a conversation no longer on: no failed
    // end, no fresh conversation.
    lock.silent(Duration::from_millis(300)).await;
    stopped(run).await;
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    lock.canvas(CANVAS_EVENT_UNLOCKED);
    assert!(!lock.hint().await);
    assert_eq!(lock.close().await, 0);
}

/// A fresh conversation after a wrong password says again, under its
/// title, that the reader is live; a no-match is an error line under the
/// conversation on screen and is not a failed attempt.
#[tokio::test]
async fn a_fresh_conversation_gets_the_ready_line_again() {
    let (mut lock, mut runs) = with_fake_reader(Arc::new(DryRun::new(Secret::new(RIGHT.to_vec()))));
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let first = lock.expect_ask().await;
    lock.finger(
        run.id,
        Finger::Ready {
            how: READY.to_owned(),
        },
    );
    lock.expect(PROMPT_INFO, 0, READY).await;
    lock.finger(run.id, Finger::NoMatch);
    assert_eq!(lock.expect(PROMPT_ERROR, 0, NOT_RECOGNISED).await, first);
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    lock.type_in(first, b"wrong");
    lock.expect(PROMPT_ERROR, 0, DRY_RUN_FAILURE).await;
    lock.expect(PROMPT_DONE, 0, "").await;
    let second = lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    assert_eq!(lock.expect(PROMPT_INFO, 0, READY).await, second);
    assert_eq!(lock.expect(PROMPT_SECRET, 0, DRY_RUN_ASK).await, second);
    assert_eq!(lock.status().await, "ok locked=yes attempts=1");
    // The password still unlocks, and the run stops with the lock.
    lock.type_in(second, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    stopped(run).await;
    assert_eq!(lock.close().await, 0);
}

/// A line while the answer is with PAM is held until PAM speaks again: an
/// error line then would read to the host as the answer's failure.
#[tokio::test]
async fn a_line_while_pam_verifies_waits_for_pam() {
    let (go_tx, go) = std::sync::mpsc::channel::<()>();
    let go = Mutex::new(go);
    let pam = Scripted(move |conv: &mut Conversation| {
        let Reply::Text(_) = conv.ask(false, DRY_RUN_ASK) else {
            return Verdict::Dismissed;
        };
        let _ = go.lock().unwrap().recv_timeout(WAIT);
        Verdict::Failed(DRY_RUN_FAILURE.to_owned())
    });
    let (mut lock, mut runs) = with_fake_reader(Arc::new(pam));
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let first = lock.expect_ask().await;
    // One channel, read in order: the answer is with PAM when the line
    // comes.
    lock.type_in(first, b"anything");
    lock.finger(run.id, Finger::NoMatch);
    lock.silent(Duration::from_millis(200)).await;
    go_tx.send(()).unwrap();
    lock.expect(PROMPT_ERROR, 0, DRY_RUN_FAILURE).await;
    lock.expect(PROMPT_DONE, 0, "").await;
    let second = lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    assert_eq!(lock.expect(PROMPT_ERROR, 0, NOT_RECOGNISED).await, second);
    assert_eq!(lock.close().await, 0);
}

/// A match in the pause after a PAM that asked nothing unlocks all the
/// same; a line in that pause waits for the next conversation; and the
/// pause's end starts nothing once unlocking.
#[tokio::test]
async fn a_match_during_a_retry_pause_unlocks() {
    let pam = Scripted(|_conv: &mut Conversation| Verdict::Failed("Permission denied".to_owned()));
    let (mut lock, mut runs) = with_fake_reader(Arc::new(pam));
    lock.locked().await;
    let run = next_run(&mut runs).await;
    lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    lock.expect(PROMPT_ERROR, 0, "Permission denied").await;
    lock.expect(PROMPT_DONE, 0, "").await;
    lock.finger(run.id, Finger::NoMatch);
    lock.silent(Duration::from_millis(100)).await;
    lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    lock.expect(PROMPT_ERROR, 0, NOT_RECOGNISED).await;
    lock.expect(PROMPT_ERROR, 0, "Permission denied").await;
    lock.expect(PROMPT_DONE, 0, "").await;
    // The second pause: a match in it.
    lock.finger(run.id, Finger::Match);
    lock.expect_lock(LOCK_UNLOCK).await;
    stopped(run).await;
    lock.silent(FIRST_PAUSE * 2 + Duration::from_millis(300))
        .await;
    assert_eq!(lock.close().await, 0);
}

/// The run lasts as long as the lock: the unlock stops it, the next lock
/// starts a fresh one, and a finding of the old run is dropped.
#[tokio::test]
async fn leaving_locked_stops_the_run_and_a_stale_finding_is_dropped() {
    let (mut lock, mut runs) = with_fake_reader(Arc::new(DryRun::new(Secret::new(RIGHT.to_vec()))));
    lock.locked().await;
    let old = next_run(&mut runs).await;
    let id = lock.expect_ask().await;
    lock.type_in(id, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    let old_id = old.id;
    stopped(old).await;
    lock.canvas(CANVAS_EVENT_UNLOCKED);
    assert!(!lock.hint().await);
    lock.locked().await;
    let new = next_run(&mut runs).await;
    assert_ne!(new.id, old_id);
    lock.expect_ask().await;
    lock.finger(old_id, Finger::Match);
    lock.silent(Duration::from_millis(200)).await;
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    // Closing stdin stops the live run too.
    assert_eq!(lock.close().await, 0);
    stopped(new).await;
}

/// The reader off: no run, ever.
#[tokio::test]
async fn no_reader_starts_no_run() {
    let mut lock = Lock::dry_run();
    lock.locked().await;
    lock.expect_ask().await;
    lock.finger(1, Finger::Match);
    lock.silent(Duration::from_millis(200)).await;
    assert_eq!(lock.close().await, 0);
}

/// Windows short enough for a test, apart enough to tell.
const SHORT: Windows = Windows {
    idle: Duration::from_millis(300),
    cap: Duration::from_millis(900),
};

fn dry_run_pam() -> Arc<dyn Pam> {
    Arc::new(DryRun::new(Secret::new(RIGHT.to_vec())))
}

impl Lock {
    fn input(&self) {
        self.canvas(CANVAS_EVENT_INPUT);
    }

    /// Run `run`'s verify is live.
    fn ready(&self, run: u32) {
        self.finger(
            run,
            Finger::Ready {
                how: READY.to_owned(),
            },
        );
    }

    /// Input every `every` until the task is aborted.
    fn refreshing(&self, every: Duration) -> JoinHandle<()> {
        let events = self.events.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let event = Event::Canvas(CanvasEventBody {
                    event: CANVAS_EVENT_INPUT,
                    ..CanvasEventBody::default()
                });
                if events.send(event).is_err() {
                    return;
                }
            }
        })
    }
}

/// Nothing announced within `quiet`: no window opened.
async fn no_run(runs: &mut UnboundedReceiver<FakeRun>, quiet: Duration) {
    if let Ok(Some(run)) = tokio::time::timeout(quiet, runs.recv()).await {
        panic!("a run {} opened", run.id);
    }
}

/// `locked` opens a window; with no trigger it closes after `idle`, its run
/// stopped with the close's reason, the panel's line becomes the wake line
/// and a fresh conversation says it in place of the ready line, and a
/// finding of the stopped run is dropped.
#[tokio::test]
async fn locked_opens_a_window_that_closes_when_idle() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    lock.locked().await;
    let opened = tokio::time::Instant::now();
    let run = next_run(&mut runs).await;
    let id = lock.expect_ask().await;
    lock.finger(
        run.id,
        Finger::Ready {
            how: READY.to_owned(),
        },
    );
    lock.expect(PROMPT_INFO, 0, READY).await;
    let run_id = run.id;
    assert_eq!(stopped(run).await.as_deref(), Some("window closed (idle)"));
    assert!(opened.elapsed() >= SHORT.idle, "closed before idle");
    assert!(opened.elapsed() < SHORT.cap, "closed only at the cap");
    assert_eq!(lock.expect(PROMPT_INFO, 0, WAKE_READER).await, id);
    lock.finger(run_id, Finger::Match);
    lock.silent(Duration::from_millis(200)).await;
    // A fresh conversation says a key wakes the reader, not that it is live.
    lock.dismiss(id);
    lock.expect(PROMPT_DONE, 0, "").await;
    let fresh = lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    assert_eq!(lock.expect(PROMPT_INFO, 0, WAKE_READER).await, fresh);
    assert_eq!(lock.expect(PROMPT_SECRET, 0, DRY_RUN_ASK).await, fresh);
    assert_eq!(lock.status().await, "ok locked=yes attempts=0");
    assert_eq!(lock.close().await, 0);
}

/// Input reaching a lock surface opens a window during the lock, and an
/// input while one is open moves its close out.
#[tokio::test]
async fn input_opens_a_window_and_refreshes_an_open_one() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    // Input before the lock opens nothing.
    lock.input();
    no_run(&mut runs, Duration::from_millis(100)).await;
    lock.locked().await;
    let first = next_run(&mut runs).await;
    lock.expect_ask().await;
    assert_eq!(
        stopped(first).await.as_deref(),
        Some("window closed (idle)")
    );
    lock.input();
    let second = next_run(&mut runs).await;
    let opened = tokio::time::Instant::now();
    // Two refreshes, each before the idle runs out.
    for _ in 0..2 {
        tokio::time::sleep(SHORT.idle * 2 / 3).await;
        lock.input();
    }
    assert_eq!(
        stopped(second).await.as_deref(),
        Some("window closed (idle)")
    );
    assert!(
        opened.elapsed() >= SHORT.idle * 4 / 3 + SHORT.idle,
        "the refreshes did not move the close out: {:?}",
        opened.elapsed()
    );
    no_run(&mut runs, Duration::from_millis(100)).await;
    assert_eq!(lock.close().await, 0);
}

/// However often it is refreshed, a window closes at its cap; the next
/// trigger opens another at once.
#[tokio::test]
async fn a_window_closes_at_its_cap_however_often_refreshed() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    lock.locked().await;
    let opened = tokio::time::Instant::now();
    let run = next_run(&mut runs).await;
    lock.expect_ask().await;
    let refreshing = lock.refreshing(SHORT.idle / 3);
    assert_eq!(stopped(run).await.as_deref(), Some("window closed (cap)"));
    assert!(opened.elapsed() >= SHORT.cap, "closed before the cap");
    // Still refreshing: a window opens again with no rest to wait out.
    next_run(&mut runs).await;
    refreshing.abort();
    assert_eq!(lock.close().await, 0);
}

/// The commonest return: the window closed with nobody there, and someone
/// comes back. Their first input opens a window at once, and the ready line
/// replaces the wake line when its verify is live.
#[tokio::test]
async fn a_trigger_right_after_a_close_opens_a_window_at_once() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let id = lock.expect_ask().await;
    assert_eq!(stopped(run).await.as_deref(), Some("window closed (idle)"));
    lock.expect(PROMPT_INFO, 0, WAKE_READER).await;
    let closed = tokio::time::Instant::now();
    lock.input();
    lock.send(Event::Resume);
    let again = next_run(&mut runs).await;
    assert!(
        closed.elapsed() < SHORT.idle,
        "the window opened late: {:?}",
        closed.elapsed()
    );
    // The resume refreshed the open window rather than opening another.
    no_run(&mut runs, Duration::from_millis(100)).await;
    lock.ready(again.id);
    assert_eq!(lock.expect(PROMPT_INFO, 0, READY).await, id);
    lock.finger(again.id, Finger::Match);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    assert_eq!(stopped(again).await, None, "the unlock's stop says nothing");
    // The unlock says nothing on the panel either.
    lock.silent(Duration::from_millis(200)).await;
    assert_eq!(lock.close().await, 0);
}

/// A reader that goes mid-window is not available until its run's verify is
/// live again: the panel says so, a fresh conversation says so after its
/// title, and the ready line replaces it when the run is back.
#[tokio::test]
async fn a_reader_gone_mid_window_is_not_available_until_ready_again() {
    let (mut lock, mut runs) = with_fake_reader(dry_run_pam());
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let id = lock.expect_ask().await;
    lock.ready(run.id);
    lock.expect(PROMPT_INFO, 0, READY).await;
    lock.finger(run.id, Finger::Gone("disconnected".to_owned()));
    assert_eq!(lock.expect(PROMPT_INFO, 0, READER_NOT_AVAILABLE).await, id);
    lock.dismiss(id);
    lock.expect(PROMPT_DONE, 0, "").await;
    let fresh = lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    assert_eq!(
        lock.expect(PROMPT_INFO, 0, READER_NOT_AVAILABLE).await,
        fresh
    );
    assert_eq!(lock.expect(PROMPT_SECRET, 0, DRY_RUN_ASK).await, fresh);
    lock.ready(run.id);
    assert_eq!(lock.expect(PROMPT_INFO, 0, READY).await, fresh);
    // The window stays open through it: one run.
    no_run(&mut runs, Duration::from_millis(100)).await;
    assert_eq!(lock.close().await, 0);
    assert_eq!(stopped(run).await, None);
}

/// The wake line, when the window closes while an answer is with PAM, waits
/// for PAM as any reader's line does.
#[tokio::test]
async fn the_wake_line_while_pam_verifies_waits_for_pam() {
    let (go_tx, go) = std::sync::mpsc::channel::<()>();
    let go = Mutex::new(go);
    let pam = Scripted(move |conv: &mut Conversation| {
        let Reply::Text(_) = conv.ask(false, DRY_RUN_ASK) else {
            return Verdict::Dismissed;
        };
        let _ = go.lock().unwrap().recv_timeout(WAIT);
        Verdict::Failed(DRY_RUN_FAILURE.to_owned())
    });
    let (mut lock, mut runs) = with_windows(Arc::new(pam), SHORT);
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let first = lock.expect_ask().await;
    lock.type_in(first, b"anything");
    assert_eq!(stopped(run).await.as_deref(), Some("window closed (idle)"));
    lock.silent(Duration::from_millis(100)).await;
    go_tx.send(()).unwrap();
    lock.expect(PROMPT_ERROR, 0, DRY_RUN_FAILURE).await;
    lock.expect(PROMPT_DONE, 0, "").await;
    let second = lock.expect(PROMPT_INFO, PROMPT_INFO_TITLE, "tester").await;
    assert_eq!(lock.expect(PROMPT_INFO, 0, WAKE_READER).await, second);
    assert_eq!(lock.close().await, 0);
}

/// A resume from suspend opens a window: the person is back. Not while
/// unlocked.
#[tokio::test]
async fn a_resume_opens_a_window() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    lock.send(Event::Resume);
    no_run(&mut runs, Duration::from_millis(100)).await;
    lock.locked().await;
    let first = next_run(&mut runs).await;
    lock.expect_ask().await;
    stopped(first).await;
    lock.expect(PROMPT_INFO, 0, WAKE_READER).await;
    lock.send(Event::Resume);
    let resumed = next_run(&mut runs).await;
    lock.ready(resumed.id);
    lock.expect(PROMPT_INFO, 0, READY).await;
    assert_eq!(lock.close().await, 0);
    assert_eq!(stopped(resumed).await, None);
}

/// A reader that cannot verify for this lock opens no window again until
/// the next lock.
#[tokio::test]
async fn an_unavailable_reader_opens_no_window_until_the_next_lock() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let id = lock.expect_ask().await;
    lock.finger(run.id, Finger::Unavailable("no reader".to_owned()));
    assert_eq!(stopped(run).await, None);
    lock.input();
    no_run(&mut runs, Duration::from_millis(200)).await;
    lock.type_in(id, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    lock.canvas(CANVAS_EVENT_UNLOCKED);
    assert!(!lock.hint().await);
    lock.locked().await;
    next_run(&mut runs).await;
    assert_eq!(lock.close().await, 0);
}

/// The machine going to sleep closes the window open, with that reason, and
/// says nothing on the panel; the before-sleep hook's `locked` and any input
/// open nothing while it sleeps; the resume opens a window, and input
/// extends it as ever.
#[tokio::test]
async fn sleep_closes_the_window_and_none_opens_until_the_resume() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let id = lock.expect_ask().await;
    lock.ready(run.id);
    lock.expect(PROMPT_INFO, 0, READY).await;
    lock.send(Event::Sleep);
    assert_eq!(
        stopped(run).await.as_deref(),
        Some("the machine is going to sleep")
    );
    lock.input();
    no_run(&mut runs, Duration::from_millis(100)).await;
    lock.silent(Duration::from_millis(100)).await;
    lock.send(Event::Resume);
    let resumed = next_run(&mut runs).await;
    let opened = tokio::time::Instant::now();
    lock.ready(resumed.id);
    assert_eq!(lock.expect(PROMPT_INFO, 0, READY).await, id);
    tokio::time::sleep(SHORT.idle * 2 / 3).await;
    lock.input();
    assert_eq!(
        stopped(resumed).await.as_deref(),
        Some("window closed (idle)")
    );
    assert!(
        opened.elapsed() >= SHORT.idle * 2 / 3 + SHORT.idle,
        "the input did not extend the window: {:?}",
        opened.elapsed()
    );
    assert_eq!(lock.expect(PROMPT_INFO, 0, WAKE_READER).await, id);
    assert_eq!(lock.close().await, 0);
}

/// The incident's order: the sleep comes while unlocked, then the
/// before-sleep hook's lock and the host's `locked`. No window opens on that
/// `locked`; the resume opens one, with the resume as its trigger.
#[tokio::test]
async fn a_lock_after_the_sleep_opens_no_window_until_the_resume() {
    let (mut lock, mut runs) = with_windows(dry_run_pam(), SHORT);
    lock.send(Event::Sleep);
    lock.locked().await;
    let id = lock.expect_ask().await;
    no_run(&mut runs, Duration::from_millis(200)).await;
    lock.send(Event::Resume);
    let resumed = next_run(&mut runs).await;
    lock.ready(resumed.id);
    assert_eq!(lock.expect(PROMPT_INFO, 0, READY).await, id);
    lock.type_in(id, RIGHT);
    lock.expect(PROMPT_DONE, 1, "").await;
    lock.expect_lock(LOCK_UNLOCK).await;
    assert_eq!(stopped(resumed).await, None);
    assert_eq!(lock.close().await, 0);
}

/// A claim polkit refused closes the window and puts the wake line up, but
/// is not a reader that cannot verify: the next input opens a window.
#[tokio::test]
async fn a_refused_claim_closes_the_window_and_input_opens_another() {
    let (mut lock, mut runs) = with_fake_reader(dry_run_pam());
    lock.locked().await;
    let run = next_run(&mut runs).await;
    let id = lock.expect_ask().await;
    lock.finger(run.id, Finger::Refused);
    assert_eq!(stopped(run).await, None);
    assert_eq!(lock.expect(PROMPT_INFO, 0, WAKE_READER).await, id);
    lock.input();
    let again = next_run(&mut runs).await;
    lock.ready(again.id);
    assert_eq!(lock.expect(PROMPT_INFO, 0, READY).await, id);
    assert_eq!(lock.close().await, 0);
    assert_eq!(stopped(again).await, None);
}
