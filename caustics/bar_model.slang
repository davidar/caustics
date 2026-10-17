// The bar's model: what the caustic builds for *itself*, and the words it
// builds it out of. Hand-written, hot-reloadable, and **not a contract** — no
// Rust reads this file (docs/abi.md, *The private buffer*).
//
// `bar_abi.slang` and `bar_wire.slang` are rendered from the one declaration
// in `bar/abi/`, and they describe the two contracts: what the embassy writes
// into the buffers, and what a separately built sidecar writes over a pipe.
// This file describes neither. It holds the whole of the
// caustic's private vocabulary — the item, block and element tables that live
// in `bar.slang`'s `private` buffer, the `Id` that names them, and the kinds,
// flags and capacities they are written in — because the embassy reads none
// of it. What crosses the buffer is an action, a witness and
// one word (docs/abi.md, *The action ring* to *The one word*), and none of those names an item.
//
// Each item accessor answers for exactly the kind whose table row gives that
// word a meaning, and returns the "nothing" of its own type for every other
// kind. So a call site never reads `it.a` and never has to remember what a
// kind put there — which is the difference between a struct and a tagged union
// pretending to be one.
//
// The **tunables module** imports this module, not `bar_abi`: it needs
// `ITEM_KIND_COUNT` for `TEXT_PX`, the item kinds for the wing orders, and
// `WHEN_*` and the `ACT_*` verbs for `DOCK_MENU`. `bar_model` re-exports
// `bar_abi` (which re-exports `bar_wire`), so one import reaches everything.

module bar_model;
__exported import bar_abi;

// ---------------------------------------------------------------------------
// Capacities. The caustic's own, because the tables they bound are its own:
// the embassy allocates `private` by the stride Slang reports for it and has
// no opinion about what is inside (docs/abi.md, *The private buffer*).
// ---------------------------------------------------------------------------

/// Element slots. The caustic allocates from `ELEM_FIRST_FREE` up, one per
/// laid-out item plus the slabs. 256 so that a long tray menu (well over a hundred rows) fits beside the bar's own.
public static const uint MAX_ELEMENTS = 256u;

/// Items the table holds: every dock entry, every window under one, every
/// chip, every row of the open menu, the clock. It sizes the per-frame walks.
public static const uint MAX_ITEMS = 256u;

/// Text blocks. A menu row is a label and an icon, so a menu of N rows needs 2N; 512 covers the longest tray menus seen.
public static const uint MAX_BLOCKS = 512u;

/// Answers the build pass indexes in one pass over the answers arena
/// (docs/abi.md, *The arenas*): every `Glyph` and `App` body the embassy has
/// received, as `(hash of the key, byte offset)` so a join is an integer
/// compare rather than a walk of the arena per name.
public static const uint MAX_ANSWERS = 256u;

/// FNV-1a 32, the caustic's **private** hash: what it joins a name in one
/// buffer to a name in another by, and what a dock entry's `ID_ENTRY` payload
/// is (docs/abi.md, *The arenas*).
///
/// Private in the strict sense — no number computed with it crosses the
/// buffer, and the embassy hashes nothing at all. A collision
/// costs one wrong icon and is guarded by comparing the byte length too.
public static const uint FNV_OFFSET = 0x811c9dc5u;
public static const uint FNV_PRIME = 0x01000193u;

/// Rows of the dock's right-click menu table in `bar_tunables.slang`. A fixed
/// width because `bar_build` walks it as an array; an edit of the tunables may reword
/// a row, reorder the table or silence one with `ACT_NONE`.
public static const uint DOCK_MENU_ROWS = 4u;

// ---------------------------------------------------------------------------
// Id: the caustic's private naming (docs/abi.md, *The private buffer*). `hi = (tag <<
// ID_TAG_SHIFT) | aux`, `lo` the payload. An id *is* the fact it names, so a
// stale one resolves to nothing or to what it always meant, by construction.
//
// No id crosses the buffer: an action carries the *operand* — a
// window id, a range of the strings buffer, a slot and a row — and the embassy
// resolves nothing. What the id is still for is this file's own bookkeeping:
// which item the menu level hangs off, which item a press landed on, and which
// item an element laid out last frame.
// ---------------------------------------------------------------------------

public static const uint ID_TAG_SHIFT = 24u;
public static const uint ID_AUX_MASK = 0x00ffffffu;

/// No id at all.
public static const uint ID_NONE = 0u;
public static const uint ID_OVERVIEW = 1u;
public static const uint ID_CLOCK = 2u;
public static const uint ID_CAFFEINE = 3u;
/// A dock entry; the payload is its app id's hash.
public static const uint ID_ENTRY = 4u;
/// A window; `aux` is the high word of the compositor's id and the payload the
/// low one.
public static const uint ID_WINDOW = 5u;
/// A row of a dock entry's own menu; `aux` is the row's `ACT_*` verb and the
/// payload its entry's app hash.
public static const uint ID_DOCK_ROW = 6u;
/// A sidecar's row; `aux` is the slot and the payload the sidecar's row id.
public static const uint ID_APPLET = 7u;
/// A chart; the payload is its row in the samples body.
public static const uint ID_CHART = 8u;
/// A cell of the clock's calendar; `aux` is which part —
/// `CAL_TITLE`, `CAL_WEEKDAY`, `CAL_DAY` — and the payload says which one:
/// `year << 8 | month` for the title, the weekday (0 = Sunday; `layout_calendar`
/// turns it into a column) for a weekday initial, the
/// day of the month for a day.
public static const uint ID_CALENDAR = 9u;
public static const uint CAL_TITLE = 0u;
public static const uint CAL_WEEKDAY = 1u;
public static const uint CAL_DAY = 2u;
/// One line of the chart numbers; the payload is its
/// chart's row in the samples body, as `ID_CHART`'s is.
public static const uint ID_NUMBERS = 10u;

public struct Id
{
    public uint lo;
    public uint hi;

    /// The same id.
    public bool eq(Id other)
    {
        return lo == other.lo && hi == other.hi;
    }

    /// "No id": what a cleared level and an element that laid nothing out
    /// carry, and what no fact ever names.
    public bool is_zero()
    {
        return lo == 0u && hi == 0u;
    }
}

/// "No id", as a value.
public Id id_zero()
{
    Id id;
    id.lo = 0u;
    id.hi = 0u;
    return id;
}

/// An id from its three parts: an `ID_*` tag, 24 bits of `aux`, a 32-bit
/// payload.
public Id id_tagged(uint tag, uint aux, uint payload)
{
    Id id;
    id.lo = payload;
    id.hi = (tag << ID_TAG_SHIFT) | (aux & ID_AUX_MASK);
    return id;
}

public uint id_tag(Id id)
{
    return id.hi >> ID_TAG_SHIFT;
}

public uint id_aux(Id id)
{
    return id.hi & ID_AUX_MASK;
}

public uint id_payload(Id id)
{
    return id.lo;
}

// ---------------------------------------------------------------------------
// Blocks: one thing an item shows.
// ---------------------------------------------------------------------------

/// A shaped string: `a` is its byte offset in the strings buffer and `b` its
/// byte length. `bar_shape` writes `size`, `glyph_count` and `flags`.
public static const uint BLOCK_TEXT = 0u;
/// One glyph of the curve buffers, drawn to fill a `px` by `px` box: `a` is
/// the glyph id. Every symbolic icon is one of these.
public static const uint BLOCK_GLYPH = 1u;
/// One slot of the image array, drawn to fill a `px` by `px` box: `a` is the
/// slot. Everything that is pixels.
public static const uint BLOCK_IMAGE = 2u;

/// `TextBlock.bflags`: this block's bytes are composed by `bar_compose` into
/// the region the embassy reserved at `a`, and `hash` names the composition
/// rather than a literal.
public static const uint BLOCK_COMPOSED = 1u;
/// `TextBlock.bflags`: one block of a menu row's **body** (a
/// notification's wrapped text under its title). Allocated a slot a byte plus eight rather than four a byte (`body_slots`),
/// because a body is hundreds of bytes and the slots are the bar's whole glyph
/// budget; a body block that runs out of them is a cut, drawn with the
/// ellipsis, and not the bug a truncated label is.
public static const uint BLOCK_BODY = 2u;
/// `TextBlock.bflags`, with BLOCK_BODY: this block starts a paragraph — the
/// body's first, or the first after one of its own newlines. A body block
/// without it continues the paragraph before it (a long paragraph is shaped
/// in chunks of at most BODY_CHUNK_BYTES).
public static const uint BLOCK_PARAGRAPH = 4u;

/// What a `BLOCK_COMPOSED` block's `hash` names.
public static const uint COMPOSE_NONE = 0u;
/// `HH:MM` from `inputs.clock_time`.
public static const uint COMPOSE_CLOCK_TIME = 1u;
/// The date from `inputs.clock_date`, in the tunables' own words and order.
public static const uint COMPOSE_CLOCK_DATE = 2u;

/// The tokens the tunables' `CLOCK_DATE_ORDER` may name.
public static const uint DATE_NONE = 0u;
/// The weekday's word, from `WEEKDAYS[weekday]`, 0 = Sunday.
public static const uint DATE_WEEKDAY = 1u;
/// The day of the month, 1..31, no leading zero.
public static const uint DATE_DAY = 2u;
/// The month's word, from `MONTHS[month - 1]`.
public static const uint DATE_MONTH = 3u;

/// One thing an item shows. 64 B, and half of it is `bar_shape`'s: a
/// `BLOCK_TEXT`'s `size`, `glyph_count` and `flags` come back from the shaper,
/// which is what lets the layout ask a buffer how wide a string is.
public struct TextBlock
{
    /// Extent in px.
    public float2 size;
    /// `BLOCK_*`.
    public uint kind;
    /// `SHAPE_RUN_*`, written by `bar_shape`.
    public uint flags;
    /// `BLOCK_TEXT`: byte offset in the strings buffer. `BLOCK_GLYPH`: the
    /// glyph id. `BLOCK_IMAGE`: the slot.
    public uint a;
    /// `BLOCK_TEXT`: byte length.
    public uint b;
    /// `BLOCK_TEXT`: the first of this block's glyph slots in `runs`.
    public uint first_glyph;
    /// Glyphs the shape pass wrote.
    public uint glyph_count;
    /// Premultiplied linear RGBA.
    public float4 color;
    /// Device px: the em size for a `BLOCK_TEXT`, the box side otherwise.
    public float px;
    /// The `getStringHash` the bytes came from, a `COMPOSE_*` when
    /// `BLOCK_COMPOSED` is set, or 0.
    public uint hash;
    /// `BLOCK_COMPOSED`.
    public uint bflags;
    /// Which buffer a `BLOCK_TEXT`'s bytes are in (docs/abi.md, *The arenas*):
    /// `SRC_STRINGS`, `SRC_ANSWERS`, or a sidecar slot index, in which case
    /// `a` is relative to that slot's region of the rows arena. `bar_shape`
    /// hands `shapeRun` the matching buffer and `bar_apply` covers only the
    /// strings buffer's, because a `REQ_COVER` carries the embassy's own
    /// bytes.
    public uint source;
}

// ---------------------------------------------------------------------------
// Items: one thing that exists.
// ---------------------------------------------------------------------------

public static const uint ITEM_NONE = 0u;
/// A dock entry. `a` is its place among the favourites or NO_INDEX; `arg0`/`arg1` are its
/// app id's range in the strings buffer, which is what an `ACT_SPAWN`,
/// `ACT_PIN` or `ACT_UNPIN` carries.
public static const uint ITEM_DOCK = 1u;
/// A window under a dock entry. `a` is its focus stamp; `arg0`/`arg1` are the
/// compositor's 64-bit id, low word first, which is what an
/// `ACT_ACTIVATE_WINDOW` or `ACT_CLOSE_WINDOW` carries.
public static const uint ITEM_WINDOW = 2u;
/// The clock: a composed time block and a composed date block.
public static const uint ITEM_CLOCK = 3u;
/// The overview button.
public static const uint ITEM_OVERVIEW = 4u;
/// One row of an open menu — a sidecar's, or a dock entry's. A dock row's `a`
/// is the `ACT_*` its tunable named and `arg0`/`arg1` its entry's app range.
public static const uint ITEM_MENU_ROW = 5u;
/// One chart of the samples body; `a` is its row.
public static const uint ITEM_CHART = 6u;
/// One chip a sidecar published, and the in-process caffeine chip. `a` is its
/// `APPLET_*`, `f` a level or -1, `fact_row` the row its live value is in.
public static const uint ITEM_APPLET = 7u;
/// A slider a sidecar published under a menu.
public static const uint ITEM_SLIDER = 8u;
/// One cell of the clock's calendar: the month's title,
/// a weekday initial or a day number, under the clock as its menu level. Text
/// and nothing else — not a button, not a row: it takes no click and wears no
/// hover chip, and today's carries IFLAG_ATTENTION.
public static const uint ITEM_CAL_CELL = 9u;
/// One line of the chart numbers: a chart's swatch, its
/// name and its newest sample composed in its unit, in the popup the strip's
/// hover or click opens. `a` is the chart's row in the samples body; `block`
/// is the name (a literal of the pool) and `aux_block` the composed value.
/// Text and a swatch — not a button, not a row: no click, no hover chip.
public static const uint ITEM_CHART_LINE = 10u;

/// How many kinds there are, and so how long `TEXT_PX` is. The tray, the
/// battery and the power button are not kinds: they are rows of a sidecar's
/// frame, drawn as `ITEM_APPLET` and its menu.
public static const uint ITEM_KIND_COUNT = 11u;

public static const uint IFLAG_PINNED = 1u << 0;
public static const uint IFLAG_RUNNING = 1u << 1;
public static const uint IFLAG_FOCUSED = 1u << 2;
public static const uint IFLAG_URGENT = 1u << 3;
public static const uint IFLAG_ATTENTION = 1u << 4;
public static const uint IFLAG_SEPARATOR = 1u << 5;
public static const uint IFLAG_DISABLED = 1u << 6;
public static const uint IFLAG_CHECKABLE = 1u << 7;
public static const uint IFLAG_CHECKED = 1u << 8;
public static const uint IFLAG_SUBMENU = 1u << 9;
/// This chip's click opens its rows as a menu.
public static const uint IFLAG_HAS_MENU = 1u << 10;
/// dbusmenu's `toggle-type`: a radio row is checkable *and* radio.
public static const uint IFLAG_RADIO = 1u << 11;
/// The row's `RFLAG_ACTIVATION`: its activation wants an activation token
/// (docs/abi.md, *Rows*, *Request*). Carried so the click needs no walk of the arena.
public static const uint IFLAG_ACTIVATION = 1u << 12;

/// One thing that exists, 64 B. What it is, where it hangs, what it shows,
/// what this file calls it, three payload words the kind fixes the meaning of,
/// and the two operand words an action carries.
public struct Item
{
    /// `ITEM_*`.
    public uint kind;
    /// `IFLAG_*`.
    public uint flags;
    /// Parent item index, or NO_INDEX.
    public uint parent;
    /// The item's text block, or NO_INDEX.
    public uint block;
    /// The item's icon block, or NO_INDEX.
    public uint icon_block;
    /// A second text block, or NO_INDEX: the clock's date beside its time,
    /// a chart number's value, a menu row's body's first block.
    public uint aux_block;
    /// What this file calls it.
    public Id id;
    /// Payload: a cardinal.
    public uint a;
    /// Payload: a fraction.
    public float f;
    /// Payload: a second float. No kind declares it, and `bar_check` says so.
    public float g;
    /// The row of the rows arena this item's **live** value comes from, as
    /// `slot * MAX_ROWS + index`, or NO_INDEX (docs/abi.md, *The arenas*).
    public uint fact_row;
    /// The action operands: a window's 64-bit id, low word first, or
    /// an app id's `(offset, length)` in the strings buffer. The embassy
    /// forwards these words and resolves nothing, so the whole of what a click
    /// means is decided here (docs/abi.md, *The action ring*).
    public uint arg0;
    public uint arg1;
    /// A menu row's **body**: how many blocks from
    /// `aux_block` on are its body, in the low byte, and BODY_MORE when the
    /// body has bytes past what was shaped. 0 for every row without one and
    /// every other kind. Written by `bar_build`. With BODY_CAPPED, the next
    /// byte is the body lines the row shows in the popup that fixed them when
    /// it opened (a toast's row keeps the lines it opened with); written by
    /// that popup's subject each frame, cleared by every build.
    public uint body_blocks;
    /// The body's wrapped lines: the first in `Private.body_lines` in the low
    /// 16 bits, the count in the next 8, and BODY_MORE when the wrap stopped
    /// with body left over. Written by `bar_apply`'s wrap, after the shape
    /// pass measured the glyphs; 0 is no lines.
    public uint body_lines;
}

/// `Item.body_blocks` and `Item.body_lines`: there is more body than this.
public static const uint BODY_MORE = 1u << 31;
/// `Item.body_blocks`: the row carries its own body-line cap, in bits 8..15.
public static const uint BODY_CAPPED = 1u << 30;

/// The body lines a row's popup fixed it to, or NO_INDEX when it fixed none.
public uint item_body_cap(Item it)
{
    return (it.body_blocks & BODY_CAPPED) != 0u ? ((it.body_blocks >> 8) & 0xffu) : NO_INDEX;
}

/// `body_blocks` with the cap `lines` set, or cleared when `lines` is NO_INDEX.
public uint body_blocks_capped(uint body_blocks, uint lines)
{
    uint word = body_blocks & ~(BODY_CAPPED | (0xffu << 8));
    return lines == NO_INDEX ? word : word | BODY_CAPPED | (min(lines, 0xffu) << 8);
}

/// The body's block count, its first wrapped line and its line count.
public uint item_body_block_count(Item it)
{
    return it.kind == ITEM_MENU_ROW ? (it.body_blocks & 0xffu) : 0u;
}

public uint item_body_first_line(Item it)
{
    return it.body_lines & 0xffffu;
}

public uint item_body_line_count(Item it)
{
    return it.kind == ITEM_MENU_ROW ? ((it.body_lines >> 16) & 0xffu) : 0u;
}

/// A dock entry's index into the favourites file, or NO_INDEX — for an entry
/// that is running but not pinned, and for every other kind.
public uint item_favourite(Item it)
{
    return it.kind == ITEM_DOCK ? it.a : NO_INDEX;
}

/// A window's focus stamp: the embassy's monotonic counter. 0 for every other
/// kind, and for a window that has never been focused.
public uint item_focus_stamp(Item it)
{
    return it.kind == ITEM_WINDOW ? it.a : 0u;
}

/// A chart's row index into `samples[0].charts`, or NO_INDEX — for a chart,
/// and for a line of the chart numbers, which names the chart it reads.
public uint item_chart_row(Item it)
{
    return (it.kind == ITEM_CHART || it.kind == ITEM_CHART_LINE) ? it.a : NO_INDEX;
}

/// An applet's `APPLET_*`, or `APPLET_OTHER`.
public uint item_applet(Item it)
{
    return it.kind == ITEM_APPLET ? it.a : APPLET_OTHER;
}

/// The verb a dock menu row performs, as an `ACT_*`, or `ACT_NONE`.
public uint item_dock_verb(Item it)
{
    return (it.kind == ITEM_MENU_ROW && id_tag(it.id) == ID_DOCK_ROW) ? it.a : ACT_NONE;
}

/// The row of the rows arena an item's live value comes from — a slot and an
/// index, packed as `slot * MAX_ROWS + index` — or NO_INDEX.
public uint item_fact_row(Item it)
{
    return (it.kind == ITEM_APPLET || it.kind == ITEM_SLIDER) ? it.fact_row : NO_INDEX;
}

/// A value 0..1 from a row's own units. The wire's reader has already rejected
/// `min >= max`; the guard is here so a hand-built row cannot divide by zero.
public float normalise_row(float value, float min, float max)
{
    return max > min ? saturate((value - min) / (max - min)) : 0.0;
}

/// An applet's level 0..1, or -1 when it has none (and for every other kind).
/// `row` is the wire `Row` at `item_fact_row(it)`; this module stays
/// binding-free, so the lookup is the host file's.
public float item_level(Item it, Row row)
{
    if (it.kind != ITEM_APPLET)
    {
        return -1.0;
    }
    return item_fact_row(it) == NO_INDEX ? it.f : row.value;
}

/// A slider's position 0..1, or 0 — normalised from the row's own range.
public float item_position(Item it, Row row)
{
    if (it.kind != ITEM_SLIDER)
    {
        return 0.0;
    }
    return item_fact_row(it) == NO_INDEX ? it.f : normalise_row(row.value, row.min, row.max);
}

// ---------------------------------------------------------------------------
// Elements: one laid-out rectangle, and what the witness ring remembers of it.
// ---------------------------------------------------------------------------

/// The bar's slab: the whole surface, and the default owner of every texel.
public static const uint ELEM_BAR = 0u;
/// The popup's slab.
public static const uint ELEM_POPUP = 1u;
/// The menu's back row.
public static const uint ELEM_BACK = 2u;
/// The preview's still.
public static const uint ELEM_PREVIEW = 3u;
/// Where the per-item allocations start.
public static const uint ELEM_FIRST_FREE = 8u;

public static const uint EFLAG_HOVERED = 1u << 0;
public static const uint EFLAG_PRESSED = 1u << 1;
public static const uint EFLAG_PRESENT = 1u << 2;

/// One element's layout, its springs, and the words the `layout:` witness is
/// compared against.
///
/// The last three make the caustic its own witness (docs/abi.md, *The witness ring*),
/// since it alone knows when an element appears, moves or leaves. After the layout,
/// `witness_placed` compares `(id, target, rect)`
/// with what it left here and pushes one witness when they differ — by **id**,
/// because an unpin can swap which app a slot holds without moving a rect.
public struct Element
{
    public float4 rect;
    public uint target;
    public uint flags;
    public uint block;
    /// The item this element lays out, or NO_INDEX.
    public uint item;
    /// That item's **id** as it was laid out. A click is
    /// answered against the last frame's elements, and a build between that
    /// layout and the click re-indexes the item table: `item` may then name
    /// another row. The id is what the pointer was over; `element_item`
    /// resolves it against this frame's table.
    public Id item_id;
    /// Hover and press springs: x = hover, y = press, z and w their
    /// velocities. The two slabs have no springs and leave it zero.
    public float4 anim;
    public float radius;
    /// `IFLAG_*` the caustic derived for this element's item — a dock entry's
    /// `PINNED`/`RUNNING`/`FOCUSED`/`URGENT`.
    public uint derived;
    /// The cardinal beside it: a dock entry's running-window count.
    public uint derived_a;
    /// What `witness_body` last said for the row here: a
    /// hash of its id and its line count, or 0 for nothing.
    public uint said_body;
    /// The id this element last *witnessed* laying out, and the rect and
    /// target it witnessed for it.
    public Id said_id;
    public uint said_target;
    /// Non-zero when a `layout:` line has been pushed for this element and no
    /// departure line since.
    public uint said_present;
    public float4 said_rect;
    /// The name that line was pushed under, kept whole: which literal family
    /// and its leading words.
    ///
    /// Kept rather than re-derived because a **departure** has to be named
    /// after the item is gone — an app id's bytes are no longer in the facts
    /// by the time its dock entry leaves — and naming the thing that left is
    /// the whole point of the line.
    public uint said_slot;
    public uint said_a0;
    public uint said_a1;
    public uint said_a2;
}

// ---------------------------------------------------------------------------
// The dock menu's `WHEN_*`, and the two name tables the witnesses need.
// ---------------------------------------------------------------------------

/// A `DOCK_MENU` row's third word: which fact about the entry it wants.
public static const uint WHEN_ALWAYS = 0u;
/// Only while the entry has a window.
public static const uint WHEN_RUNNING = 1u;
/// Only while the entry is a favourite.
public static const uint WHEN_PINNED = 2u;
/// Only while it is not.
public static const uint WHEN_UNPINNED = 3u;

/// The word a dock menu row wears in a `layout:` line, by the `ACT_*` its
/// tunable named: `dock-pin`, `dock-close`. Exactly one dock menu can be open,
/// so the verb alone names the row — and `bar/bar-test.sh` clicks it by that
/// name.
///
/// They are `getStringHash` literals, resolved by the embassy's printer through the
/// same reflection table every other literal comes from.
public uint dock_verb_literal(uint verb)
{
    switch (verb)
    {
    case ACT_SPAWN:        return getStringHash("new-window");
    case ACT_CLOSE_WINDOW: return getStringHash("close");
    case ACT_PIN:          return getStringHash("pin");
    case ACT_UNPIN:        return getStringHash("unpin");
    default:               return getStringHash("?");
    }
}

/// The word a chart wears in a `layout:` line, by the wire's `CHART_*`:
/// `chart-cpu`, `chart-gpu`: seven literals, one per `CHART_*` plus `other`.
public uint chart_literal(uint kind)
{
    switch (kind)
    {
    case CHART_CPU:  return getStringHash("cpu");
    case CHART_TEMP: return getStringHash("temp");
    case CHART_MEM:  return getStringHash("mem");
    case CHART_NET:  return getStringHash("net");
    case CHART_DISK: return getStringHash("disk");
    case CHART_GPU:  return getStringHash("gpu");
    default:         return getStringHash("other");
    }
}
