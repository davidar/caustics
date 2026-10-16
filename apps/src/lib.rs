//! `caustics-apps` — the bar's desktop-entry and colour-icon sidecar.
//!
//! It answers `Request` frames (docs/abi.md, *Request*): an app id in, an
//! [`wire::App`] out; an icon name in, a [`wire::Pixmap`] out. Both are work
//! the bar could do on a thread of its own — walk `$XDG_DATA_DIRS` parsing
//! `.desktop` files, walk five icon themes, run resvg over an SVG — and none
//! of them is a Wayland object, a file descriptor the bar holds, or a round trip. That is the
//! membership test (docs/architecture.md, *Sidecars*), so it is a sidecar's.
//!
//! It answers what it is asked, and it also
//! **answers again unasked** when an answer goes stale: every [`SWEEP`] it
//! looks again at what it said — a miss is looked for again, a file an answer
//! was made from is checked for a change — and writes the new answer when it
//! differs. The bar asks once per name and keeps what arrives, so a new
//! answer is all it takes: an `App` replaces the last for its id, a `Pixmap`
//! overwrites the slot under its key. Nothing in the bar has to forget.
//!
//! `Ok(None)` from [`wire::read`] is the bar closing its end, and this process
//! leaves with 0. There is no text on stdout, ever: stderr is the witness
//! channel, and it inherits the bar's journal.

pub mod entry;
pub mod icon;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use caustics_bar_abi::IMAGE_SLOT_PX;
use caustics_bar_abi::wire::{
    self, APP_ICON_PIXMAP, APP_ICON_SYMBOLIC, APP_MISSING, Frame, MAX_NAME_LEN, REQ_APP, REQ_ICON,
};
use caustics_sidecar::inbox::{Inbox, Next};

/// This sidecar's witness line: `caustics-apps: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-apps");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// How often what this process has said is looked at again:
/// the longest a miss is believed, and the longest a changed file goes
/// unnoticed.
///
/// A bounded age rather than a watch because what would have to be watched is
/// every directory of every icon theme — a file lands in
/// `hicolor/48x48/apps/`, and a directory's mtime moves only for its own
/// children — and because the misses are few (a handful of names per bar)
/// while installs are rare. Half a minute after `flatpak install` the dock
/// has the icon; the cost is one walk of the theme directories' mtimes per sweep and one
/// `stat` per remembered file, twice a minute.
pub const SWEEP: Duration = Duration::from_secs(30);

/// Read requests, answer them, look again every [`SWEEP`], exit 0 at EOF.
pub fn run() -> i32 {
    let mut apps = Apps::new();
    witness(&format!(
        "started; {} application directories, icon themes {:?}",
        apps.dirs.len(),
        icon::theme_order()
    ));

    // `Stdin` is the handle a thread may hold; its lock is not, so each read
    // takes the lock for itself. The buffer behind it is the process's own.
    let stdin = std::io::stdin();
    let mut failed = false;
    let mut inbox = Inbox::spawn(SWEEP, move || {
        if failed {
            return None;
        }
        match wire::read(&mut stdin.lock()) {
            Ok(None) => None,
            Ok(Some(frame)) => Some(Ok(frame)),
            Err(err) => {
                failed = true;
                Some(Err(err))
            }
        }
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    loop {
        let replies = match inbox.next() {
            Next::Closed => {
                witness("stdin closed; exiting");
                return 0;
            }
            Next::Item(Err(err)) => {
                witness(&format!("stdin: {err}"));
                return 1;
            }
            Next::Tick => apps.sweep(),
            Next::Item(Ok(frame)) => match frame {
                Frame::Request(request) => apps.answer(&request),
                // A frame `wire::read` would not decode — a `Request` from a
                // newer bar, or a kind this one does not know — says why it
                // went.
                Frame::Skipped(skipped) => {
                    witness(&format!("skipped: {skipped}"));
                    continue;
                }
                // The bar writes actions to every sidecar that owns rows; this
                // one owns none, so anything else is noise.
                other => {
                    witness(&format!("ignoring frame kind {}", other.kind()));
                    continue;
                }
            },
        };
        for reply in replies {
            if let Err(err) = wire::write(&mut out, &reply) {
                if err.kind() == std::io::ErrorKind::BrokenPipe {
                    witness("stdout closed; exiting");
                    return 0;
                }
                witness(&format!("stdout write failed: {err}"));
                return 1;
            }
        }
    }
}

/// A file an answer was made from, and its mtime then — or `None` for an
/// answer made from nothing, which is a miss.
type Seen = Option<(PathBuf, Option<SystemTime>)>;

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn seen(path: &Path) -> Seen {
    Some((path.to_path_buf(), mtime(path)))
}

/// Whether the file behind an answer is as it was: there, and not modified.
fn unchanged(file: &(PathBuf, Option<SystemTime>)) -> bool {
    let now = mtime(&file.0);
    now.is_some() && now == file.1
}

/// One app id's answer, and what it was made from.
#[derive(Clone, Debug)]
struct HeldApp {
    app: wire::App,
    pixels: Option<Vec<u8>>,
    /// The desktop entry, or `None` for `APP_MISSING`.
    entry: Seen,
    /// The icon file, or `None` when no candidate resolved.
    icon: Seen,
    /// The icon names tried, in order — what a miss is looked for under again.
    candidates: Vec<String>,
}

/// One `REQ_ICON` name's answer, and what it was made from.
#[derive(Clone, Debug)]
struct HeldIcon {
    texels: Option<Vec<u8>>,
    file: Seen,
}

/// Every answer this process has worked out, so a repeat request — the bar
/// restarted, or an image slot was evicted and wanted back — costs no walk of
/// the filesystem, and the sweep knows what to look at again.
pub struct Apps {
    /// Where `.desktop` files are looked for, and their mtimes at the last
    /// look: a new entry is a new child of one of them, which is what lets a
    /// missing app id be looked for again without a full read of every entry
    /// on the machine twice a minute.
    dirs: Vec<PathBuf>,
    dir_mtimes: Vec<Option<SystemTime>>,
    /// `app_id` → its answer.
    apps: HashMap<String, HeldApp>,
    /// `REQ_ICON` name → its texels, or `None` for "no theme has it, or it is
    /// curves" — which is answered with silence, and remembered so a burst of repeat requests (a bar restart, an evicted slot) costs one theme walk per name.
    icons: HashMap<String, HeldIcon>,
}

impl Default for Apps {
    fn default() -> Apps {
        Apps::new()
    }
}

impl Apps {
    pub fn new() -> Apps {
        Apps::with_dirs(entry::application_dirs())
    }

    /// [`Apps::new`] against an explicit set of application directories, which
    /// is what makes the answers testable against fixtures.
    pub fn with_dirs(dirs: Vec<PathBuf>) -> Apps {
        let dir_mtimes = dirs.iter().map(|dir| mtime(dir)).collect();
        Apps {
            dirs,
            dir_mtimes,
            apps: HashMap::new(),
            icons: HashMap::new(),
        }
    }

    /// The frames one request is answered with, in the order they go out.
    ///
    /// A `Pixmap` precedes the `App` that names it so the bar has the texels
    /// by the time it knows to look for them — docs/abi.md, *App*, allows either order, and
    /// this one costs the bar one fewer rebuild.
    pub fn answer(&mut self, request: &wire::Request) -> Vec<Frame> {
        match request.kind {
            REQ_APP => self.app(&request.name),
            REQ_ICON => self.icon(&request.name),
            other => {
                witness(&format!(
                    "request kind {other} for {:?} is not ours",
                    request.name
                ));
                Vec::new()
            }
        }
    }

    /// Look again at every answer given, and the frames
    /// for the ones that changed — written unasked, which the bar takes as it
    /// takes any answer.
    ///
    /// What is looked at again, and only that: a missing app id when an
    /// application directory changed; a desktop entry or an icon file whose
    /// mtime moved or that went away; an icon name, an app's or a
    /// `REQ_ICON`'s, that no theme had, which costs a theme walk only when a theme directory changed. Everything
    /// else is one `stat`.
    pub fn sweep(&mut self) -> Vec<Frame> {
        let now: Vec<Option<SystemTime>> = self.dirs.iter().map(|dir| mtime(dir)).collect();
        let dirs_changed = now != self.dir_mtimes;
        self.dir_mtimes = now;

        let mut out = Vec::new();
        let mut ids: Vec<String> = self.apps.keys().cloned().collect();
        ids.sort();
        for app_id in ids {
            let Some(held) = self.apps.get(&app_id) else {
                continue;
            };
            if !stale_app(held, dirs_changed) {
                continue;
            }
            let fresh = self.resolve_app(&app_id);
            let held = &self.apps[&app_id];
            if fresh.app != held.app || fresh.pixels != held.pixels {
                witness(&format!("{app_id:?} changed: {}", describe_app(&fresh)));
                out.extend(frames_for(fresh.app.clone(), fresh.pixels.clone()));
            }
            self.apps.insert(app_id, fresh);
        }

        let mut names: Vec<String> = self.icons.keys().cloned().collect();
        names.sort();
        for name in names {
            let Some(held) = self.icons.get(&name) else {
                continue;
            };
            if !stale_icon(&name, held) {
                continue;
            }
            let (fresh, why) = self.resolve_named_icon(&name);
            if fresh.texels != self.icons[&name].texels {
                match &fresh.texels {
                    Some(texels) => {
                        witness(&format!("icon {name:?} changed -> one slot"));
                        out.push(pixmap_frame(name.clone(), texels.clone()));
                    }
                    // A pixmap sent cannot be unsent: the bar keeps the last
                    // one under the name, which is a picture that was true.
                    None => witness(&format!(
                        "icon {name:?} went away{why}; the bar keeps the last"
                    )),
                }
            }
            self.icons.insert(name, fresh);
        }
        out
    }

    fn app(&mut self, app_id: &str) -> Vec<Frame> {
        if app_id.is_empty() {
            return Vec::new();
        }
        if let Some(held) = self.apps.get(app_id) {
            return frames_for(held.app.clone(), held.pixels.clone());
        }
        let held = self.resolve_app(app_id);
        witness(&format!("{app_id:?} -> {}", describe_app(&held)));
        let frames = frames_for(held.app.clone(), held.pixels.clone());
        self.apps.insert(app_id.to_owned(), held);
        frames
    }

    /// The whole answer for an app id, from the disk, remembering nothing.
    fn resolve_app(&self, app_id: &str) -> HeldApp {
        let found = entry::desktop_entry_in(&self.dirs, app_id);
        let mut app = match &found {
            None => wire::App {
                flags: APP_MISSING,
                app_id: app_id.to_owned(),
                name: app_id.to_owned(),
                icon: String::new(),
                path: String::new(),
                exec: Vec::new(),
            },
            Some(entry) => wire::App {
                flags: 0,
                app_id: app_id.to_owned(),
                name: fit(entry.name.clone().unwrap_or_else(|| app_id.to_owned())),
                icon: String::new(),
                path: fit(entry.path.display().to_string()),
                exec: fit_exec(entry.command().unwrap_or_default()),
            },
        };

        // The icon, if any of the candidates resolves. An entry with no `Icon=`
        // still usually has one under its own id (`kitty`, `firefox`), so the
        // id, and its last dotted part, follow the entry's own `Icon=`.
        let candidates = icon_candidates(app_id, found.as_ref().and_then(|e| e.icon.as_deref()));
        let mut pixels = None;
        let mut icon_file = None;
        for candidate in &candidates {
            let (resolved, file) = self.resolve_icon(candidate);
            match resolved {
                Resolved::None => continue,
                Resolved::Symbolic(name) => {
                    app.flags |= APP_ICON_SYMBOLIC;
                    app.icon = fit(name);
                    icon_file = file;
                    break;
                }
                Resolved::Pixels(name, texels) => {
                    app.flags |= APP_ICON_PIXMAP;
                    app.icon = fit(name);
                    pixels = Some(texels);
                    icon_file = file;
                    break;
                }
            }
        }
        HeldApp {
            app,
            pixels,
            entry: found.as_ref().and_then(|entry| seen(&entry.path)),
            icon: icon_file,
            candidates,
        }
    }

    fn icon(&mut self, name: &str) -> Vec<Frame> {
        if name.is_empty() {
            return Vec::new();
        }
        if let Some(held) = self.icons.get(name) {
            return match &held.texels {
                Some(texels) => vec![pixmap_frame(name.to_owned(), texels.clone())],
                None => Vec::new(),
            };
        }
        let (held, why) = self.resolve_named_icon(name);
        witness(&format!(
            "icon {name:?} -> {}{why}",
            if held.texels.is_some() {
                "one slot"
            } else {
                "nothing"
            }
        ));
        let frames = match &held.texels {
            Some(texels) => vec![pixmap_frame(name.to_owned(), texels.clone())],
            None => Vec::new(),
        };
        self.icons.insert(name.to_owned(), held);
        frames
    }

    /// A `REQ_ICON` name's answer from the disk, and the witness's reason when
    /// it is silence.
    fn resolve_named_icon(&self, name: &str) -> (HeldIcon, String) {
        let (resolved, file) = self.resolve_icon(name);
        let (texels, why) = match resolved {
            Resolved::Pixels(_, texels) => (Some(texels), String::new()),
            // Curves are `caustics-glyphs`', never resvg's: a
            // symbolic icon rasterised as it stands is near-black, its colour
            // being the theme's to give. Silence is the answer docs/abi.md, *Request*, allows.
            Resolved::Symbolic(_) => (None, " (symbolic: caustics-glyphs' own)".to_owned()),
            Resolved::None => (None, " (no theme has it)".to_owned()),
        };
        (HeldIcon { texels, file }, why)
    }

    /// One candidate — an absolute path or a theme name — to what it is, and
    /// the file it was read from when there was one.
    fn resolve_icon(&self, candidate: &str) -> (Resolved, Seen) {
        let as_path = Path::new(candidate);
        let (path, name) = if as_path.is_absolute() {
            if !as_path.is_file() {
                return (Resolved::None, None);
            }
            let stem = as_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(candidate)
                .to_owned();
            (as_path.to_path_buf(), stem)
        } else {
            match icon::resolve(candidate, IMAGE_SLOT_PX) {
                Some((path, _)) => (path, candidate.to_owned()),
                None => return (Resolved::None, None),
            }
        };
        let file = seen(&path);
        if icon::is_symbolic(&name, &path) {
            return (Resolved::Symbolic(name), file);
        }
        match icon::raster(&path) {
            Some(texels) => (Resolved::Pixels(name, texels), file),
            // A file that will not decode is a miss, but its path and mtime are kept, so
            // a fixed file at the same path ends the miss at the next sweep.
            None => (Resolved::None, file),
        }
    }
}

/// Whether an app's answer may have gone stale since it was made.
fn stale_app(held: &HeldApp, dirs_changed: bool) -> bool {
    let entry = match &held.entry {
        Some(file) => !unchanged(file),
        None => dirs_changed,
    };
    entry
        || match &held.icon {
            Some(file) => !unchanged(file),
            None => held.candidates.iter().any(|candidate| findable(candidate)),
        }
}

/// Whether a `REQ_ICON` answer may have gone stale since it was made.
fn stale_icon(name: &str, held: &HeldIcon) -> bool {
    match &held.file {
        Some(file) => !unchanged(file),
        None => findable(name),
    }
}

/// Whether an icon name or path finds a file now — the theme walk a miss is
/// retried with, without rendering anything.
fn findable(candidate: &str) -> bool {
    let as_path = Path::new(candidate);
    if as_path.is_absolute() {
        return as_path.is_file();
    }
    icon::resolve(candidate, IMAGE_SLOT_PX).is_some()
}

fn describe_app(held: &HeldApp) -> String {
    format!(
        "{:?} ({}), icon {:?}{}",
        held.app.name,
        if held.entry.is_some() {
            held.app.path.as_str()
        } else {
            "no desktop entry"
        },
        held.app.icon,
        icon_note(held.app.flags),
    )
}

/// What one icon candidate turned out to be.
enum Resolved {
    /// Nothing in any theme, or nothing that would decode.
    None,
    /// Curves, not pixels: the name to ask `caustics-glyphs` for.
    Symbolic(String),
    /// One slot of straight RGBA8, and the name it came from.
    Pixels(String, Vec<u8>),
}

/// `Icon=` first, then the app id and its last dot-component — which is what
/// makes `kitty` and `firefox` show an icon with no `Icon=` at all.
fn icon_candidates(app_id: &str, icon: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(3);
    let mut push = |candidate: String| {
        if !candidate.is_empty() && !out.contains(&candidate) {
            out.push(candidate);
        }
    };
    if let Some(icon) = icon {
        push(icon.to_owned());
    }
    push(app_id.to_owned());
    push(app_id.rsplit('.').next().unwrap_or(app_id).to_owned());
    out
}

fn frames_for(app: wire::App, pixels: Option<Vec<u8>>) -> Vec<Frame> {
    let key = app.app_id.clone();
    let mut out = Vec::with_capacity(2);
    if let Some(texels) = pixels {
        out.push(pixmap_frame(key, texels));
    }
    out.push(Frame::App(app));
    out
}

fn pixmap_frame(key: String, rgba: Vec<u8>) -> Frame {
    Frame::Pixmap(wire::Pixmap {
        key,
        width: IMAGE_SLOT_PX,
        height: IMAGE_SLOT_PX,
        rgba,
    })
}

fn icon_note(flags: u32) -> &'static str {
    if flags & APP_ICON_PIXMAP != 0 {
        " as pixels"
    } else if flags & APP_ICON_SYMBOLIC != 0 {
        " as curves"
    } else {
        " (none)"
    }
}

/// A field *App*'s reader would refuse is dropped rather than sent: `write`
/// errors on one past [`MAX_NAME_LEN`], and one absurd `Exec=` must not be the
/// end of the wire.
fn fit(field: String) -> String {
    if field.len() <= MAX_NAME_LEN {
        return field;
    }
    witness(&format!(
        "dropping a {} byte field (the wire's cap is {MAX_NAME_LEN})",
        field.len()
    ));
    String::new()
}

fn fit_exec(argv: Vec<String>) -> Vec<String> {
    // The blob is the words joined by NUL, which is what the length is
    // measured against.
    let blob: usize = argv
        .iter()
        .map(|word| word.len() + 1)
        .sum::<usize>()
        .saturating_sub(1);
    if blob <= MAX_NAME_LEN {
        return argv;
    }
    witness(&format!(
        "dropping a {blob} byte Exec= (the wire's cap is {MAX_NAME_LEN})"
    ));
    Vec::new()
}

#[cfg(test)]
mod tests;
