//! The strings the caustic owns, recovered from the caustic.
//!
//! A caustic that wants a label writes `getStringHash("Close")` and puts the
//! hash in the mirror; slangc records the literal behind every such call in its
//! reflection, and [`caustics_core::slang`] hands that table back on the same
//! compile that produced the SPIR-V. So the text of the bar's chrome lives in
//! exactly one place — the Slang source, beside the decision to show it — and
//! the embassy, which has to shape it, looks it up by hash. No list of strings
//! on the CPU, no ABI field per label, nothing to keep in agreement (the rule
//! it follows is docs/architecture.md, *Configuration is the caustic*).
//!
//! The table is whatever the *running* caustic says, so it is replaced whole on
//! every successful reload and kept untouched on a failed one: the hashes in the
//! mirror come from the modules on the GPU, and a table from a compile that
//! never reached the GPU would answer for strings nobody is asking about. A
//! missing hash is therefore `None` and never a guess.
//!
//! # It starts full
//!
//! A bar's first table is the **baked** one: `bar/build.rs` writes
//! slangc's reflection beside the SPIR-V it compiled the program from, and
//! [`Strings::baked`] parses `hashedStrings` out of it at startup. The SPIR-V
//! running on the first frame and the literals answering on the first frame
//! therefore come from the same compile, which is the property that matters —
//! and the dock's menu has its words before the first right click rather than
//! after the first reload.
//!
//! Two things read it. The **pool** (`Facts::literal_pool`) puts every
//! literal's bytes in the strings buffer so a composition on the GPU can spell
//! a word the embassy never learns the meaning of; and the
//! **witness printer** (`witness.rs`) resolves a format literal's hash here,
//! and a `{n:s}` argument's, because a caustic's journal line is a
//! `getStringHash` and ten words (docs/abi.md, *The witness ring*). The instance lives in
//! `Bar`; the `Frame::Module` arm of `wayland.rs` replaces it whole when a
//! generation reaches the GPU.

use std::collections::BTreeMap;

/// Every literal the running caustic names, by `getStringHash` value.
#[derive(Debug, Default)]
pub struct Strings {
    table: BTreeMap<u32, String>,
}

impl Strings {
    /// An empty table: what a bar has if the baked reflection cannot be
    /// understood, and what the tests start from.
    pub fn new() -> Strings {
        Strings::default()
    }

    /// The table the *baked* SPIR-V was compiled with.
    ///
    /// `bar/build.rs` writes slangc's `-reflection-json` output into `OUT_DIR`
    /// from the first compile of the program, so this is the literal table of
    /// the very modules the first frame runs — no slangc at startup, and no
    /// window in which the bar knows a hash but not its text.
    ///
    /// Reflection that cannot be parsed is one witness and an empty table. A
    /// bar whose dock menu has no rows in it is a poor bar; a bar that refuses
    /// to start is not a bar at all.
    pub fn baked() -> Strings {
        const REFLECTION: &str = include_str!(concat!(env!("OUT_DIR"), "/bar.reflection.json"));
        match caustics_core::slang::hashed_strings(REFLECTION) {
            Ok(table) => {
                let strings = Strings { table };
                tracing::info!("strings: {} literals (baked)", strings.len());
                strings
            }
            Err(err) => {
                tracing::warn!("strings: the baked reflection is unreadable: {err}");
                Strings::new()
            }
        }
    }

    /// The literal behind a hash, or `None` when the running caustic does not
    /// name it.
    ///
    /// The witness printer asks, once per record it prints: a caustic's
    /// journal line is a format literal's hash and up to ten words, and a
    /// `{n:s}` argument is a hash too (docs/abi.md, *The witness ring*).
    ///
    /// `None` is a real answer: a hash from a caustic that has since been
    /// swapped, or a mirror word the CPU misread, must not become a label.
    pub fn get(&self, hash: u32) -> Option<&str> {
        self.table.get(&hash).map(String::as_str)
    }

    /// How many literals the running caustic names.
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Every literal, by hash, in hash order.
    ///
    /// The whole table, because the *pool* is the whole table: on every content
    /// rebuild the embassy appends every literal's bytes to the strings buffer
    /// and publishes an index beside them, so a composition running on the GPU
    /// can spell a word by naming its hash.
    /// Hash order rather than any other so that two builds of the same table
    /// lay it out the same way, which makes a strings buffer diffable.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &str)> + '_ {
        self.table.iter().map(|(hash, text)| (*hash, text.as_str()))
    }

    /// Whether the running caustic names no literals at all. Only the tests
    /// ask — every caller in the bar wants a specific hash, and gets `None`.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Swap in the table from a reload that reached the GPU.
    ///
    /// Whole, not merged: a literal deleted from the caustic has to disappear
    /// from here too, or a stale label outlives the code that drew it.
    pub fn replace(&mut self, table: BTreeMap<u32, String>) {
        self.table = table;
        tracing::info!("strings: {} literals", self.table.len());
    }
}

#[cfg(test)]
mod tests;
