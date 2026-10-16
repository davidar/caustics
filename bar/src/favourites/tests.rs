use super::*;

/// A directory of this test's own, removed at the end. No crate for it and
/// none wanted: the pid and the tag are enough to make the name unique.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!(
            "caustics-bar-favourites-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Temp(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join("caustics").join("bar").join("favourites")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_missing_file_seeds_from_the_built_in_list() {
    let temp = Temp::new("missing");
    assert_eq!(load(&temp.file()), seed());
    assert!(!temp.file().exists(), "reading must not create it");
}

#[test]
fn store_then_load_is_the_same_order() {
    let temp = Temp::new("roundtrip");
    let want: Vec<String> = ["kitty", "org.mozilla.firefox", "com.system76.CosmicTerm"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    store(&temp.file(), &want);
    assert_eq!(load(&temp.file()), want, "the file is ordered, not a set");
    // And writing again over a file that exists is the same answer: the
    // rename replaces, it does not append.
    store(&temp.file(), &want[..1]);
    assert_eq!(load(&temp.file()), want[..1]);
    // Nothing is left beside it.
    assert!(
        !temp.file().with_extension("new").exists(),
        "the temp file was not cleaned up"
    );
}

#[test]
fn blank_lines_are_tolerated_and_a_line_with_whitespace_is_skipped() {
    let temp = Temp::new("malformed");
    let file = temp.file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "kitty\n\n  \nnot an app id\norg.mozilla.firefox\n\t\nfoo bar\nlast\n",
    )
    .unwrap();
    assert_eq!(
        load(&file),
        vec![
            "kitty".to_owned(),
            "org.mozilla.firefox".to_owned(),
            "last".to_owned()
        ],
        "the two malformed lines go and the rest survive in order"
    );
}

#[test]
fn an_empty_file_is_an_empty_dock_and_not_the_seed() {
    // A person who unpinned everything meant it. Only a *missing* file is
    // the seed, which is why `store` of an empty list is not the same as
    // never having written one.
    let temp = Temp::new("empty");
    store(&temp.file(), &[]);
    assert_eq!(load(&temp.file()), Vec::<String>::new());
}

#[test]
fn the_path_is_under_the_state_dir() {
    let path = path();
    assert!(path.is_absolute(), "{} is not absolute", path.display());
    assert!(
        path.ends_with("caustics/bar/favourites"),
        "{}",
        path.display()
    );
}
