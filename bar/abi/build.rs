//! One number for the whole contract: [`ABI_HASH`], hashed here and compiled
//! into every binary that links this crate.
//!
//! The binaries of an install set are built from one tree and are one set
//! (docs/architecture.md, *The ABI*). What says so on the wire is this hash: a
//! sidecar's first frame carries it (docs/abi.md, *Hello*), and a bar whose
//! own differs kills that child and does not start it again. A per-kind version word cannot catch this: a frame can keep its shape while the meaning of a field (which buffer a `BUF_*` number names, say) changes, so the whole contract gets one hash (docs/abi.md, *The ABI hash*).
//!
//! **What is hashed**, in this order:
//!
//! 1. `caustics/bar_abi.slang` — the buffer contract, rendered (docs/abi.md, *The two contracts*);
//! 2. `caustics/bar_wire.slang` — the wire as the GPU reads it, rendered;
//! 3. `shape_abi.slang` — the OpenType arena's header, rendered by the text
//!    engine (`punchcutter-shape`) from the same declaration tables
//!    (`punchcutter_core::decl`) and read from the engine's own Slang directory
//!    (`punchcutter_core::slang_dir()`, a build-dependency). It is a wire
//!    contract too: `caustics-glyphs` fills that arena with a `Bytes` frame, so
//!    an engine whose arena changed is a new set.
//! 4. `bar/abi/src/wire.rs` — the framing, the kinds, the caps and the
//!    `BUF_*` numbers, none of which has a Slang half to be rendered into;
//! 5. `bar/abi/src/entries.rs` — the entry list a `Module` frame is named by.
//!
//! The first three are generated from the Rust and checked in, and a test in
//! this crate fails when either of the bar's two differs from the rendering
//! (punchcutter's own test does the same for the third) — so hashing the file
//! on disk is hashing the declaration. The last two are
//! hashed as *source*, comments and all, which is coarse in the safe
//! direction: a comment edit makes a new set, and a set is installed whole.

use std::path::{Path, PathBuf};

fn main() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = [
        repo.join("caustics/bar_abi.slang"),
        repo.join("caustics/bar_wire.slang"),
        punchcutter_core::slang_dir().join("shape_abi.slang"),
        repo.join("bar/abi/src/wire.rs"),
        repo.join("bar/abi/src/entries.rs"),
    ];
    println!("cargo:rerun-if-changed=build.rs");

    // FNV-1a, 64-bit: no dependency, no ordering surprise, and a value a
    // journal line can carry as sixteen hex digits.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        for byte in read(file) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("abi_hash.rs");
    std::fs::write(&out, format!("pub const ABI_HASH: u64 = {hash:#018x};\n"))
        .unwrap_or_else(|err| panic!("could not write {}: {err}", out.display()));
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| {
        panic!(
            "the ABI hash covers {}, which could not be read: {err}",
            path.display()
        )
    })
}
