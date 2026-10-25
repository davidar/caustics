use super::*;

/// One question per distinct string, nothing for an empty one, and the
/// sidecar and kind the host named on every request.
#[test]
fn a_string_is_asked_about_once() {
    let mut covers = Covers::new("glyphs", wire::REQ_COVER);
    assert_eq!(
        covers.ask("日本語"),
        Some(("glyphs", wire::REQ_COVER, "日本語".to_owned()))
    );
    assert_eq!(covers.ask("日本語"), None, "asked before");
    assert_eq!(covers.ask(""), None, "an empty run is nothing to cover");
    assert_eq!(covers.len(), 1);
}

/// A `Font` frame is a new glyph space: what was covered in the old one is
/// asked again.
#[test]
fn a_new_font_forgets_what_was_covered() {
    let mut covers = Covers::new("glyphs", wire::REQ_COVER);
    assert!(covers.ask("ab").is_some());
    covers.forget();
    assert!(covers.is_empty());
    assert!(covers.ask("ab").is_some());
}

/// A string past the wire's name bound loses its tail, at a character
/// boundary, and nothing else; a thousand distinct strings is the bound.
#[test]
fn a_long_string_is_cut_and_the_set_is_bounded() {
    let mut covers = Covers::new("glyphs", wire::REQ_COVER);
    let long = "é".repeat(wire::MAX_NAME_LEN);
    let (_, _, sent) = covers.ask(&long).expect("asked");
    assert!(sent.len() <= wire::MAX_NAME_LEN);
    assert_eq!(sent.len() % 2, 0, "cut between two-byte characters");
    for at in 1..MAX_COVERED {
        assert!(covers.ask(&format!("run-{at}")).is_some());
    }
    assert_eq!(covers.ask("one more"), None);
    assert_eq!(covers.len(), MAX_COVERED);
}
