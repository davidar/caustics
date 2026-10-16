//! The mtime poller under the live-reload loop (caustics-reload).
//!
//! A caustic's dev loop is: save the file, see the pixels change. What makes
//! that work is a timer that a few times a second asks whether any watched
//! file's mtime moved, and a compile that keeps the old-good module when the
//! new one does not build. This is the first half — deliberately without the
//! timer, because the timer is where the two crates differ: the compositor
//! gets calloop through smithay's re-export and the bar through
//! smithay-client-toolkit's, at two different versions
//! (`Cargo.lock`: 0.13.0 and 0.14.4). Three lines of `Timer::from_duration`
//! each is cheaper than making this crate pick a calloop.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How often the caustics directory is polled. Human save reflexes are
/// hundreds of milliseconds; this is comfortably inside one.
pub const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// One file's mtime, or `None` if it is missing or unreadable.
fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// A set of files and the mtimes last seen on them.
///
/// Primed at construction with the state on disk: whatever is running when the
/// watch is created was built from these very files, so there is nothing to
/// reload yet.
pub struct MtimeWatch {
    paths: Vec<PathBuf>,
    last: Vec<Option<SystemTime>>,
}

impl MtimeWatch {
    /// Watch `paths`, recording their mtimes now.
    pub fn new(paths: Vec<PathBuf>) -> Self {
        let last = paths.iter().map(|path| mtime(path)).collect();
        Self { paths, last }
    }

    /// The files being watched, in the order they were given. The first is by
    /// convention the caustic itself and the rest the modules it imports, so
    /// callers reach for `paths()[0]` when they need something to compile.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Has any watched file changed since the last call?
    ///
    /// A file that has *gone away* does not count: `None` is what a
    /// rename-in-place save looks like from here (the editor writes a temp
    /// file and renames it over the target, and a poll landing in that window
    /// sees nothing at all), and compiling against a missing file would turn
    /// every save into a spurious diagnostic. The observation is still
    /// recorded, so the file coming back reads as a change.
    pub fn changed(&mut self) -> bool {
        let current: Vec<Option<SystemTime>> = self.paths.iter().map(|path| mtime(path)).collect();
        let changed = current
            .iter()
            .zip(self.last.iter())
            .any(|(now, before)| now != before && now.is_some());
        self.last = current;
        changed
    }
}

#[cfg(test)]
mod tests;
