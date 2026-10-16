//! The bar's ABI: **two contracts, named apart** (docs/abi.md, the
//! normative document; this crate and `caustics/bar_abi.slang` are the two
//! halves that implement it).
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
//! [`slang_source`] renders those tables into `caustics/bar_abi.slang` and
//! `caustics/bar_wire.slang`, which are checked in and written by
//! `cargo run -p caustics-bar-abi --bin gen-slang`; a test in this crate fails
//! when either file on disk differs from the rendering. So the Slang half is never a second copy to keep in step.

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

/// How the generated files say where they came from. The test that compares
/// the checked-in file with the rendering names the same command.
pub const GEN_COMMAND: &str = "cargo run -p caustics-bar-abi --bin gen-slang";

// `pub const ABI_HASH: u64`, hashed by `build.rs` from the five files that
// *are* the contract — the three rendered Slang modules, the wire's own
// declaration, and the entry list. Its doc comment is here because a build
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

/// The sentence the generated modules carry above their own prose.
pub(crate) const NORMATIVE: &[&str] = &[
    "docs/abi.md is the normative document and `bar/abi/` is the one",
    "declaration; this file is that declaration rendered (docs/abi.md, *The two contracts*). Edit the Rust",
    "and re-run the command above.",
];

/// The two generated modules, as `caustics/bar_abi.slang` and
/// `caustics/bar_wire.slang` should read.
pub fn slang_source() -> (String, String) {
    (buffer::slang_module(), wire::slang_module())
}

#[cfg(test)]
mod tests;

// `pub const ENTRIES`, in its own file so `build.rs` can hash it as source, one
// of the five files the ABI hash covers.
include!("entries.rs");

/// The buffer's names, unqualified: the bar's call sites keep reading
/// `abi::Note`, `abi::ACT_SPAWN`, `abi::as_bytes`. The wire's stay behind
/// `abi::wire::` — a sidecar frame and a GPU buffer are different things, so they are not spelled the same way.
pub use buffer::*;
