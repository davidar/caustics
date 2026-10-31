//! The conductor: one typed question at a time, each turned into `Prompt`
//! frames to the host and its answer into the socket's reply.
//!
//! Everything arrives as an [`Event`] on one channel (the socket's commands,
//! the host's answers, stdin closing) and one thread handles them in order,
//! so the state below has one owner and needs no lock.
//!
//! A conversation's life on the wire (docs/abi.md, *Prompt*): a
//! `PROMPT_INFO` title (the device's name, as the Bluetooth sidecar sent it),
//! a `PROMPT_ECHO` "PIN" or "Passkey", the host's `Answer`, and exactly one
//! end: `PROMPT_DONE` 1 for an answer that went into the reply, `PROMPT_DONE`
//! 0 for a dismissal, `PROMPT_CANCEL` for a `cancel` (BlueZ withdrew the
//! question). An answer BlueZ would refuse (a PIN past 16 bytes, a passkey
//! that is not six digits) is a `PROMPT_ERROR` line and the ask again.
//!
//! **The reply** is one line, built once in a buffer zeroed when it drops:
//! `text <what was typed>`, `cancelled`, or `error <why>`. A second question
//! while one is asked is answered `error busy` at once and nothing is shown:
//! BlueZ holds one request per agent, so a second means the asker has lost
//! track, and the one on screen is the person's.

use std::sync::mpsc::{Receiver, Sender};

use caustics_bar_abi::wire::{
    Answer, Frame, MAX_NAME_LEN, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR,
    PROMPT_INFO, PROMPT_INFO_TITLE, Prompt,
};
use zeroize::Zeroizing;

use crate::ask;
use crate::witness;

/// Where frames go: stdout in the binary, a channel in a test.
pub type Sink = Box<dyn FnMut(&Frame) -> std::io::Result<()> + Send>;

/// One reply line, its newline included, zeroed when it drops.
pub type Reply = Zeroizing<Vec<u8>>;

/// What a question asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Question {
    /// A legacy PIN, 1 to 16 bytes.
    Pin,
    /// A six-digit passkey.
    Passkey,
}

impl Question {
    /// What this question is called in a witness.
    pub fn name(self) -> &'static str {
        match self {
            Question::Pin => "a PIN",
            Question::Passkey => "a passkey",
        }
    }

    /// The ask on the prompt.
    fn words(self) -> &'static str {
        match self {
            Question::Pin => ask::PIN,
            Question::Passkey => ask::PASSKEY,
        }
    }
}

/// One line of the socket's language (`crate::socket`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `pin <title>` or `passkey <title>`.
    Ask { question: Question, title: String },
    /// `cancel`: the question asked is withdrawn.
    Cancel,
    /// `status`.
    Status,
}

/// Everything the conductor wakes up for.
pub enum Event {
    /// A command from the socket, and where its reply goes.
    Command(Command, Sender<Reply>),
    /// The host's answer (docs/abi.md, *Answer*).
    Answer(Answer),
    /// The host closed stdin.
    Closed,
}

/// The conversation on screen.
struct Active {
    id: u32,
    question: Question,
    reply: Sender<Reply>,
}

/// What the conductor knows.
pub struct Conductor {
    sink: Sink,
    active: Option<Active>,
    next_id: u32,
    /// Questions that ended without an answer since this process started: a
    /// count, never a description.
    unanswered: u32,
}

/// A reply line from words that hold no secret.
pub fn line(words: &str) -> Reply {
    let mut out = Zeroizing::new(Vec::with_capacity(words.len() + 1));
    out.extend_from_slice(words.as_bytes());
    out.push(b'\n');
    out
}

/// `text <typed>`: built once, at its full size, so it never moves.
fn text_line(typed: &[u8]) -> Reply {
    let mut out = Zeroizing::new(Vec::with_capacity(5 + typed.len() + 1));
    out.extend_from_slice(b"text ");
    out.extend_from_slice(typed);
    out.push(b'\n');
    out
}

/// The reply to an ask when the sink is going.
pub const LEAVING: &str = "error the pairing sink is leaving";
/// The reply to an ask while one is asked.
pub const BUSY: &str = "error busy";

/// [`Conductor::finish`]'s `why` for the person dismissing the prompt.
const DISMISSED: &str = "dismissed";

impl Conductor {
    pub fn new(sink: Sink) -> Self {
        Conductor {
            sink,
            active: None,
            next_id: 1,
            unanswered: 0,
        }
    }

    /// Handle events until the host closes stdin or the sink fails; then
    /// end what is open. Returns the exit status.
    pub fn run(mut self, inbox: Receiver<Event>) -> i32 {
        while let Ok(event) = inbox.recv() {
            let alive = match event {
                Event::Command(command, reply) => self.command(command, reply),
                Event::Answer(answer) => self.answer(answer),
                Event::Closed => {
                    witness("stdin closed; exiting");
                    false
                }
            };
            if !alive {
                break;
            }
        }
        if let Some(active) = self.active.take() {
            let _ = active.reply.send(line(LEAVING));
        }
        0
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

    fn command(&mut self, command: Command, reply: Sender<Reply>) -> bool {
        match command {
            Command::Status => {
                let asking = if self.active.is_some() { "yes" } else { "no" };
                let _ = reply.send(line(&format!("ok asking={asking}")));
                true
            }
            Command::Cancel => {
                let _ = reply.send(line("ok"));
                if self.active.is_none() {
                    witness("cancelled with nothing on screen; nothing to end");
                    return true;
                }
                self.finish(None, "cancelled by BlueZ")
            }
            Command::Ask { question, title } => {
                if let Some(active) = &self.active {
                    witness(&format!(
                        "{title:?} asks {}: busy with conversation {}; refused",
                        question.name(),
                        active.id
                    ));
                    let _ = reply.send(line(BUSY));
                    return true;
                }
                let id = self.next_id;
                self.next_id = self.next_id.checked_add(1).unwrap_or(1);
                witness(&format!(
                    "conversation {id}: {title:?} asks {}",
                    question.name()
                ));
                self.active = Some(Active {
                    id,
                    question,
                    reply,
                });
                let shown = self.write(id, PROMPT_INFO, PROMPT_INFO_TITLE, &title)
                    && self.write(id, PROMPT_ECHO, 0, question.words());
                if !shown && let Some(active) = self.active.take() {
                    let _ = active
                        .reply
                        .send(line("error the prompt could not be shown"));
                }
                shown
            }
        }
    }

    /// End the conversation on screen. `typed` is the answer that goes into
    /// the reply; `None` is a dismissal ([`DISMISSED`]) or a cancel, as
    /// `why` says.
    fn finish(&mut self, typed: Option<&[u8]>, why: &str) -> bool {
        let Some(active) = self.active.take() else {
            return true;
        };
        let id = active.id;
        let (written, reply) = match typed {
            Some(typed) => (self.write(id, PROMPT_DONE, 1, ""), text_line(typed)),
            None if why == DISMISSED => (self.write(id, PROMPT_DONE, 0, ""), line("cancelled")),
            None => (self.write(id, PROMPT_CANCEL, 0, ""), line("cancelled")),
        };
        if typed.is_some() {
            witness(&format!(
                "conversation {id}: answered; {} sent",
                match active.question {
                    Question::Pin => "the PIN",
                    Question::Passkey => "the passkey",
                }
            ));
        } else {
            self.unanswered = self.unanswered.saturating_add(1);
            witness(&format!(
                "conversation {id}: {why} ({} unanswered since start)",
                self.unanswered
            ));
        }
        let _ = active.reply.send(reply);
        written
    }

    fn answer(&mut self, answer: Answer) -> bool {
        let id = answer.id();
        let Some(question) = self
            .active
            .as_ref()
            .filter(|a| a.id == id)
            .map(|a| a.question)
        else {
            witness(&format!(
                "an answer for conversation {id}, which is not on screen; dropped"
            ));
            return true;
        };
        let bytes = match answer {
            Answer::Cancelled { .. } => return self.finish(None, DISMISSED),
            Answer::Text { bytes, .. } => bytes,
        };
        let why = match question {
            Question::Pin => match std::str::from_utf8(bytes.as_bytes()) {
                Ok(text) if ask::pin_valid(text) => {
                    return self.finish(Some(bytes.as_bytes()), "");
                }
                Ok(_) => ask::BAD_PIN,
                Err(_) => ask::PIN_NOT_TEXT,
            },
            Question::Passkey if ask::passkey_valid(bytes.as_bytes()) => {
                return self.finish(Some(bytes.as_bytes()), "");
            }
            Question::Passkey => ask::BAD_PASSKEY,
        };
        drop(bytes);
        witness(&format!(
            "conversation {id}: an answer BlueZ would refuse; asked again"
        ));
        self.write(id, PROMPT_ERROR, 0, why) && self.write(id, PROMPT_ECHO, 0, question.words())
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
