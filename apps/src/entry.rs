//! Desktop entries: an app id → its icon name, display name, and how to launch
//! it.
//!
//! A parser over files the bar has no other reason to open, which is the whole
//! argument for it being here: the bar copies bytes off a pipe and this
//! process does the reading.

use std::path::{Path, PathBuf};

/// The three fields of a `.desktop` file the dock cares about, plus where it
/// was found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    pub path: PathBuf,
    /// `Name=` — the dock entry's label and the preview popup's fallback
    /// title.
    pub name: Option<String>,
    /// `Icon=`: a theme name for [`crate::icon::resolve`], or an absolute path.
    pub icon: Option<String>,
    /// `Exec=`, verbatim; [`DesktopEntry::command`] strips the field codes.
    pub exec: Option<String>,
}

impl DesktopEntry {
    /// `Exec=` as an argv, with the desktop-entry field codes removed.
    ///
    /// `%f %F %u %U` are the file and URL arguments a launcher would
    /// substitute and the dock has none; `%i %c %k` are the icon, the
    /// translated name and the file's own path, which nothing the dock
    /// launches needs. `%%` is a literal percent. Splitting on whitespace
    /// rather than shell-quoting is deliberate: a `.desktop` `Exec=` is
    /// specified as a *word* list, not a shell command, and running it
    /// through a shell is how a launcher grows an injection bug.
    pub fn command(&self) -> Option<Vec<String>> {
        let exec = self.exec.as_deref()?;
        let argv: Vec<String> = exec
            .split_whitespace()
            .filter(|word| !matches!(*word, "%f" | "%F" | "%u" | "%U" | "%i" | "%c" | "%k"))
            .map(|word| word.replace("%%", "%"))
            .collect();
        (!argv.is_empty()).then_some(argv)
    }
}

/// Where `.desktop` files live: `$XDG_DATA_HOME/applications` first, then
/// every `$XDG_DATA_DIRS` entry, then the Flatpak exports both system and
/// user, which are not always in `XDG_DATA_DIRS` under a bare session.
pub fn application_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        Some(data) if data.is_absolute() => push(data.join("applications")),
        _ => {
            if let Some(home) = &home {
                push(home.join(".local/share/applications"));
            }
        }
    }
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for dir in data_dirs.split(':').filter(|d| !d.is_empty()) {
        push(PathBuf::from(dir).join("applications"));
    }
    push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if let Some(home) = &home {
        push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs
}

/// The desktop entry for an app id, searched the way every dock searches:
/// the exact file name first, then case-insensitively, then by
/// `StartupWMClass=`.
pub fn desktop_entry(app_id: &str) -> Option<DesktopEntry> {
    desktop_entry_in(&application_dirs(), app_id)
}

/// [`desktop_entry`] against an explicit set of directories, which is what
/// makes it testable against fixtures.
pub fn desktop_entry_in(dirs: &[PathBuf], app_id: &str) -> Option<DesktopEntry> {
    if app_id.is_empty() {
        return None;
    }
    // 1. `<app id>.desktop`, exactly. The common case, and the cheap one:
    //    one `open` per directory and no listing at all.
    for dir in dirs {
        let path = dir.join(format!("{app_id}.desktop"));
        if let Some(entry) = read_entry(&path) {
            return Some(entry);
        }
    }

    // 2 and 3 need the directory listing anyway, so they share one walk: a
    // case-insensitive file-name match beats a StartupWMClass match, but a
    // StartupWMClass match found in an earlier directory beats a
    // case-insensitive one found later — XDG precedence is by directory.
    let wanted = app_id.to_ascii_lowercase();
    // 4. A distrobox export: `distrobox-export` names
    //    the file `<container>-<app id>.desktop` and rewrites
    //    `StartupWMClass=` to the container's own path,
    //    `/usr/share/applications/<app id>.desktop` — so kdeconnect's window,
    //    `org.kde.kdeconnect.app`, exported from a container, has an entry that the
    //    first three rules cannot see. Reverse-DNS ids only: a bare `app`
    //    must not match every `foo-app.desktop`. Kept apart and tried last,
    //    after every directory's ordinary matches.
    let exported = wanted.contains('.').then(|| format!("-{wanted}"));
    let mut by_export: Option<DesktopEntry> = None;
    for dir in dirs {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut by_class: Option<DesktopEntry> = None;
        for file in read_dir.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let lower = stem.to_ascii_lowercase();
            if lower == wanted
                && let Some(entry) = read_entry(&path)
            {
                return Some(entry);
            }
            if by_class.is_some() && by_export.is_some() {
                continue;
            }
            let Some((entry, class)) = read_entry_with_class(&path) else {
                continue;
            };
            let class = class.map(|class| class.to_ascii_lowercase());
            if by_class.is_none() && class.as_deref() == Some(wanted.as_str()) {
                by_class = Some(entry);
                continue;
            }
            if by_export.is_none()
                && let Some(suffix) = &exported
                && (lower.ends_with(suffix.as_str())
                    || class.as_deref().is_some_and(|class| {
                        class.starts_with('/')
                            && Path::new(class).file_stem().and_then(|s| s.to_str())
                                == Some(wanted.as_str())
                    }))
            {
                by_export = Some(entry);
            }
        }
        if by_class.is_some() {
            return by_class;
        }
    }
    by_export
}

fn read_entry(path: &Path) -> Option<DesktopEntry> {
    read_entry_with_class(path).map(|(entry, _)| entry)
}

/// Parse the `[Desktop Entry]` group of a `.desktop` file. Only the group's
/// own keys count: an action group's `Name=` is a different application's.
fn read_entry_with_class(path: &Path) -> Option<(DesktopEntry, Option<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut entry = DesktopEntry {
        path: path.to_path_buf(),
        ..DesktopEntry::default()
    };
    let mut class = None;
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(group) = line.strip_prefix('[') {
            inside = group.trim_end_matches(']') == "Desktop Entry";
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        // Localised keys (`Name[de]=`) are somebody else's language; the
        // unlocalised one is the only one this reads.
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_owned();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "Name" => entry.name = Some(value),
            "Icon" => entry.icon = Some(value),
            "Exec" => entry.exec = Some(value),
            "StartupWMClass" => class = Some(value),
            _ => {}
        }
    }
    // A file with none of the three is not an answer, it is a stray file.
    (entry.name.is_some() || entry.icon.is_some() || entry.exec.is_some()).then_some((entry, class))
}

#[cfg(test)]
mod tests;
