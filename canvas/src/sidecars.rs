//! The canvas's sidecars: which it starts, and where their events go.
//!
//! The supervisor is `caustics_host::pipe`, the bar's: one thread per child,
//! the doubling backoff, the *Hello* gate (docs/abi.md, *Hello*), the
//! resolution beside this host's own binary and the `$CAUSTICS_SIDECARS`
//! override. What is the canvas's is the set, compiled in because the canvas
//! has no config file, and the sink: every event goes whole down one channel
//! to the event loop, which is the only thing that touches the canvas's state.

use caustics_host::pipe::{self, Event, Pipe};
use smithay_client_toolkit::reexports::calloop::channel::Sender;

/// The pictures and the commands: the control socket, the decoding, the
/// fitting (`wallpaper/`).
pub const WALLPAPER: &str = "caustics-wallpaper";

/// The compiler: the canvas's caustic as it is saved (`reload/`).
pub const RELOAD: &str = "caustics-reload";

/// The set the canvas starts when `$CAUSTICS_SIDECARS` says nothing.
pub const SIDECARS: &[&str] = &[WALLPAPER, RELOAD];

/// Start every sidecar of the set, each sending its events to `events`.
pub fn start(events: &Sender<Event>) -> Vec<Pipe> {
    pipe::commands(SIDECARS)
        .iter()
        .map(|command| {
            let events = events.clone();
            Pipe::start(command, caustics_bar_abi::ABI_HASH, move |event| {
                events.send(event).is_ok()
            })
        })
        .collect()
}
