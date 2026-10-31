//! The conductor: one conversation at a time, the rest queued, each turned
//! into `Prompt` frames to the host and its answer into the reply.
//!
//! Everything arrives as an [`Event`] on one channel (the bus's requests and
//! cancels, the host's answers) and one task handles them in order, so the
//! state below has one owner and needs no lock.
//!
//! A conversation's life on the wire (docs/abi.md, *Prompt*): a
//! `PROMPT_INFO` title (the connection's name), a `PROMPT_ERROR` "wrong
//! password" when NetworkManager refused the last answer, one
//! `PROMPT_SECRET`, and exactly one end: `PROMPT_CANCEL` when NetworkManager
//! withdrew the request (`CancelGetSecrets`), `PROMPT_DONE` for everything
//! else, `arg` 1 only for an answer that went into the reply. An answer the
//! panel can already tell is wrong (not text; a WPA-PSK passphrase of the
//! wrong length) is an error line and the ask again, in the same
//! conversation.

use std::collections::VecDeque;

use caustics_bar_abi::wire::{
    Answer, Frame, MAX_NAME_LEN, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ERROR, PROMPT_INFO,
    PROMPT_INFO_TITLE, PROMPT_SECRET, Prompt,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use crate::ask::{self, Ask};
use crate::reply::Secrets;
use crate::witness;

/// Where frames go: stdout in the binary, a channel in a test.
pub type Sink = Box<dyn FnMut(&Frame) -> std::io::Result<()> + Send>;

/// The error line for an answer that is not UTF-8: NetworkManager's
/// secrets are strings.
pub const NOT_TEXT: &str = "The password must be text";
/// The error line for a WPA-PSK passphrase NetworkManager would refuse.
pub const BAD_PSK: &str = "A Wi-Fi password is 8 to 63 characters";

/// How a conversation ended, as the bus method returns it.
#[derive(Debug)]
pub enum Outcome {
    /// What was typed, as the reply.
    Secret(Secrets),
    /// NetworkManager withdrew the request: `….Error.AgentCanceled`.
    Cancelled,
    /// The user dismissed the prompt: `….Error.UserCanceled`.
    Dismissed,
    /// The host went, or could not be written to: `….Error.Failed`.
    Failed,
}

/// One request, as the agent object hands it over.
pub struct Request {
    /// The `connection_path` NetworkManager named it by, which a
    /// `CancelGetSecrets` names it by too.
    pub path: String,
    /// What is asked.
    pub ask: Ask,
    /// Where the outcome goes.
    pub reply: oneshot::Sender<Outcome>,
}

/// Everything the conductor wakes up for.
pub enum Event {
    /// `GetSecrets`, with interaction allowed.
    Get(Request),
    /// `CancelGetSecrets(connection_path, setting_name)`.
    Cancel { path: String, setting: String },
    /// The host's answer (docs/abi.md, *Answer*).
    Answer(Answer),
    /// The host closed stdin.
    Closed,
}

/// The conversation on screen.
struct Active {
    id: u32,
    request: Request,
}

/// What the conductor knows.
pub struct Conductor {
    sink: Sink,
    queue: VecDeque<Request>,
    active: Option<Active>,
    next_id: u32,
    /// Conversations that ended without an answer since this process
    /// started: a count, never a description.
    unanswered: u32,
}

impl Conductor {
    pub fn new(sink: Sink) -> Self {
        Conductor {
            sink,
            queue: VecDeque::new(),
            active: None,
            next_id: 1,
            unanswered: 0,
        }
    }

    /// Handle events until the host closes stdin or the sink fails; then
    /// end what is open as failures. Returns the exit status.
    pub async fn run(mut self, mut inbox: UnboundedReceiver<Event>) -> i32 {
        while let Some(event) = inbox.recv().await {
            let alive = match event {
                Event::Get(request) => self.get(request),
                Event::Cancel { path, setting } => self.cancel(&path, &setting),
                Event::Answer(answer) => self.answer(answer),
                Event::Closed => {
                    witness("stdin closed; exiting");
                    false
                }
            };
            if !alive || !self.start_next() {
                break;
            }
        }
        self.abandon();
        0
    }

    /// The host is gone: no conversation can be answered.
    fn abandon(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active.request.reply.send(Outcome::Failed);
        }
        for request in self.queue.drain(..) {
            let _ = request.reply.send(Outcome::Failed);
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

    fn get(&mut self, request: Request) -> bool {
        if let Some(active) = &self.active {
            witness(&format!(
                "{:?} {}: queued behind conversation {} ({} waiting)",
                request.ask.title,
                request.ask.setting,
                active.id,
                self.queue.len() + 1
            ));
        }
        self.queue.push_back(request);
        true
    }

    /// Start the next queued request while nothing is on screen: after every
    /// event, so a conversation that ends hands over in arrival order.
    fn start_next(&mut self) -> bool {
        if self.active.is_some() {
            return true;
        }
        let Some(request) = self.queue.pop_front() else {
            return true;
        };
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        witness(&format!(
            "conversation {id}: {:?} {}.{}{}",
            request.ask.title,
            request.ask.setting,
            request.ask.key,
            if request.ask.request_new {
                ", the last answer refused"
            } else {
                ""
            }
        ));
        let title = request.ask.title.clone();
        let words = request.ask.words.clone();
        let request_new = request.ask.request_new;
        self.active = Some(Active { id, request });
        let written = self.write(id, PROMPT_INFO, PROMPT_INFO_TITLE, &title)
            && (!request_new || self.write(id, PROMPT_ERROR, 0, ask::WRONG))
            && self.write(id, PROMPT_SECRET, 0, &words);
        if !written {
            self.finish(Outcome::Failed);
        }
        written
    }

    /// End the active conversation: one closing frame, the reply sent.
    fn finish(&mut self, outcome: Outcome) -> bool {
        let Some(active) = self.active.take() else {
            return true;
        };
        let id = active.id;
        let written = match outcome {
            Outcome::Cancelled => self.write(id, PROMPT_CANCEL, 0, ""),
            Outcome::Secret(_) => self.write(id, PROMPT_DONE, 1, ""),
            Outcome::Failed | Outcome::Dismissed => self.write(id, PROMPT_DONE, 0, ""),
        };
        match &outcome {
            Outcome::Secret(secrets) => {
                let (setting, key) = secrets.names();
                witness(&format!(
                    "conversation {id}: answered; {setting}.{key} sent"
                ));
            }
            other => {
                self.unanswered = self.unanswered.saturating_add(1);
                let why = match other {
                    Outcome::Cancelled => "cancelled by NetworkManager",
                    Outcome::Dismissed => "dismissed",
                    _ => "failed",
                };
                witness(&format!(
                    "conversation {id}: {why} ({} unanswered since start)",
                    self.unanswered
                ));
            }
        }
        let _ = active.request.reply.send(outcome);
        written
    }

    fn cancel(&mut self, path: &str, setting: &str) -> bool {
        let matches = |r: &Request| r.path == path && r.ask.setting == setting;
        if let Some(at) = self.queue.iter().position(matches) {
            let request = self.queue.remove(at).expect("found above");
            witness(&format!(
                "{:?} {setting}: cancelled by NetworkManager while queued",
                request.ask.title
            ));
            let _ = request.reply.send(Outcome::Cancelled);
            return true;
        }
        if self.active.as_ref().is_some_and(|a| matches(&a.request)) {
            return self.finish(Outcome::Cancelled);
        }
        witness(&format!(
            "a cancel for {path} {setting}, which this agent is not asking for; ignored"
        ));
        true
    }

    fn answer(&mut self, answer: Answer) -> bool {
        let id = answer.id();
        let Some(active) = self.active.as_ref().filter(|a| a.id == id) else {
            witness(&format!(
                "an answer for conversation {id}, which is not on screen; dropped"
            ));
            return true;
        };
        let bytes = match answer {
            Answer::Cancelled { .. } => return self.finish(Outcome::Dismissed),
            Answer::Text { bytes, .. } => bytes,
        };
        let why = match std::str::from_utf8(bytes.as_bytes()) {
            Err(_) => NOT_TEXT,
            Ok(text) if active.request.ask.psk_rule && !ask::psk_valid(text) => BAD_PSK,
            Ok(text) => {
                let ask = &active.request.ask;
                let secrets = Secrets::new(&ask.setting, &ask.key, text);
                drop(bytes);
                return self.finish(Outcome::Secret(secrets));
            }
        };
        drop(bytes);
        let words = active.request.ask.words.clone();
        witness(&format!(
            "conversation {id}: an answer NetworkManager would refuse; asked again"
        ));
        self.write(id, PROMPT_ERROR, 0, why) && self.write(id, PROMPT_SECRET, 0, &words)
    }
}

/// `text` cut to at most `max` bytes, at a character boundary (a copy of
/// polkit's, which stays its own so that sink is unchanged).
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
