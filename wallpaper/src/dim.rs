//! The sidecar's half of a dim: which dim is up, how far it is, and the one
//! client that may be waiting on it (docs/abi.md, *Canvas event*).
//!
//! Every `dim` on the socket is a new dim with the next id. The host answers
//! with `CanvasEvent`s under that id: `CANVAS_EVENT_DIMMED` when it reached
//! its depth, `CANVAS_EVENT_INPUT` when input cancelled it. A client that
//! asked to `wait` is answered `done` on the first and `cancelled` on the
//! second, and also `cancelled` when its dim ends any other way: a newer dim
//! replaces it, or an undim takes it away. An event under any id but the
//! current one is a late word about a dim already replaced, and changes
//! nothing.
//!
//! Generic over the reply handle, so that the rules are tested without a
//! socket.

use caustics_bar_abi::wire::{CANVAS_EVENT_DIMMED, CANVAS_EVENT_INPUT};

/// The reply a waiting client gets when its dim reached its depth.
pub const DONE: &str = "done";
/// The reply a waiting client gets when its dim ended before that, or after
/// it by input.
pub const CANCELLED: &str = "cancelled";

/// Where the current dim is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// On its way, from `start_ms` over `duration_ms` on this process's
    /// clock (the one it stamped the dim with).
    Dimming { start_ms: u64, duration_ms: u32 },
    /// The host said it reached its depth; it stays there.
    Dimmed,
}

/// What an event from the host comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<R> {
    /// Nothing to do: an event for a dim that is not the current one.
    Stale,
    /// The dim reached its depth; answer the waiter, if any, [`DONE`].
    Done(Option<R>),
    /// Input cancelled the dim: send the host an undim, so both ends agree
    /// it is over, and answer the waiter, if any, [`CANCELLED`].
    Cancelled(Option<R>),
}

/// The current dim and its waiter.
#[derive(Debug)]
pub struct Dimmer<R> {
    /// The last id given out; the next dim's is one more (never 0).
    last: u32,
    /// The current dim's id and where it is.
    current: Option<(u32, Phase)>,
    /// The client waiting on the current dim.
    waiter: Option<R>,
}

impl<R> Default for Dimmer<R> {
    fn default() -> Self {
        Dimmer::new()
    }
}

impl<R> Dimmer<R> {
    pub fn new() -> Dimmer<R> {
        Dimmer {
            last: 0,
            current: None,
            waiter: None,
        }
    }

    /// A new dim from `start_ms` over `duration_ms`, waited on by `waiter`
    /// if one asked. Returns its id, and the waiter of the dim it replaced,
    /// to be answered [`CANCELLED`].
    pub fn begin(
        &mut self,
        start_ms: u64,
        duration_ms: u32,
        waiter: Option<R>,
    ) -> (u32, Option<R>) {
        self.last = self.last.wrapping_add(1).max(1);
        self.current = Some((
            self.last,
            Phase::Dimming {
                start_ms,
                duration_ms,
            },
        ));
        let replaced = std::mem::replace(&mut self.waiter, waiter);
        (self.last, replaced)
    }

    /// An undim: no dim. Returns the waiter, to be answered [`CANCELLED`].
    pub fn undim(&mut self) -> Option<R> {
        self.current = None;
        self.waiter.take()
    }

    /// An event from the host about dim `id`.
    pub fn event(&mut self, event: u32, id: u32) -> Outcome<R> {
        let Some((current, phase)) = self.current.as_mut() else {
            return Outcome::Stale;
        };
        if *current != id {
            return Outcome::Stale;
        }
        match event {
            CANVAS_EVENT_DIMMED => {
                *phase = Phase::Dimmed;
                Outcome::Done(self.waiter.take())
            }
            CANVAS_EVENT_INPUT => {
                self.current = None;
                Outcome::Cancelled(self.waiter.take())
            }
            _ => Outcome::Stale,
        }
    }

    /// `status`'s dim words at `now_ms`: `dim=none`, `dim=dimming
    /// progress=…`, or `dim=dimmed`.
    pub fn describe(&self, now_ms: u64) -> String {
        match self.current {
            None => "dim=none".to_owned(),
            Some((_, Phase::Dimmed)) => "dim=dimmed".to_owned(),
            Some((
                _,
                Phase::Dimming {
                    start_ms,
                    duration_ms,
                },
            )) => {
                let progress = if duration_ms == 0 {
                    1.0
                } else {
                    (now_ms.saturating_sub(start_ms) as f64 / f64::from(duration_ms)).min(1.0)
                };
                format!("dim=dimming progress={progress:.2}")
            }
        }
    }
}

#[cfg(test)]
mod tests;
