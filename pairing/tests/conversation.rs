//! The conductor without a bus: requests in as the agent object hands them
//! over, frames out as the host would read them, answers in as the host
//! writes them, and the outcome each method would return.

use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, Frame, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR, PROMPT_INFO,
    PROMPT_INFO_TITLE, Prompt, Secret,
};
use caustics_pairing::ask;
use caustics_pairing::conversation::{
    Conductor, Display, Event, Outcome, PAIRED, Question, Request, Sink, TIMED_OUT,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(5);
const BUDS: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_20";
const KEYBOARD: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_30";

struct Run {
    events: UnboundedSender<Event>,
    frames: UnboundedReceiver<Frame>,
    task: tokio::task::JoinHandle<i32>,
}

fn start() -> Run {
    let (events, inbox) = unbounded_channel();
    let (frames_tx, frames) = unbounded_channel();
    let sink: Sink = Box::new(move |frame| {
        let _ = frames_tx.send(frame.clone());
        Ok(())
    });
    let task = tokio::spawn(Conductor::new(sink).run(inbox));
    Run {
        events,
        frames,
        task,
    }
}

impl Run {
    async fn prompt(&mut self) -> Prompt {
        match tokio::time::timeout(WAIT, self.frames.recv()).await {
            Ok(Some(Frame::Prompt(prompt))) => prompt,
            other => panic!("no prompt: {other:?}"),
        }
    }

    async fn expect(&mut self, id: u32, kind: u32, arg: u32, message: &str) {
        let prompt = self.prompt().await;
        assert_eq!(
            (prompt.id, prompt.kind, prompt.arg, prompt.message.as_str()),
            (id, kind, arg, message),
            "{prompt:?}"
        );
    }

    fn nothing_more(&mut self) {
        assert!(self.frames.try_recv().is_err(), "a frame nobody expected");
    }

    fn ask(&self, device: &str, title: &str, question: Question) -> oneshot::Receiver<Outcome> {
        let (reply, outcome) = oneshot::channel();
        self.events
            .send(Event::Ask(Request {
                device: device.to_owned(),
                title: title.to_owned(),
                question,
                reply,
            }))
            .unwrap();
        outcome
    }

    fn show(&self, device: &str, title: &str, line: &str) {
        self.events
            .send(Event::Display(Display {
                device: device.to_owned(),
                title: title.to_owned(),
                line: line.to_owned(),
            }))
            .unwrap();
    }

    fn text(&self, id: u32, text: &[u8]) {
        self.events
            .send(Event::Answer(Answer::Text {
                id,
                bytes: Secret::new(text.to_vec()),
            }))
            .unwrap();
    }

    fn dismiss(&self, id: u32) {
        self.events
            .send(Event::Answer(Answer::Cancelled { id }))
            .unwrap();
    }

    async fn close(self) -> i32 {
        let _ = self.events.send(Event::Closed);
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("the conductor ends")
            .expect("the conductor does not panic")
    }
}

async fn outcome(rx: oneshot::Receiver<Outcome>) -> Outcome {
    tokio::time::timeout(WAIT, rx)
        .await
        .expect("an outcome in time")
        .expect("an outcome")
}

fn confirm(passkey: u32) -> Question {
    Question::Confirm {
        line: ask::confirm_passkey(passkey),
    }
}

#[tokio::test]
async fn a_confirmation_is_a_title_a_line_and_an_ask_and_enter_confirms() {
    let mut run = start();
    let reply = run.ask(BUDS, "Example Buds", confirm(42));
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    run.expect(1, PROMPT_INFO, 0, "Confirm passkey 000042")
        .await;
    run.expect(1, PROMPT_ECHO, 0, ask::CONFIRM).await;
    run.text(1, b"");
    run.expect(1, PROMPT_DONE, 1, "").await;
    assert!(matches!(outcome(reply).await, Outcome::Confirmed));
    run.nothing_more();
    assert_eq!(run.close().await, 0);
}

#[tokio::test]
async fn text_on_a_confirmation_is_an_error_line_and_the_ask_again() {
    let mut run = start();
    let reply = run.ask(BUDS, "Example Buds", confirm(42));
    for _ in 0..3 {
        run.prompt().await;
    }
    run.text(1, b"yes");
    run.expect(1, PROMPT_ERROR, 0, ask::CONFIRM_EMPTY).await;
    run.expect(1, PROMPT_ECHO, 0, ask::CONFIRM).await;
    run.dismiss(1);
    run.expect(1, PROMPT_DONE, 0, "").await;
    assert!(matches!(outcome(reply).await, Outcome::Rejected));
    run.close().await;
}

#[tokio::test]
async fn a_pin_out_of_bounds_is_asked_again_and_a_good_one_is_the_reply() {
    let mut run = start();
    let reply = run.ask(KEYBOARD, "Example Keyboard", Question::Pin);
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard")
        .await;
    run.expect(1, PROMPT_ECHO, 0, ask::PIN).await;
    run.text(1, b"");
    run.expect(1, PROMPT_ERROR, 0, ask::BAD_PIN).await;
    run.expect(1, PROMPT_ECHO, 0, ask::PIN).await;
    run.text(1, &[b'1'; 17]);
    run.expect(1, PROMPT_ERROR, 0, ask::BAD_PIN).await;
    run.expect(1, PROMPT_ECHO, 0, ask::PIN).await;
    run.text(1, b"\xff\xfe");
    run.expect(1, PROMPT_ERROR, 0, ask::PIN_NOT_TEXT).await;
    run.expect(1, PROMPT_ECHO, 0, ask::PIN).await;
    run.text(1, b"0000");
    run.expect(1, PROMPT_DONE, 1, "").await;
    match outcome(reply).await {
        Outcome::Pin(pin) => assert_eq!(pin.value(), "0000"),
        other => panic!("{other:?}"),
    }
    run.close().await;
}

#[tokio::test]
async fn a_passkey_is_six_digits_or_asked_again() {
    let mut run = start();
    let reply = run.ask(KEYBOARD, "Example Keyboard", Question::Passkey);
    run.prompt().await;
    run.expect(1, PROMPT_ECHO, 0, ask::PASSKEY).await;
    run.text(1, b"12345");
    run.expect(1, PROMPT_ERROR, 0, ask::BAD_PASSKEY).await;
    run.expect(1, PROMPT_ECHO, 0, ask::PASSKEY).await;
    run.text(1, b"012345");
    run.expect(1, PROMPT_DONE, 1, "").await;
    assert!(matches!(outcome(reply).await, Outcome::Passkey(12345)));
    run.close().await;
}

#[tokio::test]
async fn a_cancel_ends_the_conversation_on_screen_and_every_queued_one() {
    let mut run = start();
    let first = run.ask(BUDS, "Example Buds", Question::Pin);
    let second = run.ask(KEYBOARD, "Example Keyboard", confirm(7));
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    run.expect(1, PROMPT_ECHO, 0, ask::PIN).await;
    run.nothing_more();
    run.events.send(Event::Cancel).unwrap();
    run.expect(1, PROMPT_CANCEL, 0, "").await;
    assert!(matches!(outcome(first).await, Outcome::Canceled));
    assert!(
        matches!(outcome(second).await, Outcome::Canceled),
        "the queued one is answered with no frame"
    );
    run.nothing_more();
    // A cancel with nothing on screen ends nothing and writes nothing.
    run.events.send(Event::Cancel).unwrap();
    let next = run.ask(BUDS, "Example Buds", Question::Passkey);
    run.expect(2, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    run.expect(2, PROMPT_ECHO, 0, ask::PASSKEY).await;
    run.events.send(Event::Release).unwrap();
    run.expect(2, PROMPT_CANCEL, 0, "").await;
    assert!(matches!(outcome(next).await, Outcome::Canceled));
    run.close().await;
}

#[tokio::test]
async fn requests_queue_in_arrival_order() {
    let mut run = start();
    let first = run.ask(BUDS, "Example Buds", confirm(1));
    let second = run.ask(KEYBOARD, "Example Keyboard", Question::Pin);
    for _ in 0..3 {
        run.prompt().await;
    }
    run.nothing_more();
    run.text(1, b"");
    run.expect(1, PROMPT_DONE, 1, "").await;
    run.expect(2, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard")
        .await;
    run.expect(2, PROMPT_ECHO, 0, ask::PIN).await;
    // An answer for the conversation that ended is dropped.
    run.text(1, b"");
    run.text(2, b"1234");
    run.expect(2, PROMPT_DONE, 1, "").await;
    assert!(matches!(outcome(first).await, Outcome::Confirmed));
    assert!(matches!(outcome(second).await, Outcome::Pin(_)));
    run.close().await;
}

#[tokio::test]
async fn a_display_is_held_updated_in_place_and_ended_by_its_pairing() {
    let mut run = start();
    run.show(
        KEYBOARD,
        "Example Keyboard",
        &ask::type_on_device("000042", None),
    );
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard")
        .await;
    run.expect(1, PROMPT_INFO, 0, "Type 000042 on the device")
        .await;
    run.show(
        KEYBOARD,
        "Example Keyboard",
        &ask::type_on_device("000042", Some(2)),
    );
    run.expect(1, PROMPT_INFO, 0, "Type 000042 on the device (2 typed)")
        .await;
    // A settle for another conversation ends nothing.
    run.events
        .send(Event::Settled { id: 9, why: PAIRED })
        .unwrap();
    run.events
        .send(Event::Settled { id: 1, why: PAIRED })
        .unwrap();
    run.expect(1, PROMPT_DONE, 1, "").await;
    run.nothing_more();

    run.show(BUDS, "Example Buds", "Type 1234 on the device");
    run.expect(2, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    run.prompt().await;
    run.events
        .send(Event::Settled {
            id: 2,
            why: TIMED_OUT,
        })
        .unwrap();
    run.expect(2, PROMPT_DONE, 0, "").await;

    run.show(BUDS, "Example Buds", "Type 1234 on the device");
    run.prompt().await;
    run.prompt().await;
    run.dismiss(3);
    run.expect(3, PROMPT_DONE, 0, "").await;
    run.close().await;
}

#[tokio::test]
async fn a_request_supersedes_a_display() {
    let mut run = start();
    run.show(KEYBOARD, "Example Keyboard", "Type 000042 on the device");
    run.prompt().await;
    run.prompt().await;
    let reply = run.ask(BUDS, "Example Buds", confirm(5));
    run.expect(1, PROMPT_DONE, 0, "").await;
    run.expect(2, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    run.prompt().await;
    run.expect(2, PROMPT_ECHO, 0, ask::CONFIRM).await;
    run.text(2, b"");
    run.expect(2, PROMPT_DONE, 1, "").await;
    assert!(matches!(outcome(reply).await, Outcome::Confirmed));

    // Another device's code replaces the one on screen.
    run.show(KEYBOARD, "Example Keyboard", "Type 000042 on the device");
    run.prompt().await;
    run.prompt().await;
    run.show(BUDS, "Example Buds", "Type 1234 on the device");
    run.expect(3, PROMPT_DONE, 0, "").await;
    run.expect(4, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Buds")
        .await;
    run.expect(4, PROMPT_INFO, 0, "Type 1234 on the device")
        .await;
    run.close().await;
}

#[tokio::test]
async fn the_host_leaving_fails_every_request() {
    let mut run = start();
    let first = run.ask(BUDS, "Example Buds", confirm(1));
    let second = run.ask(KEYBOARD, "Example Keyboard", Question::Pin);
    run.prompt().await;
    assert_eq!(run.close().await, 0);
    assert!(matches!(outcome(first).await, Outcome::Failed));
    assert!(matches!(outcome(second).await, Outcome::Failed));
}
