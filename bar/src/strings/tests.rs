use super::*;

fn table(pairs: &[(u32, &str)]) -> BTreeMap<u32, String> {
    pairs
        .iter()
        .map(|(hash, literal)| (*hash, (*literal).to_owned()))
        .collect()
}

/// An empty table — what [`Strings::baked`] falls back to on a machine whose
/// baked reflection cannot be read — misses rather than inventing a literal.
#[test]
fn a_fresh_table_is_empty_and_misses() {
    let strings = Strings::new();
    assert!(strings.is_empty());
    assert_eq!(strings.len(), 0);
    assert_eq!(strings.get(1_046_322_544), None);
}

/// The reload semantics: the new caustic's literals are the only ones left.
/// An edit that renames "Close" to "Dismiss" must not leave both answerable,
/// and one that deletes a row must not leave its label behind.
#[test]
fn a_reload_replaces_the_table_whole() {
    let mut strings = Strings::new();
    strings.replace(table(&[(1, "New window"), (2, "Close")]));
    assert_eq!(strings.get(1), Some("New window"));
    assert_eq!(strings.get(2), Some("Close"));
    assert_eq!(strings.len(), 2);

    strings.replace(table(&[(1, "New window"), (3, "Bluetooth — off")]));
    assert_eq!(strings.get(1), Some("New window"));
    assert_eq!(strings.get(2), None, "the old literal is gone, not merged");
    assert_eq!(strings.get(3), Some("Bluetooth — off"));
    assert_eq!(strings.len(), 2);

    // A caustic that names nothing leaves nothing behind either.
    strings.replace(BTreeMap::new());
    assert!(strings.is_empty());
    assert_eq!(strings.get(1), None);
}

/// The table a bar actually starts on is the **baked** one, and it already
/// answers for the words the dock's menu is made of.
///
/// `build.rs` writes slangc's reflection beside the SPIR-V it compiled, so the
/// modules running on the first frame and the literals answering on the first
/// frame come from one compile. Without it the table would be empty until the
/// first reload, and a right click in that window would draw a menu with no
/// words in it.
#[test]
fn the_baked_table_already_names_the_dock_menus_words() {
    let strings = Strings::baked();
    assert!(
        !strings.is_empty(),
        "the baked reflection carries no literals at all"
    );
    for word in ["New window", "Close", "Pin", "Unpin"] {
        assert!(
            strings.table.values().any(|literal| literal == word),
            "the baked table does not name {word:?}; the dock menu would lose that row"
        );
    }
}
