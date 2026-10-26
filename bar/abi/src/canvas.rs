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
//! Seven buffers of its own, by name (docs/abi.md, *The canvas's buffer*):
//!
//! - **`params`**, one [`CanvasParams`] per surface (an output's background,
//!   its overlay while a dim is up, its prompt pane while a prompt is): its
//!   size, its scale, the frame stamp, the time and which surface it is,
//!   written before every frame drawn on it;
//! - **`facts`**, one [`CanvasFacts`]: what the host was told, where it put
//!   the pictures, and the prompt up now ([`CanvasPrompt`]), written whole
//!   when any of them changes;
//! - **`state`**, one [`CanvasState`]: the one buffer read back, a witness
//!   ring, `wants_redraw`, whether the dim has reached its depth, the shape
//!   pass's threshold and the runs the font could not draw;
//! - **`private`**, the caustic's own, at the stride Slang's reflection
//!   reports, which the host allocates and never reads or writes;
//! - **`runs`**, [`CANVAS_GLYPHS`] shaped glyphs, the caustic's as `private`
//!   is: written by `canvas_shape`, read by `canvas_draw`;
//! - **`literals`**, the [`Literal`] index of every `getStringHash` literal
//!   the running caustic names, into the strings buffer;
//! - **`prompt_text`**, [`PROMPT_TEXT_BYTES`] of the prompt's own text as
//!   the sidecar sent it, which [`CanvasPrompt`]'s ranges name.
//!
//! The text engine's buffers beside them (set 1, `text`'s curve buffers; set
//! 2, `shapeParams`, `shapeOt` and the strings buffer `shapeUtf8`) are the
//! bar's, under the bar's names, and their layouts are the engine's
//! (punchcutter/docs/arena.md).

// The contract is complete by declaration, as the bar's is: a constant the
// host never names is still one the caustic can.
#![allow(dead_code)]

use crate::buffer::{Literal, Note};
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
    /// Text blocks the caustic shapes: one `canvas_shape` thread each, and
    /// the length of [`CanvasState::covers`].
    pub const CANVAS_TEXT_BLOCKS: usize = 5;
    /// Shaped-glyph slots in `runs`: a prompt's title, two lines and a name,
    /// hundreds of glyphs rather than the bar's thousands.
    pub const CANVAS_GLYPHS: usize = 512;
    /// The strings buffer (`shapeUtf8`): every literal of the running
    /// caustic, laid end to end.
    pub const CANVAS_STRINGS_BYTES: usize = 16384;
    /// Entries in `literals`.
    pub const MAX_CANVAS_LITERALS: usize = 256;
    /// `prompt_text`: the title, the last line, the ask's text and the echoed
    /// answer, each at most a wire message (`MAX_NAME_LEN`) and so always
    /// room for all four; a host that is ever short cuts the last and says
    /// so.
    pub const PROMPT_TEXT_BYTES: usize = 4096;
}

abi_consts! {
    CANVAS_PROMPT_STATES = "`CanvasPrompt::state`: where the prompt up now is in its conversation \
                            (docs/canvas.md, *The prompt core*)";

    /// No prompt: nothing is drawn for one, and no pane is up.
    pub const PROMPT_IDLE: u32 = 0;
    /// Asking: the panel is up and keys are typed into it.
    pub const PROMPT_ASKING: u32 = 1;
    /// The answer has gone to the sidecar; the dots are frozen and keys are
    /// ignored until it says what came of it.
    pub const PROMPT_VERIFYING: u32 = 2;
    /// An error line followed the answer; the next ask is another attempt.
    pub const PROMPT_FAILED: u32 = 3;
    /// Held: the conversation has begun (its title, perhaps a line) and has
    /// not asked yet. The panel is up with no field, so the user sees that
    /// something is wanted of them while the sink's helper is at work, say
    /// a fingerprint module waiting for a finger before PAM asks for a
    /// password. Escape dismisses; other keys do nothing.
    pub const PROMPT_HELD: u32 = 4;
}

abi_consts! {
    CANVAS_TEXT_SOURCES = "`CanvasCover::source`: which buffer a run's bytes are in";

    /// The strings buffer: a literal of the caustic's.
    pub const TEXT_LITERALS: u32 = 0;
    /// `prompt_text`: the prompt's own text, as its sidecar sent it.
    pub const TEXT_PROMPT: u32 = 1;
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
    /// The prompt's pane on the top layer, up only while a prompt is: the
    /// wallpaper darkened by `PROMPT_DARKEN`, opaque, and the panel on it.
    pub const TARGET_PROMPT: u32 = 2;
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
    /// The prompt up now (`CanvasFacts::prompt`), as the host holds it
    /// (docs/canvas.md, *The prompt core*). 64 B.
    ///
    /// **The secret is not here.** For an echo-off ask the caustic is given
    /// `typed`, a count of characters, and draws dots; the bytes stay in the
    /// host. Every range is into `prompt_text` and is text a sidecar sent, or
    /// for an echo-on ask what was typed, which PAM shows by its own
    /// definition.
    { #[repr(C)] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct CanvasPrompt {
        /// The conversation's id, as its sidecar numbered it; 0 when idle.
        pub id: u32,
        /// `PROMPT_*`: idle, held, asking, verifying or failed.
        pub state: u32,
        /// 1 when the ask echoes what is typed (`PROMPT_ECHO`).
        pub echo: u32,
        /// Characters typed (characters, not bytes); frozen while verifying.
        pub typed: u32,
        /// Answers that an error line followed, in this conversation.
        pub attempts: u32,
        /// Bumped whenever any text below changes: a new value is text to
        /// shape again.
        pub generation: u32,
        /// The title (`PROMPT_INFO` with `PROMPT_INFO_TITLE`).
        pub title_off: u32,
        pub title_len: u32,
        /// The last line shown (`PROMPT_INFO` or `PROMPT_ERROR`).
        pub line_off: u32,
        pub line_len: u32,
        /// 1 when that line is an error.
        pub line_error: u32,
        /// The ask's own text (`PROMPT_SECRET`, `PROMPT_ECHO`); empty for the
        /// caustic's own word.
        pub ask_off: u32,
        pub ask_len: u32,
        /// What was typed for an echo-on ask; always empty for an echo-off one.
        pub echo_off: u32,
        pub echo_len: u32,
        pub _pad0: u32,
    }
}

abi_struct! {
    /// What the host knows (storage `facts`), written whole whenever it
    /// changes: the picture, the dim, the prompt. 144 B.
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
        /// Entries live in `literals`.
        pub literal_count: u32,
        /// The prompt up now; `state` `PROMPT_IDLE` when there is none.
        pub prompt: CanvasPrompt,
    }
}

abi_struct! {
    /// One run the font could not draw (`CanvasState::covers`): the bytes it
    /// was shaped from, for the host to send its glyphs sidecar as a
    /// `REQ_COVER` (docs/abi.md, *Request*). 16 B.
    { #[repr(C)] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct CanvasCover {
        /// `TEXT_*`: the strings buffer or `prompt_text`.
        pub source: u32,
        /// Byte offset and length in that buffer.
        pub off: u32,
        pub len: u32,
        pub _pad0: u32,
    }
}

abi_struct! {
    /// The one buffer the host reads back (storage `state`), after every
    /// frame: the caustic's witness ring, the word that says the surface just
    /// drawn wants another frame, the two that say the dim is at its depth,
    /// the shape pass's threshold, and the runs to cover. 1648 B.
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
        /// `SHAPE_WATCHDOG_MS` from the tunables: how long one `canvas_shape`
        /// pass may take on the device before the host stops shaping until the
        /// arena is written again. 0 before the first frame.
        pub shape_watchdog_ms: u32,
        /// Live entries of `covers`.
        pub cover_count: u32,
        /// The text blocks the font has a glyph missing for, as bytes; empty
        /// while no block can be drawn at all (no font has landed yet).
        pub covers: [CanvasCover; CANVAS_TEXT_BLOCKS],
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
        shape_watchdog_ms: 0,
        cover_count: 0,
        covers: [CanvasCover {
            source: 0,
            off: 0,
            len: 0,
            _pad0: 0,
        }; CANVAS_TEXT_BLOCKS],
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

    /// The live prefix of `covers`.
    pub fn covers(&self) -> &[CanvasCover] {
        &self.covers[..(self.cover_count as usize).min(CANVAS_TEXT_BLOCKS)]
    }
}

unsafe impl crate::buffer::Plain for CanvasParams {}
unsafe impl crate::buffer::Plain for CanvasFacts {}
unsafe impl crate::buffer::Plain for CanvasState {}
unsafe impl crate::buffer::Plain for CanvasPrompt {}
unsafe impl crate::buffer::Plain for CanvasCover {}

/// Every constant group `canvas_abi.slang` renders, in order: two of the bar's
/// own (rendered again, never imported) and the canvas's.
pub(crate) const CONST_GROUPS: &[ConstGroup] = &[
    crate::buffer::NOTHING_GROUP,
    crate::buffer::WITNESS_GROUP,
    crate::wire::CANVAS_GROUP,
    CANVAS_CAPACITIES,
    CANVAS_TARGETS,
    CANVAS_PROMPT_STATES,
    CANVAS_TEXT_SOURCES,
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
        decl: Literal::DECL,
        bound: true,
        offsets: Literal::offsets,
    },
    AbiStruct {
        decl: CanvasParams::DECL,
        bound: true,
        offsets: CanvasParams::offsets,
    },
    AbiStruct {
        decl: CanvasPrompt::DECL,
        bound: true,
        offsets: CanvasPrompt::offsets,
    },
    AbiStruct {
        decl: CanvasFacts::DECL,
        bound: true,
        offsets: CanvasFacts::offsets,
    },
    AbiStruct {
        decl: CanvasCover::DECL,
        bound: true,
        offsets: CanvasCover::offsets,
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
    "`Literal`, `NO_INDEX`, the witness levels) are rendered here again from the same Rust",
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
