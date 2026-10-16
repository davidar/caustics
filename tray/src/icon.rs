//! An icon's fallback chain.
//!
//! An SNI item and a dbusmenu row may each say an icon three ways at once: a
//! **name**, a **directory** the name lives in (the item's `IconThemePath`,
//! the menu's own `IconThemePath`), and **pixels** (`IconPixmap`,
//! `icon-data`). Letting a name win whenever there is one, whether or
//! not anything has it, draws nothing for kdeconnect, whose rows name Breeze
//! icons few installed themes have, with a PNG beside each name. The order, for
//! an item and for a row alike:
//!
//! 1. the name, if a theme has it — it follows the user's icon theme;
//! 2. the name in the item's own directories, flat or theme-shaped: a PNG is
//!    decoded here and crosses as pixels; an SVG crosses as **its absolute
//!    path**, which `caustics-glyphs` turns into curves when it is symbolic and
//!    `caustics-apps` rasterises otherwise (both take an absolute path as its own
//!    answer, `caustics_sidecar::icons::resolve`) — so this process stays without
//!    an SVG rasteriser and the file is still drawn;
//! 3. the pixels sent beside the name;
//! 4. the name anyway, so the bar's placeholder and the witness say what
//!    was lost.
//!
//! [`Lookup`] is the two inputs that are not the item's own — "does a theme
//! have this name" and the directories — so the tests drive the chain with a
//! fixture directory and a fake theme instead of the machine's.

use std::path::PathBuf;

use caustics_sidecar::icons;

use crate::item::WIRE_PIXMAP;
use crate::model::Icon;

/// What the chain is asked against.
#[derive(Clone, Debug)]
pub struct Lookup {
    /// Whether any icon theme has this name.
    pub themed: fn(&str) -> bool,
    /// The item's own directories, in the order they are tried.
    pub dirs: Vec<PathBuf>,
}

impl Lookup {
    /// The machine's themes, and `dirs`.
    pub fn live(dirs: Vec<PathBuf>) -> Lookup {
        Lookup {
            themed: in_a_theme,
            dirs,
        }
    }

    /// The item's `IconThemePath` (one string, possibly empty) and a menu's
    /// (a list), as one list of directories without repeats or blanks.
    pub fn dirs_of<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for path in paths {
            let path = PathBuf::from(path);
            if path.as_os_str().is_empty() || !path.is_absolute() || dirs.contains(&path) {
                continue;
            }
            dirs.push(path);
        }
        dirs
    }
}

/// [`Lookup::themed`] against the machine's icon themes.
pub fn in_a_theme(name: &str) -> bool {
    icons::resolve(name, WIRE_PIXMAP).is_some()
}

/// Which step of the chain an icon came from, for the witness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// No name and no pixels.
    Nothing,
    /// Step 1: a theme has the name.
    Theme,
    /// Step 2: a PNG in the item's own directory, decoded.
    OwnPng(PathBuf),
    /// Step 2: an SVG in the item's own directory, sent as its path.
    OwnSvg(PathBuf),
    /// Step 3: the pixels sent beside the name (or instead of one).
    Pixels,
    /// Step 4: nothing has the name; it is sent anyway.
    Lost,
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Nothing => "nothing".to_owned(),
            Source::Theme => "a theme".to_owned(),
            Source::OwnPng(path) => format!("its own {}", path.display()),
            Source::OwnSvg(path) => format!("its own {} (as a path)", path.display()),
            Source::Pixels => "its pixels".to_owned(),
            Source::Lost => "nothing has it".to_owned(),
        }
    }
}

/// Steps 1 and 2: the name, if a theme or the item's own directory has it.
pub fn by_name(lookup: &Lookup, name: &str) -> Option<(Icon, Source)> {
    if name.is_empty() {
        return None;
    }
    if (lookup.themed)(name) {
        return Some((
            Icon::Themed {
                name: name.to_owned(),
                theme_path: None,
            },
            Source::Theme,
        ));
    }
    for dir in &lookup.dirs {
        let found = icons::in_directory(dir, name);
        if let Some(png) = found.png
            && let Some((width, height, rgba)) = std::fs::read(&png)
                .ok()
                .and_then(|data| crate::menu::decode(&data))
        {
            return Some((
                Icon::Pixmap {
                    width,
                    height,
                    rgba,
                },
                Source::OwnPng(png),
            ));
        }
        if let Some(svg) = found.svg
            && let Some(path) = svg.to_str()
        {
            let icon = Icon::Themed {
                name: path.to_owned(),
                theme_path: None,
            };
            return Some((icon, Source::OwnSvg(svg)));
        }
    }
    None
}

/// The whole chain: [`by_name`], then `pixels`, then the name anyway.
///
/// `pixels` is what was sent beside the name, already decoded — an item's is
/// a bus call away, so [`crate::item`] calls [`by_name`] first and reads the
/// pixmap only when it misses.
pub fn choose(lookup: &Lookup, name: &str, pixels: Option<Icon>) -> (Icon, Source) {
    match by_name(lookup, name) {
        Some(found) => found,
        None => after_name(lookup, name, pixels),
    }
}

/// Steps 3 and 4, for a caller that has already had [`by_name`] miss.
pub fn after_name(lookup: &Lookup, name: &str, pixels: Option<Icon>) -> (Icon, Source) {
    if let Some(pixels) = pixels.filter(|icon| *icon != Icon::None) {
        return (pixels, Source::Pixels);
    }
    if name.is_empty() {
        return (Icon::None, Source::Nothing);
    }
    let theme_path = lookup.dirs.first().map(|dir| dir.display().to_string());
    (
        Icon::Themed {
            name: name.to_owned(),
            theme_path,
        },
        Source::Lost,
    )
}

/// An attention icon's chain result, if it is worth showing in place of the
/// normal icon: one that only reached step 4 — a name
/// nothing has — or had nothing at all would turn a chip that draws into a
/// placeholder, so the normal icon stays.
pub fn attention(found: (Icon, Source)) -> Option<(Icon, Source)> {
    match found.1 {
        Source::Nothing | Source::Lost => None,
        _ => Some(found),
    }
}

/// How one menu's row icons resolved, for the one witness line a fetch makes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub theme: usize,
    pub own: usize,
    pub pixels: usize,
    /// Names that fell back to pixels: the theme did not have them.
    pub instead: Vec<String>,
    /// Names nothing had.
    pub lost: Vec<String>,
}

impl Tally {
    pub fn count(&mut self, name: &str, source: &Source) {
        match source {
            Source::Nothing => {}
            Source::Theme => self.theme += 1,
            Source::OwnPng(_) | Source::OwnSvg(_) => self.own += 1,
            Source::Pixels => {
                self.pixels += 1;
                if !name.is_empty() && !self.instead.iter().any(|held| held == name) {
                    self.instead.push(name.to_owned());
                }
            }
            Source::Lost => {
                if !self.lost.iter().any(|held| held == name) {
                    self.lost.push(name.to_owned());
                }
            }
        }
    }

    pub fn describe(&self) -> String {
        let mut out = format!(
            "icons: {} themed, {} own, {} pixels",
            self.theme, self.own, self.pixels
        );
        if !self.instead.is_empty() {
            out.push_str(&format!(" (no theme has {:?})", self.instead));
        }
        if !self.lost.is_empty() {
            out.push_str(&format!(", lost {:?}", self.lost));
        }
        out
    }
}
