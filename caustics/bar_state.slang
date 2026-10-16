// The bar's state: what every module of the caustic reads and writes.
//
// The bindings of descriptor sets 0 and 2, the `Private` struct and its
// buffer, `ensure_private`, the two rings (the action ring and the witness
// ring) and the witness-name vocabulary. Every other module of the caustic
// imports this one, and it imports none of them: it names no subject.
//
// It is a module that declares bindings, the one exception to the rule in
// caustics/README.md (*Conventions*): a binding declared here and imported
// is the same global shader parameter in every module and every entry point,
// and the embassy finds each one in slangc's reflection by its name, as it
// did when they were declared in `bar.slang`.

module bar_state;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import shape;
import text;

/// Rows one toast holds, whatever TOAST_MAX_ROWS says: the tunable is clamped
/// to this, because a toast's rows are kept in `Private` for its whole life
/// and a witness carries at most eight ids.
public static const uint TOAST_ROWS_CAP = 8u;

// ---------------------------------------------------------------------------
// A menu row's body: its caps, and one wrapped line.
// ---------------------------------------------------------------------------

/// Body lines laid out per row, whatever TOAST_BODY_LINES and MENU_BODY_LINES
/// say: the larger of the two is clamped to this.
public static const uint MAX_BODY_LINES = 8u;
/// Lines the whole item table's bodies may hold between them: forty
/// notifications (`caustics-notify`'s MENU_MAX) at the cap. A row past it shows its
/// label alone.
public static const uint MAX_BODY_POOL = 320u;
/// Blocks one body is shaped as, at most: a paragraph each, and a paragraph
/// longer than BODY_CHUNK_BYTES in more than one.
public static const uint MAX_BODY_BLOCKS = 8u;
/// The longest run a body block is: the shaper's MAX_RUN_GLYPHS less the
/// eight slack slots `body_slots` adds, about halved (and rounded to 120) so
/// no one thread of `bar_shape` walks much more than a label does.
public static const uint BODY_CHUNK_BYTES = 120u;

/// One wrapped line of a body, written by `wrap_bodies` and read by the layout
/// (its width) and the draw (its glyphs). A line is a run of consecutive
/// glyphs of one paragraph, which may cross from one of its chunks into the
/// next — so it is a start and a count, not a range of one block.
public struct BodyLine
{
    /// The block the line starts in.
    public uint block;
    /// Its first glyph, within that block's run.
    public uint glyph;
    /// Glyphs on the line, trailing spaces left off.
    public uint count;
    /// Glyphs of it that still leave room for the ellipsis after them, the
    /// last of them not a space: what the line draws when it is the last one
    /// shown of a body that goes on.
    public uint fit;
    /// The line's width, px: its last glyph's far edge less its first's pen.
    public float width;
    /// The same for its first `fit` glyphs: where the ellipsis goes.
    public float fit_w;
};

// ---------------------------------------------------------------------------
// Bind group 0, shared by every entry. The embassy builds the layouts and the
// bind group from slangc's reflection, mapping each binding by its *name*, so
// the `[[vk::binding]]` literals below are this file's to choose (an attribute
// cannot take a constant, hence literals), and a reload that changes one is
// refused rather than misread (docs/abi.md, *Who writes what*, *Bindings and the oracle*). Bindings a given
// entry does not use are dead-stripped from its module, which Vulkan permits.
// ---------------------------------------------------------------------------

/// **The three channels out** (docs/abi.md, *The action ring*–*The one word*), and the whole of
/// what this file says to the embassy: the action ring, the witness ring and
/// `wants_redraw`. Nothing else here is read on the other side.
[[vk::binding(0, 0)]]
public RWStructuredBuffer<State> state;

/// Every shaped glyph on the bar, `(gid, x, y, cluster, flags)` with the
/// positions in 26.6 fixed point: written
/// by `bar_shape`, read by `bar_draw`. Read-write, because one binding cannot
/// be two things and a compute pipeline does not mind.
///
/// **Private in the same sense as `private` below** — the embassy allocates
/// `MAX_GLYPHS × 16` bytes and never writes or reads one of them. It is a
/// binding rather than a field of `Private` because `shapeRun` writes through
/// an `RWStructuredBuffer<ShapedGlyph>` handle, and an array inside a struct
/// is not one.
[[vk::binding(1, 0)]]
public RWStructuredBuffer<ShapedGlyph> runs;

[[vk::binding(2, 0)]]
public StructuredBuffer<Command> commands;

[[vk::binding(3, 0)]]
public ConstantBuffer<FrameInputs> inputs;

[[vk::binding(4, 0)]]
public ConstantBuffer<Params> params;

[[vk::binding(5, 0)]]
[format("r32ui")]
public RWTexture2D<uint> ids;

#if defined(DST_FORMAT_RGBA8)
[[vk::binding(6, 0)]]
[format("rgba8")]
public WTexture2D<float4> dst;
#else
[[vk::binding(6, 0)]]
public WTexture2D<float4> dst; // format unknown; see the build note at the top of `bar.slang`
#endif

/// The image slots: MAX_IMAGE_SLOTS layers of IMAGE_SLOT_PX square,
/// **straight** RGBA as the wire carries it (docs/abi.md, *Pixmap*; the
/// sample premultiplies), holding everything the bar draws that is
/// pixels rather than curves (a colour SVG, a PNG, a tray pixmap). A BLOCK_IMAGE names one
/// layer and fills its box with the whole of it, so there are no uv rectangles
/// anywhere in this file.
[[vk::binding(7, 0)]]
public Texture2DArray<float4> images;

[[vk::binding(8, 0)]]
public SamplerState tex_sampler;

/// The one preview slot: a fixed 2048² texture whose valid extent is
/// `inputs.preview_size`. Premultiplied RGBA, unlike the slots — the capture
/// feeder hands the embassy straight alpha and the embassy premultiplies on
/// upload, so this file never unpremultiplies anything.
[[vk::binding(9, 0)]]
public Texture2D<float4> preview;

/// The samples body, one element (docs/abi.md, *Samples*): what a sidecar wrote
/// after its wire header, copied here without the bar reading a word of it.
/// Every chart on the strip is a row of this, and nothing about a chart
/// reaches this file any other way — an ITEM_CHART carries **only** its row
/// index, and the row's kind, ceiling and samples are read here. The binding
/// number is a literal because a `[[vk::binding]]` argument cannot be an
/// expression; it is also the *only* declaration of it — the
/// embassy builds its bind group by reading this name out of the reflection
/// (docs/abi.md, *Bindings and the oracle*).
[[vk::binding(10, 0)]]
public StructuredBuffer<Samples> samples;

/// The literal pool: every `getStringHash` literal this
/// caustic names, as `(hash, offset, len)` into the strings buffer, rebuilt by
/// the embassy whenever the content is. `facts[0].header.literal_count` is how many
/// live; `literal(hash)` below is the lookup.
///
/// It is what lets `bar_compose` spell a weekday: the words are this file's
/// (`WEEKDAYS`, `MONTHS` in the tunables), the bytes are in a buffer, and the
/// embassy — which has slangc's own table of them — never learns that any of
/// them is a weekday. The binding number is this file's alone, as `samples`
/// above says.
[[vk::binding(11, 0)]]
public StructuredBuffer<Literal> literals;

/// **What the embassy knows** (docs/abi.md, *The facts*): the windows niri
/// reports, the dock's app ids in dock order, which image slot holds which
/// pixmap, which binary sits in which sidecar slot, and the literal pool.
/// Every word of it is something the embassy *originated*; what a sidecar
/// sent is in the two arenas below, byte for byte.
///
/// The binding number is this file's alone, as `samples` above says.
[[vk::binding(12, 0)]]
public StructuredBuffer<Facts> facts;

/// The wake's sources (*The wake*): a fixed ring, so it is part of the
/// private buffer's layout and not a tunable.
public static const uint WAKE_SOURCES = 16u;

/// **What this file builds for itself** (docs/abi.md, *The private buffer*): the element
/// table, the item and block tables, the scratch, the springs, the popup's
/// want, and every value a witness is compared against.
///
/// The ABI does not describe a word of it. The embassy allocates the stride
/// slangc's reflection reports for `Private`, and then never writes it and
/// never reads it — it starts zero (wgpu's creation guarantee) and
/// `ensure_private()` writes the non-zero defaults on the first pass of the
/// first frame. A reload keeps it, so the popup, the menu level and the
/// springs survive one; a reload that changed its layout changed the stride,
/// and the bindings diff refuses that with "restart the bar".
///
/// It is a buffer of its own, apart from `state`, so that only the rings are
/// copied back to the CPU each frame — never an element table or any other
/// word the embassy has no business reading.
public struct Private
{
    /// PRIVATE_MAGIC once `ensure_private` has run. wgpu zero-initialises the
    /// buffer, so 0 is "never initialised" and cannot be mistaken for a state.
    public uint magic;
    /// Elements laid out this frame (the visibility loop's bound).
    public uint element_count;
    /// The element under the pointer as of the last sample pass, or NO_INDEX.
    public uint hover_element;
    /// The element a button went down on and has not gone up, or NO_INDEX.
    public uint pressed_element;
    /// `elements[hover_element].item`, resolved once per frame.
    public uint hover_item;

    // -- the pointer, which is a message (docs/abi.md, *The action ring*) -----------------------------
    /// Where the pointer last was, in px of `pointer_target`, or (-1, -1).
    /// It arrives as `CMD_POINTER_MOTION`/`DOWN`/`UP`/`LEAVE` and is kept
    /// here.
    public float2 pointer;
    /// `TARGET_*` the pointer is on, or NO_INDEX for neither.
    public uint pointer_target;

    // -- what `bar_build` published ----------------------------------------
    public uint built_items;
    public uint built_blocks;
    public uint built_glyphs;
    /// Items it had no room for: the `items: table capped` warning's number.
    public uint built_dropped;
    /// The menu level the published table was built for.
    public Id built_for;

    // -- the popup, as a state machine this file owns (docs/abi.md, *The action ring*) ----------------
    /// POPUP_IDLE / POPUP_ASKED / POPUP_UP / POPUP_ENDING.
    public uint popup_state;
    /// POPUP_MENU, POPUP_PREVIEW, POPUP_TOAST, or 0.
    public uint popup_kind;
    /// The sidecar slot the open named, or NO_INDEX (a dock entry's menu).
    public uint popup_slot;
    /// The row that open named.
    public uint popup_row;
    /// The item the popup hangs off.
    public Id popup_id;
    /// The anchor rect in bar px, as pushed.
    public float4 popup_anchor;
    /// What the layout wants the popup to be, px — compared here rather than
    /// on the embassy's side, which is what makes `ACT_POPUP_MOVE` a message.
    public float2 popup_want;
    /// The size last *sent*, as an open or a move.
    public float2 popup_sent;
    /// How far the open menu level is scrolled, px: 0
    /// whenever the level changes or the popup closes, moved by `CMD_AXIS`,
    /// and never past `menu_scroll_max`.
    public float menu_scroll;
    /// How far it *can* scroll: the level's rows less the popup's inner
    /// height, as the last layout measured it. 0 for a level that fits.
    public float menu_scroll_max;
    /// The band of popup y the scrolled rows are drawn and hit in: below the
    /// back row, above the bottom pad. `(0, 0)` is no band — nothing clips.
    public float2 menu_clip;
    /// A toast the compositor dismissed or refused is not
    /// asked for again for the same news: the slot, the chip's row id, and
    /// the id of the newest attention row it was showing. The hold lifts when
    /// that chip stops asking or something newer arrives. `toast_hold_slot`
    /// NO_INDEX is no hold — without it a compositor that refuses the popup
    /// would be asked again every frame.
    public uint toast_hold_slot;
    public uint toast_hold_chip;
    public uint toast_hold_newest;
    /// The toast's own rows, **taken when it opens and
    /// kept until it ends**: no frame that arrives while it is up changes what
    /// it shows or where. `toast_pending` is 1 from the frame a chip first
    /// asks — its level set, its rows not built yet — until the frame they are
    /// built, shaped and wrapped, when the toast opens at exactly their size.
    /// `toast_slot`/`toast_chip` are the chip that asked; `toast_count` rows,
    /// in the frame's order at the open: `toast_ids` their row ids,
    /// `toast_lines` the body lines each shows, `toast_h` each one's height
    /// in px. `toast_size` is the size the open asked for, which is the size
    /// the toast keeps. `toast_said_held` is the newest row id a `toast: held`
    /// line was said for, so it is said once per arrival.
    public uint toast_pending;
    public uint toast_slot;
    public uint toast_chip;
    public uint toast_count;
    public uint toast_ids[TOAST_ROWS_CAP];
    public uint toast_lines[TOAST_ROWS_CAP];
    public float toast_h[TOAST_ROWS_CAP];
    public float2 toast_size;
    public uint toast_said_held;
    /// The chart numbers. `numbers_dwell` is seconds the
    /// pointer has rested on the strip with nothing up; `numbers_hold` is 1
    /// once a numbers popup ended with the pointer still on the strip — a
    /// second click, a dismissal — so the hover does not open it straight
    /// back, and it lifts when the pointer leaves the strip.
    public float numbers_dwell;
    public uint numbers_hold;
    /// The width the numbers popup last measured, px, and whether this open
    /// has measured yet: an open asks for the last width, and while it is up
    /// the width only grows, so a number gaining a digit every other second
    /// does not move the surface every other second.
    public float numbers_w;
    public uint numbers_grow;
    /// `samples[0].t`'s bits as of the last numbers build: a new tick under an
    /// open numbers popup is an `ACT_REBUILD`, which is what keeps them live.
    public uint numbers_t;

    // -- the anchor, likewise ----------------------------------------------
    /// The `(EDGE, BAR_HEIGHT)` pair last pushed as an `ACT_ANCHOR`. Zero
    /// height is "never pushed", which is what makes the first frame push one.
    public uint anchor_edge;
    public uint anchor_height;
    /// `SHAPE_WATCHDOG_MS` as last pushed. Zero is "never
    /// pushed", which is what makes the first frame push one.
    public uint watchdog_ms;

    // -- the menu level and the press, by id (see *Why the ids are ids*) ----
    public Id menu_level;
    public Id pressed_id;

    // -- clocks and the drag -----------------------------------------------
    /// Seconds the pointer has rested on the dwell candidate — or, while a
    /// preview is live, seconds it has rested on neither that preview nor its
    /// entry (the grace clock; `step_dwell` returns before the two uses meet).
    public float dwell_secs;
    /// The dwell candidate's item index, or -1.
    public float dwell_candidate;
    /// The dock entry a live preview belongs to, or -1.
    public float preview_owner;
    /// The element index of the slider being dragged, **plus one** (0 must not
    /// name element 0), else 0.
    public float drag_element;
    /// The value the pointer last put that slider at, 0..1.
    public float drag_value;

    // -- what each witness was last pushed for (docs/abi.md, *The witness ring*) ----------------------
    //
    // "Once per change" is this file's duty: the embassy prints every
    // record it reads and dedups nothing. NO_INDEX is "nothing said yet", so
    // the first frame witnesses everything.
    public uint said_dock_entries;
    public uint said_dock_running;
    public uint said_applets;
    public uint said_charts;
    public uint said_built_items;
    public uint said_built_blocks;
    public uint said_built_glyphs;
    public uint said_dropped;
    public uint said_hover;
    public uint said_redraw;
    /// Runs the shaper flagged, as last witnessed.
    public uint said_notdef;
    /// The first block whose run ran out of glyph slots, as last witnessed.
    /// NO_INDEX when none did, which is the bar every day.
    public uint said_truncated;
    /// A hash of the *set* of notdef text blocks, so `ACT_COVER` goes out when
    /// the set changes and not once per frame.
    public uint notdef_hash;

    // -- what a `CMD_ROWS` is compared against (docs/abi.md, *The arenas*) -----------------------
    //
    // The embassy does not understand a row: a `Rows` frame is
    // one `write_buffer` of the slot's region and one `CMD_ROWS`, and **this
    // file** decides whether that is a rebuild. `rows_shape` is the hash of
    // everything the item table depends on — every word of every row but
    // `value`, and the label and icon bytes their ranges name — so a slider
    // tick compares equal and costs no model pass at all.
    public uint rows_shape[MAX_SLOTS];
    /// The `Row.id` of each slot's first `ROW_ITEM`, kept when the sidecar
    /// goes so a departure witness still spells `applet-<name>` and not
    /// `applet-<name>-<row>`.
    public uint slot_first_item[MAX_SLOTS];
    /// The same comparison for the chart strip: the kinds of the samples
    /// body's rows, hashed. A tick that carries new numbers for the same
    /// charts is an upload and a redraw; a different chart set is a rebuild.
    public uint chart_shape;

    // -- the answers arena, indexed once per build (docs/abi.md, *The arenas*) -------------------
    //
    // Written at the top of `bar_build` and read by nothing else: the hash of
    // each answer's echoed key and where its body starts, so joining a name to
    // an answer is an integer compare rather than a walk of the arena per
    // name.
    public uint answer_count;
    public uint answer_hash[MAX_ANSWERS];
    public uint answer_at[MAX_ANSWERS];

    // -- the bodies' lines ---------------------------------------------------
    //
    // A body is wrapped on the glyphs' measured advances, so after the shape
    // pass; and not every frame, because it is a walk of every glyph of every
    // body. `wrap_key` hashes what the wrap read — the scale, and each body's
    // item, blocks and their shaped counts and widths — and the walk runs
    // again only when that moves.
    /// The ellipsis literal's block, shaped once per build when any row has a
    /// body, or NO_INDEX.
    public uint ellipsis_block;
    /// Bumped by every `bar_build`, so a rebuild that came out the same shape
    /// is still wrapped again: the build wrote every row's `body_lines` to 0.
    public uint build_serial;
    public uint wrap_key;
    /// Lines of `body_lines` in use.
    public uint body_line_count;
    public BodyLine body_lines[MAX_BODY_POOL];

    // -- the wake under the pointer (*The wake*) ----------------------------
    /// The sources, a ring: x and y in bar px, age in seconds, and height as
    /// a multiple of WAKE_HEIGHT. Height 0 is an empty slot.
    public float4 wake[WAKE_SOURCES];
    /// The pointer as the wake last saw it, x and y in bar px (x < 0 when it
    /// was not on the bar); the px it has travelled since the last source,
    /// and the seconds since that source.
    public float4 wake_track;
    /// The slot the next source takes.
    public uint wake_next;
    /// 1 while any source lives, as last witnessed (`wake: stirred`, `wake:
    /// still`). `bar_draw` reads it too: 0 is a slab with no light to add.
    public uint said_wake;

    // -- the tables ---------------------------------------------------------
    public Element elements[MAX_ELEMENTS];
    public Item items[MAX_ITEMS];
    public TextBlock blocks[MAX_BLOCKS];
};

[[vk::binding(13, 0)]]
public RWStructuredBuffer<Private> private;

/// **The rows arena** (docs/abi.md, *The arenas*): MAX_SLOTS regions of
/// ROWS_ARENA_BYTES, each holding one sidecar's last `Rows` body exactly as it
/// crossed the wire — `RowsHeader`, the rows, the blob, at the writer's own
/// offsets. The embassy parses none of it; this file reads it through
/// `bar_wire.slang`'s own `RowsHeader` and `Row`.
///
/// Declared as words rather than a `ByteAddressBuffer` for one reason, and it
/// is the reason a row's label is drawn at all: a label's bytes live *here*,
/// and `shapeRun` takes its text through `ShapeTables.utf8`, which is an
/// `RWStructuredBuffer<uint>` handle. A `ByteAddressBuffer` cannot be assigned
/// to one, so `bar_shape` could not shape a row. Nothing in this file writes a
/// word of it; the embassy does, one `write_buffer` per `Rows` frame.
[[vk::binding(14, 0)]]
public RWStructuredBuffer<uint> rows;

/// **The answers arena** (docs/abi.md, *The arenas*): every `Glyph` (docs/abi.md, *Glyph*) and
/// `App` (docs/abi.md, *App*) body the embassy has received, concatenated — the glyph
/// bodies first and the app bodies from `facts[0].header.glyph_bytes`, because
/// a body carries no kind word of its own and the two heads are different
/// shapes. This file joins them to the names it wants by their echoed keys.
///
/// Words rather than bytes for the same reason as `rows`: a dock entry's label
/// is the `App` answer's `Name=`, and the shaper reads it from here.
[[vk::binding(15, 0)]]
public RWStructuredBuffer<uint> answers;

/// `Private.magic` once initialised. Any non-zero word would do; a readable
/// one makes a buffer dump legible.
public static const uint PRIVATE_MAGIC = 0x62617231u; // "bar1"

/// The popup's life, as this file holds it. The ABI has only `POPUP_MENU`,
/// `POPUP_PREVIEW` and `POPUP_TOAST` — what the popup is *for* — because the states are not
/// a word the embassy reads: an open, a move and a close are messages,
/// and `CMD_POPUP_MAPPED` / `CMD_POPUP_DONE` are the answers (docs/abi.md, *The action ring*).
public static const uint POPUP_IDLE = 0u;
/// `ACT_POPUP_OPEN` pushed; waiting for a `CMD_POPUP_MAPPED` or a
/// `CMD_POPUP_DONE`.
public static const uint POPUP_ASKED = 1u;
public static const uint POPUP_UP = 2u;
/// `ACT_POPUP_CLOSE` pushed; waiting for the `CMD_POPUP_DONE`.
public static const uint POPUP_ENDING = 3u;

// ---------------------------------------------------------------------------
// Bind group 2: `shape`'s font tables and the strings buffer. Group 1 is
// `text`'s four curve buffers, declared by the module itself — this file never
// names them, so the text module can change its buffers without an edit here.
//
// `shape.slang` declares no bindings on purpose (its header says why), so the
// host picks the numbers and gathers the handles. These are that gather.
// ---------------------------------------------------------------------------

[[vk::binding(0, 2)]]
public ConstantBuffer<ShapeParams> shapeParams;

/// The **arena**: the face directory `caustics-glyphs` wrote, and behind it every
/// face's `cmap`, `hmtx`, `GDEF`, `GSUB` and `GPOS` exactly as they sit in the
/// font files (the text engine's `ot.slang` is the reader).
///
/// One binding of raw table bytes rather than one per pre-digested table (a
/// sorted `cmap`, an advance array, the PairPos matrices, the ligature list,
/// the chain rules), each of which would be a shape the CPU had to understand
/// in order to build. The arena is bytes the CPU copied, and the bar's whole
/// knowledge of a font is its length.
[[vk::binding(1, 2)]]
public StructuredBuffer<uint> shapeOt;

/// Every string on the bar, UTF-8, four bytes per word. A BLOCK_TEXT names a
/// byte range of it and nothing else: that is the whole of what the CPU says
/// about a label.
///
/// **Read-write**, because one binding cannot be two things: `bar_compose`
/// writes the clock's bytes into the range the CPU reserved for them while
/// `bar_shape` and `bar_draw` only read. A compute pipeline does not mind.
[[vk::binding(2, 2)]]
public RWStructuredBuffer<uint> shapeUtf8;

/// The handles, gathered — the two lines `shape_demo.slang` advertises. Every
/// field of `ShapeTables` is set here, and that is not a formality: an unset
/// `StructuredBuffer` field is an undefined descriptor the kernel would read
/// from, so adding a buffer to the struct without a line here is a silent
/// wrong answer rather than a compile error.
public ShapeTables shape_tables(uint source)
{
    ShapeTables t;
    t.ot = shapeOt;
    // The three buffers a block's bytes may be in (docs/abi.md, *The arenas*), and
    // which one this block uses. Three *fields* rather than three calls: see
    // the note on `ShapeTables.utf8` for what the branch would cost.
    t.utf8 = shapeUtf8;
    t.utf8_alt0 = rows;
    t.utf8_alt1 = answers;
    t.source = source;
    return t;
}

/// The baseline's distance from a line box's top, px, at em size `px`.
///
/// `hhea`'s ascender over the em, which is where a shaper puts the pen. The
/// CPU handed the two numbers over with the font (`ShapeFont`) so that this
/// file could place a run without being told where its baseline is.
public float text_baseline(float px)
{
    return px * float(shapeParams.font.ascender) / float(max(shapeParams.font.upem, 1u));
}

/// A line box's height, px: ascender to descender, the same way.
public float text_line_height(float px)
{
    let span = float(shapeParams.font.ascender - shapeParams.font.descender);
    return px * span / float(max(shapeParams.font.upem, 1u));
}

// ===========================================================================
// The private buffer: everything this file keeps between dispatches
// (docs/abi.md, *The private buffer*).
//
// The element table, the item and block tables, the menu level, the pressed
// id, the dwell clock, the drag, the popup's whole state machine and every
// value a witness is compared against. The embassy allocates it by the stride
// slangc reports and **never writes it and never reads it**: it starts zero
// (wgpu's creation guarantee) and `ensure_private` below writes the non-zero
// defaults once.
//
// It is not in `state` because everything in `state` is copied back to the
// CPU every frame: an element table there would be tens of KiB of readback
// for nothing the embassy acts on. What crosses is 21 KiB of *rings*, and only
// what was pushed into them is read.
//
// **Why the ids are ids and not indices.** An index into the item
// table is only true for the frame it was read in: a sidecar's frame arriving
// under an open menu can add a row anywhere, and every index after it moves.
// Holding an index would mean closing the menu on any reshape to stop a click
// landing on the wrong row — a menu that shuts whenever its own rows change,
// which for a Wi-Fi menu mid-scan is every result. An id
// is a *fact*: `menu_level()` resolves it to this frame's index by a scan, and
// not finding it means the item is gone, which is the one case that closes the
// menu.
//
// **And why they are integers.** A pair of floats is exact only to 2^24 —
// enough for a counter and not for a fact-derived id, which is a 32-bit app
// hash or a tagged window id. Anything wider would go in and come out as a
// different number, silently, on the path that decides where a click lands.
//
// A dispatch has no memory of its own, and a `static` here would be re-zeroed
// every frame without saying so, which is the trap this note exists to name.
// ===========================================================================

/// Write the non-zero defaults, once, on the first pass of the first frame.
///
/// Called at the top of `bar_build`, `bar_apply` and `bar_sample` — the three
/// single-thread passes — because any of them may be the first to run: the
/// shape submission goes before the frame's, and a frame that draws nothing
/// still applies. Everything not named here is zero and means zero.
public void ensure_private()
{
    if (private[0].magic == PRIVATE_MAGIC)
    {
        return;
    }
    private[0].magic = PRIVATE_MAGIC;
    private[0].hover_element = NO_INDEX;
    private[0].pressed_element = NO_INDEX;
    private[0].hover_item = NO_INDEX;
    private[0].popup_slot = NO_INDEX;
    private[0].toast_hold_slot = NO_INDEX;
    private[0].toast_slot = NO_INDEX;
    private[0].toast_said_held = NO_INDEX;
    private[0].dwell_candidate = -1.0;
    private[0].preview_owner = -1.0;
    // "Nothing witnessed yet", so the first frame says everything once.
    private[0].said_dock_entries = NO_INDEX;
    private[0].said_dock_running = NO_INDEX;
    private[0].said_applets = NO_INDEX;
    private[0].said_charts = NO_INDEX;
    private[0].said_built_items = NO_INDEX;
    private[0].said_built_blocks = NO_INDEX;
    private[0].said_built_glyphs = NO_INDEX;
    private[0].said_dropped = NO_INDEX;
    private[0].said_hover = NO_INDEX;
    private[0].said_redraw = NO_INDEX;
    private[0].said_notdef = NO_INDEX;
    private[0].said_truncated = NO_INDEX;
    private[0].ellipsis_block = NO_INDEX;
    // No pointer until one arrives as a command (docs/abi.md, *The action ring*).
    private[0].pointer = float2(-1.0, -1.0);
    private[0].pointer_target = NO_INDEX;
    private[0].wake_track = float4(-1.0, -1.0, 0.0, 0.0);
    // "This slot has never published a chip", which is what makes a departure
    // witness name the chip rather than one of its rows.
    for (uint s = 0u; s < MAX_SLOTS; ++s)
    {
        private[0].slot_first_item[s] = NO_INDEX;
    }
    // An unwritten item slot must read as "nothing here" rather than as block
    // 0 of item 0, and an unwritten element must not be `EFLAG_PRESENT`. Zero
    // gives the second for free; the first needs the NO_INDEX fields written.
    for (uint i = 0u; i < MAX_ITEMS; ++i)
    {
        private[0].items[i].kind = ITEM_NONE;
        private[0].items[i].parent = NO_INDEX;
        private[0].items[i].block = NO_INDEX;
        private[0].items[i].icon_block = NO_INDEX;
        private[0].items[i].aux_block = NO_INDEX;
        private[0].items[i].fact_row = NO_INDEX;
        private[0].items[i].body_blocks = 0u;
        private[0].items[i].body_lines = 0u;
    }
    for (uint e = 0u; e < MAX_ELEMENTS; ++e)
    {
        private[0].elements[e].target = NO_INDEX;
        private[0].elements[e].block = NO_INDEX;
        private[0].elements[e].item = NO_INDEX;
    }
}

// ===========================================================================
// The two rings (docs/abi.md, *The two contracts* rule 1, *The action ring*, *The witness ring*).
//
// A ring belongs to the frame stamped on it. A pass comparing the ring's
// `*_frame` with `inputs.frame` resets the count when they differ and appends
// when they agree — so `bar_build`, in the shape submission, and `bar_apply`
// and `bar_sample`, in the frame's, all append to the same rings with no
// clear-at-top and no dispatch flag. The embassy advances `inputs.frame` only
// after a frame's readback has completed, so a frame that failed at the
// swapchain keeps its stamp and the next attempt appends to what it pushed:
// **no action and no witness is ever lost**, except past the capacity, where
// the newest is dropped and counted.
//
// Only these three single-thread passes push. A per-texel pass pushing into a
// ring would be a race with itself.
// ===========================================================================

/// Operands under construction. Slang has no varargs, so a record is built by
/// appending; past ten words the appends are dropped, which is a `?` in the
/// printed line rather than a corrupt one.
public struct Args
{
    public uint w[10];
    public uint n;
};

public Args args_new()
{
    Args a;
    for (uint i = 0u; i < 10u; ++i)
    {
        a.w[i] = 0u;
    }
    a.n = 0u;
    return a;
}

public void arg(inout Args a, uint value)
{
    if (a.n < 10u)
    {
        a.w[a.n] = value;
        a.n += 1u;
    }
}

/// A float travels as its bit pattern; the printer reads it back with
/// `f32::from_bits` and the embassy's forwarders with the typed accessors.
public void argf(inout Args a, float value)
{
    arg(a, asuint(value));
}

public void arg2(inout Args a, float2 v) { argf(a, v.x); argf(a, v.y); }
public void arg4(inout Args a, float4 v) { arg2(a, v.xy); arg2(a, v.zw); }

/// One decision, with its operands. *The action ring*'s table is the contract; nothing here
/// is an index into anything the embassy keeps.
public void push_action(uint kind, Args a)
{
    if (state[0].surface.action_frame != inputs.frame)
    {
        state[0].surface.action_frame = inputs.frame;
        state[0].surface.action_count = 0u;
        state[0].surface.action_lost = 0u;
    }
    uint n = state[0].surface.action_count;
    if (n >= MAX_ACTIONS)
    {
        // A lost action beats a repeated one, and the embassy warns once per
        // frame it sees this.
        state[0].surface.action_lost += 1u;
        return;
    }
    state[0].actions[n].head = kind;
    state[0].actions[n].aux = 0u;
    for (uint i = 0u; i < 10u; ++i)
    {
        state[0].actions[n].arg[i] = a.w[i];
    }
    state[0].surface.action_count = n + 1u;
}

/// One line of the journal, as a format literal and its words (docs/abi.md, *The witness ring*).
///
/// `head` is `getStringHash("…")` of the literal; slangc records the text in
/// its reflection and the embassy resolves it there. There is no per-witness
/// Rust: a new line is one call here.
///
/// **Once per change is this file's duty.** The embassy prints every
/// record it reads, in order, and dedups nothing — so every caller compares
/// against what it persisted in `private` before pushing.
public void witness(uint level, uint head, Args a)
{
    if (state[0].surface.witness_frame != inputs.frame)
    {
        state[0].surface.witness_frame = inputs.frame;
        state[0].surface.witness_count = 0u;
        state[0].surface.witness_lost = 0u;
    }
    uint n = state[0].surface.witness_count;
    if (n >= MAX_WITNESSES)
    {
        state[0].surface.witness_lost += 1u;
        return;
    }
    state[0].witnesses[n].head = head;
    state[0].witnesses[n].aux = level;
    for (uint i = 0u; i < 10u; ++i)
    {
        state[0].witnesses[n].arg[i] = a.w[i];
    }
    state[0].surface.witness_count = n + 1u;
}

/// A witness with no words at all, which most of the edges are.
public void witness0(uint level, uint head)
{
    witness(level, head, args_new());
}

/// A witness of one number.
public void witness1(uint level, uint head, uint v0)
{
    Args a = args_new();
    arg(a, v0);
    witness(level, head, a);
}

/// A witness of two.
public void witness2(uint level, uint head, uint v0, uint v1)
{
    Args a = args_new();
    arg(a, v0);
    arg(a, v1);
    witness(level, head, a);
}

/// A witness of three.
public void witness3(uint level, uint head, uint v0, uint v1, uint v2)
{
    Args a = args_new();
    arg(a, v0);
    arg(a, v1);
    arg(a, v2);
    witness(level, head, a);
}

// -- the action kinds, spelled once -----------------------------------------

/// An action of no operands: the toggles, `ACT_PREVIEW_END`, `ACT_REBUILD`,
/// `ACT_POPUP_CLOSE`.
public void act(uint kind)
{
    push_action(kind, args_new());
}

/// An action of two words: a window's id, a string range, an anchor's pair, a
/// popup's size.
public void act2(uint kind, uint v0, uint v1)
{
    Args a = args_new();
    arg(a, v0);
    arg(a, v1);
    push_action(kind, a);
}

/// An action of three.
public void act3(uint kind, uint v0, uint v1, uint v2)
{
    Args a = args_new();
    arg(a, v0);
    arg(a, v1);
    arg(a, v2);
    push_action(kind, a);
}

/// An action of four: `ACT_WANT_ICON` — a range, the wire's `REQ_*`, and the
/// buffer the name's bytes are in (docs/abi.md, *The action ring*).
public void act4(uint kind, uint v0, uint v1, uint v2, uint v3)
{
    Args a = args_new();
    arg(a, v0);
    arg(a, v1);
    arg(a, v2);
    arg(a, v3);
    push_action(kind, a);
}

// ===========================================================================
// Names: what this file calls a thing in a witness line (docs/abi.md, *The witness ring*).
//
// The `layout:` family is load-bearing — `bar/bar-test.sh` parses it to know
// where to click — so the spellings below are stable. The pass that laid the
// rect out composes the line, so the embassy keeps no copy of the rects to
// diff and no map from an id to a name.
//
// One literal per element kind per family, because a format literal is a
// compile-time string and a name is part of it. `NameRef` is the family and
// the leading words; the tail of every literal starts at `count`, so one
// builder serves all four families (layout, gone, hover, stall).
// ===========================================================================

public static const uint NAME_BAR = 0u;
public static const uint NAME_POPUP = 1u;
public static const uint NAME_BACK = 2u;
public static const uint NAME_PREVIEW = 3u;
public static const uint NAME_CLOCK = 4u;
public static const uint NAME_OVERVIEW = 5u;
public static const uint NAME_CAFFEINE = 6u;
/// `window-{id}`; two words, the id low first.
public static const uint NAME_WINDOW = 7u;
/// The app id itself; two words, its range in the strings buffer.
public static const uint NAME_ENTRY = 8u;
/// `dock-{verb}`; one word, the verb's literal.
public static const uint NAME_DOCK_ROW = 9u;
/// `chart-{kind}`; one word, the kind's literal.
public static const uint NAME_CHART = 10u;
/// `chart-{kind}-{n}`; the literal and the row's index in the samples frame.
public static const uint NAME_CHART_N = 11u;
/// `applet-{slot}`; two words, the sidecar's name range.
public static const uint NAME_APPLET = 12u;
/// `applet-{slot}-{row}`; the range and the row id.
public static const uint NAME_APPLET_N = 13u;
/// Anything the caustic laid out that names no item.
public static const uint NAME_ITEM = 14u;
/// `calendar-{month}-{year}`: the calendar's title; the
/// month's literal and the year.
public static const uint NAME_CAL_TITLE = 15u;
/// `calendar-weekday-{name}`; one word, the weekday's literal.
public static const uint NAME_CAL_WEEKDAY = 16u;
/// `calendar-day-{n}`; one word, the day of the month.
public static const uint NAME_CAL_DAY = 17u;
/// `numbers-{kind} {value} {unit}`: a line of the chart numbers — the
/// chart's word as the strip's `chart-{kind}` spells it, and its
/// first series' number as composed (`numbers-cpu 23 %`), so the line a
/// witness prints says what the popup shows. Three words always: the two
/// variants below carry the same three and print fewer.
public static const uint NAME_NUMBERS = 18u;
/// The same, for a number with no unit (a third party's `CHART_OTHER`).
public static const uint NAME_NUMBERS_BARE = 19u;
/// The same, for a chart with no sample: `numbers-{kind} none`.
public static const uint NAME_NUMBERS_NONE = 20u;

/// A name: which literal family, and the words in front of the format's tail.
public struct NameRef
{
    public uint slot;
    public uint a0;
    public uint a1;
    public uint a2;
    /// How many of the three are live, which is where the tail starts.
    public uint count;
};

public NameRef name_of(uint slot, uint a0, uint a1, uint a2, uint count)
{
    NameRef n;
    n.slot = slot;
    n.a0 = a0;
    n.a1 = a1;
    n.a2 = a2;
    n.count = count;
    return n;
}

public NameRef name_plain(uint slot)  { return name_of(slot, 0u, 0u, 0u, 0u); }
