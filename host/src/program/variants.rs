//! An entry point built as several pipelines, one per value of a
//! specialisation constant, and the slot table that names every pipeline a
//! program is.
//!
//! A [`Program`](super::Program) keeps one slot per pipeline. An entry built
//! once is one slot; an entry listed in [`Specialised`] is one slot per value,
//! each built from the entry's module with the constant set to that value
//! ([`crate::spirv::specialise`]). With nothing specialised the table is the
//! entry list itself, slot `n` being entry `n`, which is what every program
//! that never asks for a variant sees.
//!
//! The rest of `Program` reads this table through [`Slots`] alone, so the
//! feature leaves by deleting this module and `crate::spirv` and indexing the
//! program's slots by entry again (docs/architecture.md, *The caustic*).

use anyhow::{Context, Result, bail};

/// One value an entry is built for, and its name in the witness lines
/// (`pipeline canvas_draw[lock]: built in …`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Variant {
    pub value: u32,
    pub name: &'static str,
}

/// An entry built once per value of the constant whose `SpecId` is `id`, in
/// the order `values` lists them, which is the order they are built in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Specialised {
    pub entry: usize,
    pub id: u32,
    pub values: &'static [Variant],
}

/// One pipeline: its entry, the constant and value it is built with if any,
/// and its name for the witness lines.
struct Key {
    entry: usize,
    constant: Option<(u32, u32)>,
    name: String,
}

/// Every pipeline a program is, entry by entry and value by value.
pub(crate) struct Slots {
    keys: Vec<Key>,
}

impl Slots {
    /// The slots of `names` (the entries' names, in order) with `specialised`
    /// built per value. An entry specialised twice, or one past the list, is
    /// a host's mistake and refused.
    pub(crate) fn new(names: &[&'static str], specialised: &[Specialised]) -> Result<Slots> {
        for (n, one) in specialised.iter().enumerate() {
            if one.entry >= names.len() {
                bail!(
                    "entry {} is specialised and there are {}",
                    one.entry,
                    names.len()
                );
            }
            if specialised[..n]
                .iter()
                .any(|other| other.entry == one.entry)
            {
                bail!("{} is specialised twice", names[one.entry]);
            }
            if one.values.is_empty() {
                bail!("{} is specialised for no value", names[one.entry]);
            }
        }
        let mut keys = Vec::new();
        for (entry, name) in names.iter().enumerate() {
            match specialised.iter().find(|one| one.entry == entry) {
                None => keys.push(Key {
                    entry,
                    constant: None,
                    name: (*name).to_owned(),
                }),
                Some(one) => keys.extend(one.values.iter().map(|variant| Key {
                    entry,
                    constant: Some((one.id, variant.value)),
                    name: format!("{name}[{}]", variant.name),
                })),
            }
        }
        Ok(Slots { keys })
    }

    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    /// The entry slot `slot` is a pipeline of.
    pub(crate) fn entry(&self, slot: usize) -> usize {
        self.keys[slot].entry
    }

    /// `canvas_draw`, or `canvas_draw[lock]`.
    pub(crate) fn name(&self, slot: usize) -> &str {
        &self.keys[slot].name
    }

    /// The slot of `entry` built once (`value` `None`), or built for `value`.
    pub(crate) fn find(&self, entry: usize, value: Option<u32>) -> Option<usize> {
        self.keys
            .iter()
            .position(|key| key.entry == entry && key.constant.map(|(_, v)| v) == value)
    }

    /// The order of need over entries, as slots: each entry's slots in the
    /// order its values are listed.
    pub(crate) fn order(&self, need: &[usize]) -> Vec<usize> {
        need.iter()
            .flat_map(|&entry| {
                (0..self.keys.len()).filter(move |&slot| self.keys[slot].entry == entry)
            })
            .collect()
    }

    /// The module slot `slot` is built from: the entry's own, or a copy with
    /// its constant set.
    pub(crate) fn module(&self, slot: usize, spirv: &[u8]) -> Result<Vec<u8>> {
        let key = &self.keys[slot];
        let Some((id, value)) = key.constant else {
            return Ok(spirv.to_vec());
        };
        crate::spirv::specialise(spirv, id, value)
            .with_context(|| key.name.clone())?
            .with_context(|| format!("{}: the module declares no SpecId {id}", key.name))
    }
}

#[cfg(test)]
mod tests;
