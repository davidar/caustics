//! The answers, against a scratch directory of `.desktop` files: what goes out
//! for an app id the host has never heard of, one it has, and an icon name.

use super::*;

/// A scratch directory of fixtures, removed at the end.
struct Fixtures(PathBuf);

impl Fixtures {
    fn new(tag: &str) -> Fixtures {
        let dir = std::env::temp_dir().join(format!("caustics-apps-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        Fixtures(dir)
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.0.join(name), body).expect("fixture written");
    }

    fn apps(&self) -> Apps {
        Apps::with_dirs(vec![self.0.clone()])
    }
}

impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn request(kind: u32, name: &str) -> wire::Request {
    wire::Request {
        kind,
        arg: 0,
        name: name.to_owned(),
    }
}

fn only_app(frames: &[Frame]) -> &wire::App {
    match frames.last() {
        Some(Frame::App(app)) => app,
        other => panic!("expected an App frame last, got {other:?}"),
    }
}

#[test]
fn an_app_id_with_no_entry_is_app_missing_and_its_own_name() {
    let fx = Fixtures::new("missing");
    let frames = fx.apps().answer(&request(REQ_APP, "com.example.Nothing"));
    assert_eq!(frames.len(), 1, "no pixmap for an app with no entry");
    let app = only_app(&frames);
    assert_eq!(app.flags, APP_MISSING);
    assert_eq!(app.app_id, "com.example.Nothing");
    // The dock shows the app id when no entry names the app.
    assert_eq!(app.name, "com.example.Nothing");
    assert!(app.path.is_empty() && app.exec.is_empty() && app.icon.is_empty());
}

#[test]
fn an_entry_becomes_a_name_a_path_and_an_argv_with_the_field_codes_gone() {
    let fx = Fixtures::new("entry");
    fx.write(
        "org.mozilla.firefox.desktop",
        "[Desktop Entry]\nName=Firefox\nIcon=caustics-no-such-icon\nExec=/usr/bin/firefox %u\n",
    );
    let frames = fx.apps().answer(&request(REQ_APP, "org.mozilla.firefox"));
    let app = only_app(&frames);
    assert_eq!(app.flags & APP_MISSING, 0);
    assert_eq!(app.name, "Firefox");
    assert!(
        app.path.ends_with("org.mozilla.firefox.desktop"),
        "{}",
        app.path
    );
    assert_eq!(app.exec, vec!["/usr/bin/firefox".to_owned()]);
}

/// The icon is pixels or it is curves, and either way the bar is told which.
///
/// Which of the two this host gives depends on its themes, so the assertion is
/// on the *shape* of the answer rather than on a particular flag: a pixmap
/// frame exactly when `APP_ICON_PIXMAP` is set, one slot of straight RGBA8,
/// keyed by the app id.
#[test]
fn an_icon_that_resolves_arrives_as_pixels_or_is_named_as_curves() {
    let fx = Fixtures::new("icon");
    fx.write(
        "caustics.test.Icon.desktop",
        "[Desktop Entry]\nName=Icon Test\nIcon=preferences-system\nExec=true\n",
    );
    let frames = fx.apps().answer(&request(REQ_APP, "caustics.test.Icon"));
    let app = only_app(&frames).clone();
    if app.flags & APP_ICON_PIXMAP != 0 {
        assert_eq!(frames.len(), 2, "the pixmap comes with the app");
        let Frame::Pixmap(pixmap) = &frames[0] else {
            panic!("the pixmap comes first")
        };
        assert_eq!(
            pixmap.key, "caustics.test.Icon",
            "keyed by the app id (docs/abi.md, *Pixmap*)"
        );
        assert_eq!(
            (pixmap.width, pixmap.height),
            (IMAGE_SLOT_PX, IMAGE_SLOT_PX)
        );
        assert_eq!(
            pixmap.rgba.len(),
            (IMAGE_SLOT_PX * IMAGE_SLOT_PX * 4) as usize
        );
        assert!(
            pixmap.rgba.chunks_exact(4).any(|t| t[3] > 0),
            "it rasterised to nothing"
        );
        println!("preferences-system arrived as one slot of pixels");
    } else if app.flags & APP_ICON_SYMBOLIC != 0 {
        assert_eq!(frames.len(), 1, "curves are caustics-glyphs', not ours");
        assert_eq!(app.icon, "preferences-system");
        println!("preferences-system is symbolic on this host; the bar asks caustics-glyphs");
    } else {
        println!("SKIP: no theme on this host has preferences-system");
    }
}

/// Every frame the sidecar writes must survive the wire it was built for —
/// which is the one thing a unit test can check that a live run cannot say
/// precisely.
#[test]
fn every_answer_round_trips_through_the_wire() {
    let fx = Fixtures::new("roundtrip");
    fx.write(
        "caustics.test.Round.desktop",
        "[Desktop Entry]\nName=Round Trip\nIcon=preferences-system\nExec=thing --flag %F\n",
    );
    let mut apps = fx.apps();
    let mut frames = apps.answer(&request(REQ_APP, "caustics.test.Round"));
    frames.extend(apps.answer(&request(REQ_ICON, "preferences-system")));
    frames.extend(apps.answer(&request(REQ_APP, "com.example.Nothing")));
    assert!(frames.len() >= 2);
    for frame in &frames {
        let mut bytes: Vec<u8> = Vec::new();
        wire::write(&mut bytes, frame).expect("the frame is writable");
        let back = wire::read(&mut bytes.as_slice())
            .expect("it reads")
            .expect("and is not an EOF");
        assert_eq!(&back, frame, "kind {} did not round trip", frame.kind());
    }
}

#[test]
fn a_repeat_request_is_answered_from_the_map_and_is_byte_identical() {
    let fx = Fixtures::new("repeat");
    fx.write(
        "caustics.test.Twice.desktop",
        "[Desktop Entry]\nName=Twice\nExec=twice\n",
    );
    let mut apps = fx.apps();
    let first = apps.answer(&request(REQ_APP, "caustics.test.Twice"));
    let again = apps.answer(&request(REQ_APP, "caustics.test.Twice"));
    assert_eq!(first, again);
    // And so is a repeat of an icon the themes do not have: silence, without a
    // second walk of five themes.
    assert!(
        apps.answer(&request(REQ_ICON, "caustics-no-such-icon"))
            .is_empty()
    );
    assert!(
        apps.answer(&request(REQ_ICON, "caustics-no-such-icon"))
            .is_empty()
    );
    assert!(
        apps.icons
            .get("caustics-no-such-icon")
            .is_some_and(|held| held.texels.is_none())
    );
}

#[test]
fn a_request_kind_that_is_not_ours_is_answered_with_nothing() {
    let fx = Fixtures::new("kinds");
    let mut apps = fx.apps();
    // REQ_GLYPH is caustics-glyphs'.
    assert!(
        apps.answer(&request(wire::REQ_GLYPH, "audio-volume-high-symbolic"))
            .is_empty()
    );
    assert!(apps.answer(&request(REQ_APP, "")).is_empty());
    assert!(apps.answer(&request(REQ_ICON, "")).is_empty());
}

#[test]
fn the_icon_candidates_are_the_icon_key_then_the_app_id_then_its_tail() {
    assert_eq!(
        icon_candidates("org.mozilla.firefox", Some("firefox")),
        vec!["firefox".to_owned(), "org.mozilla.firefox".to_owned()],
        "the tail is `firefox`, which is already there"
    );
    assert_eq!(
        icon_candidates("com.example.Thing", None),
        vec!["com.example.Thing".to_owned(), "Thing".to_owned()]
    );
    assert_eq!(icon_candidates("kitty", None), vec!["kitty".to_owned()]);
}

#[test]
fn a_field_the_wire_would_refuse_is_dropped_not_sent() {
    let long = "x".repeat(MAX_NAME_LEN + 1);
    assert!(fit(long).is_empty());
    assert_eq!(fit("short".to_owned()), "short");
    let argv: Vec<String> = (0..MAX_NAME_LEN).map(|_| "ab".to_owned()).collect();
    assert!(fit_exec(argv).is_empty());
    assert_eq!(fit_exec(vec!["a".to_owned()]), vec!["a".to_owned()]);
}

// ---------------------------------------------------------------------------
// A miss expires
// ---------------------------------------------------------------------------

/// A flat-colour PNG, as a file an app would install.
fn png(rgba: [u8; 4]) -> Vec<u8> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(8, 8).unwrap();
    pixmap.fill(resvg::tiny_skia::Color::from_rgba8(
        rgba[0], rgba[1], rgba[2], rgba[3],
    ));
    pixmap.encode_png().unwrap()
}

/// Write `bytes` at `path` with an mtime of its own: two writes inside one
/// tick of a coarse clock must still read as a change.
fn install(path: &Path, bytes: &[u8], age_s: u64) {
    std::fs::write(path, bytes).unwrap();
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(
        std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + age_s),
    )
    .unwrap();
}

/// An `Icon=` that is a JPEG path rasterises like a PNG one
/// — the fixture stands in for tailscale's `icon-data`, a JPEG on the bus.
#[test]
fn a_jpeg_path_is_pixels() {
    let fx = Fixtures::new("jpeg");
    let path = fx.0.join("photo.jpg");
    std::fs::write(&path, include_bytes!("../../tray/tests/data/icon-data.jpg")).unwrap();
    let name = path.to_str().unwrap();
    let frames = fx.apps().answer(&request(REQ_ICON, name));
    let [Frame::Pixmap(pixmap)] = frames.as_slice() else {
        panic!("{frames:?}")
    };
    assert_eq!(
        pixmap.rgba.len(),
        (IMAGE_SLOT_PX * IMAGE_SLOT_PX * 4) as usize
    );
    assert!(
        pixmap.rgba.chunks_exact(4).any(|t| t[3] == 255),
        "an opaque photo"
    );
}

/// Steam's games and a freshly installed flatpak: an app id with no entry is
/// `APP_MISSING`; the entry is installed; the sweep says it again, unasked,
/// with the name and the argv — which the bar takes as it takes any answer.
#[test]
fn a_missing_app_is_answered_again_when_its_entry_is_installed() {
    let fx = Fixtures::new("installed");
    let mut apps = fx.apps();
    let first = apps.answer(&request(REQ_APP, "caustics.test.Later"));
    assert_eq!(only_app(&first).flags, APP_MISSING);
    assert!(apps.sweep().is_empty(), "nothing changed, nothing is said");

    fx.write(
        "caustics.test.Later.desktop",
        "[Desktop Entry]\nName=Later\nExec=later --go\n",
    );
    // The directory's own mtime is what says a child arrived; give it one a
    // coarse clock cannot confuse with the first look's.
    let dir = std::fs::File::open(&fx.0).unwrap();
    dir.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    let again = apps.sweep();
    let app = only_app(&again);
    assert_eq!(app.flags & APP_MISSING, 0, "{app:?}");
    assert_eq!(app.name, "Later");
    assert_eq!(app.exec, vec!["later".to_owned(), "--go".to_owned()]);
    // And it is the answer from now on, asked or not.
    assert_eq!(apps.answer(&request(REQ_APP, "caustics.test.Later")), again);
    assert!(apps.sweep().is_empty());
}

/// An icon no theme had is looked for again, and a file changed under the
/// same name is read again — each sent once, keyed by the name the bar asked
/// under, and nothing sent while nothing changed. The name is an absolute
/// path here because the theme list is the process's (`freedesktop-icons`
/// reads it once) and a fixture cannot add a theme; the expiry under test is
/// the same for both.
#[test]
fn an_icon_that_arrives_or_changes_after_the_miss_is_sent_unasked() {
    let fx = Fixtures::new("icon-later");
    let path = fx.0.join("later.png");
    let name = path.to_str().unwrap().to_owned();
    let mut apps = fx.apps();
    assert!(
        apps.answer(&request(REQ_ICON, &name)).is_empty(),
        "a miss is silence"
    );
    assert!(apps.sweep().is_empty());

    install(&path, &png([200, 10, 10, 255]), 0);
    let arrived = apps.sweep();
    let [Frame::Pixmap(red)] = arrived.as_slice() else {
        panic!("{arrived:?}")
    };
    assert_eq!(red.key, name);
    assert_eq!(&red.rgba[..4], &[200, 10, 10, 255]);
    assert!(apps.sweep().is_empty(), "sent once");

    install(&path, &png([10, 10, 200, 255]), 60);
    let changed = apps.sweep();
    let [Frame::Pixmap(blue)] = changed.as_slice() else {
        panic!("{changed:?}")
    };
    assert_eq!(blue.key, name);
    assert_eq!(&blue.rgba[..4], &[10, 10, 200, 255]);

    // An app whose `Icon=` is that file follows it the same way.
    fx.write(
        "caustics.test.Painted.desktop",
        &format!("[Desktop Entry]\nName=P\nIcon={name}\n"),
    );
    let first = apps.answer(&request(REQ_APP, "caustics.test.Painted"));
    let Frame::Pixmap(before) = &first[0] else {
        panic!("{first:?}")
    };
    assert_eq!(&before.rgba[..4], &[10, 10, 200, 255]);
    install(&path, &png([10, 200, 10, 255]), 120);
    let after = apps.sweep();
    assert!(
        after.iter().any(
            |f| matches!(f, Frame::Pixmap(p) if p.key == "caustics.test.Painted"
            && p.rgba[..4] == [10, 200, 10, 255])
        ),
        "{after:?}"
    );
}
