//! The caustic arriving as bytes: one generation of `Module` frames collected
//! until it is whole (docs/abi.md, *Module*).
//!
//! The bar does not run slangc. Watching the files, compiling every entry
//! point and reporting the diagnostics is compiler driving, which does not
//! belong in a process whose job is to hold a Wayland surface; it is
//! `caustics-reload`'s.
//! What is left here is the shape of every sidecar's input: **frames in,
//! bytes to the device**.
//!
//! A generation is `count` frames carrying one entry point each, and it means
//! nothing until all of them have arrived — `Gpu::reload_all` is all-or-
//! nothing and a half-swapped program is a dispatch against pipelines from two
//! compiles. So this collects, checks the few things the bar can check without
//! reading a byte of SPIR-V, and hands the whole generation over at once:
//!
//! - it is for **this bar's swapchain format** (the format word against the
//!   one the device negotiated; SPIR-V bakes the storage-image format in);
//! - it is the **caustic's entry points**, in order, by name, one each
//!   ([`ENTRIES`]);
//! - nothing else. The bindings are `Gpu::reload_all`'s check and the SPIR-V
//!   is the device's.
//!
//! A generation left incomplete when a newer one starts is dropped: the
//! compiler only ever abandons a generation by being killed mid-write or by
//! recompiling on a newer save, and in both cases the older one is stale.

use std::collections::BTreeMap;

use crate::abi::ENTRIES;
use crate::abi::wire::{Module, ModuleAssembly, ModuleChunk};

/// What one `Module` frame did.
pub enum Collected {
    /// Filed; the rest of its generation is still owed.
    Waiting {
        generation: u32,
        have: usize,
        count: usize,
    },
    /// The generation is whole and every check passed.
    Ready(Ready),
    /// Not a generation this bar can run, and why — one witness, and the
    /// caustic on the GPU stays. Never fatal: the compiler is a separate
    /// process and may be a different build entirely.
    Refused(String),
}

/// A whole generation: [`ENTRIES`]`.len()` modules, in order, checked.
pub struct Ready {
    pub generation: u32,
    modules: Vec<Module>,
}

impl Ready {
    /// The SPIR-V, in the order `Gpu::reload_all` takes it — which is
    /// [`ENTRIES`]' order, which is dispatch order.
    pub fn spirv(&self) -> [&[u8]; ENTRIES.len()] {
        let slices: Vec<&[u8]> = self.modules.iter().map(|m| m.spirv.as_slice()).collect();
        slices
            .try_into()
            .expect("a ready generation is one module per entry point")
    }

    /// The reflection the bindings are checked against. Any of the modules'
    /// would do — the *parameters* in slangc's reflection are the program's,
    /// not an entry point's — so the first one is taken and the rest are read
    /// only for their literals.
    pub fn reflection(&self) -> &str {
        &self.modules[0].reflection
    }

    /// Every `getStringHash` literal the new caustic names, merged.
    ///
    /// Reflection reports only what an entry point *reaches*, so the caustic's
    /// table is the union over all of them. An unreadable reflection fails the
    /// whole generation:
    /// a hash in the mirror is only meaningful against the caustic that
    /// produced it, and half a table would answer for strings nobody asked
    /// about.
    pub fn strings(&self) -> Result<BTreeMap<u32, String>, String> {
        let mut table = BTreeMap::new();
        for module in &self.modules {
            match caustics_core::slang::hashed_strings(&module.reflection) {
                Ok(strings) => table.extend(strings),
                Err(err) => {
                    return Err(format!(
                        "{}'s reflection cannot be read ({err})",
                        module.name
                    ));
                }
            }
        }
        Ok(table)
    }

    /// SPIR-V and reflection together, for the witness line.
    pub fn bytes(&self) -> usize {
        self.modules
            .iter()
            .map(|m| m.spirv.len() + m.reflection.len())
            .sum()
    }

    pub fn len(&self) -> usize {
        self.modules.len()
    }
}

/// The generation being collected, and the program it has to be for.
#[derive(Default)]
pub struct Modules {
    /// `wire::format_code` of the define the device's swapchain format
    /// selects. `None` until the device is up, which is also until the bar has
    /// asked for a program at all.
    want: Option<u32>,
    /// The generation the slots below belong to. 0 is "none yet", which is why
    /// the compiler never sends it.
    generation: u32,
    /// One slot per entry point, filled by index.
    have: Vec<Option<Module>>,
    /// The entry point being reassembled, if a module is arriving in pieces.
    /// The rule is the wire's ([`ModuleAssembly`]); this holds one because a
    /// writer sends one module's pieces before it starts the next.
    partial: ModuleAssembly,
}

impl Modules {
    pub fn new() -> Modules {
        Modules::default()
    }

    /// The program this bar can run, learned when the device comes up. Until
    /// this is set nothing can be collected, which is also when the compiler
    /// is told the format and starts compiling at all.
    pub fn expect(&mut self, format: u32) {
        self.want = Some(format);
    }

    /// File one frame: a piece of an entry point, or the whole of one.
    ///
    /// Three checks — the format, the entry count, the name at the index —
    /// and they are made on the *whole*
    /// module, because a piece carries no name until its first bytes have
    /// landed. Reassembly is [`ModuleAssembly`]'s: this decides nothing about
    /// where a byte goes, it only says when a generation may be swapped.
    pub fn collect(&mut self, chunk: ModuleChunk) -> Collected {
        let Some(want) = self.want else {
            return Collected::Refused(format!(
                "a module for generation {} arrived before the device had a format",
                chunk.generation
            ));
        };
        let generation = chunk.generation;
        let Some(module) = self.partial.push(chunk) else {
            // A piece landed and the entry point is not whole yet. The
            // generation's own count has not moved.
            return Collected::Waiting {
                generation,
                have: self.have.iter().flatten().count(),
                count: ENTRIES.len(),
            };
        };
        if module.format != want {
            return Collected::Refused(format!(
                "generation {} was compiled for another swapchain format ({:#010x}, not {want:#010x})",
                module.generation, module.format
            ));
        }
        if module.count as usize != ENTRIES.len() {
            return Collected::Refused(format!(
                "generation {} has {} entry points and this bar runs {}",
                module.generation,
                module.count,
                ENTRIES.len()
            ));
        }
        let index = module.index as usize;
        // `wire::read` already bounds `index < count`, so this only fires if
        // the two ends' entry lists disagree — a sidecar from another build.
        if ENTRIES.get(index) != Some(&module.name.as_str()) {
            return Collected::Refused(format!(
                "generation {}'s entry point {index} is {:?} and this bar's is {:?}",
                module.generation,
                module.name,
                ENTRIES.get(index).copied().unwrap_or("none")
            ));
        }

        if module.generation != self.generation {
            let dropped = self.have.iter().flatten().count();
            if dropped > 0 {
                return self.start(module, Some(dropped));
            }
            return self.start(module, None);
        }
        self.have[index] = Some(module);
        self.finish()
    }

    /// Begin a generation, dropping whatever of the last one never completed.
    fn start(&mut self, module: Module, dropped: Option<usize>) -> Collected {
        let stale = self.generation;
        self.generation = module.generation;
        self.have = (0..ENTRIES.len()).map(|_| None).collect();
        let index = module.index as usize;
        self.have[index] = Some(module);
        if let Some(dropped) = dropped {
            tracing::info!(
                "modules: generation {stale} was abandoned at {dropped}/{} by generation {}",
                ENTRIES.len(),
                self.generation
            );
        }
        self.finish()
    }

    /// Whole yet?
    fn finish(&mut self) -> Collected {
        let have = self.have.iter().flatten().count();
        if have < ENTRIES.len() {
            return Collected::Waiting {
                generation: self.generation,
                have,
                count: ENTRIES.len(),
            };
        }
        let modules: Vec<Module> = std::mem::take(&mut self.have)
            .into_iter()
            .flatten()
            .collect();
        let generation = self.generation;
        // The slots are emptied by the take, and the generation with them: the
        // next frame starts a new one rather than re-completing this.
        self.generation = 0;
        Collected::Ready(Ready {
            generation,
            modules,
        })
    }
}

#[cfg(test)]
mod tests;
