//! `/proc/stat`, first line: busy fraction over the tick, all CPUs.

use crate::GAP;

/// The file this source reads.
pub const PATH: &str = "/proc/stat";

/// The two sums the busy fraction needs, in jiffies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Times {
    /// Every field of the aggregate `cpu` line summed.
    pub total: u64,
    /// `idle + iowait`: what does not count as busy.
    pub idle: u64,
}

/// Parse the aggregate `cpu` line. `None` when the file does not start with
/// one, which is the only shape this cares about.
pub fn parse(text: &str) -> Option<Times> {
    let line = text.lines().next()?;
    let mut fields = line.split_ascii_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    // user nice system idle iowait irq softirq steal guest guest_nice — the
    // tail grows with kernel versions, so sum whatever is there.
    let values: Vec<u64> = fields.map(|f| f.parse().ok()).collect::<Option<_>>()?;
    if values.len() < 5 {
        return None;
    }
    Some(Times {
        total: values.iter().sum(),
        idle: values[3] + values[4],
    })
}

/// The sampler: one previous reading, and nothing else.
#[derive(Debug, Default)]
pub struct Cpu {
    prev: Option<Times>,
}

impl Cpu {
    /// One tick from the live file. Unreadable is a gap, not a failure.
    pub fn sample(&mut self) -> f32 {
        match std::fs::read_to_string(PATH) {
            Ok(text) => self.sample_text(&text),
            Err(_) => {
                self.prev = None;
                GAP
            }
        }
    }

    /// One tick from a snapshot. The first one is always a gap: a rate needs
    /// two readings.
    pub fn sample_text(&mut self, text: &str) -> f32 {
        let Some(now) = parse(text) else {
            self.prev = None;
            return GAP;
        };
        let value = match self.prev {
            // A counter that went backwards (a reboot cannot happen mid-run,
            // but a hot-unplugged CPU can shrink the sum) restarts the rate.
            Some(prev) if now.total > prev.total => {
                let total = now.total - prev.total;
                let idle = now.idle.saturating_sub(prev.idle).min(total);
                ((total - idle) as f32 / total as f32).clamp(0.0, 1.0)
            }
            _ => GAP,
        };
        self.prev = Some(now);
        value
    }
}
