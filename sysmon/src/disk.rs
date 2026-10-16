//! `/proc/diskstats`: bytes read and written per second, whole disks only.
//!
//! Partitions are skipped because they double-count their disk, and the
//! virtual block devices (`zram*`, `loop*`, `dm-*`, `sr*`, `ram*`, `md*`) are skipped because
//! they are not I/O to a disk. Whole-disk detection is done on the table
//! itself — a name is a partition when another row's name is it minus a
//! trailing digit run (and an optional `p`) — so a recorded snapshot tests the
//! same code the live file does.

use crate::GAP;

/// The file this source reads.
pub const PATH: &str = "/proc/diskstats";
/// The kernel's stat unit, in bytes, regardless of the disk's own sector size.
pub const SECTOR: u64 = 512;

/// One device's sector counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counters {
    pub name: String,
    pub read: u64,
    pub written: u64,
}

/// Parse the whole table, in file order.
pub fn parse(text: &str) -> Vec<Counters> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        // major minor name reads merged sectors_read ms writes merged sectors_written …
        if f.len() < 10 {
            continue;
        }
        let (Ok(read), Ok(written)) = (f[5].parse::<u64>(), f[9].parse::<u64>()) else {
            continue;
        };
        out.push(Counters {
            name: f[2].to_owned(),
            read,
            written,
        });
    }
    out
}

/// The virtual block devices that are never a disk.
fn is_virtual(name: &str) -> bool {
    ["zram", "loop", "dm-", "sr", "ram", "md"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Whether `name` is a partition of some other row in `all`.
fn is_partition(name: &str, all: &[Counters]) -> bool {
    let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() == name.len() {
        return false;
    }
    // `nvme0n1p6` -> `nvme0n1p` -> `nvme0n1`; `sda1` -> `sda`; `nvme0n1`
    // -> `nvme0n`, which is nobody's name, so it is a whole disk.
    let parent = stem.strip_suffix('p').unwrap_or(stem);
    all.iter().any(|c| c.name == parent)
}

/// Sum the counters this chart covers: one named disk, or every whole disk.
pub fn total(counters: &[Counters], disk: Option<&str>) -> Option<(u64, u64)> {
    match disk {
        Some(want) => counters
            .iter()
            .find(|c| c.name == want)
            .map(|c| (c.read, c.written)),
        None => {
            let mut sum = (0u64, 0u64);
            for c in counters
                .iter()
                .filter(|c| !is_virtual(&c.name) && !is_partition(&c.name, counters))
            {
                sum.0 = sum.0.saturating_add(c.read);
                sum.1 = sum.1.saturating_add(c.written);
            }
            Some(sum)
        }
    }
}

/// The sampler: which disk (or all), and the previous totals.
#[derive(Debug)]
pub struct Disk {
    disk: Option<String>,
    prev: Option<(u64, u64)>,
}

impl Disk {
    pub fn new(disk: Option<String>) -> Self {
        Self { disk, prev: None }
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
        let Some(now) = total(&parse(text), self.disk.as_deref()) else {
            self.prev = None;
            return [GAP, GAP];
        };
        let out = match self.prev {
            Some(prev) if dt > 0.0 => [
                crate::net::rate(prev.0, now.0, dt) * SECTOR as f32,
                crate::net::rate(prev.1, now.1, dt) * SECTOR as f32,
            ],
            _ => [GAP, GAP],
        };
        self.prev = Some(now);
        out
    }
}
