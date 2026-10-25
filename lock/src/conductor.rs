//! The conductor: the lock's state, and every conversation turned into
//! `Prompt` frames to the host.
//!
//! Everything arrives as an [`Event`] on one channel (logind's signals, the
//! socket's commands, the host's canvas events and answers, the PAM thread's
//! steps) and one task handles them in order, so the state has one owner and
//! needs no lock.
//!
//! ```text
//!            LOCK_LOCK               CANVAS_EVENT_LOCKED
//! Unlocked ────────────▶ Locking ─────────────────────▶ Locked ─┐ conversations,
//!    ▲                                                     │    │ one after another
//!    │ CANVAS_EVENT_UNLOCKED                               │ ◀──┘
//!    └──────────────────── Unlocking ◀──────────────────────┘
//!                                     a success: LOCK_UNLOCK
//! ```
//!
//! **Nothing unlocks but a success.** logind's `Unlock` signal is witnessed
//! and nothing more; a failure, a dismissal, a host that goes, a PAM that
//! cannot start: each ends one conversation, and while the session is locked
//! the next begins at once. A lock screen is never dismissed.
//!
//! A conversation's life on the wire (docs/abi.md, *Prompt*): a
//! `PROMPT_INFO` title (the user's name), then PAM's asks and lines as it
//! makes them, then one `PROMPT_DONE`: `arg` 1 for a success (followed by the
//! `LOCK_UNLOCK`), 0 for anything else, with PAM's message as a
//! `PROMPT_ERROR` before it when PAM said no.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, CANVAS_EVENT_LOCKED, CANVAS_EVENT_UNLOCKED, CanvasEventBody, Frame, LOCK_LOCK,
    LOCK_UNLOCK, LockBody, MAX_NAME_LEN, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR, PROMPT_INFO,
    PROMPT_INFO_TITLE, PROMPT_SECRET, Prompt,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::pam::{Conversation, Pam, Reply, Step, Verdict};
use crate::socket::{Command, LOCKED};
use crate::witness;

/// Where frames go: stdout in the binary, a channel in a test.
pub type Sink = Box<dyn FnMut(&Frame) -> std::io::Result<()> + Send>;

/// The first pause after a conversation that ended without asking anything,
/// and the longest. Such a conversation is a PAM that cannot work (a service
/// that denies outright, a module that fails before it asks); starting the
/// next at once would spin, so the next waits, twice as long each time, until
/// one asks.
pub const FIRST_PAUSE: Duration = Duration::from_secs(1);
pub const LAST_PAUSE: Duration = Duration::from_secs(30);

/// Everything the conductor wakes up for.
pub enum Event {
    /// logind's `Lock` signal for this session.
    LogindLock,
    /// logind's `Unlock` signal: witnessed, never obeyed.
    LogindUnlock,
    /// A client on the socket, and where its reply goes.
    Command(Command, mpsc::Sender<String>),
    /// The host's word on the lock (docs/abi.md, *Canvas event*).
    Canvas(CanvasEventBody),
    /// The host's answer (docs/abi.md, *Answer*).
    Answer(Answer),
    /// Conversation `id`'s PAM thread.
    Attempt { id: u32, step: Step },
    /// The pause after a conversation that asked nothing is over.
    Retry,
    /// The host closed stdin.
    Closed,
}

/// Where the lock is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Unlocked,
    /// `LOCK_LOCK` written, the host has not said `locked`.
    Locking,
    /// The host said `locked`; a conversation is on, or about to be.
    Locked,
    /// `LOCK_UNLOCK` written, the host has not said `unlocked`.
    Unlocking,
}

/// The conversation on screen.
struct Active {
    id: u32,
    /// The ask PAM is blocked on, and where its reply goes.
    asking: Option<mpsc::Sender<Reply>>,
    /// It asked at least once.
    asked: bool,
}

/// What the conductor knows.
pub struct Conductor {
    pam: Arc<dyn Pam>,
    /// The user PAM authenticates, and the conversation's title.
    user: String,
    title: String,
    sink: Sink,
    /// logind's `SetLockedHint`, made on a task of its own; `None` without
    /// logind.
    hint: Option<UnboundedSender<bool>>,
    events: UnboundedSender<Event>,
    phase: Phase,
    active: Option<Active>,
    next_id: u32,
    /// Failed conversations of the lock now held, or of the last: a count,
    /// never a description (docs/canvas.md, *The prompt core*).
    failures: u32,
    /// Clients waiting in `lock wait` for the host's `locked`.
    waiters: Vec<mpsc::Sender<String>>,
    /// The pause before the next conversation after one that asked nothing.
    pause: Duration,
    /// A retry is scheduled.
    retrying: bool,
}

impl Conductor {
    /// A conductor whose PAM threads report back on `events`. `user` is who
    /// PAM authenticates; `title` heads every conversation.
    pub fn new(
        pam: Arc<dyn Pam>,
        user: String,
        title: String,
        sink: Sink,
        events: UnboundedSender<Event>,
        hint: Option<UnboundedSender<bool>>,
    ) -> Self {
        Conductor {
            pam,
            user,
            title,
            sink,
            hint,
            events,
            phase: Phase::Unlocked,
            active: None,
            next_id: 1,
            failures: 0,
            waiters: Vec::new(),
            pause: FIRST_PAUSE,
            retrying: false,
        }
    }

    /// Handle events until the host closes stdin or the sink fails. With
    /// `locked_at_start` (logind's `LockedHint` was set when this process
    /// started: a restarted host, or a restarted sidecar), the lock is asked
    /// for **before anything else**, so a restarted host re-locks before it
    /// paints (docs/canvas.md, *Failure model*). Returns the exit status.
    pub async fn run(mut self, mut inbox: UnboundedReceiver<Event>, locked_at_start: bool) -> i32 {
        if locked_at_start && !self.lock("logind's LockedHint was set at start") {
            return self.leave();
        }
        while let Some(event) = inbox.recv().await {
            let alive = match event {
                Event::LogindLock => self.lock("logind's Lock"),
                Event::LogindUnlock => {
                    witness(
                        "logind's Unlock: ignored, since nothing unlocks but a conversation \
                         that succeeded",
                    );
                    true
                }
                Event::Command(command, reply) => self.command(command, reply),
                Event::Canvas(event) => self.canvas(event),
                Event::Answer(answer) => self.answer(answer),
                Event::Attempt { id, step } => self.step(id, step),
                Event::Retry => {
                    self.retrying = false;
                    self.begin()
                }
                Event::Closed => {
                    witness("stdin closed; exiting");
                    false
                }
            };
            if !alive {
                break;
            }
        }
        self.leave()
    }

    /// The host is gone, or stdout is: nobody waits on anything any more.
    fn leave(&mut self) -> i32 {
        for waiter in self.waiters.drain(..) {
            let _ = waiter.send("error the canvas went away before it locked".to_owned());
        }
        // The PAM thread's ask, if any, reads `Gone` when this drops.
        self.active = None;
        0
    }

    fn write(&mut self, frame: Frame) -> bool {
        match (self.sink)(&frame) {
            Ok(()) => true,
            Err(err) => {
                witness(&format!("stdout: {err}; exiting"));
                false
            }
        }
    }

    fn prompt(&mut self, id: u32, kind: u32, arg: u32, message: &str) -> bool {
        self.write(Frame::Prompt(Prompt {
            id,
            kind,
            arg,
            message: cut(message, MAX_NAME_LEN).to_owned(),
        }))
    }

    fn hint(&self, locked: bool) {
        if let Some(hint) = &self.hint {
            let _ = hint.send(locked);
        }
    }

    /// Ask the host to lock, unless it is locked or locking already.
    fn lock(&mut self, why: &str) -> bool {
        match self.phase {
            Phase::Locking | Phase::Locked => {
                witness(&format!("{why}: already {:?}", self.phase));
                true
            }
            Phase::Unlocked | Phase::Unlocking => {
                self.phase = Phase::Locking;
                self.failures = 0;
                witness(&format!("{why}: locking"));
                self.write(Frame::Lock(LockBody {
                    command: LOCK_LOCK,
                    ..LockBody::default()
                }))
            }
        }
    }

    fn command(&mut self, command: Command, reply: mpsc::Sender<String>) -> bool {
        match command {
            Command::Lock { wait } => {
                let written = self.lock("the socket's lock");
                if wait && self.phase != Phase::Locked {
                    self.waiters.push(reply);
                } else {
                    let _ = reply.send(if wait { LOCKED } else { "ok" }.to_owned());
                }
                written
            }
            Command::Status => {
                let locked = matches!(self.phase, Phase::Locked | Phase::Unlocking);
                let _ = reply.send(format!(
                    "ok locked={} attempts={}",
                    if locked { "yes" } else { "no" },
                    self.failures
                ));
                true
            }
        }
    }

    fn canvas(&mut self, event: CanvasEventBody) -> bool {
        match event.event {
            CANVAS_EVENT_LOCKED => {
                if self.phase != Phase::Locking {
                    witness(&format!(
                        "the host says locked while {:?}; it is locked",
                        self.phase
                    ));
                }
                self.phase = Phase::Locked;
                self.hint(true);
                witness("locked");
                for waiter in self.waiters.drain(..) {
                    let _ = waiter.send(LOCKED.to_owned());
                }
                self.begin()
            }
            CANVAS_EVENT_UNLOCKED => {
                if self.phase != Phase::Unlocking {
                    witness(&format!(
                        "the host says unlocked while {:?}; it is unlocked",
                        self.phase
                    ));
                }
                self.phase = Phase::Unlocked;
                self.hint(false);
                witness("unlocked");
                for waiter in self.waiters.drain(..) {
                    let _ = waiter.send("error the session was unlocked".to_owned());
                }
                true
            }
            other => {
                witness(&format!(
                    "a canvas event {other} that is not the lock's; ignored"
                ));
                true
            }
        }
    }

    /// Begin a conversation, if the session is locked and none is on (or
    /// waiting out a pause).
    fn begin(&mut self) -> bool {
        if self.phase != Phase::Locked || self.active.is_some() || self.retrying {
            return true;
        }
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let title = self.title.clone();
        if !self.prompt(id, PROMPT_INFO, PROMPT_INFO_TITLE, &title) {
            return false;
        }
        let pam = Arc::clone(&self.pam);
        let user = self.user.clone();
        let conv = Conversation::new(id, self.events.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("pam-{id}"))
            .spawn(move || {
                let mut conv = conv;
                let verdict = pam.authenticate(&user, &mut conv);
                conv.end(verdict);
            });
        self.active = Some(Active {
            id,
            asking: None,
            asked: false,
        });
        if let Err(err) = spawned {
            witness(&format!("conversation {id}: no thread for PAM: {err}"));
            return self.ended(
                id,
                Verdict::Failed("the lock could not start PAM".to_owned()),
            );
        }
        true
    }

    fn step(&mut self, id: u32, step: Step) -> bool {
        let Some(active) = self.active.as_mut().filter(|a| a.id == id) else {
            // A thread whose conversation is no longer on: its ask reads
            // `Gone` as `reply` drops here.
            return true;
        };
        match step {
            Step::Ask {
                echo,
                message,
                reply,
            } => {
                active.asking = Some(reply);
                active.asked = true;
                let kind = if echo { PROMPT_ECHO } else { PROMPT_SECRET };
                self.prompt(id, kind, 0, &message)
            }
            Step::Show { error, message } => {
                let kind = if error { PROMPT_ERROR } else { PROMPT_INFO };
                self.prompt(id, kind, 0, &message)
            }
            Step::Ended(verdict) => self.ended(id, verdict),
        }
    }

    /// Conversation `id` is over: its end on the wire, and what follows it.
    fn ended(&mut self, id: u32, verdict: Verdict) -> bool {
        let asked = self.active.take().is_some_and(|a| a.asked);
        match verdict {
            Verdict::Success => {
                self.pause = FIRST_PAUSE;
                witness(&format!(
                    "conversation {id}: success after {} failed",
                    self.failures
                ));
                if !self.prompt(id, PROMPT_DONE, 1, "") {
                    return false;
                }
                if self.phase != Phase::Locked {
                    return true;
                }
                self.phase = Phase::Unlocking;
                self.write(Frame::Lock(LockBody {
                    command: LOCK_UNLOCK,
                    ..LockBody::default()
                }))
            }
            Verdict::Failed(why) => {
                if !self.prompt(id, PROMPT_ERROR, 0, &why) || !self.prompt(id, PROMPT_DONE, 0, "") {
                    return false;
                }
                if asked {
                    self.pause = FIRST_PAUSE;
                    self.failures = self.failures.saturating_add(1);
                    witness(&format!(
                        "conversation {id}: failed ({} failed in this lock)",
                        self.failures
                    ));
                    self.begin()
                } else {
                    let pause = self.pause;
                    self.pause = (self.pause * 2).min(LAST_PAUSE);
                    witness(&format!(
                        "conversation {id}: PAM ended it without asking anything ({why}); \
                         the next in {} s",
                        pause.as_secs()
                    ));
                    self.retry_after(pause);
                    true
                }
            }
            Verdict::Dismissed => {
                witness(&format!("conversation {id}: dismissed; a fresh one"));
                if !self.prompt(id, PROMPT_DONE, 0, "") {
                    return false;
                }
                self.begin()
            }
        }
    }

    fn retry_after(&mut self, pause: Duration) {
        self.retrying = true;
        let events = self.events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(pause).await;
            let _ = events.send(Event::Retry);
        });
    }

    fn answer(&mut self, answer: Answer) -> bool {
        let id = answer.id();
        let Some(reply) = self
            .active
            .as_mut()
            .filter(|a| a.id == id)
            .and_then(|a| a.asking.take())
        else {
            witness(&format!(
                "an answer for conversation {id}, which is not asking; dropped"
            ));
            return true;
        };
        let reply_to_pam = match answer {
            Answer::Text { bytes, .. } => Reply::Text(bytes),
            Answer::Cancelled { .. } => Reply::Cancelled,
        };
        // A thread that has gone takes the bytes with it, zeroed.
        let _ = reply.send(reply_to_pam);
        true
    }
}

/// `text` cut to at most `max` bytes, at a character boundary.
pub fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests;
