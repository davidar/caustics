//! `/proc/meminfo`: `(MemTotal − MemAvailable) / MemTotal`.

use crate::GAP;

/// The file this source reads.
pub const PATH: &str = "/proc/meminfo";

/// The used fraction from a snapshot, or a gap when either field is missing.
///
/// `MemAvailable` (not `MemFree`) is the kernel's own estimate of what a new
/// allocation could have, which is what every panel means by "used".
pub fn parse(text: &str) -> f32 {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb) = rest
            .split_ascii_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        else {
            continue;
        };
        match key {
            "MemTotal" => total = Some(kb),
            "MemAvailable" => available = Some(kb),
            _ => {}
        }
        if total.is_some() && available.is_some() {
            break;
        }
    }
    match (total, available) {
        (Some(total), Some(available)) if total > 0 => {
            ((total.saturating_sub(available)) as f32 / total as f32).clamp(0.0, 1.0)
        }
        _ => GAP,
    }
}

/// One tick from the live file.
pub fn sample() -> f32 {
    match std::fs::read_to_string(PATH) {
        Ok(text) => parse(&text),
        Err(_) => GAP,
    }
}
