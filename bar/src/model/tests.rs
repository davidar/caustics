use super::*;

fn win(id: u64, app: &str, stamp: u64) -> Window {
    Window {
        id,
        app_id: app.into(),
        title: format!("w{id}"),
        focused: false,
        urgent: false,
        focus_stamp: stamp,
    }
}

#[test]
fn updates_are_idempotent_and_report_change() {
    let mut m = Model::default();
    assert!(m.apply(Update::Window(win(1, "kitty", 0))));
    assert!(!m.apply(Update::Window(win(1, "kitty", 0))));
    assert!(m.apply(Update::Window(win(1, "kitty", 5))));
    assert!(m.apply(Update::WindowClosed(1)));
    assert!(!m.apply(Update::WindowClosed(1)));
    assert!(m.apply(Update::Absent("ext_workspace_manager_v1".to_owned())));
    assert!(!m.apply(Update::Absent("ext_workspace_manager_v1".to_owned())));
}

/// `windows_of` is membership and nothing else: the app's windows, each with
/// the focus stamp the caustic orders by, and in no order of its own. What the
/// test pins is that the *stamps* are there to be ordered by, because that is
/// the fact the caustic's `pick_window` needs.
#[test]
fn windows_of_is_the_apps_windows_with_their_stamps() {
    let mut m = Model::default();
    m.apply(Update::Window(win(1, "firefox", 2)));
    m.apply(Update::Window(win(2, "firefox", 9)));
    m.apply(Update::Window(win(3, "kitty", 1)));

    let mut got: Vec<(u64, u64)> = m
        .windows_of("firefox")
        .iter()
        .map(|w| (w.id, w.focus_stamp))
        .collect();
    got.sort();
    assert_eq!(got, vec![(1, 2), (2, 9)], "both windows, both stamps");
    assert!(m.windows_of("kitty").iter().all(|w| w.id == 3));
    assert!(m.windows_of("nothing").is_empty());
    assert_eq!(m.running_app_ids(), vec!["firefox", "kitty"]);
}
