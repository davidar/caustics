//! The dry run and PAM's conversation function, driven without PAM: the
//! function gets messages laid out as Linux-PAM lays them out, and a thread
//! plays the conductor.

use std::thread::JoinHandle;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::*;

/// What the conductor's side does with each ask, in order.
enum Then {
    Type(&'static [u8]),
    Dismiss,
}

/// A conversation whose conductor answers from `script` and records every
/// step it saw as `(kind, message)`.
fn conversation(script: Vec<Then>) -> (Conversation, JoinHandle<Vec<(String, String)>>) {
    let (events, inbox) = unbounded_channel();
    let player = std::thread::spawn(move || play(inbox, script));
    (Conversation::new(9, events), player)
}

fn play(mut inbox: UnboundedReceiver<Event>, script: Vec<Then>) -> Vec<(String, String)> {
    let mut script = script.into_iter();
    let mut seen = Vec::new();
    while let Some(event) = inbox.blocking_recv() {
        let Event::Attempt { id: 9, step } = event else {
            panic!("an event that is not conversation 9's");
        };
        match step {
            Step::Ask {
                echo,
                message,
                reply,
            } => {
                seen.push((if echo { "echo" } else { "secret" }.to_owned(), message));
                let _ = reply.send(match script.next() {
                    Some(Then::Type(bytes)) => Reply::Text(Secret::new(bytes.to_vec())),
                    Some(Then::Dismiss) => Reply::Cancelled,
                    None => Reply::Gone,
                });
            }
            Step::Show { error, message } => {
                seen.push((if error { "error" } else { "info" }.to_owned(), message))
            }
            Step::Ended(verdict) => seen.push(("ended".to_owned(), format!("{verdict:?}"))),
        }
    }
    seen
}

#[test]
fn the_dry_run_asks_once_and_compares() {
    let dry = DryRun::new(Secret::new(b"open".to_vec()));
    for (typed, verdict) in [
        (Then::Type(b"open"), Verdict::Success),
        (
            Then::Type(b"opens"),
            Verdict::Failed(DRY_RUN_FAILURE.to_owned()),
        ),
        (Then::Dismiss, Verdict::Dismissed),
    ] {
        let (mut conv, player) = conversation(vec![typed]);
        assert_eq!(dry.authenticate("tester", &mut conv), verdict);
        drop(conv);
        assert_eq!(
            player.join().unwrap(),
            vec![("secret".to_owned(), DRY_RUN_ASK.to_owned())]
        );
    }
}

/// Linux-PAM's layout: an array of pointers to messages.
struct Messages {
    _texts: Vec<CString>,
    messages: Vec<PamMessage>,
    pointers: Vec<*mut PamMessage>,
}

fn messages(list: &[(c_int, &str)]) -> Messages {
    let texts: Vec<CString> = list
        .iter()
        .map(|(_, text)| CString::new(*text).unwrap())
        .collect();
    let mut messages: Vec<PamMessage> = list
        .iter()
        .zip(&texts)
        .map(|((style, _), text)| PamMessage {
            msg_style: *style,
            msg: text.as_ptr(),
        })
        .collect();
    let pointers = messages.iter_mut().map(|m| m as *mut PamMessage).collect();
    Messages {
        _texts: texts,
        messages,
        pointers,
    }
}

/// `converse_with` over `list`, the conductor answering from `script`: its
/// return, the responses it made (as strings, `None` for a null one), and
/// what the conductor saw.
fn converse_over(
    list: &[(c_int, &str)],
    script: Vec<Then>,
) -> (
    c_int,
    Option<Vec<Option<String>>>,
    bool,
    Vec<(String, String)>,
) {
    let mut m = messages(list);
    let (mut conv, player) = conversation(script);
    let mut callback = Callback {
        conv: &mut conv,
        dismissed: false,
    };
    let mut resp: *mut PamResponse = std::ptr::null_mut();
    // SAFETY: the layout Linux-PAM uses, every pointer live for the call.
    let rc = unsafe {
        converse_with(
            m.pointers.len() as c_int,
            m.pointers.as_mut_ptr(),
            &mut resp,
            (&mut callback as *mut Callback<'_>).cast(),
        )
    };
    let dismissed = callback.dismissed;
    let made = (!resp.is_null()).then(|| {
        let made = (0..m.messages.len())
            .map(|i| {
                // SAFETY: the array `converse_with` made, `len` long.
                let r = unsafe { (*resp.add(i)).resp };
                (!r.is_null()).then(|| {
                    // SAFETY: a `c_copy`, freed here as PAM would.
                    let s = unsafe { CStr::from_ptr(r) }.to_string_lossy().into_owned();
                    unsafe { libc::free(r.cast()) };
                    s
                })
            })
            .collect();
        // SAFETY: as above.
        unsafe { libc::free(resp.cast()) };
        made
    });
    drop(callback);
    drop(conv);
    (rc, made, dismissed, player.join().unwrap())
}

#[test]
fn every_message_reaches_the_conductor_and_each_ask_gets_its_response() {
    let (rc, made, dismissed, seen) = converse_over(
        &[
            (TEXT_INFO, "Hello"),
            (PROMPT_ECHO_ON, "Code: "),
            (ERROR_MSG, "Careful"),
            (PROMPT_ECHO_OFF, "Password: "),
        ],
        vec![Then::Type(b"42"), Then::Type(b"hunter2")],
    );
    assert_eq!(rc, SUCCESS);
    assert!(!dismissed);
    assert_eq!(
        made,
        Some(vec![
            None,
            Some("42".to_owned()),
            None,
            Some("hunter2".to_owned())
        ])
    );
    let kinds: Vec<&str> = seen.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(kinds, ["info", "echo", "error", "secret"]);
    assert_eq!(seen[3].1, "Password: ");
}

#[test]
fn a_dismissal_refuses_the_whole_conversation_and_says_so() {
    let (rc, made, dismissed, _) =
        converse_over(&[(PROMPT_ECHO_OFF, "Password: ")], vec![Then::Dismiss]);
    assert_eq!((rc, made, dismissed), (CONV_ERR, None, true));
}

#[test]
fn nobody_to_answer_refuses_without_a_dismissal() {
    let (rc, made, dismissed, _) = converse_over(&[(PROMPT_ECHO_OFF, "Password: ")], Vec::new());
    assert_eq!((rc, made, dismissed), (CONV_ERR, None, false));
}

/// An answer with a NUL in it cannot be a C string; PAM would read a prefix
/// of it as the answer, so the conversation is refused instead.
#[test]
fn an_answer_with_a_nul_in_it_is_refused() {
    let (rc, made, _, _) = converse_over(
        &[(PROMPT_ECHO_ON, "User: "), (PROMPT_ECHO_OFF, "Password: ")],
        vec![Then::Type(b"tester"), Then::Type(b"pre\0fix")],
    );
    assert_eq!((rc, made), (BUF_ERR, None));
}

#[test]
fn an_unknown_style_and_a_bad_count_are_refused() {
    let (rc, made, _, seen) = converse_over(&[(99, "?")], Vec::new());
    assert_eq!((rc, made), (CONV_ERR, None));
    assert!(seen.is_empty());
    let mut resp: *mut PamResponse = std::ptr::null_mut();
    // SAFETY: refused before anything is read.
    let rc = unsafe { converse_with(0, std::ptr::null_mut(), &mut resp, std::ptr::null_mut()) };
    assert_eq!(rc, CONV_ERR);
    assert!(resp.is_null());
}

#[test]
fn a_service_with_a_nul_is_no_service() {
    assert!(LinuxPam::new("swaylock").is_some());
    assert!(LinuxPam::new("sway\0lock").is_none());
}
