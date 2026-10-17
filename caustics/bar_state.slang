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

/// What the toasts keep between dispatches (`notify.slang`): the hold on an
/// ended toast, and the open toast's own rows. `bar_popups` reads the rows
/// too, to tell their sidecar which were shown when the popup ends and to
/// cap a toast row's body lines (`say_toast_shown`, `toast_index_of`).
public struct NotifyScratch
{
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
};

/// What the charts keep between dispatches (`sysmon.slang`): the strip's
/// shape, what was last witnessed of it, and the chart numbers' popup.
public struct SysmonScratch
{
    /// The comparison a `CMD_ROWS` gets (`rows_shape`, below), for the chart
    /// strip: the kinds of the samples
    /// body's rows, hashed. A tick that carries new numbers for the same
    /// charts is an upload and a redraw; a different chart set is a rebuild.
    public uint chart_shape;
    /// Charts in the last build, as last witnessed (`charts: N laid out`).
    public uint said_charts;
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
    /// The first chart's item index in the build running now, which the
    /// numbers' lines hang off; written and read inside one `bar_build`.
    public uint build_first;
};

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
    /// The elements `place` made present this frame, in the order it placed
    /// them: the order `witness_placed` says their `layout:` lines in, after
    /// the layout, where each can be named.
    public uint placed_count;
    public uint placed[MAX_ELEMENTS];
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

    // -- the subjects' own, one struct each ---------------------------------
    /// The charts' (`sysmon.slang`).
    public SysmonScratch sysmon;
    /// The toasts' (`notify.slang`).
    public NotifyScratch notify;

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
    private[0].notify.toast_hold_slot = NO_INDEX;
    private[0].notify.toast_slot = NO_INDEX;
    private[0].notify.toast_said_held = NO_INDEX;
    private[0].dwell_candidate = -1.0;
    private[0].preview_owner = -1.0;
    // "Nothing witnessed yet", so the first frame says everything once.
    private[0].said_dock_entries = NO_INDEX;
    private[0].said_dock_running = NO_INDEX;
    private[0].said_applets = NO_INDEX;
    private[0].sysmon.said_charts = NO_INDEX;
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

/// `layout: <name> target={n:s} rect={n+1:f},…` — the tail is the same four
/// floats and one target hash for every family.
public uint layout_literal(uint slot)
{
    switch (slot)
    {
    case NAME_BAR:
        return getStringHash("layout: ELEM_BAR target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    case NAME_POPUP:
        return getStringHash("layout: ELEM_POPUP target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    case NAME_BACK:
        return getStringHash("layout: ELEM_BACK target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    case NAME_PREVIEW:
        return getStringHash("layout: ELEM_PREVIEW target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    case NAME_CLOCK:
        return getStringHash("layout: clock target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    case NAME_OVERVIEW:
        return getStringHash("layout: overview target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    case NAME_CAFFEINE:
        return getStringHash("layout: caffeine target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    case NAME_WINDOW:
        return getStringHash("layout: window-{0:u64} target={2:s} rect={3:f},{4:f},{5:f},{6:f}");
    case NAME_ENTRY:
        return getStringHash("layout: {0:str} target={2:s} rect={3:f},{4:f},{5:f},{6:f}");
    case NAME_DOCK_ROW:
        return getStringHash("layout: dock-{0:s} target={1:s} rect={2:f},{3:f},{4:f},{5:f}");
    case NAME_CHART:
        return getStringHash("layout: chart-{0:s} target={1:s} rect={2:f},{3:f},{4:f},{5:f}");
    case NAME_CHART_N:
        return getStringHash("layout: chart-{0:s}-{1} target={2:s} rect={3:f},{4:f},{5:f},{6:f}");
    case NAME_APPLET:
        return getStringHash("layout: applet-{0:str} target={2:s} rect={3:f},{4:f},{5:f},{6:f}");
    case NAME_APPLET_N:
        return getStringHash("layout: applet-{0:str}-{2} target={3:s} rect={4:f},{5:f},{6:f},{7:f}");
    case NAME_CAL_TITLE:
        return getStringHash("layout: calendar-{0:s}-{1} target={2:s} rect={3:f},{4:f},{5:f},{6:f}");
    case NAME_CAL_WEEKDAY:
        return getStringHash("layout: calendar-weekday-{0:s} target={1:s} rect={2:f},{3:f},{4:f},{5:f}");
    case NAME_CAL_DAY:
        return getStringHash("layout: calendar-day-{0} target={1:s} rect={2:f},{3:f},{4:f},{5:f}");
    case NAME_NUMBERS:
        return getStringHash("layout: numbers-{0:s} {1:f} {2:s} target={3:s} rect={4:f},{5:f},{6:f},{7:f}");
    case NAME_NUMBERS_BARE:
        return getStringHash("layout: numbers-{0:s} {1:f} target={3:s} rect={4:f},{5:f},{6:f},{7:f}");
    case NAME_NUMBERS_NONE:
        return getStringHash("layout: numbers-{0:s} none target={3:s} rect={4:f},{5:f},{6:f},{7:f}");
    default:
        return getStringHash("layout: ELEM_ITEM target={0:s} rect={1:f},{2:f},{3:f},{4:f}");
    }
}

/// `layout: <name> target=none rect=gone` — the departure line, pushed here so
/// the embassy never has to keep a copy of the rects to diff.
public uint gone_literal(uint slot)
{
    switch (slot)
    {
    case NAME_BAR:      return getStringHash("layout: ELEM_BAR target=none rect=gone");
    case NAME_POPUP:    return getStringHash("layout: ELEM_POPUP target=none rect=gone");
    case NAME_BACK:     return getStringHash("layout: ELEM_BACK target=none rect=gone");
    case NAME_PREVIEW:  return getStringHash("layout: ELEM_PREVIEW target=none rect=gone");
    case NAME_CLOCK:    return getStringHash("layout: clock target=none rect=gone");
    case NAME_OVERVIEW: return getStringHash("layout: overview target=none rect=gone");
    case NAME_CAFFEINE: return getStringHash("layout: caffeine target=none rect=gone");
    case NAME_WINDOW:   return getStringHash("layout: window-{0:u64} target=none rect=gone");
    case NAME_ENTRY:    return getStringHash("layout: {0:str} target=none rect=gone");
    case NAME_DOCK_ROW: return getStringHash("layout: dock-{0:s} target=none rect=gone");
    case NAME_CHART:    return getStringHash("layout: chart-{0:s} target=none rect=gone");
    case NAME_CHART_N:  return getStringHash("layout: chart-{0:s}-{1} target=none rect=gone");
    case NAME_APPLET:   return getStringHash("layout: applet-{0:str} target=none rect=gone");
    case NAME_APPLET_N: return getStringHash("layout: applet-{0:str}-{2} target=none rect=gone");
    case NAME_CAL_TITLE:   return getStringHash("layout: calendar-{0:s}-{1} target=none rect=gone");
    case NAME_CAL_WEEKDAY: return getStringHash("layout: calendar-weekday-{0:s} target=none rect=gone");
    case NAME_CAL_DAY:     return getStringHash("layout: calendar-day-{0} target=none rect=gone");
    case NAME_NUMBERS:
    case NAME_NUMBERS_BARE:
    case NAME_NUMBERS_NONE:
        return getStringHash("layout: numbers-{0:s} target=none rect=gone");
    default:            return getStringHash("layout: ELEM_ITEM target=none rect=gone");
    }
}

/// `hover: <name> at=x,y`, rounded to whole px, so it reads `at=724,12`.
public uint hover_literal(uint slot)
{
    switch (slot)
    {
    case NAME_BAR:      return getStringHash("hover: ELEM_BAR at={0:f},{1:f}");
    case NAME_POPUP:    return getStringHash("hover: ELEM_POPUP at={0:f},{1:f}");
    case NAME_BACK:     return getStringHash("hover: ELEM_BACK at={0:f},{1:f}");
    case NAME_PREVIEW:  return getStringHash("hover: ELEM_PREVIEW at={0:f},{1:f}");
    case NAME_CLOCK:    return getStringHash("hover: clock at={0:f},{1:f}");
    case NAME_OVERVIEW: return getStringHash("hover: overview at={0:f},{1:f}");
    case NAME_CAFFEINE: return getStringHash("hover: caffeine at={0:f},{1:f}");
    case NAME_WINDOW:   return getStringHash("hover: window-{0:u64} at={2:f},{3:f}");
    case NAME_ENTRY:    return getStringHash("hover: {0:str} at={2:f},{3:f}");
    case NAME_DOCK_ROW: return getStringHash("hover: dock-{0:s} at={1:f},{2:f}");
    case NAME_CHART:    return getStringHash("hover: chart-{0:s} at={1:f},{2:f}");
    case NAME_CHART_N:  return getStringHash("hover: chart-{0:s}-{1} at={2:f},{3:f}");
    case NAME_APPLET:   return getStringHash("hover: applet-{0:str} at={2:f},{3:f}");
    case NAME_APPLET_N: return getStringHash("hover: applet-{0:str}-{2} at={3:f},{4:f}");
    case NAME_CAL_TITLE:   return getStringHash("hover: calendar-{0:s}-{1} at={2:f},{3:f}");
    case NAME_CAL_WEEKDAY: return getStringHash("hover: calendar-weekday-{0:s} at={1:f},{2:f}");
    case NAME_CAL_DAY:     return getStringHash("hover: calendar-day-{0} at={1:f},{2:f}");
    case NAME_NUMBERS:
    case NAME_NUMBERS_BARE:
    case NAME_NUMBERS_NONE:
        return getStringHash("hover: numbers-{0:s} at={3:f},{4:f}");
    default:            return getStringHash("hover: ELEM_ITEM at={0:f},{1:f}");
    }
}

/// `stall: <name> anim=…` (docs/abi.md, *The witness ring*), in answer to
/// `CMD_DUMP_STALL`. One family of literals, read by a human rather than the
/// harness — the name rides as a `{0:s}`-shaped family the same way
/// the others do, which is why this takes a `NameRef` and appends.
public uint stall_literal(uint slot)
{
    switch (slot)
    {
    case NAME_BAR:      return getStringHash("stall: ELEM_BAR anim={0:f},{1:f},{2:f},{3:f}");
    case NAME_POPUP:    return getStringHash("stall: ELEM_POPUP anim={0:f},{1:f},{2:f},{3:f}");
    case NAME_BACK:     return getStringHash("stall: ELEM_BACK anim={0:f},{1:f},{2:f},{3:f}");
    case NAME_PREVIEW:  return getStringHash("stall: ELEM_PREVIEW anim={0:f},{1:f},{2:f},{3:f}");
    case NAME_CLOCK:    return getStringHash("stall: clock anim={0:f},{1:f},{2:f},{3:f}");
    case NAME_OVERVIEW: return getStringHash("stall: overview anim={0:f},{1:f},{2:f},{3:f}");
    case NAME_CAFFEINE: return getStringHash("stall: caffeine anim={0:f},{1:f},{2:f},{3:f}");
    case NAME_WINDOW:
        return getStringHash("stall: window-{0:u64} anim={2:f},{3:f},{4:f},{5:f}");
    case NAME_ENTRY:
        return getStringHash("stall: {0:str} anim={2:f},{3:f},{4:f},{5:f}");
    case NAME_DOCK_ROW:
        return getStringHash("stall: dock-{0:s} anim={1:f},{2:f},{3:f},{4:f}");
    case NAME_CHART:
        return getStringHash("stall: chart-{0:s} anim={1:f},{2:f},{3:f},{4:f}");
    case NAME_CHART_N:
        return getStringHash("stall: chart-{0:s}-{1} anim={2:f},{3:f},{4:f},{5:f}");
    case NAME_APPLET:
        return getStringHash("stall: applet-{0:str} anim={2:f},{3:f},{4:f},{5:f}");
    case NAME_APPLET_N:
        return getStringHash("stall: applet-{0:str}-{2} anim={3:f},{4:f},{5:f},{6:f}");
    case NAME_CAL_TITLE:
        return getStringHash("stall: calendar-{0:s}-{1} anim={2:f},{3:f},{4:f},{5:f}");
    case NAME_CAL_WEEKDAY:
        return getStringHash("stall: calendar-weekday-{0:s} anim={1:f},{2:f},{3:f},{4:f}");
    case NAME_CAL_DAY:
        return getStringHash("stall: calendar-day-{0} anim={1:f},{2:f},{3:f},{4:f}");
    case NAME_NUMBERS:
    case NAME_NUMBERS_BARE:
    case NAME_NUMBERS_NONE:
        return getStringHash("stall: numbers-{0:s} anim={3:f},{4:f},{5:f},{6:f}");
    default:
        return getStringHash("stall: ELEM_ITEM anim={0:f},{1:f},{2:f},{3:f}");
    }
}

/// The name's words, ready for a format's tail to be appended.
public Args name_args(NameRef nm)
{
    Args a = args_new();
    if (nm.count > 0u) { arg(a, nm.a0); }
    if (nm.count > 1u) { arg(a, nm.a1); }
    if (nm.count > 2u) { arg(a, nm.a2); }
    return a;
}

/// The `{n:s}` a `target=` reads.
public uint target_literal(uint target)
{
    if (target == TARGET_BAR)   { return getStringHash("bar"); }
    if (target == TARGET_POPUP) { return getStringHash("popup"); }
    return getStringHash("none");
}

/// Sanity cap on one block's glyph run. The buffer's real length bounds it too;
/// this only keeps a corrupt TextBlock from spinning a per-texel loop. Equal to
/// `shape`'s own MAX_RUN_GLYPHS, which is the cap the shaper writes under.
public static const uint MAX_GLYPHS_PER_BLOCK = MAX_RUN_GLYPHS;

/// The widest a shaped run's advance may be before `bar_shape` refuses to
/// believe it, px. Wider than any bar this will run on, narrower than anything
/// that makes the layout's arithmetic meaningless.
public static const float MAX_METRIC_PX = 16384.0;

// Slug's one concession to hinting, and its three numbers. They are here and
// not in the embassy because a size is the caustic's to ask for and the
// font's to round: `bar_build` snaps every TEXT_PX size here before a block
// is built, and the embassy never learns a size.

/// How far `snap_px` will move a size to make the cap height whole. One pixel:
/// beyond that the tunable's number stops being the number.
public static const int SNAP_WINDOW_PX = 1;

/// How close to a whole pixel the cap height has to land for a size to count
/// as snapped. About an eighth of a pixel — tighter than the eye, loose enough that
/// a face whose cap height is an awkward fraction of the em still matches
/// somewhere.
public static const float SNAP_TOLERANCE = 0.12;

/// The smallest em size worth drawing, device px. A floor on the snap, so an
/// absurd tunable cannot ask for a zero-pixel font.
public static const uint MIN_TEXT_PX = 6u;

// ===========================================================================
// Shape. One definition, three readers: the carve, the paint and the press
// hit-test all call `element_sdf`, which is the whole reason a rounded corner
// of a dock entry is genuinely not part of that entry.
// ===========================================================================

/// Signed distance from a point to a rounded rectangle, negative inside.
/// `rel` is the point relative to the rect's top-left corner. The standard iq
/// formulation, with the radius clamped so it can never invert the shape.
public float rounded_rect_sdf(float2 rel, float2 size, float radius)
{
    float2 half_size = size * 0.5;
    float r = clamp(radius, 0.0, min(half_size.x, half_size.y));
    float2 q = abs(rel - half_size) - (half_size - r);
    return length(max(q, float2(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

/// Signed distance from a point (in the element's target's px, measured at
/// pixel centres by the caller) to element `e`'s carved shape.
public float element_sdf(Element e, float2 p)
{
    return rounded_rect_sdf(p - e.rect.xy, e.rect.zw, e.radius);
}

/// A cheap axis-aligned reject before the sdf. With a 256-slot element table
/// and a per-texel loop this is the difference between one sdf per element per
/// texel and one comparison; the sdf is still the authority on what is inside.
public bool rect_near(float4 r, float2 p, float pad)
{
    return p.x >= r.x - pad && p.x < r.x + r.z + pad
        && p.y >= r.y - pad && p.y < r.y + r.w + pad;
}

// ===========================================================================
// Scale, the lengths derived from it, and the tables the CPU published.
// ===========================================================================

/// px per logical px, never zero — a CPU that has not learned the output
/// scale yet must not collapse the whole layout to a point.
public float scale_px()
{
    return float(max(inputs.scale, 1u));
}

/// How far a pixel size puts the cap height from a whole pixel.
public float cap_error(uint px)
{
    float cap = params.cap_height * float(px);
    return abs(cap - round(cap));
}

/// `want` device px, moved by at most SNAP_WINDOW_PX if that lands the loaded
/// font's cap height on a whole pixel. Slug coverage is unhinted, so a cap
/// height that lands between pixels draws every capital's top edge soft; a
/// size whose cap height is whole is the one hint it takes. Adwaita Sans's
/// cap height is 0.7275 em, so its whole-pixel sizes are multiples of 11 px:
/// a 32 px label snaps to 33 (cap 24.01), and 28 px stays 28 because 22 and
/// 33 are both more than a pixel away.
///
/// `params.cap_height` is 0 until a font has arrived, which is "no snap": the
/// error is zero at every candidate, nothing beats `want`, and the tunable's
/// own number comes back. No memo table: it runs a handful of times on one
/// thread per build.
public uint snap_px(uint want)
{
    want = max(want, MIN_TEXT_PX);
    if (params.cap_height <= 0.0)
    {
        return want;
    }
    uint best = want;
    float best_err = cap_error(want);
    for (int delta = 1; delta <= SNAP_WINDOW_PX; ++delta)
    {
        int candidates[2] = { int(want) - delta, int(want) + delta };
        for (uint k = 0u; k < 2u; ++k)
        {
            int candidate = candidates[k];
            if (candidate < int(MIN_TEXT_PX))
            {
                continue;
            }
            float err = cap_error(uint(candidate));
            if (err < best_err)
            {
                best_err = err;
                best = uint(candidate);
            }
        }
    }
    // Nothing in the window lands cleanly: keep the tunable's own number
    // rather than moving for no gain.
    return best_err > SNAP_TOLERANCE ? want : best;
}

/// Three lengths every pass measures with, in device px, and the slab's opacity. There is no
/// config to outrank the tunables: these are just the tunables, scaled once,
/// in one place.
public float icon_px() { return ICON_LOGICAL * scale_px(); }
public float gap_px() { return ITEM_GAP * scale_px(); }
public float slab_alpha() { return SLAB_OPACITY; }
public float slab_radius() { return BAR_RADIUS * scale_px(); }

/// The band the contents live in: what the tunables asked for, or the surface,
/// when the compositor gave less than the bar asked for. `layout()` centres in
/// it and `row_height()` is measured from it — one definition, because a chart
/// is sized off the row height and `bar_item_w` has to agree with the wing walk
/// that places it.
public float band_px()
{
    return min(float(inputs.bar_size.y), BAR_HEIGHT * scale_px());
}

/// The height of every item's rect on the bar: the band less a chip margin top
/// and bottom.
public float row_height()
{
    return max(band_px() - 2.0 * CHIP_MARGIN * scale_px(), 1.0);
}

/// A chart's width: its height times CHART_ASPECT, floored so a strip of them
/// lands on texel boundaries.
public float chart_width()
{
    return floor(row_height() * CHART_ASPECT);
}

/// Elements the caustic laid out, bounded by the table's real size.
public uint live_elements()
{
    return min(private[0].element_count, MAX_ELEMENTS);
}

/// Items **`bar_build` published**. Both tables live in `private`, so the only
/// bound is the declared one — there is no separately bound buffer
/// whose length could disagree with the count (docs/abi.md, *The private buffer*).
public uint live_items()
{
    return min(private[0].built_items, MAX_ITEMS);
}

/// Blocks `bar_build` published, same reasoning: the count is what the build
/// wrote, never found by scanning for an empty-run sentinel.
public uint live_blocks()
{
    return min(private[0].built_blocks, MAX_BLOCKS);
}

/// Literals the embassy published this frame, bounded by the buffer it bound.
public uint live_literals()
{
    uint capacity;
    uint stride;
    literals.GetDimensions(capacity, stride);
    return min(min(facts[0].header.literal_count, capacity), MAX_LITERALS);
}

/// The literal behind a `getStringHash` value, or a zero-length one when the
/// pool does not carry it.
///
/// A linear walk of a couple of dozen entries, run a handful of times by one
/// thread of `bar_compose` and nowhere else — a sort and a binary search would
/// cost more to write down than they could ever save. `len == 0` is the honest
/// "this caustic named a literal the embassy has no bytes for", and every
/// caller writes nothing rather than a placeholder.
public Literal literal(uint hash)
{
    uint n = live_literals();
    for (uint i = 0u; i < n; ++i)
    {
        if (literals[i].hash == hash)
        {
            return literals[i];
        }
    }
    Literal none;
    none.hash = hash;
    none.off = 0u;
    none.len = 0u;
    none._pad0 = 0u;
    return none;
}

// -- the facts, bounded -----------------------------------------------------

public uint fact_window_count() { return min(facts[0].header.window_count, MAX_WINDOWS); }
public uint fact_app_count()    { return min(facts[0].header.app_count, MAX_APPS); }
public uint fact_image_count()  { return min(facts[0].header.image_count, MAX_IMAGE_SLOTS); }

// ---------------------------------------------------------------------------
// The three buffers a name's bytes can be in (docs/abi.md, *The arenas*), read the
// same way: a byte at a time, bounded, out of words.
//
// `source` is `SRC_STRINGS`, `SRC_ANSWERS`, or a **sidecar slot index**, in
// which case the offset is relative to that slot's region of the rows arena —
// which is exactly the word an `ACT_WANT_ICON` carries back, so the embassy
// reads the bytes out of its own copy of the same buffer.
// ---------------------------------------------------------------------------

/// The byte offset of one slot's region in the rows arena.
public uint rows_region(uint slot) { return slot * ROWS_ARENA_BYTES; }

/// One byte of the strings buffer, or 0 past the end of what the CPU wrote.
public uint utf8_byte(uint offset)
{
    uint words;
    uint stride;
    shapeUtf8.GetDimensions(words, stride);
    uint limit = min(min(words, facts[0].header.string_words), MAX_STRING_WORDS);
    uint w = offset >> 2;
    if (w >= limit)
    {
        return 0u;
    }
    return (shapeUtf8[w] >> ((offset & 3u) * 8u)) & 0xffu;
}

/// One word of the answers arena at a byte offset, or 0 past the live bytes.
/// Every head field of a `GlyphBody` or an `AppBody` is a word at a word
/// offset, so this is how the two heads are read.
public uint answer_byte_word(uint offset)
{
    uint words;
    uint stride;
    answers.GetDimensions(words, stride);
    uint w = offset >> 2;
    return w < words ? answers[w] : 0u;
}

/// One byte of the answers arena, or 0 past the live bytes.
public uint answer_byte(uint offset)
{
    uint words;
    uint stride;
    answers.GetDimensions(words, stride);
    uint limit = min(words, (facts[0].header.answer_bytes + 3u) >> 2);
    uint w = offset >> 2;
    if (w >= limit)
    {
        return 0u;
    }
    return (answers[w] >> ((offset & 3u) * 8u)) & 0xffu;
}

/// One word of a slot's region of the rows arena, bounded by the region.
public uint rows_word(uint slot, uint byte_offset)
{
    if (slot >= MAX_SLOTS || byte_offset + 4u > ROWS_ARENA_BYTES)
    {
        return 0u;
    }
    uint words;
    uint stride;
    rows.GetDimensions(words, stride);
    uint w = (rows_region(slot) + byte_offset) >> 2;
    return w < words ? rows[w] : 0u;
}

/// One byte of a slot's region.
public uint rows_byte(uint slot, uint offset)
{
    return (rows_word(slot, offset & ~3u) >> ((offset & 3u) * 8u)) & 0xffu;
}

/// One byte of whichever of the three buffers `source` names.
public uint src_byte(uint source, uint offset)
{
    if (source == SRC_STRINGS)  { return utf8_byte(offset); }
    if (source == SRC_ANSWERS)  { return answer_byte(offset); }
    return rows_byte(source, offset);
}

// ---------------------------------------------------------------------------
// The rows arena, through the wire's own structs (docs/abi.md, *The arenas*, *Rows*)
// ---------------------------------------------------------------------------

public RowsHeader rows_header(uint slot)
{
    RowsHeader h;
    h.applet = rows_word(slot, 0u);
    h.row_count = min(rows_word(slot, 4u), MAX_ROWS);
    h.bytes_len = rows_word(slot, 8u);
    h.revision = rows_word(slot, 12u);
    return h;
}

/// A row that is not there. Every read of the arena has to survive an index
/// from a table one frame old.
public Row row_none()
{
    Row r;
    r.id = 0u;
    r.parent = NO_INDEX;
    r.role = 0u;
    r.flags = 0u;
    r.label_off = 0u;
    r.label_len = 0u;
    r.icon_off = 0u;
    r.icon_len = 0u;
    r.value = 0.0;
    r.min = 0.0;
    r.max = 0.0;
    r.body = 0u;
    return r;
}

/// A row's body: its offset in the blob, and its length.
/// The wire packs the two into one word, 16 bits each; 0 is no body.
public uint row_body_off(Row row) { return row.body & 0xffffu; }
public uint row_body_len(Row row) { return row.body >> 16; }

/// Row `index` of slot `slot`'s region: the twelve words at
/// `16 + index * 48`, which is where the frame's writer put them.
public Row row_at(uint slot, uint index)
{
    if (index >= min(rows_word(slot, 4u), MAX_ROWS))
    {
        return row_none();
    }
    uint at = 16u + index * 48u;
    Row r;
    r.id = rows_word(slot, at);
    r.parent = rows_word(slot, at + 4u);
    r.role = rows_word(slot, at + 8u);
    r.flags = rows_word(slot, at + 12u);
    r.label_off = rows_word(slot, at + 16u);
    r.label_len = rows_word(slot, at + 20u);
    r.icon_off = rows_word(slot, at + 24u);
    r.icon_len = rows_word(slot, at + 28u);
    r.value = asfloat(rows_word(slot, at + 32u));
    r.min = asfloat(rows_word(slot, at + 36u));
    r.max = asfloat(rows_word(slot, at + 40u));
    r.body = rows_word(slot, at + 44u);
    return r;
}

/// Where a region's blob starts: after the header and the rows, which is what
/// a row's `label_off` and `icon_off` are relative to (docs/abi.md, *Rows*).
public uint rows_blob(uint slot)
{
    return 16u + min(rows_word(slot, 4u), MAX_ROWS) * 48u;
}

/// The row an item's live value comes from — an applet's level, a slider's
/// position (docs/abi.md, *The arenas*). `Item.fact_row` is `slot * MAX_ROWS +
/// index`; `bar_model` stays binding-free, so the lookup is here and the
/// arithmetic is there.
public Row item_row(Item it)
{
    uint at = item_fact_row(it);
    return at == NO_INDEX ? row_none() : row_at(at / MAX_ROWS, at % MAX_ROWS);
}

public float applet_level(Item it) { return item_level(it, item_row(it)); }
public float slider_position(Item it) { return item_position(it, item_row(it)); }

// ---------------------------------------------------------------------------
// Joining by name: this file's own hash, and the two directories
// ---------------------------------------------------------------------------

/// FNV-1a 32 over a run of one of the three buffers, with the length folded
/// in — the caustic's **private** naming (docs/abi.md, *The arenas*). No number
/// computed here crosses the buffer.
public uint hash_bytes(uint source, uint off, uint len)
{
    uint h = FNV_OFFSET;
    for (uint i = 0u; i < len; ++i)
    {
        h = (h ^ src_byte(source, off + i)) * FNV_PRIME;
    }
    return (h ^ len) * FNV_PRIME;
}

/// FNV-1a 32 over a run of one slot's region, a **word** at a time: the bytes
/// outside the run in its first and last word are masked off. The same bytes
/// at another alignment hash differently, which costs at most a rebuild that
/// was not needed. For a body, which is kilobytes where a label is tens of
/// bytes.
public uint hash_region_words(uint slot, uint off, uint len)
{
    uint h = FNV_OFFSET;
    if (len == 0u)
    {
        return h;
    }
    uint end = off + len;
    uint first = off & ~3u;
    uint words = (end - first + 3u) >> 2;
    for (uint w = 0u; w < words; ++w)
    {
        uint at = first + w * 4u;
        uint word = rows_word(slot, at);
        // Keep the bytes in [off, end) and nothing else.
        uint lo = at < off ? off - at : 0u;
        uint hi = min(end - at, 4u);
        uint keep = (hi >= 4u ? 0xffffffffu : ((1u << (hi * 8u)) - 1u)) & ~((1u << (lo * 8u)) - 1u);
        h = (h ^ (word & keep)) * FNV_PRIME;
    }
    return (h ^ len) * FNV_PRIME;
}

/// Whether two runs — in any two buffers — are the same bytes.
public bool bytes_eq(uint a_src, uint a_off, uint a_len, uint b_src, uint b_off, uint b_len)
{
    if (a_len != b_len)
    {
        return false;
    }
    for (uint i = 0u; i < a_len; ++i)
    {
        if (src_byte(a_src, a_off + i) != src_byte(b_src, b_off + i))
        {
            return false;
        }
    }
    return true;
}

/// The image slot whose key is these bytes, or NO_INDEX (docs/abi.md
/// *The facts*): a `Pixmap` the embassy filed under the name it asked about.
public uint image_by_name(uint source, uint off, uint len)
{
    if (len == 0u)
    {
        return NO_INDEX;
    }
    uint n = fact_image_count();
    for (uint i = 0u; i < n; ++i)
    {
        ImageFact img = facts[0].images[i];
        if (img.name_len == 0u)
        {
            continue;
        }
        if (bytes_eq(source, off, len, SRC_STRINGS, img.name_off, img.name_len))
        {
            return img.slot;
        }
    }
    return NO_INDEX;
}

/// A block's px extent, or zero when the index names nothing.
///
/// For a BLOCK_TEXT this is what `bar_shape` measured *this frame*: the run's
/// advance by its line box. `bar_shape` runs before `bar_apply`, so the layout
/// below is never a frame behind the text it is sizing around.
public float2 block_size(uint b)
{
    return b < live_blocks() ? private[0].blocks[b].size : float2(0.0, 0.0);
}

/// An item that is not there. Slang has no null struct, and every read of the
/// item table has to survive a stale index, such as one read from a ring written a frame
/// earlier.
public Item item_none()
{
    Item it;
    it.kind = ITEM_NONE;
    it.flags = 0u;
    it.parent = NO_INDEX;
    it.block = NO_INDEX;
    it.icon_block = NO_INDEX;
    it.aux_block = NO_INDEX;
    it.id = id_zero();
    it.a = 0u;
    it.f = 0.0;
    it.g = 0.0;
    it.fact_row = NO_INDEX;
    it.arg0 = 0u;
    it.arg1 = 0u;
    it.body_blocks = 0u;
    it.body_lines = 0u;
    return it;
}

public Item item_of(uint i)
{
    return i < live_items() ? private[0].items[i] : item_none();
}

/// The index of the item carrying `id` this frame, or NO_INDEX.
///
/// A linear scan, called a handful of times per frame over at most MAX_ITEMS
/// (256) — the same order of work as the layout loops either side of it, and
/// nothing like the cost of being wrong about which row a click landed on.
public uint index_of_id(Id id)
{
    if (id.is_zero())
    {
        return NO_INDEX;
    }
    uint n = live_items();
    for (uint i = 0u; i < n; ++i)
    {
        if (private[0].items[i].id.eq(id))
        {
            return i;
        }
    }
    return NO_INDEX;
}

/// The level itself, whether or not anything in this frame's table carries it.
/// `bar_build` reads it to know whose rows to publish and records the one it
/// built for, so `bar_apply` can say when they have parted company.
public Id menu_level_id()
{
    return private[0].menu_level;
}

/// The item index whose children the popup is showing. NO_INDEX means either
/// no level at all or — the case that closes the menu, in `step_menu` — an
/// item that has gone from the table.
public uint menu_level()
{
    return index_of_id(menu_level_id());
}

/// Hold `item`'s **id** as the level. Takes an index because every caller has
/// one (a click's element, a row's `parent`); the index is resolved here, once,
/// while it is still this frame's.
public void set_menu_level(uint item)
{
    Id id = item_of(item).id;
    // A new level starts at its top: a scroll is where
    // the pointer left *this* list, and means nothing in the next one.
    if (!id.eq(private[0].menu_level))
    {
        private[0].menu_scroll = 0.0;
    }
    private[0].menu_level = id;
}

/// The id of the item under element `elem`, or `id_zero()` when the element is
/// the caustic's own (a slab, the back row) or names nothing.
public Id item_id_of_element(uint elem)
{
    if (elem >= live_elements())
    {
        return id_zero();
    }
    // The id it was laid out with, not whatever this frame's table holds at
    // the index it was laid out with (see `element_item` below).
    return private[0].elements[elem].item_id;
}

/// This frame's index of the item element `elem` was laid out for, or
/// NO_INDEX when that item has gone.
///
/// Commands are answered against the *last* frame's element table, and a
/// build that ran at the top of this frame may have re-indexed the item table
/// under it — a notification arriving above a toast's rows moves every index
/// after it. The index stands only while it still names the id that was laid
/// out; otherwise the id is looked up, so a click names the row that was drawn
/// under the pointer, never the row that has since taken its slot.
public uint element_item(uint elem)
{
    if (elem >= live_elements())
    {
        return NO_INDEX;
    }
    uint ii = private[0].elements[elem].item;
    Id id = private[0].elements[elem].item_id;
    if (ii < live_items() && (id.is_zero() || private[0].items[ii].id.eq(id)))
    {
        return ii;
    }
    return index_of_id(id);
}

/// Remember what the press landed on, so the release can check it is still
/// there (the table may be re-laid-out in between).
public void set_pressed_id(Id id)
{
    private[0].pressed_id = id;
}

public Id pressed_id()
{
    return private[0].pressed_id;
}

/// Decode one of the two index-as-float slots: negative means "nothing".
public uint scratch_index(float v)
{
    return v >= 0.0 ? uint(v + 0.5) : NO_INDEX;
}

/// The window an item names, as the two words `ACT_ACTIVATE_WINDOW` and
/// `ACT_CLOSE_WINDOW` carry. An item that is not a window carries zero, which
/// is a window id the compositor never allocates.
public void act_window(uint kind, Item it)
{
    act2(kind, it.kind == ITEM_WINDOW ? it.arg0 : 0u,
               it.kind == ITEM_WINDOW ? it.arg1 : 0u);
}

/// The app id range an item carries, as `ACT_SPAWN`/`ACT_PIN`/`ACT_UNPIN`
/// carry it: a dock entry's own, or a dock menu row's entry's.
public void act_app(uint kind, Item it)
{
    act2(kind, it.arg0, it.arg1);
}

/// One row of a sidecar, with the wire's own `ACTION_*` and the six words an
/// `ActionBody` is made of (docs/abi.md, *The action ring*, *Action*). The embassy divides `pos` by the
/// surface scale and copies; it reads no row and resolves no name.
public void act_sidecar(Item it, uint kind, uint button, float2 pos, float value)
{
    Id id = it.id;
    // Whether the embassy asks the compositor for an activation token for
    // this click (xdg-activation: what lets the window the click raises take
    // focus): the row asked (`RFLAG_ACTIVATION`) and it is an activation.
    // Read off the row here because the embassy reads no
    // row: a menu row carries it as IFLAG_ACTIVATION, a chip in its frame row.
    bool token = kind == ACTION_ACTIVATE
        && ((it.flags & IFLAG_ACTIVATION) != 0u
            || (item_row(it).flags & RFLAG_ACTIVATION) != 0u);
    Args a = args_new();
    arg(a, id_aux(id));      // the sidecar slot
    arg(a, id_payload(id));  // the row
    arg(a, kind);
    arg(a, button);
    arg2(a, pos);
    argf(a, value);
    arg(a, token ? 1u : 0u);
    push_action(ACT_SIDECAR, a);
}
