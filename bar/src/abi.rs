//! The bar's side of its ABI: a re-export of `caustics-bar-abi` (the declarations
//! are their own crate so a sidecar can import the layout without the bar's
//! dependencies), the caustic's entry-point names,
//! and the **buffer's** layout tests — which stay here because the reflection
//! test needs slangc and `caustics-core/reflect`, neither of which a sidecar
//! should carry. The wire's own tests live in the crate, which needs neither.
//!
//! Everything inside the bar keeps saying `abi::Note`, `abi::ACT_SPAWN`,
//! `abi::as_bytes`: the buffer's names come through unqualified. The wire's
//! stay behind `abi::wire::` — a sidecar frame and a GPU buffer are different
//! things, and are not spelled the same way (docs/abi.md, *The two contracts*).
//!
//! What is *not* here is the caustic's own vocabulary. The item
//! kinds, the element slots, the block kinds and the `Id` that names them live in `caustics/bar_model.slang` with the tables they describe (docs/abi.md, *The private buffer*), so a
//! test that wants one reads it out of that module's text — and nothing in
//! `bar/src/` outside a test says `ITEM_`, `ELEM_` or `ID_` at all.

pub use caustics_bar_abi::wire;
pub use caustics_bar_abi::*;

#[cfg(test)]
mod tests;
