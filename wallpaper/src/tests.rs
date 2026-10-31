use super::*;

#[test]
fn the_output_size_is_width_x_height() {
    assert_eq!(parse_size("1280x800"), Some((1280, 800)));
    assert_eq!(parse_size("0x800"), None);
    assert_eq!(parse_size("1280"), None);
    assert_eq!(parse_size("ax1"), None);
}

/// `status`'s reply: nothing shown, a fade on its way, a fade over; the path
/// last, so it reads whole with spaces in it.
#[test]
fn status_says_the_picture_and_how_far_its_fade_is() {
    assert_eq!(describe(None, 0), "fade=none image=none");
    let shown = Shown {
        path: "/a b.png".into(),
        start_ms: 1000,
        duration_ms: 4000,
    };
    assert_eq!(
        describe(Some(&shown), 2000),
        "fade=fading progress=0.25 duration=4.000 image=/a b.png"
    );
    assert_eq!(describe(Some(&shown), 5000), "fade=done image=/a b.png");
    let cut = Shown {
        duration_ms: 0,
        ..shown
    };
    assert_eq!(describe(Some(&cut), 1000), "fade=done image=/a b.png");
}

/// A socket nobody answers on is a stale one and is taken; one somebody
/// answers on for the whole window is another wallpaper's and is left alone.
#[test]
fn a_stale_socket_is_taken_and_a_live_one_is_not() {
    let dir = std::env::temp_dir().join(format!("caustics-wallpaper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("writable");
    let path = dir.join(SOCKET);
    let window = Duration::from_millis(200);
    let first = bind(&path, window).expect("binds");
    let second = bind(&path, window);
    assert!(second.is_err(), "a live socket was taken");
    drop(first);
    // The file is still there and nothing answers: stale.
    assert!(path.exists());
    let third = bind(&path, window).expect("a stale socket is taken");
    drop(third);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A listener that stops answering inside the window is the previous
/// sidecar on its way out, and its socket is taken once it has gone.
#[test]
fn a_listener_that_closes_during_the_window_is_taken() {
    let dir =
        std::env::temp_dir().join(format!("caustics-wallpaper-closing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("writable");
    let path = dir.join(SOCKET);
    let first = bind(&path, Duration::from_millis(200)).expect("binds");
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(first);
    });
    let started = Instant::now();
    let second = bind(&path, Duration::from_secs(5)).expect("taken once the first closed");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the window was waited out instead of the listener's closing"
    );
    closer.join().expect("the closer finished");
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `$XDG_STATE_HOME` when it is absolute, else `~/.local/state`, and the
/// file under it beside the bar's favourites.
#[test]
fn the_state_file_follows_the_base_directory_rule() {
    let state = |xdg: Option<&str>, home: Option<&str>| {
        state_last(xdg.map(Into::into), home.map(Into::into))
    };
    assert_eq!(
        state(Some("/s"), Some("/h")),
        Some(PathBuf::from("/s/caustics/canvas/last"))
    );
    assert_eq!(
        state(Some("relative"), Some("/h")),
        Some(PathBuf::from("/h/.local/state/caustics/canvas/last"))
    );
    assert_eq!(
        state(None, Some("/h")),
        Some(PathBuf::from("/h/.local/state/caustics/canvas/last"))
    );
    assert_eq!(state(None, None), None);
    assert_eq!(state(None, Some("relative")), None);
}

/// A start reads the runtime file first and the state file after it, so a
/// fresh login (no runtime file) shows what the last session showed.
#[test]
fn the_last_path_is_read_from_the_runtime_file_then_the_state_file() {
    let dir = std::env::temp_dir().join(format!("caustics-wallpaper-last-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let runtime = dir.join("runtime");
    let state_home = dir.join("state");
    std::fs::create_dir_all(&runtime).expect("writable");
    let last = Last::new(&runtime, state_last(Some(state_home.clone().into()), None));
    let runtime_file = runtime.join(LAST);
    let state_file = state_home.join(LAST_STATE);
    assert_eq!(last.files, vec![runtime_file.clone(), state_file.clone()]);

    // Neither file: nothing to show.
    assert_eq!(last.read(), None);

    // Keeping a path writes both, the state file's directory included.
    last.keep(Path::new("/pictures/a b.png"));
    assert_eq!(
        std::fs::read_to_string(&runtime_file).unwrap(),
        "/pictures/a b.png"
    );
    assert_eq!(
        std::fs::read_to_string(&state_file).unwrap(),
        "/pictures/a b.png"
    );

    // Both: the runtime file wins.
    std::fs::write(&state_file, "/pictures/older.png").unwrap();
    assert_eq!(
        last.read(),
        Some((PathBuf::from("/pictures/a b.png"), runtime_file.as_path()))
    );

    // A fresh login has no runtime file: the state file answers.
    std::fs::remove_file(&runtime_file).unwrap();
    assert_eq!(
        last.read(),
        Some((PathBuf::from("/pictures/older.png"), state_file.as_path()))
    );

    // A runtime file that names no absolute path is passed over.
    std::fs::write(&runtime_file, "relative.png\n").unwrap();
    assert_eq!(
        last.read(),
        Some((PathBuf::from("/pictures/older.png"), state_file.as_path()))
    );

    // Without a state directory there is only the runtime file.
    let runtime_only = Last::new(&runtime, None);
    assert_eq!(runtime_only.files, vec![runtime_file]);
    assert_eq!(runtime_only.read(), None);

    let _ = std::fs::remove_dir_all(&dir);
}
