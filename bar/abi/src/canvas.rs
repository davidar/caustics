//! **The canvas's buffer** (docs/abi.md, *The canvas's buffer*): the layout of
//! the GPU buffers the canvas host (`caustics-canvas`) and its caustic
//! (`caustics/canvas.slang`) share.
//!
//! The same kind of contract as [`crate::buffer`] and declared the same way,
//! once, through [`abi_struct!`](crate::abi_struct) and
//! [`abi_consts!`](crate::abi_consts); `canvas_abi.slang` is these tables
//! rendered ([`crate::rendered`]). It is a module of its own, not part of
//! `bar_abi.slang`, because the two caustics share no binding: the canvas
//! imports nothing the bar's caustic does, and this module renders again
//! the few names both use (`Note`, `NO_INDEX`, the witness levels) rather
//! than import the bar's.
//!
//! Four buffers, by name (docs/abi.md, *The canvas's buffer*):
//!
//! - **`params`**, one [`CanvasParams`] per surface (an output's background,
//!   and its overlay while a dim is up): its size, its scale, the frame stamp,
//!   the time and which surface it is, written before every frame drawn on it;
//! - **`facts`**, one [`CanvasFacts`]: what the host was told and where it put
//!   the pictures, written whole when a `Canvas` frame lands;
//! - **`state`**, one [`CanvasState`]: the one buffer read back, a witness
//!   ring, `wants_redraw` and whether the dim has reached its depth;
//! - **`private`**, the caustic's own, at the stride Slang's reflection
//!   reports, which the host allocates and never reads or writes.

// The contract is complete by declaration, as the bar's is: a constant the
// host never names is still one the caustic can.
#![allow(dead_code)]

use crate::buffer::Note;
use crate::decl::{AbiStruct, ConstGroup};
use crate::wire::CanvasBody;
use crate::{abi_consts, abi_struct};

abi_consts! {
    CANVAS_CAPACITIES = "The canvas's capacities and slots";

    /// Records the witness ring holds per frame. The canvas says a handful of
    /// things a fade; a full ring drops the newest and counts it.
    pub const MAX_CANVAS_WITNESSES: usize = 32;
    /// `CanvasFacts::from_slot` and `to_slot`: the picture is `image_a`.
    pub const SLOT_A: u32 = 0;
    /// The picture is `image_b`.
    pub const SLOT_B: u32 = 1;
}

abi_consts! {
    CANVAS_TARGETS = "`CanvasParams::target`: which of an output's surfaces a frame is drawn on \
                      (docs/abi.md, *The canvas's buffer*)";

    /// The background layer: the picture and its cross-fade, opaque.
    pub const TARGET_BACKGROUND: u32 = 0;
    /// The overlay layer, up only while a dim is: the background's own
    /// wallpaper, darkened by `DIM_DARKEN`, at the dim's alpha,
    /// premultiplied, over everything else on the output.
    pub const TARGET_OVERLAY: u32 = 1;
}

abi_struct! {
    /// One surface's frame facts (uniform `params`), written by the host before
    /// every frame it draws on that surface. 32 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct CanvasParams {
        /// The output's buffer, px: its logical size times its scale.
        pub width: u32,
        pub height: u32,
        /// The output's scale in 120ths (`wp_fractional_scale_v1`'s unit;
        /// 240 at scale 2).
        pub scale: u32,
        /// The frame stamp: ≥ 1, advanced after a frame's readback, and what
        /// the witness ring is stamped with (docs/abi.md, *The three rules the
        /// buffer adds*, rule 1).
        pub frame: u32,
        /// `CLOCK_BOOTTIME` ms when this frame was drawn, low word.
        pub now_lo: u32,
        /// The high word.
        pub now_hi: u32,
        /// `TARGET_*`: which surface of the output this frame is drawn on.
        pub target: u32,
        pub _pad1: u32,
    }
}

abi_struct! {
    /// What the host knows about the picture (storage `facts`), written whole
    /// whenever a `Canvas` frame lands. 80 B.
    ///
    /// The two commands are the wire's heads, copied: the host reads a show's
    /// key to put the image it names in a slot, and nothing else of either.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct CanvasFacts {
        /// The last `CANVAS_SHOW`, as it crossed the wire; zero before one.
        pub show: CanvasBody,
        /// The last `CANVAS_DIM` or `CANVAS_UNDIM`; zero before one.
        pub dim: CanvasBody,
        /// Where the picture the show fades **from** is (`SLOT_*`), or
        /// `NO_INDEX` for none: the first show fades from the dim colour.
        pub from_slot: u32,
        /// Where the picture it fades **to** is, or `NO_INDEX` before any.
        pub to_slot: u32,
        /// Shows the host has applied. A new value is a new fade, which is how
        /// the caustic says each of a fade's witnesses once.
        pub shows: u32,
        pub _pad0: u32,
    }
}

abi_struct! {
    /// The one buffer the host reads back (storage `state`), after every
    /// frame: the caustic's witness ring, the word that says the surface just
    /// drawn wants another frame, and the two that say the dim is at its
    /// depth. 1568 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, PartialEq)] }
    pub struct CanvasState {
        /// Non-zero when the surface this frame was drawn on (its
        /// `params.target`) has more to show: a fade in progress on the
        /// background, a dim on its way on the overlay. The next frame
        /// callback is a frame.
        pub wants_redraw: u32,
        /// The frame the witness ring belongs to (rule 1).
        pub witness_frame: u32,
        pub witness_count: u32,
        /// Records dropped because the ring was full.
        pub witness_lost: u32,
        /// Non-zero when `facts.dim` is a `CANVAS_DIM` whose alpha has reached
        /// its depth: the host's word for writing `CANVAS_EVENT_DIMMED`.
        pub dim_reached: u32,
        /// The `id` of the dim `dim_reached` is about, as `facts.dim` carries
        /// it.
        pub dim_id: u32,
        pub _pad0: u32,
        pub _pad1: u32,
        pub witnesses: [Note; MAX_CANVAS_WITNESSES],
    }
}

impl CanvasState {
    pub const ZERO: CanvasState = CanvasState {
        wants_redraw: 0,
        witness_frame: 0,
        witness_count: 0,
        witness_lost: 0,
        dim_reached: 0,
        dim_id: 0,
        _pad0: 0,
        _pad1: 0,
        witnesses: [Note::ZERO; MAX_CANVAS_WITNESSES],
    };

    /// The witness ring's live prefix, if it belongs to `frame`.
    pub fn witnesses_of(&self, frame: u32) -> &[Note] {
        if self.witness_frame != frame {
            return &[];
        }
        let live = (self.witness_count as usize).min(MAX_CANVAS_WITNESSES);
        &self.witnesses[..live]
    }
}

unsafe impl crate::buffer::Plain for CanvasParams {}
unsafe impl crate::buffer::Plain for CanvasFacts {}
unsafe impl crate::buffer::Plain for CanvasState {}

/// Every constant group `canvas_abi.slang` renders, in order: two of the bar's
/// own (rendered again, never imported) and the canvas's.
pub(crate) const CONST_GROUPS: &[ConstGroup] = &[
    crate::buffer::NOTHING_GROUP,
    crate::buffer::WITNESS_GROUP,
    crate::wire::CANVAS_GROUP,
    CANVAS_CAPACITIES,
    CANVAS_TARGETS,
];

/// Every struct `canvas_abi.slang` renders, all of them bound.
pub const STRUCTS: &[AbiStruct] = &[
    AbiStruct {
        decl: Note::DECL,
        bound: true,
        offsets: Note::offsets,
    },
    AbiStruct {
        decl: CanvasBody::DECL,
        bound: true,
        offsets: CanvasBody::offsets,
    },
    AbiStruct {
        decl: CanvasParams::DECL,
        bound: true,
        offsets: CanvasParams::offsets,
    },
    AbiStruct {
        decl: CanvasFacts::DECL,
        bound: true,
        offsets: CanvasFacts::offsets,
    },
    AbiStruct {
        decl: CanvasState::DECL,
        bound: true,
        offsets: CanvasState::offsets,
    },
];

/// The rendered module's own header comment.
const MODULE_DOC: &[&str] = &[
    "The canvas's buffer (docs/abi.md, *The canvas's buffer*): the layout of the GPU",
    "buffers the canvas host and its caustic share. Inert, as `bar_abi` is: no",
    "bindings, no entry points; `canvas_state.slang` binds the canvas's resources.",
    "",
    "It imports nothing of the bar's. The few names both caustics use (`Note`,",
    "`NO_INDEX`, the witness levels) are rendered here again from the same Rust",
    "declarations, and `CanvasBody` is the wire's `WIRE_CANVAS` head, copied by the",
    "host into `facts` as it crossed the pipe.",
];

/// `canvas_abi.slang`, rendered from the tables above.
pub fn slang_module() -> String {
    crate::decl::render(
        crate::RENDERER,
        crate::NORMATIVE,
        "canvas_abi",
        "bar/abi/src/canvas.rs",
        MODULE_DOC,
        &[],
        CONST_GROUPS,
        STRUCTS,
    )
}

#[cfg(test)]
mod tests;
