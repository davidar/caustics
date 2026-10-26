//! The pointer, as the caustic's wake is told it (docs/canvas.md, *The
//! wake*): which pane it is on, where on that pane's buffer, and how many
//! presses it has made there. Copied into `CanvasFacts` and nothing more;
//! what a motion or a press does to the water is the caustic's.
//!
//! Only panes whose role stirs (`Role::stirs`: the background and the lock)
//! are told here; on any other surface the pointer is, to the wake, nowhere.
//! A position is surface-local logical px as the compositor sends it, and is
//! written in the pane's buffer px, which is what the caustic draws in.

use caustics_bar_abi::canvas::CanvasFacts;

/// One output's buffer coordinate, from a surface-local logical one and the
/// scale the surface is drawn at, in 120ths.
pub fn buffer_coordinate(logical: f64, scale: u32) -> f32 {
    (logical * f64::from(scale) / 120.0) as f32
}

/// The pointer's words in `CanvasFacts`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pointer {
    /// `CanvasParams::pane` of the pane it is on; 0 for none that stirs.
    pane: u32,
    /// Where on that pane, buffer px.
    at: (f32, f32),
    /// Presses on panes that stir, ever: a count, so a press the caustic
    /// has not drawn a frame for yet is never lost to the next one.
    presses: u32,
}

impl Pointer {
    pub fn new() -> Pointer {
        Pointer::default()
    }

    /// The pointer entered or moved on `pane` (a pane that stirs), at
    /// surface-local `position`, of a surface drawn at `scale`. True when
    /// that is news.
    pub fn at(&mut self, pane: u32, position: (f64, f64), scale: u32) -> bool {
        let at = (
            buffer_coordinate(position.0, scale),
            buffer_coordinate(position.1, scale),
        );
        if self.pane == pane && self.at == at {
            return false;
        }
        self.pane = pane;
        self.at = at;
        true
    }

    /// The pointer left `pane`, or `pane` is gone: nowhere, if that is where
    /// it was. A leave of a pane it is no longer on (one the compositor sends
    /// after the enter on the next) changes nothing.
    pub fn left(&mut self, pane: u32) -> bool {
        if self.pane != pane || pane == 0 {
            return false;
        }
        self.pane = 0;
        true
    }

    /// A button went down on `pane`. Counted only where the pointer is.
    pub fn pressed(&mut self, pane: u32) -> bool {
        if self.pane != pane || pane == 0 {
            return false;
        }
        self.presses = self.presses.wrapping_add(1);
        true
    }

    /// `facts` with the pointer's words written in.
    pub fn write(&self, facts: &mut CanvasFacts) {
        facts.pointer_pane = self.pane;
        facts.pointer_x = self.at.0;
        facts.pointer_y = self.at.1;
        facts.presses = self.presses;
    }
}

#[cfg(test)]
mod tests;
