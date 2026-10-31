//! The dock's pinned app ids, on disk: `$XDG_STATE_HOME/caustics/bar/favourites`.
//!
//! **State, not config** (docs/architecture.md, *Configuration is the caustic*): the
//! dock's favourites are state the dock mutates with its Pin and Unpin rows,
//! not configuration a person edits, so they get a file of their own and the
//! bar still has no config file. This is that file: one app id per line, in
//! dock order, written by the dock and read once.
//!
//! **No seed.** A machine with no file has no favourites: the bar cannot know
//! which applications are installed on it or wanted, and a compiled-in list
//! would be entries that launch nothing wherever it guessed wrong. The dock
//! shows the running windows, and the right-click **Pin** row is how the
//! first favourite arrives; that first pin writes the file, and after that
//! the file is the truth.
//!
//! **Read once, at startup.** The bar does not watch it. A person editing it
//! under a running bar gets it on the next start, and that is the contract —
//! the alternative is two writers for one list.
//!
//! The path is a parameter everywhere below, resolved once in `Bar::new`
//! through [`path`], so the tests pass a temp directory and never touch the
//! environment.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Where the file lives: `$XDG_STATE_HOME/caustics/bar/favourites`, and
/// `~/.local/state` when the variable is unset, per the XDG base-directory
/// spec. A relative `$XDG_STATE_HOME` is ignored, as the spec says it must be.
pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("caustics").join("bar").join("favourites")
}

/// The favourites, or none. Never fails: every way of not having a usable
/// file ends in an empty list and one witness line saying which way it was.
pub fn load(path: &Path) -> Vec<String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                "favourites: none yet ({} does not exist); Pin on a dock entry adds the first",
                path.display()
            );
            return Vec::new();
        }
        Err(err) => {
            tracing::warn!(
                "favourites: could not read {}: {err}; the dock starts with none",
                path.display()
            );
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        // A blank line, and the empty last line a trailing newline leaves.
        if line.trim().is_empty() {
            continue;
        }
        // An app id has no whitespace in it. A line that does is something
        // else — a comment, a pasted path, a hand-edit gone wrong — and
        // guessing at it would put a dock entry on screen that launches
        // nothing.
        if line.chars().any(char::is_whitespace) {
            tracing::info!(
                "favourites: {}:{} is not an app id ({line:?}); skipped",
                path.display(),
                index + 1
            );
            continue;
        }
        out.push(line.to_owned());
    }
    tracing::info!("favourites: {} read from {}", out.len(), path.display());
    out
}

/// Write the list. A temp file beside it and a rename, so a crash mid-write
/// leaves the old file whole rather than a truncated one.
pub fn store(path: &Path, favourites: &[String]) {
    let Some(dir) = path.parent() else {
        tracing::warn!(
            "favourites: {} has no directory to write in",
            path.display()
        );
        return;
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::warn!("favourites: could not create {}: {err}", dir.display());
        return;
    }
    let temp = path.with_extension("new");
    let written = std::fs::File::create(&temp).and_then(|mut file| {
        for app_id in favourites {
            writeln!(file, "{app_id}")?;
        }
        file.sync_all()
    });
    if let Err(err) = written {
        tracing::warn!("favourites: could not write {}: {err}", temp.display());
        let _ = std::fs::remove_file(&temp);
        return;
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        tracing::warn!(
            "favourites: could not rename {} into place: {err}",
            temp.display()
        );
        let _ = std::fs::remove_file(&temp);
        return;
    }
    tracing::info!(
        "favourites: wrote {} to {}",
        favourites.len(),
        path.display()
    );
}

#[cfg(test)]
mod tests;
