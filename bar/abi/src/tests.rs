use std::path::{Path, PathBuf};

use crate::GEN_COMMAND;

/// The repository's `caustics/`, resolved against this crate so the test does
/// not care about the harness's working directory.
fn caustics_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../caustics")
}

/// The checked-in Slang is the rendering of the declarations, byte for byte.
///
/// Both files are checked in rather than generated at build time because
/// slangc resolves an import from the importing file's own directory before
/// any `-I`: `caustics-reload` and the reflection test hand it the
/// checked-out `caustics/`, and a file that only existed in `OUT_DIR` would
/// not be there. So this is the guard that the two stay the same thing.
#[test]
fn the_checked_in_slang_is_the_rendering() {
    let (abi, wire) = crate::slang_source();
    for (name, rendered) in [("bar_abi.slang", abi), ("bar_wire.slang", wire)] {
        let path = caustics_dir().join(name);
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
        if on_disk == rendered {
            continue;
        }
        let line = on_disk
            .lines()
            .zip(rendered.lines())
            .position(|(a, b)| a != b)
            .map_or_else(
                || "the end".to_string(),
                |index| format!("line {}", index + 1),
            );
        panic!(
            "{} is not what the declarations render ({line} differs). \
             It is generated: run `{GEN_COMMAND}`.",
            path.display()
        );
    }
}

/// `ABI_HASH` is a hash of something.
///
/// There is no second copy to compare it against — that is the point of it —
/// so what can be asserted is that `build.rs` read its five files and folded
/// them in: a value still at FNV's offset basis means it hashed nothing, and
/// zero means it was never written at all. The property that matters, "the
/// number moves when the contract does", is `build.rs`'s `rerun-if-changed`
/// lines and is checked by the nested gate (bar/bar-test.sh), which plays back a greeting carrying another hash and watches the bar refuse the child.
#[test]
fn the_contract_has_one_hash() {
    assert_ne!(crate::ABI_HASH, 0, "ABI_HASH was never written");
    assert_ne!(
        crate::ABI_HASH,
        0xcbf2_9ce4_8422_2325,
        "ABI_HASH hashed nothing"
    );
}

/// Every constant and struct is declared once, so a name cannot appear in both
/// modules — `bar_abi` re-exports `bar_wire`, and a duplicate would be a
/// compile error in every caustic at once.
#[test]
fn the_two_modules_declare_disjoint_names() {
    let mut names: Vec<&str> = Vec::new();
    for group in crate::buffer::CONST_GROUPS
        .iter()
        .chain(crate::wire::CONST_GROUPS)
    {
        names.extend(group.consts.iter().map(|decl| decl.name));
    }
    for entry in crate::buffer::STRUCTS.iter().chain(crate::wire::STRUCTS) {
        names.push(entry.decl.name);
    }
    let mut sorted = names.clone();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "a name is declared in both modules");
}

/// Binding numbers are never declared as constants: the bar builds its bind
/// groups from the reflection by name (docs/abi.md, *Bindings and the oracle*),
/// and a constant would be a second copy that could drift.
#[test]
fn no_binding_number_is_declared_as_a_constant() {
    let (abi, wire) = crate::slang_source();
    for gone in [
        // The bindings are read from the reflection by name (docs/abi.md, *Bindings and the oracle*).
        "SAMPLES_BINDING",
        "LITERALS_BINDING",
        "FACTS_BINDING",
    ] {
        let needle = format!(" {gone} ");
        for (name, source) in [("bar_abi", &abi), ("bar_wire", &wire)] {
            let found = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .any(|line| line.contains(&needle));
            assert!(!found, "`{gone}` is declared again in {name}");
        }
    }
}
