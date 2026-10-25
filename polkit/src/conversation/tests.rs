use super::*;

#[test]
fn a_long_title_is_cut_at_a_character_boundary() {
    assert_eq!(cut("abc", 5), "abc");
    assert_eq!(cut("abcdef", 3), "abc");
    // "é" is two bytes; a cut through it backs off to before it.
    assert_eq!(cut("aé", 2), "a");
    let long = "é".repeat(MAX_NAME_LEN);
    assert!(cut(&long, MAX_NAME_LEN).len() <= MAX_NAME_LEN);
}
