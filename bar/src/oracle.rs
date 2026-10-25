//! The oracle: what `bar_build` builds, checked against what only the buffers
//! can be asked (docs/abi.md, *Bindings and the oracle*).
//!
//! There is no reference builder to compare against: a reference nobody maintains becomes a liar the first time the caustic's table changes on purpose. The oracle checks what the buffers themselves can answer.
//!
//! # It is a GPU pass
//!
//! The tables it checks live in the caustic's own buffer, which the embassy
//! allocates and never reads (docs/abi.md, *The private buffer*) — so the walk runs where the tables are. `caustics/bar_check.slang` is a test-only eighth entry point,
//! `#include`d into `bar.slang` under the define `CAUSTICS_BAR_CHECK` and compiled by this
//! harness alone; it pushes `check:` witnesses into the ring the embassy
//! already drains, and what is left on this side is:
//!
//! * the **corpus**, the scenarios `corpus()` builds, each run through the production path:
//!   `Facts::compose`, `Gpu::upload_facts`, the build/compose/shape
//!   submission;
//! * the two checks that must not be the caustic checking itself — every
//!   `check: block`'s px against this file's own text parse of `TEXT_PX` and
//!   `ICON_LOGICAL`, and every `check: item` line rendering through
//!   `witness.rs` with no `?`, which checks that every built id can be named.
//!
//! # Why it is a gated `#[test]` and not an example
//!
//! `caustics-bar` is a binary-only crate. There is no `lib.rs`, so an example is a
//! separate crate that could reach neither `Gpu::headless` nor `Facts`;
//! adding a `lib.rs` to expose them is a real refactor of a binary whose whole
//! point is that it is one program. So the harness lives inside the crate, and
//! the one test it holds returns early unless `CAUSTICS_BAR_ORACLE=1` is set.
//!
//! The gate is what makes it safe beside the rest of the suite: `cargo test
//! --workspace` never creates a Vulkan instance, never compiles the oracle's entry point, and never
//! puts a blocking device poll beside `feeders::pipe::tests`, which time
//! child processes and fail when something parallel eats their slack. A
//! machine with only an
//! NVIDIA card runs the suite green and simply never runs this.
//!
//! ```text
//! CAUSTICS_BAR_ORACLE=1 cargo test -p caustics-bar --offline oracle -- --nocapture
//! ```
//!
//! On a machine with a discrete GPU, add the integrated GPU's driver as
//! `VK_DRIVER_FILES=/usr/share/vulkan/icd.d/radeon_icd.x86_64.json` (or
//! `intel_icd`), the same pin `bar/systemd/dropins/igpu-only.conf` gives the
//! unit, so the test never opens the discrete one.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::abi::{self, State};
use crate::facts::Facts;
use crate::gpu::Gpu;
use crate::model::Model;
use crate::strings::Strings;
use caustics_host::images::Images;

/// One scenario as the corpus takes it: a model, the facts the CPU would
/// upload for it, and the menu the caustic would be holding.
pub struct Scenario {
    pub name: &'static str,
    pub model: Model,
    /// The two words the caustic keeps as its menu level, or `(0, 0)` — which
    /// is all a scenario has to say about an open menu, because `bar_build`
    /// decodes the level's tag and publishes the rows itself.
    pub level: (u32, u32),
    /// Whether this scenario is about a **cap** rather than a table.
    ///
    /// A capped scenario is checked for what a cap promises — the counts
    /// stay inside the caustic's own bounds and the drop is *reported* rather
    /// than panicked over — and its description lines may not all fit the
    /// witness ring, which is not a complaint about the table.
    pub capped: bool,
}

/// The device the scenarios run on, brought up once, with `bar_check` loaded.
pub struct Oracle {
    pub gpu: Gpu,
    /// The literal table of the compile `bar_check` came from — this build's
    /// own, not the baked one, because `bar_check.slang`'s format literals are
    /// in no baked module.
    pub strings: Strings,
}

/// Where `caustics/` is, for the one compile this harness runs.
///
/// `CAUSTICS_DIR` first, which bar/bar-test.sh and bar/targets/nested.sh set; then `~/caustics-bar/caustics`, where the VM target copies the tree, because there this binary runs from `$HOME` without the source tree; then the source tree this binary was built from.
fn caustics_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CAUSTICS_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let guest = PathBuf::from(home).join("caustics-bar/caustics");
        if guest.join("bar_check.slang").is_file() {
            return guest;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../caustics")
}

impl Oracle {
    pub fn new() -> Result<Oracle> {
        let mut gpu = Gpu::headless(640, 32).context("no headless device for the oracle")?;
        let dir = caustics_dir();
        let source = dir.join("bar.slang");
        let (define, image_format_unknown) = gpu.dst_format_args();
        // The same compiler the bar's build script and `caustics-reload` use,
        // with one define more and the same search path: the caustic's
        // directory, then the bar's rendered contract, then the text engine's
        // modules (always on the path). Every other entry point comes out
        // identical to the baked one — `bar_check` is a file appended under an
        // `#ifdef` — so only this one is loaded, beside the seven already there.
        let program = punchcutter_slang::compile_program(
            &source,
            &["bar_check"],
            &punchcutter_slang::Options {
                defines: &[define, "CAUSTICS_BAR_CHECK"],
                includes: &[dir.as_path(), caustics_bar_abi::rendered_dir()],
                image_format_unknown,
            },
        )
        .with_context(|| format!("bar_check does not compile from {}", source.display()))?;
        let mut strings = crate::strings::baked();
        strings.replace(program.reflection.strings);
        gpu.set_check(&program.entries[0].spirv)?;
        Ok(Oracle { gpu, strings })
    }

    /// Put a real font in front of the shaper: a driver once miscompiled it so
    /// that every block's first glyph was `.notdef`, which only a real font on
    /// the machine's own GPU shows (docs/architecture.md, *Rules*).
    ///
    /// In production these bytes arrive from `caustics-glyphs` over a pipe as
    /// `Bytes` runs and the embassy copies them into the six buffers without
    /// looking at one. Here the sidecar's own library
    /// builds them in-process and they go into the same buffers by the same
    /// call — because a shape pass against an empty arena answers `.notdef` for
    /// every codepoint there is, and can witness nothing about shaping at all.
    pub fn load_font(&mut self) -> Result<()> {
        use crate::abi::wire::{
            BUF_BAND_CURVES, BUF_BANDS, BUF_GLYPHS, BUF_OT, BUF_POINTS, BUF_SHAPE_FONT,
        };
        let loaded = punchcutter_fonts::Loaded::load_requiring(caustics_glyphs::COMPOSED)
            .map_err(|err| anyhow::anyhow!(err))?;
        self.gpu.set_cap_height(loaded.cap_height);
        for buffer in [
            BUF_POINTS,
            BUF_BANDS,
            BUF_BAND_CURVES,
            BUF_GLYPHS,
            BUF_OT,
            BUF_SHAPE_FONT,
        ] {
            let Some(bytes) = caustics_glyphs::buffer(&loaded, buffer) else {
                anyhow::bail!("caustics-glyphs has no buffer {buffer}");
            };
            self.gpu.write_bytes(buffer, 0, bytes.len() as u32, &bytes);
        }
        Ok(())
    }

    /// [`Self::load_font`] the way the pipe delivers it: the sidecar's own
    /// frames, font first and then one answer per cover, each `Bytes` into
    /// its buffer at its offset (docs/abi.md, *Bytes*). A cover's answer may rewrite
    /// a record below what an earlier answer sent, which a whole-buffer upload
    /// could never witness.
    ///
    /// Returns the sidecar, so a test can compare the GPU's copy with what the
    /// sidecar holds.
    pub fn load_font_covered(
        &mut self,
        chain: punchcutter_fonts::fallback::Chain,
        covers: &[&str],
    ) -> Result<caustics_glyphs::Sidecar> {
        use crate::abi::wire::{self, Frame};
        let loaded = punchcutter_fonts::Loaded::load_requiring(caustics_glyphs::COMPOSED)
            .map_err(|err| anyhow::anyhow!(err))?;
        self.gpu.set_cap_height(loaded.cap_height);
        let mut sidecar = caustics_glyphs::Sidecar::with_chain(loaded, chain);
        let mut out: Vec<u8> = Vec::new();
        sidecar.write_font(&mut out)?;
        for cover in covers {
            sidecar.answer_cover(&mut out, cover)?;
        }
        let mut input = out.as_slice();
        while let Some(frame) = wire::read(&mut input)? {
            if let Frame::Bytes(bytes) = frame {
                self.gpu
                    .write_bytes(bytes.buffer, bytes.offset, bytes.total, &bytes.bytes);
            }
        }
        Ok(sidecar)
    }

    /// Run one scenario through the model passes and read the ring back.
    ///
    /// The production path throughout: every sidecar's `Rows` body goes into
    /// its region of the rows arena, `Facts::compose` writes the facts,
    /// `Gpu::upload_facts` puts them and the answers where the caustic reads
    /// them, and the build/compose/shape submission is `Gpu::shape_if_pending`'s
    /// — the same three passes in the same encoder the bar runs. `bar_check` is
    /// the one addition, in a pass of its own after them.
    ///
    /// **No `bar_apply` runs headless**, so no `CMD_ROWS` is answered: the
    /// regions are written before the passes and `bar_build` reads them as
    /// they are (docs/abi.md, *The arenas*).
    pub fn build(
        &mut self,
        facts: &mut Facts,
        images: &mut Images,
        strings: &Strings,
        scenario: &Scenario,
    ) -> Result<(Box<State>, Vec<u8>)> {
        // The device outlives a scenario and the arena is one buffer, so every
        // region starts empty — otherwise the last scenario's chips are still
        // on the bar.
        for slot in 0..abi::MAX_SLOTS as u32 {
            self.gpu.zero_rows_header(slot);
        }
        // The regions first, then the facts: the order the embassy writes
        // them in.
        for (name, rows) in &scenario.model.applets {
            let Some(slot) = facts.slot_of(name) else {
                continue;
            };
            match facts.rows_region(slot, rows) {
                Ok(body) => self.gpu.write_rows_region(slot, &body),
                Err(len) => anyhow::bail!(
                    "{}: a rows frame of {len} bytes does not fit the arena's {}",
                    scenario.name,
                    abi::ROWS_ARENA_BYTES
                ),
            }
        }
        facts.compose(&scenario.model, strings, images);
        for (slot, pixels) in images.take_dirty() {
            self.gpu.upload_image_slot(slot, &pixels);
        }
        self.gpu.upload_facts(
            facts.buffer(),
            facts.strings(),
            facts.literals(),
            facts.answers(),
        );
        if let Some(samples) = scenario.model.samples.as_deref() {
            self.gpu.upload_samples(abi::as_bytes(samples));
        }
        self.gpu.set_menu_level(scenario.level.0, scenario.level.1);
        // The bytes come **back** from the device: the letter a dock entry
        // wears while `caustics-apps` has not answered is written by the build
        // pass into the region the embassy reserved for it.
        Ok(self.gpu.run_model_passes(match scenario.model.caffeine {
            crate::model::Caffeine::Absent => abi::CAFFEINE_ABSENT,
            crate::model::Caffeine::Off => abi::CAFFEINE_OFF,
            crate::model::Caffeine::On => abi::CAFFEINE_ON,
        }))
    }
}

#[cfg(test)]
mod tests;
