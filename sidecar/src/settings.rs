//! Which program a menu's "Settings" row opens.
//!
//! Five sidecars end their menu with a settings row: network, bluetooth,
//! sound, power and notifications. The bar runs on more than one desktop, so
//! none of them names a program; they name a [`Panel`] and ask
//! [`settings_command`], which looks, in order, at:
//!
//! 1. **`$CAUSTICS_SETTINGS`**, a program name. A program in [`PROGRAMS`] gets
//!    its own word for the panel (`gnome-control-center wifi`); any other
//!    gets the panel's plain name (`network`, `sound`, …) as its one
//!    argument. This is how anyone who disagrees with the guess says so.
//! 2. **The desktop's own**, by `$XDG_CURRENT_DESKTOP` (a colon-separated
//!    list, matched case-insensitively): `COSMIC`, `GNOME` or `KDE`, when
//!    that desktop's program is on `PATH`.
//! 3. **The first of [`PROGRAMS`] on `PATH`.** A compositor such as niri has
//!    no settings program of its own, and its users usually have one of
//!    these installed beside it. `cosmic-settings` is first because it is the
//!    lightest of the three to start and its pages map one-to-one on to the
//!    five panels.
//! 4. **Nothing.** The click is witnessed and nothing starts; a settings row
//!    with nowhere to go is not an error.
//!
//! The choice is made at click time, from the sidecar's own environment.
//! [`resolve`] is the same choice as a pure function of its inputs, which is
//! what the tests ask: nothing is launched to test it.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

/// The settings pages a sidecar can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Panel {
    Network,
    Bluetooth,
    Sound,
    Power,
    Notifications,
}

impl Panel {
    /// The panel's plain name: what an unknown `$CAUSTICS_SETTINGS` program is
    /// handed as its argument.
    pub fn name(self) -> &'static str {
        match self {
            Panel::Network => "network",
            Panel::Bluetooth => "bluetooth",
            Panel::Sound => "sound",
            Panel::Power => "power",
            Panel::Notifications => "notifications",
        }
    }
}

/// A settings program the fallback knows: its executable, the
/// `$XDG_CURRENT_DESKTOP` entry that says it is the desktop's own, and its
/// word for each panel.
#[derive(Clone, Copy, Debug)]
pub struct Program {
    pub program: &'static str,
    pub desktop: &'static str,
    page: fn(Panel) -> &'static str,
}

impl Program {
    /// This program's argument for a panel.
    pub fn page(&self, panel: Panel) -> &'static str {
        (self.page)(panel)
    }
}

/// The known programs, in the order the `PATH` fallback tries them.
pub const PROGRAMS: [Program; 3] = [
    // `cosmic-settings <page>`, as its per-page desktop entries run it. There
    // is no separate Wi-Fi page; `network` is the whole of it.
    Program {
        program: "cosmic-settings",
        desktop: "COSMIC",
        page: Panel::name,
    },
    // `gnome-control-center <panel>`. The network menu is about Wi-Fi, and
    // GNOME keeps Wi-Fi on a panel of its own, apart from wired `network`.
    Program {
        program: "gnome-control-center",
        desktop: "GNOME",
        page: gnome_page,
    },
    // `systemsettings <module>`: Plasma 6's module names.
    Program {
        program: "systemsettings",
        desktop: "KDE",
        page: kde_page,
    },
];

fn gnome_page(panel: Panel) -> &'static str {
    match panel {
        Panel::Network => "wifi",
        other => other.name(),
    }
}

fn kde_page(panel: Panel) -> &'static str {
    match panel {
        Panel::Network => "kcm_networkmanagement",
        Panel::Bluetooth => "kcm_bluetooth",
        Panel::Sound => "kcm_pulseaudio",
        Panel::Power => "kcm_powerdevilprofilesconfig",
        Panel::Notifications => "kcm_notifications",
    }
}

/// The override's name.
pub const SETTINGS_ENV: &str = "CAUSTICS_SETTINGS";

/// What a settings row runs: a program and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
}

impl Invocation {
    /// `program arg…`, for a witness line.
    pub fn describe(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The choice, as a pure function: `settings_env` and `desktop` are the two
/// variables' values and `on_path` says whether a program name resolves.
/// An empty `$CAUSTICS_SETTINGS` counts as unset; a set one is taken as given,
/// without a `PATH` check, so a typo shows up as a spawn error in the
/// sidecar's witness rather than as a silent fallback.
pub fn resolve(
    panel: Panel,
    settings_env: Option<&OsStr>,
    desktop: Option<&OsStr>,
    on_path: impl Fn(&str) -> bool,
) -> Option<Invocation> {
    let invocation = |program: &Program| Invocation {
        program: program.program.to_owned(),
        args: vec![program.page(panel).to_owned()],
    };

    if let Some(chosen) = settings_env
        .and_then(OsStr::to_str)
        .filter(|s| !s.is_empty())
    {
        return Some(match PROGRAMS.iter().find(|p| p.program == chosen) {
            Some(program) => invocation(program),
            None => Invocation {
                program: chosen.to_owned(),
                args: vec![panel.name().to_owned()],
            },
        });
    }

    let desktops = desktop.and_then(OsStr::to_str).unwrap_or("");
    for entry in desktops.split(':') {
        let own = PROGRAMS
            .iter()
            .find(|p| p.desktop.eq_ignore_ascii_case(entry));
        if let Some(program) = own.filter(|p| on_path(p.program)) {
            return Some(invocation(program));
        }
    }

    PROGRAMS.iter().find(|p| on_path(p.program)).map(invocation)
}

/// [`resolve`] against this process's environment and `PATH`, as a
/// [`Command`] ready to spawn detached: stdio null, so nothing it prints can
/// reach a sidecar's wire, and its own process group, so a sidecar's restart
/// does not take the window with it. `None` when no settings program can be
/// named.
pub fn settings_command(panel: Panel) -> Option<(Command, Invocation)> {
    use std::os::unix::process::CommandExt as _;

    let path = std::env::var_os("PATH").unwrap_or_default();
    let invocation = resolve(
        panel,
        std::env::var_os(SETTINGS_ENV).as_deref(),
        std::env::var_os("XDG_CURRENT_DESKTOP").as_deref(),
        |name| on_path(name, &path),
    )?;
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    Some((command, invocation))
}

/// Start the settings program for `panel`, detached, and say what happened
/// in one line for the caller's witness: `cosmic-settings sound pid=1234`,
/// the spawn error, or that there is no settings program. A thread reaps the
/// child, so a sidecar that lives for weeks collects no zombies.
pub fn open_settings(panel: Panel) -> String {
    let Some((mut command, invocation)) = settings_command(panel) else {
        return format!(
            "no settings program for {}: set ${SETTINGS_ENV}, or install one of {}",
            panel.name(),
            PROGRAMS
                .iter()
                .map(|p| p.program)
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            format!("{} pid={pid}", invocation.describe())
        }
        Err(err) => format!("{}: {err}", invocation.describe()),
    }
}

/// Whether `name` is an executable file in one of `path`'s directories. A
/// name with a slash in it is a path and is checked as it stands.
pub fn on_path(name: &str, path: &OsStr) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    let executable = |candidate: &Path| {
        candidate
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        return executable(Path::new(name));
    }
    std::env::split_paths(path).any(|dir| executable(&dir.join(name)))
}

#[cfg(test)]
mod tests;
