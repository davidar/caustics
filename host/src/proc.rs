//! What `/proc` says a process costs: the numbers a host's health line
//! reports, for itself and for each sidecar by pid.
//!
//! Witnesses, never decisions: every reader here answers zero or `None` when
//! the file is not there, and nothing fails because of it. When to report and
//! what to say about the numbers is the host's own (the bar's `health.rs`).

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
