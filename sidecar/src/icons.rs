//! The icon-theme search, once (feature `icons`).
//!
//! Three sidecars ask "which file is this icon name": `caustics-glyphs` (is it a
//! symbolic SVG it can turn into curves), `caustics-apps` (is it pixels it can
//! rasterise) and `caustics-tray` (does any theme have it at all, or should the
//! pixels the item sent beside the name be used instead). One search, shared,
//! is what keeps the three agreeing — and keeps the tray from preferring a
//! name no theme has over the pixels sent beside it.
//!
//! What is here:
//!
//! - [`resolve`]: a name to a file and the theme it came from, in
//!   [`theme_order`]. An **absolute path** is its own answer when the file is
//!   there — a desktop entry's `Icon=` may be one, and the tray sends one for
//!   an SVG it found in an item's own directory — and any other name with a
//!   `/` in it is no name at all.
//! - [`theme_order`]: the desktop's theme (GTK's setting), then
//!   [`FALLBACK_THEMES`]. `gsettings` is a process spawn, so the answer is
//!   kept for [`THEME_ORDER_TTL`]: long enough that a menu of forty rows asks
//!   once, short enough that a theme change is followed without a restart.
//! - [`is_symbolic`]: whether a resolved file is curves (`caustics-glyphs`') or
//!   pixels (`caustics-apps`').
//! - [`in_directory`]: a name in **one directory the caller names** — an
//!   item's `IconThemePath`, flat (Steam's) or laid out as a theme
//!   (`hicolor/48x48/apps/…`) — which `freedesktop-icons` cannot be pointed
//!   at.
//! - [`Misses`]: names no theme had, held until the [`stamp`] of the theme
//!   directories changes.
//!
//! What is not: `freedesktop-icons` reads the list of installed themes once
//! per process (a `Lazy` static), so a theme *installed* while a sidecar runs
//! is not searched until it restarts. A file added to a theme that was there
//! at start is found, since every lookup reads the directories. `freedesktop-icons`
//! caches no miss; this module's [`Misses`] does, and forgets them all when
//! any theme directory's mtime moves.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tried in order after the desktop's own theme. `hicolor` is last because it
/// is the spec's fallback of last resort and its icons are the plainest.
pub const FALLBACK_THEMES: &[&str] = &["Adwaita", "Cosmic", "Papirus", "hicolor"];

/// How long one `gsettings` answer is reused by [`theme_order`].
pub const THEME_ORDER_TTL: Duration = Duration::from_secs(10);

/// How deep [`in_directory`] looks under the directory it is given:
/// `<theme>/<size>/<context>/<name>.png` is three levels.
pub const OWN_DIR_DEPTH: usize = 3;

/// Directory entries [`in_directory`] reads before it stops: an item's
/// `IconThemePath` is the item's own choice, and `/` is a path too.
pub const OWN_DIR_ENTRIES: usize = 4096;

/// How long one walk of the theme roots is trusted by [`Misses`]: a walk is tens of milliseconds, so the names one sidecar re-checks in one pass share one.
pub const STAMP_TTL: Duration = Duration::from_secs(10);

/// How deep [`stamp`] goes under a theme root, and how many directories it
/// reads before it stops: a root is the machine's, and a symlink may loop.
pub const STAMP_DEPTH: usize = 8;
pub const STAMP_DIRS: usize = 16384;

/// Names [`Misses`] holds before it forgets them all: a name is whatever a
/// tray item or a desktop entry says.
pub const MISSES_HELD: usize = 4096;

static ORDER: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);

static MISSES: Mutex<Option<Misses>> = Mutex::new(None);

/// The names no theme had, for as long as the themes are what they were.
///
/// "What they were" is [`stamp`]: every directory under the theme roots and
/// its mtime, with the theme order. A file added to a theme moves the mtime
/// of the directory it lands in, so the stamp moves and every miss is
/// forgotten; nothing is held past a change, and nothing is searched twice
/// without one.
#[derive(Debug)]
pub struct Misses {
    roots: Vec<PathBuf>,
    ttl: Duration,
    stamp: Option<(Instant, u64)>,
    names: HashSet<String>,
}

impl Misses {
    /// Misses against `roots`, one walk of which is trusted for `ttl`.
    pub fn new(roots: Vec<PathBuf>, ttl: Duration) -> Misses {
        Misses {
            roots,
            ttl,
            stamp: None,
            names: HashSet::new(),
        }
    }

    /// Whether `name` is known to be in no theme of `order`, the themes being
    /// what they were when it was last searched for.
    pub fn holds(&mut self, name: &str, order: &[String]) -> bool {
        if !self.stamp.is_some_and(|(at, _)| at.elapsed() < self.ttl) {
            let now = stamp(&self.roots, order);
            if self.stamp.is_some_and(|(_, was)| was != now) {
                self.names.clear();
            }
            self.stamp = Some((Instant::now(), now));
        }
        self.names.contains(name)
    }

    /// A search for `name` found nothing.
    pub fn remember(&mut self, name: &str) {
        if self.names.len() >= MISSES_HELD {
            self.names.clear();
        }
        self.names.insert(name.to_owned());
    }
}

/// Where icon themes live, as `freedesktop-icons` reads it: `icons` and
/// `pixmaps` under every `$XDG_DATA_DIRS` entry and `$XDG_DATA_HOME`, and
/// `~/.icons`.
pub fn theme_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    let mut roots: Vec<PathBuf> = Vec::new();
    for dir in data_dirs.split(':').map(PathBuf::from).chain(data_home) {
        roots.push(dir.join("icons"));
        roots.push(dir.join("pixmaps"));
    }
    roots.extend(home.map(|home| home.join(".icons")));
    roots
}

/// The themes as they are now, in one word: `order`, and every directory
/// under `roots` (symlinks followed, to [`STAMP_DEPTH`] and [`STAMP_DIRS`])
/// with its mtime. Directory listings and one `stat` a directory; no file is
/// opened.
pub fn stamp(roots: &[PathBuf], order: &[String]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    order.hash(&mut hasher);
    let mut budget = STAMP_DIRS;
    let mut level: Vec<PathBuf> = roots.to_vec();
    for _ in 0..=STAMP_DEPTH {
        let mut next: Vec<PathBuf> = Vec::new();
        for here in &level {
            if budget == 0 {
                break;
            }
            // A root that is not there yet is part of the stamp too: it
            // hashes as absent, and as present once it is made.
            let modified = std::fs::metadata(here)
                .and_then(|meta| meta.modified())
                .ok();
            (here, modified).hash(&mut hasher);
            if modified.is_none() {
                continue;
            }
            budget -= 1;
            let Ok(entries) = std::fs::read_dir(here) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() || (kind.is_symlink() && path.is_dir()) {
                    next.push(path);
                }
            }
        }
        if next.is_empty() || budget == 0 {
            break;
        }
        next.sort();
        level = next;
    }
    hasher.finish()
}

/// Resolve `name` in the icon themes: the file, and the theme it came from
/// (`"path"` for an absolute path that was its own answer).
///
/// `px` steers the search (a theme may hold several sizes) and does not bound
/// the answer: a scalable SVG is one file whatever size is asked for.
pub fn resolve(name: &str, px: u32) -> Option<(PathBuf, String)> {
    if name.is_empty() {
        return None;
    }
    let as_path = Path::new(name);
    if as_path.is_absolute() {
        return as_path
            .is_file()
            .then(|| (as_path.to_path_buf(), "path".to_owned()));
    }
    if name.contains('/') {
        return None;
    }
    let size = px.clamp(1, u16::MAX as u32) as u16;
    let order = theme_order();
    if misses(|held| held.holds(name, &order)) {
        return None;
    }
    for theme in order {
        if let Some(path) = freedesktop_icons::lookup(name)
            .with_size(size)
            .with_theme(&theme)
            .with_cache()
            .find()
        {
            return Some((path, theme));
        }
    }
    misses(|held| held.remember(name));
    None
}

/// The process's [`Misses`], against the machine's [`theme_roots`].
fn misses<T>(ask: impl FnOnce(&mut Misses) -> T) -> T {
    let mut held = MISSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ask(held.get_or_insert_with(|| Misses::new(theme_roots(), STAMP_TTL)))
}

/// The desktop's configured theme (if GTK settings name one) followed by
/// [`FALLBACK_THEMES`], with duplicates removed. Reused for
/// [`THEME_ORDER_TTL`].
pub fn theme_order() -> Vec<String> {
    let mut held = ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((at, order)) = held.as_ref()
        && at.elapsed() < THEME_ORDER_TTL
    {
        return order.clone();
    }
    let order = order_after(freedesktop_icons::default_theme_gtk());
    *held = Some((Instant::now(), order.clone()));
    order
}

/// The order for a given desktop theme: it, then the fallbacks not already
/// named.
pub fn order_after(current: Option<String>) -> Vec<String> {
    let mut themes: Vec<String> = Vec::with_capacity(FALLBACK_THEMES.len() + 1);
    if let Some(current) = current {
        themes.push(current);
    }
    for theme in FALLBACK_THEMES {
        if !themes.iter().any(|have| have == theme) {
            themes.push((*theme).to_string());
        }
    }
    themes
}

/// Whether a resolved file is one `caustics-glyphs` takes as curves: an SVG that a
/// theme calls symbolic.
///
/// The `-symbolic` convention: a symbolic icon is a single-colour outline,
/// which is exactly what a glyph is. Anything else — Firefox's gradients, a
/// PNG — is pixels and `caustics-apps`'.
pub fn is_symbolic(name: &str, path: &Path) -> bool {
    let svg = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
    let stem_symbolic = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.ends_with("-symbolic"));
    let symbolic = name.ends_with("-symbolic")
        || stem_symbolic
        || path
            .components()
            .any(|c| c.as_os_str().to_string_lossy() == "symbolic");
    svg && symbolic
}

/// What [`in_directory`] found: the best PNG and the best SVG under one
/// directory, either or both.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnIcon {
    pub png: Option<PathBuf>,
    pub svg: Option<PathBuf>,
}

impl OwnIcon {
    pub fn is_empty(&self) -> bool {
        self.png.is_none() && self.svg.is_none()
    }
}

/// `name` in one directory an item named as its own: `<dir>/<name>.png` (the
/// flat layout libappindicator's `IconThemePath` has, Steam's), or anywhere
/// down to [`OWN_DIR_DEPTH`] levels — the theme-shaped layout
/// (`<dir>/hicolor/48x48/apps/<name>.png`) KDE and Electron items ship.
///
/// The PNG kept is the one under the largest `NxN` directory (a flat one
/// counts as size 0, so a sized one beats it); the SVG, the first found.
/// Nothing is read but directory listings, and at most [`OWN_DIR_ENTRIES`] of
/// them. A name with a `/` in it is never looked for: a name is not a path.
pub fn in_directory(dir: &Path, name: &str) -> OwnIcon {
    let mut found = OwnIcon::default();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return found;
    }
    let png_name = format!("{name}.png");
    let svg_name = format!("{name}.svg");
    let mut best_png: Option<(u32, PathBuf)> = None;
    let mut budget = OWN_DIR_ENTRIES;
    // Breadth first, so a flat file is found before a deep one and the walk
    // spends its budget near the top.
    let mut level: Vec<PathBuf> = vec![dir.to_path_buf()];
    for depth in 0..=OWN_DIR_DEPTH {
        let mut next: Vec<PathBuf> = Vec::new();
        for here in &level {
            let Ok(entries) = std::fs::read_dir(here) else {
                continue;
            };
            for entry in entries.flatten() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if depth < OWN_DIR_DEPTH {
                        next.push(path);
                    }
                    continue;
                }
                // A symlink to a file counts: themes are full of them.
                if !(kind.is_file() || (kind.is_symlink() && path.is_file())) {
                    continue;
                }
                let file = entry.file_name();
                if file == png_name.as_str() {
                    let size = size_of_path(&path, dir);
                    if best_png.as_ref().is_none_or(|(have, _)| size > *have) {
                        best_png = Some((size, path));
                    }
                } else if file == svg_name.as_str() && found.svg.is_none() {
                    found.svg = Some(path);
                }
            }
        }
        if budget == 0 {
            break;
        }
        next.sort();
        level = next;
    }
    found.png = best_png.map(|(_, path)| path);
    found
}

/// The pixel size a theme-shaped path says it is: the largest `NxN` (or
/// `NxN@S`, times `S`) component between `dir` and the file, or 0.
fn size_of_path(path: &Path, dir: &Path) -> u32 {
    let rest = path.strip_prefix(dir).unwrap_or(path);
    rest.components()
        .filter_map(|c| {
            let text = c.as_os_str().to_str()?;
            let (dims, scale) = match text.split_once('@') {
                Some((dims, scale)) => (dims, scale.trim_end_matches('x').parse::<u32>().ok()?),
                None => (text, 1),
            };
            let (w, h) = dims.split_once('x')?;
            let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
            Some(w.max(h).saturating_mul(scale))
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
