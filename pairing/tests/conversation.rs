//! The conductor without a socket: commands in as the socket hands them
//! over, frames out as the host would read them, answers in as the host
//! writes them, and the reply line each command gets.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use caustics_bar_abi::wire::{
    Answer, Frame, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR, PROMPT_INFO,
    PROMPT_INFO_TITLE, Prompt, Secret,
};
use caustics_pairing::ask;
use caustics_pairing::conversation::{
    BUSY, Command, Conductor, Event, LEAVING, Question, Reply, Sink,
};

const WAIT: Duration = Duration::from_secs(5);

struct Run {
    events: Sender<Event>,
    frames: Receiver<Frame>,
    task: std::thread::JoinHandle<i32>,
}

fn start() -> Run {
    let (events, inbox) = channel();
    let (frames_tx, frames) = channel();
    let sink: Sink = Box::new(move |frame| {
        let _ = frames_tx.send(frame.clone());
        Ok(())
    });
    let task = std::thread::spawn(move || Conductor::new(sink).run(inbox));
    Run {
        events,
        frames,
        task,
    }
}

impl Run {
    fn prompt(&self) -> Prompt {
        match self.frames.recv_timeout(WAIT) {
            Ok(Frame::Prompt(prompt)) => prompt,
            other => panic!("no prompt: {other:?}"),
        }
    }

    fn expect(&self, id: u32, kind: u32, arg: u32, message: &str) {
        let prompt = self.prompt();
        assert_eq!(
            (prompt.id, prompt.kind, prompt.arg, prompt.message.as_str()),
            (id, kind, arg, message),
            "{prompt:?}"
        );
    }

    fn nothing_more(&self) {
        assert!(
            self.frames.recv_timeout(Duration::from_millis(50)).is_err(),
            "a frame nobody expected"
        );
    }

    fn command(&self, command: Command) -> Receiver<Reply> {
        let (tx, rx) = channel();
        self.events.send(Event::Command(command, tx)).unwrap();
        rx
    }

    fn ask(&self, question: Question, title: &str) -> Receiver<Reply> {
        self.command(Command::Ask {
            question,
            title: title.to_owned(),
        })
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

    fn close(self) -> i32 {
        let _ = self.events.send(Event::Closed);
        self.task.join().unwrap()
    }
}

fn reply(rx: &Receiver<Reply>) -> String {
    let line = rx.recv_timeout(WAIT).expect("a reply");
    String::from_utf8(line.to_vec()).expect("a reply is text")
}

#[test]
fn a_pin_is_a_title_and_an_ask_and_a_good_answer_is_the_reply() {
    let run = start();
    let rx = run.ask(Question::Pin, "Example Keyboard");
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard");
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    run.text(1, b"0000");
    run.expect(1, PROMPT_DONE, 1, "");
    assert_eq!(reply(&rx), "text 0000\n");
    run.nothing_more();
    assert_eq!(run.close(), 0);
}

#[test]
fn a_pin_out_of_bounds_is_asked_again() {
    let run = start();
    let rx = run.ask(Question::Pin, "Example Keyboard");
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard");
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    run.text(1, b"");
    run.expect(1, PROMPT_ERROR, 0, ask::BAD_PIN);
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    run.text(1, &[b'9'; 17]);
    run.expect(1, PROMPT_ERROR, 0, ask::BAD_PIN);
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    run.text(1, &[0xff, 0xfe]);
    run.expect(1, PROMPT_ERROR, 0, ask::PIN_NOT_TEXT);
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    assert!(rx.try_recv().is_err(), "no reply while it is asked again");
    run.text(1, b"1234");
    run.expect(1, PROMPT_DONE, 1, "");
    assert_eq!(reply(&rx), "text 1234\n");
    run.close();
}

#[test]
fn a_passkey_is_six_digits_or_asked_again() {
    let run = start();
    let rx = run.ask(Question::Passkey, "Example Keyboard");
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard");
    run.expect(1, PROMPT_ECHO, 0, ask::PASSKEY);
    run.text(1, b"12345");
    run.expect(1, PROMPT_ERROR, 0, ask::BAD_PASSKEY);
    run.expect(1, PROMPT_ECHO, 0, ask::PASSKEY);
    run.text(1, b"000042");
    run.expect(1, PROMPT_DONE, 1, "");
    assert_eq!(reply(&rx), "text 000042\n");
    run.close();
}

#[test]
fn a_dismissal_is_cancelled_and_done_zero() {
    let run = start();
    let rx = run.ask(Question::Pin, "Example Keyboard");
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard");
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    run.dismiss(1);
    run.expect(1, PROMPT_DONE, 0, "");
    assert_eq!(reply(&rx), "cancelled\n");
    run.close();
}

#[test]
fn a_cancel_takes_the_prompt_down_and_the_asker_hears_cancelled() {
    let run = start();
    let asked = run.ask(Question::Passkey, "Example Keyboard");
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard");
    run.expect(1, PROMPT_ECHO, 0, ask::PASSKEY);
    let cancel = run.command(Command::Cancel);
    assert_eq!(reply(&cancel), "ok\n");
    run.expect(1, PROMPT_CANCEL, 0, "");
    assert_eq!(reply(&asked), "cancelled\n");
    // An answer for it now is for nothing on screen.
    run.text(1, b"123456");
    run.nothing_more();
    // A cancel with nothing asked is still ok, and shows nothing.
    assert_eq!(reply(&run.command(Command::Cancel)), "ok\n");
    run.nothing_more();
    run.close();
}

#[test]
fn a_second_question_while_one_is_asked_is_busy() {
    let run = start();
    let first = run.ask(Question::Pin, "Example Keyboard");
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard");
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    assert_eq!(reply(&run.command(Command::Status)), "ok asking=yes\n");
    let second = run.ask(Question::Passkey, "Example Mouse");
    assert_eq!(reply(&second), format!("{BUSY}\n"));
    run.nothing_more();
    run.text(1, b"0000");
    run.expect(1, PROMPT_DONE, 1, "");
    assert_eq!(reply(&first), "text 0000\n");
    assert_eq!(reply(&run.command(Command::Status)), "ok asking=no\n");
    // The next one is conversation 2.
    let _third = run.ask(Question::Pin, "Example Mouse");
    run.expect(2, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Mouse");
    run.close();
}

#[test]
fn the_host_leaving_answers_the_asker() {
    let run = start();
    let rx = run.ask(Question::Pin, "Example Keyboard");
    run.expect(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Example Keyboard");
    run.expect(1, PROMPT_ECHO, 0, ask::PIN);
    assert_eq!(run.close(), 0);
    assert_eq!(reply(&rx), format!("{LEAVING}\n"));
}
