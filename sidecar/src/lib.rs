//! `caustics-sidecar` — what every bar sidecar wants and none should write twice.
//!
//! - [`Witness`]: one line on stderr, prefixed with the sidecar's name, in
//!   **one write**. The prefix is the binary's name, set once in a `const`.
//! - [`drain`] and its [`Drain`] guard (feature `drain`): a task that reads one
//!   signal stream and does nothing else, and [`forward`], the same task for a stream
//!   whose *payload* is used, translating into an unbounded channel.
//! - [`inbox`]: a reader thread and a clock, for a sidecar that answers
//!   requests and must also wake on its own.
//! - [`icons`] (feature `icons`): the icon-theme search `caustics-apps`,
//!   `caustics-glyphs` and `caustics-tray` share.
//! - [`settings`]: which program a menu's "Settings" row opens — the
//!   `$CAUSTICS_SETTINGS` override, else the desktop's own, else the first known
//!   one on `PATH`, else none — so no sidecar names a settings program.
//! - [`session`] (feature `session`): this process's uid and user name, and
//!   logind's session for it, which the canvas's polkit and lock sinks both
//!   speak for.
//! - [`test_bus`] (feature `test-bus`): a private `dbus-daemon` for the D-Bus
//!   sidecars' deadlock tests.
//!
//! The rule the drain is for, as docs/architecture.md *Rules* has it: **a zbus stream
//! is a bounded channel the socket reader blocks on; a stream is drained by a
//! task that awaits nothing else.**

use std::io::Write as _;

/// A sidecar's witness: its name, fixed once.
///
/// ```
/// const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-demo");
/// pub fn witness(line: &str) {
///     WITNESS.line(line)
/// }
/// # witness("started");
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Witness {
    name: &'static str,
}

impl Witness {
    /// The prefix every line gets: the sidecar's binary name, `caustics-tray`.
    pub const fn new(name: &'static str) -> Self {
        Witness { name }
    }

    /// Whose witness this is.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// One line on stderr, in **one write** (docs/abi.md, *Writing a sidecar*).
    ///
    /// Not `eprintln!`: `Stderr` is unbuffered and `write_fmt` issues a write
    /// per piece of the format string. A sidecar's stderr is the bar's own,
    /// inherited, so those pieces interleave with the bar's tracing and
    /// journald makes an entry of whatever lands between two newlines — one witness can become three. The
    /// whole line is built first and handed to one `write_all` under the
    /// stderr lock.
    pub fn line(&self, line: &str) {
        let bytes = self.render(line);
        let _ = std::io::stderr().lock().write_all(&bytes);
    }

    /// The bytes [`Witness::line`] writes: `"<name>: <line>\n"`.
    pub fn render(&self, line: &str) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.name.len() + line.len() + 3);
        bytes.extend_from_slice(self.name.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        bytes
    }
}

#[cfg(feature = "drain")]
mod drain;
#[cfg(feature = "drain")]
pub use drain::{Drain, drain, forward};

#[cfg(feature = "icons")]
pub mod icons;

pub mod inbox;

pub mod settings;

#[cfg(feature = "session")]
pub mod session;

#[cfg(feature = "test-bus")]
pub mod test_bus;

#[cfg(test)]
mod tests;
