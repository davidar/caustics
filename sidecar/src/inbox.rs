//! A reader thread and a clock, for a sidecar that answers requests on stdin and must also wake on a clock.
//!
//! `caustics-apps` and `caustics-glyphs` answer requests and also re-check their misses
//! on a period. A blocking `wire::read` on stdin cannot time out, so the read
//! runs on a thread that hands each frame
//! over a channel, and the loop waits on the channel **with a deadline**: a
//! frame is [`Next::Item`], the deadline passing is [`Next::Tick`], and the
//! reader finishing (EOF, or the error it handed over last) is
//! [`Next::Closed`]. Std only — neither sidecar has an async runtime, and one
//! is not worth having for a clock.

use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// What the loop wakes up to.
#[derive(Debug, PartialEq, Eq)]
pub enum Next<T> {
    /// The reader produced one.
    Item(T),
    /// A period passed since the last tick.
    Tick,
    /// The reader is done: it returned `None`, or its thread ended.
    Closed,
}

/// The receiving end: one reader thread's items, and a tick every `period`.
pub struct Inbox<T> {
    rx: Receiver<T>,
    period: Duration,
    next_tick: Instant,
}

impl<T: Send + 'static> Inbox<T> {
    /// Start `read` on a thread of its own, called until it returns `None`;
    /// the first tick is one `period` from now.
    pub fn spawn(period: Duration, mut read: impl FnMut() -> Option<T> + Send + 'static) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            while let Some(item) = read() {
                if tx.send(item).is_err() {
                    break;
                }
            }
        });
        Inbox {
            rx,
            period,
            next_tick: Instant::now() + period,
        }
    }
}

impl<T> Inbox<T> {
    /// The next item, or the tick if it comes first. An item that is waiting
    /// is taken before a tick that is due, so a busy reader never starves —
    /// and a tick that is overdue fires on the next call that finds the
    /// channel empty.
    pub fn next(&mut self) -> Next<T> {
        let wait = self.next_tick.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(wait) {
            Ok(item) => Next::Item(item),
            Err(RecvTimeoutError::Timeout) => {
                self.next_tick = Instant::now() + self.period;
                Next::Tick
            }
            Err(RecvTimeoutError::Disconnected) => Next::Closed,
        }
    }
}
