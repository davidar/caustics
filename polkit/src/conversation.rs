//! The conductor: one conversation at a time, the rest queued, each turned
//! into `Prompt` frames to the host and driven through one helper.
//!
//! Everything arrives as an [`Event`] on one channel (the bus's requests and
//! cancels, the host's answers, the helper's lines) and one task handles
//! them in order, so the state below has one owner and needs no lock.
//!
//! A conversation's life on the wire (docs/abi.md, *Prompt*): a
//! `PROMPT_INFO` title (polkit's message), then the helper's asks and lines
//! as it writes them, then exactly one end: `PROMPT_CANCEL` when the
//! requester withdrew it (`CancelAuthentication`), `PROMPT_DONE` for
//! everything else, with `arg` 1 only for a helper that said `SUCCESS` and
//! exited 0.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;

use caustics_bar_abi::wire::{
    Answer, Frame, MAX_NAME_LEN, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR,
    PROMPT_INFO, PROMPT_INFO_TITLE, PROMPT_SECRET, Prompt,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use crate::helper::{self, Helper, Line, Out};
use crate::witness;

/// Where frames go: stdout in the binary, a channel in a test.
pub type Sink = Box<dyn FnMut(&Frame) -> std::io::Result<()> + Send>;

/// How a conversation ended, as the bus method returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The helper said `SUCCESS` and exited 0: polkitd has been told.
    Authorized,
    /// It did not: a wrong answer, a helper that died or could not start,
    /// an answer it could not take, the host gone.
    Failed,
    /// The requester withdrew it.
    Cancelled,
    /// The user dismissed the prompt.
    Dismissed,
}

/// One request, as the agent object hands it over.
pub struct Begin {
    /// polkit's action id, for the witness.
    pub action_id: String,
    /// polkit's message for the action: the panel's title.
    pub message: String,
    /// The user to authenticate as, by name: the helper's argument.
    pub user: String,
    /// Which uid that is, for the witness.
    pub uid: u32,
    /// The cookie polkitd gave. Never witnessed.
    pub cookie: String,
    /// Where the outcome goes.
    pub reply: oneshot::Sender<Outcome>,
}

/// Everything the conductor wakes up for.
pub enum Event {
    /// `BeginAuthentication`.
    Begin(Begin),
    /// `CancelAuthentication(cookie)`.
    Cancel { cookie: String },
    /// The host's answer (docs/abi.md, *Answer*).
    Answer(Answer),
    /// The helper of conversation `id` wrote a line or ended.
    Helper { id: u32, out: Out },
    /// The host closed stdin.
    Closed,
}

/// The conversation on screen.
struct Active {
    id: u32,
    begin: Begin,
    helper: Option<Helper>,
    /// The last line the helper wrote was an ask, and no answer has gone.
    asking: bool,
    /// The helper wrote `SUCCESS`.
    success: bool,
    /// An answer has gone to this helper run: its `FAILURE` is a wrong
    /// answer, not a helper that could not ask.
    answered: bool,
    /// Helper runs that ended in `FAILURE` after an answer, so far: a wrong
    /// answer runs the helper again under the same conversation, up to
    /// [`MAX_ATTEMPTS`].
    attempts: u32,
}

/// How many answers one request may take before it fails: a mistyped
/// password is an error line and a fresh ask in the same conversation, as
/// the desktop agents allow it, and the request fails only after this many.
/// polkitd's cookie stays good for the request, so each attempt is one more
/// run of the helper with the same cookie.
pub const MAX_ATTEMPTS: u32 = 3;

/// What the conductor knows.
pub struct Conductor {
    helper_path: PathBuf,
    /// Set over this process's environment for every helper: nothing in the
    /// binary, a fake helper's settings in a test.
    helper_env: Vec<(OsString, OsString)>,
    sink: Sink,
    events: UnboundedSender<Event>,
    queue: VecDeque<Begin>,
    active: Option<Active>,
    next_id: u32,
    /// Failed conversations since this process started: a count, never a
    /// description (docs/canvas.md, *The prompt core*).
    failed: u32,
    /// The session is locked. Nothing sets it yet: there is no lock
    /// (docs/canvas.md, *Order*). When the lock sidecar exists, the host
    /// says so on this sidecar's stdin, and a request that arrives while it
    /// is set waits in the queue until it is not (docs/canvas.md, *How each
    /// job is wired*): [`Conductor::start_next`] starts nothing while it is
    /// set, and clearing it is followed by a call to it.
    locked: bool,
}

impl Conductor {
    /// A conductor whose helpers report back on `events`.
    pub fn new(helper_path: PathBuf, sink: Sink, events: UnboundedSender<Event>) -> Self {
        Conductor {
            helper_path,
            helper_env: Vec::new(),
            sink,
            events,
            queue: VecDeque::new(),
            active: None,
            next_id: 1,
            failed: 0,
            locked: false,
        }
    }

    /// The same, with `env` set for every helper it spawns.
    pub fn with_helper_env(mut self, env: Vec<(OsString, OsString)>) -> Self {
        self.helper_env = env;
        self
    }

    /// Handle events until the host closes stdin or the sink fails; then
    /// end what is open as failures. Returns the exit status.
    pub async fn run(mut self, mut inbox: UnboundedReceiver<Event>) -> i32 {
        while let Some(event) = inbox.recv().await {
            let alive = match event {
                Event::Begin(begin) => self.begin(begin).await,
                Event::Cancel { cookie } => self.cancel(&cookie).await,
                Event::Answer(answer) => self.answer(answer).await,
                Event::Helper { id, out } => self.helper(id, out).await,
                Event::Closed => {
                    witness("stdin closed; exiting");
                    false
                }
            };
            if !alive || !self.start_next().await {
                break;
            }
        }
        self.abandon().await;
        0
    }

    /// The host is gone: no conversation can be answered.
    async fn abandon(&mut self) {
        if let Some(active) = self.active.take() {
            if let Some(helper) = active.helper {
                helper.kill().await;
            }
            let _ = active.begin.reply.send(Outcome::Failed);
        }
        for begin in self.queue.drain(..) {
            let _ = begin.reply.send(Outcome::Failed);
        }
    }

    fn write(&mut self, id: u32, kind: u32, arg: u32, message: &str) -> bool {
        let frame = Frame::Prompt(Prompt {
            id,
            kind,
            arg,
            message: cut(message, MAX_NAME_LEN).to_owned(),
        });
        match (self.sink)(&frame) {
            Ok(()) => true,
            Err(err) => {
                witness(&format!("stdout: {err}; exiting"));
                false
            }
        }
    }

    async fn begin(&mut self, begin: Begin) -> bool {
        if self.active.is_some() || self.locked {
            witness(&format!(
                "{} for uid {}: queued behind {} ({} waiting)",
                begin.action_id,
                begin.uid,
                if self.locked {
                    "the lock".to_owned()
                } else {
                    format!("conversation {}", self.active.as_ref().map_or(0, |a| a.id))
                },
                self.queue.len() + 1
            ));
        }
        self.queue.push_back(begin);
        true
    }

    /// Start queued requests while nothing is on screen and the session is
    /// not locked: after every event, so a conversation that ends (or never
    /// got its helper) hands over to the next in arrival order.
    async fn start_next(&mut self) -> bool {
        while self.active.is_none() && !self.locked {
            let Some(begin) = self.queue.pop_front() else {
                break;
            };
            if !self.start(begin).await {
                return false;
            }
        }
        true
    }

    async fn start(&mut self, begin: Begin) -> bool {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        witness(&format!(
            "conversation {id}: {} as uid {}",
            begin.action_id, begin.uid
        ));
        if !self.write(id, PROMPT_INFO, PROMPT_INFO_TITLE, &begin.message) {
            let _ = begin.reply.send(Outcome::Failed);
            return false;
        }
        let events = self.events.clone();
        let spawned = Helper::spawn(
            &self.helper_path,
            &self.helper_env,
            &begin.user,
            &begin.cookie,
            move |out| events.send(Event::Helper { id, out }).is_ok(),
        )
        .await;
        let helper = match spawned {
            Ok(helper) => Some(helper),
            Err(err) => {
                witness(&format!(
                    "conversation {id}: the helper {} would not start: {err}",
                    self.helper_path.display()
                ));
                None
            }
        };
        let started = helper.is_some();
        self.active = Some(Active {
            id,
            begin,
            helper,
            asking: false,
            success: false,
            answered: false,
            attempts: 0,
        });
        if started {
            true
        } else {
            self.finish(Outcome::Failed).await
        }
    }

    /// End the active conversation: one closing frame, the helper gone, the
    /// reply sent. The run loop starts the next.
    async fn finish(&mut self, outcome: Outcome) -> bool {
        let Some(active) = self.active.take() else {
            return true;
        };
        if let Some(helper) = active.helper {
            helper.kill().await;
        }
        let id = active.id;
        let written = match outcome {
            Outcome::Cancelled => self.write(id, PROMPT_CANCEL, 0, ""),
            Outcome::Authorized => self.write(id, PROMPT_DONE, 1, ""),
            Outcome::Failed | Outcome::Dismissed => self.write(id, PROMPT_DONE, 0, ""),
        };
        match outcome {
            Outcome::Authorized => witness(&format!("conversation {id}: authorized")),
            Outcome::Cancelled => {
                witness(&format!("conversation {id}: cancelled by its requester"))
            }
            Outcome::Dismissed => witness(&format!("conversation {id}: dismissed")),
            Outcome::Failed => {
                self.failed = self.failed.saturating_add(1);
                witness(&format!(
                    "conversation {id}: failed ({} failed since start)",
                    self.failed
                ));
            }
        }
        let _ = active.begin.reply.send(outcome);
        written
    }

    async fn cancel(&mut self, cookie: &str) -> bool {
        if let Some(at) = self.queue.iter().position(|b| b.cookie == cookie) {
            let begin = self.queue.remove(at).expect("found above");
            witness(&format!(
                "{} for uid {}: cancelled by its requester while queued",
                begin.action_id, begin.uid
            ));
            let _ = begin.reply.send(Outcome::Cancelled);
            return true;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.begin.cookie == cookie)
        {
            return self.finish(Outcome::Cancelled).await;
        }
        witness("a cancel for no conversation of this agent's; ignored");
        true
    }

    async fn answer(&mut self, answer: Answer) -> bool {
        let id = answer.id();
        let Some(active) = self.active.as_mut().filter(|a| a.id == id) else {
            witness(&format!(
                "an answer for conversation {id}, which is not on screen; dropped"
            ));
            return true;
        };
        match answer {
            Answer::Cancelled { .. } => self.finish(Outcome::Dismissed).await,
            Answer::Text { bytes, .. } => {
                if !active.asking {
                    witness(&format!(
                        "an answer for conversation {id}, which is not asking; dropped"
                    ));
                    return true;
                }
                let line = match helper::answer_line(bytes.as_bytes()) {
                    Ok(line) => line,
                    Err(why) => {
                        witness(&format!(
                            "conversation {id}: an answer the helper cannot take ({why:?}); \
                             ending it"
                        ));
                        return self.finish(Outcome::Failed).await;
                    }
                };
                drop(bytes);
                active.asking = false;
                active.answered = true;
                let written = match active.helper.as_mut() {
                    Some(helper) => helper.answer(line).await,
                    None => Err(std::io::ErrorKind::BrokenPipe.into()),
                };
                if let Err(err) = written {
                    witness(&format!(
                        "conversation {id}: the helper's stdin: {err}; ending it"
                    ));
                    return self.finish(Outcome::Failed).await;
                }
                true
            }
        }
    }

    async fn helper(&mut self, id: u32, out: Out) -> bool {
        let Some(active) = self.active.as_mut().filter(|a| a.id == id) else {
            // A line from a helper that was already killed.
            return true;
        };
        let line = match out {
            Out::Line(line) => line,
            Out::Ended => {
                let helper = active.helper.take();
                let status = match helper {
                    Some(helper) => helper.wait().await.ok(),
                    None => None,
                };
                let ok = active.success && status.is_some_and(|s| s.success());
                if active.success && !ok {
                    witness(&format!(
                        "conversation {id}: the helper said SUCCESS and exited {}",
                        status.map_or_else(|| "unseen".to_owned(), |s| s.to_string())
                    ));
                }
                if ok {
                    return self.finish(Outcome::Authorized).await;
                }
                // A wrong answer: the helper said FAILURE after an ask was
                // answered. The request is still polkitd's and the cookie
                // still good, so the helper runs again under the same
                // conversation; the error line it wrote is already on its way
                // to the host, and the next ask follows it.
                let again =
                    (active.answered && !active.success && active.attempts + 1 < MAX_ATTEMPTS)
                        .then(|| {
                            active.attempts += 1;
                            active.answered = false;
                            active.asking = false;
                            (
                                active.attempts,
                                active.begin.user.clone(),
                                active.begin.cookie.clone(),
                            )
                        });
                let Some((attempts, user, cookie)) = again else {
                    return self.finish(Outcome::Failed).await;
                };
                witness(&format!(
                    "conversation {id}: attempt {attempts} failed; the helper runs again"
                ));
                let events = self.events.clone();
                let spawned = Helper::spawn(
                    &self.helper_path,
                    &self.helper_env,
                    &user,
                    &cookie,
                    move |out| events.send(Event::Helper { id, out }).is_ok(),
                )
                .await;
                return match spawned {
                    Ok(helper) => {
                        if let Some(active) = self.active.as_mut() {
                            active.helper = Some(helper);
                        }
                        true
                    }
                    Err(err) => {
                        witness(&format!(
                            "conversation {id}: the helper {} would not start again: {err}",
                            self.helper_path.display()
                        ));
                        self.finish(Outcome::Failed).await
                    }
                };
            }
        };
        match line {
            Line::EchoOff(text) => {
                active.asking = true;
                self.write(id, PROMPT_SECRET, 0, &text)
            }
            Line::EchoOn(text) => {
                active.asking = true;
                self.write(id, PROMPT_ECHO, 0, &text)
            }
            Line::Info(text) => self.write(id, PROMPT_INFO, 0, &text),
            Line::Error(text) => self.write(id, PROMPT_ERROR, 0, &text),
            Line::Success => {
                active.success = true;
                true
            }
            Line::Failure => {
                active.success = false;
                true
            }
            Line::Unknown(len) => {
                witness(&format!(
                    "conversation {id}: a line of {len} bytes the helper protocol has not got; \
                     ending it"
                ));
                self.finish(Outcome::Failed).await
            }
        }
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
