//! The desktop-entry tests. The scratch directory of `.desktop` files is
//! built per test.

use super::*;

/// A scratch directory of `.desktop` files, removed at the end.
struct Fixtures(PathBuf);

impl Fixtures {
    fn new(tag: &str) -> Fixtures {
        let dir =
            std::env::temp_dir().join(format!("caustics-desktop-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        Fixtures(dir)
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.0.join(name), body).expect("fixture written");
    }

    fn dirs(&self) -> Vec<PathBuf> {
        vec![self.0.clone()]
    }
}

impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_exact_desktop_file_wins_and_its_three_fields_come_back() {
    let fx = Fixtures::new("exact");
    fx.write(
        "org.mozilla.firefox.desktop",
        "[Desktop Entry]\n\
         Name=Firefox\n\
         Name[de]=Feuerfuchs\n\
         Icon=firefox\n\
         Exec=/usr/bin/firefox %u\n\
         # a comment\n\
         \n\
         [Desktop Action new-window]\n\
         Name=New Window\n\
         Exec=/usr/bin/firefox --new-window\n",
    );
    let entry = desktop_entry_in(&fx.dirs(), "org.mozilla.firefox").expect("found");
    assert_eq!(entry.name.as_deref(), Some("Firefox"));
    assert_eq!(entry.icon.as_deref(), Some("firefox"));
    // The action group's Exec must not have overwritten the entry's own.
    assert_eq!(entry.command(), Some(vec!["/usr/bin/firefox".to_owned()]));
}

#[test]
fn a_case_insensitive_name_and_a_startup_wm_class_are_the_two_fallbacks() {
    let fx = Fixtures::new("fallbacks");
    fx.write(
        "Kitty.desktop",
        "[Desktop Entry]\nName=kitty\nIcon=kitty\nExec=kitty\n",
    );
    fx.write(
        "com.example.Thing.desktop",
        "[Desktop Entry]\nName=Thing\nIcon=thing\nExec=thing\nStartupWMClass=thing-gtk\n",
    );
    // Case-insensitive on the file name...
    assert_eq!(
        desktop_entry_in(&fx.dirs(), "kitty").and_then(|e| e.icon),
        Some("kitty".to_owned())
    );
    // ...and StartupWMClass when the file name says nothing.
    assert_eq!(
        desktop_entry_in(&fx.dirs(), "thing-gtk").and_then(|e| e.name),
        Some("Thing".to_owned())
    );
    // Nothing at all is None, not a panic and not a wrong answer.
    assert!(desktop_entry_in(&fx.dirs(), "no.such.app").is_none());
    assert!(desktop_entry_in(&fx.dirs(), "").is_none());
}

#[test]
fn field_codes_are_stripped_from_exec() {
    let entry = DesktopEntry {
        exec: Some("env FOO=1 /usr/bin/thing --flag %U %i %c %k 100%%".to_owned()),
        ..DesktopEntry::default()
    };
    assert_eq!(
        entry.command(),
        Some(
            ["env", "FOO=1", "/usr/bin/thing", "--flag", "100%"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        )
    );
    assert!(DesktopEntry::default().command().is_none());
}

#[test]
fn a_file_with_nothing_useful_is_not_an_answer() {
    let fx = Fixtures::new("empty");
    fx.write(
        "ghost.desktop",
        "[Desktop Entry]\nType=Application\nNoDisplay=true\n",
    );
    assert!(desktop_entry_in(&fx.dirs(), "ghost").is_none());
}

#[test]
fn the_application_dirs_include_the_flatpak_exports() {
    let dirs = application_dirs();
    assert!(
        dirs.iter()
            .any(|d| d.ends_with("flatpak/exports/share/applications")),
        "{dirs:?}"
    );
    assert!(
        dirs.iter()
            .any(|d| d == Path::new("/usr/share/applications")),
        "{dirs:?}"
    );
    // No duplicates: the search walks these in order and a repeat is work
    // done twice for the same answer.
    let mut sorted = dirs.clone();
    sorted.sort();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(sorted.len(), before, "{dirs:?}");
}

/// kdeconnect in a distrobox, as `distrobox-export` installs it:
/// the window is `org.kde.kdeconnect.app`, the exported entry is
/// `kdeconnect-org.kde.kdeconnect.app.desktop` with `StartupWMClass` rewritten
/// to the container's path. Found by either half; never for a bare id, and
/// never ahead of an ordinary match.
#[test]
fn a_distrobox_export_is_the_last_resort_for_a_reverse_dns_id() {
    let fx = Fixtures::new("distrobox");
    fx.write(
        "kdeconnect-org.kde.kdeconnect.app.desktop",
        "[Desktop Entry]\nName=KDE Connect (on kdeconnect)\nIcon=kdeconnect\n\
         StartupWMClass=/usr/share/applications/org.kde.kdeconnect.app.desktop\n",
    );
    let entry = desktop_entry_in(&fx.dirs(), "org.kde.kdeconnect.app").expect("the export");
    assert_eq!(entry.name.as_deref(), Some("KDE Connect (on kdeconnect)"));

    // The class half on its own, under a file name the suffix does not match.
    fx.write(
        "box-renamed.desktop",
        "[Desktop Entry]\nName=Renamed\nStartupWMClass=/usr/share/applications/org.example.Tool.desktop\n",
    );
    let entry = desktop_entry_in(&fx.dirs(), "org.example.Tool").expect("by its rewritten class");
    assert_eq!(entry.name.as_deref(), Some("Renamed"));

    // A bare id matches no `*-app.desktop`.
    fx.write("thing-app.desktop", "[Desktop Entry]\nName=Thing\n");
    assert_eq!(desktop_entry_in(&fx.dirs(), "app"), None);

    // An ordinary match wins over the export.
    fx.write(
        "org.kde.kdeconnect.app.desktop",
        "[Desktop Entry]\nName=Native\n",
    );
    let entry = desktop_entry_in(&fx.dirs(), "org.kde.kdeconnect.app").unwrap();
    assert_eq!(entry.name.as_deref(), Some("Native"));
}
