//! The bar's literal table: [`caustics_host::strings::Strings`], started from
//! the reflection `bar/build.rs` baked beside this bar's SPIR-V.
//!
//! The table and its rules are the crate's (the running caustic's literals,
//! replaced whole on a reload that reached the GPU). What is the bar's is which
//! reflection it starts from, and the test that the words its dock menu is made
//! of are already in it.

pub use caustics_host::strings::Strings;

/// The table the *baked* SPIR-V was compiled with.
pub fn baked() -> Strings {
    Strings::baked(crate::gpu::BAKED_REFLECTION)
}

#[cfg(test)]
mod tests;
