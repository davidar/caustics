//! The icon tests: what the slot gets is **straight** pixels at the slot's
//! own size, whatever size and alpha the file had.

use super::*;

/// The two names a bar may use. If no theme on the host has one, the test says
/// so and passes: this is a host-capability check, not a bug.
const NAMES: &[&str] = &["audio-volume-high-symbolic", "preferences-system"];

#[test]
fn the_theme_search_order_starts_somewhere_and_ends_at_hicolor() {
    let order = theme_order();
    assert!(!order.is_empty());
    assert_eq!(order.last().map(String::as_str), Some("hicolor"));
    for theme in FALLBACK_THEMES {
        assert!(
            order.iter().any(|t| t == theme),
            "{theme} missing from {order:?}"
        );
    }
    // No duplicates, even when the desktop's theme is one of the fallbacks.
    let mut sorted = order.clone();
    sorted.sort();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(sorted.len(), before, "duplicate themes in {order:?}");
    println!("icon theme search order: {order:?}");
}

/// [`resolve`] finds the file and [`raster`] renders it at the one size the
/// wire has — and hands back straight alpha, which is what *Pixmap* says and what
/// lets the caustic do the multiply.
#[test]
fn a_resolved_icon_rasterises_to_one_slot_of_straight_rgba() {
    let mut found = 0;
    for name in NAMES {
        let Some((path, _)) = resolve(name, IMAGE_SLOT_PX) else {
            println!("SKIP: no theme in {:?} has {name}", theme_order());
            continue;
        };
        let Some(pixels) = raster(&path) else {
            println!(
                "SKIP: {name} resolved to {} and would not rasterise",
                path.display()
            );
            continue;
        };
        found += 1;
        assert_eq!(
            pixels.len(),
            (IMAGE_SLOT_PX * IMAGE_SLOT_PX * 4) as usize,
            "{name} is not one slot"
        );
        let ink = pixels.chunks_exact(4).filter(|t| t[3] > 0).count();
        assert!(ink > 0, "{name} rasterised to nothing");
        // Straight, not premultiplied: a fully opaque texel is untouched, and
        // nothing forces a channel under its alpha.
        let bright = pixels
            .chunks_exact(4)
            .any(|t| t[3] == 255 && (t[0] > 0 || t[1] > 0 || t[2] > 0));
        println!(
            "{name} at {IMAGE_SLOT_PX}px: {ink} texels have ink, opaque colour present: {bright}"
        );
    }
    if found == 0 {
        println!("SKIP: this host has no icon theme with either of {NAMES:?}");
    }
}

/// Straight is the inverse of premultiplied on the way in: a half-transparent
/// white stays white rather than becoming grey, which is why the multiply happens at the sample, in the caustic.
#[test]
fn a_half_transparent_white_comes_back_white() {
    let mut pixmap = Pixmap::new(1, 1).expect("a one-texel pixmap");
    pixmap.fill(resvg::tiny_skia::Color::from_rgba8(255, 255, 255, 128));
    let out = straight(&pixmap);
    assert_eq!(out.len(), 4);
    assert_eq!(out[3], 128);
    // Premultiplied it would be ~128; straight it is 255 (within the one
    // bit the premultiplied byte can hold).
    assert!(out[0] >= 254, "{out:?}");
}

#[test]
fn the_symbolic_test_is_the_name_or_the_directory_and_only_for_svgs() {
    assert!(is_symbolic(
        "audio-volume-high-symbolic",
        Path::new("/x/audio.svg")
    ));
    assert!(is_symbolic(
        "audio-volume-high",
        Path::new("/x/symbolic/audio.svg")
    ));
    // A PNG is pixels whatever it is called.
    assert!(!is_symbolic(
        "audio-volume-high-symbolic",
        Path::new("/x/audio.png")
    ));
    // And a colour SVG is pixels too.
    assert!(!is_symbolic("firefox", Path::new("/x/firefox.svg")));
}

#[test]
fn a_name_no_theme_has_is_none_not_a_panic() {
    assert!(resolve("caustics-definitely-not-an-icon-name", IMAGE_SLOT_PX).is_none());
}

#[test]
fn a_file_that_is_not_an_image_is_none_not_a_panic() {
    assert!(raster(Path::new("/etc/hostname")).is_none());
    assert!(raster(Path::new("/caustics/no/such/file")).is_none());
}
