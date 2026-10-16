//! Writes `caustics/bar_abi.slang` and `caustics/bar_wire.slang` from the
//! declarations in `bar/abi/src/{buffer,wire}.rs` (docs/abi.md, *The two contracts*).
//!
//! Both files are checked in — `caustics-reload` and the reflection test run
//! slangc against the checked-out `caustics/`, and slangc resolves an import
//! from the importing file's own directory before any `-I` — so this is run by hand after a declaration changes. A test in the crate
//! fails when either file on disk differs from the rendering, and names this
//! command.

use std::path::{Path, PathBuf};

fn main() {
    let caustics = caustics_dir();
    let (abi, wire) = caustics_bar_abi::slang_source();
    for (name, source) in [("bar_abi.slang", abi), ("bar_wire.slang", wire)] {
        let path = caustics.join(name);
        let changed = std::fs::read_to_string(&path)
            .map(|old| old != source)
            .unwrap_or(true);
        std::fs::write(&path, &source)
            .unwrap_or_else(|err| panic!("could not write {}: {err}", path.display()));
        println!(
            "{} {}",
            if changed { "wrote  " } else { "current" },
            path.display()
        );
    }
}

/// The repository's `caustics/`, resolved against this crate so the command
/// does not care about the working directory.
fn caustics_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../caustics")
}
