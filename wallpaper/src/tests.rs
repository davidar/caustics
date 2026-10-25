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
/// answers on is another wallpaper's and is left alone.
#[test]
fn a_stale_socket_is_taken_and_a_live_one_is_not() {
    let dir = std::env::temp_dir().join(format!("caustics-wallpaper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("writable");
    let path = dir.join(SOCKET);
    let first = bind(&path).expect("binds");
    let second = bind(&path);
    assert!(second.is_err(), "a live socket was taken");
    drop(first);
    // The file is still there and nothing answers: stale.
    assert!(path.exists());
    let third = bind(&path).expect("a stale socket is taken");
    drop(third);
    let _ = std::fs::remove_dir_all(&dir);
}
