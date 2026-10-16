use super::*;

/// A scratch directory of our own, removed at the end of the test.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("caustics-watch-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Filesystem mtimes have coarse resolution on some filesystems, so a
/// rewrite is dated explicitly rather than trusted to differ.
fn touch(path: &Path, contents: &str, secs_from_now: u64) {
    std::fs::write(path, contents).expect("the temp dir is writable");
    let when = SystemTime::now() + Duration::from_secs(secs_from_now);
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("just written");
    file.set_modified(when).expect("mtimes are settable");
}

#[test]
fn a_fresh_watch_has_nothing_to_report() {
    let dir = Dir::new("fresh");
    let path = dir.join("caustic.slang");
    touch(&path, "one", 0);
    let mut watch = MtimeWatch::new(vec![path]);
    assert!(!watch.changed(), "the state at startup is not a change");
    assert!(!watch.changed());
}

#[test]
fn a_save_is_a_change_exactly_once() {
    let dir = Dir::new("save");
    let path = dir.join("caustic.slang");
    touch(&path, "one", 0);
    let mut watch = MtimeWatch::new(vec![path.clone()]);
    touch(&path, "two", 10);
    assert!(watch.changed(), "a newer mtime is a change");
    assert!(!watch.changed(), "and only the first poll sees it");
}

#[test]
fn a_file_that_vanished_mid_save_is_not_a_change_until_it_returns() {
    let dir = Dir::new("rename");
    let path = dir.join("caustic.slang");
    touch(&path, "one", 0);
    let mut watch = MtimeWatch::new(vec![path.clone()]);

    // The window between the editor's unlink and its rename.
    std::fs::remove_file(&path).expect("it is there");
    assert!(
        !watch.changed(),
        "a missing file is not something to compile"
    );

    touch(&path, "two", 10);
    assert!(watch.changed(), "the replacement landing is the change");
}

#[test]
fn any_watched_file_counts() {
    let dir = Dir::new("modules");
    let caustic = dir.join("caustic.slang");
    let module = dir.join("module.slang");
    touch(&caustic, "one", 0);
    touch(&module, "one", 0);
    let mut watch = MtimeWatch::new(vec![caustic, module.clone()]);
    assert_eq!(watch.paths().len(), 2);
    touch(&module, "two", 10);
    assert!(watch.changed(), "editing an imported module reloads too");
}
