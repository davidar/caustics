//! The conductor against fake PAMs: events go in as logind, the socket and
//! the host would send them, frames come out on a channel as the host would
//! read them, and the hint logind would be told comes out on another.

use std::sync::Mutex;
use std::time::Duration;

use caustics_bar_abi::wire::{CANVAS_EVENT_DIMMED, Secret};
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;

use super::*;
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
