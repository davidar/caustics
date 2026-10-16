//! **The wire** (docs/abi.md, *The wire*): what a sidecar writes to the bar over a
//! pipe, and what the bar writes back.
//!
//! The other contract, [`crate::buffer`], is compiled from one tree and needs
//! no binary stability. This one is the opposite: the two ends are separate
//! processes. So every frame carries [`WIRE_MAGIC`], its kind and its length,
//! and a reader skips a kind it does not know rather than failing.
//!
//! **What a kind's bytes mean is the contract's, and the contract has one
//! number**: [`crate::ABI_HASH`], which every sidecar states in its first
//! frame ([`Hello`], *Hello*) and a bar refuses a child over. A version word per
//! kind could not do that: ten of them were all correct while an
//! `caustics-glyphs` from another tree wrote a flattened `cmap` array into the
//! buffer number the arena had taken (docs/abi.md, *The ABI hash*).
//!
//! Two rules hold for every frame kind:
//!
//! - **Whole state.** A sidecar writes the complete picture whenever anything
//!   changes; there are no deltas, so a restart of either end needs no replay.
//! - **One `write_all`.** [`write`] emits header and body together and
//!   flushes, so a reader never sees half a frame from a writer that was
//!   killed mid-tick.
//!
//! # Writing a sidecar
//!
//! Depend on `caustics-bar-abi`, build a [`Frame`], call [`write`] whenever the
//! picture changes, and read stdin with [`read`] on a thread, exiting on
//! `Ok(None)` — the bar holds the write end, so its EOF is the sidecar's
//! notice to leave. `sysmon/` is the worked example for [`Samples`];
//! `examples/rows_demo.rs` is the worked example for [`Rows`].

use std::io::{self, Read, Write};

use crate::buffer::{IMAGE_SLOT_PX, NO_INDEX, Plain, as_bytes, write_from_bytes};
use crate::decl::{AbiStruct, ConstGroup};
use crate::{abi_consts, abi_struct};

/// `WireHeader::magic`: the bytes `CAUS`, read as a little-endian word.
///
/// It is checked on the first byte of every frame because the one mistake a
/// new sidecar makes is printing to stdout — and stdout is the wire.
pub const WIRE_MAGIC: u32 = 0x5355_4143;

/// A [`Samples`] body (docs/abi.md, *Samples*).
pub const WIRE_SAMPLES: u32 = 1;
/// A [`Rows`] body (docs/abi.md, *Rows*).
pub const WIRE_ROWS: u32 = 2;
/// An [`ActionBody`] (docs/abi.md, *Action*), the one kind the *bar* writes.
pub const WIRE_ACTION: u32 = 3;

/// A body longer than this is a broken writer, not a frame kind from the
/// future: the reader errors and the bar restarts the child.
///
/// **One mebibyte**, which is the number *Pixmap icons*'s pixmap icons asked for when a
/// `Rows` frame could carry several 256² of them. A row's icon is a name now,
/// and pixels travel one 16 KiB [`Pixmap`] at a time, so no body a
/// sidecar sends today comes near it — but the cap is a bound on a broken
/// writer, not a size estimate, and a `Module` chunk is sized against it.
///
/// It is not raised for big shaders: `bar_shape` with an OpenType shaper in it
/// is 1.53 MiB of SPIR-V, and [`Module`] has what [`Bytes`] has — an
/// `offset`, a `total`, and a body split across frames ([`Module::chunks`]).
/// A shader only grows (Unicode tables, syllable machines, paint graphs), and
/// it does not grow into this number.
///
/// Every fixed body is tiny ([`Samples`] is 8336 B), so this bounds only the
/// variable-size kinds — and it bounds them before a single byte is allocated,
/// which is the property the cap exists for.
pub const WIRE_MAX_BODY: usize = 1 << 20;

// ---------------------------------------------------------------------------
// The answer kinds (docs/abi.md, *Bytes*–*Module*): what a sidecar that turns a
// name into something drawable writes, and the one frame the bar
// writes back to ask for it. Every body below is the bar *copying bytes*: a
// `Bytes` lands in a GPU buffer at an offset, a `Pixmap` in an image slot, and
// the three small ones in a table the item build reads. Nothing is parsed
// past the counts that bound the copy.
//
// `Module` is the same shape one level down: SPIR-V and its reflection
// arrive as bytes and go to the device, and the bar runs no compiler.
// ---------------------------------------------------------------------------

/// A run of bytes for one of the caustic's buffers (docs/abi.md, *Bytes*).
pub const WIRE_BYTES: u32 = 4;
/// What the font is: its em, its cap height, where its glyph ids end (docs/abi.md, *Font*).
pub const WIRE_FONT: u32 = 5;
/// A name became a glyph id, or could not (docs/abi.md, *Glyph*).
pub const WIRE_GLYPH: u32 = 6;
/// **The bar → a sidecar**: resolve this name (docs/abi.md, *Request*).
pub const WIRE_REQUEST: u32 = 7;
/// An app id became a desktop entry: its name, icon, path and argv (docs/abi.md, *App*).
pub const WIRE_APP: u32 = 8;
/// A keyed image for one slot of the fixed-slot array (docs/abi.md, *Pixmap*).
pub const WIRE_PIXMAP: u32 = 9;
/// One entry point of the caustic, freshly compiled (docs/abi.md, *Module*).
pub const WIRE_MODULE: u32 = 10;
/// **A sidecar's first frame**: which contract it was built against (docs/abi.md, *Hello*).
pub const WIRE_HELLO: u32 = 11;

/// Which buffer a `Bytes` frame writes. The four curve buffers are
/// `caustics/text.slang`'s (`caustics-text`'s layout); `BUF_SHAPE_FONT` is the
/// `ShapeFont` half of `shapeParams`, and `BUF_OT` is the **arena** —
/// `caustics/ot.slang`'s directory and the faces' `cmap`, `hmtx`, `GDEF`,
/// `GSUB` and `GPOS` exactly as they sit in the font files.
///
/// One name where there were nine. The nine were nine flattened tables — a sorted `cmap`, an advance array, the PairPos
/// subtables and their class matrices, the ligature list, the chain rules —
/// and each was a shape the CPU had to understand in order to build. The
/// arena is bytes the CPU copied.
pub const BUF_POINTS: u32 = 1;
pub const BUF_BANDS: u32 = 2;
pub const BUF_BAND_CURVES: u32 = 3;
pub const BUF_GLYPHS: u32 = 4;
pub const BUF_SHAPE_FONT: u32 = 5;
pub const BUF_OT: u32 = 6;
/// The highest `BUF_*`; a `Bytes` frame naming more is skipped (docs/abi.md, *Bytes*).
pub const BUF_LAST: u32 = 6;

abi_consts! {
    CHARTS = "The samples body (docs/abi.md, *Samples*): how much history it carries, and what a row \
              *measures*. What a row of a given kind looks like — heat or line, which \
              colour — is `bar_tunables.slang`'s, and drawing it is `bar.slang`'s";

    /// Chart rows the body carries room for.
    pub const MAX_CHARTS: usize = 8;
    /// Columns per series: two minutes of history at one sample a second, with
    /// room to spare. Oldest first, right-aligned — the valid tail is the last
    /// `sample_count` columns.
    pub const CHART_SAMPLES: usize = 128;

    /// A measurement the bar has no name for (a third-party sidecar's).
    pub const CHART_OTHER: u32 = 0;
    /// CPU busy fraction 0..1, all CPUs (ceiling 1).
    pub const CHART_CPU: u32 = 1;
    /// A temperature in °C (ceiling 100).
    pub const CHART_TEMP: u32 = 2;
    /// Memory used fraction 0..1 (ceiling 1).
    pub const CHART_MEM: u32 = 3;
    /// Network bytes/s: series 0 down, series 1 up (ceiling 0 = auto).
    pub const CHART_NET: u32 = 4;
    /// Disk bytes/s: series 0 read, series 1 written (ceiling 0 = auto).
    pub const CHART_DISK: u32 = 5;
    /// GPU busy percent 0..100: series 0 the first vendor found (AMD before
    /// NVIDIA), series 1 the second when both exist (ceiling 100).
    pub const CHART_GPU: u32 = 6;
}

abi_consts! {
    APPLETS = "What an applet *is*, stated by the sidecar the way `CHART_*` is \
               (`RowsHeader::applet` on this wire, `Item::a` of an ITEM_APPLET in the \
               caustic's private item table), so the caustic can order applets by kind the way it orders items \
               by kind";

    /// An applet the bar has no name for (a third-party sidecar's).
    pub const APPLET_OTHER: u32 = 0;
    pub const APPLET_AUDIO: u32 = 1;
    pub const APPLET_NETWORK: u32 = 2;
    pub const APPLET_BLUETOOTH: u32 = 3;
    /// The system tray, from the `caustics-tray` sidecar: one `ROW_ITEM` per
    /// StatusNotifierItem in one frame.
    pub const APPLET_TRAY: u32 = 4;
    /// The power button and the battery, from the `caustics-power` sidecar: two
    /// `ROW_ITEM`s in one frame, the battery after the button.
    pub const APPLET_POWER: u32 = 5;
    /// The idle-inhibit toggle. The one applet value
    /// that is **not** a sidecar's: caffeine holds a `zwp_idle_inhibitor_v1` on
    /// the bar's own layer surface, which is what the bar's membership test
    /// keeps in the process (docs/architecture.md), so the embassy publishes this item
    /// itself. Its two states are told apart by the icon name alone, because a
    /// bar chip has no check affordance.
    pub const APPLET_CAFFEINE: u32 = 6;
    /// The notifications, from the `caustics-notify` sidecar:
    /// one `ROW_ITEM`, a bell with the unread count as its label, whose menu
    /// is do-not-disturb, the history newest first, and clear-all. The
    /// sidecar is the session's `org.freedesktop.Notifications` daemon, and
    /// publishes no chip while another daemon holds that name.
    pub const APPLET_NOTIFY: u32 = 7;
}

abi_consts! {
    ROWS = "`Rows` (docs/abi.md, *Rows*): how many rows one frame may carry, what a row *is*, and the \
            flags a row wears";

    /// Rows one `Rows` frame may carry.
    pub const MAX_ROWS: usize = 256;

    /// A bar item: `parent` must be [`NO_INDEX`].
    pub const ROW_ITEM: u32 = 1;
    /// A menu row under a bar item or under a `RFLAG_SUBMENU` row.
    pub const ROW_MENU: u32 = 2;
    /// A separator in the menu.
    pub const ROW_SEPARATOR: u32 = 3;
    /// A slider row in the menu.
    pub const ROW_SLIDER: u32 = 4;

    pub const RFLAG_DISABLED: u32 = 1 << 0;
    pub const RFLAG_CHECKABLE: u32 = 1 << 1;
    pub const RFLAG_CHECKED: u32 = 1 << 2;
    /// This row has children.
    pub const RFLAG_SUBMENU: u32 = 1 << 3;
    pub const RFLAG_ATTENTION: u32 = 1 << 4;
    /// On a `ROW_ITEM`: "show my rows without a click" — a toast.
    /// `caustics-notify` sets it on its chip while a notification younger than its
    /// toast time exists and do-not-disturb is off, and clears it with a fresh
    /// frame when the youngest expires. The caustic opens the item's
    /// `RFLAG_ATTENTION` rows as a popup without a grab while it is set. Bit 5
    /// was `RFLAG_ICON_PIXMAP` (retired: a row's icon is a name now and pixels
    /// go by `Pixmap`, *Pixmap icons*, *Pixmap*).
    pub const RFLAG_SHOW: u32 = 1 << 5;
    /// A bar item whose click opens its rows; without it a click on the bar
    /// item is an [`ACTION_ACTIVATE`] sent back.
    pub const RFLAG_HAS_MENU: u32 = 1 << 6;
    pub const RFLAG_RADIO: u32 = 1 << 7;
    /// A bar item whose label is its **title**, not text to draw beside the
    /// icon: the caustic draws the title's first character as the chip's
    /// placeholder while the icon is unresolved, and nothing of it once the
    /// icon lands. A tray item sets it.
    pub const RFLAG_LABEL_TITLE: u32 = 1 << 8;
    /// "An [`ACTION_ACTIVATE`] on this row wants an activation token": the
    /// caustic says so in the `ACT_SIDECAR` it
    /// pushes, and the embassy asks `xdg_activation_v1` for a token only then.
    /// The row's flag is the whole gate; the greeting's `HELLO_ACTIVATION` is
    /// retired, because it guarded nothing a row cannot say.
    /// `caustics-notify` sets it on the rows whose click invokes an action (a
    /// notification with a `default` action, a named action's row), never on
    /// do-not-disturb, clear-all, settings or a dismissal.
    pub const RFLAG_ACTIVATION: u32 = 1 << 9;
}

abi_consts! {
    ACTIONS = "`ActionBody::kind` (docs/abi.md, *Action*): what the bar tells a sidecar happened to one of \
               its rows. The caustic names one of these inside an `ACT_SIDECAR` and the \
               embassy forwards the number it was given, so they live here — a separately \
               built sidecar is the other end of them. `ACT_*` is the **buffer's** own action table \
               (docs/abi.md, *The action ring*); these are the wire's";

    pub const ACTION_ACTIVATE: u32 = 1;
    pub const ACTION_SECONDARY: u32 = 2;
    pub const ACTION_MIDDLE: u32 = 3;
    /// A slider moved (4 and 5 are unused; a preview is between the caustic and
    /// the embassy and never reaches a sidecar): the value rides in `ActionBody::value`, in the row's own
    /// units.
    pub const ACTION_SET_VALUE: u32 = 6;
    /// The bar opened the menu of a sidecar's row, and the row is the one the
    /// menu hangs off (docs/abi.md, *Action*). **The embassy writes this one,
    /// never the caustic**: it is a fact about the popup the embassy owns, and
    /// it reaches the sidecar as a `WIRE_ACTION` frame like every other kind.
    ///
    /// A sidecar may ignore it. `caustics-network` is the one that does not — it
    /// scans for the access points in range while its menu is up, and stops on
    /// [`ACTION_MENU_CLOSE`].
    pub const ACTION_MENU_OPEN: u32 = 7;
    /// That menu closed, by any route: a click elsewhere, a leaf row
    /// performing, the compositor dismissing the popup, or the bar exiting
    /// first. Same `row` as the [`ACTION_MENU_OPEN`] that preceded it.
    pub const ACTION_MENU_CLOSE: u32 = 8;
    /// A toast that showed this row has ended:
    /// the row was on screen, in a popup the compositor mapped, and is not
    /// now. One action per row the toast held — an [`ActionBody`] names one
    /// row — sent when the toast ends by any route (its chip stopped asking,
    /// a click, a menu replacing it, the compositor dismissing it). A toast
    /// that never mapped (refused, or replaced before it was) sends none.
    /// **The caustic writes it**; a sidecar that keeps no "shown" state
    /// ignores it.
    pub const ACTION_TOAST_SHOWN: u32 = 9;
}

abi_consts! {
    GLYPHS = "`GlyphBody::flags` (docs/abi.md, *Glyph*)";

    /// No theme has an icon of that name, or it is not curves — the bar
    /// remembers "not a glyph" and stops asking.
    pub const GLYPH_MISSING: u32 = 1 << 0;
}

abi_consts! {
    APPS = "`AppBody::flags` (docs/abi.md, *App*)";

    /// No desktop entry for that app id; the name is the app id.
    pub const APP_MISSING: u32 = 1 << 0;
    /// `icon` is a symbolic name — ask `caustics-glyphs` for it.
    pub const APP_ICON_SYMBOLIC: u32 = 1 << 1;
    /// The icon is pixels and arrives as a `Pixmap` keyed by the app id
    /// (before or after this frame; the bar draws whichever it has).
    pub const APP_ICON_PIXMAP: u32 = 1 << 2;
}

abi_consts! {
    REQUESTS = "`RequestBody::kind` (docs/abi.md, *Request*): what the bar asks a sidecar to resolve";

    /// A themed icon name → a `Glyph` (from `caustics-glyphs`).
    pub const REQ_GLYPH: u32 = 1;
    /// An app id → an `App` (from the apps sidecar).
    pub const REQ_APP: u32 = 2;
    /// A themed icon name that is *not* curves → a `Pixmap` keyed by the name
    /// (from the apps sidecar).
    pub const REQ_ICON: u32 = 3;
    /// The **bytes of a string** whose run lacked a glyph → `caustics-glyphs`
    /// appends whatever its merged font is missing and answers with `Bytes`.
    ///
    /// The one request whose `name` is not a name: it is the UTF-8 the bar
    /// already holds in its strings buffer, copied out of the block the caustic
    /// flagged. The bar decodes none of it — which codepoints are missing and
    /// which face has them is the sidecar's to work out — and the answer is the
    /// buffers growing, not a frame that names anything.
    pub const REQ_COVER: u32 = 4;
    /// The swapchain format's slangc define, exactly as `Gpu::dst_format_args`
    /// spells it — compile the caustic for *this* program (from the compiler
    /// sidecar). Sent once at startup, and remembered by the
    /// pipe, so a restarted compiler learns the format again with no bar code
    /// for it.
    pub const REQ_PROGRAM: u32 = 5;
    /// An **`xdg_activation_v1` token** for a click on one of the sidecar's
    /// rows: `arg` is the row id the `ACTION_ACTIVATE` named
    /// and `name` is the token the compositor gave the bar for that click.
    ///
    /// The one request that is not a question: nothing answers it, and it is
    /// sent only for a click on a row flagged [`RFLAG_ACTIVATION`], after the
    /// action it belongs to, never remembered and never replayed — a token
    /// for a child that has gone is as stale as the click.
    pub const REQ_ACTIVATION: u32 = 6;
    /// The highest `REQ_*`; a `Request` naming more is skipped (docs/abi.md, *Request*).
    pub const REQ_LAST: u32 = 6;
}

/// Longest name a `Glyph`, `Request`, `App` field or `Pixmap` key may carry.
///
/// A minted `Pixmap` key (docs/abi.md, *Pixmap*) is the sidecar's name, a colon and sixteen
/// hex digits — `caustics-tray:` and the digits are 26 bytes — so the convention
/// sits far inside it.
pub const MAX_NAME_LEN: usize = 1024;

/// FNV-1a 64 — what a sidecar hashes a pixmap's texels with to mint its key
/// (docs/abi.md, *Pixmap*, [`pixmap_key`]).
///
/// In the ABI crate because the key convention is the wire's: a sidecar that
/// mints keys and the tests that check them must agree on the digits.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The reference form of a minted `Pixmap` key (docs/abi.md, *Pixmap*):
/// `<sidecar>:<fnv1a64 of the rgba, 16 hex digits>`.
///
/// Unique bar-wide because the sidecar's name leads, and **content-addressed**:
/// an icon that comes back unchanged is the same key and is never sent again,
/// and one that changed — Dropbox's sync animation — is a new key, so no slot
/// ever holds a stale picture under a live name.
pub fn pixmap_key(sidecar: &str, rgba: &[u8]) -> String {
    format!("{sidecar}:{:016x}", fnv1a64(rgba))
}

/// Entry points one generation of a `Module` run may carry (docs/abi.md, *Module*). The
/// caustic has six; the bound is here so a malformed `count` cannot make the
/// bar hold an arbitrary number of half-generations.
pub const MAX_MODULES: usize = 16;

// ---------------------------------------------------------------------------
// The framing
// ---------------------------------------------------------------------------

/// Every frame is one of these followed by `len` bytes of body. 12 B.
///
/// **Three words**, where there were four: the magic says the stream
/// is ours, the kind says what to do with the body and the length says how far
/// to the next frame, which is the whole of what a reader needs to skip a
/// frame it has no use for. The fourth was that kind's own layout version, and
/// [`crate::ABI_HASH`] retired all ten of them (docs/abi.md, *The ABI hash*).
///
/// This struct and [`Hello`]'s body are the two things that may never change
/// shape, because they are what a bar reads *before* it knows whether the
/// child is from its own tree.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WireHeader {
    /// [`WIRE_MAGIC`].
    pub magic: u32,
    /// `WIRE_*`: which body follows.
    pub kind: u32,
    /// Body bytes following this header.
    pub len: u32,
}

impl WireHeader {
    pub const SIZE: usize = 12;

    pub const fn new(kind: u32, len: u32) -> Self {
        WireHeader {
            magic: WIRE_MAGIC,
            kind,
            len,
        }
    }
}

unsafe impl Plain for WireHeader {}

// ---------------------------------------------------------------------------
// *Samples* Samples
// ---------------------------------------------------------------------------

abi_struct! {
    /// One chart, in the sidecar's order. 16 B.
    { #[repr(C)] #[derive(Clone, Copy, Debug, PartialEq)] }
    pub struct ChartRow {
        /// What the caustic divides by; 0 means auto — the window maximum,
        /// floored by `RATE_FLOOR`. Units are the sidecar's raw units.
        pub ceiling: f32,
        /// 1 or 2 series in this row.
        pub series: u32,
        /// `CHART_*`.
        pub kind: u32,
        pub _rsvd0: u32,
    }
}

impl ChartRow {
    pub const ZERO: ChartRow = ChartRow {
        ceiling: 0.0,
        series: 1,
        kind: crate::buffer::CHART_OTHER,
        _rsvd0: 0,
    };
}

abi_struct! {
    /// The chart strip's history: the one wire body the GPU reads, so it is also
    /// declared in `caustics/bar_abi.slang` and uploaded to binding 10 as-is.
    /// Fixed size, 8336 B.
    ///
    /// Chart `c`'s series `k` (0 or 1) is `samples[(2c + k) * CHART_SAMPLES + i]`,
    /// oldest first, right-aligned. A negative value is a gap (a device asleep, a
    /// rate with no delta yet) and breaks the line on either side of it. A row the
    /// sidecar could not fill still occupies its index, all gaps and `series = 1`,
    /// so indices never shift under the item table.
    { #[repr(C)] #[derive(Clone, Copy, Debug, PartialEq)] }
    pub struct Samples {
        /// Valid columns, `0..=CHART_SAMPLES`, the same for every row.
        pub sample_count: u32,
        /// Valid entries in `charts`.
        pub chart_count: u32,
        /// `CLOCK_MONOTONIC` seconds at the tick — the stall witness's clock.
        pub t: f32,
        pub _rsvd0: u32,
        pub charts: [ChartRow; MAX_CHARTS],
        pub samples: [f32; MAX_CHARTS * 2 * CHART_SAMPLES],
    }
}

/// `size_of::<Samples>()`, spelled out because the reader checks a frame's
/// `len` against it before decoding.
pub const SAMPLES_SIZE: usize = 16 + MAX_CHARTS * 16 + MAX_CHARTS * 2 * CHART_SAMPLES * 4;

impl Samples {
    /// An empty body: no rows, every column a gap. What a sidecar starts from,
    /// and what the bar uploads before the first tick lands.
    pub const ZERO: Samples = Samples {
        sample_count: 0,
        chart_count: 0,
        t: 0.0,
        _rsvd0: 0,
        charts: [ChartRow::ZERO; MAX_CHARTS],
        samples: [-1.0; MAX_CHARTS * 2 * CHART_SAMPLES],
    };

    /// Column `i` (oldest first) of series `k` of chart `c`, or a gap when any
    /// index is out of range.
    pub fn sample(&self, chart: usize, series: usize, i: usize) -> f32 {
        if chart >= MAX_CHARTS || series >= 2 || i >= CHART_SAMPLES {
            return -1.0;
        }
        self.samples[(2 * chart + series) * CHART_SAMPLES + i]
    }

    /// The counts are in range. The whole of what the reader checks past the
    /// framing — everything else in here is the caustic's to interpret.
    pub fn is_valid(&self) -> bool {
        self.sample_count as usize <= CHART_SAMPLES && self.chart_count as usize <= MAX_CHARTS
    }
}

unsafe impl Plain for ChartRow {}
unsafe impl Plain for Samples {}

// ---------------------------------------------------------------------------
// *Rows* Rows
// ---------------------------------------------------------------------------

abi_struct! {
    /// The header of a [`Rows`] frame. 16 B.
    { #[repr(C)] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct RowsHeader {
        /// `APPLET_*`: what this applet is (ordering, and the caustic's icon
        /// fallbacks).
        pub applet: u32,
        /// Rows following this header, at most [`MAX_ROWS`].
        pub row_count: u32,
        /// Blob bytes following the rows.
        pub bytes_len: u32,
        /// The writer's own counter; it appears in the bar's witness and means
        /// nothing else.
        pub revision: u32,
    }
}

abi_struct! {
    /// One row, as it crosses the wire: **the item table's own shape, with
    /// strings where the table has blocks**. 48 B.
    { #[repr(C)] #[derive(Clone, Copy, Debug, Default, PartialEq)] }
    pub struct Row {
        /// The sidecar's id for this row: unique in the frame, stable across
        /// frames, and what comes back in an [`ActionBody`].
        pub id: u32,
        /// Index of the parent row in this frame, or [`NO_INDEX`] for a bar item.
        pub parent: u32,
        /// `ROW_*`.
        pub role: u32,
        /// `RFLAG_*`.
        pub flags: u32,
        /// UTF-8 label in the blob (`label_len` 0 = none).
        pub label_off: u32,
        pub label_len: u32,
        /// Icon in the blob (`icon_len` 0 = none): a name in UTF-8 — themed, or
        /// a `Pixmap` key the sidecar sent (docs/abi.md, *Pixmap icons*). Never pixels.
        pub icon_off: u32,
        pub icon_len: u32,
        /// `ROW_SLIDER`: the position. `ROW_ITEM`: a level to draw on the icon, or
        /// −1.
        pub value: f32,
        /// `ROW_SLIDER`: the range the value lives in (`min < max`).
        pub min: f32,
        pub max: f32,
        /// A second UTF-8 range of the blob, **packed**: the offset in the low
        /// 16 bits, the length in the high 16 (0 = none). `ROW_MENU`: the body
        /// drawn wrapped under the label, which is then the row's title
        /// (a notification's body). Every role's range is validated; only a menu
        /// row's means anything. Sixteen bits each because the region a frame
        /// lands in is `ROWS_ARENA_BYTES`, 64 KiB, so no blob offset reaches
        /// 2^16.
        pub body: u32,
    }
}

impl Row {
    /// The body's `(offset, length)` in the blob, unpacked.
    pub fn body_range(&self) -> (u32, u32) {
        (self.body & BODY_FIELD_MAX, self.body >> 16)
    }
}

/// The largest offset or length [`Row::body`] can carry.
pub const BODY_FIELD_MAX: u32 = 0xFFFF;

unsafe impl Plain for RowsHeader {}
unsafe impl Plain for Row {}

/// A decoded [`WIRE_ROWS`] frame: an applet's whole picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    /// `APPLET_*`.
    pub applet: u32,
    /// The writer's own counter, for the witness line.
    pub revision: u32,
    pub rows: Vec<Row>,
    /// Labels and icons, addressed by the rows' offsets.
    pub bytes: Vec<u8>,
}

impl Rows {
    /// Row `i`'s label, or `None` when it has none. Invalid UTF-8 becomes
    /// U+FFFD rather than rejecting the frame: a label is a human's text and a
    /// mangled one is still better than a missing applet.
    pub fn label(&self, i: usize) -> Option<std::borrow::Cow<'_, str>> {
        let row = self.rows.get(i)?;
        let bytes = self.slice(row.label_off, row.label_len)?;
        Some(String::from_utf8_lossy(bytes))
    }

    /// Row `i`'s body, or `None` when it has none.
    /// Invalid UTF-8 becomes U+FFFD, as a label's does.
    pub fn body(&self, i: usize) -> Option<std::borrow::Cow<'_, str>> {
        let row = self.rows.get(i)?;
        let (off, len) = row.body_range();
        let bytes = self.slice(off, len)?;
        Some(String::from_utf8_lossy(bytes))
    }

    /// Row `i`'s icon name — themed, or a `Pixmap` key (docs/abi.md, *Pixmap icons*) — or `None`
    /// when it has none or its bytes are not UTF-8.
    pub fn icon(&self, i: usize) -> Option<&str> {
        let row = self.rows.get(i)?;
        let bytes = self.slice(row.icon_off, row.icon_len)?;
        std::str::from_utf8(bytes).ok()
    }

    fn slice(&self, off: u32, len: u32) -> Option<&[u8]> {
        if len == 0 {
            return None;
        }
        let start = off as usize;
        let end = start.checked_add(len as usize)?;
        self.bytes.get(start..end)
    }

    /// Body bytes this frame serialises to.
    pub fn body_len(&self) -> usize {
        16 + self.rows.len() * size_of::<Row>() + self.bytes.len()
    }
}

/// A handle to a row already added to a [`RowsBuilder`]: how a child names its
/// parent without anyone counting indices by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRef(u32);

/// Builds a [`Rows`] frame: labels and icon names as `&str`,
/// parents by handle.
///
/// Rows come out in the order they were added, and a parent is always added
/// before its children — which is the invariant the reader checks, so a frame
/// built here always passes it.
#[derive(Clone, Debug)]
pub struct RowsBuilder {
    applet: u32,
    revision: u32,
    rows: Vec<Row>,
    bytes: Vec<u8>,
}

impl RowsBuilder {
    pub fn new(applet: u32, revision: u32) -> Self {
        RowsBuilder {
            applet,
            revision,
            rows: Vec::new(),
            bytes: Vec::new(),
        }
    }

    /// A bar item: an icon, an optional label, an optional level on the icon
    /// (−1 for none), and `RFLAG_HAS_MENU` when a click opens its rows.
    pub fn item(
        &mut self,
        id: u32,
        label: Option<&str>,
        icon: Option<&str>,
        level: f32,
        flags: u32,
    ) -> RowRef {
        self.push(id, NO_INDEX, ROW_ITEM, flags, label, icon, level, 0.0, 0.0)
    }

    /// A menu row under `parent`.
    pub fn menu(
        &mut self,
        parent: RowRef,
        id: u32,
        label: Option<&str>,
        icon: Option<&str>,
        flags: u32,
    ) -> RowRef {
        self.push(id, parent.0, ROW_MENU, flags, label, icon, 0.0, 0.0, 0.0)
    }

    /// A separator under `parent`.
    pub fn separator(&mut self, parent: RowRef, id: u32) -> RowRef {
        self.push(id, parent.0, ROW_SEPARATOR, 0, None, None, 0.0, 0.0, 0.0)
    }

    /// A slider row under `parent`, in the sidecar's own units: the bar
    /// normalises on the way in and de-normalises the action on the way out.
    pub fn slider(
        &mut self,
        parent: RowRef,
        id: u32,
        label: Option<&str>,
        value: f32,
        min: f32,
        max: f32,
        flags: u32,
    ) -> RowRef {
        self.push(
            id, parent.0, ROW_SLIDER, flags, label, None, value, min, max,
        )
    }

    /// Give `row` a body: a second string, drawn wrapped under its label
    /// (a notification's body text). The packing is 16 bits of offset and 16 of
    /// length, so a body that would start or run past 2^16 bytes of blob is
    /// not attached and this says so with `false`; a frame that large does
    /// not fit its region anyway. An empty body attaches nothing.
    pub fn body(&mut self, row: RowRef, body: &str) -> bool {
        let at = row.0 as usize;
        if at >= self.rows.len() {
            return false;
        }
        if body.is_empty() {
            self.rows[at].body = 0;
            return true;
        }
        let off = self.bytes.len();
        if off > BODY_FIELD_MAX as usize || body.len() > BODY_FIELD_MAX as usize {
            return false;
        }
        self.bytes.extend_from_slice(body.as_bytes());
        self.rows[at].body = (off as u32) | ((body.len() as u32) << 16);
        true
    }

    pub fn build(self) -> Rows {
        Rows {
            applet: self.applet,
            revision: self.revision,
            rows: self.rows,
            bytes: self.bytes,
        }
    }

    /// The frame, ready for [`write`].
    pub fn frame(self) -> Frame {
        Frame::Rows(self.build())
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        id: u32,
        parent: u32,
        role: u32,
        flags: u32,
        label: Option<&str>,
        icon: Option<&str>,
        value: f32,
        min: f32,
        max: f32,
    ) -> RowRef {
        let (label_off, label_len) = self.blob(label.map(str::as_bytes));
        let (icon_off, icon_len) = self.blob(icon.map(str::as_bytes));
        let index = self.rows.len() as u32;
        self.rows.push(Row {
            id,
            parent,
            role,
            flags,
            label_off,
            label_len,
            icon_off,
            icon_len,
            value,
            min,
            max,
            body: 0,
        });
        RowRef(index)
    }

    fn blob(&mut self, bytes: Option<&[u8]>) -> (u32, u32) {
        match bytes {
            Some(bytes) if !bytes.is_empty() => {
                let off = self.bytes.len() as u32;
                self.bytes.extend_from_slice(bytes);
                (off, bytes.len() as u32)
            }
            _ => (0, 0),
        }
    }
}

// ---------------------------------------------------------------------------
// *Action* Action
// ---------------------------------------------------------------------------

/// What the bar writes to a sidecar's stdin when the caustic decided something
/// about one of its rows — or when the embassy has something to tell it about
/// the menu those rows are. Fixed size, 32 B.
///
/// The bar never waits for a reply: the sidecar's reply is its next [`Rows`]
/// frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActionBody {
    /// The `Row::id` the sidecar gave.
    pub row: u32,
    /// `ACTION_ACTIVATE`, `ACTION_SECONDARY`, `ACTION_MIDDLE`, `ACTION_SET_VALUE`,
    /// `ACTION_TOAST_SHOWN`, or — from the embassy rather than the caustic —
    /// `ACTION_MENU_OPEN` / `ACTION_MENU_CLOSE`, which a sidecar may ignore.
    pub kind: u32,
    /// `BTN_*` for a click, 0 otherwise.
    pub button: u32,
    pub _rsvd0: u32,
    /// Logical px in the bar surface (an SNI `Activate(x, y)` wants it).
    pub pos: [f32; 2],
    /// `ACTION_SET_VALUE`: the new value, in the row's own `min..max` — the bar
    /// de-normalises before writing it.
    pub value: f32,
    pub _rsvd1: u32,
}

unsafe impl Plain for ActionBody {}

// ---------------------------------------------------------------------------
// Reading and writing
// ---------------------------------------------------------------------------

/// Why a frame was skipped rather than decoded. Each is one witness line, and
/// the reader's caller counts them one per kind per child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// A frame kind this crate does not have: a sidecar from a tree with more
    /// kinds in it than this one. It got past the [`Hello`] gate, so the
    /// contract is the same and the kind is genuinely additional.
    Kind,
    /// `len` wrong for a fixed-size body, or inconsistent with the counts in a
    /// variable one.
    Length,
    /// The body decoded but did not pass *Rows*'s validation.
    Body,
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Skip::Kind => "unknown kind",
            Skip::Length => "bad length",
            Skip::Body => "malformed body",
        })
    }
}

/// A frame that was read and not decoded. The bytes are gone: the reader
/// consumed `len` of them, which is what "length-prefixed" buys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub kind: u32,
    pub why: Skip,
}

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "kind {}: {}", self.kind, self.why)
    }
}

/// One frame off the wire.
// ---------------------------------------------------------------------------
// The answer bodies (docs/abi.md, *Bytes*–*Pixmap*)
// ---------------------------------------------------------------------------

/// *Bytes* — the fixed head of a `WIRE_BYTES` body; `len` bytes follow.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BytesHeader {
    /// `BUF_*`.
    pub buffer: u32,
    /// Byte offset into that buffer where these bytes land.
    pub offset: u32,
    /// Bytes following this header.
    pub len: u32,
    /// The buffer's live length in bytes once this write has landed — what
    /// the bar sizes (and grows) the GPU buffer to, and what the caustic's
    /// counts are derived from. Never less than `offset + len`.
    pub total: u32,
}
unsafe impl Plain for BytesHeader {}

/// A `WIRE_BYTES` frame, decoded: where it goes and what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bytes {
    pub buffer: u32,
    pub offset: u32,
    pub total: u32,
    pub bytes: Vec<u8>,
}

/// *Font* — the fixed head of a `WIRE_FONT` body; `family_len` bytes of UTF-8
/// follow.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FontBody {
    /// Units per em.
    pub upem: u32,
    /// Cap height in em — what the caustic snaps a pixel size against.
    pub cap_height: f32,
    /// Glyph ids `0..glyph_count` are the font's own.
    pub glyph_count: u32,
    /// Glyph ids from here up are icons the same sidecar appends.
    pub icon_base: u32,
    pub family_len: u32,
    pub _rsvd0: u32,
    pub _rsvd1: u32,
    pub _rsvd2: u32,
}
unsafe impl Plain for FontBody {}

/// A `WIRE_FONT` frame, decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    pub upem: u32,
    pub cap_height: f32,
    pub glyph_count: u32,
    pub icon_base: u32,
    pub family: String,
}

abi_struct! {
    /// *Glyph* — the fixed head of a `WIRE_GLYPH` body; `name_len` bytes follow.
    { #[repr(C)] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct GlyphBody {
        /// The glyph id the name resolved to (meaningless with `GLYPH_MISSING`).
        pub gid: u32,
        /// `GLYPH_*`.
        pub flags: u32,
        pub name_len: u32,
        pub _rsvd0: u32,
    }
}
unsafe impl Plain for GlyphBody {}

/// A `WIRE_GLYPH` frame, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub gid: u32,
    pub flags: u32,
    pub name: String,
}

/// *Request* — the fixed head of a `WIRE_REQUEST` body; `name_len` bytes follow.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestBody {
    /// `REQ_*`.
    pub kind: u32,
    pub name_len: u32,
    /// The kind's one word: the row id for [`REQ_ACTIVATION`], zero for the
    /// others.
    pub arg: u32,
    pub _rsvd1: u32,
}
unsafe impl Plain for RequestBody {}

/// A `WIRE_REQUEST` frame, decoded: the bar asking a sidecar for a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub kind: u32,
    /// [`RequestBody::arg`]: the row for [`REQ_ACTIVATION`], else zero.
    pub arg: u32,
    pub name: String,
}

abi_struct! {
    /// *App* — the fixed head of a `WIRE_APP` body; the blob follows as app id,
    /// name, icon, path, then `exec_len` bytes of NUL-separated argv words.
    { #[repr(C)] #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] }
    pub struct AppBody {
        /// `APP_*`.
        pub flags: u32,
        pub app_len: u32,
        pub name_len: u32,
        pub icon_len: u32,
        pub path_len: u32,
        pub exec_len: u32,
        pub _rsvd0: u32,
        pub _rsvd1: u32,
    }
}
unsafe impl Plain for AppBody {}

/// A `WIRE_APP` frame, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub flags: u32,
    /// The app id that was asked for, echoed.
    pub app_id: String,
    /// `Name=`, or the app id under `APP_MISSING`.
    pub name: String,
    /// `Icon=`: a themed name (symbolic under `APP_ICON_SYMBOLIC`), or empty.
    pub icon: String,
    /// The `.desktop` file, for `gio launch`; empty under `APP_MISSING`.
    pub path: String,
    /// `Exec=` as argv words with the field codes stripped; empty if none.
    pub exec: Vec<String>,
}

/// *Pixmap* — the fixed head of a `WIRE_PIXMAP` body; `key_len` bytes of key
/// then `width * height * 4` bytes of straight (not premultiplied) RGBA8
/// follow. `width` and `height` are both `IMAGE_SLOT_PX`: the writer fits.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixmapBody {
    pub key_len: u32,
    pub width: u32,
    pub height: u32,
    pub _rsvd0: u32,
}
unsafe impl Plain for PixmapBody {}

/// A `WIRE_PIXMAP` frame, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixmap {
    pub key: String,
    pub width: u32,
    pub height: u32,
    /// Straight RGBA8, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// *Module* — the fixed head of a `WIRE_MODULE` body, and behind it **one piece**
/// of the module's payload.
///
/// The payload is `name_len` bytes of entry point, then `spirv_len` bytes of
/// SPIR-V, then `reflection_len` bytes of slangc's reflection JSON, in that
/// order. `total` is the whole of it and `offset` says where this frame's bytes
/// land in it — the shape [`BytesHeader`] has (docs/abi.md, *Bytes*), given to `Module` so that
/// a shader outgrowing [`WIRE_MAX_BODY`] is several frames
/// rather than a refused generation.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleHeader {
    /// Which compile this frame belongs to. A generation is complete when
    /// `count` entry points have arrived **whole**; a newer one starting
    /// abandons an older one that never completed.
    pub generation: u32,
    /// This entry point's index in the caustic's entry list, which is also
    /// the order the pipelines are built in. `index < count`.
    pub index: u32,
    /// Entry points in this generation.
    pub count: u32,
    /// [`format_code`] of the slangc define this module was compiled for. A
    /// generation compiled for another swapchain format is not this session's.
    pub format: u32,
    /// The whole module's three runs, repeated in every piece — so a reader can
    /// size the payload from the first frame it sees and cut it up after the
    /// last, without holding a piece's header apart from its bytes.
    pub spirv_len: u32,
    pub reflection_len: u32,
    pub name_len: u32,
    /// Byte offset of this frame's bytes within the payload.
    pub offset: u32,
    /// The payload's whole length: `name_len + spirv_len + reflection_len`.
    pub total: u32,
    pub _rsvd0: u32,
}
unsafe impl Plain for ModuleHeader {}

/// A `WIRE_MODULE` frame, decoded: one **piece** of one entry point.
///
/// A module that fits in a frame is one chunk at offset 0; a larger one is
/// several, in order. [`ModuleAssembly`] puts them back together, and it rather
/// than the bar is where that rule lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleChunk {
    pub generation: u32,
    pub index: u32,
    pub count: u32,
    pub format: u32,
    pub name_len: u32,
    pub spirv_len: u32,
    pub reflection_len: u32,
    pub offset: u32,
    pub total: u32,
    /// This piece of `name || spirv || reflection`.
    pub bytes: Vec<u8>,
}

/// One whole entry point of a fresh compile: what a writer holds before it cuts
/// the payload up, and what a reader holds once it has put it back together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub generation: u32,
    pub index: u32,
    pub count: u32,
    pub format: u32,
    /// The entry point's name, from the caustic's entry list.
    pub name: String,
    pub spirv: Vec<u8>,
    /// slangc's reflection of this entry point, verbatim.
    pub reflection: String,
}

impl Module {
    /// The payload this module's frames carry between them.
    fn payload(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(self.name.len() + self.spirv.len() + self.reflection.len());
        out.extend_from_slice(self.name.as_bytes());
        out.extend_from_slice(&self.spirv);
        out.extend_from_slice(self.reflection.as_bytes());
        out
    }

    /// Cut this module into frames whose body is at most `max_body` bytes,
    /// header included. Always at least one, even for an empty payload.
    ///
    /// `max_body` is [`WIRE_MAX_BODY`] in every caller; it is a parameter so a
    /// test can ask for a cap small enough to force several pieces without
    /// building a megabyte of SPIR-V to do it.
    pub fn chunks(&self, max_body: usize) -> Vec<ModuleChunk> {
        let payload = self.payload();
        let room = max_body.saturating_sub(size_of::<ModuleHeader>()).max(1);
        let total = payload.len() as u32;
        let piece = |offset: u32, bytes: Vec<u8>| ModuleChunk {
            generation: self.generation,
            index: self.index,
            count: self.count,
            format: self.format,
            name_len: self.name.len() as u32,
            spirv_len: self.spirv.len() as u32,
            reflection_len: self.reflection.len() as u32,
            offset,
            total,
            bytes,
        };
        if payload.is_empty() {
            return vec![piece(0, Vec::new())];
        }
        payload
            .chunks(room)
            .enumerate()
            .map(|(i, run)| piece((i * room) as u32, run.to_vec()))
            .collect()
    }
}

/// The pieces of one entry point, collected until the payload is whole.
///
/// The rule is *Module*'s and lives here rather than in the bar: a writer sends a
/// module's pieces **in order**, so a chunk whose `offset` is not where the last
/// one ended starts the payload over. That is the only state, and it cannot
/// grow past the `total` its header states.
#[derive(Clone, Debug, Default)]
pub struct ModuleAssembly {
    head: Option<ModuleChunk>,
    payload: Vec<u8>,
}

impl ModuleAssembly {
    pub fn new() -> ModuleAssembly {
        ModuleAssembly::default()
    }

    /// The generation this assembly is collecting, if any.
    pub fn generation(&self) -> Option<u32> {
        self.head.as_ref().map(|head| head.generation)
    }

    /// Bytes of payload in hand, and the payload's whole length.
    pub fn progress(&self) -> (usize, usize) {
        (
            self.payload.len(),
            self.head.as_ref().map_or(0, |head| head.total as usize),
        )
    }

    /// Forget what is held: the generation it belongs to is stale.
    pub fn clear(&mut self) {
        self.head = None;
        self.payload.clear();
    }

    /// File one piece; `Some` once the entry point is whole.
    ///
    /// A piece for another generation, or one that does not continue where the
    /// last left off, discards what was held: a compiler abandons a module only
    /// by being killed mid-write or by recompiling, and either way the bytes in
    /// hand are stale.
    pub fn push(&mut self, chunk: ModuleChunk) -> Option<Module> {
        let continues = match &self.head {
            Some(head) => {
                head.generation == chunk.generation
                    && head.index == chunk.index
                    && head.total == chunk.total
                    && self.payload.len() as u32 == chunk.offset
            }
            None => false,
        };
        if !continues {
            self.clear();
            if chunk.offset != 0 {
                // The tail of a module whose head this reader never saw.
                return None;
            }
        }
        self.payload.extend_from_slice(&chunk.bytes);
        if (self.payload.len() as u32) < chunk.total {
            self.head = Some(chunk);
            return None;
        }
        let payload = std::mem::take(&mut self.payload);
        self.head = None;
        let name_end = chunk.name_len as usize;
        let spirv_end = name_end + chunk.spirv_len as usize;
        Some(Module {
            generation: chunk.generation,
            index: chunk.index,
            count: chunk.count,
            format: chunk.format,
            name: String::from_utf8_lossy(&payload[..name_end]).into_owned(),
            spirv: payload[name_end..spirv_end].to_vec(),
            reflection: String::from_utf8_lossy(&payload[spirv_end..]).into_owned(),
        })
    }
}

/// The swapchain format a program was built for, as one word both ends
/// compute the same way from the define they each already hold: the bar from
/// `Gpu::dst_format_args`, the compiler sidecar from the `REQ_PROGRAM` it was
/// sent. FNV-1a over the define's bytes — not a digest and not meant to be,
/// only a comparison that neither side has to keep a table for.
// ---------------------------------------------------------------------------
// *Hello* Hello
// ---------------------------------------------------------------------------

/// *Hello* — the fixed head of a `WIRE_HELLO` body; `name_len` bytes of UTF-8
/// follow. 16 B.
///
/// The 64-bit hash crosses as two words because [`Plain`] is a promise about
/// `u32`/`f32` leaves and no padding, and because every other number on this
/// wire is a word. Low half first.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HelloBody {
    /// [`crate::ABI_HASH`], low 32 bits.
    pub hash_lo: u32,
    /// [`crate::ABI_HASH`], high 32 bits.
    pub hash_hi: u32,
    /// The name the sidecar calls itself, in the blob that follows.
    pub name_len: u32,
    /// Reserved: written zero, read by nobody. Bit 0 was `HELLO_ACTIVATION`,
    /// retired because a row's `RFLAG_ACTIVATION` already says everything it
    /// gated.
    pub flags: u32,
}
unsafe impl Plain for HelloBody {}

/// **A sidecar's first frame** (docs/abi.md, *Hello*): which contract it was built against.
///
/// The bar compares `abi_hash` with its own [`crate::ABI_HASH`] and, when they
/// differ, writes one witness naming the child and both numbers, kills it and
/// **does not start it again** — a sidecar from another tree is not a crash and
/// a backoff would only replay it. Any other kind before this one is refused
/// the same way: a process that will not say what it is has not said it is
/// ours.
///
/// `name` is for the witness. It is what the sidecar calls itself, which need
/// not be the file stem the bar spawned — a mismatch is worth a line and
/// nothing more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub abi_hash: u64,
    pub name: String,
    /// [`HelloBody::flags`]: reserved, zero.
    pub flags: u32,
}

/// Write the [`Hello`] every sidecar owes, to `out`.
///
/// The one call a sidecar's `main` makes before its `run`: `wire::hello(&mut
/// std::io::stdout(), env!("CARGO_BIN_NAME"))`.
pub fn hello<W: Write>(out: &mut W, name: &str) -> io::Result<()> {
    write(
        out,
        &Frame::Hello(Hello {
            abi_hash: crate::ABI_HASH,
            name: name.to_owned(),
            flags: 0,
        }),
    )
}

/// [`hello`] to this process's stdout, or one line on stderr and exit 1.
///
/// A sidecar that cannot greet the bar has nowhere to write its frames either,
/// so there is nothing for it to do afterwards. One `write_all` of a whole
/// line, per *Writing a sidecar*.
pub fn greet(name: &str) {
    if let Err(err) = hello(&mut io::stdout(), name) {
        let _ = io::stderr().write_all(format!("{name}: no wire to greet on: {err}\n").as_bytes());
        std::process::exit(1);
    }
}

/// The swapchain format a program was built for, as one word both ends
/// compute the same way from the define they each already hold: the bar from
/// `Gpu::dst_format_args`, the compiler sidecar from the `REQ_PROGRAM` it was
/// sent. FNV-1a over the define's bytes — not a digest and not meant to be,
/// only a comparison that neither side has to keep a table for.
pub fn format_code(define: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in define.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Samples(Box<Samples>),
    Rows(Rows),
    Action(ActionBody),
    /// *Bytes*: bytes for a GPU buffer.
    Bytes(Bytes),
    /// *Font*: the font's facts.
    Font(Font),
    /// *Glyph*: a name → a glyph id.
    Glyph(Glyph),
    /// *Request*: the bar asking for a name.
    Request(Request),
    /// *App*: an app id → its desktop entry.
    App(App),
    /// *Pixmap*: an image for a slot.
    Pixmap(Pixmap),
    /// *Module*: one piece of one entry point of a fresh compile.
    Module(ModuleChunk),
    /// *Hello*: a sidecar's first frame, naming the contract it was built
    /// against.
    Hello(Hello),
    /// Read whole, understood not at all — and that is fine (docs/abi.md, *Framing*, rules 5–7).
    Skipped(Skipped),
}

impl Frame {
    /// `WIRE_*` for the kinds this crate writes; the skipped frame's own.
    pub fn kind(&self) -> u32 {
        match self {
            Frame::Samples(_) => WIRE_SAMPLES,
            Frame::Rows(_) => WIRE_ROWS,
            Frame::Action(_) => WIRE_ACTION,
            Frame::Bytes(_) => WIRE_BYTES,
            Frame::Font(_) => WIRE_FONT,
            Frame::Glyph(_) => WIRE_GLYPH,
            Frame::Request(_) => WIRE_REQUEST,
            Frame::App(_) => WIRE_APP,
            Frame::Pixmap(_) => WIRE_PIXMAP,
            Frame::Module(_) => WIRE_MODULE,
            Frame::Hello(_) => WIRE_HELLO,
            Frame::Skipped(skipped) => skipped.kind,
        }
    }
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

/// Read one frame (docs/abi.md, *Framing*).
///
/// - `Ok(None)` — a clean close: EOF landed exactly on a frame boundary and
///   the other end is gone.
/// - `Ok(Some(Frame::Skipped(..)))` — a frame the reader consumed and did not
///   understand. The child keeps running; the caller witnesses it once.
/// - `Err` — the stream is not ours (a bad magic: a sidecar that printed to
///   stdout), an impossible length, or an EOF inside a header or a body.
pub fn read<R: Read>(input: &mut R) -> io::Result<Option<Frame>> {
    // 1. The header. EOF exactly here is the clean close.
    let mut head = [0u8; WireHeader::SIZE];
    let mut filled = 0;
    while filled < head.len() {
        match input.read(&mut head[filled..])? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(invalid("EOF inside a frame header")),
            n => filled += n,
        }
    }
    let mut header = WireHeader::default();
    if !write_from_bytes(&mut header, &head) {
        return Err(invalid("EOF inside a frame header"));
    }

    // 2. and 3. Not ours, or impossible.
    if header.magic != WIRE_MAGIC {
        return Err(invalid(&format!(
            "bad magic {:#010x} (a sidecar writing text to stdout?)",
            header.magic
        )));
    }
    let len = header.len as usize;
    if len > WIRE_MAX_BODY {
        return Err(invalid(&format!(
            "body of {len} bytes, over the {WIRE_MAX_BODY} cap"
        )));
    }

    // 4. The body, always consumed whole, whatever happens to it below.
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;

    let skip = |why: Skip| {
        Ok(Some(Frame::Skipped(Skipped {
            kind: header.kind,
            why,
        })))
    };

    // 5., 6. and 7. The kind, then the length, then the body.
    match header.kind {
        WIRE_SAMPLES => {
            if len != SAMPLES_SIZE {
                return skip(Skip::Length);
            }
            let mut samples = Box::new(Samples::ZERO);
            if !write_from_bytes(&mut *samples, &body) {
                return skip(Skip::Length);
            }
            if !samples.is_valid() {
                return skip(Skip::Body);
            }
            Ok(Some(Frame::Samples(samples)))
        }
        WIRE_ROWS => match decode_rows(&body) {
            Ok(rows) => Ok(Some(Frame::Rows(rows))),
            Err(why) => skip(why),
        },
        WIRE_ACTION => {
            if len != size_of::<ActionBody>() {
                return skip(Skip::Length);
            }
            let mut action = ActionBody::default();
            if !write_from_bytes(&mut action, &body) {
                return skip(Skip::Length);
            }
            Ok(Some(Frame::Action(action)))
        }
        WIRE_BYTES => match decode_bytes(&body) {
            Ok(bytes) => Ok(Some(Frame::Bytes(bytes))),
            Err(why) => skip(why),
        },
        WIRE_FONT => match decode_font(&body) {
            Ok(font) => Ok(Some(Frame::Font(font))),
            Err(why) => skip(why),
        },
        WIRE_GLYPH => match decode_glyph(&body) {
            Ok(glyph) => Ok(Some(Frame::Glyph(glyph))),
            Err(why) => skip(why),
        },
        WIRE_REQUEST => match decode_request(&body) {
            Ok(request) => Ok(Some(Frame::Request(request))),
            Err(why) => skip(why),
        },
        WIRE_APP => match decode_app(&body) {
            Ok(app) => Ok(Some(Frame::App(app))),
            Err(why) => skip(why),
        },
        WIRE_PIXMAP => match decode_pixmap(&body) {
            Ok(pixmap) => Ok(Some(Frame::Pixmap(pixmap))),
            Err(why) => skip(why),
        },
        WIRE_MODULE => match decode_module(&body) {
            Ok(module) => Ok(Some(Frame::Module(module))),
            Err(why) => skip(why),
        },
        WIRE_HELLO => match decode_hello(&body) {
            Ok(hello) => Ok(Some(Frame::Hello(hello))),
            Err(why) => skip(why),
        },
        _ => skip(Skip::Kind),
    }
}

/// Split a fixed `Plain` head off a body. `Skip::Length` if the body is
/// shorter than the head.
fn split_head<T: Plain + Default>(body: &[u8]) -> Result<(T, &[u8]), Skip> {
    let Some((head, tail)) = body.split_at_checked(size_of::<T>()) else {
        return Err(Skip::Length);
    };
    let mut value = T::default();
    if !write_from_bytes(&mut value, head) {
        return Err(Skip::Length);
    }
    Ok((value, tail))
}

/// Take `len` bytes off the front of a blob as a name: bounded by
/// `MAX_NAME_LEN`, lossily UTF-8 (a mangled name is replaced, not rejected,
/// as *Rows*'s labels are).
fn take_name(blob: &mut &[u8], len: u32) -> Result<String, Skip> {
    let len = len as usize;
    if len > MAX_NAME_LEN {
        return Err(Skip::Body);
    }
    let Some((name, rest)) = blob.split_at_checked(len) else {
        return Err(Skip::Length);
    };
    *blob = rest;
    Ok(String::from_utf8_lossy(name).into_owned())
}

fn decode_bytes(body: &[u8]) -> Result<Bytes, Skip> {
    let (head, tail): (BytesHeader, _) = split_head(body)?;
    if head.buffer == 0 || head.buffer > BUF_LAST {
        return Err(Skip::Body);
    }
    if tail.len() != head.len as usize {
        return Err(Skip::Length);
    }
    if (head.offset as u64 + head.len as u64) > head.total as u64 {
        return Err(Skip::Body);
    }
    Ok(Bytes {
        buffer: head.buffer,
        offset: head.offset,
        total: head.total,
        bytes: tail.to_vec(),
    })
}

fn decode_font(body: &[u8]) -> Result<Font, Skip> {
    let (head, mut tail): (FontBody, _) = split_head(body)?;
    if head.upem == 0 || !(head.cap_height > 0.0) || head.icon_base < head.glyph_count {
        return Err(Skip::Body);
    }
    let family = take_name(&mut tail, head.family_len)?;
    if !tail.is_empty() {
        return Err(Skip::Length);
    }
    Ok(Font {
        upem: head.upem,
        cap_height: head.cap_height,
        glyph_count: head.glyph_count,
        icon_base: head.icon_base,
        family,
    })
}

fn decode_glyph(body: &[u8]) -> Result<Glyph, Skip> {
    let (head, mut tail): (GlyphBody, _) = split_head(body)?;
    let name = take_name(&mut tail, head.name_len)?;
    if !tail.is_empty() {
        return Err(Skip::Length);
    }
    if name.is_empty() {
        return Err(Skip::Body);
    }
    Ok(Glyph {
        gid: head.gid,
        flags: head.flags,
        name,
    })
}

fn decode_request(body: &[u8]) -> Result<Request, Skip> {
    let (head, mut tail): (RequestBody, _) = split_head(body)?;
    if head.kind == 0 || head.kind > REQ_LAST {
        return Err(Skip::Body);
    }
    let name = take_name(&mut tail, head.name_len)?;
    if !tail.is_empty() {
        return Err(Skip::Length);
    }
    if name.is_empty() {
        return Err(Skip::Body);
    }
    Ok(Request {
        kind: head.kind,
        arg: head.arg,
        name,
    })
}

fn decode_app(body: &[u8]) -> Result<App, Skip> {
    let (head, mut tail): (AppBody, _) = split_head(body)?;
    let app_id = take_name(&mut tail, head.app_len)?;
    let name = take_name(&mut tail, head.name_len)?;
    let icon = take_name(&mut tail, head.icon_len)?;
    let path = take_name(&mut tail, head.path_len)?;
    let exec_blob = take_name(&mut tail, head.exec_len)?;
    if !tail.is_empty() {
        return Err(Skip::Length);
    }
    if app_id.is_empty() {
        return Err(Skip::Body);
    }
    let exec: Vec<String> = if exec_blob.is_empty() {
        Vec::new()
    } else {
        exec_blob.split('\0').map(str::to_owned).collect()
    };
    Ok(App {
        flags: head.flags,
        app_id,
        name,
        icon,
        path,
        exec,
    })
}

fn decode_pixmap(body: &[u8]) -> Result<Pixmap, Skip> {
    let (head, mut tail): (PixmapBody, _) = split_head(body)?;
    if head.width != IMAGE_SLOT_PX || head.height != IMAGE_SLOT_PX {
        return Err(Skip::Body);
    }
    let key = take_name(&mut tail, head.key_len)?;
    if key.is_empty() {
        return Err(Skip::Body);
    }
    let texels = (head.width as usize) * (head.height as usize) * 4;
    if tail.len() != texels {
        return Err(Skip::Length);
    }
    Ok(Pixmap {
        key,
        width: head.width,
        height: head.height,
        rgba: tail.to_vec(),
    })
}

fn decode_module(body: &[u8]) -> Result<ModuleChunk, Skip> {
    let (head, tail): (ModuleHeader, _) = split_head(body)?;
    if head.count == 0 || head.count as usize > MAX_MODULES || head.index >= head.count {
        return Err(Skip::Body);
    }
    // SPIR-V is words: a length that is not a multiple of four is not a
    // module, and an empty one is a compile that produced nothing.
    if head.spirv_len == 0 || head.spirv_len % 4 != 0 {
        return Err(Skip::Body);
    }
    if head.name_len == 0 || head.name_len as usize > MAX_NAME_LEN {
        return Err(Skip::Body);
    }
    // The three runs are the payload, every piece says so, and a piece lands
    // inside it. Everything here is a header the reader can check against
    // itself before a byte is copied.
    let runs =
        u64::from(head.name_len) + u64::from(head.spirv_len) + u64::from(head.reflection_len);
    if runs != u64::from(head.total) {
        return Err(Skip::Body);
    }
    if u64::from(head.offset) + tail.len() as u64 > u64::from(head.total) {
        return Err(Skip::Body);
    }
    Ok(ModuleChunk {
        generation: head.generation,
        index: head.index,
        count: head.count,
        format: head.format,
        name_len: head.name_len,
        spirv_len: head.spirv_len,
        reflection_len: head.reflection_len,
        offset: head.offset,
        total: head.total,
        bytes: tail.to_vec(),
    })
}

/// *Hello*: two words of hash and a name that is UTF-8 and not longer than a
/// name may be. Nothing else — this body is read before the contract is known
/// to be shared, so it checks only itself.
fn decode_hello(body: &[u8]) -> Result<Hello, Skip> {
    let (head, tail): (HelloBody, _) = split_head(body)?;
    if tail.len() != head.name_len as usize || tail.len() > MAX_NAME_LEN {
        return Err(Skip::Length);
    }
    let Ok(name) = std::str::from_utf8(tail) else {
        return Err(Skip::Body);
    };
    Ok(Hello {
        abi_hash: u64::from(head.hash_lo) | (u64::from(head.hash_hi) << 32),
        name: name.to_owned(),
        flags: head.flags,
    })
}

/// *Rows*'s validation, in one place: the frame is whole and its tree is a tree,
/// or the whole frame is skipped.
fn decode_rows(body: &[u8]) -> Result<Rows, Skip> {
    let Some((head, tail)) = body.split_at_checked(size_of::<RowsHeader>()) else {
        return Err(Skip::Length);
    };
    let mut header = RowsHeader::default();
    if !write_from_bytes(&mut header, head) {
        return Err(Skip::Length);
    }
    let row_count = header.row_count as usize;
    if row_count > MAX_ROWS {
        return Err(Skip::Length);
    }
    let rows_len = row_count * size_of::<Row>();
    let Some((row_bytes, blob)) = tail.split_at_checked(rows_len) else {
        return Err(Skip::Length);
    };
    if blob.len() != header.bytes_len as usize {
        return Err(Skip::Length);
    }

    let mut rows = Vec::with_capacity(row_count);
    for chunk in row_bytes.chunks_exact(size_of::<Row>()) {
        let mut row = Row::default();
        if !write_from_bytes(&mut row, chunk) {
            return Err(Skip::Length);
        }
        rows.push(row);
    }

    let within =
        |off: u32, len: u32| -> bool { len == 0 || (off as u64 + len as u64) <= blob.len() as u64 };
    for (i, row) in rows.iter().enumerate() {
        let (body_off, body_len) = row.body_range();
        if !within(row.label_off, row.label_len)
            || !within(row.icon_off, row.icon_len)
            || !within(body_off, body_len)
        {
            return Err(Skip::Body);
        }
        match row.role {
            ROW_ITEM => {
                if row.parent != NO_INDEX {
                    return Err(Skip::Body); // a bar item hangs from nothing
                }
            }
            ROW_MENU | ROW_SEPARATOR | ROW_SLIDER => {
                // Parents precede children, so the tree is acyclic by
                // construction and one pass is enough to check it.
                let parent = row.parent as usize;
                if row.parent == NO_INDEX || parent >= i {
                    return Err(Skip::Body);
                }
                let owner = &rows[parent];
                if owner.role != ROW_ITEM && owner.flags & RFLAG_SUBMENU == 0 {
                    return Err(Skip::Body);
                }
                if row.role == ROW_SLIDER && !(row.min < row.max) {
                    return Err(Skip::Body);
                }
            }
            _ => return Err(Skip::Body),
        }
    }

    Ok(Rows {
        applet: header.applet,
        revision: header.revision,
        rows,
        bytes: blob.to_vec(),
    })
}

/// A name field's bytes, bounded by `MAX_NAME_LEN` so a reader's bound is a
/// writer's error rather than a skipped frame.
fn name_bytes(name: &str) -> io::Result<&[u8]> {
    if name.len() > MAX_NAME_LEN {
        return Err(invalid(&format!(
            "a name of {} bytes, over MAX_NAME_LEN",
            name.len()
        )));
    }
    Ok(name.as_bytes())
}

/// Write one frame: header and body in **one** `write_all`, then a flush, so a
/// reader never sees half a frame from a writer that was killed mid-tick.
///
/// A [`Frame::Skipped`] cannot be written — there are no bytes left of it — and
/// is an error rather than a silent no-op.
pub fn write<W: Write>(out: &mut W, frame: &Frame) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    match frame {
        Frame::Samples(samples) => {
            buf.reserve(WireHeader::SIZE + SAMPLES_SIZE);
            let header = WireHeader::new(WIRE_SAMPLES, SAMPLES_SIZE as u32);
            buf.extend_from_slice(as_bytes(&header));
            buf.extend_from_slice(as_bytes(&**samples));
        }
        Frame::Rows(rows) => {
            if rows.rows.len() > MAX_ROWS {
                return Err(invalid("more than MAX_ROWS rows"));
            }
            let len = rows.body_len();
            if len > WIRE_MAX_BODY {
                return Err(invalid(&format!(
                    "rows body of {len} bytes, over the {WIRE_MAX_BODY} cap"
                )));
            }
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_ROWS, len as u32)));
            let header = RowsHeader {
                applet: rows.applet,
                row_count: rows.rows.len() as u32,
                bytes_len: rows.bytes.len() as u32,
                revision: rows.revision,
            };
            buf.extend_from_slice(as_bytes(&header));
            for row in &rows.rows {
                buf.extend_from_slice(as_bytes(row));
            }
            buf.extend_from_slice(&rows.bytes);
        }
        Frame::Action(action) => {
            let len = size_of::<ActionBody>();
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_ACTION, len as u32)));
            buf.extend_from_slice(as_bytes(action));
        }
        Frame::Bytes(bytes) => {
            if bytes.buffer == 0 || bytes.buffer > BUF_LAST {
                return Err(invalid("a Bytes frame names no buffer"));
            }
            let len = size_of::<BytesHeader>() + bytes.bytes.len();
            if len > WIRE_MAX_BODY {
                return Err(invalid(&format!(
                    "bytes body of {len} bytes, over the {WIRE_MAX_BODY} cap: chunk it"
                )));
            }
            let head = BytesHeader {
                buffer: bytes.buffer,
                offset: bytes.offset,
                len: bytes.bytes.len() as u32,
                total: bytes.total.max(bytes.offset + bytes.bytes.len() as u32),
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_BYTES, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            buf.extend_from_slice(&bytes.bytes);
        }
        Frame::Font(font) => {
            let family = name_bytes(&font.family)?;
            let len = size_of::<FontBody>() + family.len();
            let head = FontBody {
                upem: font.upem,
                cap_height: font.cap_height,
                glyph_count: font.glyph_count,
                icon_base: font.icon_base,
                family_len: family.len() as u32,
                ..FontBody::default()
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_FONT, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            buf.extend_from_slice(family);
        }
        Frame::Glyph(glyph) => {
            let name = name_bytes(&glyph.name)?;
            let len = size_of::<GlyphBody>() + name.len();
            let head = GlyphBody {
                gid: glyph.gid,
                flags: glyph.flags,
                name_len: name.len() as u32,
                _rsvd0: 0,
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_GLYPH, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            buf.extend_from_slice(name);
        }
        Frame::Request(request) => {
            let name = name_bytes(&request.name)?;
            let len = size_of::<RequestBody>() + name.len();
            let head = RequestBody {
                kind: request.kind,
                name_len: name.len() as u32,
                arg: request.arg,
                _rsvd1: 0,
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_REQUEST, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            buf.extend_from_slice(name);
        }
        Frame::App(app) => {
            let exec_blob = app.exec.join("\0");
            let fields = [
                name_bytes(&app.app_id)?,
                name_bytes(&app.name)?,
                name_bytes(&app.icon)?,
                name_bytes(&app.path)?,
                name_bytes(&exec_blob)?,
            ];
            let blob: usize = fields.iter().map(|f| f.len()).sum();
            let len = size_of::<AppBody>() + blob;
            let head = AppBody {
                flags: app.flags,
                app_len: fields[0].len() as u32,
                name_len: fields[1].len() as u32,
                icon_len: fields[2].len() as u32,
                path_len: fields[3].len() as u32,
                exec_len: fields[4].len() as u32,
                _rsvd0: 0,
                _rsvd1: 0,
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_APP, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            for field in fields {
                buf.extend_from_slice(field);
            }
        }
        Frame::Pixmap(pixmap) => {
            if pixmap.width != IMAGE_SLOT_PX || pixmap.height != IMAGE_SLOT_PX {
                return Err(invalid(
                    "a Pixmap frame must be fitted to IMAGE_SLOT_PX squared",
                ));
            }
            let texels = (pixmap.width as usize) * (pixmap.height as usize) * 4;
            if pixmap.rgba.len() != texels {
                return Err(invalid("a Pixmap frame's rgba does not match its size"));
            }
            let key = name_bytes(&pixmap.key)?;
            let len = size_of::<PixmapBody>() + key.len() + texels;
            let head = PixmapBody {
                key_len: key.len() as u32,
                width: pixmap.width,
                height: pixmap.height,
                _rsvd0: 0,
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_PIXMAP, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            buf.extend_from_slice(key);
            buf.extend_from_slice(&pixmap.rgba);
        }
        Frame::Module(chunk) => {
            let len = size_of::<ModuleHeader>() + chunk.bytes.len();
            if len > WIRE_MAX_BODY {
                return Err(invalid(&format!(
                    "module body of {len} bytes, over the {WIRE_MAX_BODY} cap: chunk it"
                )));
            }
            if chunk.spirv_len == 0 || chunk.spirv_len % 4 != 0 {
                return Err(invalid(
                    "a Module frame's SPIR-V is not a whole number of words",
                ));
            }
            if chunk.name_len == 0 || chunk.name_len as usize > MAX_NAME_LEN {
                return Err(invalid("a Module frame names no entry point"));
            }
            if chunk.count == 0 || chunk.count as usize > MAX_MODULES || chunk.index >= chunk.count
            {
                return Err(invalid(
                    "a Module frame's index is not inside its generation",
                ));
            }
            if chunk.name_len + chunk.spirv_len + chunk.reflection_len != chunk.total {
                return Err(invalid("a Module frame's three runs are not its total"));
            }
            if chunk.offset as usize + chunk.bytes.len() > chunk.total as usize {
                return Err(invalid("a Module frame's piece lands outside its payload"));
            }
            let head = ModuleHeader {
                generation: chunk.generation,
                index: chunk.index,
                count: chunk.count,
                format: chunk.format,
                spirv_len: chunk.spirv_len,
                reflection_len: chunk.reflection_len,
                name_len: chunk.name_len,
                offset: chunk.offset,
                total: chunk.total,
                _rsvd0: 0,
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_MODULE, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            buf.extend_from_slice(&chunk.bytes);
        }
        Frame::Hello(hello) => {
            let name = name_bytes(&hello.name)?;
            let len = size_of::<HelloBody>() + name.len();
            let head = HelloBody {
                hash_lo: hello.abi_hash as u32,
                hash_hi: (hello.abi_hash >> 32) as u32,
                name_len: name.len() as u32,
                flags: hello.flags,
            };
            buf.reserve(WireHeader::SIZE + len);
            buf.extend_from_slice(as_bytes(&WireHeader::new(WIRE_HELLO, len as u32)));
            buf.extend_from_slice(as_bytes(&head));
            buf.extend_from_slice(name);
        }
        Frame::Skipped(skipped) => {
            return Err(invalid(&format!(
                "a skipped frame has no bytes to write ({skipped})"
            )));
        }
    }
    out.write_all(&buf)?;
    out.flush()
}

// ---------------------------------------------------------------------------
// The table (docs/abi.md, *The two contracts*): what `caustics/bar_wire.slang` is
// rendered from — the part of this contract the **GPU** reads, and nothing
// else. The framing, the decoded owned types, the reader and the writer are
// the embassy's and have no Slang half.
// ---------------------------------------------------------------------------

/// Every constant group the wire renders, in order.
pub(crate) const CONST_GROUPS: &[ConstGroup] =
    &[CHARTS, APPLETS, ROWS, ACTIONS, GLYPHS, APPS, REQUESTS];

/// Every wire struct the GPU reads, or will: [`ChartRow`] and [`Samples`] are
/// bound by the samples buffer (docs/abi.md, *Samples*). The four heads after them are what the
/// caustic reads out of the two arenas — a `Rows` body in the
/// rows arena, a `Glyph` or `App` body in the answers arena — and `bound` is
/// **still** false for them, because slangc reflects the types a binding's
/// layout needs and the arenas are bound as `RWStructuredBuffer<uint>`: they
/// have to be, since `shapeRun` takes its text through an
/// `RWStructuredBuffer<uint>` handle and a row's label lives in the arena. So
/// the caustic reads these four word by word at literal offsets, and what
/// pins those offsets is `bar/src/abi.rs`'s `the_arenas_hold_what_the_wire_
/// can_send` (every field of `Row` and of the two heads, from `offset_of!`)
/// together with the wire's own tests in `wire/tests.rs`.
pub const STRUCTS: &[AbiStruct] = &[
    AbiStruct {
        decl: ChartRow::DECL,
        bound: true,
        offsets: ChartRow::offsets,
    },
    AbiStruct {
        decl: Samples::DECL,
        bound: true,
        offsets: Samples::offsets,
    },
    AbiStruct {
        decl: RowsHeader::DECL,
        bound: false,
        offsets: RowsHeader::offsets,
    },
    AbiStruct {
        decl: Row::DECL,
        bound: false,
        offsets: Row::offsets,
    },
    AbiStruct {
        decl: GlyphBody::DECL,
        bound: false,
        offsets: GlyphBody::offsets,
    },
    AbiStruct {
        decl: AppBody::DECL,
        bound: false,
        offsets: AppBody::offsets,
    },
];

/// The generated module's own header comment.
const MODULE_DOC: &[&str] = &[
    "The wire (docs/abi.md, *The wire*), as far as the GPU reads it: what a",
    "*separately built* sidecar wrote after its frame header, byte for byte.",
    "",
    "The bar parses none of it — `wire::read` checks the framing, the embassy",
    "copies the body, and the caustic is the only thing that reads a row. What a",
    "row of a given kind *looks like* is `bar_tunables.slang`'s, and drawing it",
    "is `bar.slang`'s.",
    "",
    "This contract is the opposite of the buffer's: the two ends are separate",
    "processes, so every frame carries a magic, its kind and its length, and a",
    "reader skips a kind it does not know. What the bytes of a kind *mean* is",
    "the contract's, and the contract has one number — ABI_HASH, which every",
    "sidecar states in its first frame and a bar refuses a child over (docs/abi.md, *Hello*).",
    "",
    "`bar_abi.slang` re-exports this module, so a file that imports the buffer",
    "sees these names too — one wire body (Samples) is a GPU buffer as it",
    "stands, and the applet and action words are the item table's vocabulary.",
];

/// `caustics/bar_wire.slang`, rendered from the tables above.
pub fn slang_module() -> String {
    crate::decl::render(
        crate::GEN_COMMAND,
        crate::NORMATIVE,
        "bar_wire",
        "bar/abi/src/wire.rs",
        MODULE_DOC,
        &[],
        CONST_GROUPS,
        STRUCTS,
    )
}

#[cfg(test)]
mod tests;
