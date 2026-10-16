//! `/proc/net/dev`: received and transmitted bytes per second.
//!
//! The bare `net` chart is the sum over every interface but `lo` — that
//! arithmetic is the sidecar's, because "all interfaces" is a sampling
//! question, not a drawing one. An interface that appears mid-run joins the
//! sum; one that vanishes leaves it, and the tick where the sum shrinks is
//! clamped to zero rather than reported as a negative rate.

use crate::GAP;

/// The file this source reads.
pub const PATH: &str = "/proc/net/dev";

/// One interface's byte counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counters {
    pub name: String,
    pub rx: u64,
    pub tx: u64,
}

/// Parse the whole table. The first two lines are headers.
pub fn parse(text: &str) -> Vec<Counters> {
    let mut out = Vec::new();
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
        // Receive: bytes packets errs drop fifo frame compressed multicast,
        // then Transmit: bytes … — so rx is 0 and tx is 8.
        if fields.len() < 9 {
            continue;
        }
        let (Ok(rx), Ok(tx)) = (fields[0].parse::<u64>(), fields[8].parse::<u64>()) else {
            continue;
        };
        out.push(Counters {
            name: name.trim().to_owned(),
            rx,
            tx,
        });
    }
    out
}

/// Sum the counters this chart covers: one named interface, or everything but
/// the loopback.
pub fn total(counters: &[Counters], iface: Option<&str>) -> Option<(u64, u64)> {
    match iface {
        Some(want) => counters
            .iter()
            .find(|c| c.name == want)
            .map(|c| (c.rx, c.tx)),
        None => {
            let mut sum = (0u64, 0u64);
            for c in counters.iter().filter(|c| c.name != "lo") {
                sum.0 = sum.0.saturating_add(c.rx);
                sum.1 = sum.1.saturating_add(c.tx);
            }
            Some(sum)
        }
    }
}

/// The sampler: which interface (or all), and the previous totals.
#[derive(Debug)]
pub struct Net {
    iface: Option<String>,
    prev: Option<(u64, u64)>,
}

impl Net {
    pub fn new(iface: Option<String>) -> Self {
        Self { iface, prev: None }
    }

    /// One tick from the live file. `dt` is the seconds since the last tick.
    pub fn sample(&mut self, dt: f64) -> [f32; 2] {
        match std::fs::read_to_string(PATH) {
            Ok(text) => self.sample_text(&text, dt),
            Err(_) => {
                self.prev = None;
                [GAP, GAP]
            }
        }
    }

    /// One tick from a snapshot. The first is a gap: a rate needs two.
    pub fn sample_text(&mut self, text: &str, dt: f64) -> [f32; 2] {
        let Some(now) = total(&parse(text), self.iface.as_deref()) else {
            self.prev = None;
            return [GAP, GAP];
        };
        let out = match self.prev {
            Some(prev) if dt > 0.0 => [rate(prev.0, now.0, dt), rate(prev.1, now.1, dt)],
            _ => [GAP, GAP],
        };
        self.prev = Some(now);
        out
    }
}

/// Bytes per second between two counter readings, clamped at zero.
pub(crate) fn rate(prev: u64, now: u64, dt: f64) -> f32 {
    (now.saturating_sub(prev) as f64 / dt) as f32
}
