//! A stand-in for the canvas host's prompt, for the nested gate and for
//! trying the sidecar by hand: it runs a sink sidecar on its pipes, reads
//! its frames with the wire's own reader, logs each one as a line, and
//! answers each ask from a script. Not a sidecar and never installed.
//!
//! ```text
//! fake_host <sidecar> [args…]
//! ```
//!
//! Its own environment (the sidecar inherits it, and with it whatever the
//! sidecar's own settings are):
//!
//! - `FAKE_HOST_SCRIPT`: one word per ask, in order: `text` answers with
//!   `$FAKE_HOST_SECRET`, `cancel` dismisses the prompt. An ask past the end
//!   of the script is left open.
//! - `FAKE_HOST_SECRET`: the bytes a `text` answer carries. **Never
//!   logged**: the log says `answered id=N with text`, and the gate greps the
//!   log, with the sidecar's witnesses in it, for the secret.
//!
//! Each line of the log is on stdout, `fake-host: …`. It exits with the
//! sidecar's status once the sidecar's stdout ends; killed, it takes the
//! sidecar's stdin with it, which is the sidecar's notice to leave.

use std::io::Write as _;
use std::process::{Command, Stdio};

use caustics_bar_abi::wire::{
    self, Answer, Frame, PROMPT_CANCEL, PROMPT_DONE, PROMPT_ECHO, PROMPT_ERROR, PROMPT_INFO,
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
                let answer = match script.pop_front().as_deref() {
                    Some("text") => Answer::Text {
                        id: prompt.id,
                        bytes: Secret::new(secret.as_encoded_bytes().to_vec()),
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
                let with = match answer {
                    Answer::Text { .. } => "text",
                    Answer::Cancelled { .. } => "cancel",
                };
                // `ChildStdin` is unbuffered: the frame `write` built, and
                // zeroes, is the only copy on this side.
                match wire::write(&mut to, &Frame::Answer(answer)) {
                    Ok(()) => log(&format!("answered id={} with {with}", prompt.id)),
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
