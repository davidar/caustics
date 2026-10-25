//! The caustic arriving as bytes: one generation of `Module` frames collected
//! until it is whole (docs/abi.md, *Module*).
//!
//! The bar does not compile. Watching the files, compiling every entry
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
//! - it is the **caustic's entry points**, in order, by name, one each (the
//!   list the host passes to [`Modules::new`]; the bar's is the ABI crate's
//!   `ENTRIES`);
//! - nothing else. The bindings are `Gpu::reload_all`'s check and the SPIR-V
//!   is the device's.
//!
//! A generation left incomplete when a newer one starts is dropped: the
//! compiler only ever abandons a generation by being killed mid-write or by
//! recompiling on a newer save, and in both cases the older one is stale.

use punchcutter_slang::Reflection;

use caustics_bar_abi::wire::{Module, ModuleAssembly, ModuleChunk};

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

/// A whole generation: one module per entry point, in order, checked.
pub struct Ready {
    pub generation: u32,
    modules: Vec<Module>,
}

impl Ready {
    /// The SPIR-V, in the order `Gpu::reload_all` takes it, which is the
    /// entry list's order, which is dispatch order.
    pub fn spirv(&self) -> Vec<&[u8]> {
        self.modules.iter().map(|m| m.spirv.as_slice()).collect()
    }

    /// The new caustic's reflection: the bindings `Gpu::reload_all` checks
    /// and every `getStringHash` literal the caustic names.
    ///
    /// The compiler links every entry point at once and reflects the whole
    /// program, so every module carries the same reflection; each is read
    /// all the same and their literals merged, which costs nothing when they
    /// agree and keeps a generation from a writer that compiled entry points
    /// apart whole. The bindings are the first module's. An unreadable
    /// reflection anywhere fails the whole generation: a hash in the mirror
    /// is only meaningful against the caustic that produced it, and half a
    /// table would answer for strings nobody asked about.
    pub fn reflection(&self) -> Result<Reflection, String> {
        let mut merged: Option<Reflection> = None;
        for module in &self.modules {
            let reflection = Reflection::from_json(&module.reflection)
                .map_err(|err| format!("{}'s reflection cannot be read ({err})", module.name))?;
            match merged.as_mut() {
                Some(first) => first.strings.extend(reflection.strings),
                None => merged = Some(reflection),
            }
        }
        Ok(merged.unwrap_or_default())
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
pub struct Modules {
    /// The program's entry points, in dispatch order: what a generation has
    /// to consist of, by name and by index.
    entries: &'static [&'static str],
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
    /// A collector for a program of these entry points, in this order.
    pub fn new(entries: &'static [&'static str]) -> Modules {
        Modules {
            entries,
            want: None,
            generation: 0,
            have: Vec::new(),
            partial: ModuleAssembly::default(),
        }
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
                count: self.entries.len(),
            };
        };
        if module.format != want {
            return Collected::Refused(format!(
                "generation {} was compiled for another swapchain format ({:#010x}, not {want:#010x})",
                module.generation, module.format
            ));
        }
        if module.count as usize != self.entries.len() {
            return Collected::Refused(format!(
                "generation {} has {} entry points and this bar runs {}",
                module.generation,
                module.count,
                self.entries.len()
            ));
        }
        let index = module.index as usize;
        // `wire::read` already bounds `index < count`, so this only fires if
        // the two ends' entry lists disagree — a sidecar from another build.
        if self.entries.get(index) != Some(&module.name.as_str()) {
            return Collected::Refused(format!(
                "generation {}'s entry point {index} is {:?} and this bar's is {:?}",
                module.generation,
                module.name,
                self.entries.get(index).copied().unwrap_or("none")
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
        self.have = (0..self.entries.len()).map(|_| None).collect();
        let index = module.index as usize;
        self.have[index] = Some(module);
        if let Some(dropped) = dropped {
            tracing::info!(
                "modules: generation {stale} was abandoned at {dropped}/{} by generation {}",
                self.entries.len(),
                self.generation
            );
        }
        self.finish()
    }

    /// Whole yet?
    fn finish(&mut self) -> Collected {
        let have = self.have.iter().flatten().count();
        if have < self.entries.len() {
            return Collected::Waiting {
                generation: self.generation,
                have,
                count: self.entries.len(),
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
