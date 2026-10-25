//! `polkit-agent-helper-1`: the setuid program that runs PAM for an agent
//! and reports the outcome to polkitd itself.
//!
//! # The protocol
//!
//! As read in polkit 127 (the version this was written against), in
//! `src/polkitagent/polkitagentsession.c` (the agent's side,
//! `polkit_agent_session_initiate`, `io_watch_have_data`,
//! `polkit_agent_session_response`) and `src/polkitagent/
//! polkitagenthelper-pam.c` with `polkitagenthelperprivate.c` (the helper's
//! side, `main`, `send_to_helper`, `conversation_function`, `read_cookie`),
//! and compared with soteria's `src/dbus.rs`, which speaks the same lines:
//!
//! 1. The agent spawns `<prefix>/lib/polkit-1/polkit-agent-helper-1 <user>`,
//!    one argument, the **name** of the user to authenticate (from the
//!    identity's uid, `getpwuid`), with stdin and stdout piped. The helper
//!    refuses a tty on stdin and clears its environment but `LANG` and
//!    `LANGUAGE`.
//! 2. The agent writes the **cookie and a newline** on the helper's stdin.
//!    (A third argument carrying the cookie is the pre-2015 form, kept by the
//!    helper for old agents; it puts the cookie on the command line, where
//!    every process can read it, and is never used here.)
//! 3. The helper starts PAM (`polkit-1` service) and writes one line on
//!    stdout per PAM message: `PAM_PROMPT_ECHO_OFF <text>`,
//!    `PAM_PROMPT_ECHO_ON <text>`, `PAM_ERROR_MSG <text>` or
//!    `PAM_TEXT_INFO <text>`, the text with one trailing newline dropped and
//!    then escaped by `g_strescape` (so the line has no newline in it; the
//!    agent undoes it with `g_strcompress`, [`unescape`]). The prefix and
//!    the text are separated by one space, which is there even when the
//!    text is empty.
//! 4. After each of the two prompts the helper reads **one line** from stdin
//!    with `fgets` into a `PAM_MAX_RESP_SIZE` (512) buffer and drops its
//!    newline: the agent's answer is its bytes and `\n`
//!    (`polkit_agent_session_response` adds the newline when the answer does
//!    not end in one). An answer of 511 bytes or more, or one with a newline
//!    in it, would be read as two answers, so [`answer_line`] refuses both.
//!    After an error or an info line it reads nothing.
//! 5. The helper ends with one line, `SUCCESS` or `FAILURE`, flushes, waits
//!    100 ms and exits: 0 after `SUCCESS`, 1 (or 2 for a failed
//!    `pam_authenticate`) after `FAILURE`. Before `SUCCESS` it has already
//!    told polkitd (`AuthenticationAgentResponse2`, with the cookie and the
//!    user), so the agent reports nothing upstream; it only learns the
//!    outcome.
//! 6. The agent's side: any other line, a read error or an EOF before
//!    `SUCCESS` is a failure. Cancelling is `SIGTERM` to the helper and a
//!    wait.
//!
//! This process requires both the `SUCCESS` line and an exit status of 0
//! before it calls a conversation a success.
//!
//! polkit 127 can also run the helper as a socket-activated service
//! (`/run/polkit/agent-helper.socket`): the agent connects, writes the user
//! name and a newline, then the cookie, and the lines are the same. That
//! socket is disabled by default on the distribution this was written on, and
//! this sidecar spawns the setuid helper only.
//!
//! **Nothing a helper line says is written to a witness.** A line names its
//! kind and its length at most; the text goes to the host as a `Prompt`, and
//! the answers go to the helper and nowhere else.

use std::ffi::OsString;
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use zeroize::Zeroizing;

/// The helper polkit installs, where it installs it.
pub const DEFAULT_PATH: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

/// `PAM_MAX_RESP_SIZE`: the helper's `fgets` buffer.
pub const PAM_MAX_RESP_SIZE: usize = 512;

/// The longest answer the helper reads as one: its buffer, less the newline
/// and the terminating NUL.
pub const MAX_ANSWER: usize = PAM_MAX_RESP_SIZE - 2;

/// The longest line read from the helper: a PAM message is at most
/// `PAM_MAX_MSG_SIZE` (512) bytes, which `g_strescape` makes at most four
/// times longer, behind the longest prefix. A longer line is not the helper
/// speaking.
pub const MAX_LINE: usize = 4 * 512 + 64;

/// One line from the helper's stdout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// `PAM_PROMPT_ECHO_OFF`: ask, echo off. An answer is owed.
    EchoOff(String),
    /// `PAM_PROMPT_ECHO_ON`: ask, echo on. An answer is owed.
    EchoOn(String),
    /// `PAM_ERROR_MSG`.
    Error(String),
    /// `PAM_TEXT_INFO`.
    Info(String),
    /// `SUCCESS`: the helper told polkitd the user is who they said.
    Success,
    /// `FAILURE`.
    Failure,
    /// Anything else. How long it was is all a witness may say.
    Unknown(usize),
}

/// One line, its newline already gone, as `io_watch_have_data` reads it:
/// unescaped first, then matched by prefix.
pub fn parse(line: &[u8]) -> Line {
    let text = unescape(line);
    let text = String::from_utf8_lossy(&text);
    let after = |prefix: &str| text.strip_prefix(prefix).map(str::to_owned);
    if let Some(rest) = after("PAM_PROMPT_ECHO_OFF ") {
        Line::EchoOff(rest)
    } else if let Some(rest) = after("PAM_PROMPT_ECHO_ON ") {
        Line::EchoOn(rest)
    } else if let Some(rest) = after("PAM_ERROR_MSG ") {
        Line::Error(rest)
    } else if let Some(rest) = after("PAM_TEXT_INFO ") {
        Line::Info(rest)
    } else if text.starts_with("SUCCESS") {
        Line::Success
    } else if text.starts_with("FAILURE") {
        Line::Failure
    } else {
        Line::Unknown(line.len())
    }
}

/// `g_strcompress`: undo `g_strescape`. `\b \f \n \r \t \v \" \\` and
/// one to three octal digits; a backslash before anything else stands for
/// that character, and a trailing backslash is dropped.
pub fn unescape(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        let byte = line[i];
        i += 1;
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let Some(&next) = line.get(i) else {
            break;
        };
        i += 1;
        match next {
            b'0'..=b'7' => {
                let mut value = u32::from(next - b'0');
                for _ in 0..2 {
                    match line.get(i) {
                        Some(&d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
            }
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            other => out.push(other),
        }
    }
    out
}

/// Why an answer cannot go to the helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfit {
    /// Longer than [`MAX_ANSWER`]: the helper would read it as two.
    TooLong,
    /// A newline or a NUL: the first ends the helper's line early, the
    /// second its string.
    Breaks,
}

/// The bytes written to the helper for one answer: the answer and a
/// newline, in a buffer zeroed when it drops.
pub fn answer_line(answer: &[u8]) -> Result<Zeroizing<Vec<u8>>, Unfit> {
    if answer.len() > MAX_ANSWER {
        return Err(Unfit::TooLong);
    }
    if answer.iter().any(|b| *b == b'\n' || *b == 0) {
        return Err(Unfit::Breaks);
    }
    let mut line = Zeroizing::new(Vec::with_capacity(answer.len() + 1));
    line.extend_from_slice(answer);
    line.push(b'\n');
    Ok(line)
}

/// What the reader task sends for its helper.
#[derive(Debug)]
pub enum Out {
    /// A line.
    Line(Line),
    /// stdout ended: the helper exited or is exiting.
    Ended,
}

/// A running helper: its stdin, and the child to wait on or kill.
pub struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: tokio::task::JoinHandle<()>,
}

impl Helper {
    /// Spawn `path <user>` with this process's environment and `env` over
    /// it, write the cookie on its stdin, and read its stdout on a task of
    /// its own, each line handed to `out`.
    pub async fn spawn(
        path: &Path,
        env: &[(OsString, OsString)],
        user: &str,
        cookie: &str,
        mut out: impl FnMut(Out) -> bool + Send + 'static,
    ) -> std::io::Result<Helper> {
        let mut child = Command::new(path)
            .arg(user)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped above");
        let stdout = child.stdout.take().expect("piped above");
        let mut line = Zeroizing::new(Vec::with_capacity(cookie.len() + 1));
        line.extend_from_slice(cookie.as_bytes());
        line.push(b'\n');
        if let Err(err) = stdin.write_all(&line).await {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(err);
        }
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout);
            let mut buf = Vec::with_capacity(256);
            loop {
                buf.clear();
                let read = (&mut lines)
                    .take(MAX_LINE as u64 + 1)
                    .read_until(b'\n', &mut buf)
                    .await;
                let line = match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) if buf.last() != Some(&b'\n') && buf.len() > MAX_LINE => {
                        Line::Unknown(buf.len())
                    }
                    Ok(_) => {
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                        }
                        parse(&buf)
                    }
                };
                let unknown = matches!(line, Line::Unknown(_));
                if !out(Out::Line(line)) || unknown {
                    return;
                }
            }
            out(Out::Ended);
        });
        Ok(Helper {
            child,
            stdin: Some(stdin),
            reader,
        })
    }

    /// Write one answer line, then let `line` go (it zeroes itself).
    pub async fn answer(&mut self, line: Zeroizing<Vec<u8>>) -> std::io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        };
        stdin.write_all(&line).await?;
        stdin.flush().await
    }

    /// Wait for the helper to exit, once its stdout has ended.
    pub async fn wait(mut self) -> std::io::Result<ExitStatus> {
        self.stdin = None;
        self.child.wait().await
    }

    /// `SIGTERM`, as `kill_helper` sends, and a wait; the reader stops.
    pub async fn kill(mut self) {
        self.reader.abort();
        self.stdin = None;
        if let Some(pid) = self.child.id() {
            // SAFETY: `pid` is this process's own unreaped child (`id()` is
            // `None` once it has been waited for), so the number cannot have
            // been reused by anything else.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
        let _ = self.child.wait().await;
    }
}

#[cfg(test)]
mod tests;
