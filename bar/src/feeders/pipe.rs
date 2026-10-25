//! The bar's sidecars: which ones it starts, and what their frames become.
//!
//! The supervisor is `caustics_host::pipe` (docs/architecture.md, *Sidecars*):
//! one thread per child, the doubling backoff, the *Hello* gate (docs/abi.md,
//! *Hello*), the resolution beside the bar's own binary and the
//! `$CAUSTICS_SIDECARS` override. What is the bar's is the set it starts,
//! compiled in because the bar has no config file ([`SIDECARS`]), the three of
//! them it names elsewhere, and [`update_of`], which turns what a pipe reports
//! into the [`Update`] the model applies. A samples body goes on as
//! [`Update::Samples`] and reaches binding 10 without the bar looking inside
//! it; the caustic is the only thing that reads a row.

pub use caustics_host::pipe::{Event, Pipe, name_of};

#[cfg(test)]
use caustics_host::pipe::SIDECARS_ENV;

use crate::abi::{self, wire::Frame};
use crate::model::{Update, Updates};

/// The sidecars the bar starts when `$CAUSTICS_SIDECARS` says nothing. Compiled
/// in, because the bar has no config file and never will.
///
/// The sampler, then the applets that keep D-Bus and the rest of the system
/// out of this process — `caustics-tray` on the session bus, `caustics-power` on the
/// system bus, and so on. Each is absent-tolerant in the ordinary way: a binary
/// that is not installed beside the bar is one `Absent` and one witness.
///
/// [`APPS_SIDECAR`] publishes nothing: it writes only in reply to a `Request`
/// (docs/abi.md, *Request*), so a bar without it has a dock of app ids and no
/// colour icons, and nothing else about it is different.
///
/// [`GLYPHS_SIDECAR`] answers as well, but it also *fills*:
/// the caustic's six font buffers arrive from it as `Bytes` runs
/// (docs/abi.md, *Bytes*), and a symbolic icon name becomes a glyph id. Its
/// absence is the one that is not cosmetic — a bar with no text in it.
///
/// [`RELOAD_SIDECAR`] is the only one whose absence costs
/// nothing a running bar can see: without it the caustic on the GPU is the
/// baked one and stays the baked one, and the bar simply never hot-reloads.
pub const SIDECARS: &[&str] = &[
    "caustics-sysmon",
    "caustics-tray",
    "caustics-power",
    "caustics-audio",
    "caustics-network",
    "caustics-bluetooth",
    // The session's notification daemon. Its chip waits
    // for `org.freedesktop.Notifications`, so beside another daemon it is a
    // running sidecar with no frame.
    "caustics-notify",
    APPS_SIDECAR,
    GLYPHS_SIDECAR,
    RELOAD_SIDECAR,
];

/// The sidecar an app id or a themed icon name is asked of. Named rather than spelt out at the call sites, because `facts.rs` asks it for every dock entry and every icon the caustic wants and the two ends must agree.
pub const APPS_SIDECAR: &str = "caustics-apps";

/// The sidecar the font and every symbolic icon come from.
/// Asked for an icon name *first*: only what it answers `GLYPH_MISSING` for
/// goes on to [`APPS_SIDECAR`] as pixels.
pub const GLYPHS_SIDECAR: &str = "caustics-glyphs";

/// The sidecar the caustic itself comes from: it watches
/// the `.slang` files, compiles them with Slang in its own process and writes
/// `Module` frames. Asked once, for
/// the swapchain format the device negotiated (`REQ_PROGRAM`), and thereafter
/// it speaks unprompted like the sampler does.
pub const RELOAD_SIDECAR: &str = "caustics-reload";

/// The commands to run: `$CAUSTICS_SIDECARS` if it names any, else
/// [`SIDECARS`], each resolved beside the bar's own binary first
/// (`caustics_host::pipe::commands`).
pub fn commands() -> Vec<String> {
    caustics_host::pipe::commands(SIDECARS)
}

/// Spawn one sidecar whose events reach the model as updates, greeted against
/// this build's contract.
pub fn start(command: &str, updates: Updates) -> Pipe {
    Pipe::start(command, abi::ABI_HASH, move |event| {
        match update_of(event) {
            Some(update) => updates.send(update).is_ok(),
            None => true,
        }
    })
}

/// What a pipe's event is to the model: an absence is a witness, a gone
/// sidecar takes its state with it, and a frame is a samples body, a rows
/// body, or an answer the embassy copies where it goes (`wayland.rs`). An
/// exit before a restart is nothing to the bar: the pipe has said so in the
/// journal, and the rows and samples stay until the successor replaces them.
pub fn update_of(event: Event) -> Option<Update> {
    Some(match event {
        Event::Absent(name) => Update::Absent(name),
        Event::Gone(name) => Update::SidecarGone(name),
        Event::Exited(_) => return None,
        Event::Frame {
            sidecar,
            frame: Frame::Samples(body),
        } => Update::Samples { sidecar, body },
        Event::Frame {
            sidecar,
            frame: Frame::Rows(rows),
        } => Update::Rows { sidecar, rows },
        Event::Frame { sidecar, frame } => Update::Frame { sidecar, frame },
    })
}

#[cfg(test)]
mod tests;
