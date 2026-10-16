//! The bar's ABI: **two contracts, named apart** (docs/abi.md, the
//! normative document; this crate and the Slang it renders, `bar_abi.slang`
//! and `bar_wire.slang`, are the two halves that implement it).
//!
//! | | [`buffer`] | [`wire`] |
//! |---|---|---|
//! | parties | `caustics-bar` and `caustics/bar.slang` | `caustics-bar` and any sidecar |
//! | built | together, from one tree | separately, by anyone, at any time |
//! | stability | none needed across builds | byte-stable within one [`ABI_HASH`] |
//! | version | [`BAR_ABI_VERSION`], a courtesy | [`wire::WIRE_MAGIC`] + [`ABI_HASH`] in the `Hello` frame (docs/abi.md, *Hello*) |
//! | enforced by | one declaration ([`decl`]) plus the layout test in `bar/src/abi.rs` | [`wire::read`]: magic, kind, length, bounds, and the `Hello` hash gate |
//!
//! They are two declarations because they are two promises; both feed one [`ABI_HASH`], and an install set is built whole.
//!
//! Its own crate so a sidecar can import the layout
//! without the bar's dependencies: `punchcutter-core`'s `plain` and `decl`
//! modules and these declarations, nothing else.
//!
//! [`ENTRIES`], the caustic's entry-point names, lives here too: the compiler
//! sidecar compiles every entry in that list and names each `Module` frame
//! after one (docs/abi.md, *Module*), so it is a thing two separately built processes must
//! agree on — which is what this crate is for. The buffer's layout tests stay
//! in `bar/src/abi.rs` because the reflection test needs slangc; the wire's
//! tests are here, because the wire needs nothing but this crate.

//! # Three channels out, and one buffer this crate does not describe
//!
//! The caustic talks back through exactly three things
//! (docs/abi.md, *The action ring*–*The one word*): an **action** ring of [`Note`]s whose operands
//! the embassy forwards and never resolves, a **witness** ring of `Note`s
//! whose head is a format literal's `getStringHash` and whose words the
//! embassy prints, and [`Surface::wants_redraw`]. That is `State`, and it is
//! the only buffer read back.
//!
//! Everything the caustic builds for *itself* — the element, item and block
//! tables, the scratch, the springs, the popup's want — is in a binding this
//! crate does not describe at all: the embassy allocates it at the stride
//! slangc's reflection reports and never writes or reads a word (docs/abi.md, *The private buffer*), and
//! the vocabulary for it is `caustics/bar_model.slang`'s.
//!
//! # Declared once (docs/abi.md, *The two contracts*)
//!
//! Every struct and constant below is written through [`abi_struct!`] and
//! [`abi_consts!`], which emit the Rust item *and* a table describing it.
//! [`rendered`] renders those tables into `bar_abi.slang` and
//! `bar_wire.slang`, which are not in the tree: the bar's build script writes
//! them beside its build ([`write_rendered`]) and a process that compiles the
//! caustic at run time finds them in [`rendered_dir`]. So the Slang half is
//! never a second copy to keep in step; there is no copy at all.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod buffer;
pub mod wire;

/// **Declared once**, and declared once for two projects: the macros and the
/// renderer are `punchcutter-core`'s, because the text engine declares the
/// OpenType arena's header through the same tables.
///
/// Re-exported here — module and macros both — so every call site in this
/// crate still reads `crate::decl::…` and `crate::abi_struct!`.
pub use punchcutter_core::decl;
pub use punchcutter_core::{abi_consts, abi_struct, slang_ty};

/// The swapchain formats a caustic is baked for, and the slangc flags that
/// select each: one table for the bar's build script, the bar and the compiler
/// sidecar.
pub mod format;
pub use format::DstFormat;

/// How the rendered modules say where they came from, in their first line.
const RENDERER: &str = "`caustics_bar_abi::rendered()`";

// `pub const ABI_HASH: u64`, hashed by `build.rs` from the four inputs that
// *are* the contract — the buffer's and the wire's declarations as source, the
// text engine's arena header, and the entry list. Its doc comment is here because a build
// script cannot write one that rustdoc will keep.
//
/// **One number for the whole contract** (docs/abi.md, *Hello*).
///
/// Every binary of an install set links this crate, so every binary of the set carries the same value; `bar/install.sh` reads it out of the built bar (`caustics-bar
/// --abi-hash`) and names the set's directory after it, and a sidecar's first
/// frame carries it so a bar can refuse a child from another tree before it
/// writes a second byte.
///
/// It replaced the ten `*_VERSION` words the wire used to carry, one per frame
/// kind (docs/abi.md, *The ABI hash*). A kind's layout is the contract's, and the contract has one
/// hash: the frame that once slipped past ten correct version words was a
/// `Bytes` body of the right shape naming a buffer whose *meaning* had
/// changed, which no per-kind version could ever have caught.
///
/// `build.rs` says exactly what goes into it.
pub use hash::ABI_HASH;

mod hash {
    include!(concat!(env!("OUT_DIR"), "/abi_hash.rs"));
}

/// The sentence the rendered modules carry above their own prose.
pub(crate) const NORMATIVE: &[&str] = &[
    "docs/abi.md is the normative document and `bar/abi/` is the one",
    "declaration; this file is that declaration rendered (docs/abi.md, *The two contracts*)",
    "and is not in the tree. Edit the Rust; a build renders it again.",
];

/// The file names of the two rendered modules, as a caustic's `import`
/// resolves them: the buffer, then the wire.
pub const RENDERED: [&str; 2] = ["bar_abi.slang", "bar_wire.slang"];

/// The two rendered modules, each with the file name in [`RENDERED`]: what
/// slangc reads when the caustic says `import bar_abi;`.
pub fn rendered() -> [(&'static str, String); 2] {
    [
        (RENDERED[0], buffer::slang_module()),
        (RENDERED[1], wire::slang_module()),
    ]
}

/// Writes the two rendered modules into `dir`, creating it, so that `dir` can
/// go on slangc's include path. A build script writes them under `OUT_DIR`;
/// a process that compiles at run time calls [`rendered_dir`] instead.
pub fn write_rendered(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, text) in rendered() {
        std::fs::write(dir.join(name), text)?;
    }
    Ok(())
}

/// Whether `dir` already holds the two, byte for byte.
fn is_rendered(dir: &Path) -> bool {
    rendered().iter().all(|(name, text)| {
        std::fs::read(dir.join(name)).is_ok_and(|on_disk| on_disk == text.as_bytes())
    })
}

/// A directory holding the two rendered modules, for a process that compiles
/// the caustic at run time: pass it to slangc as an include path.
///
/// It is `$XDG_CACHE_HOME/caustics/rendered/<ABI_HASH>/` (`~/.cache` when
/// that is unset), the hash as sixteen hex digits, so each contract has its
/// own; the system temporary directory stands in when the cache is not
/// writable. Written once, atomically (into a sibling, then renamed), and
/// checked rather than rewritten when already there. Resolved once per
/// process, so a caller can hash or compare it.
///
/// When no directory can be written the last one tried comes back as it is,
/// and the compile that uses it reports the missing module, with the reason
/// on stderr here.
pub fn rendered_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let name = format!("{ABI_HASH:016x}");
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
        let roots = cache.into_iter().chain([std::env::temp_dir()]);
        let mut last = PathBuf::from(&name);
        for root in roots {
            let dir = root.join("caustics").join("rendered").join(&name);
            if is_rendered(&dir) {
                return dir;
            }
            match materialise(&dir) {
                Ok(()) => return dir,
                Err(err) => eprintln!("caustics-bar-abi: cannot write {}: {err}", dir.display()),
            }
            last = dir;
        }
        last
    })
}

/// Writes the modules into a sibling of `dir` and renames it into place, so
/// no reader ever sees a half-written directory. Two processes doing this at
/// once both succeed: the rename that loses finds the winner's copy in place.
///
/// A directory already there whose files differ (a renderer change that left
/// the hash alone, or a damaged copy) is replaced, once.
fn materialise(dir: &Path) -> io::Result<()> {
    let parent = dir
        .parent()
        .expect("a directory under a cache root has a parent");
    let leaf = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rendered");
    let staging = parent.join(format!("{leaf}.tmp-{}", std::process::id()));
    write_rendered(&staging)?;
    let mut result = std::fs::rename(&staging, dir);
    if result.is_err() && dir.is_dir() && !is_rendered(dir) {
        let stale = parent.join(format!("{leaf}.stale-{}", std::process::id()));
        if std::fs::rename(dir, &stale).is_ok() {
            let _ = std::fs::remove_dir_all(&stale);
        }
        result = std::fs::rename(&staging, dir);
    }
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            if is_rendered(dir) { Ok(()) } else { Err(err) }
        }
    }
}

#[cfg(test)]
mod tests;

// `pub const ENTRIES`, in its own file so `build.rs` can hash it as source, one
// of the four inputs the ABI hash covers.
include!("entries.rs");

/// The buffer's names, unqualified: the bar's call sites keep reading
/// `abi::Note`, `abi::ACT_SPAWN`, `abi::as_bytes`. The wire's stay behind
/// `abi::wire::` — a sidecar frame and a GPU buffer are different things, so they are not spelled the same way.
pub use buffer::*;
