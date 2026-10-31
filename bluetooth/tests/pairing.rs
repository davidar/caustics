//! The agent's conductor with no bus: events in as the agent object hands
//! them over, clicks in as the bar sends them, the sink's replies in, and
//! out the outcome each method returns, the socket calls the loop makes,
//! and the rows the chip shows.

use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{
    self, Frame, RFLAG_ATTENTION, RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_SHOW, ROW_ITEM, ROW_MENU,
    ROW_SEPARATOR, Rows, Secret,
};
use caustics_bluetooth::agent::{Event, Outcome, Request};
use caustics_bluetooth::bluez::{Adapter, State};
use caustics_bluetooth::pairing::{DISPLAY_FOR, Effect, NO_SINK, Pairing, View};
use caustics_bluetooth::rows::{
    DEVICE_ID_BIT, ROW_CHIP, ROW_CONFIRM, ROW_DISPLAY, ROW_POWER, ROW_QUESTION, ROW_REJECT,
    ROW_SEP_AGENT, picture, picture_with,
};
use caustics_bluetooth::sink::{Reply, Typed};
use tokio::sync::oneshot;

const PHONE: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_40";
const KEYBOARD: &str = "/org/bluez/hci0/dev_02_5E_00_00_0B_30";

fn ask(
    pairing: &mut Pairing,
    title: &str,
    request: Request,
) -> (oneshot::Receiver<Outcome>, Vec<Effect>) {
    let (reply, outcome) = oneshot::channel();
    let effects = pairing.event(
        Event::Ask {
            device: PHONE.to_owned(),
            title: title.to_owned(),
            request,
            reply,
        },
        Instant::now(),
    );
    (outcome, effects)
}

fn outcome(mut rx: oneshot::Receiver<Outcome>) -> Outcome {
    rx.try_recv().expect("the method has its outcome")
}

fn pending(rx: &mut oneshot::Receiver<Outcome>) -> bool {
    matches!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty))
}

#[test]
fn a_confirmation_is_a_question_and_two_answers_and_confirm_returns() {
    let mut pairing = Pairing::new();
    let (mut rx, effects) = ask(
        &mut pairing,
        "Pixel 9",
        Request::Confirm { passkey: 123_456 },
    );
    assert!(effects.is_empty(), "a yes or a no asks no sink");
    assert!(pending(&mut rx));
    let view = pairing.view();
    assert_eq!(
        view.question.as_deref(),
        Some("Confirm passkey 123 456 for Pixel 9")
    );
    assert!(view.showing());
    assert_eq!(pairing.click(ROW_CONFIRM), Some(Vec::new()));
    assert!(matches!(outcome(rx), Outcome::Confirmed));
    assert!(!pairing.view().showing(), "the rows are down");
}

#[test]
fn reject_returns_rejected_and_a_late_click_does_nothing() {
    let mut pairing = Pairing::new();
    let (rx, _) = ask(&mut pairing, "Pixel 9", Request::Authorize);
    assert_eq!(
        pairing.view().question.as_deref(),
        Some("Allow Pixel 9 to pair")
    );
    pairing.click(ROW_REJECT);
    assert!(matches!(outcome(rx), Outcome::Rejected(_)));
    assert_eq!(
        pairing.click(ROW_CONFIRM),
        Some(Vec::new()),
        "still the agent's row"
    );
    // A row that is not the agent's is the device table's.
    assert_eq!(pairing.click(ROW_POWER), None);
    assert_eq!(pairing.click(DEVICE_ID_BIT | 7), None);
}

#[test]
fn a_service_is_named_and_a_cancel_takes_the_rows_down() {
    let mut pairing = Pairing::new();
    let (rx, _) = ask(
        &mut pairing,
        "Pixel 9",
        Request::Service {
            uuid: "0000110b-0000-1000-8000-00805f9b34fb".to_owned(),
        },
    );
    assert_eq!(
        pairing.view().question.as_deref(),
        Some("Allow Audio Sink for Pixel 9")
    );
    let effects = pairing.event(Event::Cancel, Instant::now());
    assert!(effects.is_empty(), "no sink was asked, so none is told");
    assert!(matches!(outcome(rx), Outcome::Canceled));
    assert_eq!(pairing.view(), View::default());
}

#[test]
fn requests_queue_and_the_next_is_asked_when_one_ends() {
    let mut pairing = Pairing::new();
    let (first, _) = ask(&mut pairing, "Pixel 9", Request::Confirm { passkey: 1 });
    let (mut second, _) = ask(&mut pairing, "Pixel 9", Request::Authorize);
    assert!(pending(&mut second));
    assert_eq!(
        pairing.view().question.as_deref(),
        Some("Confirm passkey 000 001 for Pixel 9")
    );
    pairing.click(ROW_CONFIRM);
    assert!(matches!(outcome(first), Outcome::Confirmed));
    assert_eq!(
        pairing.view().question.as_deref(),
        Some("Allow Pixel 9 to pair")
    );
    pairing.click(ROW_CONFIRM);
    assert!(matches!(outcome(second), Outcome::Confirmed));
}

#[test]
fn a_cancel_ends_the_asked_and_every_queued() {
    let mut pairing = Pairing::new();
    let (first, _) = ask(&mut pairing, "Pixel 9", Request::Confirm { passkey: 1 });
    let (second, _) = ask(&mut pairing, "Pixel 9", Request::Authorize);
    pairing.event(Event::Release, Instant::now());
    assert!(matches!(outcome(first), Outcome::Canceled));
    assert!(matches!(outcome(second), Outcome::Canceled));
    assert!(!pairing.view().showing());
}

#[test]
fn a_typed_question_asks_the_sink_and_its_text_is_the_reply() {
    let mut pairing = Pairing::new();
    let (mut rx, effects) = ask(&mut pairing, "Example Keyboard", Request::Pin);
    assert_eq!(
        effects,
        vec![Effect::Ask {
            id: 1,
            typed: Typed::Pin,
            title: "Example Keyboard".to_owned()
        }]
    );
    assert!(
        !pairing.view().showing(),
        "a typed question is the canvas's, not a row"
    );
    assert!(pending(&mut rx));
    pairing.typed(1, Reply::Text(Secret::new(b"0000".to_vec())));
    match outcome(rx) {
        Outcome::Pin(pin) => assert_eq!(pin.value(), "0000"),
        other => panic!("{other:?}"),
    }

    let (rx, effects) = ask(&mut pairing, "Example Keyboard", Request::Passkey);
    assert_eq!(
        effects,
        vec![Effect::Ask {
            id: 2,
            typed: Typed::Passkey,
            title: "Example Keyboard".to_owned()
        }]
    );
    pairing.typed(2, Reply::Text(Secret::new(b"000042".to_vec())));
    assert!(matches!(outcome(rx), Outcome::Passkey(42)));
}

#[test]
fn a_typed_answer_bluez_would_refuse_is_rejected_here() {
    let mut pairing = Pairing::new();
    let (rx, _) = ask(&mut pairing, "Example Keyboard", Request::Pin);
    pairing.typed(1, Reply::Text(Secret::new(vec![b'9'; 17])));
    assert!(matches!(outcome(rx), Outcome::Rejected(_)));
    let (rx, _) = ask(&mut pairing, "Example Keyboard", Request::Passkey);
    pairing.typed(2, Reply::Text(Secret::new(b"12345".to_vec())));
    assert!(matches!(outcome(rx), Outcome::Rejected(_)));
}

#[test]
fn no_sink_is_rejected_with_the_words_that_say_how_to_start_one() {
    let mut pairing = Pairing::new();
    let (rx, _) = ask(&mut pairing, "Example Keyboard", Request::Pin);
    pairing.typed(1, Reply::Unreachable("no such file".to_owned()));
    match outcome(rx) {
        Outcome::Rejected(words) => assert_eq!(words, NO_SINK),
        other => panic!("{other:?}"),
    }
    assert!(NO_SINK.contains("canvas/README.md, *Enabling the pairing agent*"));
}

#[test]
fn a_dismissal_on_the_canvas_is_rejected_and_a_cancel_tells_the_sink() {
    let mut pairing = Pairing::new();
    let (rx, _) = ask(&mut pairing, "Example Keyboard", Request::Pin);
    pairing.typed(1, Reply::Cancelled);
    assert!(matches!(outcome(rx), Outcome::Rejected(_)));

    let (rx, _) = ask(&mut pairing, "Example Keyboard", Request::Passkey);
    let effects = pairing.event(Event::Cancel, Instant::now());
    assert_eq!(effects, vec![Effect::Cancel]);
    assert!(matches!(outcome(rx), Outcome::Canceled));
    // The sink's `cancelled` that follows is for nothing asked.
    assert!(pairing.typed(2, Reply::Cancelled).is_empty());
}

#[test]
fn a_display_is_one_row_updated_in_place_and_ended_by_its_pairing() {
    let mut pairing = Pairing::new();
    let now = Instant::now();
    let show = |pairing: &mut Pairing, entered| {
        pairing.event(
            Event::Display {
                device: KEYBOARD.to_owned(),
                title: "Example Keyboard".to_owned(),
                code: "123 456".to_owned(),
                entered,
            },
            now,
        )
    };
    show(&mut pairing, Some(0));
    assert_eq!(
        pairing.view().display.as_deref(),
        Some("Type 123 456 on Example Keyboard")
    );
    assert_eq!(pairing.deadline(), Some(now + DISPLAY_FOR));
    show(&mut pairing, Some(2));
    assert_eq!(
        pairing.view().display.as_deref(),
        Some("Type 123 456 on Example Keyboard (2 of 6 typed)")
    );
    assert!(
        !pairing.paired(&[PHONE]),
        "another device's pairing ends nothing"
    );
    assert!(pairing.paired(&[PHONE, KEYBOARD]));
    assert_eq!(pairing.view(), View::default());
    assert_eq!(pairing.deadline(), None);
}

#[test]
fn a_display_ends_after_its_minute_or_a_click_or_a_cancel() {
    let mut pairing = Pairing::new();
    let now = Instant::now();
    let show = |pairing: &mut Pairing| {
        pairing.event(
            Event::Display {
                device: KEYBOARD.to_owned(),
                title: "Example Keyboard".to_owned(),
                code: "0000".to_owned(),
                entered: None,
            },
            now,
        )
    };
    show(&mut pairing);
    assert!(!pairing.expire(now + DISPLAY_FOR - Duration::from_secs(1)));
    assert!(pairing.expire(now + DISPLAY_FOR));
    assert!(!pairing.view().showing());
    show(&mut pairing);
    assert_eq!(pairing.click(ROW_DISPLAY), Some(Vec::new()));
    assert!(!pairing.view().showing());
    show(&mut pairing);
    pairing.event(Event::Cancel, now);
    assert!(!pairing.view().showing());
}

#[test]
fn leaving_refuses_whatever_waits() {
    let mut pairing = Pairing::new();
    let (first, _) = ask(&mut pairing, "Pixel 9", Request::Confirm { passkey: 1 });
    let (second, _) = ask(&mut pairing, "Example Keyboard", Request::Pin);
    pairing.leave();
    assert!(matches!(outcome(first), Outcome::Rejected(_)));
    assert!(matches!(outcome(second), Outcome::Rejected(_)));
}

// ---------------------------------------------------------------------------
// The rows the chip shows while it asks
// ---------------------------------------------------------------------------

fn decoded(frame: Frame) -> Rows {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &frame).expect("writing to a Vec");
    match wire::read(&mut bytes.as_slice()) {
        Ok(Some(Frame::Rows(rows))) => rows,
        other => panic!("expected a rows frame, got {other:?}"),
    }
}

fn powered() -> State {
    State {
        adapter: Some(Adapter {
            path: "/org/bluez/hci0".to_owned(),
            name: "workstation".to_owned(),
            powered: true,
            discovering: false,
        }),
        radios: vec!["/org/bluez/hci0".to_owned()],
        ..State::default()
    }
}

#[test]
fn a_question_is_three_attention_rows_a_toast_shows_and_the_menu_leads_with() {
    let asking = View {
        question: Some("Confirm passkey 123 456 for Pixel 9".to_owned()),
        display: None,
    };
    let rows = decoded(picture_with(2, &powered(), &asking));
    let shape: Vec<(u32, u32, u32)> = rows
        .rows
        .iter()
        .map(|r| {
            (
                r.id,
                r.role,
                r.flags & (RFLAG_ATTENTION | RFLAG_DISABLED | RFLAG_SHOW),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (ROW_CHIP, ROW_ITEM, RFLAG_SHOW),
            (ROW_QUESTION, ROW_MENU, RFLAG_ATTENTION | RFLAG_DISABLED),
            (ROW_CONFIRM, ROW_MENU, RFLAG_ATTENTION),
            (ROW_REJECT, ROW_MENU, RFLAG_ATTENTION),
            (ROW_SEP_AGENT, ROW_SEPARATOR, 0),
            (ROW_POWER, ROW_MENU, 0),
        ]
    );
    assert!(rows.rows[0].flags & RFLAG_HAS_MENU != 0);
    assert_eq!(
        rows.label(1).as_deref(),
        Some("Confirm passkey 123 456 for Pixel 9")
    );
    assert_eq!(rows.label(2).as_deref(), Some("Confirm"));
    assert_eq!(rows.label(3).as_deref(), Some("Reject"));
    // Every agent row is a direct child of the chip: what a toast takes.
    for row in &rows.rows[1..4] {
        assert_eq!(row.parent, 0);
    }
}

#[test]
fn nothing_asked_is_the_picture_as_ever() {
    assert_eq!(
        decoded(picture_with(3, &powered(), &View::default())),
        decoded(picture(3, &powered()))
    );
    let rows = decoded(picture(3, &powered()));
    assert!(rows.rows[0].flags & RFLAG_SHOW == 0);
}

#[test]
fn a_question_with_no_adapter_still_has_its_chip() {
    let asking = View {
        question: None,
        display: Some("Type 0000 on Example Keyboard".to_owned()),
    };
    let rows = decoded(picture_with(4, &State::default(), &asking));
    let ids: Vec<u32> = rows.rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![ROW_CHIP, ROW_DISPLAY]);
    assert!(rows.rows[0].flags & RFLAG_SHOW != 0);
    assert!(rows.rows[1].flags & RFLAG_ATTENTION != 0);
    assert!(decoded(picture(4, &State::default())).rows.is_empty());
}

#[test]
fn the_agent_s_ids_are_fixed_below_the_device_bit() {
    for id in [
        ROW_QUESTION,
        ROW_CONFIRM,
        ROW_REJECT,
        ROW_DISPLAY,
        ROW_SEP_AGENT,
    ] {
        assert_eq!(id & DEVICE_ID_BIT, 0, "{id}");
        // The retired ids stay retired.
        assert!(![4, 5].contains(&id));
    }
}
