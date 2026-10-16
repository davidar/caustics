//! The shared search, against fixture directories, and one check against the host's own themes.

use super::*;

/// A scratch directory, removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "caustics-sidecar-icons-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        Scratch(dir)
    }

    fn touch(&self, rel: &str) -> PathBuf {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_theme_order_starts_somewhere_and_ends_at_hicolor_without_duplicates() {
    let order = theme_order();
    assert_eq!(order.last().map(String::as_str), Some("hicolor"));
    for theme in FALLBACK_THEMES {
        assert!(
            order.iter().any(|t| t == theme),
            "{theme} missing from {order:?}"
        );
    }
    assert_eq!(
        order_after(Some("Adwaita".into())),
        vec!["Adwaita", "Cosmic", "Papirus", "hicolor"]
    );
    assert_eq!(order_after(Some("Breeze".into()))[0], "Breeze");
    assert_eq!(order_after(None).len(), FALLBACK_THEMES.len());
    // The second call is the held answer, not a second `gsettings`.
    assert_eq!(theme_order(), order);
}

#[test]
fn an_absolute_path_is_its_own_answer_and_a_relative_path_is_none() {
    let scratch = Scratch::new("path");
    let file = scratch.touch("thing.svg");
    let name = file.to_str().unwrap();
    assert_eq!(resolve(name, 64), Some((file.clone(), "path".to_owned())));
    assert_eq!(resolve(&format!("{name}.gone"), 64), None);
    assert_eq!(resolve("some/relative", 64), None);
    assert_eq!(resolve("", 64), None);
}

#[test]
fn symbolic_is_an_svg_the_theme_or_the_name_calls_symbolic() {
    assert!(is_symbolic(
        "x-symbolic",
        Path::new("/usr/share/icons/A/16/x-symbolic.svg")
    ));
    assert!(is_symbolic(
        "x",
        Path::new("/usr/share/icons/A/symbolic/apps/x.svg")
    ));
    // A path sent as the name: the file's own stem says it.
    assert!(is_symbolic(
        "/opt/i/x-symbolic.svg",
        Path::new("/opt/i/x-symbolic.svg")
    ));
    assert!(!is_symbolic(
        "x-symbolic",
        Path::new("/usr/share/icons/A/16/x-symbolic.png")
    ));
    assert!(!is_symbolic(
        "firefox",
        Path::new("/usr/share/icons/hicolor/scalable/apps/firefox.svg")
    ));
}

#[test]
fn an_own_directory_is_read_flat_or_as_a_theme_and_the_largest_png_wins() {
    let scratch = Scratch::new("own");
    // Steam's: flat.
    let flat = scratch.touch("flat/steam_tray_mono.png");
    let found = in_directory(&scratch.0.join("flat"), "steam_tray_mono");
    assert_eq!(found.png.as_deref(), Some(flat.as_path()));
    assert_eq!(found.svg, None);

    // Theme-shaped, three sizes and a scalable one.
    scratch.touch("themed/hicolor/16x16/apps/kde-thing.png");
    let big = scratch.touch("themed/hicolor/48x48/apps/kde-thing.png");
    scratch.touch("themed/hicolor/22x22/apps/kde-thing.png");
    let svg = scratch.touch("themed/hicolor/scalable/apps/kde-thing.svg");
    let found = in_directory(&scratch.0.join("themed"), "kde-thing");
    assert_eq!(
        found.png.as_deref(),
        Some(big.as_path()),
        "48x48 beats 22 and 16"
    );
    assert_eq!(found.svg.as_deref(), Some(svg.as_path()));

    // HiDPI counts its scale; a flat file loses to any sized one.
    let hidpi = scratch.touch("hidpi/hicolor/32x32@2/apps/a.png");
    scratch.touch("hidpi/hicolor/48x48/apps/a.png");
    scratch.touch("hidpi/a.png");
    assert_eq!(
        in_directory(&scratch.0.join("hidpi"), "a").png.as_deref(),
        Some(hidpi.as_path())
    );

    // Too deep, a different name, a name that is a path, a missing directory.
    scratch.touch("deep/a/b/c/d/x.png");
    assert!(in_directory(&scratch.0.join("deep"), "x").is_empty());
    assert!(in_directory(&scratch.0.join("flat"), "dropboxstatus-idle").is_empty());
    assert!(in_directory(&scratch.0, "flat/steam_tray_mono").is_empty());
    assert!(in_directory(Path::new("/nonexistent/caustics"), "x").is_empty());
}

/// The one host check: a name every theme set on a desktop has, if this host
/// has any, resolves to a file. Skipped, not failed, on a host with none.
#[test]
fn a_common_name_resolves_on_a_host_with_themes() {
    match resolve("audio-volume-high-symbolic", 64) {
        Some((path, theme)) => {
            assert!(path.is_file());
            println!(
                "audio-volume-high-symbolic is {} in {theme}",
                path.display()
            );
        }
        None => println!(
            "SKIP: no theme in {:?} has audio-volume-high-symbolic",
            theme_order()
        ),
    }
    assert_eq!(resolve("caustics-no-theme-has-this-name", 64), None);
}

#[test]
fn a_miss_is_held_until_a_theme_directory_changes() {
    let scratch = Scratch::new("misses");
    scratch.touch("icons/hicolor/48x48/apps/there.png");
    let order = vec!["hicolor".to_owned()];
    // No reuse of a walk, so the test need not wait one out.
    let mut misses = Misses::new(
        vec![scratch.0.join("icons"), scratch.0.join("pixmaps")],
        Duration::ZERO,
    );

    assert!(!misses.holds("later", &order), "never searched for");
    misses.remember("later");
    assert!(misses.holds("later", &order));
    assert!(misses.holds("later", &order), "and again: nothing changed");

    // A file that lands in an existing directory moves that directory's mtime.
    let apps = scratch.0.join("icons/hicolor/48x48/apps");
    let before = std::fs::metadata(&apps).unwrap().modified().unwrap();
    scratch.touch("icons/hicolor/48x48/apps/later.png");
    let dir = std::fs::File::open(&apps).unwrap();
    dir.set_modified(before + Duration::from_secs(60)).unwrap();
    assert!(
        !misses.holds("later", &order),
        "the themes changed; search again"
    );

    // So does a root that was not there, and a change of theme order.
    misses.remember("other");
    scratch.touch("pixmaps/other.png");
    assert!(!misses.holds("other", &order));
    misses.remember("other");
    assert!(misses.holds("other", &order));
    assert!(!misses.holds("other", &["Adwaita".to_owned(), "hicolor".to_owned()]));
}

#[test]
fn a_held_miss_is_not_searched_for_again() {
    let name = "caustics-no-theme-has-this-name-either";
    let first = Instant::now();
    assert_eq!(resolve(name, 64), None);
    let first = first.elapsed();
    let again = Instant::now();
    for _ in 0..20 {
        assert_eq!(resolve(name, 64), None);
    }
    let again = again.elapsed();
    eprintln!("first miss {first:?}; twenty more {again:?}");
    assert!(misses(|held| held.names.contains(name)));
}
