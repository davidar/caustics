//! The canvas's pairing sink, as this sidecar reaches it: one line each way
//! over `$XDG_RUNTIME_DIR/caustics-pairing.sock` (pairing/README.md, *The
//! socket*).
//!
//! ```text
//! pin <title>        → text <PIN>        | cancelled | error <why>
//! passkey <title>    → text <six digits> | cancelled | error <why>
//! cancel             → ok
//! status             → ok asking=yes|no
//! ```
//!
//! Only a question that must be typed comes here (`RequestPinCode`,
//! `RequestPasskey`); a yes or a no is the chip's own rows. **The reply is
//! read into a zeroing buffer** and handed up as a [`Secret`], which the
//! conductor turns into BlueZ's reply and drops; no copy of it reaches a
//! row, a witness or stdout.

use std::path::{Path, PathBuf};
use std::time::Duration;

use caustics_bar_abi::wire::Secret;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use zeroize::Zeroizing;

/// The socket's file name in `$XDG_RUNTIME_DIR`.
pub const SOCKET: &str = "caustics-pairing.sock";

/// The longest line the sink reads; a title is cut to fit under it.
pub const MAX_LINE: usize = 256;

/// The longest reply this side reads: `text ` and a PIN are far shorter,
/// and an `error …` past it is cut.
pub const MAX_REPLY: usize = 512;

/// How long `status` and `cancel` may take: the sink answers both at once,
/// so a sink that does not is as good as absent.
pub const QUICK: Duration = Duration::from_secs(2);

/// What a typed question asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typed {
    /// A legacy PIN, 1 to 16 bytes.
    Pin,
    /// A six-digit passkey.
    Passkey,
}

impl Typed {
    /// The command's first word.
    pub fn word(self) -> &'static str {
        match self {
            Typed::Pin => "pin",
            Typed::Passkey => "passkey",
        }
    }
}

/// What the sink answered a `pin` or a `passkey`.
#[derive(Debug)]
pub enum Reply {
    /// `text …`: what was typed, past the `text `.
    Text(Secret),
    /// `cancelled`: the person dismissed the prompt, or a `cancel` ended it.
    Cancelled,
    /// `error …`: the sink's words.
    Error(String),
    /// Nothing answers on the socket, or the line was cut off.
    Unreachable(String),
}

/// The socket in the session's runtime directory, or `None` without one.
pub fn socket_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(SOCKET))
}

/// `title` as one word of the line: a newline, a carriage return or a NUL
/// (which the sink refuses) becomes a space, and the whole is cut at a
/// character boundary to leave the command room.
pub fn line_title(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\0') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let max = MAX_LINE - "passkey \n".len();
    let mut end = clean.len().min(max);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    let cut = clean[..end].trim();
    if cut.is_empty() {
        "a device".to_owned()
    } else {
        cut.to_owned()
    }
}

/// Whether a sink answers `status`: the capability this sidecar registers
/// with follows it.
pub async fn answers(path: &Path) -> bool {
    matches!(
        tokio::time::timeout(QUICK, exchange(path, b"status\n")).await,
        Ok(Ok(reply)) if reply.starts_with(b"ok")
    )
}

/// Ask the sink for a PIN or a passkey and wait for the person to answer.
/// No timeout: the person may take as long as BlueZ lets them, and BlueZ's
/// `Cancel` reaches the sink as a [`cancel`] on a connection of its own.
pub async fn ask(path: &Path, typed: Typed, title: &str) -> Reply {
    let line = format!("{} {}\n", typed.word(), line_title(title));
    let reply = match exchange(path, line.as_bytes()).await {
        Ok(reply) => reply,
        Err(why) => return Reply::Unreachable(why),
    };
    if let Some(text) = reply.strip_prefix(b"text ") {
        return Reply::Text(Secret::new(text.to_vec()));
    }
    match &reply[..] {
        b"cancelled" => Reply::Cancelled,
        _ => match reply.strip_prefix(b"error ") {
            Some(why) => Reply::Error(String::from_utf8_lossy(why).into_owned()),
            None => Reply::Error("a reply the socket does not speak".to_owned()),
        },
    }
}

/// BlueZ withdrew the question: tell the sink, which takes its prompt down
/// and answers the waiting `pin` or `passkey` with `cancelled`.
pub async fn cancel(path: &Path) -> Result<(), String> {
    match tokio::time::timeout(QUICK, exchange(path, b"cancel\n")).await {
        Ok(Ok(reply)) if reply.starts_with(b"ok") => Ok(()),
        Ok(Ok(_)) => Err("the sink did not say ok".to_owned()),
        Ok(Err(why)) => Err(why),
        Err(_) => Err("the sink did not answer in time".to_owned()),
    }
}

/// One line out, one line back, the newline stripped. The reply is read
/// into a buffer made once at its full size, so it never moves, and zeroed
/// when it drops; the caller's copy of it is a [`Secret`].
async fn exchange(path: &Path, line: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut stream = UnixStream::connect(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    stream
        .write_all(line)
        .await
        .map_err(|e| format!("writing to the sink: {e}"))?;
    let mut buf = Zeroizing::new(vec![0u8; MAX_REPLY]);
    let mut len = 0;
    loop {
        if len == buf.len() {
            return Err("a reply longer than this side reads".to_owned());
        }
        let n = stream
            .read(&mut buf[len..])
            .await
            .map_err(|e| format!("reading the sink's reply: {e}"))?;
        if n == 0 {
            return Err("the sink closed the socket without a reply".to_owned());
        }
        if let Some(at) = buf[len..len + n].iter().position(|&b| b == b'\n') {
            buf.truncate(len + at);
            return Ok(buf);
        }
        len += n;
    }
}
