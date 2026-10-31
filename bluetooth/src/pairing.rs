//! The agent's conductor: one question BlueZ waits on at a time, the rest
//! queued, and a code to type on the other device beside it. Pure: the loop
//! hands it the agent's events, the clicks on its rows, the sink's replies
//! and the clock, and it answers BlueZ's methods, says what the chip's rows
//! are ([`View`]), and names the socket calls the loop makes ([`Effect`]).
//!
//! - A **yes-or-no question** (`RequestConfirmation`, `RequestAuthorization`,
//!   `AuthorizeService`) is three `RFLAG_ATTENTION` rows under the chip: the
//!   question, **Confirm** and **Reject**, and the chip wears `RFLAG_SHOW`,
//!   so the bar shows them as a toast without a click and without a grab,
//!   and in the menu when it is open (docs/abi.md, *Rows*, *Toasts*).
//!   Confirm returns the method, Reject returns `org.bluez.Error.Rejected`.
//! - A **typed question** (`RequestPinCode`, `RequestPasskey`) goes to the
//!   canvas's pairing sink ([`Effect::Ask`]) and its reply to BlueZ. No sink
//!   is `Rejected`, and one witness that says how to start it.
//! - A **display** (`DisplayPasskey`, `DisplayPinCode`) is one attention row
//!   with `RFLAG_SHOW`, "Type 123 456 on Pixel 9", updated in place as the
//!   other device reports digits typed, and taken down when the device is
//!   paired, BlueZ cancels or releases, [`DISPLAY_FOR`] passes, or the row
//!   is clicked.
//!
//! **`Cancel` and `Release`** end everything this agent holds: the question
//! asked with `org.bluez.Error.Canceled` (a typed one also takes the sink's
//! prompt down, [`Effect::Cancel`]), every queued one the same, and the
//! display. BlueZ names no request in a `Cancel`, and holds one per agent.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use caustics_bar_abi::wire::Secret;
use tokio::sync::oneshot;

use crate::agent::{Event, Outcome, Request};
use crate::ask;
use crate::reply::Pin;
use crate::sink::{Reply, Typed};
use crate::witness;

/// How long a display is held at most when nothing ends it first: past the
/// pairing timeouts of the devices that ask a person to type a code.
pub const DISPLAY_FOR: Duration = Duration::from_secs(60);

/// The witness, and BlueZ's error words, for a typed question with no sink.
pub const NO_SINK: &str = "needs a typed PIN; the canvas's pairing sink is not running \
     (canvas/README.md, *Enabling the pairing agent*)";

/// Something the loop does for the conductor: a socket call to the pairing
/// sink, spawned so the loop never waits on a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// `pin <title>` or `passkey <title>`; the reply comes back to
    /// [`Pairing::typed`] under `id`.
    Ask {
        id: u32,
        typed: Typed,
        title: String,
    },
    /// `cancel`: the typed question asked was withdrawn.
    Cancel,
}

/// One request BlueZ waits on. Its device is in no row: a question names
/// the device by its title, and BlueZ answers for one device at a time.
struct Asked {
    title: String,
    request: Request,
    reply: oneshot::Sender<Outcome>,
}

/// The request being asked now.
struct Active {
    id: u32,
    asked: Asked,
    /// The question's row, for a yes or a no; `None` while the sink asks.
    line: Option<String>,
}

/// A code shown to type on the other device.
struct Shown {
    device: String,
    title: String,
    code: String,
    entered: Option<u16>,
    until: Instant,
}

/// What the chip's rows show of the agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    /// The yes-or-no question asked, when one is.
    pub question: Option<String>,
    /// The code to type on the other device, when one is shown.
    pub display: Option<String>,
}

impl View {
    /// Whether the chip asks for its toast.
    pub fn showing(&self) -> bool {
        self.question.is_some() || self.display.is_some()
    }
}

/// What the conductor knows.
#[derive(Default)]
pub struct Pairing {
    active: Option<Active>,
    queue: VecDeque<Asked>,
    display: Option<Shown>,
    next_id: u32,
    /// Questions that ended without an answer since this process started: a
    /// count, never a description.
    unanswered: u32,
}

impl Pairing {
    pub fn new() -> Pairing {
        Pairing {
            next_id: 1,
            ..Pairing::default()
        }
    }

    /// The chip's rows, as they are now.
    pub fn view(&self) -> View {
        View {
            question: self.active.as_ref().and_then(|a| a.line.clone()),
            display: self
                .display
                .as_ref()
                .map(|d| ask::type_on_device(&d.code, d.entered, &d.title)),
        }
    }

    /// When the display ends by itself, if one is shown.
    pub fn deadline(&self) -> Option<Instant> {
        self.display.as_ref().map(|d| d.until)
    }

    /// One of the agent object's events.
    pub fn event(&mut self, event: Event, now: Instant) -> Vec<Effect> {
        match event {
            Event::Ask {
                device: _,
                title,
                request,
                reply,
            } => {
                let asked = Asked {
                    title,
                    request,
                    reply,
                };
                if let Some(active) = &self.active {
                    witness(&format!(
                        "agent: {:?} asks {}: queued behind question {} ({} waiting)",
                        asked.title,
                        asked.request.name(),
                        active.id,
                        self.queue.len() + 1
                    ));
                }
                self.queue.push_back(asked);
                self.start_next()
            }
            Event::Display {
                device,
                title,
                code,
                entered,
            } => {
                match &mut self.display {
                    // The same device's code again: how much is typed moved.
                    Some(shown) if shown.device == device && shown.code == code => {
                        shown.entered = entered;
                    }
                    _ => {
                        witness(&format!(
                            "agent: {title:?} shows a code to type on the device"
                        ));
                        self.display = Some(Shown {
                            device,
                            title,
                            code,
                            entered,
                            until: now + DISPLAY_FOR,
                        });
                    }
                }
                Vec::new()
            }
            Event::Cancel => self.withdraw("cancelled by BlueZ"),
            Event::Release => self.withdraw("released by BlueZ"),
        }
    }

    /// A click on one of the agent's rows: `Some` when `row` is one, so the
    /// loop does not look for it in the device table.
    pub fn click(&mut self, row: u32) -> Option<Vec<Effect>> {
        use crate::rows::{ROW_CONFIRM, ROW_DISPLAY, ROW_QUESTION, ROW_REJECT, ROW_SEP_AGENT};
        match row {
            ROW_CONFIRM | ROW_REJECT => {
                let yes_or_no = self.active.as_ref().is_some_and(|a| a.line.is_some());
                if !yes_or_no {
                    witness("agent: a click on a question that is no longer asked; dropped");
                    return Some(Vec::new());
                }
                let outcome = if row == ROW_CONFIRM {
                    Outcome::Confirmed
                } else {
                    Outcome::Rejected("rejected on the bar")
                };
                self.finish(outcome, None);
                Some(self.start_next())
            }
            ROW_DISPLAY => {
                self.end_display("dismissed");
                Some(Vec::new())
            }
            ROW_QUESTION | ROW_SEP_AGENT => Some(Vec::new()),
            _ => None,
        }
    }

    /// The sink's reply to the typed question `id`.
    pub fn typed(&mut self, id: u32, reply: Reply) -> Vec<Effect> {
        let Some(active) = self.active.as_ref().filter(|a| a.id == id) else {
            // BlueZ cancelled it first; the reply (a `cancelled`, most often)
            // is dropped, and any text in it zeroed.
            drop(reply);
            return Vec::new();
        };
        let request = active.asked.request.clone();
        let outcome = match reply {
            Reply::Text(text) => answer(&request, text),
            Reply::Cancelled => Outcome::Rejected("dismissed on the canvas"),
            Reply::Error(why) => {
                witness(&format!(
                    "agent: question {id}: the pairing sink said: {why}"
                ));
                Outcome::Rejected("the canvas's pairing sink refused it")
            }
            Reply::Unreachable(why) => {
                witness(&format!("agent: question {id}: {NO_SINK} ({why})"));
                Outcome::Rejected(NO_SINK)
            }
        };
        self.finish(outcome, None);
        self.start_next()
    }

    /// The picture moved: a display whose device is now paired is over.
    pub fn paired(&mut self, paired: &[&str]) -> bool {
        let done = self
            .display
            .as_ref()
            .is_some_and(|d| paired.contains(&d.device.as_str()));
        if done {
            self.end_display("paired");
        }
        done
    }

    /// The clock: a display held for [`DISPLAY_FOR`] is over.
    pub fn expire(&mut self, now: Instant) -> bool {
        let done = self.display.as_ref().is_some_and(|d| now >= d.until);
        if done {
            self.end_display("timed out");
        }
        done
    }

    /// The sidecar is leaving: everything BlueZ waits on is refused.
    pub fn leave(&mut self) {
        if self.active.is_some() {
            self.finish(Outcome::Rejected("the agent is leaving"), None);
        }
        for asked in self.queue.drain(..) {
            let _ = asked.reply.send(Outcome::Rejected("the agent is leaving"));
        }
        self.display = None;
    }

    /// Start the next queued request while nothing is asked.
    fn start_next(&mut self) -> Vec<Effect> {
        if self.active.is_some() {
            return Vec::new();
        }
        let Some(asked) = self.queue.pop_front() else {
            return Vec::new();
        };
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let line = match &asked.request {
            Request::Confirm { passkey } => Some(ask::confirm_passkey(*passkey, &asked.title)),
            Request::Authorize => Some(ask::allow_pairing(&asked.title)),
            Request::Service { uuid } => Some(ask::allow_service(uuid, &asked.title)),
            Request::Pin | Request::Passkey => None,
        };
        let typed = match asked.request {
            Request::Pin => Some(Typed::Pin),
            Request::Passkey => Some(Typed::Passkey),
            _ => None,
        };
        witness(&format!(
            "agent: question {id}: {:?} asks {}{}",
            asked.title,
            asked.request.name(),
            if typed.is_some() {
                "; asking the canvas's pairing sink"
            } else {
                "; asking on the bar"
            }
        ));
        let title = asked.title.clone();
        self.active = Some(Active { id, asked, line });
        match typed {
            Some(typed) => vec![Effect::Ask { id, typed, title }],
            None => Vec::new(),
        }
    }

    /// End the question asked with `outcome`; `why` is a cancel's witness.
    fn finish(&mut self, outcome: Outcome, why: Option<&str>) {
        let Some(active) = self.active.take() else {
            return;
        };
        let id = active.id;
        let said = match &outcome {
            Outcome::Confirmed => "confirmed".to_owned(),
            Outcome::Pin(_) => "answered; the PIN sent".to_owned(),
            Outcome::Passkey(_) => "answered; the passkey sent".to_owned(),
            Outcome::Rejected(words) => format!("rejected ({words})"),
            Outcome::Canceled => why.unwrap_or("cancelled by BlueZ").to_owned(),
        };
        if matches!(outcome, Outcome::Rejected(_) | Outcome::Canceled) {
            self.unanswered = self.unanswered.saturating_add(1);
            witness(&format!(
                "agent: question {id}: {said}; {} unanswered since start",
                self.unanswered
            ));
        } else {
            witness(&format!("agent: question {id}: {said}"));
        }
        let _ = active.asked.reply.send(outcome);
    }

    /// `Cancel` or `Release`.
    fn withdraw(&mut self, why: &'static str) -> Vec<Effect> {
        for asked in self.queue.drain(..) {
            witness(&format!(
                "agent: {:?} asks {}: {why} while queued",
                asked.title,
                asked.request.name()
            ));
            let _ = asked.reply.send(Outcome::Canceled);
        }
        let mut effects = Vec::new();
        if let Some(active) = &self.active {
            if active.line.is_none() {
                effects.push(Effect::Cancel);
            }
            self.finish(Outcome::Canceled, Some(why));
        } else if self.display.is_none() {
            witness(&format!("agent: {why} with nothing asked; nothing to end"));
        }
        self.end_display(why);
        effects
    }

    fn end_display(&mut self, why: &str) {
        if let Some(shown) = self.display.take() {
            witness(&format!(
                "agent: the code for {:?} is gone ({why})",
                shown.title
            ));
        }
    }
}

/// The sink's text as the reply `request` wants, or `Rejected` when it is
/// not one BlueZ takes. `text` is zeroed when it drops, here.
fn answer(request: &Request, text: Secret) -> Outcome {
    match request {
        Request::Pin => match std::str::from_utf8(text.as_bytes()) {
            Ok(pin) if ask::pin_valid(pin) => Outcome::Pin(Pin::new(pin)),
            _ => Outcome::Rejected("the sink's PIN is not one BlueZ takes"),
        },
        Request::Passkey => match ask::passkey(text.as_bytes()) {
            Some(passkey) => Outcome::Passkey(passkey),
            None => Outcome::Rejected("the sink's passkey is not six digits"),
        },
        _ => Outcome::Rejected("text for a question that asks none"),
    }
}
