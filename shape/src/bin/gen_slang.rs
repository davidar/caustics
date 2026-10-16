//! Writes `caustics/shape_abi.slang` from the declarations in
//! `shape/src/abi.rs` (docs/abi.md, *The two contracts* for the pattern).
//!
//! The file is checked in — `caustics-reload` and the harness run slangc against
//! the checked-out `caustics/`, and slangc resolves an import from the
//! importing file's own directory before any `-I` — so this is
//! run by hand after a declaration changes. A test in the crate fails when the
//! file on disk differs from the rendering, and names this command.

use std::path::{Path, PathBuf};

fn main() {
    let path = caustics_dir().join("shape_abi.slang");
    let source = caustics_shape::abi::slang_module();
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

/// The repository's `caustics/`, resolved against this crate so the command
/// does not care about the working directory.
fn caustics_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../caustics")
}
