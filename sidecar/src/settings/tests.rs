use std::ffi::OsStr;

use super::*;

const ALL: [Panel; 5] = [
    Panel::Network,
    Panel::Bluetooth,
    Panel::Sound,
    Panel::Power,
    Panel::Notifications,
];

fn only<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
    move |name| names.contains(&name)
}

fn call(program: &str, arg: &str) -> Option<Invocation> {
    Some(Invocation {
        program: program.to_owned(),
        args: vec![arg.to_owned()],
    })
}

#[test]
fn the_override_wins_over_the_desktop_and_path() {
    let chosen = resolve(
        Panel::Sound,
        Some(OsStr::new("gnome-control-center")),
        Some(OsStr::new("COSMIC")),
        only(&["cosmic-settings"]),
    );
    assert_eq!(chosen, call("gnome-control-center", "sound"));
}

#[test]
fn a_known_override_gets_its_own_page_words() {
    let wifi = resolve(
        Panel::Network,
        Some(OsStr::new("gnome-control-center")),
        None,
        only(&[]),
    );
    assert_eq!(wifi, call("gnome-control-center", "wifi"));
    let kcm = resolve(
        Panel::Sound,
        Some(OsStr::new("systemsettings")),
        None,
        only(&[]),
    );
    assert_eq!(kcm, call("systemsettings", "kcm_pulseaudio"));
}

#[test]
fn an_unknown_override_gets_the_panel_name_and_no_path_check() {
    for panel in ALL {
        let chosen = resolve(panel, Some(OsStr::new("my-settings")), None, only(&[]));
        assert_eq!(chosen, call("my-settings", panel.name()));
    }
}

#[test]
fn an_empty_override_is_unset() {
    let chosen = resolve(
        Panel::Power,
        Some(OsStr::new("")),
        None,
        only(&["cosmic-settings"]),
    );
    assert_eq!(chosen, call("cosmic-settings", "power"));
}

#[test]
fn the_desktop_picks_its_own_program() {
    let path = only(&["cosmic-settings", "gnome-control-center", "systemsettings"]);
    assert_eq!(
        resolve(
            Panel::Bluetooth,
            None,
            Some(OsStr::new("ubuntu:GNOME")),
            &path
        ),
        call("gnome-control-center", "bluetooth")
    );
    assert_eq!(
        resolve(Panel::Notifications, None, Some(OsStr::new("KDE")), &path),
        call("systemsettings", "kcm_notifications")
    );
    assert_eq!(
        resolve(Panel::Network, None, Some(OsStr::new("cosmic")), &path),
        call("cosmic-settings", "network")
    );
}

#[test]
fn a_desktop_whose_program_is_missing_falls_through_to_path() {
    let chosen = resolve(
        Panel::Sound,
        None,
        Some(OsStr::new("GNOME")),
        only(&["systemsettings"]),
    );
    assert_eq!(chosen, call("systemsettings", "kcm_pulseaudio"));
}

#[test]
fn a_desktop_without_a_program_takes_the_first_on_path_in_order() {
    // A compositor with no settings program, and two installed beside it: the
    // documented order decides.
    let chosen = resolve(
        Panel::Sound,
        None,
        Some(OsStr::new("niri")),
        only(&["gnome-control-center", "cosmic-settings"]),
    );
    assert_eq!(chosen, call("cosmic-settings", "sound"));
    assert_eq!(
        PROGRAMS.map(|p| p.program),
        ["cosmic-settings", "gnome-control-center", "systemsettings"]
    );
}

#[test]
fn nothing_found_is_none() {
    for panel in ALL {
        assert_eq!(
            resolve(panel, None, Some(OsStr::new("niri")), only(&[])),
            None
        );
        assert_eq!(resolve(panel, None, None, only(&[])), None);
    }
}

#[test]
fn cosmic_pages_are_the_panel_names() {
    for panel in ALL {
        assert_eq!(PROGRAMS[0].page(panel), panel.name());
    }
}

#[test]
fn on_path_finds_an_executable_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = std::env::temp_dir().join(format!("caustics-settings-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("fake-settings");
    std::fs::write(&exe, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("not-executable"), "").unwrap();

    let path = std::env::join_paths([Path::new("/nonexistent-caustics"), &dir]).unwrap();
    assert!(on_path("fake-settings", &path));
    assert!(!on_path("not-executable", &path));
    assert!(!on_path("no-such-program", &path));
    assert!(!on_path("fake-settings", OsStr::new("")));
    assert!(on_path(exe.to_str().unwrap(), OsStr::new("")));
    let _ = std::fs::remove_dir_all(&dir);
}
