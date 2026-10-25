//! Where the pictures go: the host's half of a show (docs/abi.md, *The
//! canvas's buffer*).
//!
//! The canvas has two image slots. A show cross-fades from the picture on
//! screen to a new one, so the new one goes in whichever slot the picture on
//! screen is not in, and `facts` says which is which. That is the whole of
//! the routing, and it is routing: the host reads a show's key to find the
//! image it names, and copies the command's head into `facts` without
//! reading its times.
//!
//! **An image waits for its show.** The sidecar sends the image and then the
//! show that names it. The image is not written into a slot when it lands,
//! because the slot it would go in may be the one a fade still in progress is
//! fading *from*; it is held here and placed when its show arrives, which is
//! when the fade it belongs to begins.

use caustics_bar_abi::NO_INDEX;
use caustics_bar_abi::canvas::{CanvasFacts, SLOT_A, SLOT_B};
use caustics_bar_abi::wire::{Canvas, Image};

/// What a command asks of the device.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Write `facts`, and first put `image` in `slot`.
    Upload { slot: u32, image: Image },
    /// Write `facts`; no picture moves.
    Facts,
}

/// The routing state: the image waiting for its show, which key each slot
/// holds, and the facts the caustic reads.
#[derive(Debug)]
pub struct Slots {
    waiting: Option<Image>,
    keys: [Option<String>; 2],
    facts: CanvasFacts,
}

impl Default for Slots {
    fn default() -> Self {
        Slots::new()
    }
}

impl Slots {
    /// No picture: both slots empty, nothing to fade from or to.
    pub fn new() -> Slots {
        Slots {
            waiting: None,
            keys: [None, None],
            facts: CanvasFacts {
                from_slot: NO_INDEX,
                to_slot: NO_INDEX,
                ..CanvasFacts::default()
            },
        }
    }

    /// What `facts` holds now.
    pub fn facts(&self) -> &CanvasFacts {
        &self.facts
    }

    /// The key the slot on screen holds, if any.
    pub fn showing(&self) -> Option<&str> {
        self.slot_key(self.facts.to_slot)
    }

    fn slot_key(&self, slot: u32) -> Option<&str> {
        self.keys.get(slot as usize).and_then(|key| key.as_deref())
    }

    /// An image arrived whole. It waits for the show that names it; a second
    /// image before that show replaces it, and the first is returned so the
    /// caller can say it was never shown.
    pub fn hold(&mut self, image: Image) -> Option<Image> {
        self.waiting.replace(image)
    }

    /// The sidecar that sent the waiting image is gone, and its show with it.
    pub fn forget_waiting(&mut self) -> Option<Image> {
        self.waiting.take()
    }

    /// A command. A show of the waiting image places it; a show of the
    /// picture already on screen restarts nothing and moves nothing; a show
    /// of anything else is refused, and the reason is the caller's witness.
    pub fn apply(&mut self, command: &Canvas) -> Result<Applied, String> {
        match command {
            Canvas::Show { key, .. } => {
                if self.waiting.as_ref().is_some_and(|image| image.key == *key) {
                    let image = self.waiting.take().expect("checked just above");
                    let slot = match self.facts.to_slot {
                        SLOT_A => SLOT_B,
                        _ => SLOT_A,
                    };
                    self.facts.from_slot = self.facts.to_slot;
                    self.facts.to_slot = slot;
                    self.facts.show = command.body();
                    self.facts.shows = self.facts.shows.wrapping_add(1);
                    self.keys[slot as usize] = Some(image.key.clone());
                    return Ok(Applied::Upload { slot, image });
                }
                if self.showing() == Some(key.as_str()) {
                    // The picture on screen, named again (a sidecar that
                    // restarted re-sending what it last showed): a fade from
                    // it to itself is no fade, so nothing changes.
                    return Ok(Applied::Facts);
                }
                Err(format!(
                    "a show for {key:?}, which never arrived as an image; the picture stays"
                ))
            }
            Canvas::Dim { .. } | Canvas::Undim => {
                self.facts.dim = command.body();
                Ok(Applied::Facts)
            }
        }
    }
}

#[cfg(test)]
mod tests;
