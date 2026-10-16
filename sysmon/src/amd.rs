//! The AMD half of the `gpu` chart: `gpu_busy_percent` in drm sysfs.
//!
//! amdgpu publishes its own busy percentage, so there is nothing to open: a
//! `read` of one file per tick. The one guard is `power/runtime_status`: a
//! card that is suspended (a discrete GPU at rest) samples as a gap rather
//! than being woken by the read; an integrated GPU reports `active` or
//! `unsupported` and is read every tick.

use std::path::{Path, PathBuf};

use crate::GAP;

/// Where the drm class lives.
pub const ROOT: &str = "/sys/class/drm";
/// PCI vendor id of AMD/ATI.
pub const VENDOR: &str = "0x1002";

/// A discovered AMD device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Amd {
    /// The `gpu_busy_percent` file, read every tick.
    pub busy: PathBuf,
    /// The card's `power/runtime_status`, read before `busy`: `suspended` or
    /// `suspending` means a gap, since the read itself would wake the device.
    pub status: PathBuf,
    /// `amd:card0 = 0000:05:00.0` — the discovery witness.
    pub witness: String,
}

/// Every `cardN` (not a connector, not a render node) under `root`, sorted.
pub(crate) fn cards(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(dir) => dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.strip_prefix("card").is_some_and(|rest| {
                        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
                    })
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// The PCI vendor id of a card's device, trimmed.
pub(crate) fn vendor(card: &Path) -> Option<String> {
    std::fs::read_to_string(card.join("device/vendor"))
        .ok()
        .map(|s| s.trim().to_owned())
}

/// The slot name the card's `device` symlink resolves to (`0000:05:00.0`).
pub(crate) fn slot(card: &Path) -> String {
    std::fs::canonicalize(card.join("device"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "?".into())
}

/// The first AMD card with a readable `gpu_busy_percent`.
pub fn discover(root: &Path) -> Option<Amd> {
    for card in cards(root) {
        if vendor(&card).as_deref() != Some(VENDOR) {
            continue;
        }
        let busy = card.join("device/gpu_busy_percent");
        if !busy.exists() {
            continue;
        }
        let status = card.join("device/power/runtime_status");
        let name = card
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("card")
            .to_owned();
        let witness = format!("amd:{name} = {}", slot(&card));
        return Some(Amd {
            busy,
            status,
            witness,
        });
    }
    None
}

impl Amd {
    /// One tick: busy percent, 0..100, or a gap for a card that is asleep.
    pub fn sample(&self) -> f32 {
        if let Ok(status) = std::fs::read_to_string(&self.status) {
            if matches!(status.trim(), "suspended" | "suspending") {
                return GAP;
            }
        }
        match std::fs::read_to_string(&self.busy)
            .ok()
            .map(|s| s.trim().parse::<f32>())
        {
            Some(Ok(percent)) => percent.clamp(0.0, 100.0),
            _ => GAP,
        }
    }
}
