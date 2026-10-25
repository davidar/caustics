//! A stand-in for the canvas host's prompt and lock, for the nested gate and
//! for trying a sink sidecar by hand (`caustics-polkit`, `caustics-lock`): it
//! runs the sidecar on its pipes, reads its frames with the wire's own
//! reader, logs each one as a line, answers each ask from a script, and
//! confirms a lock or an unlock when told to. Not a sidecar and never
//! installed.
//!
//! ```text
//! fake_host <sidecar> [args…]
//! ```
//!
//! Its own environment (the sidecar inherits it, and with it whatever the
//! sidecar's own settings are):
//!
//! - `FAKE_HOST_SCRIPT`: one word per ask, in order: `text` answers with
//!   `$FAKE_HOST_SECRET`, `text:<VAR>` with the bytes of the variable `VAR`
//!   names, `cancel` dismisses the prompt. An ask past the end of the script
//!   is left open.
//! - `FAKE_HOST_SECRET`: the bytes a `text` answer carries. **Never
//!   logged**, nor any `text:<VAR>`'s: the log says `answered id=N with
//!   text` (or `with text:VAR`), and the gate greps the log, with the
//!   sidecar's witnesses in it, for every secret.
//! - `FAKE_HOST_EVENTS`: which of the lock's canvas events to send, as the
//!   host will once its lock surfaces exist: `locked` answers every
//!   `LOCK_LOCK` with `CANVAS_EVENT_LOCKED`, `unlocked` every `LOCK_UNLOCK`
//!   with `CANVAS_EVENT_UNLOCKED`. Unset, a lock is logged and never
//!   confirmed.
//!
//! Each line of the log is on stdout, `fake-host: …`. It exits with the
//! sidecar's status once the sidecar's stdout ends; killed, it takes the
//! sidecar's stdin with it, which is the sidecar's notice to leave.

use std::io::Write as _;
use std::process::{Command, Stdio};

use caustics_bar_abi::wire::{
    self, Answer, CANVAS_EVENT_LOCKED, CANVAS_EVENT_UNLOCKED, CanvasEventBody, Frame, LOCK_LOCK,
    LOCK_UNLOCK, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR, PROMPT_INFO,
    PROMPT_INFO_TITLE, PROMPT_SECRET, Secret,
};

fn log(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(format!("fake-host: {line}\n").as_bytes());
    let _ = out.flush();
}

fn kind(kind: u32, arg: u32) -> &'static str {
    match (kind, arg) {
        (PROMPT_INFO, PROMPT_INFO_TITLE) => "title",
        (PROMPT_SECRET, _) => "secret",
        (PROMPT_ECHO, _) => "echo",
        (PROMPT_INFO, _) => "info",
        (PROMPT_ERROR, _) => "error",
        (PROMPT_CANCEL, _) => "cancel",
        (PROMPT_DONE, _) => "done",
        _ => "unknown",
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(program) = args.next() else {
        log("usage: fake_host <sidecar> [args…]");
        std::process::exit(2);
    };
    let mut script: std::collections::VecDeque<String> = std::env::var("FAKE_HOST_SCRIPT")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let secret = std::env::var_os("FAKE_HOST_SECRET").unwrap_or_default();
    let events: Vec<String> = std::env::var("FAKE_HOST_EVENTS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let mut child = match Command::new(&program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            log(&format!("could not start {}: {e}", program.display()));
            std::process::exit(1);
        }
    };
    log(&format!(
        "started {} as pid {}",
        program.display(),
        child.id()
    ));
    let mut to = child.stdin.take().expect("piped");
    let mut from = child.stdout.take().expect("piped");
    loop {
        let frame = match wire::read(&mut from) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(e) => {
                log(&format!("the sidecar's stdout: {e}"));
                break;
            }
        };
        match frame {
            Frame::Hello(hello) => log(&format!("hello from {}", hello.name)),
            Frame::Prompt(prompt) => {
                log(&format!(
                    "prompt id={} kind={} arg={} message={:?}",
                    prompt.id,
                    kind(prompt.kind, prompt.arg),
                    prompt.arg,
                    prompt.message
                ));
                if !matches!(prompt.kind, PROMPT_SECRET | PROMPT_ECHO) {
                    continue;
                }
                let word = script.pop_front();
                let answer = match word.as_deref() {
                    Some("text") => Answer::Text {
                        id: prompt.id,
                        bytes: Secret::new(secret.as_encoded_bytes().to_vec()),
                    },
                    Some(named) if named.starts_with("text:") => Answer::Text {
                        id: prompt.id,
                        bytes: Secret::new(
                            std::env::var_os(&named["text:".len()..])
                                .unwrap_or_default()
                                .into_encoded_bytes(),
                        ),
                    },
                    Some("cancel") => Answer::Cancelled { id: prompt.id },
                    Some(other) => {
                        log(&format!(
                            "the script's {other:?} is no answer; leaving the ask open"
                        ));
                        continue;
                    }
                    None => {
                        log("the script is done; leaving the ask open");
                        continue;
                    }
                };
                // The script's word, which names a variable and never holds
                // its bytes.
                let with = word.unwrap_or_default();
                // `ChildStdin` is unbuffered: the frame `write` built, and
                // zeroes, is the only copy on this side.
                match wire::write(&mut to, &Frame::Answer(answer)) {
                    Ok(()) => log(&format!("answered id={} with {with}", prompt.id)),
                    Err(e) => log(&format!("the sidecar's stdin: {e}")),
                }
            }
            Frame::Lock(lock) => {
                let (command, event, word) = match lock.command {
                    LOCK_LOCK => ("lock", CANVAS_EVENT_LOCKED, "locked"),
                    LOCK_UNLOCK => ("unlock", CANVAS_EVENT_UNLOCKED, "unlocked"),
                    _ => ("unknown", 0, ""),
                };
                log(&format!("lock command={command}"));
                if event == 0 || !events.iter().any(|e| e == word) {
                    continue;
                }
                let frame = Frame::CanvasEvent(CanvasEventBody {
                    event,
                    ..CanvasEventBody::default()
                });
                match wire::write(&mut to, &frame) {
                    Ok(()) => log(&format!("sent {word}")),
                    Err(e) => log(&format!("the sidecar's stdin: {e}")),
                }
            }
            other => log(&format!("a frame of kind {}", other.kind())),
        }
    }
    drop(to);
    let status = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    log(&format!("the sidecar exited {status}"));
    std::process::exit(status);
}
