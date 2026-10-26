use super::*;
use crate::keys::{Typed, key_of};

const POLKIT: &str = "caustics-polkit";

fn prompt(id: u32, kind: u32, arg: u32, message: &str) -> Prompt {
    Prompt {
        id,
        kind,
        arg,
        message: message.to_owned(),
    }
}

fn typed(text: &str) -> Key {
    Key::Text(Typed::new(text.as_bytes()).expect("printable"))
}

/// Keys as xkbcommon would hand them over: by keysym.
fn keysym(sym: u32) -> Key {
    key_of(sym, b"", false).expect("a named key")
}

fn answers(effects: &[Effect]) -> Vec<&Answer> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Answer { answer, .. } => Some(answer),
            _ => None,
        })
        .collect()
}

/// A title holds the conversation without a panel; the ask raises it; what
/// is typed reaches the caustic as a count and never as text; Return writes
/// it down the pipe once and freezes the dots; done lowers the panel.
#[test]
fn a_conversation_asks_answers_and_ends() {
    let mut prompts = Prompts::new();
    let effects = prompts.frame(
        POLKIT,
        &prompt(1, PROMPT_INFO, PROMPT_INFO_TITLE, "Allow it?"),
    );
    assert!(
        effects.contains(&Effect::Raise),
        "the title raises the panel: the user sees the conversation before its ask"
    );
    assert!(prompts.takes_keys());
    let (facts, text, _) = prompts.facts();
    assert_eq!(
        (facts.state, facts.typed, facts.ask_len),
        (PROMPT_HELD, 0, 0)
    );
    assert_eq!(
        &text[facts.title_off as usize..][..facts.title_len as usize],
        b"Allow it?"
    );
    assert!(
        prompts.key(typed("x")).is_empty(),
        "keys do nothing while held"
    );

    let effects = prompts.frame(POLKIT, &prompt(1, PROMPT_SECRET, 0, "Password: "));
    assert!(
        !effects.contains(&Effect::Raise),
        "the ask raises nothing more: the panel is already up"
    );
    assert!(prompts.takes_keys());
    for text in ["s", "e", "c"] {
        assert!(prompts.key(typed(text)).contains(&Effect::Changed));
    }
    let (facts, text, cut) = prompts.facts();
    assert_eq!(
        (facts.state, facts.typed, facts.echo),
        (PROMPT_ASKING, 3, 0)
    );
    assert!(!cut);
    assert_eq!(
        &text[facts.title_off as usize..][..facts.title_len as usize],
        b"Allow it?"
    );
    assert_eq!(
        &text[facts.ask_off as usize..][..facts.ask_len as usize],
        b"Password: "
    );
    assert_eq!(facts.echo_len, 0);
    assert!(
        !text.windows(3).any(|w| w == b"sec"),
        "the secret is not in the text"
    );

    let effects = prompts.key(keysym(crate::keys::KEY_RETURN));
    assert_eq!(
        answers(&effects),
        vec![&Answer::Text {
            id: 1,
            bytes: caustics_bar_abi::wire::Secret::new(b"sec".to_vec())
        }]
    );
    let facts = prompts.facts().0;
    assert_eq!(
        (facts.state, facts.typed),
        (PROMPT_VERIFYING, 3),
        "the dots are frozen"
    );
    assert!(
        prompts.key(typed("x")).is_empty(),
        "keys are ignored while verifying"
    );
    assert!(prompts.key(keysym(crate::keys::KEY_RETURN)).is_empty());

    prompts.frame(POLKIT, &prompt(1, PROMPT_INFO, 0, "ok"));
    let effects = prompts.frame(POLKIT, &prompt(1, PROMPT_DONE, 1, ""));
    assert!(effects.contains(&Effect::Lower));
    assert!(!prompts.takes_keys());
    assert_eq!(prompts.facts().0.state, PROMPT_IDLE);
}

/// Escape while held dismisses the conversation like Escape while asking:
/// `ANSWER_CANCELLED` to the sink, the panes go, and the ask that follows is
/// dismissed again.
#[test]
fn escape_while_held_dismisses() {
    let mut prompts = Prompts::new();
    let effects = prompts.frame(
        POLKIT,
        &prompt(7, PROMPT_INFO, PROMPT_INFO_TITLE, "Allow it?"),
    );
    assert!(effects.contains(&Effect::Raise));
    let effects = prompts.key(keysym(crate::keys::KEY_ESCAPE));
    assert_eq!(answers(&effects), vec![&Answer::Cancelled { id: 7 }]);
    assert!(effects.contains(&Effect::Lower));
    assert!(!prompts.takes_keys());
    assert_eq!(prompts.facts().0.state, PROMPT_IDLE);
    let effects = prompts.frame(POLKIT, &prompt(7, PROMPT_SECRET, 0, ""));
    assert_eq!(answers(&effects), vec![&Answer::Cancelled { id: 7 }]);
    assert!(!effects.contains(&Effect::Raise));
}

/// An error line after an answer is a failed attempt, counted; the next ask
/// is asking again with the count kept and the error line shown.
#[test]
fn a_wrong_answer_is_an_attempt_and_the_next_ask_asks_again() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(4, PROMPT_SECRET, 0, "Password: "));
    prompts.key(typed("w"));
    prompts.key(keysym(crate::keys::KEY_RETURN));
    prompts.frame(POLKIT, &prompt(4, PROMPT_ERROR, 0, "wrong"));
    let facts = prompts.facts().0;
    assert_eq!(
        (facts.state, facts.attempts, facts.line_error),
        (PROMPT_FAILED, 1, 1)
    );
    assert!(
        prompts.key(typed("x")).is_empty(),
        "nothing is typed between asks"
    );

    let effects = prompts.frame(POLKIT, &prompt(4, PROMPT_SECRET, 0, "Password: "));
    assert!(
        !effects.contains(&Effect::Raise),
        "the panes are already up"
    );
    let (facts, text, _) = prompts.facts();
    assert_eq!(
        (facts.state, facts.attempts, facts.typed),
        (PROMPT_ASKING, 1, 0)
    );
    assert_eq!(
        &text[facts.line_off as usize..][..facts.line_len as usize],
        b"wrong"
    );
    // A second ask that follows an answer with no error between them (a
    // name, then a password) is not an attempt.
    prompts.key(typed("r"));
    prompts.key(keysym(crate::keys::KEY_RETURN));
    prompts.frame(POLKIT, &prompt(4, PROMPT_SECRET, 0, "Code: "));
    assert_eq!(prompts.facts().0.attempts, 1);
}

/// BackSpace takes a character, Control-U the line, and both change what
/// the caustic is told; on an empty entry they change nothing.
#[test]
fn backspace_and_clear_edit_the_entry() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(2, PROMPT_SECRET, 0, ""));
    assert!(prompts.key(keysym(crate::keys::KEY_BACKSPACE)).is_empty());
    for text in ["a", "é", "c"] {
        prompts.key(typed(text));
    }
    prompts.key(keysym(crate::keys::KEY_BACKSPACE));
    assert_eq!(prompts.facts().0.typed, 2);
    let clear = key_of(crate::keys::KEY_U, b"\x15", true).expect("Control-U");
    assert!(prompts.key(clear.clone()).contains(&Effect::Changed));
    assert_eq!(prompts.facts().0.typed, 0);
    assert!(prompts.key(clear).is_empty());
}

/// Escape dismisses at any point the panes are up, verifying included: one
/// `ANSWER_CANCELLED`, the panes down, the keys the dim's again. An ask
/// under the dismissed conversation is dismissed again; its end clears it;
/// a new conversation is a new prompt.
#[test]
fn escape_dismisses_and_the_conversation_waits_for_its_end() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(5, PROMPT_SECRET, 0, ""));
    prompts.key(typed("a"));
    prompts.key(keysym(crate::keys::KEY_RETURN));
    let effects = prompts.key(keysym(crate::keys::KEY_ESCAPE));
    assert_eq!(answers(&effects), vec![&Answer::Cancelled { id: 5 }]);
    assert!(effects.contains(&Effect::Lower));
    assert!(!prompts.takes_keys());
    assert_eq!(prompts.facts().0.state, PROMPT_IDLE);

    let effects = prompts.frame(POLKIT, &prompt(5, PROMPT_SECRET, 0, ""));
    assert_eq!(answers(&effects), vec![&Answer::Cancelled { id: 5 }]);
    assert!(!effects.contains(&Effect::Raise));
    assert!(
        prompts
            .frame(POLKIT, &prompt(5, PROMPT_DONE, 0, ""))
            .is_empty()
    );

    let effects = prompts.frame(POLKIT, &prompt(6, PROMPT_SECRET, 0, ""));
    assert!(effects.contains(&Effect::Raise));
}

/// One conversation at a time: an ask under another id, or from another
/// sidecar, is answered cancelled at once and changes nothing on screen; an
/// end for one that is not up is ignored.
#[test]
fn a_second_conversation_is_cancelled_at_once() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(1, PROMPT_SECRET, 0, ""));
    prompts.key(typed("a"));
    let effects = prompts.frame(POLKIT, &prompt(2, PROMPT_SECRET, 0, ""));
    assert_eq!(answers(&effects), vec![&Answer::Cancelled { id: 2 }]);
    let effects = prompts.frame(
        "caustics-lock",
        &prompt(1, PROMPT_INFO, PROMPT_INFO_TITLE, "x"),
    );
    assert_eq!(answers(&effects), vec![&Answer::Cancelled { id: 1 }]);
    assert!(matches!(&effects[1], Effect::Answer { sidecar, .. } if sidecar == "caustics-lock"));
    assert!(answers(&prompts.frame(POLKIT, &prompt(2, PROMPT_DONE, 0, ""))).is_empty());
    let facts = prompts.facts().0;
    assert_eq!((facts.id, facts.state, facts.typed), (1, PROMPT_ASKING, 1));
    // A withdrawn conversation takes the panes down.
    let effects = prompts.frame(POLKIT, &prompt(1, PROMPT_CANCEL, 0, ""));
    assert!(effects.contains(&Effect::Lower));
    assert!(
        answers(&effects).is_empty(),
        "a withdrawal is owed no answer"
    );
}

/// The asking sidecar's pipe closing takes its prompt down; another's does
/// not.
#[test]
fn a_gone_sidecar_takes_its_prompt() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(1, PROMPT_SECRET, 0, ""));
    assert!(prompts.gone("caustics-wallpaper").is_empty());
    let effects = prompts.gone(POLKIT);
    assert!(effects.contains(&Effect::Lower));
    assert!(!prompts.takes_keys());
}

/// An echo-on ask shows what is typed, and keeps showing it while verifying.
#[test]
fn an_echo_on_ask_shows_its_text() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(3, PROMPT_ECHO, 0, "Login as: "));
    for text in ["a", "n", "n"] {
        prompts.key(typed(text));
    }
    let (facts, text, _) = prompts.facts();
    assert_eq!(facts.echo, 1);
    assert_eq!(
        &text[facts.echo_off as usize..][..facts.echo_len as usize],
        b"ann"
    );
    prompts.key(keysym(crate::keys::KEY_RETURN));
    let (facts, text, _) = prompts.facts();
    assert_eq!(facts.state, PROMPT_VERIFYING);
    assert_eq!(
        &text[facts.echo_off as usize..][..facts.echo_len as usize],
        b"ann"
    );
}

/// The text is bounded by the buffer, cut at a character boundary, and says
/// so.
#[test]
fn the_text_is_cut_to_its_buffer() {
    let mut prompts = Prompts::new();
    let long = "é".repeat(1000);
    prompts.frame(POLKIT, &prompt(1, PROMPT_INFO, PROMPT_INFO_TITLE, &long));
    prompts.frame(POLKIT, &prompt(1, PROMPT_INFO, 0, &long));
    prompts.frame(POLKIT, &prompt(1, PROMPT_SECRET, 0, &long));
    let (facts, text, cut) = prompts.facts();
    assert!(cut);
    assert!(text.len() <= PROMPT_TEXT_BYTES);
    assert!(std::str::from_utf8(&text).is_ok());
    assert_eq!(facts.line_off + facts.line_len, text.len() as u32);
}

/// The state's `Debug` shows nothing typed, in either kind of ask.
#[test]
fn no_debug_of_the_prompt_shows_what_was_typed() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(1, PROMPT_SECRET, 0, "Password: "));
    for text in ["q", "u", "e", "x"] {
        prompts.key(typed(text));
    }
    let shown = format!("{prompts:?}");
    assert!(!shown.contains("quex"), "{shown}");
    let mut echo = Prompts::new();
    echo.frame(POLKIT, &prompt(1, PROMPT_ECHO, 0, ""));
    for text in ["z", "y", "w"] {
        echo.key(typed(text));
    }
    echo.key(keysym(crate::keys::KEY_RETURN));
    let shown = format!("{echo:?}");
    assert!(!shown.contains("zyw"), "{shown}");
}

const LOCK: &str = "caustics-lock";

/// The lock reserves the core: another sink's panel up at the lock is
/// dismissed (cancelled to its sink, the panel lowered), and another sink's
/// ask while locked is cancelled at once and shows nothing, even with no
/// conversation up; the lock's own conversation is taken, and once the
/// reservation ends any sink's is again.
#[test]
fn the_lock_reserves_the_prompt_for_its_own() {
    let mut prompts = Prompts::new();
    prompts.frame(POLKIT, &prompt(1, PROMPT_SECRET, 0, "Password: "));
    assert!(prompts.raised());
    prompts.key(typed("x"));
    let effects = prompts.reserve(Some(LOCK));
    assert!(effects.contains(&Effect::Lower), "{effects:?}");
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::Answer { sidecar, answer: Answer::Cancelled { id: 1 } } if sidecar == POLKIT))
            .count(),
        1
    );
    assert!(!prompts.raised());
    // polkit's end for the dismissed one, and a new ask of its own, while
    // locked: ignored, and cancelled.
    assert!(answers(&prompts.frame(POLKIT, &prompt(1, PROMPT_DONE, 0, ""))).is_empty());
    let effects = prompts.frame(POLKIT, &prompt(2, PROMPT_SECRET, 0, "Password: "));
    assert!(matches!(
        answers(&effects).as_slice(),
        [Answer::Cancelled { id: 2 }]
    ));
    assert!(!effects.contains(&Effect::Raise));
    assert!(!prompts.raised());
    // The lock's own replaces the dismissed one: its title raises the panel.
    let effects = prompts.frame(LOCK, &prompt(1, PROMPT_INFO, PROMPT_INFO_TITLE, "user"));
    assert!(answers(&effects).is_empty());
    assert!(effects.contains(&Effect::Raise));
    let effects = prompts.frame(LOCK, &prompt(1, PROMPT_SECRET, 0, "Password: "));
    assert!(!effects.contains(&Effect::Raise));
    assert!(prompts.takes_keys());
    // A reservation again for the same sidecar dismisses nothing of its own.
    assert!(prompts.reserve(Some(LOCK)).is_empty());
    assert!(prompts.raised());
    prompts.frame(LOCK, &prompt(1, PROMPT_DONE, 1, ""));
    prompts.reserve(None);
    let effects = prompts.frame(POLKIT, &prompt(3, PROMPT_SECRET, 0, "Password: "));
    assert!(effects.contains(&Effect::Raise), "{effects:?}");
}
