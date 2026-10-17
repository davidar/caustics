//! **The buffer** (docs/abi.md, *The buffer*): the layout of the GPU buffers the
//! bar's embassy and its caustic share.
//!
//! Both halves are built from one tree — the caustic from source, at build and
//! on every reload — so this contract has no binary stability to keep across
//! builds. [`BAR_ABI_VERSION`] is a label; nothing reads it.
//!
//! **Declared once** (docs/abi.md, *Declared once*): every struct and constant below
//! is written through [`abi_struct!`](crate::abi_struct) and
//! [`abi_consts!`](crate::abi_consts), which emit the Rust item and a table
//! beside it, and `bar_abi.slang` is that table rendered by
//! [`crate::rendered`], wherever a compile needs it and never in the tree. So
//! the Slang half is not a second copy to keep in step, and the generic layout
//! test in `bar/src/abi.rs` still compares slangc's own per-field offsets
//! against `offset_of!` — by walking the same table, rather than a
//! hand-written list of it.
//!
//! The other contract is [`crate::wire`], and it is a different kind of
//! promise: a sidecar built last month and a bar built today must agree byte
//! for byte, so it is framed so a reader can skip what it does not know, and
//! a sidecar's first frame names the contract it was built against (docs/abi.md, *Hello*).
//!
//! # Layout rules (docs/abi.md, *Layout rules*, restated once)
//!
//! std430. Every struct is 16-byte aligned and a multiple of 16 bytes long; no
//! field is anything but a `u32`/`f32` or an array of them, for which every
//! bit pattern is valid — which is what makes the [`Plain`] byte views sound.
//! Padding is `_padN`, scalar words, never a 3-vector: std430 aligns a 3-vector to 16 B and slangc's reflection once put one at the wrong offset.
//! [`NO_INDEX`] means "nothing"
//! wherever an index is optional, on both contracts.
//!
//! # Three channels out, and a buffer that is not described here
//!
//! The caustic talks back through exactly three things, and everything it
//! builds for itself is in a buffer this file does not describe.
//!
//! - **[`State`]**, the one buffer the embassy reads back: a [`Surface`] of
//!   polled words, an **action** ring and a **witness** ring, both of [`Note`]
//!   (docs/abi.md, *The action ring*–*The one word*). An action is a kind and its operands, forwarded and never
//!   resolved — a window id is niri's, an app id is a range of the strings
//!   buffer, a sidecar action is the wire body's words. A witness is the
//!   `getStringHash` of a format literal and up to ten words, and the embassy
//!   prints it; there is no per-witness Rust, and a new line in the journal is
//!   one Slang line.
//! - **`private`**, one binding of one struct whose stride comes from slangc's
//!   reflection (docs/abi.md, *The private buffer*). The element table, the item and block tables, the
//!   scratch, the springs, the popup's want, the last pointer position, every
//!   rows region's shape hash, every value a witness compares against — all of
//!   it lives there, the embassy allocates it and **never writes or reads
//!   it**, and the vocabulary for it (`ITEM_*`, `ELEM_*`, `BLOCK_*`, `Id`, …)
//!   is `caustics/bar_model.slang`'s, not this file's. `runs` is private in
//!   the same sense and is a binding only because `shapeRun` writes through a
//!   buffer handle.
//! - Both start **zero**: wgpu zero-initialises a buffer at creation and the
//!   caustic initialises itself, so nothing is uploaded at bring-up to clear them (docs/abi.md, *Who writes what*).
//!
//! # What the embassy writes
//!
//! What it **originates**, and nothing else (docs/abi.md, *The facts*): [`FrameInputs`],
//! [`Params`] and [`Command`] per frame; [`Facts`] — a header, the windows
//! niri reports, the dock's app ids in dock order, the image directory, the
//! sidecar slots by name and the literal pool — and the UTF-8 all of those
//! address. What a **sidecar** sent it copies byte for byte into three arenas
//! this file does not lay out because the *wire* does (docs/abi.md, *The arenas*): `samples`, the
//! `rows` arena of [`MAX_SLOTS`] × [`ROWS_ARENA_BYTES`], and the `answers`
//! arena of [`ANSWERS_BYTES`]. The caustic reads those through
//! `bar_wire.slang` and joins them to the facts by name.
//!
//! Nothing on this side hashes anything at all (docs/abi.md, *The private buffer*).

// The ABI is complete by contract, not by current use: `bar_abi.slang`
// is rendered from the tables below, so a constant the embassy never reads is
// still a constant the caustic can. Deleting one because this side does not
// name it would take it out of the caustic's vocabulary too.
#![allow(dead_code)]

use crate::decl::{AbiStruct, ConstGroup};
use crate::{abi_consts, abi_struct};

/// The words the **wire** declares that the caustic reads as the buffer's own
/// (docs/abi.md, *The arenas*): an applet's kind, a chart's kind and capacities,
/// and the action kinds the embassy forwards to a sidecar unchanged.
///
/// They are declared in [`crate::wire`] because a separately built sidecar is
/// the other end of every one of them, and re-exported here because the bar
/// and the caustic read them as the buffer's own. `abi::APPLET_AUDIO` and
/// `abi::wire::APPLET_AUDIO` are the same constant, spelled from either side.
pub use crate::wire::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SECONDARY,
    ACTION_SET_VALUE, APPLET_AUDIO, APPLET_BLUETOOTH, APPLET_CAFFEINE, APPLET_NETWORK,
    APPLET_NOTIFY, APPLET_OTHER, APPLET_POWER, APPLET_TRAY, CHART_CPU, CHART_DISK, CHART_GPU,
    CHART_MEM, CHART_NET, CHART_OTHER, CHART_SAMPLES, CHART_TEMP, MAX_CHARTS,
};

abi_consts! {
    CAPACITIES = "Versions and capacities";

    /// The buffer layout version. A label for the layout: the two halves are compiled together, so nothing reads it at run time.
    pub const BAR_ABI_VERSION: u32 = 14;

    /// Positioned-glyph slots the shape pass may fill - the whole bar's text at
    /// once, every block's run laid end to end.
    ///
    /// The one capacity of the caustic's own memory that stays here, because
    /// the buffer it bounds is a *binding*: `shape.slang`'s `shapeRun` writes
    /// through an `RWStructuredBuffer<ShapedGlyph>` handle and an array inside
    /// a struct is not one, so `runs` is allocated by the embassy and — like
    /// `private` — never written or read by it (docs/abi.md, *The private buffer*).
    ///
    /// `bar_build` gives a text block `min(bytes * 4 + 8, MAX_RUN_GLYPHS)`
    /// slots — `Batch::push`'s own rule, so a substitution that grows a Latin
    /// label fits — and that wants a table four times the 4096 a slot per byte
    /// would. At 32 bytes
    /// a `ShapedGlyph` that is 512 KiB of device memory the embassy allocates
    /// and never touches; the alternative is a label the build
    /// drops for want of slots, which is how a window title goes missing.
    ///
    /// Doubled again to **32768** (1 MiB) because a notification's body is
    /// shaped too, a slot a byte plus eight and up to `BODY_SHAPE_BYTES` (384)
    /// of it per row, and `caustics-notify`'s menu is forty rows — about 16k slots
    /// of bodies on top of everything the bar already shapes.
    pub const MAX_GLYPHS: usize = 32768;

    /// Words of UTF-8 the strings buffer holds, four bytes each. 64 KiB, and
    /// it carries only what the *embassy* originates: the literal
    /// pool, every window title, every app id, every sidecar's name, the image
    /// directory's keys, and the composed region. A row's label and a
    /// sidecar's answer are in the arenas (docs/abi.md, *The arenas*).
    pub const MAX_STRING_WORDS: usize = 16384;

    /// Slots in the image array: everything the bar draws that is pixels rather
    /// than curves (a colour SVG, a PNG, a tray pixmap), each one slot.
    ///
    /// Fixed, and evicted by least-recently-referenced generation, never packed.
    /// Fixed slots need no packer, because every image is fitted to one size before it arrives.
    pub const MAX_IMAGE_SLOTS: usize = 64;

    /// The side of one image slot, device px.
    ///
    /// `ICON_LOGICAL` is 20 in `caustics/bar_tunables.slang` and the largest
    /// buffer scale the bar expects to meet is 3, so 60 px is the most any icon
    /// is drawn at; 64 is that rounded up; every sidecar fits its pixmaps to it before the wire. A slot holds the
    /// image at this size whatever the live icon px is, and the caustic's
    /// sampler scales it - which is the whole reason a scale change now
    /// rasterises nothing.
    pub const IMAGE_SLOT_PX: u32 = 64;

    /// The preview slot's side, device px: one fixed square texture whose live
    /// extent rides in [`FrameInputs::preview_size`]. The caustic reads the
    /// extent and never the cap.
    pub const PREVIEW_MAX_DIM: u32 = 2048;

    /// Commands the ring holds per frame. Overflow is dropped with a warning.
    pub const MAX_COMMANDS: usize = 256;

    /// Actions the caustic may push in one frame (docs/abi.md, *The action ring*). A
    /// frame carries at most a click, a popup message and a handful of
    /// requests; 64 is room for a first frame that wants every icon on the bar.
    /// Past it the ring drops the **newest** and counts it in
    /// [`Surface::action_lost`], because a lost action beats a repeated one.
    pub const MAX_ACTIONS: usize = 64;

    /// Witnesses the caustic may push in one frame (docs/abi.md, *The witness ring*). A
    /// first laid-out frame witnesses every element, so this is the caustic's
    /// own element cap plus room for a frame's summary and build lines.
    pub const MAX_WITNESSES: usize = 384;

    /// Literals of the running caustic's string table the pool carries. The
    /// embassy appends every one of them to the strings
    /// buffer on every rebuild and publishes an index in the `literals` buffer,
    /// so a composition on the GPU can spell a weekday out of words the embassy
    /// never learns the meaning of. It is also the table the witness printer
    /// resolves a format literal and a `{n:s}` argument through.
    pub const MAX_LITERALS: usize = 256;

    /// Windows [`Facts::windows`] carries room for.
    pub const MAX_WINDOWS: usize = 128;

    /// Dock entries [`Facts::apps`] carries room for, in dock order.
    pub const MAX_APPS: usize = 64;

    /// Sidecar slots: one per entry of the resolved sidecar list, fixed for the
    /// process life, and the region of the rows arena a sidecar writes into.
    pub const MAX_SLOTS: usize = 16;

    /// Bytes of the rows arena one sidecar slot owns (docs/abi.md, *The arenas*).
    ///
    /// A `Rows` frame's body — header, rows, blob — goes in whole at the
    /// writer's own offsets, and nothing in it is pixels (a pixmap icon is a
    /// key, *Pixmap*). Sized from a measured tray: 141 rows with
    /// their labels and four icon keys is about 8 KB, so 64 KiB is roughly 900
    /// rows' worth — past the wire's `MAX_ROWS`, which bounds a
    /// frame first. A body past it is one witness and the region kept, so a
    /// wrong guess is visible.
    pub const ROWS_ARENA_BYTES: usize = 65536;

    /// Bytes of the answers arena: every `Glyph` and `App` frame the embassy
    /// has received, concatenated (docs/abi.md, *The arenas*). When it would overflow, the oldest
    /// entries are dropped; a want the caustic pushes again is asked again.
    pub const ANSWERS_BYTES: usize = 131072;

    /// Reservations the caustic-owned region of the strings buffer holds room
    /// for, [`COMPOSE_MAX_BYTES`] each — 6 KiB of the 64.
    ///
    /// Two are the clock's compositions. The rest are `bar_build`'s: a dock
    /// entry with no icon wears the first letter of its app id, **uppercased**,
    /// and a letter is bytes the CPU never wrote — so the pass takes a
    /// reservation and writes them itself. A dock longer than this gets no
    /// letter on the tail, which is a blank chip rather than a wrong one.
    pub const MAX_COMPOSED: usize = 64;

    /// Bytes the CPU reserves for a composed block, and the cap the caustic
    /// writes under. A multiple of four, so a composition owns whole words of
    /// the strings buffer and cannot touch a neighbour's bytes.
    pub const COMPOSE_MAX_BYTES: u32 = 96;
}

abi_consts! {
    WINDOW_FLAGS = "`WindowFact::flags`";

    pub const WFLAG_FOCUSED: u32 = 1 << 0;
    pub const WFLAG_URGENT: u32 = 1 << 1;
}

abi_consts! {
    APP_FLAGS = "`AppFact::flags`. What an app is *called* and what its icon is are not \
                 here: they are the apps sidecar's answer, in the answers arena, joined by \
                 the app id string (docs/abi.md, *The arenas*)";

    /// The entry is in the favourites file. Its place in the dock is its place
    /// in this table, which the embassy wrote in dock order.
    pub const AFLAG_FAVOURITE: u32 = 1 << 0;
}

abi_consts! {
    CAFFEINE_STATES = "`FrameInputs::caffeine` — the in-process idle inhibitor";

    pub const CAFFEINE_ABSENT: u32 = 0;
    pub const CAFFEINE_OFF: u32 = 1;
    pub const CAFFEINE_ON: u32 = 2;
}

abi_consts! {
    NOTHING = "The one index value that means \"nothing\", on both contracts";

    pub const NO_INDEX: u32 = 0xffff_ffff;
}

abi_consts! {
    TARGETS = "Which surface a dispatch, a pointer event or an element belongs to. \"Neither\" \
               is not a value here: the pointer's target is the caustic's own word in \
               `private`, and NO_INDEX is what it holds when there is no pointer";

    pub const TARGET_BAR: u32 = 0;
    pub const TARGET_POPUP: u32 = 1;
}

abi_consts! {
    EDGES = "Which edge the bar takes";

    pub const EDGE_TOP: u32 = 0;
    pub const EDGE_BOTTOM: u32 = 1;
}

abi_consts! {
    ACTIONS = "`Note::head` in the **action** ring (docs/abi.md, *The action ring*): what the caustic \
               decided, and the operands the embassy forwards without resolving. No payload \
               is an index into a table the embassy keeps — a window id is niri's, an app id \
               is a range of the strings buffer, a sidecar action is the wire body's words, \
               a popup is its geometry";

    /// An unwritten slot.
    pub const ACT_NONE: u32 = 0;
    /// `arg[0..2]`: the window's 64-bit id, low word first. niri `FocusWindow`.
    pub const ACT_ACTIVATE_WINDOW: u32 = 1;
    /// `arg[0..2]`: the window's id. niri `CloseWindow`.
    pub const ACT_CLOSE_WINDOW: u32 = 2;
    /// `arg[0..2]`: the window's id. The capture session.
    pub const ACT_PREVIEW_BEGIN: u32 = 3;
    /// No operands: stop capturing.
    pub const ACT_PREVIEW_END: u32 = 4;
    /// `arg[0..2]`: the app id's `(offset, length)` in the strings buffer.
    pub const ACT_SPAWN: u32 = 5;
    /// `arg[0..2]`: the app id's range. Append it to the favourites file.
    pub const ACT_PIN: u32 = 6;
    /// `arg[0..2]`: the app id's range. Take it out again.
    pub const ACT_UNPIN: u32 = 7;
    /// No operands.
    pub const ACT_TOGGLE_OVERVIEW: u32 = 8;
    /// No operands: the in-process inhibitor.
    pub const ACT_TOGGLE_CAFFEINE: u32 = 9;
    /// `arg[0..8]`: `slot, row, kind, button, pos_x(f), pos_y(f), value(f),
    /// token`. `slot` picks the child, and the next six (`row` through `value`) are copied to its stdin as an `ActionBody` (docs/abi.md, *Action*) word for word; `kind` is the **wire's** `ACTION_*`
    /// and `pos` is divided by the scale on the way. `token` is 1 when the row
    /// carries `RFLAG_ACTIVATION` and the kind is `ACTION_ACTIVATE`: the
    /// embassy asks for an activation token for that click.
    pub const ACT_SIDECAR: u32 = 10;
    /// `arg[0..9]`: `kind, slot, row, ax(f), ay(f), aw(f), ah(f), w, h`. A new
    /// `xdg_popup` of [`POPUP_MENU`] (with a grab), [`POPUP_PREVIEW`] or
    /// [`POPUP_TOAST`], anchored at the rect, `w × h` physical px; a menu's
    /// `slot` other than [`NO_INDEX`] also sends that sidecar an
    /// `ACTION_MENU_OPEN {row}`.
    pub const ACT_POPUP_OPEN: u32 = 11;
    /// `arg[0..2]`: the wanted size in physical px. Reposition the open popup.
    pub const ACT_POPUP_MOVE: u32 = 12;
    /// No operands: destroy it, and `ACTION_MENU_CLOSE` to the slot that got
    /// the open.
    pub const ACT_POPUP_CLOSE: u32 = 13;
    /// `arg[0..2]`: `EDGE_*` and the height in logical px.
    pub const ACT_ANCHOR: u32 = 14;
    /// No operands: the shape submission runs next frame.
    pub const ACT_REBUILD: u32 = 15;
    /// `arg[0..3]`: `off, len, source` — a text block's range and **which
    /// buffer the bytes are in** (`SRC_*` below, as [`ACT_WANT_ICON`]'s), out
    /// to `caustics-glyphs` as a `REQ_COVER` (docs/abi.md, *Request*). A block is a title or a
    /// literal in `strings`, a sidecar's row label in its slot's region, or an
    /// app's name in `answers`.
    pub const ACT_COVER: u32 = 16;
    /// `arg[0..4]`: `off, len, REQ_*, source` — a range, the kind of question,
    /// and **which buffer the bytes are in** (`SRC_*` below).
    pub const ACT_WANT_ICON: u32 = 17;
    /// `arg[0]`: how many milliseconds one `bar_shape` dispatch may take before
    /// the embassy calls it a runaway. The threshold is
    /// `SHAPE_WATCHDOG_MS` in the tunables and so the caustic's; the response —
    /// one witness, and no more shaping until the arena's front block is
    /// written again — is fixed and the embassy's, because only it holds the
    /// queue and the timestamps. Pushed on the frame the caustic's own memory
    /// is made, and again whenever a reload changes the number.
    pub const ACT_WATCHDOG: u32 = 18;
    /// `arg[0..(MAX_IMAGE_SLOTS + 31) / 32]`: a bitmask of the image slots
    /// this build placed in a `BLOCK_IMAGE`, slot `n` at bit `n % 32` of word
    /// `n / 32`. Pushed once per `bar_build`,
    /// **before** its `ACT_WANT_ICON`s, so the drain marks what is on the bar
    /// before it serves a want that may evict. The embassy copies the mask
    /// into the slot array whole — a set bit is drawn, a clear one is not —
    /// and decides nothing: only the caustic lays the blocks out, so only it
    /// can say which pictures are in use.
    pub const ACT_IMAGES_DRAWN: u32 = 19;
}

abi_consts! {
    ICON_SOURCES = "`ACT_WANT_ICON`'s fourth operand and `ACT_COVER`'s third (docs/abi.md \
                    *The action ring*): which buffer the bytes are in. A name or a label the caustic hands \
                    back lives in one of three places — a literal's or a title's bytes in \
                    `strings`, a row's `icon` or `label` in its slot's region of the rows \
                    arena, an `AppBody::icon` or `name` in `answers` — and a range alone \
                    cannot say which. A value below `MAX_SLOTS` is a rows region's slot index; the \
                    embassy reads the bytes out of its own copy of that buffer, because it \
                    wrote all three";

    /// The strings buffer.
    pub const SRC_STRINGS: u32 = MAX_SLOTS as u32;
    /// The answers arena.
    pub const SRC_ANSWERS: u32 = MAX_SLOTS as u32 + 1;
}

abi_consts! {
    WITNESSES = "`Note::aux` in the **witness** ring (docs/abi.md, *The witness ring*): the level the \
                 embassy prints the line at";

    pub const WITNESS_DEBUG: u32 = 0;
    pub const WITNESS_INFO: u32 = 1;
    pub const WITNESS_WARN: u32 = 2;
}

abi_consts! {
    BUTTONS = "Linux `BTN_*` codes, as `wl_pointer.button` delivers them and as they go into \
               `Command::args[0]` and come back in an `ACT_SIDECAR`'s operands";

    pub const BTN_LEFT: u32 = 0x110;
    pub const BTN_RIGHT: u32 = 0x111;
    pub const BTN_MIDDLE: u32 = 0x112;
}

abi_consts! {
    POPUP = "What a popup is for: [`ACT_POPUP_OPEN`]'s first operand. The popup's state machine is the caustic's: the popup is a message, and \
             `CMD_POPUP_MAPPED`/`CMD_POPUP_DONE` are the answers";

    /// A menu: the embassy takes a pointer grab for it.
    pub const POPUP_MENU: u32 = 1;
    /// A window preview: no grab, and the pointer keeps reaching the bar.
    pub const POPUP_PREVIEW: u32 = 2;
    /// A toast: an applet's attention rows shown without a click
    /// (`RFLAG_SHOW`). No grab, and no `ACTION_MENU_OPEN`:
    /// the sidecar asked for it, and a toast is not its menu being read.
    pub const POPUP_TOAST: u32 = 3;
}

abi_consts! {
    COMMANDS = "`Command::tag` — what the embassy put in the ring";

    pub const CMD_NONE: u32 = 0;
    pub const CMD_POINTER_MOTION: u32 = 1;
    pub const CMD_POINTER_DOWN: u32 = 2;
    pub const CMD_POINTER_UP: u32 = 3;
    pub const CMD_POINTER_LEAVE: u32 = 4;
    pub const CMD_POPUP_MAPPED: u32 = 5;
    pub const CMD_POPUP_DONE: u32 = 6;
    /// `args[0]`: the sidecar slot whose region of the rows arena was just
    /// rewritten (docs/abi.md, *The arenas*). **`bar_apply` decides whether that is
    /// a rebuild**: it compares the region's shape against the copy it keeps in
    /// `private` and pushes `ACT_REBUILD` only if they differ, so a slider tick
    /// is one `write_buffer`, one command and one frame with no model pass.
    pub const CMD_ROWS: u32 = 7;
    /// The embassy's wall clock says `wants_redraw` has been held too long
    /// (docs/abi.md, *The witness ring*): answer with one `stall:` witness per moving
    /// element. The *trigger* is the embassy's because the pass that would
    /// notice from `dt` is the one that stops running when frames do.
    pub const CMD_DUMP_STALL: u32 = 8;
    /// A wheel or touchpad scroll on `target` at `pos`.
    /// `args[0]`: the vertical distance in px as an `i32`'s bits (logical px
    /// × the scale, positive down); `args[1]`: the discrete steps, likewise,
    /// for a device that sends only those. Horizontal is not carried. What a
    /// scroll moves — a menu taller than the screen — and what a step is
    /// worth are the caustic's.
    pub const CMD_AXIS: u32 = 9;
}

/// Every constant group of the buffer, in the order `bar_abi.slang`
/// renders them.
pub(crate) const CONST_GROUPS: &[ConstGroup] = &[
    CAPACITIES,
    WINDOW_FLAGS,
    APP_FLAGS,
    CAFFEINE_STATES,
    NOTHING,
    TARGETS,
    EDGES,
    ACTIONS,
    ICON_SOURCES,
    WITNESSES,
    BUTTONS,
    POPUP,
    COMMANDS,
];

// ---------------------------------------------------------------------------
// The structs (docs/abi.md, *Structs*)
// ---------------------------------------------------------------------------

abi_struct! {
    /// Per-frame CPU inputs (uniform). 80 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct FrameInputs {
        /// The frame this dispatch belongs to, ≥ 1 (docs/abi.md, *The two contracts*, rule
        /// 1). The embassy writes it before the first dispatch of a frame and
        /// advances it only once that frame's readback has completed, so a
        /// frame that failed at the swapchain keeps its stamp and the next
        /// attempt **appends** to the rings it already pushed into. 0 never
        /// occurs.
        pub frame: u32,
        /// Seconds since the last frame, clamped by the caustic.
        pub dt: f32,
        /// Seconds since the bar started.
        pub time: f32,
        /// Buffer scale (px per logical px).
        pub scale: u32,
        /// `EDGE_*` the surface *is* anchored to.
        pub edge: u32,
        /// `CAFFEINE_*`: the in-process idle inhibitor's state. A per-frame
        /// input because it is a Wayland object the embassy owns and not a
        /// fact about content, so a toggle is a frame and never a rebuild.
        pub caffeine: u32,
        /// Valid entries in the command ring this frame.
        pub command_count: u32,
        pub _pad0: u32,
        /// The bar surface in px.
        pub bar_size: [u32; 2],
        /// The popup surface in px, (0, 0) while unmapped.
        pub popup_size: [u32; 2],
        /// The valid extent of the preview texture in texels, (0, 0) when no
        /// capture has landed. The texture itself is a fixed [`PREVIEW_MAX_DIM`]²
        /// slot.
        pub preview_size: [u32; 2],
        /// Local wall time as a number: `hour * 60 + minute`. The
        /// embassy asks the C library what time it is — that is I/O, and
        /// stays here — and the caustic writes the digits.
        pub clock_time: u32,
        /// Local date as a number: `weekday << 16 | month << 8 | day`, weekday
        /// 0 = Sunday, month 1..12 (v11).
        pub clock_date: u32,
        /// The output the bar's layer surface is on, in px: its logical size
        /// (`xdg_output`) times [`FrameInputs::scale`]. (0, 0) until the
        /// surface has entered an output the embassy knows the size of, and
        /// the caustic reads zero as "no bound". One fact
        /// copied from a protocol event: the embassy owns the `wl_output` and
        /// the surface's enter and leave, and decides nothing with it.
        pub output_size: [u32; 2],
        /// The local month as a number: `year << 16 | first << 8 | days` —
        /// the year, the weekday of the month's 1st (0 = Sunday) and how many
        /// days the month has. The calendar's grid is laid
        /// out from it and `clock_date`; the leap rule and the 1st's weekday
        /// are worked out beside the embassy's one local-time read.
        pub clock_month: u32,
        pub _pad1: u32,
    }
}

abi_struct! {
    /// Per-target dispatch parameters (uniform). 16 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct Params {
        /// `TARGET_*`.
        pub target: u32,
        /// The font's cap height in em (docs/abi.md, *Font*); 0 until a font has
        /// arrived. The caustic snaps pixel sizes against it.
        pub cap_height: f32,
        pub _pad0: u32,
        pub _pad1: u32,
    }
}

abi_struct! {
    /// One command in the ring. 32 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct Command {
        pub tag: u32,
        pub target: u32,
        pub pos: [f32; 2],
        /// Per-tag payload: DOWN/UP carry the `BTN_*` code in
        /// `args[0]`, MAPPED the popup size in px in `args[0..2]`,
        /// `CMD_ROWS` the sidecar slot in `args[0]`, and `CMD_AXIS` the px and
        /// the steps as signed words in `args[0..2]`.
        pub args: [u32; 4],
    }
}

impl Command {
    pub const fn pointer_motion(target: u32, pos: [f32; 2]) -> Self {
        Self {
            tag: CMD_POINTER_MOTION,
            target,
            pos,
            args: [0; 4],
        }
    }
    pub const fn pointer_down(target: u32, pos: [f32; 2], button: u32) -> Self {
        Self {
            tag: CMD_POINTER_DOWN,
            target,
            pos,
            args: [button, 0, 0, 0],
        }
    }
    pub const fn pointer_up(target: u32, pos: [f32; 2], button: u32) -> Self {
        Self {
            tag: CMD_POINTER_UP,
            target,
            pos,
            args: [button, 0, 0, 0],
        }
    }
    pub const fn pointer_leave(target: u32) -> Self {
        Self {
            tag: CMD_POINTER_LEAVE,
            target,
            pos: [-1.0, -1.0],
            args: [0; 4],
        }
    }
    pub const fn popup_mapped(width: u32, height: u32) -> Self {
        Self {
            tag: CMD_POPUP_MAPPED,
            target: TARGET_POPUP,
            pos: [0.0, 0.0],
            args: [width, height, 0, 0],
        }
    }
    pub const fn popup_done() -> Self {
        Self {
            tag: CMD_POPUP_DONE,
            target: TARGET_POPUP,
            pos: [0.0, 0.0],
            args: [0; 4],
        }
    }
    /// "Slot `slot`'s region of the rows arena has been rewritten; decide for
    /// yourself whether that is a rebuild" (docs/abi.md, *The arenas*).
    pub const fn rows(slot: u32) -> Self {
        Self {
            tag: CMD_ROWS,
            target: TARGET_BAR,
            pos: [0.0, 0.0],
            args: [slot, 0, 0, 0],
        }
    }
    /// "You have been animating for seconds; say what is holding you awake"
    /// (docs/abi.md, *The witness ring*). The embassy's wall clock is the trigger and the
    /// caustic's numbers are the answer.
    pub const fn dump_stall() -> Self {
        Self {
            tag: CMD_DUMP_STALL,
            target: TARGET_BAR,
            pos: [0.0, 0.0],
            args: [0; 4],
        }
    }
    /// "The wheel moved this far over `target`": px and
    /// steps as the compositor sent them, each an `i32` carried as its bits.
    pub const fn axis(target: u32, pos: [f32; 2], px: i32, steps: i32) -> Self {
        Self {
            tag: CMD_AXIS,
            target,
            pos,
            args: [px as u32, steps as u32, 0, 0],
        }
    }
}

abi_struct! {
    /// One literal of the running caustic's string table, in the pool the embassy
    /// publishes on every content rebuild. 16 B.
    ///
    /// The caustic names a literal by `getStringHash("…")`, slangc records the
    /// text, and the bar's `Strings` (bar/src/strings.rs) has the table; this is that table as the
    /// GPU can read it — the bytes appended to the strings buffer, and an index of
    /// `(hash, off, len)` beside them. What it buys is a composition that spells
    /// a word the embassy never learns the meaning of: `WEEKDAYS` and `MONTHS` are
    /// hashes in `caustics/bar_tunables.slang` and the date is written on the GPU.
    { #[repr(C)] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct Literal {
        /// The `getStringHash` value.
        pub hash: u32,
        /// Byte offset of the literal's bytes in the strings buffer.
        pub off: u32,
        /// Its byte length.
        pub len: u32,
        pub _pad0: u32,
    }
}

impl Literal {
    pub const ZERO: Literal = Literal {
        hash: 0,
        off: 0,
        len: 0,
        _pad0: 0,
    };
}

// ---------------------------------------------------------------------------
// The out channels (docs/abi.md, *The action ring*–*The one word*): one struct of polled words and
// two rings of records. This is the whole of what the caustic says.
// ---------------------------------------------------------------------------

abi_struct! {
    /// The caustic's words the embassy polls. 32 B.
    ///
    /// [`Surface::wants_redraw`] is the only one the embassy *acts* on without
    /// a message (docs/abi.md, *The one word*); everything else here is bookkeeping for the two rings.
    /// [`Surface::frame`] doubles as "has the caustic run at all", which is
    /// what the first-frame guards key on.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct Surface {
        /// [`FrameInputs::frame`] as last seen by `bar_apply`; 0 = the caustic
        /// has not run.
        pub frame: u32,
        /// 1 while something animates; the embassy schedules another frame.
        pub wants_redraw: u32,
        /// The frame the action ring belongs to (*The three rules the buffer adds*, rule 1).
        pub action_frame: u32,
        /// Live entries in [`State::actions`].
        pub action_count: u32,
        /// Pushes past [`MAX_ACTIONS`] in this frame. The ring drops the
        /// newest, because a lost action beats a repeated one.
        pub action_lost: u32,
        /// The frame the witness ring belongs to.
        pub witness_frame: u32,
        /// Live entries in [`State::witnesses`].
        pub witness_count: u32,
        /// Pushes past [`MAX_WITNESSES`] in this frame.
        pub witness_lost: u32,
    }
}

abi_struct! {
    /// One ring record, 48 B: a head, a level and ten words.
    ///
    /// In the action ring `head` is an `ACT_*` and the words are that kind's
    /// operands (docs/abi.md, *The action ring*). In the witness ring `head` is the `getStringHash` of a
    /// format literal and the words are what it substitutes (docs/abi.md, *The witness ring*). A float
    /// travels as its bit pattern, which is what makes one record serve both.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct Note {
        /// Action: `ACT_*`. Witness: `getStringHash` of the format literal.
        pub head: u32,
        /// Action: 0. Witness: `WITNESS_*`.
        pub aux: u32,
        pub arg: [u32; 10],
    }
}

impl Note {
    pub const ZERO: Note = Note {
        head: ACT_NONE,
        aux: 0,
        arg: [0; 10],
    };

    /// One operand as a float. The ring carries a float as `asuint` of it, so
    /// this is the one place the bits come back.
    pub fn arg_f(&self, index: usize) -> f32 {
        f32::from_bits(self.arg.get(index).copied().unwrap_or(0))
    }

    fn arg_u(&self, index: usize) -> u32 {
        self.arg.get(index).copied().unwrap_or(0)
    }

    /// The 64-bit window id an `ACT_ACTIVATE_WINDOW`, `ACT_CLOSE_WINDOW` or
    /// `ACT_PREVIEW_BEGIN` carries — niri's own, forwarded verbatim.
    pub fn window_id(&self) -> u64 {
        u64::from(self.arg_u(0)) | (u64::from(self.arg_u(1)) << 32)
    }

    /// The `(offset, length)` into the strings buffer an `ACT_SPAWN`,
    /// `ACT_PIN` or `ACT_UNPIN` carries.
    pub fn string_range(&self) -> (u32, u32) {
        (self.arg_u(0), self.arg_u(1))
    }

    /// An `ACT_SIDECAR`'s operands: which slot, which row, and the wire's own
    /// `ActionBody` words.
    pub fn sidecar(&self) -> SidecarArgs {
        SidecarArgs {
            slot: self.arg_u(0),
            row: self.arg_u(1),
            kind: self.arg_u(2),
            button: self.arg_u(3),
            pos: [self.arg_f(4), self.arg_f(5)],
            value: self.arg_f(6),
            token: self.arg_u(7) != 0,
        }
    }

    /// An `ACT_POPUP_OPEN`'s operands.
    pub fn popup_open(&self) -> PopupOpen {
        PopupOpen {
            kind: self.arg_u(0),
            slot: self.arg_u(1),
            row: self.arg_u(2),
            anchor: [self.arg_f(3), self.arg_f(4), self.arg_f(5), self.arg_f(6)],
            want: [self.arg_u(7), self.arg_u(8)],
        }
    }

    /// An `ACT_POPUP_MOVE`'s wanted size, physical px.
    pub fn popup_size(&self) -> [u32; 2] {
        [self.arg_u(0), self.arg_u(1)]
    }

    /// An `ACT_ANCHOR`'s `(EDGE_*, height in logical px)`.
    pub fn anchor(&self) -> (u32, u32) {
        (self.arg_u(0), self.arg_u(1))
    }

    /// An `ACT_WATCHDOG`'s threshold, in milliseconds.
    pub fn watchdog_ms(&self) -> u32 {
        self.arg_u(0)
    }

    /// An `ACT_IMAGES_DRAWN`'s mask: [`IMAGE_DRAWN_WORDS`] words, slot `n` at
    /// bit `n % 32` of word `n / 32`.
    pub fn images_drawn(&self) -> &[u32] {
        &self.arg[..IMAGE_DRAWN_WORDS]
    }

    /// An `ACT_COVER`'s operands: a range and the buffer it is a range of (a
    /// slot index, [`SRC_STRINGS`] or [`SRC_ANSWERS`]).
    pub fn cover(&self) -> Cover {
        Cover {
            off: self.arg_u(0),
            len: self.arg_u(1),
            source: self.arg_u(2),
        }
    }

    /// An `ACT_WANT_ICON`'s operands: a range, the wire's `REQ_*`, and the
    /// buffer the bytes are in (a slot index, [`SRC_STRINGS`] or
    /// [`SRC_ANSWERS`]).
    pub fn want_icon(&self) -> WantIcon {
        WantIcon {
            off: self.arg_u(0),
            len: self.arg_u(1),
            kind: self.arg_u(2),
            source: self.arg_u(3),
        }
    }
}

/// An [`ACT_SIDECAR`]'s operands, given their meanings. The embassy divides
/// `pos` by the surface scale and copies the six words into an `ActionBody`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidecarArgs {
    pub slot: u32,
    pub row: u32,
    /// The **wire's** `ACTION_*`, as the caustic named it.
    pub kind: u32,
    pub button: u32,
    /// Physical px in the target the click landed on.
    pub pos: [f32; 2],
    pub value: f32,
    /// The row carries `RFLAG_ACTIVATION` and this is an `ACTION_ACTIVATE`:
    /// ask the compositor for a token for the click (docs/architecture.md,
    /// *Notifications*).
    pub token: bool,
}

/// The words an [`ACT_IMAGES_DRAWN`] mask takes: one bit per image slot.
pub const IMAGE_DRAWN_WORDS: usize = MAX_IMAGE_SLOTS.div_ceil(32);
const _: () = assert!(
    IMAGE_DRAWN_WORDS <= 10,
    "the mask must fit a Note's ten words"
);

/// An [`ACT_POPUP_OPEN`]'s operands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupOpen {
    /// [`POPUP_MENU`], [`POPUP_PREVIEW`] or [`POPUP_TOAST`].
    pub kind: u32,
    /// The sidecar slot to send `ACTION_MENU_OPEN`, or [`NO_INDEX`].
    pub slot: u32,
    /// The row that open names.
    pub row: u32,
    /// The anchor rect in bar px.
    pub anchor: [f32; 4],
    /// The size asked for, physical px.
    pub want: [u32; 2],
}

/// An [`ACT_COVER`]'s operands (docs/abi.md, *The action ring*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cover {
    pub off: u32,
    pub len: u32,
    /// A sidecar slot, [`SRC_STRINGS`] or [`SRC_ANSWERS`].
    pub source: u32,
}

/// An [`ACT_WANT_ICON`]'s operands (docs/abi.md, *The action ring*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WantIcon {
    pub off: u32,
    pub len: u32,
    /// The wire's `REQ_*`, which is also which sidecar answers it.
    pub kind: u32,
    /// A sidecar slot, [`SRC_STRINGS`] or [`SRC_ANSWERS`].
    pub source: u32,
}

abi_struct! {
    /// The whole state buffer: what the embassy reads back, and the only
    /// buffer it does (docs/abi.md, *Who writes what*). 21536 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, PartialEq)] }
    pub struct State {
        pub surface: Surface,
        pub actions: [Note; MAX_ACTIONS],
        pub witnesses: [Note; MAX_WITNESSES],
    }
}

impl State {
    pub const ZERO: State = State {
        surface: Surface {
            frame: 0,
            wants_redraw: 0,
            action_frame: 0,
            action_count: 0,
            action_lost: 0,
            witness_frame: 0,
            witness_count: 0,
            witness_lost: 0,
        },
        actions: [Note::ZERO; MAX_ACTIONS],
        witnesses: [Note::ZERO; MAX_WITNESSES],
    };

    /// The action ring's live prefix, bounded by the ring's own length.
    ///
    /// Which of these the embassy has already acted on is the *caller's*
    /// bookkeeping: a ring belongs to the frame stamped on it (*The three rules the buffer adds*, rule 1), so
    /// a frame that failed at the swapchain keeps its stamp and the next
    /// attempt appends to what it pushed.
    pub fn live_actions(&self) -> &[Note] {
        &self.actions[..(self.surface.action_count as usize).min(MAX_ACTIONS)]
    }

    /// The witness ring's live prefix, likewise.
    pub fn live_witnesses(&self) -> &[Note] {
        &self.witnesses[..(self.surface.witness_count as usize).min(MAX_WITNESSES)]
    }
}

impl Default for State {
    fn default() -> Self {
        State::ZERO
    }
}

/// The state buffer: one [`State`] — the surface's words and the two rings.
/// The **only** buffer the embassy reads back (docs/abi.md, *Who writes what*).
pub const STATE_BUFFER_SIZE: u64 = size_of::<State>() as u64;
pub const COMMAND_RING_SIZE: u64 = size_of::<Command>() as u64 * MAX_COMMANDS as u64;
/// The run buffer: [`MAX_GLYPHS`] `punchcutter_shape::ShapedGlyph`s, 32 bytes each
/// (a glyph carries its cluster).
///
/// The 32 is that struct's size. It is not named here because a sidecar
/// importing this crate has no business pulling a font parser in with it; the
/// pair is pinned by `the_glyph_record_is_thirty_two_bytes` in
/// `bar/src/abi/tests.rs`, which does have `punchcutter-shape`.
///
/// Private in the same sense as `private` itself (docs/abi.md, *The private buffer*): the
/// embassy allocates it and never writes or reads one of them. It is a
/// binding rather than a field of `Private` because `shapeRun` writes through
/// an `RWStructuredBuffer<ShapedGlyph>` handle and an array inside a struct is
/// not one.
pub const GLYPH_BUFFER_SIZE: u64 = 32 * MAX_GLYPHS as u64;
/// The strings buffer: [`MAX_STRING_WORDS`] words of UTF-8.
pub const STRINGS_BUFFER_SIZE: u64 = 4 * MAX_STRING_WORDS as u64;
/// The literal pool: [`MAX_LITERALS`] [`Literal`]s.
pub const LITERALS_BUFFER_SIZE: u64 = size_of::<Literal>() as u64 * MAX_LITERALS as u64;
/// The facts buffer: one [`Facts`].
pub const FACTS_BUFFER_SIZE: u64 = size_of::<Facts>() as u64;
/// The rows arena: [`MAX_SLOTS`] regions of [`ROWS_ARENA_BYTES`], each holding
/// one sidecar's last `Rows` body verbatim (docs/abi.md, *The arenas*).
pub const ROWS_BUFFER_SIZE: u64 = (MAX_SLOTS * ROWS_ARENA_BYTES) as u64;
/// The answers arena: every `Glyph` and `App` body the embassy has received.
pub const ANSWERS_BUFFER_SIZE: u64 = ANSWERS_BYTES as u64;

// ---------------------------------------------------------------------------
// The facts (docs/abi.md, *The facts*): what the embassy *originates*, as tables.
// Everything is positional; every string is a range into the strings buffer;
// nothing is hashed on this side.
// ---------------------------------------------------------------------------

abi_struct! {
    /// The counts and the two offsets every other table is read against. 32 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct FactsHeader {
        /// Live entries of [`Facts::windows`].
        pub window_count: u32,
        /// Live entries of [`Facts::apps`].
        pub app_count: u32,
        /// Live entries of [`Facts::images`].
        pub image_count: u32,
        /// Live entries of [`Facts::literals`].
        pub literal_count: u32,
        /// Live words of `strings`, the composed region included; `compose_base`
        /// is where the process's own part ends.
        pub string_words: u32,
        /// Byte offset in the strings buffer of the caustic-owned composed
        /// region: [`MAX_COMPOSED`] × [`COMPOSE_MAX_BYTES`], zeroed by the CPU
        /// and written by `bar_build` and `bar_compose`. Word-aligned.
        pub compose_base: u32,
        /// Live bytes of the answers arena.
        pub answer_bytes: u32,
        /// Where the **app** answers begin in it: the arena is the `Glyph`
        /// bodies first and the `App` bodies after, because a body carries no
        /// kind word of its own and the two heads are different shapes
        /// (docs/abi.md, *The arenas*).
        pub glyph_bytes: u32,
    }
}

abi_struct! {
    /// One window niri reports, in niri's order. 32 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct WindowFact {
        /// The compositor's id, low 32 bits.
        pub id_lo: u32,
        /// ...high 24 bits; bits 24..31 are zero (the caustic packs the id into 56 bits beside an `Id` kind, caustics/bar_model.slang).
        pub id_hi: u32,
        /// Index into [`Facts::apps`] of the dock entry it hangs under, or
        /// [`NO_INDEX`] — the join the embassy makes because it is the one
        /// that knows both lists.
        pub app_row: u32,
        /// The title's bytes in the strings buffer.
        pub title_off: u32,
        pub title_len: u32,
        /// `WFLAG_*`.
        pub flags: u32,
        /// The model's monotonic focus counter, truncated: what orders an
        /// entry's windows.
        pub focus_stamp: u32,
        pub _pad0: u32,
    }
}

abi_struct! {
    /// One dock entry, **in dock order**: the favourites file's entries first,
    /// in file order, then every running app id not among them, in the order
    /// first seen. 16 B.
    ///
    /// What the entry is *called* and what its icon is are not here: they are
    /// the apps sidecar's answer in the answers arena, joined by this id's own
    /// bytes (docs/abi.md, *The arenas*).
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct AppFact {
        /// The app id's own bytes in the strings buffer, in the head region
        /// that never moves — the operand every `ACT_SPAWN`, `ACT_PIN` and
        /// `ACT_UNPIN` carries, and the name a departure witness quotes a
        /// frame later (docs/abi.md, *The facts*).
        pub id_off: u32,
        pub id_len: u32,
        /// `AFLAG_*`.
        pub flags: u32,
        pub _pad0: u32,
    }
}

abi_struct! {
    /// Which image slot holds which pixmap: the fixed-slot array's directory.
    /// 16 B.
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct ImageFact {
        /// The key the pixmap arrived under — an app id, an icon name, or a
        /// key a sidecar minted (docs/abi.md, *Pixmap*) — in the strings buffer.
        pub name_off: u32,
        pub name_len: u32,
        /// Layer of the image array.
        pub slot: u32,
        pub _pad0: u32,
    }
}

abi_struct! {
    /// One sidecar slot, by name. 16 B.
    ///
    /// Exists so a witness can say `applet-caustics-power-3` without the embassy
    /// naming anything; the slot's *rows* are its region of the rows arena
    /// (docs/abi.md, *The arenas*).
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct SlotFact {
        /// The sidecar's name — the file stem it was spawned as — in the head
        /// region of the strings buffer, which never moves. Length 0 = no
        /// child has ever taken this slot.
        pub name_off: u32,
        pub name_len: u32,
        pub _pad0: u32,
        pub _pad1: u32,
    }
}

abi_struct! {
    /// What the embassy knows, as one buffer. 10528 B, uploaded whole on every
    /// change (docs/abi.md, *The facts*).
    { #[repr(C, align(16))] #[derive(Clone, Copy, Debug, PartialEq)] }
    pub struct Facts {
        pub header: FactsHeader,
        pub windows: [WindowFact; MAX_WINDOWS],
        pub apps: [AppFact; MAX_APPS],
        pub images: [ImageFact; MAX_IMAGE_SLOTS],
        pub slots: [SlotFact; MAX_SLOTS],
        pub literals: [Literal; MAX_LITERALS],
    }
}

impl Facts {
    pub const ZERO: Facts = Facts {
        header: FactsHeader {
            window_count: 0,
            app_count: 0,
            image_count: 0,
            literal_count: 0,
            string_words: 0,
            compose_base: 0,
            answer_bytes: 0,
            glyph_bytes: 0,
        },
        windows: [WindowFact::ZERO; MAX_WINDOWS],
        apps: [AppFact::ZERO; MAX_APPS],
        images: [ImageFact::ZERO; MAX_IMAGE_SLOTS],
        slots: [SlotFact::ZERO; MAX_SLOTS],
        literals: [Literal::ZERO; MAX_LITERALS],
    };
}

impl Default for Facts {
    fn default() -> Self {
        Self::ZERO
    }
}

impl WindowFact {
    pub const ZERO: WindowFact = WindowFact {
        id_lo: 0,
        id_hi: 0,
        app_row: NO_INDEX,
        title_off: 0,
        title_len: 0,
        flags: 0,
        focus_stamp: 0,
        _pad0: 0,
    };
}

impl AppFact {
    pub const ZERO: AppFact = AppFact {
        id_off: 0,
        id_len: 0,
        flags: 0,
        _pad0: 0,
    };
}

impl ImageFact {
    pub const ZERO: ImageFact = ImageFact {
        name_off: 0,
        name_len: 0,
        slot: 0,
        _pad0: 0,
    };
}

impl SlotFact {
    pub const ZERO: SlotFact = SlotFact {
        name_off: 0,
        name_len: 0,
        _pad0: 0,
        _pad1: 0,
    };
}

/// The byte views every upload and every readback goes through, from
/// `punchcutter_core::plain` — the trait and its three functions are the text
/// engine's too; the layouts in this file are the bar's alone.
///
/// # Safety
///
/// An implementor must be `#[repr(C)]`, contain no padding bytes anywhere in
/// its layout, and consist only of fields for which every bit pattern is a
/// valid value (`u32`, `f32`, and arrays thereof). Every struct in this module
/// satisfies that by construction — the layout tests (bar/src/abi/tests.rs) pin every struct's size and every field's offset.
#[allow(unused_imports)]
pub use punchcutter_core::plain::{
    Plain, bytes_of as as_bytes, bytes_of_slice as slice_as_bytes, write_from_bytes,
};

unsafe impl Plain for FrameInputs {}
unsafe impl Plain for Params {}
unsafe impl Plain for Command {}
unsafe impl Plain for Literal {}
unsafe impl Plain for Surface {}
unsafe impl Plain for Note {}
unsafe impl Plain for State {}
unsafe impl Plain for FactsHeader {}
unsafe impl Plain for WindowFact {}
unsafe impl Plain for AppFact {}
unsafe impl Plain for ImageFact {}
unsafe impl Plain for SlotFact {}
unsafe impl Plain for Facts {}

// ---------------------------------------------------------------------------
// The table (docs/abi.md, *Declared once*): what `bar_abi.slang` is rendered
// from, in the order it renders them.
// ---------------------------------------------------------------------------

/// Every struct of the buffer, in rendering order.
pub const STRUCTS: &[AbiStruct] = &[
    AbiStruct {
        decl: FrameInputs::DECL,
        bound: true,
        offsets: FrameInputs::offsets,
    },
    AbiStruct {
        decl: Params::DECL,
        bound: true,
        offsets: Params::offsets,
    },
    AbiStruct {
        decl: Command::DECL,
        bound: true,
        offsets: Command::offsets,
    },
    AbiStruct {
        decl: Literal::DECL,
        bound: true,
        offsets: Literal::offsets,
    },
    AbiStruct {
        decl: Surface::DECL,
        bound: true,
        offsets: Surface::offsets,
    },
    AbiStruct {
        decl: Note::DECL,
        bound: true,
        offsets: Note::offsets,
    },
    AbiStruct {
        decl: State::DECL,
        bound: true,
        offsets: State::offsets,
    },
    AbiStruct {
        decl: FactsHeader::DECL,
        bound: true,
        offsets: FactsHeader::offsets,
    },
    AbiStruct {
        decl: WindowFact::DECL,
        bound: true,
        offsets: WindowFact::offsets,
    },
    AbiStruct {
        decl: AppFact::DECL,
        bound: true,
        offsets: AppFact::offsets,
    },
    AbiStruct {
        decl: ImageFact::DECL,
        bound: true,
        offsets: ImageFact::offsets,
    },
    AbiStruct {
        decl: SlotFact::DECL,
        bound: true,
        offsets: SlotFact::offsets,
    },
    AbiStruct {
        decl: Facts::DECL,
        bound: true,
        offsets: Facts::offsets,
    },
];

/// The rendered module's own header comment: kept beside the declarations it is about.
const MODULE_DOC: &[&str] = &[
    "The buffer (docs/abi.md, *The buffer*): the layout of the GPU buffers the bar's",
    "embassy and its caustic share. Deliberately inert — no bindings, no entry",
    "points, no state; `bar_state.slang` binds the caustic's own resources (docs/abi.md, *Who writes what*), and the",
    "caustic's own helper functions are `bar_model.slang`.",
    "",
    "Both halves are built from one tree, so this contract has no binary",
    "stability to keep across builds and BAR_ABI_VERSION is a label nothing",
    "reads. The other contract — what a *separately built*",
    "sidecar writes over a pipe — is `bar_wire.slang`, imported and re-exported",
    "below because three of its bodies reach the GPU as they stand: the samples",
    "body, a Rows frame in the rows arena, and a Glyph or App answer in the",
    "answers arena (docs/abi.md, *The arenas*).",
    "",
    "Layout rules (docs/abi.md, *Layout rules*): std430; every struct 16-byte aligned;",
    "every size a multiple of 16; padding is `_padN`, written zero and read by",
    "neither side, scalar words and never a `uint3` — std430 aligns a",
    "3-vector to 16 B and slangc's reflection duly put one at the wrong offset.",
];

/// `bar_abi.slang`, rendered from the tables above.
pub fn slang_module() -> String {
    crate::decl::render(
        crate::RENDERER,
        crate::NORMATIVE,
        "bar_abi",
        "bar/abi/src/buffer.rs",
        MODULE_DOC,
        &["__exported import bar_wire;"],
        CONST_GROUPS,
        STRUCTS,
    )
}

#[cfg(test)]
mod tests;
