//! The conductor: one conversation at a time, the rest queued, each turned
//! into `Prompt` frames to the host and its answer into the method's reply.
//!
//! Everything arrives as an [`Event`] on one channel (BlueZ's requests,
//! cancels and release, the end of a shown passkey, the host's answers) and
//! one task handles them in order, so the state below has one owner and
//! needs no lock.
//!
//! A conversation's life on the wire (docs/abi.md, *Prompt*): a
//! `PROMPT_INFO` title (the device's alias), then one of
//!
//! - a **confirmation** (`RequestConfirmation`, `RequestAuthorization`,
//!   `AuthorizeService`): a `PROMPT_INFO` line saying what is asked ("Confirm
//!   passkey 123456", "Allow pairing", "Allow Audio Sink") and a
//!   `PROMPT_ECHO` [`ask::CONFIRM`]. The canvas host sends an empty
//!   `ANSWER_TEXT` for Enter on the empty field, which confirms; any text is
//!   an error line and the ask again; `ANSWER_CANCELLED` rejects.
//! - a **question** (`RequestPinCode`, `RequestPasskey`): a `PROMPT_ECHO`
//!   "PIN" or "Passkey"; an answer BlueZ would refuse (a PIN past 16 bytes,
//!   a passkey that is not six digits) is an error line and the ask again.
//! - a **display** (`DisplayPinCode`, `DisplayPasskey`): a `PROMPT_INFO`
//!   line saying what to type on the device, and no ask. The method has
//!   already returned; the conversation is held until the device is paired,
//!   BlueZ cancels or releases the agent, the user dismisses it, or
//!   [`DISPLAY_FOR`] passes (BlueZ sends a display no reply is owed to, so it
//!   never cancels one). A `DisplayPasskey` for the device on screen updates
//!   its line in place.
//!
//! and exactly one end: `PROMPT_CANCEL` when BlueZ withdrew the request
//! (`Cancel`, `Release`), `PROMPT_DONE` for everything else, `arg` 1 for a
//! confirmation confirmed, an answer that went into the reply, or a display
//! whose device paired.
//!
//! **An ask supersedes a display.** BlueZ holds one request per agent at a
//! time, and a display is not a request it is waiting on, so a request that
//! arrives while a display is on screen means BlueZ has moved on: the
//! display ends (`PROMPT_DONE` 0) and the request is put to the user at once.

use std::collections::VecDeque;
use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, Frame, MAX_NAME_LEN, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR,
    PROMPT_INFO, PROMPT_INFO_TITLE, Prompt,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::ask;
use crate::reply::Pin;
use crate::witness;

/// Where frames go: stdout in the binary, a channel in a test.
pub type Sink = Box<dyn FnMut(&Frame) -> std::io::Result<()> + Send>;

/// Started when a display begins, with its conversation's id and the
/// device's path: watch the device and send [`Event::Settled`] when its
/// pairing is over. The conductor drops what it returns when the display
/// ends, which stops the watch.
pub type Watch = Box<dyn FnMut(u32, &str) -> Option<Watching> + Send>;

/// How long a display is held at most when nothing ends it first: past the
/// pairing timeouts of the devices that ask a user to type a code.
pub const DISPLAY_FOR: Duration = Duration::from_secs(60);

/// A watch's task, stopped when this drops.
pub struct Watching(pub JoinHandle<()>);

impl Drop for Watching {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How a conversation ended, as the bus method returns it.
#[derive(Debug)]
pub enum Outcome {
    /// A confirmation confirmed: the method returns.
    Confirmed,
    /// The PIN typed, as the reply.
    Pin(Pin),
    /// The passkey typed, as the reply.
    Passkey(u32),
    /// The user dismissed the prompt: `org.bluez.Error.Rejected`.
    Rejected,
    /// BlueZ withdrew the request (`Cancel`, `Release`):
    /// `org.bluez.Error.Canceled`.
    Canceled,
    /// The host went, or could not be written to: `org.bluez.Error.Rejected`,
    /// with words that say so.
    Failed,
}

/// What a request asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Question {
    /// Confirm or reject; `line` says what.
    Confirm { line: String },
    /// A legacy PIN, 1 to 16 characters.
    Pin,
    /// A six-digit passkey.
    Passkey,
}

impl Question {
    /// What this request is called in a witness: never a passkey.
    fn name(&self) -> &'static str {
        match self {
            Question::Confirm { .. } => "a confirmation",
            Question::Pin => "a PIN",
            Question::Passkey => "a passkey",
        }
    }
}

/// One request BlueZ waits on, as the agent object hands it over.
pub struct Request {
    /// The device's object path.
    pub device: String,
    /// The panel's title: the device's alias, else its address.
    pub title: String,
    pub question: Question,
    /// Where the outcome goes.
    pub reply: oneshot::Sender<Outcome>,
}

/// A code to type on the device, which BlueZ waits on no reply for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Display {
    pub device: String,
    pub title: String,
    /// What the line says ([`ask::type_on_device`]).
    pub line: String,
}

/// Everything the conductor wakes up for.
pub enum Event {
    /// A request BlueZ waits on.
    Ask(Request),
    /// `DisplayPinCode` or `DisplayPasskey`.
    Display(Display),
    /// `Cancel()`: the request on screen is withdrawn.
    Cancel,
    /// `Release()`: BlueZ has unregistered this agent.
    Release,
    /// The display under `id` is over: its device paired, or
    /// [`DISPLAY_FOR`] passed. `why` is for the witness.
    Settled { id: u32, why: &'static str },
    /// The host's answer (docs/abi.md, *Answer*).
    Answer(Answer),
    /// The host closed stdin.
    Closed,
}

/// Something waiting its turn.
enum Queued {
    Ask(Request),
    Display(Display),
}

impl Queued {
    fn title(&self) -> &str {
        match self {
            Queued::Ask(request) => &request.title,
            Queued::Display(display) => &display.title,
        }
    }
}

/// What the conversation on screen is.
enum Held {
    Ask(Request),
    Display {
        device: String,
        _watching: Option<Watching>,
    },
}

/// The conversation on screen.
struct Active {
    id: u32,
    held: Held,
}

/// What the conductor knows.
pub struct Conductor {
    sink: Sink,
    watch: Watch,
    queue: VecDeque<Queued>,
    active: Option<Active>,
    next_id: u32,
    /// Conversations that ended without an answer since this process
    /// started: a count, never a description.
    unanswered: u32,
}

impl Conductor {
    /// A conductor that watches no device: a display is held until
    /// [`DISPLAY_FOR`]'s end comes from elsewhere, or a cancel. The tests'.
    pub fn new(sink: Sink) -> Self {
        Conductor::with_watch(sink, Box::new(|_, _| None))
    }

    pub fn with_watch(sink: Sink, watch: Watch) -> Self {
        Conductor {
            sink,
            watch,
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
                Event::Ask(request) => self.ask(request),
                Event::Display(display) => self.display(display),
                Event::Cancel => self.withdraw("cancelled by BlueZ"),
                Event::Release => self.withdraw("released by BlueZ"),
                Event::Settled { id, why } => self.settled(id, why),
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
        if let Some(Active {
            held: Held::Ask(request),
            ..
        }) = self.active.take()
        {
            let _ = request.reply.send(Outcome::Failed);
        }
        for queued in self.queue.drain(..) {
            if let Queued::Ask(request) = queued {
                let _ = request.reply.send(Outcome::Failed);
            }
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

    fn ask(&mut self, request: Request) -> bool {
        match self.active.as_ref() {
            Some(Active {
                held: Held::Display { .. },
                ..
            }) => {
                // BlueZ has moved on from the code it showed: the request
                // goes first.
                let ended = self.finish(Outcome::Rejected, Some("superseded by a request"));
                self.queue.push_front(Queued::Ask(request));
                ended
            }
            Some(active) => {
                witness(&format!(
                    "{:?} {}: queued behind conversation {} ({} waiting)",
                    request.title,
                    request.question.name(),
                    active.id,
                    self.queue.len() + 1
                ));
                self.queue.push_back(Queued::Ask(request));
                true
            }
            None => {
                self.queue.push_back(Queued::Ask(request));
                true
            }
        }
    }

    fn display(&mut self, display: Display) -> bool {
        match self.active.as_ref() {
            // The same device's display again: `DisplayPasskey` says how
            // much has been typed. The line changes in place.
            Some(Active {
                id,
                held: Held::Display { device, .. },
            }) if *device == display.device => {
                let id = *id;
                self.write(id, PROMPT_INFO, 0, &display.line)
            }
            // Another device's code: BlueZ has moved on from this one.
            Some(Active {
                held: Held::Display { .. },
                ..
            }) => {
                let ended = self.finish(Outcome::Rejected, Some("superseded by another device's"));
                self.queue.push_front(Queued::Display(display));
                ended
            }
            _ => {
                self.queue.push_back(Queued::Display(display));
                true
            }
        }
    }

    /// Start the next queued request while nothing is on screen: after every
    /// event, so a conversation that ends hands over in arrival order.
    fn start_next(&mut self) -> bool {
        if self.active.is_some() {
            return true;
        }
        let Some(queued) = self.queue.pop_front() else {
            return true;
        };
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let title = queued.title().to_owned();
        let written = match queued {
            Queued::Ask(request) => {
                witness(&format!(
                    "conversation {id}: {:?} asks {}",
                    request.title,
                    request.question.name()
                ));
                let question = request.question.clone();
                self.active = Some(Active {
                    id,
                    held: Held::Ask(request),
                });
                self.write(id, PROMPT_INFO, PROMPT_INFO_TITLE, &title)
                    && match &question {
                        Question::Confirm { line } => {
                            self.write(id, PROMPT_INFO, 0, line)
                                && self.write(id, PROMPT_ECHO, 0, ask::CONFIRM)
                        }
                        Question::Pin => self.write(id, PROMPT_ECHO, 0, ask::PIN),
                        Question::Passkey => self.write(id, PROMPT_ECHO, 0, ask::PASSKEY),
                    }
            }
            Queued::Display(display) => {
                witness(&format!(
                    "conversation {id}: {:?} shows a code to type on the device",
                    display.title
                ));
                let watching = (self.watch)(id, &display.device);
                self.active = Some(Active {
                    id,
                    held: Held::Display {
                        device: display.device,
                        _watching: watching,
                    },
                });
                self.write(id, PROMPT_INFO, PROMPT_INFO_TITLE, &title)
                    && self.write(id, PROMPT_INFO, 0, &display.line)
            }
        };
        if !written {
            self.finish(Outcome::Failed, Some("not shown"));
        }
        written
    }

    /// End the active conversation: one closing frame, and for a request the
    /// reply sent. `why` is a display's witness.
    fn finish(&mut self, outcome: Outcome, why: Option<&str>) -> bool {
        let Some(active) = self.active.take() else {
            return true;
        };
        let id = active.id;
        let written = match outcome {
            Outcome::Canceled => self.write(id, PROMPT_CANCEL, 0, ""),
            Outcome::Confirmed | Outcome::Pin(_) | Outcome::Passkey(_) => {
                self.write(id, PROMPT_DONE, 1, "")
            }
            Outcome::Rejected | Outcome::Failed => self.write(id, PROMPT_DONE, 0, ""),
        };
        let request = match active.held {
            Held::Display { .. } => {
                witness(&format!(
                    "conversation {id}: the code is gone ({})",
                    why.unwrap_or("ended")
                ));
                return written;
            }
            Held::Ask(request) => request,
        };
        let said = match &outcome {
            Outcome::Confirmed => "confirmed",
            Outcome::Pin(_) => "answered; the PIN sent",
            Outcome::Passkey(_) => "answered; the passkey sent",
            Outcome::Rejected => "rejected",
            Outcome::Canceled => why.unwrap_or("cancelled by BlueZ"),
            Outcome::Failed => "failed",
        };
        if matches!(
            outcome,
            Outcome::Rejected | Outcome::Canceled | Outcome::Failed
        ) {
            self.unanswered = self.unanswered.saturating_add(1);
            witness(&format!(
                "conversation {id}: {said} ({} unanswered since start)",
                self.unanswered
            ));
        } else {
            witness(&format!("conversation {id}: {said}"));
        }
        let _ = request.reply.send(outcome);
        written
    }

    /// `Cancel` or `Release`: BlueZ holds one request per agent and names
    /// none, so everything this agent holds ends. The one on screen ends
    /// with `PROMPT_CANCEL`; a queued request is answered with no frame (the
    /// host never saw it), and a queued display is dropped.
    fn withdraw(&mut self, why: &'static str) -> bool {
        for queued in self.queue.drain(..) {
            if let Queued::Ask(request) = queued {
                witness(&format!(
                    "{:?} {}: {why} while queued",
                    request.title,
                    request.question.name()
                ));
                let _ = request.reply.send(Outcome::Canceled);
            }
        }
        if self.active.is_none() {
            witness(&format!("{why} with nothing on screen; nothing to end"));
            return true;
        }
        self.finish(Outcome::Canceled, Some(why))
    }

    fn settled(&mut self, id: u32, why: &'static str) -> bool {
        let shown = self
            .active
            .as_ref()
            .is_some_and(|a| a.id == id && matches!(a.held, Held::Display { .. }));
        if !shown {
            return true;
        }
        let outcome = if why == PAIRED {
            Outcome::Confirmed
        } else {
            Outcome::Failed
        };
        self.finish(outcome, Some(why))
    }

    fn answer(&mut self, answer: Answer) -> bool {
        let id = answer.id();
        let Some(active) = self.active.as_ref().filter(|a| a.id == id) else {
            witness(&format!(
                "an answer for conversation {id}, which is not on screen; dropped"
            ));
            return true;
        };
        let question = match &active.held {
            Held::Display { .. } => {
                return match answer {
                    Answer::Cancelled { .. } => self.finish(Outcome::Rejected, Some("dismissed")),
                    Answer::Text { .. } => {
                        witness(&format!(
                            "conversation {id}: text for a code that asks nothing; dropped"
                        ));
                        true
                    }
                };
            }
            Held::Ask(request) => request.question.clone(),
        };
        let bytes = match answer {
            Answer::Cancelled { .. } => return self.finish(Outcome::Rejected, None),
            Answer::Text { bytes, .. } => bytes,
        };
        let (why, words) = match &question {
            Question::Confirm { .. } if bytes.as_bytes().is_empty() => {
                return self.finish(Outcome::Confirmed, None);
            }
            Question::Confirm { .. } => (ask::CONFIRM_EMPTY, ask::CONFIRM),
            Question::Pin => match std::str::from_utf8(bytes.as_bytes()) {
                Ok(text) if ask::pin_valid(text) => {
                    let pin = Pin::new(text);
                    drop(bytes);
                    return self.finish(Outcome::Pin(pin), None);
                }
                Ok(_) => (ask::BAD_PIN, ask::PIN),
                Err(_) => (ask::PIN_NOT_TEXT, ask::PIN),
            },
            Question::Passkey => match ask::passkey(bytes.as_bytes()) {
                Some(passkey) => {
                    drop(bytes);
                    return self.finish(Outcome::Passkey(passkey), None);
                }
                None => (ask::BAD_PASSKEY, ask::PASSKEY),
            },
        };
        drop(bytes);
        witness(&format!(
            "conversation {id}: an answer BlueZ would refuse; asked again"
        ));
        self.write(id, PROMPT_ERROR, 0, why) && self.write(id, PROMPT_ECHO, 0, words)
    }
}

/// [`Event::Settled`]'s `why` for a device that paired: the one ending of a
/// display that is a success.
pub const PAIRED: &str = "paired";
/// [`Event::Settled`]'s `why` for a display nothing ended in
/// [`DISPLAY_FOR`].
pub const TIMED_OUT: &str = "timed out";

/// `text` cut to at most `max` bytes, at a character boundary (a copy of
/// the secret agent's, which stays its own so that sink is unchanged).
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
