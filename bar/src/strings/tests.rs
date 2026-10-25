use super::*;

/// The table a bar actually starts on is the **baked** one, and it already
/// answers for the words the dock's menu is made of.
///
/// `build.rs` writes Slang's reflection beside the SPIR-V it compiled, so the
/// modules running on the first frame and the literals answering on the first
/// frame come from one compile. Without it the table would be empty until the
/// first reload, and a right click in that window would draw a menu with no
/// words in it.
#[test]
fn the_baked_table_already_names_the_dock_menus_words() {
    let strings = baked();
    assert!(
        !strings.is_empty(),
        "the baked reflection carries no literals at all"
    );
    for word in ["New window", "Close", "Pin", "Unpin"] {
        assert!(
            strings.iter().any(|(_, literal)| literal == word),
            "the baked table does not name {word:?}; the dock menu would lose that row"
        );
    }
}
