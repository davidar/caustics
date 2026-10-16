//! `$NIRI_SOCKET` — focus in, actions out.
//!
//! niri's IPC is a Unix socket that speaks one JSON `Request` per line and
//! answers one `Reply` line: `{"Ok": <Response>}` or `{"Err": "…"}`. Two
//! connections are used, and they are used differently:
//!
//! 1. **Actions.** One short-lived `UnixStream` per action: connect, write the
//!    line, read the reply, log it, close. A dock click is a local socket
//!    round trip and the embassy can afford to block on it.
//! 2. **The event stream.** One long-lived connection sending `"EventStream"`,
//!    read on its own thread. niri answers `{"Ok":"Handled"}` first and then
//!    streams bare `Event` objects, one per line, starting with the full
//!    current state. That first `Ok` line is *not* an event and is skipped.
//!
//! The protocol is niri's documented JSON IPC (`Request`, `Reply`,
//! `Response`, `Event`, `Window` in the `niri-ipc` crate). The few fields the
//! bar reads are walked by hand out of `serde_json::Value`; the crate is not
//! a dependency because the bar needs a dozen of its fields and taking it
//! would pin the bar to niri's release cadence.
//!
//! This feeder is the *only* source of `focused` and `urgent`
//! (ext-foreign-toplevel-list v1 has no state event at all), and it agrees
//! with the ext feeder on the window id, so the two halves join in
//! [`WindowSet`] before anything is sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::WindowSet;
use super::compositor::Compositor;
use crate::model::{Action, Update, Updates};

/// How long to wait before dialling the event stream again after it dropped.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);

/// The niri IPC feeder: one implementation of the [`Compositor`] seam
/// (`feeders/compositor.rs`), and the only one there is.
pub struct NiriIpc {
    socket: Option<PathBuf>,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for NiriIpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NiriIpc")
            .field("socket", &self.socket)
            .finish()
    }
}

impl Drop for NiriIpc {
    fn drop(&mut self) {
        // The reader thread is parked in a blocking `read`, so this only takes
        // effect at the next line or reconnect. It is not joined: the thread
        // holds nothing the process needs at exit, and the channel closing is
        // the path that actually stops it.
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl NiriIpc {
    /// The name this feeder reports itself absent under, and the one its
    /// witness lines carry (`compositor.rs`, *The witness word*).
    pub const ABSENT: &'static str = "niri-ipc";
}

impl Compositor for NiriIpc {
    fn name(&self) -> &'static str {
        Self::ABSENT
    }

    /// Find the socket and start the event-stream thread.
    ///
    /// No socket is not an error: one witness line, one
    /// `Update::Absent("niri-ipc")`, and every action afterwards is a witness
    /// and a drop. The bar still shows whatever the Wayland side knows.
    fn start(windows: WindowSet, updates: Updates) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let socket = match std::env::var_os("NIRI_SOCKET").map(PathBuf::from) {
            Some(path) if path.exists() => Some(path),
            Some(path) => {
                tracing::info!("niri-ipc absent: $NIRI_SOCKET is {path:?}, which does not exist");
                None
            }
            None => {
                tracing::info!("niri-ipc absent: $NIRI_SOCKET is unset");
                None
            }
        };

        let Some(path) = socket else {
            let _ = updates.send(Update::Absent(Self::ABSENT.to_owned()));
            return Self { socket: None, stop };
        };

        tracing::info!("bound niri-ipc at {path:?}");
        let thread_path = path.clone();
        let thread_stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("niri-events".into())
            .spawn(move || event_stream(thread_path, windows, updates, thread_stop))
            .expect("spawning the niri event-stream thread");

        Self {
            socket: Some(path),
            stop,
        }
    }

    /// Whether the socket was found.
    fn is_bound(&self) -> bool {
        self.socket.is_some()
    }

    /// Perform one action. Returns whether it belongs to this feeder — which
    /// is true for all four of its kinds even when the socket is missing, so
    /// the caller does not go looking for another owner.
    fn perform(&mut self, action: &Action) -> bool {
        let request = match action {
            Action::Activate(id) => json!({ "Action": { "FocusWindow": { "id": id } } }),
            Action::Close(id) => json!({ "Action": { "CloseWindow": { "id": id } } }),
            Action::ToggleOverview => json!({ "Action": { "ToggleOverview": {} } }),
            // A dock launch. niri's `Action::Spawn` takes the argv
            // whole and starts it the way a keybind does — its own systemd
            // scope, the session's environment — which is the compositor's
            // job and not the bar's.
            Action::Launch { argv } => json!({ "Action": { "Spawn": { "command": argv } } }),
            // Log Out is not here: it is `caustics-power`'s — the sidecar that
            // owns the menu owns what its rows do, and it speaks the one line
            // of niri IPC itself.
            _ => return false,
        };

        let Some(socket) = self.socket.as_deref() else {
            tracing::info!("niri-ipc {action:?}: dropped, no socket");
            return true;
        };

        match round_trip(socket, &request) {
            Ok(reply) => tracing::info!("niri-ipc {action:?}: {}", reply.trim()),
            Err(err) => tracing::info!("niri-ipc {action:?}: failed, {err}"),
        }
        true
    }
}

/// One request, one reply, one connection.
fn round_trip(socket: &Path, request: &Value) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{request}")?;
    stream.flush()?;
    // niri closes its write half after a non-streaming reply, so reading to
    // the end is the same as reading the one line and cheaper to get right.
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.take(64 * 1024).read_to_string(&mut reply)?;
    Ok(reply)
}

/// The event-stream thread: connect, translate every line, reconnect on loss.
fn event_stream(socket: PathBuf, windows: WindowSet, updates: Updates, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match open_stream(&socket) {
            Ok(reader) => {
                tracing::info!("niri-ipc event stream open");
                for line in reader.lines() {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let line = match line {
                        Ok(line) => line,
                        Err(err) => {
                            tracing::info!("niri-ipc event stream read failed: {err}");
                            break;
                        }
                    };
                    for update in translate(&line, &windows) {
                        // A closed channel means the embassy is gone, so this thread ends.
                        if updates.send(update).is_err() {
                            return;
                        }
                    }
                }
                tracing::info!("niri-ipc event stream closed");
            }
            Err(err) => tracing::info!("niri-ipc event stream failed to open: {err}"),
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(RECONNECT_BACKOFF);
    }
}

fn open_stream(socket: &Path) -> std::io::Result<BufReader<UnixStream>> {
    let mut stream = UnixStream::connect(socket)?;
    writeln!(stream, "{}", json!("EventStream"))?;
    stream.flush()?;
    Ok(BufReader::new(stream))
}

// ---------------------------------------------------------------------------
// The translation, which is the whole testable part
// ---------------------------------------------------------------------------

/// One event-stream line into the updates it implies, folding the niri half of
/// each window into `windows` on the way.
///
/// A line that is not an event we use — the opening `{"Ok":"Handled"}` reply,
/// workspaces, keyboard layouts, casts — yields nothing and is not an error.
pub fn translate(line: &str, windows: &WindowSet) -> Vec<Update> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        tracing::info!("niri-ipc: unparseable event line, ignored");
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let Some((tag, body)) = object.iter().next() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    match tag.as_str() {
        // The full current state. Anything missing from it has closed.
        "WindowsChanged" => {
            let listed = body.get("windows").and_then(Value::as_array);
            let mut seen = Vec::new();
            for window in listed.into_iter().flatten() {
                if let Some(id) = merge_window(window, windows, &mut out) {
                    seen.push(id);
                }
            }
            for id in windows.ids() {
                if !seen.contains(&id) {
                    windows.remove(id);
                    out.push(Update::WindowClosed(id));
                }
            }
        }
        "WindowOpenedOrChanged" => {
            if let Some(window) = body.get("window") {
                merge_window(window, windows, &mut out);
            }
        }
        "WindowClosed" => {
            if let Some(id) = body.get("id").and_then(Value::as_u64) {
                windows.remove(id);
                out.push(Update::WindowClosed(id));
            }
        }
        "WindowFocusChanged" => {
            let id = body.get("id").and_then(Value::as_u64);
            out.extend(windows.set_focus(id).into_iter().map(Update::Window));
        }
        "WindowUrgencyChanged" => {
            let id = body.get("id").and_then(Value::as_u64);
            let urgent = body.get("urgent").and_then(Value::as_bool).unwrap_or(false);
            if let Some(window) = id.and_then(|id| windows.set_urgent(id, urgent)) {
                out.push(Update::Window(window));
            }
        }
        _ => {}
    }
    out
}

/// One niri `Window` object into the shared set. Returns its id when the
/// object was well formed, and pushes the updates its arrival implies —
/// including unfocusing everything else, which `WindowOpenedOrChanged`
/// promises.
fn merge_window(window: &Value, windows: &WindowSet, out: &mut Vec<Update>) -> Option<u64> {
    let id = window.get("id").and_then(Value::as_u64)?;
    // `title` and `app_id` are `Option<String>` on the wire; null is "".
    let title = window
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let app_id = window
        .get("app_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let focused = window
        .get("is_focused")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let urgent = window
        .get("is_urgent")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if let Some(merged) = windows.merge_state(id, app_id, title, focused, urgent) {
        out.push(Update::Window(merged));
    }
    if focused {
        // The focused window is unique; everything else loses the flag. The
        // one just merged already has it, so it is not touched again.
        out.extend(windows.set_focus(Some(id)).into_iter().map(Update::Window));
    }
    Some(id)
}

#[cfg(test)]
mod tests;
