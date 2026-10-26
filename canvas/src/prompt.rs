//! The prompt core: the host's half of a conversation (docs/canvas.md, *The
//! prompt core*; docs/abi.md, *Prompt*, *Answer*).
//!
//! A sink sidecar asks over the wire: a title, asks and lines, and one end.
//! This module is the state machine that turns those frames and the keys
//! into what the host does: raise the prompt's panes, lower them, write an
//! answer down the asking sidecar's stdin, and what the caustic is told
//! (`CanvasPrompt` and the text behind it). It owns no Wayland object and
//! no pipe; [`Effect`]s say what to do, and `wayland.rs` does it.
//!
//! ```text
//! Held ─ask→ Asking ─Return→ Verifying ─error line→ Failed ─ask→ Asking …
//!   │           │                │                     │
//!   └───────────┴── done or cancel: the panes go, the conversation ends ──┘
//!               └── Escape, any time the panes are up: ANSWER_CANCELLED, the panes go
//! ```
//!
//! - **Held**: a conversation whose title (or a line) has arrived and no ask
//!   yet. The panes are up on every output with the title and no field, and
//!   the keyboard is taken, so that the user sees at once that something is
//!   wanted of them: a PAM stack that tries a fingerprint reader first asks
//!   for the password only after that module's timeout, and a conversation
//!   that drew nothing until then looked like nothing had happened. Escape
//!   dismisses; other keys do nothing until the ask.
//! - **Asking**: the panes are up on every output and keys type into the
//!   [`Entry`]. Return writes `Answer::Text` (one write; the entry is zeroed
//!   as the answer leaves it) and goes on to **Verifying**, where the dots
//!   are frozen and keys are ignored. An error line then is a failed attempt,
//!   counted and never described: **Failed**, until the sidecar asks again
//!   or ends it.
//! - **One conversation at a time.** A `Prompt` under any other id, or from
//!   any other sidecar, while one is held or up is answered
//!   `ANSWER_CANCELLED` at once (the sidecars queue their own; this is the
//!   backstop). An end for a conversation that is not this one is ignored.
//! - **Escape** dismisses: `ANSWER_CANCELLED`, the panes go, and the
//!   conversation is **Dismissed** until its end arrives; any ask under it
//!   meanwhile is dismissed again, and a new conversation replaces it.
//! - The sidecar's pipe closing ends its conversation and takes the panes
//!   down (docs/canvas.md, *Failure model*).
//! - **While the session is locked** the core is **reserved** for the lock's
//!   sidecar ([`Prompts::reserve`], `lock.rs`): another sink's conversation
//!   up at the lock is dismissed, and another sink's frames while locked are
//!   answered `ANSWER_CANCELLED` (or ignored, for an end), so the lock
//!   screen's prompt is only ever the lock's and a lock conversation is never
//!   refused for another's.
//!
//! The panes are wherever the host draws the prompt: the top layer, or the
//! lock surfaces while the session is locked. Which is `wayland.rs`'s
//! business; here [`Effect::Raise`] and [`Effect::Lower`] mean "the panel is
//! to be shown" and "no longer".
//!
//! **The secret never leaves the host except down the pipe.** For an
//! echo-off ask the caustic is given a count of characters; the text written
//! into `prompt_text` is the title, the line, the ask's words and, for an
//! echo-on ask only, what was typed. Nothing here logs a message, a typed
//! byte or a count; the `Debug` of a [`Prompts`] shows no typed byte.

use caustics_bar_abi::canvas::{
    CanvasPrompt, PROMPT_ASKING, PROMPT_FAILED, PROMPT_HELD, PROMPT_IDLE, PROMPT_TEXT_BYTES,
    PROMPT_VERIFYING,
};
use caustics_bar_abi::wire::{
    Answer, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR, PROMPT_INFO, PROMPT_INFO_TITLE,
    PROMPT_SECRET, Prompt,
};

use crate::keys::{Entry, Key};

/// Where a conversation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Held,
    Asking,
    Verifying,
    Failed,
    Dismissed,
}

impl Phase {
    /// Whether the panes are up for it.
    fn raised(self) -> bool {
        matches!(
            self,
            Phase::Held | Phase::Asking | Phase::Verifying | Phase::Failed
        )
    }
}

/// What the host is to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Write this answer down `sidecar`'s stdin, once. Dropping it zeroes
    /// the bytes it carries.
    Answer { sidecar: String, answer: Answer },
    /// Show the panel on every output and take the keyboard: the top-layer
    /// panes up, or the lock panes redrawn while locked.
    Raise,
    /// No more panel: the top-layer panes down, or the lock panes redrawn.
    Lower,
    /// What the caustic is told changed: write it, and draw.
    Changed,
    /// One line for the journal. Never a message, a typed byte or a count.
    Note(String),
}

/// One conversation.
#[derive(Debug)]
struct Conversation {
    sidecar: String,
    id: u32,
    phase: Phase,
    echo: bool,
    title: String,
    line: String,
    line_error: bool,
    ask: String,
    entry: Entry,
    /// The count shown while verifying: what was typed when Return went;
    /// and for an echo-on ask, the text.
    frozen: u32,
    shown: Shown,
    attempts: u32,
}

impl Conversation {
    fn new(sidecar: &str, id: u32) -> Conversation {
        Conversation {
            sidecar: sidecar.to_owned(),
            id,
            phase: Phase::Held,
            echo: false,
            title: String::new(),
            line: String::new(),
            line_error: false,
            ask: String::new(),
            entry: Entry::new(),
            frozen: 0,
            shown: Shown::default(),
            attempts: 0,
        }
    }

    fn is(&self, sidecar: &str, id: u32) -> bool {
        self.sidecar == sidecar && self.id == id
    }
}

/// The prompt core: at most one conversation.
#[derive(Debug, Default)]
pub struct Prompts {
    up: Option<Conversation>,
    /// Bumped on every change to what the caustic is told.
    generation: u32,
    /// The only sidecar whose conversations are taken, while the session is
    /// locked.
    reserved: Option<String>,
}

impl Prompts {
    pub fn new() -> Prompts {
        Prompts::default()
    }

    /// Whether a conversation's panel is shown: from its first frame until it
    /// ends or is dismissed, on whichever panes carry it (the top layer's, or
    /// the lock's while locked).
    pub fn raised(&self) -> bool {
        self.up.as_ref().is_some_and(|c| c.phase.raised())
    }

    /// Whether keys go to the prompt rather than the dim: whenever the panel
    /// is shown, whatever the prompt then does with them. The compositor
    /// gives the keyboard to the top-layer pane, or to a lock surface while
    /// locked, and either way the key reaches the host.
    pub fn takes_keys(&self) -> bool {
        self.raised()
    }

    /// Reserve the core for `sidecar` (the session is locked for it), or end
    /// the reservation (`None`, unlocked). A conversation of another sidecar
    /// that is up when the reservation begins is dismissed as Escape would:
    /// `ANSWER_CANCELLED` to its sink, and the panel goes.
    pub fn reserve(&mut self, sidecar: Option<&str>) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.reserved = sidecar.map(str::to_owned);
        let Some(only) = sidecar else {
            return effects;
        };
        let Some(c) = self
            .up
            .as_mut()
            .filter(|c| c.sidecar != only && c.phase != Phase::Dismissed)
        else {
            return effects;
        };
        let raised = c.phase.raised();
        c.entry.clear();
        c.phase = Phase::Dismissed;
        effects.push(Effect::Note(format!(
            "prompt: conversation {} of {} dismissed for the lock; told to {}",
            c.id, c.sidecar, c.sidecar
        )));
        effects.push(cancelled(&c.sidecar, c.id));
        if raised {
            effects.push(Effect::Lower);
        }
        self.changed(&mut effects);
        effects
    }

    fn changed(&mut self, effects: &mut Vec<Effect>) {
        self.generation = self.generation.wrapping_add(1);
        effects.push(Effect::Changed);
    }

    /// One `Prompt` frame from `sidecar`.
    pub fn frame(&mut self, sidecar: &str, prompt: &Prompt) -> Vec<Effect> {
        let mut effects = Vec::new();
        let ends = matches!(prompt.kind, PROMPT_CANCEL | PROMPT_DONE);
        let asks = matches!(prompt.kind, PROMPT_SECRET | PROMPT_ECHO);
        let id = prompt.id;

        // While locked, only the lock's sidecar converses.
        if let Some(only) = self.reserved.as_deref()
            && only != sidecar
        {
            if !ends {
                effects.push(Effect::Note(format!(
                    "prompt: {sidecar} asked under conversation {id} while the session is \
                     locked; answered cancelled"
                )));
                effects.push(cancelled(sidecar, id));
            }
            return effects;
        }

        // A dismissed conversation waits only for its end; a new one
        // replaces it.
        if let Some(c) = self.up.as_ref()
            && c.phase == Phase::Dismissed
        {
            if c.is(sidecar, id) {
                if ends {
                    self.up = None;
                } else if asks {
                    effects.push(cancelled(sidecar, id));
                }
                return effects;
            }
            self.up = None;
        }

        match self.up.as_ref() {
            Some(c) if !c.is(sidecar, id) => {
                if ends {
                    effects.push(Effect::Note(format!(
                        "prompt: an end from {sidecar} for conversation {id}, which is not \
                         the one up; ignored"
                    )));
                } else {
                    effects.push(Effect::Note(format!(
                        "prompt: {sidecar} asked under conversation {id} while conversation {} \
                         of {} is up; answered cancelled",
                        c.id, c.sidecar
                    )));
                    effects.push(cancelled(sidecar, id));
                }
                return effects;
            }
            None if ends => {
                effects.push(Effect::Note(format!(
                    "prompt: an end from {sidecar} for conversation {id}, which never began; \
                     ignored"
                )));
                return effects;
            }
            None => {
                self.up = Some(Conversation::new(sidecar, id));
                effects.push(Effect::Note(format!(
                    "prompt: conversation {id} from {sidecar} began"
                )));
                // The panes come up for the conversation's first frame,
                // title or ask, and stay up until its end.
                effects.push(Effect::Raise);
            }
            Some(_) => {}
        }

        let c = self.up.as_mut().expect("held or made above");
        match prompt.kind {
            PROMPT_INFO if prompt.arg == PROMPT_INFO_TITLE => c.title = prompt.message.clone(),
            PROMPT_INFO => {
                c.line = prompt.message.clone();
                c.line_error = false;
                effects.push(Effect::Note(format!(
                    "prompt: conversation {id}: an info line"
                )));
            }
            PROMPT_ERROR => {
                c.line = prompt.message.clone();
                c.line_error = true;
                if c.phase == Phase::Verifying {
                    c.phase = Phase::Failed;
                    c.attempts = c.attempts.saturating_add(1);
                    effects.push(Effect::Note(format!(
                        "prompt: conversation {id}: an error line after the answer \
                         (attempts {})",
                        c.attempts
                    )));
                } else {
                    // A line beside the ask, not an answer's verdict (the
                    // lock's fingerprint reader saying a finger did not
                    // match): shown, and not counted.
                    effects.push(Effect::Note(format!(
                        "prompt: conversation {id}: an error line, no answer pending"
                    )));
                }
            }
            PROMPT_SECRET | PROMPT_ECHO => {
                c.ask = prompt.message.clone();
                c.echo = prompt.kind == PROMPT_ECHO;
                c.entry.clear();
                c.frozen = 0;
                c.shown = Shown::default();
                c.phase = Phase::Asking;
                effects.push(Effect::Note(format!(
                    "prompt: conversation {id}: an ask (echo {}, attempts {})",
                    u32::from(c.echo),
                    c.attempts
                )));
            }
            PROMPT_CANCEL | PROMPT_DONE => {
                let raised = c.phase.raised();
                effects.push(Effect::Note(if prompt.kind == PROMPT_CANCEL {
                    format!("prompt: conversation {id} withdrawn by {sidecar}")
                } else {
                    format!(
                        "prompt: conversation {id} done, {}",
                        if prompt.arg == 1 {
                            "success"
                        } else {
                            "failure"
                        }
                    )
                }));
                self.up = None;
                if raised {
                    effects.push(Effect::Lower);
                }
            }
            _ => return effects,
        }
        self.changed(&mut effects);
        effects
    }

    /// One key, while [`Prompts::takes_keys`].
    pub fn key(&mut self, key: Key) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(c) = self.up.as_mut().filter(|c| c.phase.raised()) else {
            return effects;
        };
        match (c.phase, key) {
            (_, Key::Cancel) => {
                c.entry.clear();
                c.phase = Phase::Dismissed;
                effects.push(Effect::Note(format!(
                    "prompt: conversation {} dismissed; told to {}",
                    c.id, c.sidecar
                )));
                effects.push(cancelled(&c.sidecar, c.id));
                effects.push(Effect::Lower);
            }
            (Phase::Asking, Key::Text(typed)) => {
                if !c.entry.push(&typed) {
                    return effects;
                }
            }
            (Phase::Asking, Key::Backspace) => {
                if !c.entry.backspace() {
                    return effects;
                }
            }
            (Phase::Asking, Key::Clear) => {
                if c.entry.is_empty() {
                    return effects;
                }
                c.entry.clear();
            }
            (Phase::Asking, Key::Submit) => {
                c.frozen = c.entry.chars();
                if c.echo {
                    c.shown = Shown(c.entry.echoed().to_owned());
                }
                let bytes = c.entry.take();
                c.phase = Phase::Verifying;
                effects.push(Effect::Note(format!(
                    "prompt: conversation {} answered; told to {}",
                    c.id, c.sidecar
                )));
                effects.push(Effect::Answer {
                    sidecar: c.sidecar.clone(),
                    answer: Answer::Text { id: c.id, bytes },
                });
            }
            // Verifying and failed: the answer has gone, and only Escape
            // means anything until the sidecar speaks.
            _ => return effects,
        }
        self.changed(&mut effects);
        effects
    }

    /// `sidecar`'s pipe closed: its conversation is over, and the panes go.
    pub fn gone(&mut self, sidecar: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(c) = self.up.as_ref().filter(|c| c.sidecar == sidecar) else {
            return effects;
        };
        let raised = c.phase.raised();
        effects.push(Effect::Note(format!(
            "prompt: {sidecar} went mid-conversation {}; the prompt goes with it",
            c.id
        )));
        self.up = None;
        if raised {
            effects.push(Effect::Lower);
        }
        self.changed(&mut effects);
        effects
    }

    /// What the caustic is told: the facts, the text they range over, and
    /// whether that text had to be cut to `PROMPT_TEXT_BYTES`.
    pub fn facts(&self) -> (CanvasPrompt, Vec<u8>, bool) {
        let mut facts = CanvasPrompt {
            generation: self.generation,
            ..CanvasPrompt::default()
        };
        let mut text: Vec<u8> = Vec::new();
        let mut cut = false;
        let Some(c) = self.up.as_ref() else {
            return (facts, text, cut);
        };
        facts.state = match c.phase {
            Phase::Held => PROMPT_HELD,
            Phase::Asking => PROMPT_ASKING,
            Phase::Verifying => PROMPT_VERIFYING,
            Phase::Failed => PROMPT_FAILED,
            Phase::Dismissed => PROMPT_IDLE,
        };
        if facts.state == PROMPT_IDLE {
            return (facts, text, cut);
        }
        facts.id = c.id;
        facts.echo = u32::from(c.echo);
        facts.attempts = c.attempts;
        facts.typed = if c.phase == Phase::Asking {
            c.entry.chars()
        } else {
            c.frozen
        };
        facts.line_error = u32::from(c.line_error);
        let mut put = |s: &str| -> (u32, u32) {
            let room = PROMPT_TEXT_BYTES.saturating_sub(text.len());
            let mut end = s.len().min(room);
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            cut |= end < s.len();
            let off = text.len() as u32;
            text.extend_from_slice(&s.as_bytes()[..end]);
            (off, end as u32)
        };
        (facts.title_off, facts.title_len) = put(&c.title);
        (facts.ask_off, facts.ask_len) = put(&c.ask);
        (facts.line_off, facts.line_len) = put(&c.line);
        // The typed text only for an echo-on ask: an echo-off answer is a
        // count and nothing else.
        if c.echo {
            let echoed = if c.phase == Phase::Asking {
                c.entry.echoed()
            } else {
                c.shown.0.as_str()
            };
            (facts.echo_off, facts.echo_len) = put(echoed);
        }
        (facts, text, cut)
    }
}

/// What an echo-on ask had typed when it was answered. Not a secret by
/// PAM's definition, and still kept out of a `Debug`.
#[derive(Default)]
struct Shown(String);

impl std::fmt::Debug for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shown(..)")
    }
}

fn cancelled(sidecar: &str, id: u32) -> Effect {
    Effect::Answer {
        sidecar: sidecar.to_owned(),
        answer: Answer::Cancelled { id },
    }
}

#[cfg(test)]
mod tests;
