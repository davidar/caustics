//! Log Out: niri's `Quit` over `$NIRI_SOCKET` (the one
//! row of this menu that is not D-Bus at all).
//!
//! The sidecar that owns the menu owns what its rows do, so `caustics-power`
//! speaks this one line of niri IPC itself. A sidecar knowing its compositor
//! is fine; the bar knowing that a row means "log out" is not.
//!
//! `skip_confirmation: false` is the point: niri puts up its own "really
//! quit?", and this menu does not have to.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// The request, verbatim. A literal rather than a `json!` because this crate
/// has no serde and one fixed line does not need one — and because the test
/// asserts the exact bytes a Log Out row would send.
pub const QUIT: &str = r#"{"Action":{"Quit":{"skip_confirmation":false}}}"#;

/// The environment variable niri sets to its IPC socket.
pub const SOCKET_ENV: &str = "NIRI_SOCKET";

/// How long either half of the round trip may take. Blocking I/O, so the
/// caller runs [`quit`] under `spawn_blocking`: the loop's thread is
/// also the one zbus's reader shares.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// One request, one reply, one connection, and the reply read (a reply
/// discarded would witness an `Err` from niri as "ok"). `Err` carries the
/// witness's text — including "the socket is not there", which is not a
/// failure so much as a compositor that is not niri.
pub fn quit() -> Result<(), String> {
    let socket = std::env::var_os(SOCKET_ENV).ok_or_else(|| format!("${SOCKET_ENV} is unset"))?;
    let reply = round_trip(&socket, QUIT).map_err(|e| e.to_string())?;
    outcome(&reply)
}

/// niri's reply to an action, judged: `{"Ok":"Handled"}` is done, and
/// anything else — `{"Err":"…"}`, nothing at all, a reply this sidecar does
/// not know — is said as niri said it. No serde: the reply's first key is the
/// whole question.
pub fn outcome(reply: &str) -> Result<(), String> {
    let reply = reply.trim();
    if reply.starts_with(r#"{"Ok""#) {
        return Ok(());
    }
    if reply.is_empty() {
        return Err("niri closed the socket without a reply".to_owned());
    }
    Err(format!("niri answered {reply}"))
}

fn round_trip(socket: &OsStr, request: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    writeln!(stream, "{request}")?;
    stream.flush()?;
    // niri closes its write half after a non-streaming reply, so reading to
    // the end is the same as reading the one line and cheaper to get right.
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.take(64 * 1024).read_to_string(&mut reply)?;
    Ok(reply.trim().to_owned())
}
