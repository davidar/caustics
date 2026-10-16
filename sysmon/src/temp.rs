//! hwmon: the CPU package temperature, in °C.
//!
//! Discovery runs once at start and remembers one `temp*_input` path. The
//! preference order: AMD's `k10temp`/`Tctl` first,
//! then Intel's `coretemp`/`Package id 0`, then
//! `zenpower`, then the ACPI thermal zone as a last resort.

use std::path::{Path, PathBuf};

use crate::GAP;

/// Where the hwmon class lives.
pub const ROOT: &str = "/sys/class/hwmon";

/// The chosen sensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The `temp*_input` file, read every tick.
    pub input: PathBuf,
    /// `hwmon5/k10temp/Tctl` — the discovery witness.
    pub witness: String,
}

/// Each driver's preference, lowest first, and which label to look for.
fn rank(name: &str) -> Option<(u32, Option<&'static str>)> {
    match name {
        "k10temp" => Some((0, Some("Tctl"))),
        "coretemp" => Some((1, Some("Package id 0"))),
        "zenpower" => Some((2, Some("Tdie"))),
        "acpitz" => Some((3, None)),
        _ => None,
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Pick a sensor under `root`. `None` when none of the four drivers is there.
pub fn discover(root: &Path) -> Option<Source> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("hwmon"))
        })
        .collect();
    // `hwmon10` sorts before `hwmon2` this way, which is fine: the rank
    // decides, and a stable order only matters for reproducibility.
    dirs.sort();

    let mut best: Option<(u32, Source)> = None;
    for dir in dirs {
        let Some(name) = read_trimmed(&dir.join("name")) else {
            continue;
        };
        let Some((rank, want_label)) = rank(&name) else {
            continue;
        };
        if best.as_ref().is_some_and(|(r, _)| *r <= rank) {
            continue;
        }
        let Some((input, label)) = pick_input(&dir, want_label) else {
            continue;
        };
        let dirname = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("hwmon")
            .to_owned();
        let witness = format!("{dirname}/{name}/{label}");
        best = Some((rank, Source { input, witness }));
    }
    best.map(|(_, source)| source)
}

/// The `temp*_input` in `dir` whose label matches, else `temp1_input`.
fn pick_input(dir: &Path, want_label: Option<&str>) -> Option<(PathBuf, String)> {
    if let Some(want) = want_label {
        for n in 1..=32u32 {
            let label = dir.join(format!("temp{n}_label"));
            if read_trimmed(&label).as_deref() != Some(want) {
                continue;
            }
            let input = dir.join(format!("temp{n}_input"));
            if input.exists() {
                return Some((input, want.to_owned()));
            }
        }
    }
    let input = dir.join("temp1_input");
    if input.exists() {
        let label = read_trimmed(&dir.join("temp1_label")).unwrap_or_else(|| "temp1".into());
        return Some((input, label));
    }
    None
}

/// One tick: millidegrees on disk, °C on the wire.
pub fn read(input: &Path) -> f32 {
    match read_trimmed(input).and_then(|s| s.parse::<i64>().ok()) {
        Some(milli) => milli as f32 / 1000.0,
        None => GAP,
    }
}
