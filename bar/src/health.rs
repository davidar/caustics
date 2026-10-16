//! After-the-fact instrumentation: what the journal needs to carry so that a
//! bar that started hogging something can be debugged without having watched
//! it happen.
//!
//! Two lines, both witnesses, neither a decision:
//!
//! - **The stall witness.** The design's whole power story is that the bar
//!   sleeps between events (docs/architecture.md, *The frame*), and the one
//!   failure that silently breaks it is a `wants_redraw` that never clears —
//!   a spring, the dwell clock or the pointer's wake (`WAKE_SETTLE_S`) failing to settle — after
//!   which the bar renders at the compositor's rate and the journal shows a
//!   single `redraw: animating`. [`STALL_WARN`] after that line, if the bar is
//!   still animating, one `WARN` names everything that could be holding it
//!   awake; it repeats every [`STALL_REPEAT`] while the stall lasts.
//! - **The health line.** [`STARTUP_REPORT`] after a start, every
//!   [`REPORT_INTERVAL`] thereafter, and on every stall
//!   warning, one `INFO` with frames drawn since the last one, CPU seconds
//!   used since the last one, resident memory, the image slots and the pixmap store, the curve buffers, the live capture
//!   session and the popup state. One line an hour is nothing to read and
//!   everything to diff.
//!
//! The process numbers come from `/proc/self`, which is what `top` reads;
//! nothing here is sampled between reports.

use std::time::{Duration, Instant};

/// How long `wants_redraw` may stay set before the bar says what it is
/// animating. Ten seconds is far longer than any spring, dwell or fade here
/// (the longest, a hover spring, settles in about 0.6 s).
pub const STALL_WARN: Duration = Duration::from_secs(10);
/// How often the warning repeats while the stall persists.
pub const STALL_REPEAT: Duration = Duration::from_secs(60);
/// How often the health line is logged unprompted.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(3600);
/// How long after a start the **first** health line is written. Long enough
/// that every sidecar has had its first second and its first backoff, short
/// enough that `bar/install.sh` can print the line rather than tell the user
/// to go and look for it — which is the point:
/// one line naming every binary of the set that just went in, and what
/// happened to each.
pub const STARTUP_REPORT: Duration = Duration::from_secs(10);

/// The bookkeeping behind both lines.
pub struct Health {
    /// When the current animating stretch began, or `None` while settled.
    pub animating_since: Option<Instant>,
    /// When the stall warning last fired for the current stretch.
    pub last_stall_warn: Option<Instant>,
    pub last_report: Instant,
    pub frames_at_report: u64,
    pub cpu_at_report: f64,
}

impl Health {
    pub fn new() -> Self {
        Self {
            animating_since: None,
            last_stall_warn: None,
            last_report: Instant::now(),
            frames_at_report: 0,
            cpu_at_report: cpu_seconds(),
        }
    }

    /// Record this frame's `wants_redraw`. Returns `Some(stretch)` when a
    /// stall warning is due — the caller logs what it knows and asks the
    /// caustic for the rest with a `CMD_DUMP_STALL` — and `None` otherwise.
    pub fn observe_redraw(&mut self, animating: bool, now: Instant) -> Option<Duration> {
        if !animating {
            self.animating_since = None;
            self.last_stall_warn = None;
            return None;
        }
        let since = *self.animating_since.get_or_insert(now);
        let stretch = now.duration_since(since);
        if stretch < STALL_WARN {
            return None;
        }
        let due = match self.last_stall_warn {
            None => true,
            Some(last) => now.duration_since(last) >= STALL_REPEAT,
        };
        if !due {
            return None;
        }
        self.last_stall_warn = Some(now);
        Some(stretch)
    }

    /// The deltas since the last report, and reset them. Returns
    /// `(seconds, frames, cpu_seconds)` for the interval.
    pub fn take_interval(&mut self, frames: u64, now: Instant) -> (f64, u64, f64) {
        let cpu = cpu_seconds();
        let secs = now.duration_since(self.last_report).as_secs_f64();
        let d_frames = frames.saturating_sub(self.frames_at_report);
        let d_cpu = (cpu - self.cpu_at_report).max(0.0);
        self.last_report = now;
        self.frames_at_report = frames;
        self.cpu_at_report = cpu;
        (secs, d_frames, d_cpu)
    }
}

/// User + system CPU time of this process, in seconds, from `/proc/self/stat`.
/// Zero if `/proc` is unreadable: the numbers are a witness, never a reason
/// to fail.
pub fn cpu_seconds() -> f64 {
    proc_stat("/proc/self/stat")
        .map(|(cpu, _)| cpu)
        .unwrap_or(0.0)
}

/// What one `/proc/<pid>/stat` says a process has cost: CPU seconds and
/// resident bytes. `None` when the file is not there — which for the sidecar
/// is the honest answer between a death and a restart.
pub fn proc_cost(pid: u32) -> Option<(f64, u64)> {
    proc_stat(&format!("/proc/{pid}/stat"))
}

/// `(cpu seconds, rss bytes)` out of one `stat` file.
fn proc_stat(path: &str) -> Option<(f64, u64)> {
    let stat = std::fs::read_to_string(path).ok()?;
    // The command name is in parentheses and may contain spaces; fields are
    // counted from after the closing one.
    let (_, rest) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the ')' the fields are numbered from 3 (state); utime is field 14
    // and stime 15, so indices 11 and 12 here, and `rss` (in pages) is field
    // 24, index 21.
    let field = |i: usize| {
        fields
            .get(i)
            .and_then(|f| f.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    let ticks = field(11) + field(12);
    // SAFETY: sysconf is a plain query with no pointer arguments.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = if hz > 0 { hz as f64 } else { 100.0 };
    // SAFETY: as above.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = if page > 0 { page as u64 } else { 4096 };
    Some((ticks / hz, field(21) as u64 * page))
}

/// Resident set size of this process in bytes, from `/proc/self/statm`.
pub fn rss_bytes() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let pages = statm
        .split_whitespace()
        .nth(1)
        .and_then(|f| f.parse::<u64>().ok())
        .unwrap_or(0);
    // SAFETY: sysconf is a plain query with no pointer arguments.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    pages * if page > 0 { page as u64 } else { 4096 }
}

#[cfg(test)]
mod tests;
