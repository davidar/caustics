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
//! > `Texture2DArray` in the caustic, `insert(key, pixels) -> slot` with LRU
//! > eviction by last-drawn generation.
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
//! drain hands to [`Images::touch_drawn`] whole — a set bit is *drawn* and
//! stamped with the current generation, a clear one is not drawn.
//! [`Images::insert`] stamps too, so a picture
//! that landed this generation survives until a build has had the chance to
//! draw it. When the slots run short, [`Images::claim`] takes the least
//! recently stamped slot that is neither drawn nor this generation's — which
//! is exactly the question an eviction has to answer — and refuses only when
//! there is none.
//!
//! Looking a key up ([`Images::slot_of`]) does **not** stamp it. The image
//! directory looks up every name ever asked on every compose, so if a lookup
//! stamped, every slot ever named would be "drawn" forever: nothing would age
//! out, content-hashed keys only arrive, and within a day the array refuses
//! new tray pixmaps while holding 64 pictures nothing draws.
//!
//! A slot is **not** freed when a table stops using it: a tray item that comes
//! and goes every few seconds would then reload its pixmap every time. It ages
//! out instead, and only when the slots run short.

use std::collections::HashMap;

use crate::abi;

/// One slot: what is in it, and when it was last drawn.
#[derive(Clone, Debug, Default)]
struct Slot {
    /// The key whose pixels are here, or `None` for an empty slot.
    key: Option<String>,
    /// The generation this slot was last drawn (or filled) in.
    touched: u64,
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
    /// Slots taken from someone else, for the health line.
    evictions: u64,
    /// The keys those slots held, for [`Self::take_evicted`].
    evicted: Vec<String>,
    /// Inserts refused because every slot was drawn or filled this generation.
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
            evictions: 0,
            evicted: Vec::new(),
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

    /// Start a rebuild. Everything the rebuild touches carries this stamp, so
    /// everything that does not is a candidate for eviction.
    pub fn begin_generation(&mut self) {
        self.generation += 1;
    }

    /// Rebuilds so far. In the health line beside the slot count, because a
    /// generation number climbing with no evictions beside it is exactly the
    /// picture of a bar whose images are stable.
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

    pub fn evictions(&self) -> u64 {
        self.evictions
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
    /// A lookup, not a draw: it leaves the slot's age alone. What keeps a slot
    /// young is the caustic drawing it ([`Self::touch_drawn`]).
    pub fn slot_of(&self, key: &str) -> Option<u32> {
        self.by_key.get(key).copied()
    }

    /// The caustic's `ACT_IMAGES_DRAWN` mask, slot `n` at bit `n % 32` of
    /// word `n / 32`: replaces every slot's drawn mark, and stamps the drawn
    /// ones with this generation. A copy of what the caustic said — the
    /// embassy decides nothing here.
    pub fn touch_drawn(&mut self, words: &[u32]) {
        let generation = self.generation;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let word = words.get(index / 32).copied().unwrap_or(0);
            let drawn = slot.key.is_some() && word & (1 << (index % 32)) != 0;
            slot.drawn = drawn;
            if drawn {
                slot.touched = generation;
            }
        }
    }

    /// Put `rgba` — exactly [`abi::IMAGE_SLOT_PX`]² **straight** RGBA — in a slot
    /// and return it.
    ///
    /// An existing key is overwritten in place, which is what a tray item whose
    /// pixmap changed wants. `None` means every slot is drawn or was filled in
    /// this generation, which is a bar asking for more than
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
        self.slots[slot as usize].touched = self.generation;
        if !self.dirty.contains(&slot) {
            self.dirty.push(slot);
        }
        Some(slot)
    }

    /// An empty slot, else the least recently drawn one that the caustic's
    /// last build did not draw and that was not filled this generation.
    fn claim(&mut self, key: &str) -> Option<u32> {
        if let Some(free) = self.slots.iter().position(|slot| slot.key.is_none()) {
            self.slots[free].key = Some(key.to_owned());
            self.slots[free].drawn = false;
            self.by_key.insert(key.to_owned(), free as u32);
            return Some(free as u32);
        }
        let generation = self.generation;
        let Some((oldest, stamp)) = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.drawn && slot.touched != generation)
            .min_by_key(|(_, slot)| slot.touched)
            .map(|(index, slot)| (index, slot.touched))
        else {
            self.refusals += 1;
            tracing::warn!(
                "images: all {} slots are drawn or new this frame; {key} is not shown",
                abi::MAX_IMAGE_SLOTS
            );
            return None;
        };
        let evicted = self.slots[oldest].key.take();
        if let Some(old) = &evicted {
            self.by_key.remove(old);
            self.evicted.push(old.clone());
        }
        self.evictions += 1;
        tracing::debug!(
            "images: slot {oldest} goes from {} to {key} (last drawn in generation {stamp}, now {})",
            evicted.as_deref().unwrap_or("nothing"),
            self.generation
        );
        self.slots[oldest].key = Some(key.to_owned());
        self.slots[oldest].drawn = false;
        self.by_key.insert(key.to_owned(), oldest as u32);
        Some(oldest as u32)
    }

    /// The keys of the slots taken from somebody else since the last call.
    ///
    /// An evicted icon has to be *asked for again* — its pixels came off a
    /// pipe and this array was the only copy — so the
    /// compose drains this at its top and un-marks those names, themed and app
    /// id alike, and the caustic's next want lands them from the store.
    /// Without it an eviction is a hole in the bar for the life of the
    /// process.
    pub fn take_evicted(&mut self) -> Vec<String> {
        std::mem::take(&mut self.evicted)
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
