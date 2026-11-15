//! The image slots: everything the bar draws that is pixels rather than curves
//! (docs/architecture.md, *Images and icons*).
//!
//! A glyph and a symbolic icon are curves in one buffer the caustic
//! walks per texel, which leaves exactly three things that are genuinely
//! pixels: a colour SVG (the dock's app icons — Firefox's is gradients), a PNG,
//! and a tray item's pixmap off the wire. All three arrive at a known size and
//! all three are square, so the structure they want is not an atlas:
//!
//! > [`abi::MAX_IMAGE_SLOTS`] slots of [`abi::IMAGE_SLOT_PX`]² RGBA, a
//! > `Texture2DArray` in the caustic, `insert(key, pixels) -> slot`, and a slot
//! > is held for exactly as long as the caustic draws it.
//!
//! # Straight alpha
//!
//! The pixels here are **straight** (non-premultiplied) RGBA8 — what a
//! `Pixmap` frame carries (docs/abi.md, *Pixmap*). The caustic multiplies by
//! the texel's own alpha at the sample, one multiply per texel in
//! `draw_block_at`'s `BLOCK_IMAGE` arm, which is the one place it can be and
//! the one place it is. Nothing on this side of the
//! wire touches a texel: a sidecar writes the bytes, the bar copies them into
//! a slot, the GPU reads them.
//!
//! There is no shelf packer, no free list, no growth pass, no uv rectangles, no re-raster on a
//! scale change, because a glyph's size is the only reason any of those exist.
//!
//! # The generation, and what the caustic drew
//!
//! The embassy calls [`Images::begin_generation`] once per compose. Only the
//! caustic lays the blocks out, so only it knows which slots are on the bar:
//! once per `bar_build` it pushes an `ACT_IMAGES_DRAWN` whose bitmask the
//! drain hands to [`Images::touch_drawn`] whole — a set bit is *drawn*, and a
//! clear one is **freed there and then**: the key no longer answers and the
//! slot is the next insert's. How long an undrawn picture keeps its bit is
//! the caustic's (`IMAGE_LINGER_S` in the tunables): it holds the bit for a
//! slot drawn within that time, so an alternating icon keeps both pictures,
//! and this side never measures time at all.
//!
//! The one slot a clear bit does not free is one no build has had the chance
//! to draw: a picture lands, the *next* compose is what names its slot to the
//! caustic, and only a build after that compose can draw it. So
//! [`Images::insert`] stamps the slot with the generation it landed in, and a
//! mask frees a slot only when a compose has come since.
//!
//! This array is therefore not a cache and nothing in it ages. What an icon
//! comes back from when it is drawn again is the embassy's pixmap store, or
//! the sidecar that made it; [`Images::take_freed`] is how the embassy hears
//! which names to stop treating as answered.
//!
//! Looking a key up ([`Images::slot_of`]) is not a draw and keeps nothing: the
//! image directory looks up every name asked on every compose.

use std::collections::HashMap;

use caustics_bar_abi as abi;

/// One slot: what is in it, and whether it is on the bar.
#[derive(Clone, Debug, Default)]
struct Slot {
    /// The key whose pixels are here, or `None` for an empty slot.
    key: Option<String>,
    /// The generation its pixels landed in: a build can draw it only after
    /// the compose that follows.
    landed: u64,
    /// Whether the caustic's last `ACT_IMAGES_DRAWN` had this slot's bit set.
    drawn: bool,
}

/// The slot array, CPU side: the pixels, and who is in which slot.
pub struct Images {
    /// `MAX_IMAGE_SLOTS * IMAGE_SLOT_PX² * 4` bytes — 1 MiB at 64 slots of
    /// 64 px, fixed for the life of the process.
    pixels: Vec<u8>,
    slots: Vec<Slot>,
    by_key: HashMap<String, u32>,
    generation: u64,
    /// Slots whose pixels changed since the last [`Self::take_dirty`].
    dirty: Vec<u32>,
    /// Slots let go because a build stopped drawing them, for the health line.
    frees: u64,
    /// The keys those slots held, for [`Self::take_freed`].
    freed: Vec<String>,
    /// Inserts refused because every slot was drawn or newly filled.
    refusals: u64,
}

impl Default for Images {
    fn default() -> Self {
        Images::new()
    }
}

impl Images {
    pub fn new() -> Images {
        let texels = abi::IMAGE_SLOT_PX as usize * abi::IMAGE_SLOT_PX as usize;
        Images {
            pixels: vec![0; texels * 4 * abi::MAX_IMAGE_SLOTS],
            slots: vec![Slot::default(); abi::MAX_IMAGE_SLOTS],
            by_key: HashMap::new(),
            generation: 0,
            dirty: Vec::new(),
            frees: 0,
            freed: Vec::new(),
            refusals: 0,
        }
    }

    /// Bytes one slot occupies: [`abi::IMAGE_SLOT_PX`]² RGBA. Every image is
    /// fitted to that square before it arrives, whatever size it will be drawn
    /// at: the caustic's sampler does the scaling, which is what makes a scale
    /// change free.
    pub const fn slot_bytes() -> usize {
        abi::IMAGE_SLOT_PX as usize * abi::IMAGE_SLOT_PX as usize * 4
    }

    /// Start a compose: the one that names to the caustic every slot landed
    /// before it.
    pub fn begin_generation(&mut self) {
        self.generation += 1;
    }

    /// Composes so far, for the health line.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Slots in use.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Slots there are.
    pub const fn capacity() -> usize {
        abi::MAX_IMAGE_SLOTS
    }

    /// Slots freed since the process began.
    pub fn frees(&self) -> u64 {
        self.frees
    }

    pub fn refusals(&self) -> u64 {
        self.refusals
    }

    /// Slots the caustic's last build drew, for the health line.
    pub fn drawn(&self) -> usize {
        self.slots.iter().filter(|slot| slot.drawn).count()
    }

    /// The slot `key` is in, or `None`, in which case the caller produces the
    /// pixels and calls [`Self::insert`].
    ///
    /// A lookup, not a draw: what keeps a slot is the caustic drawing it
    /// ([`Self::touch_drawn`]).
    pub fn slot_of(&self, key: &str) -> Option<u32> {
        self.by_key.get(key).copied()
    }

    /// The caustic's `ACT_IMAGES_DRAWN` mask, slot `n` at bit `n % 32` of
    /// word `n / 32`: replaces every slot's drawn mark, and frees each slot
    /// the build could have drawn and did not. `true` when one was freed,
    /// which owes a compose: the directory still names it.
    ///
    /// A copy of what the caustic said — the embassy decides nothing here.
    pub fn touch_drawn(&mut self, words: &[u32]) -> bool {
        let generation = self.generation;
        let mut freed = false;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let word = words.get(index / 32).copied().unwrap_or(0);
            slot.drawn = slot.key.is_some() && word & (1 << (index % 32)) != 0;
            // Landed since the last compose: no build has been told of it.
            if slot.drawn || slot.landed >= generation {
                continue;
            }
            let Some(key) = slot.key.take() else {
                continue;
            };
            tracing::debug!("images: slot {index} lets {key} go, which no block draws");
            self.by_key.remove(&key);
            self.freed.push(key);
            self.frees += 1;
            freed = true;
        }
        freed
    }

    /// Put `rgba` — exactly [`abi::IMAGE_SLOT_PX`]² **straight** RGBA — in a slot
    /// and return it.
    ///
    /// An existing key is overwritten in place, which is what a tray item whose
    /// pixmap changed wants. `None` means every slot is drawn or waiting for
    /// its first build, which is a bar asking for more than
    /// [`abi::MAX_IMAGE_SLOTS`] images at once: one witness and a missing icon,
    /// never a panic and never a wrong slot.
    pub fn insert(&mut self, key: &str, rgba: &[u8]) -> Option<u32> {
        if rgba.len() != Self::slot_bytes() {
            tracing::warn!(
                "images: {key} is {} bytes, not the {} one slot holds",
                rgba.len(),
                Self::slot_bytes()
            );
            return None;
        }
        let slot = match self.by_key.get(key) {
            Some(slot) => *slot,
            None => self.claim(key)?,
        };
        let at = slot as usize * Self::slot_bytes();
        self.pixels[at..at + Self::slot_bytes()].copy_from_slice(rgba);
        self.slots[slot as usize].landed = self.generation;
        if !self.dirty.contains(&slot) {
            self.dirty.push(slot);
        }
        Some(slot)
    }

    /// An empty slot, or `None`: nothing is ever taken from another key.
    fn claim(&mut self, key: &str) -> Option<u32> {
        let Some(free) = self.slots.iter().position(|slot| slot.key.is_none()) else {
            self.refusals += 1;
            tracing::warn!(
                "images: all {} slots are drawn or new this frame; {key} is not shown",
                abi::MAX_IMAGE_SLOTS
            );
            return None;
        };
        self.slots[free].key = Some(key.to_owned());
        self.slots[free].drawn = false;
        self.by_key.insert(key.to_owned(), free as u32);
        // The other half of `touch_drawn`'s line: a picture that keeps landing
        // and being let go reads as a pair of these per change.
        tracing::debug!("images: slot {free} takes {key}");
        Some(free as u32)
    }

    /// Whether [`Self::take_freed`] has anything to give.
    pub fn has_freed(&self) -> bool {
        !self.freed.is_empty()
    }

    /// The keys of the slots freed since the last call.
    ///
    /// A freed icon has to be *asked for again* when it is next drawn — this
    /// array held the only copy the caustic can sample — so the compose drains
    /// this at its top and un-marks those names, themed and app id alike, and
    /// the caustic's next want lands them from the store. Without it the name
    /// would stay "answered" with no slot behind it.
    pub fn take_freed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.freed)
    }

    /// The slots whose pixels changed, and the bytes of each — what the GPU
    /// upload walks. Cleared by the call.
    pub fn take_dirty(&mut self) -> Vec<(u32, Vec<u8>)> {
        let bytes = Self::slot_bytes();
        std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|slot| {
                let at = slot as usize * bytes;
                (slot, self.pixels[at..at + bytes].to_vec())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
