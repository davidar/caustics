// The bar, as a caustic — layout, hit-testing, policy and drawing, in one file.
//
// docs/architecture.md's argument for a GPU program that holds the policy,
// applied to a status bar:
// the thing that decides where an element *is* is the same thing that decides
// which texel belongs to it and what colour that texel ends up. Change
// ICON_LOGICAL in bar_tunables.slang and every dock entry grows, the carve grows
// with it, the hover glow traces the new boundary and the pointer starts
// hitting the new edge — with no second definition anywhere to keep in sync,
// and no rebuild (`caustics-reload`, the compiler sidecar, recompiles this file and
// the bar swaps every pipeline together in well under a second).
//
// Seven entry points over one descriptor set 0 (docs/abi.md, *Who writes what*). Three
// run in the shape submission, only when something built is owed —
// `bar_build` (the item and block tables), `bar_compose` (the bytes of
// composed text) and `bar_shape` (described below). The other four run every
// frame, `bar_apply` once and the rest once per mapped target — the bar's
// layer surface and, while it is up, the popup:
//
//   bar_apply       1x1x1  — the command ring, the click policy, the action
//                            ring, the popup state machine, the springs, and
//                            the layout of *every* element of *every* target.
//                            Runs once per frame, before anything.
//   bar_visibility  8x8    — element id per texel into `ids`. The carve.
//   bar_sample      1x1x1  — the id under the pointer -> `Private.hover_element`.
//   bar_draw        8x8    — paint, premultiplied, onto the swapchain image.
//
// ---------------------------------------------------------------------------
// Items, not named elements.
//
// This file knows no element by name. It lays out an **item table**: the
// dock's entries and the windows under them, the tray's items, the battery,
// the clock, the rows of an open menu, each tagged with a wing. Everything
// below lays *that* out: the left wing left-to-right from the edge padding,
// the right wing right-to-left, the centre wing centred on the whole bar, and
// the popup either as menu rows or as a captured still. Elements are allocated
// from ELEM_FIRST_FREE up, one per laid-out item, and `Element.item` is the
// map the CPU reads a hover back through.
//
// The other half is that this file *decides*. A click on a dock
// entry picks which window to focus here, from the item table, and pushes an
// `Action`; the CPU carries it out without re-deriving anything. The action
// ring is cleared at the top of every `bar_apply` and filled during it, so a
// frame's decisions appear in the action ring the embassy reads back for
// that frame exactly once — no acknowledgement command, no chance of replaying a click.
//
// State that must survive a dispatch and has no home elsewhere (the menu
// level; the hover-dwell clock) lives in `private` (`Private`, below), which
// is this file's alone. Never a `static` — a compute dispatch has no memory
// between runs, and a `static` here would silently be re-zeroed.
//
// Nothing in here is per-applet: an applet is a sidecar publishing rows
// (ITEM_APPLET, ITEM_SLIDER), and the item payload is read through the
// `item_*` accessors of `bar_model`.
// ---------------------------------------------------------------------------
//
// ---------------------------------------------------------------------------
// **Text and icons are this file's.** There is no glyph atlas: no CPU
// shaping a string, rasterising every glyph at the size it will be drawn at,
// packing and evicting bitmaps and handing over quads with uv rectangles —
// and re-doing the lot whenever the output scale changes. Instead:
//
//   `import text`    Slug coverage straight from quadratic outlines, in
//                    descriptor set 1. A glyph is an index and a size is a
//                    multiply, so a fractional scale costs nothing.
//   `import shape`   Latin shaping as a compute pass, its font tables in
//                    descriptor set 2. `bar_shape` is the entry point
//                    that turns each BLOCK_TEXT's *bytes* into positioned
//                    glyphs and writes the run's advance back into the
//                    block's `size`, so the layout reads every text width out
//                    of a buffer and the CPU never learns a text metric.
//
// What the embassy does is I/O: read the font file, read an SVG, keep the
// clock, and put UTF-8 in a buffer. The shape pass runs before `bar_apply`
// because `layout()` measures what it wrote.
// ---------------------------------------------------------------------------
//
// This caustic blends: the bar is translucent by
// design (SLAB_OPACITY in the tunables, 0.5 as shipped), the surface is
// PreMultiplied, and every colour written to `dst` is premultiplied. That is
// the one rule this file never bends — a straight-alpha write here shows up as
// a bright halo around every glyph. The image slots hold straight alpha as the
// wire carries it and the sample premultiplies; the preview texture is
// premultiplied on upload. So every colour this file computes with is
// premultiplied, and there is no unpremultiply step anywhere in it.
//
// The embassy writes `inputs`, `params`, the command ring, the facts, the
// strings up to `compose_base`, the image slots, the preview, and the two
// arenas a sidecar fills (`samples`, `rows`). This file writes `state` (the
// action and witness rings, read back one frame late — the only readback),
// `private`, the run buffer, and the strings after `compose_base`
// (docs/abi.md, *Who writes what*).
//
// ---------------------------------------------------------------------------
// Destination format. Storage-image formats are baked into SPIR-V, so this
// file compiles once per swapchain format:
//
//   -DDST_FORMAT_RGBA8   rgba8unorm swapchain (an explicit SPIR-V format)
//   -DDST_FORMAT_BGRA8   bgra8unorm swapchain (SPIR-V has no BGRA image
//                        format; the image is declared format-unknown, which
//                        needs -default-image-format-unknown on the command
//                        line and the BGRA8UNORM_STORAGE feature at runtime)
//
// Six of the seven entries never touch `dst`, so a missing define is not a
// hard #error: the image falls back to format-unknown, which still needs
// -default-image-format-unknown. build.rs compiles every entry
// entries per variant.
//
// One toolchain hazard, if you are editing this file: never let a
// `[numthreads]` attribute detach from its entry point and land on a helper
// above it. slangc 2026.14.1 segfaults instead of diagnosing it
// (docs/slangc-numthreads.md).
// ---------------------------------------------------------------------------

// ===========================================================================
// The modules imported (docs/abi.md). `bar_abi` and `bar_tunables` declare
// neither a binding nor an entry point; `text` declares its four buffers in
// descriptor **set 1** and `shape` declares none at all, gathering its handles
// through a `ShapeTables` this file fills from set 2 — so set 0 below is
// this file's alone.
//
//   bar_abi        the byte-exact contract with the embassy — generated from
//                  `bar/abi/src/buffer.rs`, and re-exporting `bar_wire`, the
//                  half of the sidecar contract the GPU reads (docs/abi.md, *The arenas*).
//   bar_model      the helpers that read the ABI's words by meaning: the id
//                  parts, the item payload accessors. Hand-written, because a
//                  generated file holds declarations and nothing that computes.
//   bar_tunables   every knob this file reads — and the bar's whole config
//                  (docs/architecture.md, *Configuration is the caustic*).
//                  Edit it here and save: `caustics-reload` recompiles this
//                  file with it, and the running bar moves.
// ===========================================================================

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import shape;
import text;
import bar_state;

/// Sanity cap on one block's glyph run. The buffer's real length bounds it too;
/// this only keeps a corrupt TextBlock from spinning a per-texel loop. Equal to
/// `shape`'s own MAX_RUN_GLYPHS, which is the cap the shaper writes under.
static const uint MAX_GLYPHS_PER_BLOCK = MAX_RUN_GLYPHS;

/// The widest a shaped run's advance may be before `bar_shape` refuses to
/// believe it, px. Wider than any bar this will run on, narrower than anything
/// that makes the layout's arithmetic meaningless.
static const float MAX_METRIC_PX = 16384.0;

// Slug's one concession to hinting, and its three numbers. They are here and
// not in the embassy because a size is the caustic's to ask for and the
// font's to round: `bar_build` snaps every TEXT_PX size here before a block
// is built, and the embassy never learns a size.

/// How far `snap_px` will move a size to make the cap height whole. One pixel:
/// beyond that the tunable's number stops being the number.
static const int SNAP_WINDOW_PX = 1;

/// How close to a whole pixel the cap height has to land for a size to count
/// as snapped. About an eighth of a pixel — tighter than the eye, loose enough that
/// a face whose cap height is an awkward fraction of the em still matches
/// somewhere.
static const float SNAP_TOLERANCE = 0.12;

/// The smallest em size worth drawing, device px. A floor on the snap, so an
/// absurd tunable cannot ask for a zero-pixel font.
static const uint MIN_TEXT_PX = 6u;

// ===========================================================================
// Shape. One definition, three readers: the carve, the paint and the press
// hit-test all call `element_sdf`, which is the whole reason a rounded corner
// of a dock entry is genuinely not part of that entry.
// ===========================================================================

/// Signed distance from a point to a rounded rectangle, negative inside.
/// `rel` is the point relative to the rect's top-left corner. The standard iq
/// formulation, with the radius clamped so it can never invert the shape.
float rounded_rect_sdf(float2 rel, float2 size, float radius)
{
    float2 half_size = size * 0.5;
    float r = clamp(radius, 0.0, min(half_size.x, half_size.y));
    float2 q = abs(rel - half_size) - (half_size - r);
    return length(max(q, float2(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

/// Signed distance from a point (in the element's target's px, measured at
/// pixel centres by the caller) to element `e`'s carved shape.
float element_sdf(Element e, float2 p)
{
    return rounded_rect_sdf(p - e.rect.xy, e.rect.zw, e.radius);
}

/// A cheap axis-aligned reject before the sdf. With a 256-slot element table
/// and a per-texel loop this is the difference between one sdf per element per
/// texel and one comparison; the sdf is still the authority on what is inside.
bool rect_near(float4 r, float2 p, float pad)
{
    return p.x >= r.x - pad && p.x < r.x + r.z + pad
        && p.y >= r.y - pad && p.y < r.y + r.w + pad;
}

/// Distance from `p` to the segment ab. The building block of the two SDF
/// symbols this file draws instead of shipping a symbol font of its own.
float sd_segment(float2 p, float2 a, float2 b)
{
    float2 pa = p - a;
    float2 ba = b - a;
    float h = saturate(dot(pa, ba) / max(dot(ba, ba), 1e-6));
    return length(pa - ba * h);
}

/// A check mark inscribed in the `m`-sided box at `org`.
float check_sdf(float2 p, float2 org, float m)
{
    float2 a = org + float2(0.20, 0.52) * m;
    float2 b = org + float2(0.41, 0.74) * m;
    float2 c = org + float2(0.82, 0.26) * m;
    return min(sd_segment(p, a, b), sd_segment(p, b, c));
}

/// A chevron in the `m`-sided box at `org`, pointing right when `dir` is
/// positive and left when it is negative.
float chevron_sdf(float2 p, float2 org, float m, float dir)
{
    float tip_x = dir > 0.0 ? 0.66 : 0.34;
    float back_x = dir > 0.0 ? 0.36 : 0.64;
    float2 tip = org + float2(tip_x, 0.50) * m;
    float2 top = org + float2(back_x, 0.24) * m;
    float2 bot = org + float2(back_x, 0.76) * m;
    return min(sd_segment(p, top, tip), sd_segment(p, tip, bot));
}

// ===========================================================================
// Scale, the lengths derived from it, and the tables the CPU published.
// ===========================================================================

/// px per logical px, never zero — a CPU that has not learned the output
/// scale yet must not collapse the whole layout to a point.
float scale_px()
{
    return float(max(inputs.scale, 1u));
}

/// How far a pixel size puts the cap height from a whole pixel.
float cap_error(uint px)
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
uint snap_px(uint want)
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
float icon_px() { return ICON_LOGICAL * scale_px(); }
float gap_px() { return ITEM_GAP * scale_px(); }
float slab_alpha() { return SLAB_OPACITY; }
float slab_radius() { return BAR_RADIUS * scale_px(); }

/// The band the contents live in: what the tunables asked for, or the surface,
/// when the compositor gave less than the bar asked for. `layout()` centres in
/// it and `row_height()` is measured from it — one definition, because a chart
/// is sized off the row height and `bar_item_w` has to agree with the wing walk
/// that places it.
float band_px()
{
    return min(float(inputs.bar_size.y), BAR_HEIGHT * scale_px());
}

/// The height of every item's rect on the bar: the band less a chip margin top
/// and bottom.
float row_height()
{
    return max(band_px() - 2.0 * CHIP_MARGIN * scale_px(), 1.0);
}

/// A chart's width: its height times CHART_ASPECT, floored so a strip of them
/// lands on texel boundaries.
float chart_width()
{
    return floor(row_height() * CHART_ASPECT);
}

/// Elements the caustic laid out, bounded by the table's real size.
uint live_elements()
{
    return min(private[0].element_count, MAX_ELEMENTS);
}

/// Items **`bar_build` published**. Both tables live in `private`, so the only
/// bound is the declared one — there is no separately bound buffer
/// whose length could disagree with the count (docs/abi.md, *The private buffer*).
uint live_items()
{
    return min(private[0].built_items, MAX_ITEMS);
}

/// Blocks `bar_build` published, same reasoning: the count is what the build
/// wrote, never found by scanning for an empty-run sentinel.
uint live_blocks()
{
    return min(private[0].built_blocks, MAX_BLOCKS);
}

/// Literals the embassy published this frame, bounded by the buffer it bound.
uint live_literals()
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
Literal literal(uint hash)
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

uint fact_window_count() { return min(facts[0].header.window_count, MAX_WINDOWS); }
uint fact_app_count()    { return min(facts[0].header.app_count, MAX_APPS); }
uint fact_image_count()  { return min(facts[0].header.image_count, MAX_IMAGE_SLOTS); }

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
uint rows_region(uint slot) { return slot * ROWS_ARENA_BYTES; }

/// One byte of the strings buffer, or 0 past the end of what the CPU wrote.
uint utf8_byte(uint offset)
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
uint answer_byte_word(uint offset)
{
    uint words;
    uint stride;
    answers.GetDimensions(words, stride);
    uint w = offset >> 2;
    return w < words ? answers[w] : 0u;
}

/// One byte of the answers arena, or 0 past the live bytes.
uint answer_byte(uint offset)
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
uint rows_word(uint slot, uint byte_offset)
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
uint rows_byte(uint slot, uint offset)
{
    return (rows_word(slot, offset & ~3u) >> ((offset & 3u) * 8u)) & 0xffu;
}

/// One byte of whichever of the three buffers `source` names.
uint src_byte(uint source, uint offset)
{
    if (source == SRC_STRINGS)  { return utf8_byte(offset); }
    if (source == SRC_ANSWERS)  { return answer_byte(offset); }
    return rows_byte(source, offset);
}

// ---------------------------------------------------------------------------
// The rows arena, through the wire's own structs (docs/abi.md, *The arenas*, *Rows*)
// ---------------------------------------------------------------------------

RowsHeader rows_header(uint slot)
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
Row row_none()
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
uint row_body_off(Row row) { return row.body & 0xffffu; }
uint row_body_len(Row row) { return row.body >> 16; }

/// Row `index` of slot `slot`'s region: the twelve words at
/// `16 + index * 48`, which is where the frame's writer put them.
Row row_at(uint slot, uint index)
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
uint rows_blob(uint slot)
{
    return 16u + min(rows_word(slot, 4u), MAX_ROWS) * 48u;
}

/// The row an item's live value comes from — an applet's level, a slider's
/// position (docs/abi.md, *The arenas*). `Item.fact_row` is `slot * MAX_ROWS +
/// index`; `bar_model` stays binding-free, so the lookup is here and the
/// arithmetic is there.
Row item_row(Item it)
{
    uint at = item_fact_row(it);
    return at == NO_INDEX ? row_none() : row_at(at / MAX_ROWS, at % MAX_ROWS);
}

float applet_level(Item it) { return item_level(it, item_row(it)); }
float slider_position(Item it) { return item_position(it, item_row(it)); }

// ---------------------------------------------------------------------------
// Joining by name: this file's own hash, and the two directories
// ---------------------------------------------------------------------------

/// FNV-1a 32 over a run of one of the three buffers, with the length folded
/// in — the caustic's **private** naming (docs/abi.md, *The arenas*). No number
/// computed here crosses the buffer.
uint hash_bytes(uint source, uint off, uint len)
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
uint hash_region_words(uint slot, uint off, uint len)
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
bool bytes_eq(uint a_src, uint a_off, uint a_len, uint b_src, uint b_off, uint b_len)
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
uint image_by_name(uint source, uint off, uint len)
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
float2 block_size(uint b)
{
    return b < live_blocks() ? private[0].blocks[b].size : float2(0.0, 0.0);
}

/// An item that is not there. Slang has no null struct, and every read of the
/// item table has to survive a stale index, such as one read from a ring written a frame
/// earlier.
Item item_none()
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

Item item_of(uint i)
{
    return i < live_items() ? private[0].items[i] : item_none();
}

/// Whether this item shows its text block where it is laid out. A dock entry
/// and a tray item carry a title block too, but it belongs to a preview popup
/// or a tooltip, never to the strip itself.
///
/// An applet's and a slider's labels are *optional* — a sidecar may publish a
/// volume percentage beside its icon or only the icon — so for those two the
/// question is about the item, not the kind, which is why this takes one.
bool item_has_label(Item it)
{
    if (it.kind == ITEM_APPLET || it.kind == ITEM_SLIDER)
    {
        return it.block != NO_INDEX;
    }
    return it.kind == ITEM_CLOCK || it.kind == ITEM_MENU_ROW || it.kind == ITEM_CAL_CELL
        || it.kind == ITEM_CHART_LINE;
}

/// Which kinds are worth a hover chip and a press dip. **A chart is not a
/// button**: it takes no click, so it gets neither, and the
/// wake — which is painted on the slab, not per item — goes under the chart's
/// own ground. A control that does nothing when pressed is worse than
/// one that never offered.
///
/// The clock is one while it opens the calendar; the
/// calendar's own cells never are.
bool item_is_button(uint kind)
{
    return kind == ITEM_DOCK || kind == ITEM_OVERVIEW
        || kind == ITEM_MENU_ROW || kind == ITEM_APPLET
        || (kind == ITEM_CLOCK && CLOCK_SHOW_CALENDAR);
}

/// Which kinds are rows of the open menu: a menu row, and a slider a sidecar
/// published under one (docs/abi.md, *Rows*). The popup's whole loop keys on
/// this, so a slider descends, measures and hit-tests exactly as a row does.
bool menu_row_kind(uint kind)
{
    return kind == ITEM_MENU_ROW || kind == ITEM_SLIDER;
}

/// One menu row's height: a separator's hairline row, a slider's taller row
/// with its track band, or a plain row.
float menu_row_height(Item it)
{
    float s = scale_px();
    if (it.kind == ITEM_SLIDER)
    {
        return MENU_SLIDER_H * s;
    }
    return ((it.flags & IFLAG_SEPARATOR) != 0u ? MENU_SEP_H : MENU_ROW_H) * s;
}

float item_icon_w(Item it)
{
    return it.icon_block == NO_INDEX ? 0.0 : block_size(it.icon_block).x;
}

float item_label_w(Item it)
{
    return item_has_label(it) ? block_size(it.block).x : 0.0;
}

/// An item's *second* text block, when its kind draws one and the tunables say
/// to: the clock's date, composed and shaped beside its time. NO_INDEX otherwise,
/// which is what makes CLOCK_SHOW_DATE a width change as well as a paint one.
uint item_aux_block(Item it)
{
    return (it.kind == ITEM_CLOCK && CLOCK_SHOW_DATE) ? it.aux_block : NO_INDEX;
}

float item_aux_w(Item it)
{
    uint block = item_aux_block(it);
    return block == NO_INDEX ? 0.0 : block_size(block).x;
}

/// Icon, aux block and label, laid end to end with a gap between each pair
/// that exists. The layout sizes a chip around this and the draw pass places
/// the same blocks by the same arithmetic, which is why content never drifts
/// out of its own carve.
float item_content_w(Item it)
{
    float gap = gap_px();
    float w = item_icon_w(it);
    float aux = item_aux_w(it);
    if (aux > 0.0)
    {
        w += (w > 0.0 ? gap : 0.0) + aux;
    }
    float label = item_label_w(it);
    if (label > 0.0)
    {
        w += (w > 0.0 ? gap : 0.0) + label;
    }
    return w;
}

/// The block whose extent this item's rect is sized around — its label if it
/// shows one, else its icon. Kept as `Element.block` so the `layout:` witness
/// this file pushes can say what an element carries.
uint item_primary_block(Item it)
{
    return item_has_label(it) ? it.block : it.icon_block;
}

/// One item's width on the bar. A button-shaped item is never narrower than a
/// square chip, so an entry whose icon failed to load is still a target the
/// same size as its neighbours.
float bar_item_w(Item it)
{
    // A chart carries no blocks at all: its width is its height times the
    // aspect, and it has no chip to pad.
    if (it.kind == ITEM_CHART)
    {
        return chart_width();
    }
    float inner = item_content_w(it);
    if (!item_has_label(it))
    {
        inner = max(inner, icon_px());
    }
    return inner + 2.0 * CHIP_PAD * scale_px();
}

/// The index of the item carrying `id` this frame, or NO_INDEX.
///
/// A linear scan, called a handful of times per frame over at most MAX_ITEMS
/// (256) — the same order of work as the layout loops either side of it, and
/// nothing like the cost of being wrong about which row a click landed on.
uint index_of_id(Id id)
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
Id menu_level_id()
{
    return private[0].menu_level;
}

/// The item index whose children the popup is showing. NO_INDEX means either
/// no level at all or — the case that closes the menu, in `step_menu` — an
/// item that has gone from the table.
uint menu_level()
{
    return index_of_id(menu_level_id());
}

/// Hold `item`'s **id** as the level. Takes an index because every caller has
/// one (a click's element, a row's `parent`); the index is resolved here, once,
/// while it is still this frame's.
void set_menu_level(uint item)
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
Id item_id_of_element(uint elem)
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
uint element_item(uint elem)
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
void set_pressed_id(Id id)
{
    private[0].pressed_id = id;
}

Id pressed_id()
{
    return private[0].pressed_id;
}

/// Forget everything the popup was: called when it closes for any reason.
void reset_popup_scratch()
{
    private[0].dwell_secs = 0.0;
    private[0].dwell_candidate = -1.0;
    private[0].preview_owner = -1.0;
    private[0].menu_level = id_zero();
    private[0].popup_slot = NO_INDEX;
    private[0].popup_row = 0u;
    private[0].popup_sent = float2(0.0, 0.0);
    private[0].menu_scroll = 0.0;
    private[0].menu_scroll_max = 0.0;
    private[0].menu_clip = float2(0.0, 0.0);
}

/// Decode one of the two index-as-float slots: negative means "nothing".
uint scratch_index(float v)
{
    return v >= 0.0 ? uint(v + 0.5) : NO_INDEX;
}

// ===========================================================================
// Sliders. One definition of where the track is, read by the layout, by the
// drag arithmetic in `apply_command` and by the paint — which is what makes
// grabbing the knob not jump.
// ===========================================================================

/// The track inside a slider row's rect: the row inset by the menu gutter on
/// each side (so it lines up with the label column above it), across the
/// bottom band.
float4 slider_track(float4 rect)
{
    float s = scale_px();
    float g = MENU_GUTTER * s;
    float band = SLIDER_BAND * s;
    float h = SLIDER_TRACK_H * s;
    float x = rect.x + g;
    float w = max(rect.z - 2.0 * g, 1.0);
    float y = rect.y + rect.w - band + floor((band - h) * 0.5);
    return float4(x, y, w, h);
}

/// The knob's travel: (origin, span), the track inset by half a knob at each
/// end so the knob never overhangs it.
float2 slider_travel(float4 rect)
{
    float knob = SLIDER_KNOB * scale_px();
    float4 t = slider_track(rect);
    return float2(t.x + knob * 0.5, max(t.z - knob, 1.0));
}

/// Pointer x (popup px) -> value 0..1. The exact inverse of the knob's
/// placement in `draw_slider`.
float slider_value_at(float4 rect, float x)
{
    float2 travel = slider_travel(rect);
    return saturate((x - travel.x) / travel.y);
}

/// A press or a motion while pressed, on a slider row: push the value the
/// pointer is at. Called from CMD_POINTER_DOWN (a click on the track is a
/// seek) and from CMD_POINTER_MOTION (the drag), and the release goes through
/// `click` like every other release.
void drag_slider(uint target, float2 pos)
{
    uint p = private[0].pressed_element;
    if (p >= live_elements() || private[0].elements[p].target != target)
    {
        return;
    }
    Item it = item_of(element_item(p));
    if (it.kind != ITEM_SLIDER || (it.flags & IFLAG_DISABLED) != 0u)
    {
        return;
    }
    float value = slider_value_at(private[0].elements[p].rect, pos.x);
    // In the **sidecar's own units**: the row carries its range and
    // this is the one place that has both it and the position, so the embassy
    // copies a number rather than de-normalising one (docs/abi.md, *The action ring*).
    Row row = item_row(it);
    act_sidecar(it, ACTION_SET_VALUE, 0u, pos, lerp(row.min, row.max, value));
    // Remember where the pointer put it, so the paint draws the knob there
    // now rather than where the sidecar's last frame left it.
    private[0].drag_element = float(p + 1u);
    private[0].drag_value = value;
}

/// The window an item names, as the two words `ACT_ACTIVATE_WINDOW` and
/// `ACT_CLOSE_WINDOW` carry. An item that is not a window carries zero, which
/// is a window id the compositor never allocates.
void act_window(uint kind, Item it)
{
    act2(kind, it.kind == ITEM_WINDOW ? it.arg0 : 0u,
               it.kind == ITEM_WINDOW ? it.arg1 : 0u);
}

/// The app id range an item carries, as `ACT_SPAWN`/`ACT_PIN`/`ACT_UNPIN`
/// carry it: a dock entry's own, or a dock menu row's entry's.
void act_app(uint kind, Item it)
{
    act2(kind, it.arg0, it.arg1);
}

/// One row of a sidecar, with the wire's own `ACTION_*` and the six words an
/// `ActionBody` is made of (docs/abi.md, *The action ring*, *Action*). The embassy divides `pos` by the
/// surface scale and copies; it reads no row and resolves no name.
void act_sidecar(Item it, uint kind, uint button, float2 pos, float value)
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

/// The chart kind's literal for a samples row, and whether that kind has
/// appeared earlier in the frame — which is what puts the row index on the
/// second and later charts of a kind and leaves the first bare.
uint chart_name(uint row, out bool repeat)
{
    repeat = false;
    uint n = min(samples[0].chart_count, MAX_CHARTS);
    if (row >= n)
    {
        return chart_literal(CHART_OTHER);
    }
    uint kind = samples[0].charts[row].kind;
    for (uint i = 0u; i < row; ++i)
    {
        repeat = repeat || samples[0].charts[i].kind == kind;
    }
    return chart_literal(kind);
}

/// What to call the item an element lays out.
NameRef item_name(uint item)
{
    Item it = item_of(item);
    if (it.kind == ITEM_NONE)
    {
        return name_plain(NAME_ITEM);
    }
    uint tag = id_tag(it.id);
    switch (tag)
    {
    case ID_OVERVIEW: return name_plain(NAME_OVERVIEW);
    case ID_CLOCK:    return name_plain(NAME_CLOCK);
    case ID_CAFFEINE: return name_plain(NAME_CAFFEINE);
    // The app id's own bytes, which are the operand words an `ACT_SPAWN`
    // carries too — one range, read twice.
    case ID_ENTRY:    return name_of(NAME_ENTRY, it.arg0, it.arg1, 0u, 2u);
    case ID_WINDOW:   return name_of(NAME_WINDOW, it.arg0, it.arg1, 0u, 2u);
    // `dock-pin`, not `dock-<app id>-pin`: exactly one dock menu can be open,
    // so the verb alone names the row, and it is what `bar/bar-test.sh` clicks.
    case ID_DOCK_ROW:
        return name_of(NAME_DOCK_ROW, dock_verb_literal(item_dock_verb(it)), 0u, 0u, 1u);
    case ID_CHART:
    {
        bool repeat;
        uint hash = chart_name(id_payload(it.id), repeat);
        return repeat ? name_of(NAME_CHART_N, hash, id_payload(it.id), 0u, 2u)
                      : name_of(NAME_CHART, hash, 0u, 0u, 1u);
    }
    case ID_APPLET:
    {
        uint slot = id_aux(it.id);
        if (slot >= MAX_SLOTS)
        {
            return name_plain(NAME_ITEM);
        }
        SlotFact sc = facts[0].slots[slot];
        uint row = id_payload(it.id);
        // The slot's *first* `ROW_ITEM` wears the bare name; every other row
        // carries its own id, which is what the harness clicks by. Which row
        // that is, this file works out in `bar_build` and keeps, so a
        // departure line still names the chip after the sidecar has gone.
        return private[0].slot_first_item[slot] == row
            ? name_of(NAME_APPLET, sc.name_off, sc.name_len, 0u, 2u)
            : name_of(NAME_APPLET_N, sc.name_off, sc.name_len, row, 3u);
    }
    case ID_CALENDAR:
    {
        uint payload = id_payload(it.id);
        switch (id_aux(it.id))
        {
        case CAL_TITLE:
        {
            uint month = payload & 0xffu;
            return (month >= 1u && month <= 12u)
                ? name_of(NAME_CAL_TITLE, MONTHS[month - 1u], payload >> 8, 0u, 2u)
                : name_plain(NAME_ITEM);
        }
        case CAL_WEEKDAY:
            return payload < 7u ? name_of(NAME_CAL_WEEKDAY, WEEKDAYS[payload], 0u, 0u, 1u)
                                : name_plain(NAME_ITEM);
        default:
            return name_of(NAME_CAL_DAY, payload, 0u, 0u, 1u);
        }
    }
    case ID_NUMBERS:
    {
        // The number as the line composes it, read off the samples body now:
        // a `layout:` line is said when the line moves, so it carries the
        // number of that frame.
        uint c = id_payload(it.id);
        ChartRow row = chart_row(c);
        ChartReading r = chart_reading(row.kind, row.ceiling, chart_latest(c, 0u));
        uint name = chart_literal(row.kind);
        if (r.gap)
        {
            return name_of(NAME_NUMBERS_NONE, name, 0u, 0u, 3u);
        }
        return name_of(r.unit == 0u ? NAME_NUMBERS_BARE : NAME_NUMBERS, name,
                       asuint(r.value), r.unit, 3u);
    }
    default: return name_plain(NAME_ITEM);
    }
}

/// What to call an element: one of the four fixed slots, or its item.
NameRef element_name(uint idx, uint item)
{
    switch (idx)
    {
    case ELEM_BAR:     return name_plain(NAME_BAR);
    case ELEM_POPUP:   return name_plain(NAME_POPUP);
    case ELEM_BACK:    return name_plain(NAME_BACK);
    case ELEM_PREVIEW: return name_plain(NAME_PREVIEW);
    default:           return item_name(item);
    }
}

/// `layout: <name> target={n:s} rect={n+1:f},…` — the tail is the same four
/// floats and one target hash for every family.
uint layout_literal(uint slot)
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
uint gone_literal(uint slot)
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
uint hover_literal(uint slot)
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
uint stall_literal(uint slot)
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
Args name_args(NameRef nm)
{
    Args a = args_new();
    if (nm.count > 0u) { arg(a, nm.a0); }
    if (nm.count > 1u) { arg(a, nm.a1); }
    if (nm.count > 2u) { arg(a, nm.a2); }
    return a;
}

/// The `{n:s}` a `target=` reads.
uint target_literal(uint target)
{
    if (target == TARGET_BAR)   { return getStringHash("bar"); }
    if (target == TARGET_POPUP) { return getStringHash("popup"); }
    return getStringHash("none");
}

/// `hover: <name> at=x,y`, once per change of the hovered element.
///
/// Pushed from `bar_sample`, which is where the answer changes, and again at
/// the end of `bar_apply`, which is where a pointer *leave* clears it — the
/// sample pass does not run for a target the pointer is not on.
///
/// The position is **rounded here**, so the line reads `at=724,12`.
void witness_hover()
{
    uint hover = private[0].hover_element;
    if (hover == private[0].said_hover)
    {
        return;
    }
    private[0].said_hover = hover;
    float2 at = float2(round(private[0].pointer.x), round(private[0].pointer.y));
    if (hover >= live_elements())
    {
        Args a = args_new();
        arg2(a, at);
        witness(WITNESS_INFO, getStringHash("hover: none at={0:f},{1:f}"), a);
        return;
    }
    NameRef nm = element_name(hover, private[0].elements[hover].item);
    Args a = name_args(nm);
    arg2(a, at);
    witness(WITNESS_INFO, hover_literal(nm.slot), a);
}
// ===========================================================================
// bar_apply — the per-frame decisions and the layout.
// ===========================================================================

/// A dock entry's IFLAG_*, derived from what exists rather than read off the
/// item: the embassy reports windows and favourites and decides nothing, so
/// whether an entry is running, focused or urgent is this file's question to
/// answer from the table. `windows` comes back as the number of ITEM_WINDOW
/// children, which is what the running marks count.
///
/// PINNED is "the entry has a favourite index"; RUNNING, FOCUSED and URGENT
/// are its children's. One walk of the item table per *placed* dock entry —
/// never per texel, which is why `place` stores the answer in the element and
/// `draw_element` reads it from there.
uint dock_flags(uint entry, out uint windows)
{
    windows = 0u;
    uint flags = item_favourite(item_of(entry)) != NO_INDEX ? IFLAG_PINNED : 0u;
    uint n = live_items();
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != ITEM_WINDOW || it.parent != entry)
        {
            continue;
        }
        windows += 1u;
        flags |= IFLAG_RUNNING | (it.flags & (IFLAG_FOCUSED | IFLAG_URGENT));
    }
    return flags;
}

/// Write one element's layout, preserving its springs. Hover/press flags are
/// re-derived here from the ids the sample pass published, so the hit test, the
/// witness and the tint the draw pass applies always agree.
void place(uint idx, uint target, float4 rect, float radius, uint block, uint item, bool present)
{
    if (idx >= MAX_ELEMENTS)
    {
        return;
    }

    // An element slot recycled for a different item must not inherit the old
    // one's springs: the dock relayouts when a window opens, and without this
    // the entry that slid into the slot would arrive with its hover glow
    // already lit.
    if (private[0].elements[idx].item != item)
    {
        private[0].elements[idx].anim = float4(0.0, 0.0, 0.0, 0.0);
    }

    private[0].elements[idx].rect = rect;
    private[0].elements[idx].target = target;
    private[0].elements[idx].block = block;
    private[0].elements[idx].item = item;
    private[0].elements[idx].item_id = item_of(item).id;
    private[0].elements[idx].radius = max(radius, 0.0);

    // What the CPU does not say about a dock entry (see `dock_flags`).
    // Counted once per placed element, which is a handful of walks per frame;
    // the draw pass runs per texel and must never walk the item table.
    uint derived = 0u;
    uint derived_a = 0u;
    Item placed_item = item_of(item);
    if (placed_item.kind == ITEM_DOCK)
    {
        derived = dock_flags(item, derived_a);
    }
    private[0].elements[idx].derived = derived;
    private[0].elements[idx].derived_a = derived_a;

    uint flags = present ? EFLAG_PRESENT : 0u;
    if (present && idx == private[0].hover_element)
    {
        flags |= EFLAG_HOVERED;
    }
    if (present && idx == private[0].pressed_element)
    {
        flags |= EFLAG_PRESSED;
    }
    private[0].elements[idx].flags = flags;

    // One `layout:` line per element whose `(id, target, rect)` moved, out of
    // the pass that moved it (docs/abi.md, *The witness ring*).
    //
    // **By id**, not by item index. An unpin moves a running app from the
    // pinned run to the running tail, which swaps which app every slot after
    // it holds while leaving both the rects and the item *indices* exactly as
    // they were — so an index-keyed witness would log nothing, and
    // `layout: <app A>` would go on naming a rect that is now app B's.
    if (!present)
    {
        return; // the departure walk at the end of `layout` says so, once
    }
    Id id = placed_item.kind == ITEM_NONE ? id_zero() : placed_item.id;
    bool moved = private[0].elements[idx].said_present == 0u
                 || !private[0].elements[idx].said_id.eq(id)
                 || private[0].elements[idx].said_target != target
                 || any(private[0].elements[idx].said_rect != rect);
    if (!moved)
    {
        return;
    }
    NameRef nm = element_name(idx, item);
    Args a = name_args(nm);
    arg(a, target_literal(target));
    arg4(a, rect);
    witness(WITNESS_DEBUG, layout_literal(nm.slot), a);

    private[0].elements[idx].said_present = 1u;
    private[0].elements[idx].said_id = id;
    private[0].elements[idx].said_target = target;
    private[0].elements[idx].said_rect = rect;
    private[0].elements[idx].said_slot = nm.slot;
    private[0].elements[idx].said_a0 = nm.a0;
    private[0].elements[idx].said_a1 = nm.a1;
    private[0].elements[idx].said_a2 = nm.a2;
}

/// The `layout: <name> target=none rect=gone` line, one per element the
/// previous frame used and this one does not.
///
/// The name is the one the
/// element was last witnessed under, kept whole in the element — an app id's
/// bytes are no longer in the facts by the time its dock entry leaves.
void witness_departures()
{
    for (uint i = 0u; i < MAX_ELEMENTS; ++i)
    {
        // An element not laid out this frame says its row's body lines again
        // the next time it is: a new popup is news.
        if ((private[0].elements[i].flags & EFLAG_PRESENT) == 0u)
        {
            private[0].elements[i].said_body = 0u;
        }
        if (private[0].elements[i].said_present == 0u)
        {
            continue;
        }
        if ((private[0].elements[i].flags & EFLAG_PRESENT) != 0u)
        {
            continue;
        }
        NameRef nm = name_of(private[0].elements[i].said_slot,
                             private[0].elements[i].said_a0,
                             private[0].elements[i].said_a1,
                             private[0].elements[i].said_a2,
                             0u);
        // `count` is not kept: the gone literal reads the same leading words
        // the layout literal did, so all three are pushed and the format takes
        // what it names.
        Args a = args_new();
        arg(a, nm.a0);
        arg(a, nm.a1);
        arg(a, nm.a2);
        witness(WITNESS_DEBUG, gone_literal(nm.slot), a);
        private[0].elements[i].said_present = 0u;
    }
}

/// Whether `p` is outside the band a scrolled menu's rows are drawn in
/// (`menu_clip`). Only a popup element that lays out an item — a menu
/// row — is ever cut; the back row, the preview and its title carry none. The
/// visibility carve, the draw and the hit test all ask, so a half row at
/// either end of the band is neither drawn nor clicked past it.
bool element_clipped(Element e, float2 p)
{
    float2 band = private[0].menu_clip;
    if (e.target != TARGET_POPUP || e.item >= MAX_ITEMS || band.y <= band.x)
    {
        return false;
    }
    return p.y < band.x || p.y >= band.y;
}

/// Is `e` drawn into `target` this frame?
bool element_live(Element e, uint target)
{
    return e.target == target && (e.flags & EFLAG_PRESENT) != 0u;
}

/// The element that owns `p` in `target`, or NO_INDEX. Reverse index order, so
/// the same "later index wins" rule the visibility pass uses decides it.
uint element_at(uint target, float2 p)
{
    uint n = live_elements();
    for (uint k = n; k > 0u; --k)
    {
        uint i = k - 1u;
        Element e = private[0].elements[i];
        if (element_live(e, target) && rect_near(e.rect, p, 0.0) && element_sdf(e, p) <= 0.0
            && !element_clipped(e, p))
        {
            return i;
        }
    }
    return NO_INDEX;
}

/// What a press at `pos` in `target` lands on.
///
/// The visibility buffer is the answer whenever it has one: `hover_element` is a
/// texel fetch from the carve, corners and all, and confirming that the press
/// is still inside that element costs one sdf. It has no answer for a press
/// that arrives in the same frame as the pointer's *first* motion into a
/// target — the popup's very first click, every time — so the fallback is the
/// same rounded-rect arithmetic the carve is made of. One definition of the
/// shape, two readers; never a lost click.
uint press_target(uint target, float2 pos)
{
    uint h = private[0].hover_element;
    if (h < live_elements())
    {
        Element e = private[0].elements[h];
        if (element_live(e, target) && element_sdf(e, pos) <= 0.0 && !element_clipped(e, pos))
        {
            return h;
        }
    }
    return element_at(target, pos);
}

/// Whether window `a` is more recent than window `b`: a greater focus stamp,
/// or the same stamp and a lower registry id.
///
/// The tie is not a formality. Every window of an app that has never been
/// focused carries stamp 0, so a freshly started app's three windows are all
/// tied — and the registry allocates their ids in the order they appeared,
/// which makes the oldest the tie's winner.
bool window_newer(Item a, Item b)
{
    uint sa = item_focus_stamp(a);
    uint sb = item_focus_stamp(b);
    if (sa != sb)
    {
        return sa > sb;
    }
    if (a.id.hi != b.id.hi)
    {
        return a.id.hi < b.id.hi;
    }
    return a.id.lo < b.id.lo;
}

/// The most recently focused window under a dock entry, or NO_INDEX.
///
/// A max over `item_focus_stamp`: the table's order means nothing, so the CPU
/// never has to sort an entry's children.
uint first_window(uint entry)
{
    uint n = live_items();
    uint best = NO_INDEX;
    for (uint i = 0u; i < n; ++i)
    {
        if (private[0].items[i].kind != ITEM_WINDOW || private[0].items[i].parent != entry)
        {
            continue;
        }
        if (best == NO_INDEX || window_newer(private[0].items[i], private[0].items[best]))
        {
            best = i;
        }
    }
    return best;
}

/// Which window a click on a dock entry activates, as an **item index**: the
/// next one after the focused one in recency order — so clicking a focused
/// entry cycles — or the most recent one when nothing under the entry has
/// focus, or NO_INDEX when nothing is running and the embassy has to launch it.
///
/// This is the whole point of the item table: the *caustic* decides, from
/// what exists, and the embassy carries out an id it was handed. The window's id is
/// what `ACT_ACTIVATE_WINDOW` and `ACT_CLOSE_WINDOW` carry (`act_window`), so a
/// dock menu row's Close and the entry's own left click name the same window
/// by construction.
uint pick_window_index(uint entry)
{
    uint n = live_items();
    uint first = NO_INDEX;
    uint focused = NO_INDEX;
    uint after = NO_INDEX;

    // One pass, three maxima over `window_newer`: the newest child, the newest
    // focused child, and the newest child *older* than that one. Reading the
    // recency off a stamp rather than off the table's order is what lets the
    // table be in any order.
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != ITEM_WINDOW || it.parent != entry)
        {
            continue;
        }
        if (first == NO_INDEX || window_newer(it, private[0].items[first]))
        {
            first = i;
        }
        if ((it.flags & IFLAG_FOCUSED) != 0u)
        {
            if (focused == NO_INDEX || window_newer(it, private[0].items[focused]))
            {
                focused = i;
            }
        }
    }
    if (focused != NO_INDEX)
    {
        for (uint i = 0u; i < n; ++i)
        {
            Item it = private[0].items[i];
            if (it.kind != ITEM_WINDOW || it.parent != entry || i == focused)
            {
                continue;
            }
            if (!window_newer(it, private[0].items[focused])
                && (after == NO_INDEX || window_newer(it, private[0].items[after])))
            {
                after = i;
            }
        }
    }

    uint pick = first;
    if (focused != NO_INDEX)
    {
        // Past the last one wraps to the first, which for a single window is
        // a re-focus and costs nothing.
        pick = after != NO_INDEX ? after : first;
    }
    return pick;
}

/// The same choice as an `Id` for the action ring, or `fallback` — the entry's
/// own id — when nothing is running under it.
Id pick_window(uint entry, Id fallback)
{
    uint pick = pick_window_index(entry);
    if (pick == NO_INDEX)
    {
        return fallback;
    }
    return private[0].items[pick].id;
}

/// Is a popup asked for or up?
bool popup_live()
{
    uint st = private[0].popup_state;
    return st == POPUP_ASKED || st == POPUP_UP;
}

/// A toast that was on screen is ending: tell its sidecar which rows it
/// showed, one `ACTION_TOAST_SHOWN` per row (an `ActionBody` names one row),
/// so the sidecar marks exactly those shown.
/// Only a toast the compositor **mapped** showed anything — one refused, or
/// replaced before it mapped, says nothing, and the sidecar's own clock is
/// what retires its rows. Called from every route that ends a toast while it
/// is still `POPUP_UP`: `close_popup` (the chip stopped asking, its rows went,
/// a click), `request_popup` (a menu replacing it) and `CMD_POPUP_DONE` (the
/// compositor dismissing it); the state moving on is what keeps it to once.
void say_toast_shown()
{
    if (private[0].popup_kind != POPUP_TOAST || private[0].popup_state != POPUP_UP)
    {
        return;
    }
    uint slot = private[0].toast_slot;
    uint count = min(private[0].toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count; ++k)
    {
        Args a = args_new();
        arg(a, slot);
        arg(a, private[0].toast_ids[k]);
        arg(a, ACTION_TOAST_SHOWN);
        arg(a, 0u);              // no button
        arg2(a, float2(0.0, 0.0));
        argf(a, 0.0);
        arg(a, 0u);              // no token
        push_action(ACT_SIDECAR, a);
    }
    witness2(WITNESS_INFO, getStringHash("toast: shown slot={0} rows={1}"), slot, count);
}

/// Ask the embassy for a popup, as one message (docs/abi.md, *The action ring*).
///
/// `kind` is what decides whether the popup takes a grab: a menu does, a
/// preview must not, or the pointer could never reach the next dock entry.
/// `slot` and `row` are the sidecar and the row the open belongs to, or
/// NO_INDEX for a dock entry's own menu, which has nobody at the other end;
/// the embassy sends that sidecar an `ACTION_MENU_OPEN` and remembers who to
/// tell when it closes.
///
/// A request while a popup is up is the replace case — the preview-then-right-
/// click — and the embassy tears the old one down without a `CMD_POPUP_DONE`,
/// because this file has already moved on to the new one.
void request_popup(uint kind, float4 anchor, Id id, float2 want)
{
    // A toast is the one popup a grabbed menu ends without asking: the
    // replace path below tears its surface down, since the bar has one popup.
    if (popup_live() && private[0].popup_kind == POPUP_TOAST && kind != POPUP_TOAST)
    {
        witness1(WITNESS_INFO, getStringHash("toast: closed ({0:s})"),
                 kind == POPUP_MENU ? getStringHash("menu") : getStringHash("replaced"));
        say_toast_shown();
    }
    uint slot = id_tag(id) == ID_APPLET ? id_aux(id) : NO_INDEX;
    uint row = id_tag(id) == ID_APPLET ? id_payload(id) : 0u;
    uint w = uint(max(want.x, 1.0) + 0.5);
    uint h = uint(max(want.y, 1.0) + 0.5);

    private[0].popup_state = POPUP_ASKED;
    private[0].popup_kind = kind;
    private[0].popup_id = id;
    private[0].popup_slot = slot;
    private[0].popup_row = row;
    private[0].popup_anchor = anchor;
    private[0].popup_want = want;
    private[0].popup_sent = float2(float(w), float(h));

    Args a = args_new();
    arg(a, kind);
    arg(a, slot);
    arg(a, row);
    arg4(a, anchor);
    arg(a, w);
    arg(a, h);
    push_action(ACT_POPUP_OPEN, a);
}

/// End the popup: one `ACT_POPUP_CLOSE`, and the `CMD_POPUP_DONE` that comes
/// back is what resets the rest. Idempotent, so every route to "this popup is
/// over" can call it.
void close_popup()
{
    if (!popup_live())
    {
        return;
    }
    say_toast_shown();
    private[0].popup_state = POPUP_ENDING;
    act(ACT_POPUP_CLOSE);
}

// ---------------------------------------------------------------------------
// Toasts: a sidecar's chip that carries `RFLAG_SHOW`
// has its `RFLAG_ATTENTION` rows shown without a click, as a popup without a
// grab. Whether a chip asks, and which rows it would show, is read off the rows
// arena, where the sidecar's frame is verbatim.
//
// **A toast's rows are fixed while it is up.** When a chip first asks, its
// level is set and nothing opens: the toast waits a frame for `bar_build` to
// build those rows, the shaper to shape them and `wrap_bodies` to wrap them,
// and then opens at exactly their size — ids, order, body lines and heights
// taken there and kept in `Private` until it ends. A frame that arrives while it
// is up changes none of that: a new attention row waits for this toast to end
// (`toast: held`), a row the sidecar removed leaves a gap rather than moving
// the rows under the pointer, and the toast ends when none of its own rows is
// still in the frame and still flagged. The size it opened at is the size it
// keeps, so it never moves. And a click on one of its rows holds it as a
// refusal does, so the next frame cannot reopen it with "the rest".
// ---------------------------------------------------------------------------

/// The frame index of slot `slot`'s `ROW_ITEM` whose id is `id`, or NO_INDEX
/// when the frame no longer carries it (or the slot has no frame at all).
uint chip_row(uint slot, uint id)
{
    if (slot >= MAX_SLOTS)
    {
        return NO_INDEX;
    }
    uint n = rows_header(slot).row_count;
    for (uint r = 0u; r < n; ++r)
    {
        uint at = 16u + r * 48u;
        if (rows_word(slot, at + 8u) == ROW_ITEM && rows_word(slot, at) == id)
        {
            return r;
        }
    }
    return NO_INDEX;
}

/// Is frame row `row` one a toast of chip `chip` shows: a direct child of the
/// chip, flagged `RFLAG_ATTENTION`, and a row or a slider — never a separator.
bool toast_row(Row row, uint chip)
{
    return row.parent == chip && (row.flags & RFLAG_ATTENTION) != 0u
        && (row.role == ROW_MENU || row.role == ROW_SLIDER);
}

/// Rows one toast holds: TOAST_MAX_ROWS, within what `Private` keeps.
uint toast_cap()
{
    return min(TOAST_MAX_ROWS, TOAST_ROWS_CAP);
}

/// How many rows a toast of chip row `chip` in `slot` would show, at most
/// `toast_cap()`, in the frame's own order (newest first, as the sidecar
/// ordered them); `newest` is the first one's id, NO_INDEX for none.
uint toast_rows(uint slot, uint chip, out uint newest)
{
    newest = NO_INDEX;
    if (chip == NO_INDEX)
    {
        return 0u;
    }
    uint n = rows_header(slot).row_count;
    uint count = 0u;
    for (uint r = chip + 1u; r < n && count < toast_cap(); ++r)
    {
        Row row = row_at(slot, r);
        if (!toast_row(row, chip))
        {
            continue;
        }
        if (count == 0u)
        {
            newest = row.id;
        }
        count += 1u;
    }
    return count;
}

/// Whether row id `id` is one of the open toast's own rows.
bool toast_holds(uint id)
{
    uint count = min(private[0].toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count; ++k)
    {
        if (private[0].toast_ids[k] == id)
        {
            return true;
        }
    }
    return false;
}

/// The open toast's index of item `it`, or NO_INDEX when it is none of its
/// rows (or no toast is up).
uint toast_index_of(Item it)
{
    if (private[0].popup_kind != POPUP_TOAST || id_tag(it.id) != ID_APPLET
        || id_aux(it.id) != private[0].toast_slot)
    {
        return NO_INDEX;
    }
    uint count = min(private[0].toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count; ++k)
    {
        if (private[0].toast_ids[k] == id_payload(it.id))
        {
            return k;
        }
    }
    return NO_INDEX;
}

/// End the toast, saying why: the chip stopped asking, its frame went, none
/// of its rows is left, or a row in it was clicked. (A menu replacing it says
/// so in `request_popup`; the compositor ending it, in `CMD_POPUP_DONE`.)
void end_toast(uint why)
{
    witness1(WITNESS_INFO, getStringHash("toast: closed ({0:s})"), why);
    close_popup();
}

/// Hold the toast hanging off `id` until its news changes: no toast for that
/// chip until it stops asking or a newer row than the newest it has now is
/// first. A refusal, a dismissal and a click all hold, so the
/// frame that follows cannot reopen what the user just ended. It
/// also lifts once none of the ended toast's rows still asks: the sidecar
/// heard `ACTION_TOAST_SHOWN`, and what asks now is what it never showed.
void hold_toast(Id id)
{
    uint slot = id_aux(id);
    uint chip = chip_row(slot, id_payload(id));
    uint newest;
    toast_rows(slot, chip, newest);
    private[0].toast_hold_slot = slot;
    private[0].toast_hold_chip = id_payload(id);
    private[0].toast_hold_newest = newest;
}

/// The bar element a chip is laid out as — present, on the bar — or NO_INDEX.
uint bar_element_of(uint item)
{
    uint n = live_elements();
    for (uint e = ELEM_FIRST_FREE; e < n; ++e)
    {
        Element el = private[0].elements[e];
        if (el.item == item && el.target == TARGET_BAR && (el.flags & EFLAG_PRESENT) != 0u)
        {
            return e;
        }
    }
    return NO_INDEX;
}

/// `toast: open slot={0} rows={1} ids=…`: the ids the toast fixed on, in the
/// order it draws them. One literal per count, because a format is a
/// compile-time string.
void witness_toast_open(uint slot, uint count)
{
    Args a = args_new();
    arg(a, slot);
    arg(a, count);
    for (uint k = 0u; k < count && k < TOAST_ROWS_CAP; ++k)
    {
        arg(a, private[0].toast_ids[k]);
    }
    uint head;
    switch (count)
    {
    case 1u: head = getStringHash("toast: open slot={0} rows={1} ids={2}"); break;
    case 2u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3}"); break;
    case 3u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4}"); break;
    case 4u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5}"); break;
    case 5u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6}"); break;
    case 6u:
        head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6},{7}");
        break;
    case 7u:
        head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6},{7},{8}");
        break;
    default:
        head = getStringHash(
            "toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6},{7},{8},{9}");
        break;
    }
    witness(WITNESS_INFO, head, a);
}

/// A toast asked for and not open yet: open it once its rows are built,
/// shaped and wrapped, at exactly their size. True when this frame is
/// accounted for — opened, or still waiting; false when the chip that asked
/// has stopped (or another popup took the level), and the caller looks again.
bool step_toast_pending(bool rebuild)
{
    uint slot = private[0].toast_slot;
    uint chip_id = private[0].toast_chip;
    uint chip = chip_row(slot, chip_id);
    Id id = id_tagged(ID_APPLET, slot, chip_id);
    bool held = private[0].toast_hold_slot == slot && private[0].toast_hold_chip == chip_id;
    bool asking = chip != NO_INDEX && (row_at(slot, chip).flags & RFLAG_SHOW) != 0u && !held;
    if (!asking || !menu_level_id().eq(id))
    {
        private[0].toast_pending = 0u;
        return false;
    }
    // Built for this level, from the arena as it is now: a frame that arrived
    // this frame with a new shape is built next frame, and the rows are taken
    // from that build.
    uint level = index_of_id(id);
    uint elem = level == NO_INDEX ? NO_INDEX : bar_element_of(level);
    if (elem == NO_INDEX || rebuild || !private[0].built_for.eq(id))
    {
        return true;
    }

    float s = scale_px();
    uint n = rows_header(slot).row_count;
    uint count = 0u;
    float rows_h = 0.0;
    for (uint r = chip + 1u; r < n && count < toast_cap(); ++r)
    {
        Row row = row_at(slot, r);
        if (!toast_row(row, chip))
        {
            continue;
        }
        uint ri = index_of_id(id_tagged(ID_APPLET, slot, row.id));
        if (ri == NO_INDEX || private[0].items[ri].parent != level)
        {
            return true; // not built yet: the next frame's build has it
        }
        Item it = private[0].items[ri];
        uint lines = min(item_body_line_count(it), min(TOAST_BODY_LINES, MAX_BODY_LINES));
        float h = menu_row_height(it);
        if (lines > 0u)
        {
            h += (float(lines) * MENU_BODY_LINE_H + MENU_BODY_PAD) * s;
        }
        private[0].toast_ids[count] = row.id;
        private[0].toast_lines[count] = lines;
        private[0].toast_h[count] = h;
        rows_h += h;
        count += 1u;
    }
    if (count == 0u)
    {
        private[0].toast_pending = 0u;
        return false; // nothing to show is not a toast
    }

    // As wide as a menu may be — every toast the same width, as a
    // notification's toast is anywhere else, and the width a body wraps to —
    // and as tall as its rows, within the output. Nothing it shows can change
    // either, so the popup never moves.
    float ppad = POPUP_PAD * s;
    float want_h = rows_h + 2.0 * ppad;
    float output_h = float(inputs.output_size.y);
    if (output_h > 0.0)
    {
        float room = output_h - float(inputs.bar_size.y) - POPUP_MARGIN * s;
        want_h = min(want_h, max(room, 2.0 * ppad + MENU_ROW_H * s));
    }
    float2 size = float2(floor(MENU_MAX_W * s + 0.5), floor(want_h + 0.5));
    private[0].toast_count = count;
    private[0].toast_size = size;
    private[0].toast_pending = 0u;
    private[0].toast_said_held = NO_INDEX;
    request_popup(POPUP_TOAST, private[0].elements[elem].rect, id, size);
    witness_toast_open(slot, count);
    return true;
}

/// The toast's whole life, once a frame, after the layout (so the chip's rect
/// is this frame's).
///
/// **Up**: end it when its chip no longer carries `RFLAG_SHOW`, when the chip
/// or its whole frame is gone, or when none of its own rows is still in the
/// frame and flagged; say `toast: held` once for each row that arrives while it
/// is up. **Pending**: open it when its rows are built. **Idle**: for the first
/// chip, in slot order, that asks and has something to show, set the level and
/// wait. Anything else up — a menu, a preview, a popup still ending — and the
/// bit is ignored: a toast never replaces a popup, only a menu replaces a
/// toast.
void step_toast(bool rebuild)
{
    uint newest;

    if (popup_live() && private[0].popup_kind == POPUP_TOAST)
    {
        Id id = private[0].popup_id;
        uint slot = id_aux(id);
        uint chip = chip_row(slot, id_payload(id));
        if (chip == NO_INDEX)
        {
            end_toast(getStringHash("gone"));
            return;
        }
        if ((row_at(slot, chip).flags & RFLAG_SHOW) == 0u)
        {
            end_toast(getStringHash("flag"));
            return;
        }
        uint live = 0u;
        uint fresh = NO_INDEX;
        uint n = rows_header(slot).row_count;
        for (uint r = chip + 1u; r < n; ++r)
        {
            Row row = row_at(slot, r);
            if (!toast_row(row, chip))
            {
                continue;
            }
            if (toast_holds(row.id))
            {
                live += 1u;
            }
            else if (fresh == NO_INDEX)
            {
                fresh = row.id;
            }
        }
        if (live == 0u)
        {
            end_toast(getStringHash("empty"));
            return;
        }
        if (fresh != NO_INDEX && fresh != private[0].toast_said_held)
        {
            private[0].toast_said_held = fresh;
            witness1(WITNESS_INFO, getStringHash("toast: held (new row while up) row={0}"),
                     fresh);
        }
        return;
    }
    if (private[0].popup_state != POPUP_IDLE)
    {
        // A menu or a preview took the popup (and with a menu, the level).
        private[0].toast_pending = 0u;
        return;
    }

    // The hold lifts when its chip stops asking, when something newer is
    // first, or when none of the ended toast's own rows is still asking:
    // the sidecar has heard `ACTION_TOAST_SHOWN` and dropped them, so what
    // still asks is news the toast never showed — a row that arrived behind
    // it — and it toasts now, alone. Until that frame the toast's rows still
    // ask, and the hold is what keeps them from reopening with it. The ended
    // toast's ids are still `toast_ids`: only the next toast's open rewrites
    // them.
    uint held = private[0].toast_hold_slot;
    if (held != NO_INDEX)
    {
        uint chip = chip_row(held, private[0].toast_hold_chip);
        bool asking = chip != NO_INDEX && (row_at(held, chip).flags & RFLAG_SHOW) != 0u;
        toast_rows(held, chip, newest);
        bool kept = false;
        if (asking)
        {
            uint n = rows_header(held).row_count;
            for (uint r = chip + 1u; r < n && !kept; ++r)
            {
                Row row = row_at(held, r);
                kept = toast_row(row, chip) && toast_holds(row.id);
            }
        }
        if (!asking || newest != private[0].toast_hold_newest || !kept)
        {
            private[0].toast_hold_slot = NO_INDEX;
        }
    }

    if (private[0].toast_pending != 0u && step_toast_pending(rebuild))
    {
        return;
    }

    for (uint slot = 0u; slot < MAX_SLOTS; ++slot)
    {
        uint n = rows_header(slot).row_count;
        for (uint r = 0u; r < n; ++r)
        {
            // Two words a row, not twelve: this scan runs every frame over
            // every slot, and almost every row is not a chip that asks.
            uint at = 16u + r * 48u;
            if (rows_word(slot, at + 8u) != ROW_ITEM
                || (rows_word(slot, at + 12u) & RFLAG_SHOW) == 0u)
            {
                continue;
            }
            Row row = row_at(slot, r);
            if (private[0].toast_hold_slot == slot && private[0].toast_hold_chip == row.id)
            {
                continue;
            }
            if (toast_rows(slot, r, newest) == 0u)
            {
                continue; // nothing to show is not a toast
            }
            // The chip has to be on the bar to anchor anything: a frame that
            // has just arrived is built and laid out a frame from now, and the
            // bit is still there then.
            Id id = id_tagged(ID_APPLET, slot, row.id);
            uint item = index_of_id(id);
            if (item == NO_INDEX || bar_element_of(item) == NO_INDEX)
            {
                continue;
            }
            // The level is the chip, so `bar_build` publishes its rows; the
            // toast opens once they are built (`step_toast_pending`) — this
            // frame, when they already are.
            set_menu_level(item);
            private[0].toast_pending = 1u;
            private[0].toast_slot = slot;
            private[0].toast_chip = row.id;
            step_toast_pending(rebuild);
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// The calendar: the clock's click opens this month as a
// menu-kind popup — a title, the weekday initials, the days in a grid, today
// marked. Its level is the clock's own id, so the popup's whole life (the
// grab, a click elsewhere, a second click on the clock) is a menu's.
//
// Everything below is arithmetic on two words the embassy publishes:
// `inputs.clock_date` (today, and the month) and `inputs.clock_month` (the
// year, the 1st's weekday and the month's length). The cells are built in
// `bar_build` (`build_calendar`) and placed in `layout_calendar`.
// ---------------------------------------------------------------------------

/// The column weekday `weekday` (0 = Sunday) sits in.
uint calendar_column(uint weekday)
{
    uint start = CAL_WEEK_STARTS_MONDAY ? 1u : 0u;
    return (weekday + 7u - start) % 7u;
}

/// How many days this month has; 0 when the embassy has no local time.
uint calendar_days()
{
    return min(inputs.clock_month & 0xffu, 31u);
}

/// The grid position of day `day` (1-based), counted row-major from the first
/// week's first column.
uint calendar_cell(uint day)
{
    uint first = min((inputs.clock_month >> 8) & 0xffu, 6u);
    return calendar_column(first) + max(day, 1u) - 1u;
}

/// How many weeks this month spans: four to six.
uint calendar_rows()
{
    uint days = calendar_days();
    return days == 0u ? 0u : calendar_cell(days) / 7u + 1u;
}

/// The calendar popup's size: the grid, or the title if that is wider, inside
/// the popup's pad; a title row, the weekday row and the weeks. Known from the
/// facts alone, so the click asks for the right size before a cell is built.
float2 calendar_want(float ppad, float title_w)
{
    float s = scale_px();
    float cell = CAL_CELL_PX * s;
    float gap = CAL_GAP * s;
    uint rows = calendar_rows();
    float grid_w = 7.0 * cell + 6.0 * gap;
    float grid_h = float(rows + 1u) * cell + float(rows) * gap;
    return float2(max(grid_w, title_w) + 2.0 * ppad, 2.0 * ppad + MENU_ROW_H * s + grid_h);
}

/// One `calendar: open` line, when the click asks for the popup: what the
/// facts say the grid is, which is what a wrong grid would be checked against.
void witness_calendar()
{
    uint month = (inputs.clock_date >> 8) & 0xffu;
    uint first = min((inputs.clock_month >> 8) & 0xffu, 6u);
    Args a = args_new();
    arg(a, (month >= 1u && month <= 12u) ? MONTHS[month - 1u] : getStringHash("?"));
    arg(a, inputs.clock_month >> 16);
    arg(a, calendar_days());
    arg(a, WEEKDAYS[first]);
    arg(a, calendar_rows());
    witness(WITNESS_INFO,
            getStringHash("calendar: open {0:s} {1} ({2} days, the 1st a {3:s}, {4} weeks)"), a);
}

/// A hover preview that is up when a click opens a menu instead: end its
/// capture session and forget the dwell here — the embassy swaps the surface
/// under the `ACT_POPUP_OPEN` that names something else — because nothing
/// downstream would: once the kind is a menu, `step_dwell` no longer sees a
/// preview to tear down.
void end_preview_for_menu()
{
    if (popup_live() && private[0].popup_kind == POPUP_PREVIEW)
    {
        if (private[0].preview_owner >= 0.0)
        {
            act(ACT_PREVIEW_END);
        }
        private[0].dwell_secs = 0.0;
        private[0].dwell_candidate = -1.0;
        private[0].preview_owner = -1.0;
    }
}

// ---------------------------------------------------------------------------
// The chart numbers: the strip's hover asks for a popup
// without a grab, its click for a menu, and both hold one line per chart.
//
// The level is the **chart** the popup is about — the one under the pointer,
// or the one clicked — held by id like any menu level. `bar_build` builds the
// lines while the level is a chart (`build_numbers`), the layout lays them
// out while it is (`layout_numbers`), and the marked line is read off the
// level where it is drawn, so moving the pointer from one chart to the next
// moves the mark. Nothing about this is the embassy's: the hover is the
// pointer this file already keeps, the popup is `ACT_POPUP_OPEN` with the kind
// that says whether to grab, and a new samples tick under an open popup is an
// `ACT_REBUILD` (in `bar_apply`), which the embassy answers as it answers a
// clock tick.
// ---------------------------------------------------------------------------

/// Whether the level is a chart, which is what makes a popup the numbers.
bool numbers_level()
{
    return id_tag(menu_level_id()) == ID_CHART;
}

/// Is a numbers popup asked for or up?
bool numbers_live()
{
    return popup_live() && numbers_level()
        && (private[0].popup_kind == POPUP_PREVIEW || private[0].popup_kind == POPUP_MENU);
}

/// The strip: the union of every chart laid out on the bar this frame, which
/// is the popup's anchor and what "on the strip" means — the gaps between two
/// charts included, so crossing one does not end the popup. False when no
/// chart is laid out.
bool chart_strip(out float4 strip)
{
    strip = float4(0.0, 0.0, 0.0, 0.0);
    float2 lo = float2(0.0, 0.0);
    float2 hi = float2(0.0, 0.0);
    bool any = false;
    uint n = live_elements();
    for (uint i = ELEM_FIRST_FREE; i < n; ++i)
    {
        Element e = private[0].elements[i];
        if (!element_live(e, TARGET_BAR) || item_of(e.item).kind != ITEM_CHART)
        {
            continue;
        }
        float2 a = e.rect.xy;
        float2 b = e.rect.xy + e.rect.zw;
        lo = any ? min(lo, a) : a;
        hi = any ? max(hi, b) : b;
        any = true;
    }
    if (any)
    {
        strip = float4(lo, hi - lo);
    }
    return any;
}

/// The chart item under the pointer on the bar, or NO_INDEX — read from the
/// rects, not the carve, so it answers in the frame the pointer moved.
uint chart_under_pointer()
{
    if (private[0].pointer_target != TARGET_BAR)
    {
        return NO_INDEX;
    }
    uint e = element_at(TARGET_BAR, private[0].pointer);
    if (e == NO_INDEX)
    {
        return NO_INDEX;
    }
    uint item = private[0].elements[e].item;
    return item_of(item).kind == ITEM_CHART ? item : NO_INDEX;
}

/// How many lines the numbers hold: one per chart the samples body carries,
/// laid out on the strip or not.
uint numbers_count()
{
    return min(samples[0].chart_count, MAX_CHARTS);
}

/// The numbers popup's size: the width the last one measured (the lines are
/// built a frame after the open, and shaped after that), and the lines' height
/// from the samples body alone.
float2 numbers_want()
{
    float s = scale_px();
    float ppad = POPUP_PAD * s;
    float w = max(private[0].numbers_w, MENU_MIN_W * s);
    float h = 2.0 * ppad + float(max(numbers_count(), 1u)) * CHART_NUMBERS_LINE_PX * s;
    return float2(w, h);
}

/// Ask for the numbers as a popup of `kind` about chart item `chart`, anchored
/// at the strip, and say so.
void open_numbers(uint kind, uint chart, float4 strip)
{
    set_menu_level(chart);
    private[0].numbers_grow = 0u;
    private[0].numbers_dwell = 0.0;
    request_popup(kind, strip, item_of(chart).id, numbers_want());
    bool repeat;
    Args a = args_new();
    arg(a, kind == POPUP_MENU ? getStringHash("menu") : getStringHash("preview"));
    arg(a, chart_name(item_chart_row(item_of(chart)), repeat));
    arg(a, numbers_count());
    witness(WITNESS_INFO, getStringHash("numbers: open kind={0:s} chart={1:s} lines={2}"), a);
}

/// End the numbers popup for `why`, held until the pointer leaves the strip
/// so the hover does not ask for it straight back.
void close_numbers(uint why)
{
    close_popup();
    private[0].numbers_hold = 1u;
    private[0].numbers_dwell = 0.0;
    witness1(WITNESS_INFO, getStringHash("numbers: closed ({0:s})"), why);
}

/// The strip's hover, once a frame after the layout (so the strip is this
/// frame's): the dwell, the open, the mark following the pointer across the
/// charts, and the end when the pointer leaves the strip. A menu-kind numbers
/// popup is the click's and stays; only a second click or the compositor ends
/// it. Returns true while the dwell clock is running, so the frame loop comes
/// back for it.
bool step_numbers(float dt)
{
    float4 strip;
    bool any = chart_strip(strip);
    float2 p = private[0].pointer;
    bool on_strip = any && private[0].pointer_target == TARGET_BAR
                    && p.x >= strip.x && p.x < strip.x + strip.z
                    && p.y >= strip.y && p.y < strip.y + strip.w;
    if (!on_strip)
    {
        private[0].numbers_hold = 0u;
        private[0].numbers_dwell = 0.0;
    }
    uint chart = on_strip ? chart_under_pointer() : NO_INDEX;

    if (numbers_live() && private[0].popup_kind == POPUP_PREVIEW)
    {
        if (!on_strip)
        {
            close_numbers(getStringHash("left"));
            // Leaving is not a dismissal: the next rest on the strip opens it.
            private[0].numbers_hold = 0u;
            return false;
        }
        // The mark follows the pointer; a gap between two charts keeps the
        // last one. The level moving is a rebuild, which `bar_apply` asks for.
        if (chart != NO_INDEX && !item_of(chart).id.eq(menu_level_id()))
        {
            set_menu_level(chart);
        }
        return false;
    }

    bool idle = private[0].popup_state == POPUP_IDLE;
    // A gap between two charts (or a rounded corner) is on the strip but on
    // no chart: the dwell starts over and no frame is asked for, so a pointer
    // resting there does not redraw at display rate. The next motion onto a
    // chart brings its own frame.
    if (!idle || !CHART_NUMBERS_ON_HOVER || private[0].numbers_hold != 0u || !on_strip
        || chart == NO_INDEX)
    {
        private[0].numbers_dwell = 0.0;
        return false;
    }
    private[0].numbers_dwell += dt;
    if (private[0].numbers_dwell < HOVER_DWELL_S)
    {
        return true;
    }
    open_numbers(POPUP_PREVIEW, chart, strip);
    return false;
}

/// Up one menu level. The back row is present only while there is one to go
/// up to, so this cannot walk off the top.
void menu_ascend()
{
    uint level = menu_level();
    if (level < live_items())
    {
        set_menu_level(private[0].items[level].parent);
    }
}

/// A click landed on `elem` with `button` at `pos`: the bar's entire policy,
/// in one function. Everything it decides leaves as an **action** with its own
/// operands (docs/abi.md, *The action ring*) — a window's id, an app id's bytes, a slot
/// and a row — and the embassy forwards it without resolving anything.
void click(uint elem, uint button, float2 pos)
{
    // The back row is the caustic's own affordance — no item describes it, and
    // it is the only control that goes *up* a level.
    if (elem == ELEM_BACK)
    {
        menu_ascend();
        return;
    }

    uint ii = element_item(elem);
    if (ii >= live_items())
    {
        return; // a slab, or an item that has gone since it was drawn
    }
    Item it = private[0].items[ii];
    Id own = it.id;
    bool caffeine = id_tag(own) == ID_CAFFEINE;

    if (button == BTN_MIDDLE)
    {
        // Middle click is "another one of those" for a dock entry. For an
        // applet it is the sidecar's to interpret, and the wire carries the
        // kind rather than a meaning — `caustics-tray` turns it into SNI's
        // SecondaryActivate, which is what every tray does.
        if (it.kind == ITEM_DOCK)
        {
            act_app(ACT_SPAWN, it);
        }
        else if (it.kind == ITEM_APPLET && !caffeine)
        {
            act_sidecar(it, ACTION_MIDDLE, button, pos, 0.0);
        }
        return;
    }

    if (button == BTN_RIGHT)
    {
        // An applet's secondary click is the sidecar's to interpret, and the
        // wire carries the kind rather than a meaning.
        if (it.kind == ITEM_APPLET)
        {
            if (!caffeine)
            {
                act_sidecar(it, ACTION_SECONDARY, button, pos, 0.0);
            }
            return;
        }
        // So is a sidecar's menu row's: `caustics-notify`
        // reads it as "dismiss without acting". The menu stays up — the row
        // leaves it with the sidecar's next frame — and a toast ends and is
        // held, as on a left click.
        if (it.kind == ITEM_MENU_ROW && id_tag(own) == ID_APPLET
            && (it.flags & (IFLAG_SEPARATOR | IFLAG_DISABLED)) == 0u)
        {
            act_sidecar(it, ACTION_SECONDARY, button, pos, 0.0);
            if (private[0].popup_kind == POPUP_TOAST && popup_live())
            {
                hold_toast(private[0].popup_id);
                end_toast(getStringHash("click"));
            }
            return;
        }
        // A dock entry opens its own menu — New window, Close, Pin or Unpin
        // (DOCK_MENU in the tunables). `own` is the *entry's* id: the menu is
        // about the app, and `bar_build` publishes the rows of the level held
        // here.
        if (it.kind == ITEM_DOCK)
        {
            if (popup_live() && private[0].popup_kind == POPUP_MENU
                && private[0].popup_id.eq(own))
            {
                // Right-clicking the entry whose menu is up closes it, the same
                // rule the applet arm has.
                close_popup();
                return;
            }
            // The one item on the bar that may *already* have a popup of its
            // own: the hover preview, which fires after HOVER_DWELL_S on any
            // entry with a window, so by the time a right click lands it is
            // usually up.
            end_preview_for_menu();
            set_menu_level(ii);
            request_popup(POPUP_MENU, private[0].elements[elem].rect, own,
                          private[0].popup_want);
        }
        return;
    }

    switch (it.kind)
    {
    case ITEM_DOCK:
    {
        // The whole of what a dock click means, decided here: the window this
        // file picked, or — nothing running under the entry — the app id's own
        // bytes for the embassy to resolve to an argv (a desktop-entry lookup,
        // launched through the compositor; this file never sees a command line).
        uint pick = pick_window_index(ii);
        if (pick == NO_INDEX)
        {
            act_app(ACT_SPAWN, it);
        }
        else
        {
            act_window(ACT_ACTIVATE_WINDOW, private[0].items[pick]);
        }
        break;
    }

    case ITEM_OVERVIEW:
        act(ACT_TOGGLE_OVERVIEW);
        break;

    // The calendar: a menu whose level is the clock.
    // Clicking the clock while it is up closes it, as an applet's chip does.
    case ITEM_CLOCK:
    {
        if (!CLOCK_SHOW_CALENDAR)
        {
            break;
        }
        if (popup_live() && private[0].popup_kind == POPUP_MENU
            && private[0].popup_id.eq(own))
        {
            close_popup();
            break;
        }
        // A dock preview still in its grace period when the click lands.
        end_preview_for_menu();
        set_menu_level(ii);
        request_popup(POPUP_MENU, private[0].elements[elem].rect, own,
                      calendar_want(POPUP_PAD * scale_px(), 0.0));
        witness_calendar();
        break;
    }

    // The chart numbers as a menu: it takes the grab, so
    // a click elsewhere ends it, and a second click on the strip closes it as
    // the clock's does. The hover's preview, if up, is replaced by the menu —
    // the embassy's replace path — and the click is the only thing a chart
    // answers: no hover chip, no press dip (`item_is_button`).
    case ITEM_CHART:
    {
        if (numbers_live() && private[0].popup_kind == POPUP_MENU)
        {
            close_numbers(getStringHash("click"));
            break;
        }
        float4 strip;
        if (!chart_strip(strip))
        {
            break;
        }
        end_preview_for_menu();
        open_numbers(POPUP_MENU, ii, strip);
        break;
    }

    // One kind for every chip a sidecar publishes — the tray icons, the
    // battery and the power button among them — and
    // the one chip that is not a sidecar's at all: caffeine, whose inhibitor
    // is an object on the bar's own Wayland connection.
    case ITEM_APPLET:
    {
        if (caffeine)
        {
            act(ACT_TOGGLE_CAFFEINE);
            break;
        }
        if ((it.flags & IFLAG_HAS_MENU) == 0u)
        {
            // No menu: one `ACTION_ACTIVATE` naming the row, with `pos` the
            // physical px this file saw — the embassy divides by the scale,
            // because that is what SNI's `Activate(x, y)` takes at the
            // sidecar's far end.
            act_sidecar(it, ACTION_ACTIVATE, button, pos, 0.0);
            break;
        }
        if (popup_live() && private[0].popup_kind == POPUP_MENU
            && private[0].popup_id.eq(own))
        {
            // Clicking the item whose menu is up closes it, the way every
            // other tray does.
            close_popup();
            break;
        }
        set_menu_level(ii);
        request_popup(POPUP_MENU, private[0].elements[elem].rect, own,
                      private[0].popup_want);
        break;
    }

    case ITEM_MENU_ROW:
    {
        if ((it.flags & (IFLAG_SEPARATOR | IFLAG_DISABLED)) != 0u)
        {
            break;
        }
        if (private[0].popup_kind == POPUP_TOAST && popup_live())
        {
            // A toast's row is a leaf whatever it carries: the click is the
            // `ACT_SIDECAR` the same row sends from the menu, and a row with a
            // submenu (a notification with actions) is activated rather than
            // descended into — a toast has no back row to come up by. The
            // click holds the toast as a refusal does: the
            // frames before the sidecar's answer still ask, and must not
            // reopen it with the rows the user just walked away from.
            act_sidecar(it, ACTION_ACTIVATE, button, pos, 0.0);
            hold_toast(private[0].popup_id);
            end_toast(getStringHash("click"));
            break;
        }
        if ((it.flags & IFLAG_SUBMENU) != 0u)
        {
            // Descend. The popup stays mapped; the want follows the new level
            // and `ACT_POPUP_MOVE` goes out when the two differ.
            set_menu_level(ii);
            break;
        }
        if (id_tag(own) == ID_DOCK_ROW)
        {
            // A dock entry's menu row: the verb the tunable named, with its
            // own operands. `ACT_CLOSE_WINDOW` carries the window the entry's
            // own left click would act on, so Close and "click, then close"
            // cannot disagree; the other three carry the entry's app id range.
            uint verb = item_dock_verb(it);
            if (verb == ACT_CLOSE_WINDOW)
            {
                uint pick = it.parent < live_items() ? pick_window_index(it.parent) : NO_INDEX;
                if (pick != NO_INDEX)
                {
                    act_window(ACT_CLOSE_WINDOW, private[0].items[pick]);
                }
            }
            else if (verb != ACT_NONE)
            {
                act_app(verb, it);
            }
        }
        else
        {
            act_sidecar(it, ACTION_ACTIVATE, button, pos, 0.0);
        }
        close_popup();
        break;
    }

    // A slider ends its drag where the pointer left it, and the popup stays
    // up: unlike a leaf menu row, moving a volume is not "done with this
    // menu". The press and every motion in between already pushed the same
    // kind, so the sidecar sees a stream ending on the final position.
    case ITEM_SLIDER:
        if ((it.flags & IFLAG_DISABLED) == 0u)
        {
            Row row = item_row(it);
            float at = slider_value_at(private[0].elements[elem].rect, pos.x);
            act_sidecar(it, ACTION_SET_VALUE, button, pos, lerp(row.min, row.max, at));
        }
        break;

    default:
        break;
    }
}

/// Where the pointer is, as this file keeps it (docs/abi.md, *The action ring*). The
/// position and its target arrive as commands and live in `private`, which
/// is what makes the pointer a message like every other input.
void set_pointer(uint target, float2 pos)
{
    private[0].pointer = pos;
    private[0].pointer_target = target;
}

/// One command from the ring.
void apply_command(Command c)
{
    switch (c.tag)
    {
    case CMD_POINTER_MOTION:
        // Hover is the sample pass's job — this file keeps the position and
        // the ids are truth. Motion carries policy for exactly one thing:
        // a slider being dragged, where the pointer's x *is* the
        // value. Everything else reads the resulting hover downstream.
        set_pointer(c.target, c.pos);
        drag_slider(c.target, c.pos);
        break;

    case CMD_POINTER_DOWN:
    {
        set_pointer(c.target, c.pos);
        uint hit = press_target(c.target, c.pos);
        private[0].pressed_element = hit;
        // What the press landed *on*, not just where: a sidecar's frame may
        // re-lay the menu out between the press and the release, and an
        // element index alone would then fire the row that moved into its
        // place. The caustic's own elements — a slab, the back row —
        // carry no item and so record the zero id, which still matches itself.
        set_pressed_id(item_id_of_element(hit));
        // Pressing anywhere on a slider's row seeks it and leaves the drag
        // live, so a click on the track is a seek and the drag that may
        // follow is already running.
        drag_slider(c.target, c.pos);
        if (c.target == TARGET_BAR)
        {
            wake_click(c.pos);
        }
        break;
    }

    case CMD_POINTER_UP:
    {
        set_pointer(c.target, c.pos);
        uint hit = press_target(c.target, c.pos);
        uint pressed = private[0].pressed_element;
        // The same element *and* the same item: a release across a re-layout
        // fires nothing rather than firing whatever took that slot — which is
        // what lets an open menu survive its rows changing under it.
        bool same_item = pressed_id().eq(item_id_of_element(hit));
        if (pressed != NO_INDEX && pressed == hit && same_item)
        {
            // The button code rides on the release, so there is no need to
            // remember it from the press.
            click(pressed, c.args.x, c.pos);
        }
        private[0].pressed_element = NO_INDEX;
        set_pressed_id(id_zero());
        break;
    }

    case CMD_POINTER_LEAVE:
    {
        // The pointer is on neither surface as far as this target knows. Only
        // the target that was left may say so — the pointer moving from the
        // bar into the popup leaves the bar, and the popup has it.
        if (private[0].pointer_target == c.target)
        {
            private[0].pointer = float2(-1.0, -1.0);
            private[0].pointer_target = NO_INDEX;
        }
        // Clear hover only if it belongs to the target that was left: the
        // pointer moving from the bar into the popup leaves the bar, and the
        // popup's hover must survive it.
        uint h = private[0].hover_element;
        if (h < live_elements() && private[0].elements[h].target == c.target)
        {
            private[0].hover_element = NO_INDEX;
        }
        // A button held while the pointer leaves is a button that will never
        // click; releasing it here is what stops a stuck press (docs/architecture.md,
        // *Rules*: a toolkit that drops a pointer-leave).
        uint p = private[0].pressed_element;
        if (p < live_elements() && private[0].elements[p].target == c.target)
        {
            private[0].pressed_element = NO_INDEX;
            set_pressed_id(id_zero());
        }
        break;
    }

    case CMD_POPUP_MAPPED:
        // The size that came back is `inputs.popup_size`; this only says the
        // surface exists. A `CMD_POPUP_DONE` instead means the compositor
        // refused, which the arm below answers the same way it answers a
        // dismissal.
        private[0].popup_state = POPUP_UP;
        private[0].popup_sent = float2(float(c.args.x), float(c.args.y));
        break;

    case CMD_POPUP_DONE:
    {
        // A preview popup that goes away without this file asking — the
        // compositor dismissed it, or the embassy gave up — still owes the
        // capture feeder a teardown. A leaked session is a screencast light
        // that never goes out.
        if (private[0].popup_kind == POPUP_PREVIEW && private[0].preview_owner >= 0.0)
        {
            act(ACT_PREVIEW_END);
        }
        // A toast this file did not end — the compositor dismissed it, or
        // refused it outright — is held until its news changes, or the next
        // frame would ask for the same one again.
        if (private[0].popup_kind == POPUP_TOAST && private[0].popup_state != POPUP_ENDING)
        {
            say_toast_shown();
            hold_toast(private[0].popup_id);
            witness1(WITNESS_INFO, getStringHash("toast: closed ({0:s})"),
                     getStringHash("dismissed"));
        }
        // The chart numbers the compositor ended — a click elsewhere under the
        // menu's grab, or a refusal — are held while the pointer stays on the
        // strip, or a refused preview would be asked for again every dwell.
        if (numbers_level() && private[0].popup_state != POPUP_ENDING)
        {
            private[0].numbers_hold = 1u;
            witness1(WITNESS_INFO, getStringHash("numbers: closed ({0:s})"),
                     getStringHash("dismissed"));
        }
        private[0].popup_state = POPUP_IDLE;
        private[0].popup_kind = 0u;
        private[0].popup_id = id_zero();
        private[0].pressed_element = NO_INDEX;
        reset_popup_scratch();
        break;
    }

    case CMD_AXIS:
    {
        // The wheel over an open menu scrolls it. The
        // embassy sends px when the compositor gave px and steps when it gave
        // only steps; a step is one row, which is this file's number.
        if (c.target != TARGET_POPUP || private[0].popup_kind != POPUP_MENU)
        {
            break;
        }
        float delta = float(asint(c.args.x));
        if (delta == 0.0)
        {
            delta = float(asint(c.args.y)) * MENU_ROW_H * scale_px();
        }
        // Clamped against what the last layout measured, so a burst of ticks
        // past the end followed by one back moves back from the end. A level
        // that fits has no room and ignores the wheel.
        private[0].menu_scroll = clamp(private[0].menu_scroll + delta, 0.0,
                                       private[0].menu_scroll_max);
        break;
    }

    case CMD_DUMP_STALL:
        // Nothing here: `bar_apply` looks for this tag in the ring itself and
        // answers it at the *end* of the frame, after the springs have been
        // stepped — the numbers worth reporting are this frame's, not last
        // frame's.
        break;

    default:
        // Unknown tag: ignore. A newer embassy talking to an older caustic
        // should degrade, not corrupt.
        break;
    }
}

/// The one thing that ends a menu's life for the table's sake: the item
/// the popup hangs off is **gone**.
///
/// The level is an id, not an index, so a reshape of the item table under an
/// open menu is no hazard (*Why the ids are ids*, above) and the only question
/// is whether the thing the menu belongs to still exists —
/// a sidecar that died, a tray app that quit, a dock entry whose last window
/// closed. `menu_level()` answers it: NO_INDEX with a live menu popup means the
/// id resolves to nothing, and there is nothing to draw rows under.
///
/// Called before the layout, so a level that has gone never reaches
/// `layout_menu` at all.
void step_menu()
{
    if (popup_live() && private[0].popup_kind == POPUP_MENU && menu_level() == NO_INDEX)
    {
        close_popup();
    }
    // A reload that turned the calendar off while it was up.
    if (popup_live() && private[0].popup_kind == POPUP_MENU && !CLOCK_SHOW_CALENDAR
        && id_tag(menu_level_id()) == ID_CLOCK)
    {
        close_popup();
    }
}

// ---------------------------------------------------------------------------
// Layout. The wings over the item table, then whichever popup is up.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// A row's body, wrapped.
//
// `bar_build` split the body into blocks, a paragraph each (a long one in
// chunks), and `bar_shape` shaped them; here the shaped glyphs are broken into
// lines. Greedy, on the
// measured advances: a line takes glyphs while they fit, breaks at the last
// space that fits, and a word longer than a line is cut at the glyph that
// fits. A paragraph always starts a line. The lines go in `Private.body_lines`
// and the row's `body_lines` names its share; the layout reads their count
// and widths, the draw their glyphs.
//
// Every row wraps to one width — the popup at its widest, `body_wrap_w` — so a
// row's lines do not depend on the popup's size, which depends on them: a
// body wider than that asks for the widest popup, and one narrower is a line
// per paragraph and asks for no more than it is.
// ---------------------------------------------------------------------------

/// The width a row's body wraps to, px: MENU_MAX_W less the pads and both
/// gutters, less the row's icon and the gap after it — the body starts where
/// the label does.
float body_wrap_w(Item it)
{
    float s = scale_px();
    float w = MENU_MAX_W * s - 2.0 * (POPUP_PAD + MENU_GUTTER) * s;
    float icon = item_icon_w(it);
    if (icon > 0.0)
    {
        w -= icon + gap_px();
    }
    return max(w, 1.0);
}

/// Lines wrapped per row: the larger of the two line counts, capped.
uint body_max_lines()
{
    return min(max(TOAST_BODY_LINES, MENU_BODY_LINES), MAX_BODY_LINES);
}

/// Body lines this popup shows of a row: TOAST_BODY_LINES in a toast,
/// MENU_BODY_LINES in a menu, never more than the row has.
uint body_shown(Item it)
{
    uint cap = MENU_BODY_LINES;
    if (private[0].popup_kind == POPUP_TOAST)
    {
        // A toast's row shows the lines it opened with and never more: its
        // rect was sized for them. A re-wrap that finds
        // fewer leaves the rest of the rect empty; one that finds more cuts.
        uint k = toast_index_of(it);
        cap = k == NO_INDEX ? TOAST_BODY_LINES : private[0].toast_lines[k];
    }
    return min(item_body_line_count(it), min(cap, MAX_BODY_LINES));
}

/// Whether the last line shown ends in the ellipsis: lines left unshown, or
/// body left unwrapped (unshaped, unbuilt, or past the lines laid out).
bool body_cut(Item it)
{
    uint shown = body_shown(it);
    return shown > 0u
        && (shown < item_body_line_count(it) || (it.body_lines & BODY_MORE) != 0u);
}

/// The ellipsis' width, px, or 0 while it has none.
float ellipsis_w()
{
    uint b = private[0].ellipsis_block;
    return b < MAX_BLOCKS ? block_size(b).x : 0.0;
}

/// Pen x of glyph `k` of a block's run, px; the run's whole advance at
/// `k == glyph_count`, so `run_pen(blk, k + 1)` is glyph k's far edge.
float run_pen(TextBlock blk, uint k)
{
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint at = blk.first_glyph + k;
    if (k >= blk.glyph_count || at >= min(capacity, MAX_GLYPHS))
    {
        return blk.size.x;
    }
    return float(runs[at].x) / 64.0;
}

/// Whether glyph `k` of a block came from a space: the byte at its cluster.
bool run_space(TextBlock blk, uint k)
{
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint at = blk.first_glyph + k;
    if (at >= min(capacity, MAX_GLYPHS))
    {
        return false;
    }
    uint c = src_byte(blk.source, blk.a + runs[at].cluster);
    return c == 0x20u || c == 0x09u;
}

/// How much of a line leaves room for the ellipsis: the glyphs up to the last
/// non-space whose far edge is within `limit` of the line's start, and that
/// edge. A walk of the line, across its paragraph's chunks.
void body_line_fit(uint block, uint glyph, uint count, float limit,
                   out uint fit, out float fit_w)
{
    fit = 0u;
    fit_w = 0.0;
    uint nblocks = live_blocks();
    uint b = block;
    uint g = glyph;
    uint taken = 0u;
    float shift = 0.0;
    bool started = false;
    for (uint j = 0u; j < MAX_BODY_BLOCKS && taken < count && b < nblocks; ++j)
    {
        TextBlock blk = private[0].blocks[b];
        if (!started)
        {
            shift = -run_pen(blk, g);
            started = true;
        }
        uint n = min(blk.glyph_count, MAX_GLYPHS_PER_BLOCK);
        for (uint k = g; k < n && taken < count; ++k)
        {
            float end = shift + run_pen(blk, k + 1u);
            if (end > limit)
            {
                return;
            }
            taken += 1u;
            if (!run_space(blk, k))
            {
                fit = taken;
                fit_w = end;
            }
        }
        shift += blk.size.x;
        b += 1u;
        g = 0u;
    }
}

/// One line into the pool, its fit measured; false when the pool is full.
bool push_body_line(uint block, uint glyph, uint count, float width, float wrap_w)
{
    uint at = private[0].body_line_count;
    if (at >= MAX_BODY_POOL || count == 0u)
    {
        return count == 0u;
    }
    BodyLine line;
    line.block = block;
    line.glyph = glyph;
    line.count = count;
    line.width = width;
    uint fit;
    float fit_w;
    body_line_fit(block, glyph, count, wrap_w - ellipsis_w(), fit, fit_w);
    line.fit = fit;
    line.fit_w = fit_w;
    private[0].body_lines[at] = line;
    private[0].body_line_count = at + 1u;
    return true;
}

/// Wrap item `i`'s body into the pool and write its `body_lines`.
void wrap_body(uint i)
{
    Item it = private[0].items[i];
    uint nb = item_body_block_count(it);
    uint first_line = private[0].body_line_count;
    uint lines = 0u;
    bool more = (it.body_blocks & BODY_MORE) != 0u;
    float wrap_w = body_wrap_w(it);
    uint max_lines = body_max_lines();
    uint nblocks = live_blocks();

    bool open = false;      // a line has been started
    uint ls_b = 0u;         // where it starts: a block and a glyph in it
    uint ls_g = 0u;
    float ls_x = 0.0;       // and its pen, in the paragraph's x
    uint n = 0u;            // glyphs on it so far, spaces too
    uint ns_n = 0u;         // to its last non-space glyph
    float ns_w = 0.0;
    bool cand = false;      // a space it may break at, and the line up to it
    uint cand_n = 0u;
    float cand_w = 0.0;
    uint ws_b = 0u;         // the word the last glyph is in: where it starts
    uint ws_g = 0u;
    uint ws_n = 0u;
    float ws_x = 0.0;
    bool prev_space = true;
    bool stopped = false;
    float base = 0.0;       // the paragraph's x at this block's origin

    for (uint j = 0u; j < nb && !stopped; ++j)
    {
        uint bi = it.aux_block + j;
        if (bi >= nblocks)
        {
            break;
        }
        TextBlock blk = private[0].blocks[bi];
        if ((blk.bflags & BLOCK_PARAGRAPH) != 0u)
        {
            // A newline of the body's own: whatever line is open ends here.
            if (open)
            {
                if (!push_body_line(ls_b, ls_g, ns_n, ns_w, wrap_w))
                {
                    more = true;
                    break;
                }
                lines += 1u;
                open = false;
            }
            base = 0.0;
        }
        if (lines >= max_lines)
        {
            more = true;
            break;
        }
        uint count = min(blk.glyph_count, MAX_GLYPHS_PER_BLOCK);
        for (uint k = 0u; k < count; ++k)
        {
            float gx = base + run_pen(blk, k);
            float gend = base + run_pen(blk, k + 1u);
            bool space = run_space(blk, k);
            if (!open)
            {
                if (space)
                {
                    continue; // a line never starts with a space
                }
                open = true;
                ls_b = bi;
                ls_g = k;
                ls_x = gx;
                n = 0u;
                ns_n = 0u;
                ns_w = 0.0;
                cand = false;
                prev_space = true;
            }
            if (space)
            {
                // Spaces hang past the edge; the first of a run is a place
                // to break, and the line before it is what that leaves.
                if (!prev_space)
                {
                    cand = true;
                    cand_n = ns_n;
                    cand_w = ns_w;
                }
                prev_space = true;
                n += 1u;
                continue;
            }
            if (prev_space)
            {
                ws_b = bi;
                ws_g = k;
                ws_n = n;
                ws_x = gx;
            }
            prev_space = false;
            if (gend - ls_x > wrap_w && n > 0u && cand)
            {
                // At the last space that fits: the word this glyph is in
                // starts the next line.
                if (!push_body_line(ls_b, ls_g, cand_n, cand_w, wrap_w))
                {
                    stopped = true;
                    more = true;
                    break;
                }
                lines += 1u;
                ls_b = ws_b;
                ls_g = ws_g;
                ls_x = ws_x;
                n -= ws_n;
                ns_n = n;
                ns_w = gx - ls_x;
                ws_n = 0u;
                cand = false;
                if (lines >= max_lines)
                {
                    stopped = true;
                    more = true;
                    break;
                }
            }
            if (gend - ls_x > wrap_w && n > 0u)
            {
                // A word longer than the line: cut at the glyph that fits.
                if (!push_body_line(ls_b, ls_g, n, gx - ls_x, wrap_w))
                {
                    stopped = true;
                    more = true;
                    break;
                }
                lines += 1u;
                ls_b = bi;
                ls_g = k;
                ls_x = gx;
                n = 0u;
                ns_n = 0u;
                ns_w = 0.0;
                ws_b = bi;
                ws_g = k;
                ws_n = 0u;
                ws_x = gx;
                cand = false;
                if (lines >= max_lines)
                {
                    stopped = true;
                    more = true;
                    break;
                }
            }
            n += 1u;
            ns_n = n;
            ns_w = gend - ls_x;
        }
        // A chunk the shaper could not finish is the end of what can be
        // drawn: the rest of its bytes have no glyphs.
        if (!stopped && (blk.flags & (SHAPE_RUN_TRUNCATED | SHAPE_RUN_DEADLINE)) != 0u)
        {
            more = true;
            break;
        }
        base += blk.size.x;
    }
    if (open && !stopped && lines < max_lines)
    {
        if (push_body_line(ls_b, ls_g, ns_n, ns_w, wrap_w))
        {
            lines += 1u;
        }
        else
        {
            more = true;
        }
    }
    private[0].items[i].body_lines =
        (first_line & 0xffffu) | (min(lines, 0xffu) << 16) | (more ? BODY_MORE : 0u);
}

/// Every body of the item table, wrapped — when anything the wrap reads has
/// moved since the last time: a build, the scale, a block's shaped count or
/// width. Otherwise nothing: this is a walk of every glyph of every body, and
/// `bar_apply` runs every frame.
void wrap_bodies()
{
    uint n = live_items();
    uint nblocks = live_blocks();
    uint h = FNV_OFFSET;
    h = (h ^ private[0].build_serial) * FNV_PRIME;
    h = (h ^ asuint(scale_px())) * FNV_PRIME;
    h = (h ^ asuint(ellipsis_w())) * FNV_PRIME;
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        uint nb = item_body_block_count(it);
        if (nb == 0u)
        {
            continue;
        }
        h = (h ^ i) * FNV_PRIME;
        h = (h ^ asuint(item_icon_w(it))) * FNV_PRIME;
        for (uint j = 0u; j < nb && it.aux_block + j < nblocks; ++j)
        {
            TextBlock blk = private[0].blocks[it.aux_block + j];
            h = (h ^ blk.glyph_count) * FNV_PRIME;
            h = (h ^ asuint(blk.size.x)) * FNV_PRIME;
            h = (h ^ blk.flags) * FNV_PRIME;
        }
    }
    h = h == 0u ? FNV_PRIME : h;
    if (h == private[0].wrap_key)
    {
        return;
    }
    private[0].wrap_key = h;
    private[0].body_line_count = 0u;
    for (uint i = 0u; i < n; ++i)
    {
        if (item_body_block_count(private[0].items[i]) == 0u)
        {
            private[0].items[i].body_lines = 0u;
            continue;
        }
        wrap_body(i);
    }
}

/// A menu row's height in this popup: a plain row's, and under it the body
/// lines shown and the pad below the last.
float level_row_h(Item it)
{
    float h = menu_row_height(it);
    uint shown = body_shown(it);
    if (shown > 0u)
    {
        h += (float(shown) * MENU_BODY_LINE_H + MENU_BODY_PAD) * scale_px();
    }
    return h;
}

/// A menu row's content width: its icon and label, or its icon and its widest
/// body line shown — the last with the ellipsis after it when it is cut —
/// whichever is wider.
float level_row_w(Item it)
{
    float w = item_content_w(it);
    uint shown = body_shown(it);
    if (shown == 0u)
    {
        return w;
    }
    float lead = item_icon_w(it) > 0.0 ? item_icon_w(it) + gap_px() : 0.0;
    uint first = item_body_first_line(it);
    bool cut = body_cut(it);
    for (uint k = 0u; k < shown && first + k < private[0].body_line_count; ++k)
    {
        BodyLine line = private[0].body_lines[first + k];
        float lw = line.width;
        if (cut && k + 1u == shown)
        {
            lw = line.fit_w + ellipsis_w();
        }
        w = max(w, lead + lw);
    }
    return w;
}

/// Say how many body lines a row placed at `elem` shows, once per change of
/// row or count: `body: slot=… row=… lines=… cut=…`, INFO
/// in a toast — one line per notification shown — and DEBUG in a menu, which
/// lays out a dozen at a time. The layout's rect says the same in px; this
/// says it in lines.
void witness_body(uint elem, Item it, bool open, bool toast)
{
    if (elem >= MAX_ELEMENTS)
    {
        return;
    }
    uint shown = body_shown(it);
    bool cut = body_cut(it);
    uint key = 0u;
    if (shown > 0u && open)
    {
        key = (FNV_OFFSET ^ it.id.lo) * FNV_PRIME;
        key = (key ^ it.id.hi) * FNV_PRIME;
        key = ((key ^ (shown | (cut ? 0x100u : 0u))) * FNV_PRIME) | 1u;
    }
    if (key == private[0].elements[elem].said_body)
    {
        return;
    }
    private[0].elements[elem].said_body = key;
    if (key == 0u)
    {
        return;
    }
    Args a = args_new();
    arg(a, id_aux(it.id));
    arg(a, id_payload(it.id));
    arg(a, shown);
    arg(a, cut ? 1u : 0u);
    witness(toast ? WITNESS_INFO : WITNESS_DEBUG,
            getStringHash("body: slot={0} row={1} lines={2} cut={3}"), a);
}

/// Is item `it` a row of level `level`: every menu row under the level. (A
/// toast's rows are its own list, `Private.toast_ids`.)
bool level_row(Item it, uint level)
{
    return menu_row_kind(it.kind) && it.parent == level;
}

/// The calendar's cells, measured and placed. Returns the
/// next free element index.
///
/// The title spans the popup under its pad; the grid is centred under it,
/// seven columns of CAL_CELL_PX with CAL_GAP between, the weekday initials in
/// its first row and the days from the 1st's column on. Where a cell goes is
/// read off its id — a weekday's index, a day's number — against this frame's
/// facts, so the cells need no position of their own. A calendar is never
/// taller than a title and seven rows, so it has no scroll and no clip.
uint layout_calendar(uint next, bool open, float2 popup, float ppad)
{
    float s = scale_px();
    uint n = live_items();
    uint level = menu_level();
    float cell = CAL_CELL_PX * s;
    float gap = CAL_GAP * s;
    float title_h = MENU_ROW_H * s;

    float title_w = 0.0;
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind == ITEM_CAL_CELL && it.parent == level && id_aux(it.id) == CAL_TITLE)
        {
            title_w = max(title_w, item_content_w(it) + 2.0 * CHIP_PAD * s);
        }
    }
    float2 want = calendar_want(ppad, title_w);
    private[0].popup_want = want;
    private[0].menu_scroll = 0.0;
    private[0].menu_scroll_max = 0.0;
    private[0].menu_clip = float2(0.0, 0.0);

    float2 size = open ? popup : want;
    float inner_w = max(size.x - 2.0 * ppad, 1.0);
    float grid_w = 7.0 * cell + 6.0 * gap;
    float x0 = ppad + floor((inner_w - grid_w) * 0.5);
    float y0 = ppad + title_h;
    float step = cell + gap;

    place(ELEM_BACK, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0, NO_INDEX, NO_INDEX, false);
    for (uint i = 0u; i < n && next < MAX_ELEMENTS; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != ITEM_CAL_CELL || it.parent != level)
        {
            continue;
        }
        uint which = id_payload(it.id);
        float4 rect;
        switch (id_aux(it.id))
        {
        case CAL_TITLE:
            rect = float4(ppad, ppad, inner_w, title_h);
            break;
        case CAL_WEEKDAY:
            rect = float4(x0 + float(calendar_column(which % 7u)) * step, y0, cell, cell);
            break;
        default:
        {
            uint at = calendar_cell(which);
            rect = float4(x0 + float(at % 7u) * step, y0 + float(at / 7u + 1u) * step, cell, cell);
            break;
        }
        }
        place(next, TARGET_POPUP, rect, MENU_RADIUS * s, it.block, i, open);
        next += 1u;
    }
    place(ELEM_PREVIEW, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0,
          NO_INDEX, NO_INDEX, false);
    return next;
}

/// The widest name and the widest value among the chart numbers' lines, px,
/// and how many lines there are.
uint numbers_columns(out float name_w, out float value_w)
{
    name_w = 0.0;
    value_w = 0.0;
    uint lines = 0u;
    uint n = live_items();
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != ITEM_CHART_LINE)
        {
            continue;
        }
        name_w = max(name_w, block_size(it.block).x);
        value_w = max(value_w, block_size(it.aux_block).x);
        lines += 1u;
    }
    return lines;
}

/// The chart numbers' lines, measured and placed. Returns
/// the next free element index.
///
/// One line per chart, in the samples body's order — the strip's own — each
/// the popup's inner width and CHART_NUMBERS_LINE_PX tall. Inside a line the
/// swatch takes the left gutter a menu row's check mark takes, the name starts
/// where a row's label does, and the value is right-aligned against the right
/// gutter, so the numbers read as a column. The lines are not buttons and the
/// popup never scrolls: eight charts is the samples body's cap.
uint layout_numbers(uint next, bool open, float2 popup, float ppad)
{
    float s = scale_px();
    float line_h = CHART_NUMBERS_LINE_PX * s;
    float gutter = MENU_GUTTER * s;

    float name_w;
    float value_w;
    uint lines = numbers_columns(name_w, value_w);
    float2 want = numbers_want();
    if (lines > 0u && name_w > 0.0)
    {
        float measured = max(2.0 * (ppad + gutter) + name_w + 2.0 * gap_px() + value_w,
                             MENU_MIN_W * s);
        // Grows while up; the first measurement of an open replaces the last
        // open's width, so a narrower set of numbers is not held wide forever.
        private[0].numbers_w = private[0].numbers_grow == 0u
            ? measured
            : max(private[0].numbers_w, measured);
        private[0].numbers_grow = 1u;
        want = float2(private[0].numbers_w,
                      2.0 * ppad + float(lines) * line_h);
    }
    private[0].popup_want = want;
    private[0].menu_scroll = 0.0;
    private[0].menu_scroll_max = 0.0;
    private[0].menu_clip = float2(0.0, 0.0);

    float2 size = open ? popup : want;
    float inner_w = max(size.x - 2.0 * ppad, 1.0);
    place(ELEM_BACK, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0, NO_INDEX, NO_INDEX, false);
    uint n = live_items();
    uint row = 0u;
    for (uint i = 0u; i < n && next < MAX_ELEMENTS; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != ITEM_CHART_LINE)
        {
            continue;
        }
        float4 rect = float4(ppad, ppad + float(row) * line_h, inner_w, line_h);
        place(next, TARGET_POPUP, rect, MENU_RADIUS * s, it.block, i, open);
        next += 1u;
        row += 1u;
    }
    place(ELEM_PREVIEW, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0,
          NO_INDEX, NO_INDEX, false);
    return next;
}

/// A toast's rows, placed. Returns the next free element.
///
/// Nothing here is measured: the rows, their order and their heights are the
/// ones `step_toast_pending` took when the toast opened, and the popup's want
/// is the size it opened at. Each row's rect is where it was when the toast
/// opened, whatever frames have arrived since — a new attention row is not
/// one of them, and a row the sidecar removed is a gap, not a reason for the
/// rows below to move up under the pointer. A row that no longer fits the
/// surface the compositor gave is not placed.
uint layout_toast(uint next, bool open, float2 popup, float ppad)
{
    uint level = menu_level();
    uint slot = private[0].toast_slot;
    float2 size = private[0].toast_size;
    private[0].popup_want = size;
    private[0].menu_scroll = 0.0;
    private[0].menu_scroll_max = 0.0;
    private[0].menu_clip = float2(0.0, 0.0);

    float view_h = open ? popup.y : size.y;
    float inner_w = max((open ? popup.x : size.x) - 2.0 * ppad, 1.0);
    float bottom = view_h - ppad;
    place(ELEM_BACK, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0, NO_INDEX, NO_INDEX, false);
    float y = ppad;
    uint count = min(private[0].toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count && next < MAX_ELEMENTS; ++k)
    {
        float h = private[0].toast_h[k];
        float at = y;
        y += h;
        uint i = index_of_id(id_tagged(ID_APPLET, slot, private[0].toast_ids[k]));
        if (i == NO_INDEX || private[0].items[i].parent != level || at + h > bottom + 0.5)
        {
            continue;
        }
        Item it = private[0].items[i];
        place(next, TARGET_POPUP, float4(ppad, floor(at), inner_w, h), MENU_RADIUS * scale_px(),
              it.block, i, open);
        witness_body(next, it, open, true);
        next += 1u;
    }
    place(ELEM_PREVIEW, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0,
          NO_INDEX, NO_INDEX, false);
    return next;
}

/// The menu rows of the current level, measured and placed. Returns the next
/// free element index.
///
/// A toast has a layout of its own (`layout_toast`): the
/// rows it opened with, where it opened with them.
uint layout_menu(uint next, bool open, float2 popup, float ppad, bool toast)
{
    float s = scale_px();
    uint n = live_items();
    uint level = menu_level();
    float gutter = MENU_GUTTER * s;

    if (toast)
    {
        return layout_toast(next, open, popup, ppad);
    }
    // The clock's menu is its calendar: a grid, not rows.
    if (level < n && private[0].items[level].kind == ITEM_CLOCK)
    {
        return layout_calendar(next, open, popup, ppad);
    }

    // A back row exists exactly when the level is itself a row — i.e. the bar has
    // descended into a submenu. At the root the level is the item whose menu
    // this is (a tray item, the power button, an applet).
    bool has_back = level < n && menu_row_kind(private[0].items[level].kind);

    // Measure first: the popup's size is a request the embassy answers, and it
    // has to be current before the surface is asked for.
    float back_h = has_back ? MENU_ROW_H * s : 0.0;
    float widest = 0.0;
    float content_h = 0.0;
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (!level_row(it, level))
        {
            continue;
        }
        content_h += level_row_h(it);
        widest = max(widest, level_row_w(it));
    }
    float wanted_w = clamp(widest + 2.0 * (ppad + gutter), MENU_MIN_W * s, MENU_MAX_W * s);
    float wanted_h = 2.0 * ppad + back_h + content_h;
    // No taller than the output less the bar less a margin: tailscale's
    // exit-node level is ~130 rows, and a surface asked for
    // at its whole height fits nowhere. Zero is an output not yet known, and
    // no bound. Never less than the back row and one row, so a tiny output
    // still gets a menu that can scroll.
    float output_h = float(inputs.output_size.y);
    if (output_h > 0.0)
    {
        float room = output_h - float(inputs.bar_size.y) - POPUP_MARGIN * s;
        wanted_h = min(wanted_h, max(room, 2.0 * ppad + back_h + MENU_ROW_H * s));
    }
    private[0].popup_want = float2(wanted_w, wanted_h);

    // Then place, into whatever the compositor actually gave — or, while
    // it has given nothing yet, into what was asked for, so the rows placed
    // absent are the ones that will be there.
    float view_h = open ? popup.y : wanted_h;
    float inner_w = max(popup.x - 2.0 * ppad, 1.0);
    float top = ppad + back_h;
    float bottom = max(view_h - ppad, top);

    // The scroll, clamped against this frame's rows: a level that shrank
    // under an open menu (a sidecar's rows changing) pulls it back.
    float scroll_max = max(content_h - (bottom - top), 0.0);
    private[0].menu_scroll_max = scroll_max;
    float scroll = clamp(private[0].menu_scroll, 0.0, scroll_max);
    private[0].menu_scroll = scroll;
    // The band scrolled rows are drawn and hit in. Only a level that scrolls
    // has one: a half row at either end is cut there rather than drawn over
    // the back row or the pad.
    private[0].menu_clip = scroll_max > 0.0 ? float2(top, bottom) : float2(0.0, 0.0);

    // The back row is placed before the rows and is no part of the scroll.
    place(ELEM_BACK, TARGET_POPUP, float4(ppad, ppad, inner_w, MENU_ROW_H * s), MENU_RADIUS * s,
          NO_INDEX, NO_INDEX, open && has_back);

    float y = top - scroll;
    for (uint i = 0u; i < n && next < MAX_ELEMENTS; ++i)
    {
        Item it = private[0].items[i];
        if (!level_row(it, level))
        {
            continue;
        }
        float h = level_row_h(it);
        float at = y;
        y += h;
        // A row wholly outside the band is not placed at all: the element
        // table holds what can be seen, which keeps MAX_ELEMENTS far from a
        // 130-row level.
        if (at + h <= top || at >= bottom)
        {
            continue;
        }
        bool sep = (it.flags & IFLAG_SEPARATOR) != 0u;
        place(next, TARGET_POPUP, float4(ppad, floor(at), inner_w, h),
              sep ? 0.0 : MENU_RADIUS * s, it.block, i, open);
        witness_body(next, it, open, false);
        next += 1u;
    }

    // The preview slot belongs to the other popup kind.
    place(ELEM_PREVIEW, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0,
          NO_INDEX, NO_INDEX, false);
    return next;
}

/// The title block a preview popup shows: the captured window's, or the dock
/// entry's when the window has since gone.
uint preview_title(uint entry)
{
    uint win = entry == NO_INDEX ? NO_INDEX : first_window(entry);
    if (win != NO_INDEX)
    {
        return private[0].items[win].block;
    }
    return entry < live_items() ? private[0].items[entry].block : NO_INDEX;
}

/// The size a preview popup asks for: the box, cropped to the capture's aspect
/// so a portrait window gets a portrait popup rather than a letterboxed wide
/// one, plus the title's line and the pads.
///
/// It lives in its own function because the *request* is made after the layout
/// has run — the dwell clock is downstream of it — so `layout_preview` cannot
/// be the only place this is worked out without leaving the first request
/// carrying whatever the previous popup wanted.
float2 preview_want(uint entry, float ppad)
{
    float s = scale_px();
    float w = PREVIEW_MAX_W * s;
    float h = PREVIEW_MAX_H * s;
    float2 src = float2(inputs.preview_size);
    if (src.x > 0.0 && src.y > 0.0)
    {
        float aspect = src.x / src.y;
        float fitted = w / max(aspect, 1e-4);
        if (fitted <= h)
        {
            h = fitted;
        }
        else
        {
            w = h * aspect;
        }
    }
    return float2(w + 2.0 * ppad, h + block_size(preview_title(entry)).y + 3.0 * ppad);
}

/// The captured still and the window's title. Returns the next free element.
uint layout_preview(uint next, bool open, float2 popup, float ppad)
{
    float s = scale_px();
    uint entry = scratch_index(private[0].preview_owner);
    uint title = preview_title(entry);
    float2 tsize = block_size(title);
    float2 src = float2(inputs.preview_size);

    private[0].popup_want = preview_want(entry, ppad);

    // Where it actually goes: letterboxed into what the compositor gave,
    // which is not always what was asked for.
    float avail_w = max(popup.x - 2.0 * ppad, 1.0);
    float avail_h = max(popup.y - 3.0 * ppad - tsize.y, 1.0);
    float aspect = (src.x > 0.0 && src.y > 0.0) ? src.x / src.y : avail_w / max(avail_h, 1.0);
    float w = avail_w;
    float h = avail_w / max(aspect, 1e-4);
    if (h > avail_h)
    {
        h = avail_h;
        w = avail_h * aspect;
    }
    w = floor(max(w, 1.0));
    h = floor(max(h, 1.0));

    place(ELEM_PREVIEW, TARGET_POPUP,
          float4(ppad + floor((avail_w - w) * 0.5), ppad, w, h),
          PREVIEW_RADIUS * s, NO_INDEX, NO_INDEX, open);

    if (title != NO_INDEX && next < MAX_ELEMENTS)
    {
        place(next, TARGET_POPUP,
              float4(ppad, ppad + h + ppad, avail_w, tsize.y), 0.0, title, NO_INDEX, open);
        next += 1u;
    }

    // The back row belongs to the other popup kind.
    place(ELEM_BACK, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0,
          NO_INDEX, NO_INDEX, false);
    return next;
}

/// How many ordering passes one kind takes on a wing. Applets order among
/// themselves by `APPLET_ORDER` in `bar_tunables.slang`, so their kind takes
/// one pass per entry of it — which is what lets a wing array name ITEM_APPLET
/// once and every applet sidecar land where the tunables say, with no edit here
/// and none in the wing arrays either. Every other kind takes one pass.
uint kind_passes(uint kind)
{
    return kind == ITEM_APPLET ? uint(APPLET_ORDER.getCount()) : 1u;
}

/// Whether item `it` is placed on pass `sub` of kind `kind`.
bool item_in_pass(Item it, uint kind, uint sub)
{
    if (it.kind == ITEM_NONE || it.kind != kind)
    {
        return false;
    }
    if (kind != ITEM_APPLET)
    {
        return true;
    }

    uint applet = item_applet(it);
    uint want = APPLET_ORDER[sub];
    if (applet == want)
    {
        return true;
    }
    // An applet kind the array does not name is a third-party sidecar's own
    // value, and it rides in APPLET_OTHER's pass: a sidecar must not be able
    // to make its own item vanish off the bar by picking a number nobody here
    // has heard of.
    if (want != APPLET_OTHER)
    {
        return false;
    }
    for (uint k = 0u; k < uint(APPLET_ORDER.getCount()); ++k)
    {
        if (APPLET_ORDER[k] == applet)
        {
            return false;
        }
    }
    return true;
}

/// The gap between two adjacent items of the centre wing. Two charts sit
/// CHART_GAP apart — the strip reads as one instrument cluster, as a
/// system monitor's charts do in a desktop panel — and every other pair takes the
/// wing gap. The side wings keep ITEM_GAP throughout; nothing puts a chart in
/// one, and if a tunables edit ever does, it lands there evenly spaced.
float center_gap(uint prev_kind, uint kind)
{
    return (prev_kind == ITEM_CHART && kind == ITEM_CHART)
           ? CHART_GAP * scale_px()
           : gap_px();
}

/// The centre group's width with at most `keep` charts in it: the same walk
/// the placement loop makes, so the group the drop rule measures is exactly the
/// group it then places. Charts are dropped from the *end* — CENTER_ORDER puts
/// them after the clock, and the last chart of the sidecar's list is the one
/// the strip can most afford to lose.
float center_span(uint keep)
{
    uint n = live_items();
    float total = 0.0;
    uint charts = 0u;
    uint prev = ITEM_NONE;
    for (uint k = 0u; k < uint(CENTER_ORDER.getCount()); ++k)
    {
        uint kind = CENTER_ORDER[k];
        uint passes = kind_passes(kind);
        for (uint sub = 0u; sub < passes; ++sub)
        {
            for (uint i = 0u; i < n; ++i)
            {
                Item it = private[0].items[i];
                if (!item_in_pass(it, kind, sub))
                {
                    continue;
                }
                if (kind == ITEM_CHART)
                {
                    if (charts >= keep)
                    {
                        continue;
                    }
                    charts += 1u;
                }
                total += (prev == ITEM_NONE ? 0.0 : center_gap(prev, kind)) + bar_item_w(it);
                prev = kind;
            }
        }
    }
    return total;
}

/// Every element of every target, from the tunables and the item table. Immediate
/// mode: the elements are recomputed every frame, and what persists is in
/// `Private` (the springs, the popup and toast state, the scroll, the dwell
/// and the drag).
void layout()
{
    float s = scale_px();
    float2 bar = float2(inputs.bar_size);
    bool bar_mapped = bar.x > 0.0 && bar.y > 0.0;

    // Everything starts absent; the walks below turn on exactly what they
    // place. Cheaper than reasoning about last frame's count, and it is what
    // makes a shrinking dock leave nothing behind.
    for (uint i = 0u; i < MAX_ELEMENTS; ++i)
    {
        private[0].elements[i].flags = 0u;
    }

    // The slab: the whole surface, and the default owner of every texel on it.
    // A click on "nothing" is still a click on the bar, never a fall-through.
    place(ELEM_BAR, TARGET_BAR, float4(0.0, 0.0, bar), slab_radius(),
          NO_INDEX, NO_INDEX, bar_mapped);

    // The band the contents live in: what the bar asked for, or the surface, if
    // the compositor gave less. `band_px()` and
    // `row_height()` are the same arithmetic, factored out because
    // `bar_item_w` sizes a chart off the row height.
    float band = band_px();
    float row_h = row_height();
    float row_y = floor((band - row_h) * 0.5);
    float chip_r = CHIP_RADIUS * s;
    float gap = gap_px();
    float pad = EDGE_PADDING * s;

    uint n = live_items();
    uint next = ELEM_FIRST_FREE;

    // No permutation: `bar_build` emits the dock's entries in the order they
    // are *shown* — the favourites in the file's own order, then the
    // running-but-unpinned tail by first-seen — so this pass needs no sort
    // and no `uint[MAX_ITEMS]` locals.

    // The three wings, each laid out by walking its order array in
    // `bar_tunables.slang` and, per kind, the item table in order
    // (placement is layout, and layout is this file's). A kind
    // named in no array is simply never placed on the bar, which is how
    // ITEM_WINDOW and ITEM_MENU_ROW stay off it, and how a kind can be named
    // in an order array before anything publishes one.

    // Left wing: left to right from the edge padding.
    float x = pad;
    for (uint k = 0u; k < uint(LEFT_ORDER.getCount()); ++k)
    {
        uint kind = LEFT_ORDER[k];
        uint passes = kind_passes(kind);
        for (uint sub = 0u; sub < passes; ++sub)
        {
            for (uint i = 0u; i < n && next < MAX_ELEMENTS; ++i)
            {
                uint ii = i;
                Item it = private[0].items[ii];
                if (!item_in_pass(it, kind, sub))
                {
                    continue;
                }
                float w = bar_item_w(it);
                place(next, TARGET_BAR, float4(floor(x), row_y, w, row_h), chip_r,
                      item_primary_block(it), ii, bar_mapped);
                next += 1u;
                x += w + gap;
            }
        }
    }
    // Where the centre group may not reach. An empty wing leaves this at the
    // edge padding; a wing with items leaves a gap's worth of clearance past
    // its last one, because the walk above adds the gap after every item.
    float left_end = x;

    // Right wing: right to left from the far edge, so the array's *first*
    // entry ends up outermost on screen.
    x = bar.x - pad;
    for (uint k = 0u; k < uint(RIGHT_ORDER.getCount()); ++k)
    {
        uint kind = RIGHT_ORDER[k];
        uint passes = kind_passes(kind);
        for (uint sub = 0u; sub < passes; ++sub)
        {
            for (uint i = 0u; i < n && next < MAX_ELEMENTS; ++i)
            {
                uint ii = i;
                Item it = private[0].items[ii];
                if (!item_in_pass(it, kind, sub))
                {
                    continue;
                }
                float w = bar_item_w(it);
                x -= w;
                place(next, TARGET_BAR, float4(floor(x), row_y, w, row_h), chip_r,
                      item_primary_block(it), ii, bar_mapped);
                next += 1u;
                x -= gap;
            }
        }
    }
    float right_start = x;

    // Centre wing: measured, then centred on the *whole* bar rather than on
    // the free space between the wings — a clock that shifts when the tray
    // grows reads as broken, and this is a bar, not a text layout engine.
    //
    // The clock shares the wing with the chart strip, and the two
    // together can outgrow the space between the side wings (the clock's date
    // alone cost 120 px). When the centred group would overlap either wing,
    // **charts are dropped from the end until it fits** — no scrolling, no
    // shrinking, and a reader of the journal (the test harness) learns which
    // survived from the `layout:` lines that stop naming them.
    uint chart_items = 0u;
    for (uint i = 0u; i < n; ++i)
    {
        if (private[0].items[i].kind == ITEM_CHART)
        {
            chart_items += 1u;
        }
    }
    uint keep = chart_items;
    float total = center_span(keep);
    x = floor((bar.x - total) * 0.5);
    while (keep > 0u && (x < left_end || x + total > right_start))
    {
        keep -= 1u;
        total = center_span(keep);
        x = floor((bar.x - total) * 0.5);
    }

    uint charts_placed = 0u;
    uint prev_kind = ITEM_NONE;
    for (uint k = 0u; k < uint(CENTER_ORDER.getCount()); ++k)
    {
        uint kind = CENTER_ORDER[k];
        uint passes = kind_passes(kind);
        for (uint sub = 0u; sub < passes; ++sub)
        {
            for (uint i = 0u; i < n && next < MAX_ELEMENTS; ++i)
            {
                uint ii = i;
                Item it = private[0].items[ii];
                if (!item_in_pass(it, kind, sub))
                {
                    continue;
                }
                if (kind == ITEM_CHART)
                {
                    if (charts_placed >= keep)
                    {
                        continue;
                    }
                    charts_placed += 1u;
                }
                float w = bar_item_w(it);
                if (prev_kind != ITEM_NONE)
                {
                    x += center_gap(prev_kind, kind);
                }
                place(next, TARGET_BAR, float4(floor(x), row_y, w, row_h),
                      kind == ITEM_CHART ? 0.0 : chip_r,
                      item_primary_block(it), ii, bar_mapped);
                next += 1u;
                prev_kind = kind;
                x += w;
            }
        }
    }

    // The popup exists only while the compositor says it is mapped *and* the
    // CPU has said how big it came out. Anything less and its elements are
    // absent, which is what keeps the carve and the draw off an unmapped
    // surface.
    float2 popup = float2(inputs.popup_size);
    bool open = private[0].popup_state == POPUP_UP && popup.x > 0.0 && popup.y > 0.0;
    place(ELEM_POPUP, TARGET_POPUP, float4(0.0, 0.0, popup), POPUP_RADIUS * s,
          NO_INDEX, NO_INDEX, open);

    float ppad = POPUP_PAD * s;
    uint kind = private[0].popup_kind;
    if ((kind == POPUP_MENU || kind == POPUP_PREVIEW) && numbers_level())
    {
        next = layout_numbers(next, open, popup, ppad);
    }
    else if (kind == POPUP_MENU || kind == POPUP_TOAST)
    {
        next = layout_menu(next, open, popup, ppad, kind == POPUP_TOAST);
    }
    else if (kind == POPUP_PREVIEW)
    {
        next = layout_preview(next, open, popup, ppad);
    }

    private[0].element_count = max(next, ELEM_FIRST_FREE);
}

// ---------------------------------------------------------------------------
// Hover dwell and the preview's life. The one piece of policy that is a clock
// rather than an event.
// ---------------------------------------------------------------------------

/// Advance the dwell timer, ask for a preview when it fills, and end one when
/// the pointer has left both the entry and the popup. Returns true while the
/// clock is still running, so the frame loop knows to come back.
bool step_dwell(float dt)
{
    uint n = live_items();

    // What the pointer is on, resolved to an item once, here, so every branch
    // below reads the same answer.
    uint h = private[0].hover_element;
    uint hover_elem = h < live_elements() ? h : NO_INDEX;
    uint hitem = hover_elem == NO_INDEX ? NO_INDEX : private[0].elements[hover_elem].item;
    private[0].hover_item = hitem;

    // A dock entry's preview: the chart numbers ride the same kind without a
    // capture behind them, and `step_numbers` owns their life.
    bool preview_live = private[0].popup_kind == POPUP_PREVIEW && popup_live()
                        && !numbers_level();

    // While a preview is up, the dwell clock has nothing to do — the fire
    // below is gated on POPUP_IDLE — so `dwell_secs` is free, and it becomes
    // the grace clock. Both branches return, which is what keeps the two uses
    // of that word from ever running into each other.
    if (preview_live)
    {
        uint owner = scratch_index(private[0].preview_owner);
        bool on_entry = hitem != NO_INDEX && hitem == owner;
        bool on_popup = private[0].pointer_target == TARGET_POPUP;
        if (on_entry || on_popup)
        {
            private[0].dwell_secs = 0.0;
            return false;
        }
        private[0].dwell_secs += dt;
        if (private[0].dwell_secs < PREVIEW_GRACE_S)
        {
            // The pointer may simply be between the two surfaces. Keep the
            // preview, and keep asking for frames so the grace can expire.
            return true;
        }
        // The capture feeder is holding a session open for a window nobody is
        // looking at any more.
        act(ACT_PREVIEW_END);
        close_popup();
        private[0].preview_owner = -1.0;
        private[0].dwell_secs = 0.0;
        private[0].dwell_candidate = -1.0;
        return false;
    }

    // Only a dock entry with something to show is worth a dwell.
    float candidate = -1.0;
    if (hitem != NO_INDEX && hitem < n
        && private[0].items[hitem].kind == ITEM_DOCK
        && first_window(hitem) != NO_INDEX)
    {
        candidate = float(hitem);
    }

    if (candidate != private[0].dwell_candidate)
    {
        private[0].dwell_candidate = candidate;
        private[0].dwell_secs = 0.0;
    }
    else if (candidate >= 0.0)
    {
        private[0].dwell_secs += dt;
    }

    bool fire = candidate >= 0.0
                && private[0].dwell_secs >= HOVER_DWELL_S
                && private[0].popup_state == POPUP_IDLE;
    if (fire)
    {
        uint entry = uint(candidate + 0.5);
        uint win = first_window(entry);
        act_window(ACT_PREVIEW_BEGIN, private[0].items[win]);
        // The anchor is the hovered element's own rect — `hover_elem` *is* the
        // entry, since that is where `hitem` came from.
        request_popup(POPUP_PREVIEW, private[0].elements[hover_elem].rect,
                      private[0].items[entry].id,
                      preview_want(entry, POPUP_PAD * scale_px()));
        private[0].preview_owner = candidate;
    }

    return candidate >= 0.0 && private[0].dwell_secs < HOVER_DWELL_S;
}

// ---------------------------------------------------------------------------
// Springs.
// ---------------------------------------------------------------------------

/// One critically damped spring step, semi-implicit (symplectic) Euler:
/// velocity first, then value with the *new* velocity. `sv` is (value,
/// velocity); the return is the stepped pair, parked exactly on the target
/// once the motion stops being visible so an idle bar stops asking for frames.
float2 spring_step(float2 sv, float target, float dt)
{
    float k = SPRING_STIFFNESS;
    float c = 2.0 * SPRING_DAMPING_RATIO * sqrt(k);
    sv.y += (-k * (sv.x - target) - c * sv.y) * dt;
    sv.x += sv.y * dt;
    if (abs(sv.x - target) < SETTLE_EPS && abs(sv.y) < SETTLE_VEL_EPS)
    {
        sv = float2(target, 0.0);
    }
    return sv;
}

// ---------------------------------------------------------------------------
// The wake. The slab is the floor of a shallow pool: a water surface above it
// carries the waves the pointer's motion makes, parallel light from straight
// above is refracted by that surface, and what the slab shows is the caustic
// the surface casts on its floor — a bright net where the refraction folds,
// pale cores, and warm and cool fringes on the flanks, because each colour
// bends by its own index. `tools/logo/caustic.py` traces the same physics
// offline, ray by ray; here it is per pixel, from the surface's curvature.
//
// The surface is a sum of WAKE_SOURCES expanding wave trains, a ring in
// `Private.wake`: the pointer drops one each WAKE_SPACING of travel along the
// bar, as tall as its speed asks, and a click drops one WAKE_CLICK tall. A
// source runs out at WAKE_SPEED and dies, smoothly, WAKE_SETTLE_S after it
// was dropped. So a pointer that rests excites nothing, the last source dies
// within WAKE_SETTLE_S of the last motion, `step_wake` reports nothing moving
// and the bar asks for no frames (docs/architecture.md, *The frame*).
//
// A source takes the slot of the oldest, and is never dropped sooner than
// WAKE_SETTLE_S / WAKE_SOURCES after the last, so the slot it takes is
// already empty; only a click may cut an old train short.
// ---------------------------------------------------------------------------

/// Water's index for green light, and red's and blue's as offsets from it
/// (water runs about 1.331 to 1.337 across the visible range);
/// WAKE_DISPERSION scales the offsets.
static const float WAKE_IOR = 1.333;
static const float3 WAKE_IOR_SPREAD = float3(-0.002, 0.0, 0.004);
/// A train's Gaussian envelope, in wavelengths: about two crests and troughs.
static const float WAKE_TRAIN = 1.3;
/// Seconds a new source takes to rise, so a train is never born as a point.
static const float WAKE_RISE_S = 0.06;
/// Logical px of pointer travel between sources.
static const float WAKE_SPACING = 10.0;
/// A source shorter than this, as a fraction of WAKE_HEIGHT, is not dropped:
/// a hand that trembles on the bar excites nothing.
static const float WAKE_MIN_HEIGHT = 0.05;
/// The tone map: the floor's light as a multiple of flat water's, capped at
/// WAKE_IMAX (at the fold itself it is unbounded), then a soft shoulder whose
/// gain and power keep the glow dark and the net bright.
static const float WAKE_IMAX = 6.0;
static const float WAKE_EXPOSURE = 0.9;
static const float WAKE_CONTRAST = 1.3;

/// Drop a source at bar px `at`, `height` times WAKE_HEIGHT tall.
void wake_drop(float2 at, float height)
{
    uint i = min(private[0].wake_next, WAKE_SOURCES - 1u);
    private[0].wake[i] = float4(at, 0.0, height);
    private[0].wake_next = (i + 1u) % WAKE_SOURCES;
}

/// A click drops a source of its own, at the click.
void wake_click(float2 at)
{
    if (WAKE_ALPHA > 0.0)
    {
        wake_drop(at, WAKE_CLICK);
        private[0].wake_track.w = 0.0;
    }
}

/// Age the sources, drop one if the pointer has travelled far enough, and say
/// when the wake stirs and when it is still. True while any source lives.
bool step_wake(float dt)
{
    float s = scale_px();
    float settle = max(WAKE_SETTLE_S, 0.05);

    // `wake_track`: the pointer as the wake last saw it (x < 0: not on the
    // bar), the px it has travelled since the last source, and the seconds.
    float4 t = private[0].wake_track;
    t.w = min(t.w + dt, 60.0);
    bool on_bar = private[0].pointer_target == TARGET_BAR && private[0].pointer.x >= 0.0;
    bool drop = false;
    float height = 0.0;
    if (!on_bar)
    {
        t.xyz = float3(-1.0, -1.0, 0.0);
    }
    else if (t.x < 0.0)
    {
        // Arriving is not moving: the first position only starts the count.
        t.xyz = float3(private[0].pointer, 0.0);
    }
    else
    {
        float2 p = private[0].pointer;
        t.z += length(p - t.xy);
        t.xy = p;
        if (t.z >= WAKE_SPACING * s && t.w >= settle / float(WAKE_SOURCES))
        {
            // The mean speed since the last source, logical px/s. After a
            // rest it reads low, so the first source after one is gentle.
            float speed = t.z / (max(t.w, 1e-3) * s);
            height = saturate(speed / max(WAKE_FULL_SPEED, 1.0));
            drop = height >= WAKE_MIN_HEIGHT && WAKE_ALPHA > 0.0;
            t.z = 0.0;
            t.w = 0.0;
        }
    }
    private[0].wake_track = t;

    bool alive = false;
    for (uint i = 0u; i < WAKE_SOURCES; ++i)
    {
        float4 w = private[0].wake[i];
        if (w.w <= 0.0)
        {
            continue;
        }
        w.z += dt;
        if (w.z >= settle)
        {
            w = float4(0.0, 0.0, 0.0, 0.0);
        }
        else
        {
            alive = true;
        }
        private[0].wake[i] = w;
    }
    if (drop)
    {
        wake_drop(private[0].pointer, height);
        alive = true;
    }

    uint stirred = alive ? 1u : 0u;
    if (stirred != private[0].said_wake)
    {
        private[0].said_wake = stirred;
        if (alive)
        {
            witness0(WITNESS_INFO, getStringHash("wake: stirred"));
        }
        else
        {
            Args a = args_new();
            argf(a, t.w);
            witness(WITNESS_INFO, getStringHash("wake: still, {0:f} s after its last source"), a);
        }
    }
    return alive;
}

/// The wake's surface at bar px `q`: its slope `grad` and its curvature
/// `hess` (the Hessian's xx, yy and xy), summed over the live sources.
///
/// A source is a radial train `h(r) = a · env(u) · cos(k·u)`, u = r − R, the
/// envelope a Gaussian and R the distance it has run. With h' and h'' its
/// radial derivatives and n the unit vector from the source, the slope is
/// h'·n and the Hessian h''·n nᵀ + (h'/r)·(I − n nᵀ): the radial curvature,
/// and the ring's own bend. r is kept off zero, where the train has not
/// risen yet anyway (WAKE_RISE_S).
void wake_field(float2 q, out float2 grad, out float3 hess)
{
    float s = scale_px();
    float settle = max(WAKE_SETTLE_S, 0.05);
    float lambda = max(WAKE_WAVELENGTH, 1.0) * s;
    float k = 6.2831853 / lambda;
    float sigma = WAKE_TRAIN * lambda;
    float inv_s2 = 1.0 / (sigma * sigma);
    grad = float2(0.0, 0.0);
    hess = float3(0.0, 0.0, 0.0);
    for (uint i = 0u; i < WAKE_SOURCES; ++i)
    {
        float4 w = private[0].wake[i];
        if (w.w <= 0.0)
        {
            continue;
        }
        float run = WAKE_SPEED * s * w.z;
        float2 d = q - w.xy;
        float r = length(d);
        float u = r - run;
        if (abs(u) > 3.0 * sigma)
        {
            continue; // the train's envelope is below e^-9 here
        }
        // Height: the source's share, damped to nothing at WAKE_SETTLE_S,
        // risen over WAKE_RISE_S, and spread thinner as the ring grows.
        float fade = 1.0 - w.z / settle;
        float a = w.w * WAKE_HEIGHT * s * fade * fade * saturate(w.z / WAKE_RISE_S)
                  / sqrt(1.0 + run / lambda);
        float g = exp(-u * u * inv_s2);
        float g1 = -2.0 * u * inv_s2 * g;
        float g2 = (4.0 * u * u * inv_s2 - 2.0) * inv_s2 * g;
        float c = cos(k * u);
        float sn = sin(k * u);
        float h1 = g1 * c - g * k * sn;
        float h2 = g2 * c - 2.0 * g1 * k * sn - g * k * k * c;
        float rr = max(r, 0.5 * sigma);
        float2 n = d / rr;
        float bend = h1 / rr;
        grad += a * h1 * n;
        hess += a * float3(h2 * n.x * n.x + bend * n.y * n.y,
                           h2 * n.y * n.y + bend * n.x * n.x,
                           (h2 - bend) * n.x * n.y);
    }
}

/// The wake's light on the slab colour `col` at bar px `p`.
///
/// A ray through surface point q, refracted, reaches the floor at
/// q + reach·∇h(q), with reach = depth·(1 − 1/n) for near-vertical light, so
/// the light there is 1/|det(I + reach·H)| of flat water's: the reciprocal of
/// the map's Jacobian, unbounded where the map folds. Floor point p is lit
/// from the q with q + reach·∇h(q) = p, found by one fixed-point step from
/// q = p; each channel then takes the determinant with its own index, which
/// moves its fold a little and parts the colours on a fold's flanks.
float4 wake_light(float2 p, float4 col)
{
    if (WAKE_ALPHA <= 0.0 || private[0].said_wake == 0u)
    {
        return col;
    }
    float2 grad;
    float3 hess;
    wake_field(p, grad, hess);
    if (all(grad == 0.0) && all(hess == 0.0))
    {
        return col; // no train reaches this pixel
    }
    float s = scale_px();
    float3 ior = WAKE_IOR + WAKE_DISPERSION * WAKE_IOR_SPREAD;
    float3 reach = max(WAKE_DEPTH, 0.0) * s * (1.0 - 1.0 / ior);
    float2 q = p - reach.y * grad;
    wake_field(q, grad, hess);

    float3 det = (1.0 + reach * hess.x) * (1.0 + reach * hess.y) - reach * reach * hess.z * hess.z;
    float3 light = min(1.0 / max(abs(det), 1e-4), WAKE_IMAX);
    float3 v = pow(1.0 - exp(-WAKE_EXPOSURE * max(light - 1.0, 0.0)), WAKE_CONTRAST);
    float peak = max(v.r, max(v.g, v.b));
    if (peak <= 0.002)
    {
        return col;
    }
    // The accent, turning toward white where the light is hottest, so a
    // crossing reads as light; each channel keeps its own band's share.
    float hot = saturate((dot(light, float3(1.0 / 3.0)) - 2.5) / 3.0);
    float3 tint = lerp(HOVER_RGB, float3(1.0, 1.0, 1.0), 0.5 * hot);
    return ink(tint * v / peak, WAKE_ALPHA * peak, col);
}

/// Step every element's hover and press springs; returns true while any of
/// them is still moving. `Element.anim` is caustic-private (the ABI says so):
/// x = hover 0..1, y = press 0..1, z and w their velocities.
///
/// The two slabs are skipped: neither has a hover chip, so neither has a
/// spring to step.
bool integrate_springs(float dt)
{
    bool moving = false;
    uint n = live_elements();
    for (uint i = 0u; i < n; ++i)
    {
        if (i == ELEM_BAR || i == ELEM_POPUP)
        {
            continue;
        }
        uint flags = private[0].elements[i].flags;
        float4 anim = private[0].elements[i].anim;

        float hover_to = (flags & EFLAG_HOVERED) != 0u ? 1.0 : 0.0;
        float press_to = (flags & EFLAG_PRESSED) != 0u ? 1.0 : 0.0;

        float2 hover = spring_step(float2(anim.x, anim.z), hover_to, dt);
        float2 press = spring_step(float2(anim.y, anim.w), press_to, dt);

        private[0].elements[i].anim = float4(hover.x, press.x, hover.y, press.y);
        moving = moving || hover.x != hover_to || press.x != press_to;
    }
    return moving;
}

// ===========================================================================
// bar_compose — the clock's two strings, written on the GPU.
//
// The embassy publishes the wall clock as two *numbers*:
// `inputs.clock_time` is `hour * 60 + minute` and
// `inputs.clock_date` is `weekday << 16 | month << 8 | day`, both local, both
// straight out of `localtime_r` — which is I/O, and stays the embassy's. Every
// character after that is this file's: the digits from the numbers, the
// weekday and month words copied out of the literal pool by the hashes
// `WEEKDAYS` and `MONTHS` name, in `CLOCK_DATE_ORDER`'s order. So the clock's
// format is a tunable like any other, and the embassy holds no `strftime`
// and no format string.
//
// One thread, walking the block table for BLOCK_COMPOSED blocks. It runs at
// the top of the shape submission, in its own pass *before* `bar_shape`'s:
// `bar_shape` reads the byte length this writes, and two dispatches inside one
// compute pass have no memory dependency between them (the note on
// `Gpu::shape_if_pending` in bar/src/gpu.rs explains it). Two passes
// in one encoder do: the strings buffer is a read-write storage binding, so
// wgpu emits a barrier for it at the second pass's bind.
//
// A composition is never longer than COMPOSE_MAX_BYTES, which is what the CPU
// reserved — zeroed and word-aligned — so this owns whole words of the strings
// buffer and no neighbour's bytes are ever in one of them.
// ===========================================================================

/// One composition under construction: bytes and how many.
struct Composed
{
    uint bytes[COMPOSE_MAX_BYTES];
    uint len;
};

/// An empty one. Written out in full rather than left to a default, because a
/// partially initialised local is exactly the sort of thing that reads
/// differently on two drivers.
Composed compose_new()
{
    Composed c;
    for (uint i = 0u; i < COMPOSE_MAX_BYTES; ++i)
    {
        c.bytes[i] = 0u;
    }
    c.len = 0u;
    return c;
}

void compose_byte(inout Composed c, uint byte)
{
    if (c.len < COMPOSE_MAX_BYTES)
    {
        c.bytes[c.len] = byte & 0xffu;
        c.len += 1u;
    }
}

/// A literal of this caustic's own string table, by `getStringHash` value. A
/// hash the pool cannot answer for appends nothing — the honest answer, and
/// the same answer a missing dock-menu literal gets in `bar_build`.
void compose_literal(inout Composed c, uint hash)
{
    Literal lit = literal(hash);
    // Capped at what the composition could hold anyway. `compose_byte` already
    // refuses to write past the end; the cap is on the *loop*, so a length word
    // that arrived wrong cannot spin one thread of the GPU for four billion
    // iterations with a device-lost at the end of it.
    uint len = min(lit.len, COMPOSE_MAX_BYTES);
    for (uint i = 0u; i < len; ++i)
    {
        compose_byte(c, utf8_byte(lit.off + i));
    }
}

/// `value` in decimal, at least `min_digits` wide, zero-padded.
void compose_number(inout Composed c, uint value, uint min_digits)
{
    uint digits[10];
    uint n = 0u;
    uint rest = value;
    for (uint i = 0u; i < 10u; ++i)
    {
        digits[i] = 0u;
    }
    while (rest > 0u && n < 10u)
    {
        digits[n] = rest % 10u;
        rest /= 10u;
        n += 1u;
    }
    while (n < max(min_digits, 1u) && n < 10u)
    {
        digits[n] = 0u;
        n += 1u;
    }
    for (uint i = n; i > 0u; --i)
    {
        compose_byte(c, 0x30u + digits[i - 1u]);
    }
}

/// `HH:MM` from `hour * 60 + minute`.
Composed compose_clock_time(uint minutes)
{
    Composed c = compose_new();
    compose_number(c, (minutes / 60u) % 24u, 2u);
    compose_byte(c, 0x3au); // ':'
    compose_number(c, minutes % 60u, 2u);
    return c;
}

/// The date, in CLOCK_DATE_ORDER's order and in the tunables' own words.
///
/// The separator is one space *between parts that said something*: a token
/// this file has no arm for, or a weekday the pool has no literal for, leaves
/// no gap behind it rather than a double space.
Composed compose_clock_date(uint packed)
{
    uint weekday = (packed >> 16) & 0xffu;
    uint month = (packed >> 8) & 0xffu;
    uint day = packed & 0xffu;

    Composed c = compose_new();
    for (uint k = 0u; k < uint(CLOCK_DATE_ORDER.getCount()); ++k)
    {
        uint before = c.len;
        if (before > 0u)
        {
            compose_byte(c, 0x20u); // ' '
        }
        uint after_space = c.len;
        switch (CLOCK_DATE_ORDER[k])
        {
        case DATE_WEEKDAY:
            if (weekday < 7u)
            {
                compose_literal(c, WEEKDAYS[weekday]);
            }
            break;
        case DATE_DAY:
            if (day >= 1u && day <= 31u)
            {
                compose_number(c, day, 1u);
            }
            break;
        case DATE_MONTH:
            if (month >= 1u && month <= 12u)
            {
                compose_literal(c, MONTHS[month - 1u]);
            }
            break;
        default:
            break;
        }
        if (c.len == after_space)
        {
            c.len = before; // the part said nothing; take the space back
        }
    }
    return c;
}

// ---------------------------------------------------------------------------
// The chart numbers' values, composed beside the clock's:
// a sample in the samples body's raw units becomes digits, at most one
// decimal, and the unit's literal from the pool. The rules, and the words,
// are the tunables' (`CHART_NAMES` and below); the arithmetic is here.
// ---------------------------------------------------------------------------

/// One number as a line shows it: the value already rounded to what will be
/// printed, how many decimals it is printed with (0 or 1), the unit's literal
/// (0 for none), or a gap.
struct ChartReading
{
    float value;
    uint decimals;
    uint unit;
    bool gap;
};

/// The newest sample of series `k` of chart `c`: the last column of the
/// valid tail, or -1 (a gap) when there is none.
float chart_latest(uint c, uint k)
{
    uint sc = live_samples();
    return sc == 0u ? -1.0 : chart_sample(c, k, sc - 1u);
}

/// `v` rounded to `decimals` (0 or 1) and capped where the digits would
/// outgrow `compose_number`'s ten.
float chart_round(float v, uint decimals)
{
    // floor(x + 0.5), not round(): GLSL.std.450 Round leaves an exact .5 to
    // the driver, and 23.5 % must read the same on RADV and lavapipe.
    float r = decimals == 0u ? floor(v + 0.5) : floor(v * 10.0 + 0.5) / 10.0;
    return clamp(r, 0.0, 999999999.0);
}

/// What a chart of `kind` whose row divides by `ceiling` shows for sample `v`.
///
/// - CPU, MEM, GPU: an integer percent of the ceiling — `caustics-sysmon` sends
///   the CPU and memory as a fraction of 1 and the GPU as a percent of 100,
///   and the ceiling is what makes both a percent (a third party with no
///   ceiling is taken as a fraction).
/// - TEMP: whole degrees Celsius, as the sensor's °C arrive.
/// - NET, DISK: bytes per second with a binary prefix. The prefix climbs while
///   the number as it would print is 1024 or more, so 1023.6 B/s is `1.0
///   KiB/s` and never `1024 B/s`; one decimal below 10 under a prefix, never
///   for plain bytes (`9.5 KiB/s`, `12 MiB/s`, `512 B/s`).
/// - anything else: the raw number, one decimal below 10, no unit.
ChartReading chart_reading(uint kind, float ceiling, float v)
{
    ChartReading r;
    r.value = 0.0;
    r.decimals = 0u;
    r.unit = 0u;
    r.gap = !(v >= 0.0); // negative is the wire's gap, and so is a NaN
    if (r.gap)
    {
        return r;
    }
    switch (kind)
    {
    case CHART_CPU:
    case CHART_MEM:
    case CHART_GPU:
    {
        float pct = ceiling > 0.0 ? v / ceiling * 100.0 : v * 100.0;
        r.value = chart_round(pct, 0u);
        r.unit = CHART_UNIT_PERCENT;
        break;
    }
    case CHART_TEMP:
        r.value = chart_round(v, 0u);
        r.unit = CHART_UNIT_TEMP;
        break;
    case CHART_NET:
    case CHART_DISK:
    {
        float x = v;
        uint prefix = 0u;
        uint top = uint(CHART_RATE_UNITS.getCount()) - 1u;
        // `round(x) >= 1024` for plain bytes; under a prefix the integer form
        // is the one that can reach it, and 9.95 rounds to 10 in either form.
        while (prefix < top && x >= 1023.5)
        {
            x /= 1024.0;
            prefix += 1u;
        }
        r.decimals = (prefix > 0u && x < 9.95) ? 1u : 0u;
        r.value = chart_round(x, r.decimals);
        r.unit = CHART_RATE_UNITS[prefix];
        break;
    }
    default:
        r.decimals = v < 9.95 ? 1u : 0u;
        r.value = chart_round(v, r.decimals);
        break;
    }
    return r;
}

/// One reading: the number and its unit after a space, or CHART_NO_VALUE.
void compose_reading(inout Composed c, ChartReading r)
{
    if (r.gap)
    {
        compose_literal(c, CHART_NO_VALUE);
        return;
    }
    if (r.decimals == 0u)
    {
        compose_number(c, uint(r.value), 1u);
    }
    else
    {
        uint tenths = uint(round(r.value * 10.0));
        compose_number(c, tenths / 10u, 1u);
        compose_byte(c, 0x2eu); // '.'
        compose_byte(c, 0x30u + tenths % 10u);
    }
    if (r.unit != 0u)
    {
        compose_byte(c, 0x20u); // ' '
        compose_literal(c, r.unit);
    }
}

/// The value half of chart `c`'s line: its newest sample in the unit its kind
/// implies, or both series' after their marks for a two-series chart (`↓ 1.2
/// MiB/s ↑ 48 KiB/s`), joined by ` / ` when the kind has no marks.
Composed compose_chart_value(uint c)
{
    ChartRow row = chart_row(c);
    uint kind = row.kind;
    uint2 marks = kind < uint(CHART_SERIES_MARKS.getCount())
        ? CHART_SERIES_MARKS[kind]
        : uint2(0u, 0u);
    uint series = clamp(row.series, 1u, 2u);
    Composed text = compose_new();
    for (uint k = 0u; k < series; ++k)
    {
        uint mark = series > 1u ? (k == 0u ? marks.x : marks.y) : 0u;
        if (k > 0u)
        {
            compose_byte(text, 0x20u);
            if (marks.x == 0u && marks.y == 0u)
            {
                compose_byte(text, 0x2fu); // '/'
                compose_byte(text, 0x20u);
            }
        }
        if (mark != 0u)
        {
            compose_literal(text, mark);
            compose_byte(text, 0x20u);
        }
        compose_reading(text, chart_reading(kind, row.ceiling, chart_latest(c, k)));
    }
    return text;
}

/// The bytes into the block's reserved, word-aligned range. Returns how many
/// landed, which is what goes back into the block's length.
uint compose_store(uint byte_offset, Composed c)
{
    uint words;
    uint stride;
    shapeUtf8.GetDimensions(words, stride);
    uint limit = min(words, MAX_STRING_WORDS);
    uint base = byte_offset >> 2;
    uint len = min(c.len, COMPOSE_MAX_BYTES);
    uint nwords = (len + 3u) / 4u;
    for (uint w = 0u; w < nwords; ++w)
    {
        if (base + w >= limit)
        {
            return w * 4u;
        }
        uint v = 0u;
        for (uint k = 0u; k < 4u; ++k)
        {
            uint i = w * 4u + k;
            v |= (i < len ? c.bytes[i] : 0u) << (k * 8u);
        }
        shapeUtf8[base + w] = v;
    }
    return len;
}

// ===========================================================================
// bar_build — the item table, built here (docs/abi.md, *The private buffer*).
//
// The embassy writes down **facts** — the windows, the app ids it has seen,
// the favourites, each sidecar's rows transcribed, the icon directory — and
// this pass turns them into the `Item` and `TextBlock` tables. Every choice
// is made here: which icon a state wears, which entries the dock has and in
// what order, what a label is sized at, whose menu is open.
//
// One thread, because the work is a walk of a few thousand words with four
// running allocators and no shape a workgroup could take. It runs in its own
// compute pass at the top of the shape submission, before `bar_compose`, and
// only when something marked that submission pending — a fact arrived, the
// clock ticked, or `bar_apply` pushed an `ACT_REBUILD` because the menu level
// moved under it.
// ===========================================================================

/// How many names one build may ask about. The eight-deep list is the
/// caustic's, and so is the preference that fills it: a glyph question stops
/// an icon being drawn at all, where a pixel question is a fallback for a name
/// that already came back "not curves" — so a full list gives a glyph question
/// a pixel question's place rather than starving behind a name nothing will
/// ever answer for. The embassy cannot dedup a want it never saw.
static const uint WANT_LIST = 8u;

/// Words of the image slots this build drew, one bit a slot — what an
/// `ACT_IMAGES_DRAWN` carries (docs/abi.md, *The action ring*). The embassy ages its
/// slots by this mask and nothing else, because only this file knows which
/// pictures are on the bar: a slot the build stopped drawing ages toward
/// reuse, and one still drawn is never evicted under it.
static const uint DRAWN_WORDS = (MAX_IMAGE_SLOTS + 31u) / 32u;

/// The four allocators and the two witnesses of one build. Passed `inout`
/// through everything below, because one thread and no statics is the whole
/// shape of this pass (a `static` in a compute dispatch is re-zeroed every run
/// without saying so).
struct Build
{
    uint items;      // next item index
    uint blocks;     // next block index
    uint glyphs;     // next positioned-glyph slot
    uint reserved;   // reservations taken from the caustic's own byte region
    uint dropped;    // items there was no room for
    uint wants;      // names listed for an ACT_WANT_ICON
    uint want_key[WANT_LIST];
    uint want_src[WANT_LIST];
    uint want_off[WANT_LIST];
    uint want_len[WANT_LIST];
    uint want_req[WANT_LIST];
    uint drawn[DRAWN_WORDS]; // image slots placed in a BLOCK_IMAGE
};

Build build_new()
{
    Build b;
    b.items = 0u;
    b.blocks = 0u;
    b.glyphs = 0u;
    b.reserved = 0u;
    b.dropped = 0u;
    b.wants = 0u;
    for (uint i = 0u; i < WANT_LIST; ++i)
    {
        b.want_key[i] = 0u;
        b.want_src[i] = SRC_STRINGS;
        b.want_off[i] = 0u;
        b.want_len[i] = 0u;
        b.want_req[i] = 0u;
    }
    for (uint w = 0u; w < DRAWN_WORDS; ++w)
    {
        b.drawn[w] = 0u;
    }
    return b;
}

// -- the allocators ---------------------------------------------------------

/// Glyph slots a text block of `bytes` bytes is allocated — **`Batch::push`'s
/// rule**, which is four a byte plus eight, capped at MAX_RUN_GLYPHS.
///
/// Not one slot per byte, because shaping *does* add glyphs: a MultipleSubst
/// adds them, and a SequenceContext whose nested lookup is one turned two
/// input glyphs into five in HarfBuzz's corpus. A block's slots are allocated
/// here and the next block starts where this one was promised to end, so at
/// one a byte a Latin label that multiplied past its byte count would come
/// back SHAPE_RUN_TRUNCATED, which is safe and wrong.
///
/// With this rule truncation is a **bug rather than a size**, and `bar_shape`
/// witnesses the block that hit it.
uint text_slots(uint bytes)
{
    return min(bytes * 4u + 8u, MAX_RUN_GLYPHS);
}

/// Glyph slots a **body** block is allocated: one a byte
/// plus eight. A body is hundreds of bytes where a label is tens, forty rows
/// of them are the bar's whole glyph budget at four a byte, and a body block
/// whose substitutions outgrow this comes back SHAPE_RUN_TRUNCATED and is
/// drawn as a cut — the ellipsis — rather than witnessed as a bug.
uint body_slots(uint bytes)
{
    return min(bytes + 8u, MAX_RUN_GLYPHS);
}

/// The slots a block was allocated, by the rule it was allocated under. The
/// shape pass may fill exactly these: the next block's run starts after them.
uint block_slots(TextBlock blk)
{
    return (blk.bflags & BLOCK_BODY) != 0u ? body_slots(blk.b) : text_slots(blk.b);
}

/// Append an item. Full is counted in `Private.built_dropped`, which
/// `bar_build` witnesses as one `items: table capped` line, and a missing
/// item, never a panic.
uint build_push(inout Build b, Item it)
{
    if (b.items >= MAX_ITEMS)
    {
        b.dropped += 1u;
        return NO_INDEX;
    }
    private[0].items[b.items] = it;
    b.items += 1u;
    return b.items - 1u;
}

/// A run of one of the three buffers as a block: the bytes are already there,
/// put there by whoever wrote the thing that names them — the embassy for a
/// title or a literal, a sidecar for a row's label or an app's name.
///
/// `source` is `SRC_STRINGS`, `SRC_ANSWERS` or a sidecar slot; `bar_shape`
/// hands `shapeRun` the matching buffer (docs/abi.md, *The arenas*).
///
/// The slots come from `text_slots` — four a byte plus eight — and out of
/// blocks or out of slots is NO_INDEX and a missing label, never a truncated
/// string drawn as if it were whole.
uint build_text(inout Build b, uint source, uint off, uint len, uint px, float4 ink, uint hash)
{
    return build_text_as(b, source, off, len, px, ink, hash, 0u);
}

/// `build_text` with the block's `bflags`: BLOCK_BODY allocates `body_slots`
/// rather than `text_slots`.
uint build_text_as(inout Build b, uint source, uint off, uint len, uint px, float4 ink,
                   uint hash, uint bflags)
{
    if (len == 0u || px == 0u)
    {
        return NO_INDEX;
    }
    uint slots = (bflags & BLOCK_BODY) != 0u ? body_slots(len) : text_slots(len);
    if (b.blocks >= MAX_BLOCKS || b.glyphs + slots > MAX_GLYPHS)
    {
        return NO_INDEX;
    }
    TextBlock blk;
    blk.size = float2(0.0, 0.0);
    blk.kind = BLOCK_TEXT;
    blk.flags = 0u;
    blk.a = off;
    blk.b = len;
    blk.first_glyph = b.glyphs;
    blk.glyph_count = 0u;
    blk.color = ink;
    blk.px = float(px);
    blk.hash = hash;
    blk.bflags = bflags;
    blk.source = source;
    private[0].blocks[b.blocks] = blk;
    b.glyphs += slots;
    b.blocks += 1u;
    return b.blocks - 1u;
}

/// One glyph or one image slot, filling a square box of `px`.
uint build_icon(inout Build b, uint kind, uint payload, float px, float4 ink)
{
    if (b.blocks >= MAX_BLOCKS || px <= 0.0)
    {
        return NO_INDEX;
    }
    TextBlock blk;
    blk.size = float2(px, px);
    blk.kind = kind;
    blk.flags = 0u;
    blk.a = payload;
    blk.b = 0u;
    blk.first_glyph = 0u;
    blk.glyph_count = 0u;
    blk.color = ink;
    blk.px = px;
    blk.hash = 0u;
    blk.bflags = 0u;
    blk.source = SRC_STRINGS;
    private[0].blocks[b.blocks] = blk;
    b.blocks += 1u;
    // A picture on the bar: its slot is in use, which the embassy hears once
    // per build (`ACT_IMAGES_DRAWN`) and ages its slots by.
    if (kind == BLOCK_IMAGE && payload < MAX_IMAGE_SLOTS)
    {
        b.drawn[payload >> 5u] |= 1u << (payload & 31u);
    }
    return b.blocks - 1u;
}

/// COMPOSE_MAX_BYTES of the region the embassy zeroed and never writes again,
/// word-aligned by construction, or NO_INDEX when they are all taken.
uint build_reserve(inout Build b)
{
    if (b.reserved >= MAX_COMPOSED)
    {
        return NO_INDEX;
    }
    uint off = facts[0].header.compose_base + b.reserved * COMPOSE_MAX_BYTES;
    b.reserved += 1u;
    return off;
}

/// A block whose bytes `bar_compose` writes: the reservation, and the name of
/// the composition that fills it (docs/abi.md, *The facts*).
uint build_composed(inout Build b, uint px, uint compose)
{
    if (px == 0u || b.blocks >= MAX_BLOCKS)
    {
        return NO_INDEX;
    }
    // The reservation is the whole region's worth, because `bar_compose` has
    // not written its bytes yet and `blk.b` is 0 until it does.
    uint slots = text_slots(COMPOSE_MAX_BYTES);
    if (b.glyphs + slots > MAX_GLYPHS)
    {
        return NO_INDEX;
    }
    uint off = build_reserve(b);
    if (off == NO_INDEX)
    {
        return NO_INDEX;
    }
    TextBlock blk;
    blk.size = float2(0.0, 0.0);
    blk.kind = BLOCK_TEXT;
    blk.flags = 0u;
    blk.a = off;
    blk.b = 0u;
    blk.first_glyph = b.glyphs;
    blk.glyph_count = 0u;
    blk.color = LABEL_INK;
    blk.px = float(px);
    blk.hash = compose;
    blk.bflags = BLOCK_COMPOSED;
    blk.source = SRC_STRINGS;
    private[0].blocks[b.blocks] = blk;
    b.glyphs += slots;
    b.blocks += 1u;
    return b.blocks - 1u;
}

// -- the text a fact carries ------------------------------------------------

/// Whether a byte is ASCII whitespace, for the trim below.
bool build_is_space(uint c)
{
    return c == 0x20u || c == 0x09u || c == 0x0bu || c == 0x0cu;
}

/// The first line of a run of bytes, trimmed: dbusmenu rows and window titles
/// both arrive multi-line on the live bus and a row is one line high. The
/// range shrinks; no byte moves.
void build_first_line(uint source, uint off, uint len, out uint out_off, out uint out_len)
{
    uint start = off;
    uint end = off + len;
    for (uint i = off; i < end; ++i)
    {
        uint c = src_byte(source, i);
        if (c == 0x0au || c == 0x0du)
        {
            end = i;
            break;
        }
    }
    while (start < end && build_is_space(src_byte(source, start)))
    {
        start += 1u;
    }
    while (end > start && build_is_space(src_byte(source, end - 1u)))
    {
        end -= 1u;
    }
    out_off = start;
    out_len = end - start;
}

/// A chip's placeholder: the first character of a name, uppercased, as a text
/// block in the icon's square.
///
/// Two callers, one rule each. A dock entry passes its **app id** with
/// `last_component` set, and wears the initial of the id's last dot-component
/// (`org.mozilla.firefox` → `F`). A tray item passes its **title** — the label
/// of a `RFLAG_LABEL_TITLE` row, in the slot's region of the rows arena — and
/// wears the title's first character. No character at
/// all is `?`, so a chip whose name nothing answers is never empty.
///
/// **ASCII only**, deliberately: only an ASCII letter is uppercased, which
/// affects exactly one thing — the letter a chip
/// wears while nothing has answered for its icon, and only when that initial
/// is non-ASCII. The bytes are this pass's own: it takes a reservation from
/// the caustic's region and writes them, because an uppercased byte is a byte
/// the embassy never wrote.
uint build_letter(inout Build b, uint source, uint off, uint len, bool last_component, uint px)
{
    uint start = off;
    if (last_component)
    {
        for (uint i = off; i < off + len; ++i)
        {
            if (src_byte(source, i) == 0x2eu) // '.'
            {
                start = i + 1u;
            }
        }
    }
    Composed c = compose_new();
    if (start >= off + len)
    {
        compose_byte(c, 0x3fu); // '?'
    }
    else
    {
        uint lead = src_byte(source, start);
        uint bytes = 1u;
        if (lead >= 0xf0u)      { bytes = 4u; }
        else if (lead >= 0xe0u) { bytes = 3u; }
        else if (lead >= 0xc0u) { bytes = 2u; }
        bytes = min(bytes, off + len - start);
        if (bytes == 1u && lead >= 0x61u && lead <= 0x7au)
        {
            lead -= 32u;
        }
        compose_byte(c, lead);
        for (uint k = 1u; k < bytes; ++k)
        {
            compose_byte(c, src_byte(source, start + k));
        }
    }
    uint at = build_reserve(b);
    if (at == NO_INDEX)
    {
        return NO_INDEX;
    }
    uint wrote = compose_store(at, c);
    return build_text(b, SRC_STRINGS, at, wrote, px, ICON_INK, 0u);
}

// -- what this build wants asked ------------------------------------------

/// List a name nothing has answered for, as the `(offset, length, REQ_*,
/// source)` an `ACT_WANT_ICON` carries (docs/abi.md, *The action ring*).
///
/// **Bytes and the buffer they are in.** A name lives in one of three places —
/// a literal in `strings`, a row's `icon` in its slot's region of the rows
/// arena, an `AppBody.icon` in `answers` — and a range alone cannot say which,
/// so the source travels with it and the embassy reads its own copy of that
/// buffer. `key` is this file's private hash of the same bytes, and it is only
/// ever the dedup within one build.
///
/// A glyph question stops an icon being drawn at all; a pixel question is a
/// fallback for a name that already came back "not curves". So when the list
/// is full a glyph question takes a pixel question's place, rather than
/// starving behind a name nothing will ever answer for. Which wants fit is the
/// caustic's to decide, because the embassy cannot dedup a want it never saw;
/// asking twice for the same name costs nothing, since the embassy asks once
/// per name and remembers (docs/abi.md, *Request*).
void build_want(inout Build b, uint key, uint source, uint off, uint len, uint req)
{
    if (len == 0u || req == 0u)
    {
        return;
    }
    for (uint i = 0u; i < b.wants; ++i)
    {
        if (b.want_key[i] == key)
        {
            return;
        }
    }
    if (b.wants < WANT_LIST)
    {
        b.want_key[b.wants] = key;
        b.want_src[b.wants] = source;
        b.want_off[b.wants] = off;
        b.want_len[b.wants] = len;
        b.want_req[b.wants] = req;
        b.wants += 1u;
        return;
    }
    if (req == REQ_ICON)
    {
        return;
    }
    for (uint i = 0u; i < WANT_LIST; ++i)
    {
        if (b.want_req[i] == REQ_ICON)
        {
            b.want_key[i] = key;
            b.want_src[i] = source;
            b.want_off[i] = off;
            b.want_len[i] = len;
            b.want_req[i] = req;
            return;
        }
    }
}

// -- the answers arena, indexed (docs/abi.md, *The arenas*) ----------------------

/// Walk the arena once and write `(hash of the echoed key, byte offset)` for
/// every answer into `private`. Called at the top of `bar_build`.
///
/// The glyph bodies come first and the app bodies from `glyph_bytes`, because
/// a body carries no kind word of its own: where an entry *is* says what it
/// is. Each entry is its head, then its bytes, padded to a word.
void index_answers()
{
    uint live = facts[0].header.answer_bytes;
    uint split = min(facts[0].header.glyph_bytes, live);
    uint count = 0u;
    uint at = 0u;
    // The glyph half: `GlyphBody` then `name_len` bytes.
    while (at + 16u <= split && count < MAX_ANSWERS)
    {
        uint name_len = answer_byte_word(at + 8u);
        uint next = at + 16u + ((name_len + 3u) & ~3u);
        if (name_len == 0u || next > split)
        {
            break;
        }
        private[0].answer_hash[count] = hash_bytes(SRC_ANSWERS, at + 16u, name_len);
        private[0].answer_at[count] = at;
        count += 1u;
        at = next;
    }
    // The app half: `AppBody` then the five fields' bytes in order.
    at = split;
    while (at + 32u <= live && count < MAX_ANSWERS)
    {
        uint app_len = answer_byte_word(at + 4u);
        uint fields = app_len + answer_byte_word(at + 8u) + answer_byte_word(at + 12u)
                      + answer_byte_word(at + 16u) + answer_byte_word(at + 20u);
        uint next = at + 32u + ((fields + 3u) & ~3u);
        if (app_len == 0u || next > live)
        {
            break;
        }
        private[0].answer_hash[count] = hash_bytes(SRC_ANSWERS, at + 32u, app_len);
        private[0].answer_at[count] = at;
        count += 1u;
        at = next;
    }
    private[0].answer_count = count;
}

/// Where the answer for `hash` begins, or NO_INDEX. `glyph` says which half to
/// look in, because an icon name and an app id are different questions and a
/// hash could in principle answer both.
uint answer_of(uint hash, bool glyph)
{
    uint split = facts[0].header.glyph_bytes;
    uint n = min(private[0].answer_count, MAX_ANSWERS);
    for (uint i = 0u; i < n; ++i)
    {
        if (private[0].answer_hash[i] != hash)
        {
            continue;
        }
        uint at = private[0].answer_at[i];
        if ((at < split) == glyph)
        {
            return at;
        }
    }
    return NO_INDEX;
}

// -- the stitch -------------------------------------------------------------

/// One themed icon name — a literal's bytes, a row's `icon` range, an
/// `AppBody.icon` — as a block: curves if `caustics-glyphs` has them, pixels if
/// `caustics-apps` does, **nothing for one round trip** while an answer is
/// pending, and `stop = false` when no theme has it at all, which is what
/// makes a preference list fall through to its next name.
///
/// A list is a preference order, and settling on a later candidate before the
/// earlier one has answered would take a slot for the wrong picture — which is
/// what `stop` is for.
uint build_stitch_named(inout Build b, uint source, uint off, uint len, float px, out bool stop)
{
    stop = true;
    if (len == 0u)
    {
        return NO_INDEX;
    }
    uint key = hash_bytes(source, off, len);
    uint at = answer_of(key, true);
    if (at == NO_INDEX)
    {
        // Never asked, or asked and not yet answered. The embassy asks once
        // per name and remembers (docs/abi.md, *Request*), so repeating this costs nothing.
        build_want(b, key, source, off, len, REQ_GLYPH);
        return NO_INDEX;
    }
    uint flags = answer_byte_word(at + 4u);
    if ((flags & GLYPH_MISSING) == 0u)
    {
        return build_icon(b, BLOCK_GLYPH, answer_byte_word(at), px, ICON_INK);
    }
    // Not curves. The pixels may already be in a slot under this name, which
    // is what a `REQ_ICON` answer is.
    uint slot = image_by_name(source, off, len);
    if (slot != NO_INDEX)
    {
        return build_icon(b, BLOCK_IMAGE, slot, px, IMAGE_INK);
    }
    build_want(b, key, source, off, len, REQ_ICON);
    stop = false;
    return NO_INDEX;
}

/// The same, for a name the *tunables* gave: its bytes are in the literal
/// pool.
uint build_stitch_literal(inout Build b, uint hash, float px, out bool stop)
{
    Literal lit = literal(hash);
    return build_stitch_named(b, SRC_STRINGS, lit.off, lit.len, px, stop);
}

// -- what the item table is -------------------------------------------------

/// The overview button: the first of `OVERVIEW_ICONS` anything can draw, and
/// `OVERVIEW_GLYPH`'s one character when nothing can.
void build_overview(inout Build b, uint icon_px_u)
{
    uint icon = NO_INDEX;
    bool stop = false;
    for (uint i = 0u; i < uint(OVERVIEW_ICONS.getCount()) && icon == NO_INDEX && !stop; ++i)
    {
        icon = build_stitch_literal(b, OVERVIEW_ICONS[i], float(icon_px_u), stop);
    }
    if (icon == NO_INDEX)
    {
        Literal lit = literal(OVERVIEW_GLYPH);
        icon = build_text(b, SRC_STRINGS, lit.off, lit.len, icon_px_u, ICON_INK,
                          OVERVIEW_GLYPH);
    }
    Item it = item_none();
    it.kind = ITEM_OVERVIEW;
    it.icon_block = icon;
    it.id = id_tagged(ID_OVERVIEW, 0u, 0u);
    build_push(b, it);
}

/// The clock: two reservations `bar_compose` fills, both always taken so that
/// CLOCK_SHOW_DATE stays a width-and-paint decision needing no rebuild.
/// Returns its index, which the calendar's cells hang off.
uint build_clock(inout Build b, uint px)
{
    uint block = build_composed(b, px, COMPOSE_CLOCK_TIME);
    uint aux = build_composed(b, px, COMPOSE_CLOCK_DATE);
    Item it = item_none();
    it.kind = ITEM_CLOCK;
    it.block = block;
    it.aux_block = aux;
    it.id = id_tagged(ID_CLOCK, 0u, 0u);
    return build_push(b, it);
}

/// How many bytes the UTF-8 character led by `lead` takes.
uint utf8_char_len(uint lead)
{
    if (lead >= 0xf0u) { return 4u; }
    if (lead >= 0xe0u) { return 3u; }
    if (lead >= 0xc0u) { return 2u; }
    return 1u;
}

/// One calendar cell under the clock.
void build_cal_cell(inout Build b, uint parent, uint block, uint part, uint payload, uint flags)
{
    Item it = item_none();
    it.kind = ITEM_CAL_CELL;
    it.parent = parent;
    it.block = block;
    it.id = id_tagged(ID_CALENDAR, part, payload);
    it.flags = flags;
    build_push(b, it);
}

/// The calendar's cells, while the clock is the menu level: the title, seven
/// weekday initials, one cell per day.
///
/// **Composed here and not in `bar_compose`**, as a tray chip's letter is:
/// the cells exist only on a build for the clock's level, and a minute tick
/// runs this pass too, so a day that turns over under an open calendar moves
/// the mark with it. Two reservations for all of it: the title (the `MONTHS`
/// word and the year), and every day's digits end to end in one run
/// (`12345678910...31`, 53 bytes of the 96), each day's block a range of it. A
/// weekday initial is the first character of its `WEEKDAYS` literal, read
/// straight out of the pool: no bytes to write at all.
///
/// The weekday row is IFLAG_DISABLED: it is drawn at DISABLED_ALPHA, which is
/// what a column header is, and it takes no click (no cell does). Today's day
/// carries IFLAG_ATTENTION, the wash an attention row wears.
void build_calendar(inout Build b, uint parent, uint px)
{
    uint month = (inputs.clock_date >> 8) & 0xffu;
    uint today = inputs.clock_date & 0xffu;
    uint year = inputs.clock_month >> 16;

    // The title.
    Composed title = compose_new();
    if (month >= 1u && month <= 12u)
    {
        compose_literal(title, MONTHS[month - 1u]);
        compose_byte(title, 0x20u); // ' '
    }
    compose_number(title, year, 1u);
    uint at = build_reserve(b);
    if (at != NO_INDEX)
    {
        uint wrote = compose_store(at, title);
        build_cal_cell(b, parent, build_text(b, SRC_STRINGS, at, wrote, px, LABEL_INK, 0u),
                       CAL_TITLE, (year << 8) | month, 0u);
    }

    // The weekday initials, in column order.
    uint start = CAL_WEEK_STARTS_MONDAY ? 1u : 0u;
    for (uint c = 0u; c < 7u; ++c)
    {
        uint weekday = (c + start) % 7u;
        Literal lit = literal(WEEKDAYS[weekday]);
        uint len = lit.len == 0u ? 0u : min(utf8_char_len(utf8_byte(lit.off)), lit.len);
        build_cal_cell(b, parent, build_text(b, SRC_STRINGS, lit.off, len, px, LABEL_INK, 0u),
                       CAL_WEEKDAY, weekday, IFLAG_DISABLED);
    }

    // The days: one run of digits, a range each.
    uint days = calendar_days();
    Composed digits = compose_new();
    for (uint d = 1u; d <= days; ++d)
    {
        compose_number(digits, d, 1u);
    }
    uint run = build_reserve(b);
    if (run == NO_INDEX)
    {
        return;
    }
    uint wrote = compose_store(run, digits);
    for (uint d = 1u; d <= days; ++d)
    {
        // Days 1 to 9 are one byte each from the run's start; 10 on are two.
        uint off = d < 10u ? d - 1u : 9u + (d - 10u) * 2u;
        uint len = d < 10u ? 1u : 2u;
        uint block = off + len <= wrote
            ? build_text(b, SRC_STRINGS, run + off, len, px, LABEL_INK, 0u)
            : NO_INDEX;
        build_cal_cell(b, parent, block, CAL_DAY, d, d == today ? IFLAG_ATTENTION : 0u);
    }
}

/// The caffeine chip, or nothing at all: a state whose icons no theme has
/// would be an empty square, and an empty square is worse than no chip.
void build_caffeine(inout Build b, uint icon_px_u)
{
    uint live = inputs.caffeine;
    if (live == CAFFEINE_ABSENT)
    {
        return;
    }
    uint icon = NO_INDEX;
    bool stop = false;
    if (live == CAFFEINE_ON)
    {
        for (uint i = 0u; i < uint(CAFFEINE_ON_ICONS.getCount()) && icon == NO_INDEX && !stop; ++i)
        {
            icon = build_stitch_literal(b, CAFFEINE_ON_ICONS[i], float(icon_px_u), stop);
        }
    }
    else
    {
        for (uint i = 0u; i < uint(CAFFEINE_OFF_ICONS.getCount()) && icon == NO_INDEX && !stop; ++i)
        {
            icon = build_stitch_literal(b, CAFFEINE_OFF_ICONS[i], float(icon_px_u), stop);
        }
    }
    if (icon == NO_INDEX)
    {
        return;
    }
    Item it = item_none();
    it.kind = ITEM_APPLET;
    it.icon_block = icon;
    it.id = id_tagged(ID_CAFFEINE, 0u, 0u);
    it.a = APPLET_CAFFEINE;
    it.f = -1.0;
    build_push(b, it);
}

/// One dock entry, and its item index (docs/abi.md, *The facts*, *The arenas*).
///
/// The `AppFact` is the app id and one flag; **everything else about the entry
/// is the apps sidecar's answer**, joined here by the id's own bytes. So this
/// is where the two halves meet: the label is the answer's `Name=` (or the app
/// id, which is also what an `APP_MISSING` answer echoes), and the icon is
/// either the answer's symbolic `Icon=` — a themed name like any other, glyphs
/// first and pixels after — or the pixmap the same sidecar keyed by the app
/// id, for which there is no second sidecar to fall through to.
///
/// No answer at all is one `REQ_APP` want and the entry's first letter, which
/// is what a bar with no `caustics-apps` beside it looks like.
uint build_entry(inout Build b, uint app_row, uint favourite, uint text_px, uint icon_px_u)
{
    AppFact app = facts[0].apps[app_row];
    uint at = answer_of(hash_bytes(SRC_STRINGS, app.id_off, app.id_len), false);

    uint icon = NO_INDEX;
    uint label_src = SRC_STRINGS;
    uint label_off = app.id_off;
    uint label_len = app.id_len;
    if (at == NO_INDEX)
    {
        build_want(b, hash_bytes(SRC_STRINGS, app.id_off, app.id_len),
                   SRC_STRINGS, app.id_off, app.id_len, REQ_APP);
    }
    else
    {
        uint flags = answer_byte_word(at);
        uint app_len = answer_byte_word(at + 4u);
        uint name_len = answer_byte_word(at + 8u);
        uint icon_len = answer_byte_word(at + 12u);
        uint name_at = at + 32u + app_len;
        uint icon_at = name_at + name_len;
        if (name_len > 0u)
        {
            label_src = SRC_ANSWERS;
            label_off = name_at;
            label_len = name_len;
        }
        bool stop = true;
        if ((flags & APP_ICON_SYMBOLIC) != 0u && icon_len > 0u)
        {
            icon = build_stitch_named(b, SRC_ANSWERS, icon_at, icon_len,
                                      float(icon_px_u), stop);
        }
        else
        {
            uint slot = image_by_name(SRC_STRINGS, app.id_off, app.id_len);
            if (slot != NO_INDEX)
            {
                icon = build_icon(b, BLOCK_IMAGE, slot, float(icon_px_u), IMAGE_INK);
            }
        }
    }
    if (icon == NO_INDEX)
    {
        icon = build_letter(b, SRC_STRINGS, app.id_off, app.id_len, true, icon_px_u);
    }
    uint block = build_text(b, label_src, label_off, label_len, text_px, LABEL_INK, 0u);
    Item it = item_none();
    it.kind = ITEM_DOCK;
    it.block = block;
    it.icon_block = icon;
    // The entry's name, as this file names it: its own hash of the app id's
    // bytes (docs/abi.md, *The arenas*). No number here crosses the buffer — the
    // level the menu is held at is this id, and a click carries the range.
    it.id = id_tagged(ID_ENTRY, 0u, hash_bytes(SRC_STRINGS, app.id_off, app.id_len));
    it.a = favourite;
    // The operands every action about this entry carries: the app id's own
    // bytes, which an `ACT_SPAWN`, `ACT_PIN` or `ACT_UNPIN` hands the embassy
    // whole (docs/abi.md, *The action ring*), and which the `layout:` line names it by.
    it.arg0 = app.id_off;
    it.arg1 = app.id_len;
    return build_push(b, it);
}

/// One chart per row of the samples body: the row's index, and that is the
/// whole item. Everything else a chart draws is read there. Returns the first
/// chart's item index, or NO_INDEX — the charts are consecutive, so row `r`
/// is that plus `r`.
uint build_charts(inout Build b)
{
    uint n = min(samples[0].chart_count, MAX_CHARTS);
    uint first = NO_INDEX;
    for (uint i = 0u; i < n; ++i)
    {
        Item it = item_none();
        it.kind = ITEM_CHART;
        it.id = id_tagged(ID_CHART, 0u, i);
        it.a = i;
        uint index = build_push(b, it);
        first = i == 0u ? index : first;
    }
    return first;
}

/// The chart numbers' lines, while a chart is the level:
/// one per row of the samples body, in its order — the strip's own — each the
/// chart's name, a literal of the pool by `CHART_NAMES`, and its value
/// composed by `compose_chart_value` into a reservation of this pass's own,
/// as the calendar's cells are. Composed **here** and not in `bar_compose`
/// because the lines exist only on a build for a chart's level, and every
/// samples tick under an open popup is a build (`bar_apply`'s `ACT_REBUILD`).
///
/// A line hangs off the level's chart; which line is marked is read off the
/// level where it is drawn, so the mark is not a build's.
void build_numbers(inout Build b, uint parent, uint px)
{
    uint n = min(samples[0].chart_count, MAX_CHARTS);
    for (uint c = 0u; c < n; ++c)
    {
        uint kind = samples[0].charts[c].kind;
        uint name_hash = CHART_NAMES[kind < uint(CHART_NAMES.getCount()) ? kind : CHART_OTHER];
        Literal lit = literal(name_hash);
        uint name = build_text(b, SRC_STRINGS, lit.off, lit.len, px, LABEL_INK, name_hash);
        uint value = NO_INDEX;
        uint at = build_reserve(b);
        if (at != NO_INDEX)
        {
            uint wrote = compose_store(at, compose_chart_value(c));
            value = build_text(b, SRC_STRINGS, at, wrote, px, LABEL_INK, 0u);
        }
        Item it = item_none();
        it.kind = ITEM_CHART_LINE;
        it.parent = parent;
        it.block = name;
        it.aux_block = value;
        it.id = id_tagged(ID_NUMBERS, 0u, c);
        it.a = c;
        build_push(b, it);
    }
}

/// A row's icon: a **name** whose bytes are in the slot's own region of the
/// rows arena (docs/abi.md, *Pixmap icons*) — a themed one, or a `Pixmap` key the sidecar sent
/// before the frame that names it (docs/abi.md, *Pixmap*). Either way the stitch resolves it:
/// curves if a theme has them, else the slot the key's pixels landed in.
uint build_row_icon(inout Build b, uint slot, Row row, uint icon_px_u)
{
    if (row.icon_len == 0u)
    {
        return NO_INDEX;
    }
    bool stop = true;
    return build_stitch_named(b, slot, rows_blob(slot) + row.icon_off, row.icon_len,
                              float(icon_px_u), stop);
}

/// A row's label, first line only, at the size its item kind asks for. The
/// bytes are the sidecar's own, in its region of the rows arena.
uint build_row_label(inout Build b, uint slot, Row row, uint px)
{
    if (row.label_len == 0u)
    {
        return NO_INDEX;
    }
    uint off;
    uint len;
    build_first_line(slot, rows_blob(slot) + row.label_off, row.label_len, off, len);
    return build_text(b, slot, off, len, px, LABEL_INK, 0u);
}

/// Whether a byte ends a line of a body: LF, or the CR of a CRLF.
bool build_is_newline(uint c)
{
    return c == 0x0au || c == 0x0du;
}

/// A menu row's **body** as blocks: one per paragraph — a
/// run between the body's own newlines, its leading and trailing spaces off,
/// an empty one skipped — and a paragraph longer than BODY_CHUNK_BYTES in
/// chunks, each cut after its last space (a word with no space in reach at a
/// UTF-8 boundary instead), so no chunk boundary falls inside a word that
/// could have been shaped whole. Newlines are never shaped: they are where a
/// paragraph ends, and a `.notdef` for U+000A would be a cover question about
/// nothing.
///
/// At most BODY_SHAPE_BYTES are shaped and MAX_BODY_BLOCKS built; `meta` is
/// the item's `body_blocks` word — the count, and BODY_MORE when bytes that
/// are not whitespace were left unshaped. The blocks are consecutive, which
/// is what lets the wrap and the draw walk a paragraph across its chunks.
/// Returns the first, or NO_INDEX for no body.
uint build_body(inout Build b, uint slot, Row row, uint px, out uint meta)
{
    meta = 0u;
    uint len = row_body_len(row);
    if (len == 0u || px == 0u)
    {
        return NO_INDEX;
    }
    uint pos = rows_blob(slot) + row_body_off(row);
    uint end = pos + len;
    // Trailing whitespace is not "more": a body ending in a newline is whole.
    for (uint k = 0u; k < 64u && end > pos; ++k)
    {
        uint c = src_byte(slot, end - 1u);
        if (!build_is_space(c) && !build_is_newline(c))
        {
            break;
        }
        end -= 1u;
    }
    // Every byte looked at counts here, newlines and skipped spaces too, so a
    // body of nothing but newlines is a bounded walk.
    uint scan_end = pos + min(end - pos, 2u * BODY_SHAPE_BYTES);
    uint budget = BODY_SHAPE_BYTES;
    uint first = NO_INDEX;
    uint count = 0u;
    bool more = false;
    bool paragraph = true;
    for (uint step = 0u; step < 4u * BODY_SHAPE_BYTES && pos < scan_end; ++step)
    {
        if (count >= MAX_BODY_BLOCKS || budget == 0u)
        {
            break;
        }
        uint c = src_byte(slot, pos);
        if (c == 0x0au)
        {
            paragraph = true;
            pos += 1u;
            continue;
        }
        if (c == 0x0du || (paragraph && build_is_space(c)))
        {
            pos += 1u;
            continue;
        }
        // To the newline, the chunk's length, the budget or the scan's end.
        uint cap = min(min(scan_end, pos + BODY_CHUNK_BYTES), pos + budget);
        uint stop = pos;
        while (stop < cap && !build_is_newline(src_byte(slot, stop)))
        {
            stop += 1u;
        }
        bool whole = stop >= end || build_is_newline(src_byte(slot, stop));
        if (whole)
        {
            while (stop > pos + 1u && build_is_space(src_byte(slot, stop - 1u)))
            {
                stop -= 1u;
            }
        }
        else
        {
            // After the chunk's last space; else at a character's first byte.
            uint cut = stop;
            while (cut > pos + 1u && src_byte(slot, cut - 1u) != 0x20u)
            {
                cut -= 1u;
            }
            if (cut > pos + 1u)
            {
                stop = cut;
            }
            else
            {
                while (stop > pos + 1u && (src_byte(slot, stop) & 0xc0u) == 0x80u)
                {
                    stop -= 1u;
                }
            }
        }
        uint flags = BLOCK_BODY | (paragraph ? BLOCK_PARAGRAPH : 0u);
        uint blk = build_text_as(b, slot, pos, stop - pos, px, BODY_INK, 0u, flags);
        if (blk == NO_INDEX)
        {
            more = true;
            break;
        }
        if (first == NO_INDEX)
        {
            first = blk;
        }
        count += 1u;
        paragraph = false;
        budget -= min(budget, stop - pos);
        pos = stop;
    }
    // Whatever was not reached, unless it is only the newlines and spaces
    // between paragraphs.
    for (uint k = 0u; k < 64u && !more && pos < end; ++k)
    {
        uint c = src_byte(slot, pos);
        more = !build_is_space(c) && !build_is_newline(c);
        pos += 1u;
    }
    more = more || pos < end;
    meta = count | (more ? BODY_MORE : 0u);
    return first;
}

/// The `IFLAG_*` of an applet chip, from the three `RFLAG_*` it carries (docs/abi.md, *Rows*).
uint build_applet_flags(uint flags)
{
    uint out_flags = 0u;
    if ((flags & RFLAG_HAS_MENU) != 0u)  { out_flags |= IFLAG_HAS_MENU; }
    if ((flags & RFLAG_ATTENTION) != 0u) { out_flags |= IFLAG_ATTENTION; }
    if ((flags & RFLAG_DISABLED) != 0u)  { out_flags |= IFLAG_DISABLED; }
    return out_flags;
}

/// A menu row's or a slider's flags, from its role and its `RFLAG_*`. A flag a
/// kind has no meaning for does not travel: the flag table in docs/abi.md,
/// *Rows*, is the contract.
uint build_row_flags(uint role, uint flags)
{
    uint disabled = (flags & RFLAG_DISABLED) != 0u ? IFLAG_DISABLED : 0u;
    if (role == ROW_SLIDER)
    {
        return disabled;
    }
    uint out_flags = disabled;
    if (role == ROW_SEPARATOR)           { out_flags |= IFLAG_SEPARATOR; }
    if ((flags & RFLAG_CHECKABLE) != 0u) { out_flags |= IFLAG_CHECKABLE; }
    // dbusmenu's `toggle-type`, one wire further out: a radio row is checkable
    // *and* radio, so a caustic that draws neither still draws a check box.
    if ((flags & RFLAG_RADIO) != 0u)     { out_flags |= IFLAG_CHECKABLE | IFLAG_RADIO; }
    if ((flags & RFLAG_CHECKED) != 0u)   { out_flags |= IFLAG_CHECKED; }
    if ((flags & RFLAG_SUBMENU) != 0u)   { out_flags |= IFLAG_SUBMENU; }
    // Drawn as nothing in a menu; it is what a toast's rows are picked by.
    if ((flags & RFLAG_ATTENTION) != 0u) { out_flags |= IFLAG_ATTENTION; }
    if ((flags & RFLAG_ACTIVATION) != 0u) { out_flags |= IFLAG_ACTIVATION; }
    return out_flags;
}

/// Whether a dock menu row's `WHEN_*` holds. The two facts it asks about —
/// is the app running, is it pinned — are both in the facts buffer, so the
/// caustic answers them itself.
bool build_row_applies(uint when, bool running, bool pinned)
{
    switch (when)
    {
    case WHEN_ALWAYS:   return true;
    case WHEN_RUNNING:  return running;
    case WHEN_PINNED:   return pinned;
    case WHEN_UNPINNED: return !pinned;
    default:            return false;
    }
}

[shader("compute")]
[numthreads(1, 1, 1)]
void bar_build(uint3 gid: SV_DispatchThreadID)
{
    if (gid.x != 0u || gid.y != 0u || gid.z != 0u)
    {
        return;
    }
    // The shape submission goes before the frame's, so this may be the first
    // pass of the first frame and the private buffer may still be all zeroes.
    ensure_private();
    // Every `Glyph` and `App` body the embassy has copied in, indexed by the
    // key it echoes, so every join below is an integer compare (docs/abi.md, *The arenas*).
    index_answers();

    Build b = build_new();
    uint icon_px_u = uint(max(icon_px(), 1.0) + 0.5);
    // The sizes this pass draws at: each kind's TEXT_PX in device px, snapped
    // against the loaded font's cap height (`snap_px`). Computed here, once
    // per build, and nowhere else, so every block of a kind is one size.
    uint text_px[ITEM_KIND_COUNT];
    for (uint k = 0u; k < ITEM_KIND_COUNT; ++k)
    {
        text_px[k] = snap_px(uint(max(TEXT_PX[k] * scale_px(), 1.0) + 0.5));
    }

    // Three locals, every one initialised in full: slangc warns on a
    // conditionally assigned local, and `compose_new`'s note is the reason —
    // a partially initialised one reads differently on two drivers.
    uint win_count[MAX_APPS];
    uint entry_item[MAX_APPS];
    for (uint i = 0u; i < MAX_APPS; ++i)
    {
        win_count[i] = 0u;
        entry_item[i] = NO_INDEX;
    }

    uint windows = fact_window_count();
    uint apps = fact_app_count();
    for (uint w = 0u; w < windows; ++w)
    {
        uint row = facts[0].windows[w].app_row;
        if (row < apps)
        {
            win_count[row] += 1u;
        }
    }

    // ---- the visible items, in this file's order (the dock's own order is the
    // apps table's: docs/abi.md, *The facts*) ----
    build_overview(b, icon_px_u);

    // The dock, in **shown** order, which is the order the embassy wrote the
    // apps table in: the favourites in the file's own order, then the
    // running-but-unpinned tail in first-seen order. Publishing it sorted is
    // what keeps a sort and its `uint[MAX_ITEMS]` locals out of `bar_apply`,
    // and a favourite is a flag on the entry rather than a table of its own.
    uint favourites = 0u;
    for (uint a = 0u; a < apps; ++a)
    {
        bool pinned = (facts[0].apps[a].flags & AFLAG_FAVOURITE) != 0u;
        if (!pinned && win_count[a] == 0u && !DOCK_KEEPS_UNPINNED_WITHOUT_WINDOWS)
        {
            continue;
        }
        uint place = pinned ? favourites : NO_INDEX;
        favourites += pinned ? 1u : 0u;
        entry_item[a] = build_entry(b, a, place, text_px[ITEM_DOCK], icon_px_u);
    }

    uint clock_item = build_clock(b, text_px[ITEM_CLOCK]);

    // Caffeine first among the applets and before every sidecar's, so its
    // index cannot move: it is bound once at startup where a sidecar's chips
    // come and go with the child.
    build_caffeine(b, icon_px_u);

    // ---- the applets: one chip per ROW_ITEM of every present slot ----
    //
    // In **slot order**, which is the order the pipe feeder resolved its
    // children in — publication order, not placement: `APPLET_ORDER` in the
    // tunables decides which chip lands where, and what this fixes is only the
    // tie-break between two chips of the same kind from different sidecars.
    Id level = menu_level_id();
    uint level_slot = id_tag(level) == ID_APPLET ? id_aux(level) : NO_INDEX;
    // Frame row index -> item index, for the menu pass below. One slot's menu
    // is open at a time, so one array is enough.
    uint placed[MAX_ROWS];
    for (uint i = 0u; i < MAX_ROWS; ++i)
    {
        placed[i] = NO_INDEX;
    }

    for (uint slot = 0u; slot < MAX_SLOTS; ++slot)
    {
        RowsHeader head = rows_header(slot);
        if (head.row_count == 0u)
        {
            continue;
        }
        // The chip a departure witness names `applet-<name>` without a row
        // suffix. Kept in `private` rather than derived each time, so the line
        // still spells it after the sidecar has gone.
        uint first_item = NO_INDEX;
        for (uint r = 0u; r < head.row_count; ++r)
        {
            Row row = row_at(slot, r);
            if (row.role != ROW_ITEM)
            {
                continue;
            }
            if (first_item == NO_INDEX)
            {
                first_item = row.id;
            }
            Item it = item_none();
            it.kind = ITEM_APPLET;
            it.icon_block = build_row_icon(b, slot, row, icon_px_u);
            if ((row.flags & RFLAG_LABEL_TITLE) != 0u)
            {
                // A title, not a label: never drawn
                // beside the icon, and its first character is the chip while
                // the icon is pending or nothing answers for it — the tray's
                // equivalent of a dock entry's letter. The next build after
                // the icon lands takes the first branch and the letter goes.
                if (it.icon_block == NO_INDEX)
                {
                    uint off;
                    uint len;
                    build_first_line(slot, rows_blob(slot) + row.label_off, row.label_len,
                                     off, len);
                    it.icon_block = build_letter(b, slot, off, len, false, icon_px_u);
                }
            }
            else
            {
                it.block = build_row_label(b, slot, row, text_px[ITEM_APPLET]);
            }
            it.id = id_tagged(ID_APPLET, slot, row.id);
            it.a = head.applet;
            it.flags = build_applet_flags(row.flags);
            // The level is the row's, read where it is drawn: a frame that
            // moves only a value is one buffer write and no rebuild at all.
            it.fact_row = slot * MAX_ROWS + r;
            uint index = build_push(b, it);
            if (slot == level_slot && index != NO_INDEX)
            {
                placed[r] = index;
            }
        }
        if (first_item != NO_INDEX)
        {
            private[0].slot_first_item[slot] = first_item;
        }
    }

    // The charts last of the visible items: their count follows the sidecar's
    // list, and a sidecar restarted with a different one must not move the
    // power button or a tray item out from under a click.
    uint first_chart = build_charts(b);

    // ---- the open menu's rows, then every window ----
    //
    // Both live past the visible items so that neither can move a visible
    // index, which is what keeps `Element.item` stable across window churn and
    // the hover springs from resetting.
    uint tag = id_tag(level);
    bool bodies = false;
    if (tag == ID_APPLET && level_slot < MAX_SLOTS)
    {
        uint menu_rows = rows_header(level_slot).row_count;
        for (uint r = 0u; r < menu_rows; ++r)
        {
            Row row = row_at(level_slot, r);
            if (row.role == ROW_ITEM)
            {
                continue;
            }
            // The frame reader guarantees a parent precedes its children, so
            // one pass places the whole tree. A row whose parent was dropped —
            // the table filled up — is dropped with it rather than hung off
            // item 0.
            if (row.parent >= MAX_ROWS || placed[row.parent] == NO_INDEX)
            {
                continue;
            }
            bool separator = row.role == ROW_SEPARATOR;
            bool slider = row.role == ROW_SLIDER;
            Item it = item_none();
            it.kind = slider ? ITEM_SLIDER : ITEM_MENU_ROW;
            it.parent = placed[row.parent];
            it.block = separator
                ? NO_INDEX
                : build_row_label(b, level_slot, row,
                                  text_px[slider ? ITEM_SLIDER : ITEM_MENU_ROW]);
            it.icon_block =
                separator ? NO_INDEX : build_row_icon(b, level_slot, row, icon_px_u);
            if (row.role == ROW_MENU)
            {
                // The label is the row's title and the body hangs under it.
                uint meta;
                it.aux_block = build_body(b, level_slot, row, text_px[ITEM_MENU_ROW], meta);
                it.body_blocks = it.aux_block == NO_INDEX ? 0u : meta;
                bodies = bodies || it.aux_block != NO_INDEX;
            }
            it.id = id_tagged(ID_APPLET, level_slot, row.id);
            it.flags = build_row_flags(row.role, row.flags);
            if (slider)
            {
                it.fact_row = level_slot * MAX_ROWS + r;
            }
            uint index = build_push(b, it);
            if (index != NO_INDEX)
            {
                placed[r] = index;
            }
        }
    }
    else if (tag == ID_ENTRY)
    {
        // A dock entry's own menu: the `DOCK_MENU` table's rows, where the
        // `WHEN_*` holds. Both facts it asks about (running, pinned) are in
        // the facts buffer (docs/abi.md, *The facts*).
        uint hash = id_payload(level);
        uint row_of = NO_INDEX;
        for (uint a = 0u; a < apps; ++a)
        {
            AppFact entry = facts[0].apps[a];
            if (hash_bytes(SRC_STRINGS, entry.id_off, entry.id_len) == hash)
            {
                row_of = a;
                break;
            }
        }
        uint parent = row_of == NO_INDEX ? NO_INDEX : entry_item[row_of];
        if (parent != NO_INDEX)
        {
            bool running = win_count[row_of] > 0u;
            bool pinned = (facts[0].apps[row_of].flags & AFLAG_FAVOURITE) != 0u;
            for (uint r = 0u; r < DOCK_MENU_ROWS; ++r)
            {
                uint4 spec = DOCK_MENU[r];
                // The verb column is an `ACT_*`, so a click on
                // this row is that action with the entry's own operands and
                // the embassy translates nothing (docs/abi.md, *The action ring*).
                if (spec.y == ACT_NONE || !build_row_applies(spec.z, running, pinned))
                {
                    continue;
                }
                // A row whose label the pool cannot spell is not published at
                // all: an unlabelled hit target that pins or closes something
                // is worse than a short menu.
                Literal lit = literal(spec.x);
                uint block = build_text(b, SRC_STRINGS, lit.off, lit.len,
                                        text_px[ITEM_MENU_ROW], LABEL_INK, spec.x);
                if (block == NO_INDEX)
                {
                    continue;
                }
                Item it = item_none();
                it.kind = ITEM_MENU_ROW;
                it.parent = parent;
                it.block = block;
                it.id = id_tagged(ID_DOCK_ROW, spec.y, hash);
                it.a = spec.y;
                // The entry's app id range, so `ACT_SPAWN`, `ACT_PIN` and
                // `ACT_UNPIN` carry it straight off the row.
                it.arg0 = facts[0].apps[row_of].id_off;
                it.arg1 = facts[0].apps[row_of].id_len;
                build_push(b, it);
            }
        }
    }
    else if (tag == ID_CLOCK && CLOCK_SHOW_CALENDAR && clock_item != NO_INDEX)
    {
        build_calendar(b, clock_item, text_px[ITEM_CAL_CELL]);
    }
    else if (tag == ID_CHART && first_chart != NO_INDEX
             && id_payload(level) < min(samples[0].chart_count, MAX_CHARTS))
    {
        build_numbers(b, first_chart + id_payload(level), text_px[ITEM_CHART_LINE]);
    }

    // The one ellipsis every cut body ends in, shaped once at the body's size.
    private[0].ellipsis_block = NO_INDEX;
    if (bodies)
    {
        Literal ell = literal(BODY_ELLIPSIS);
        private[0].ellipsis_block = build_text(b, SRC_STRINGS, ell.off, ell.len,
                                               text_px[ITEM_MENU_ROW], BODY_INK, BODY_ELLIPSIS);
    }

    for (uint w = 0u; w < windows; ++w)
    {
        WindowFact win = facts[0].windows[w];
        if (win.app_row >= apps || entry_item[win.app_row] == NO_INDEX)
        {
            continue;
        }
        uint off;
        uint len;
        build_first_line(SRC_STRINGS, win.title_off, win.title_len, off, len);
        Item it = item_none();
        it.kind = ITEM_WINDOW;
        it.parent = entry_item[win.app_row];
        it.block =
            build_text(b, SRC_STRINGS, off, len, text_px[ITEM_WINDOW], LABEL_INK, 0u);
        it.id = id_tagged(ID_WINDOW, win.id_hi, win.id_lo);
        it.a = win.focus_stamp;
        // The compositor's own 64-bit id, whole: what an `ACT_ACTIVATE_WINDOW`
        // or an `ACT_CLOSE_WINDOW` carries, and what the `layout:` line names
        // it by. The `Id` above is this file's naming and is 24 bits narrower
        // in `aux`, which is why the operand words are their own.
        it.arg0 = win.id_lo;
        it.arg1 = win.id_hi;
        if ((win.flags & WFLAG_FOCUSED) != 0u) { it.flags |= IFLAG_FOCUSED; }
        if ((win.flags & WFLAG_URGENT) != 0u)  { it.flags |= IFLAG_URGENT; }
        build_push(b, it);
    }

    // ---- what comes back ----
    private[0].built_items = b.items;
    private[0].built_blocks = b.blocks;
    private[0].built_glyphs = b.glyphs;
    private[0].built_dropped = b.dropped;
    // The level this table was built for. `bar_apply` compares the live one
    // against it and pushes an `ACT_REBUILD` when they differ, which the
    // embassy answers exactly as it answers a clock tick.
    private[0].built_for = level;
    private[0].build_serial += 1u;

    // ---- the image slots this build drew ----
    //
    // Before the wants, so the embassy marks what is on the bar before it
    // serves a want that may take a slot from something else. Every build
    // says it, an empty mask included: a picture no longer drawn is then one
    // the slots may let go.
    Args drawn = args_new();
    for (uint w = 0u; w < DRAWN_WORDS; ++w)
    {
        arg(drawn, b.drawn[w]);
    }
    push_action(ACT_IMAGES_DRAWN, drawn);

    // ---- the names this build wants resolved ----
    //
    // One action per name, with the bytes and the `REQ_*` (docs/abi.md
    // *The action ring*). The embassy reads the range, writes a `Request` and remembers the
    // name; a want repeated by the next build costs nothing.
    for (uint w = 0u; w < b.wants; ++w)
    {
        act4(ACT_WANT_ICON, b.want_off[w], b.want_len[w], b.want_req[w], b.want_src[w]);
    }

    // ---- the witnesses, each against what it last said --------------------
    uint entries = 0u;
    uint running = 0u;
    uint applets = 0u;
    uint charts = 0u;
    for (uint i = 0u; i < b.items; ++i)
    {
        uint kind = private[0].items[i].kind;
        if (kind == ITEM_DOCK)
        {
            entries += 1u;
            // The caustic's own rule, said out loud: an entry is running when
            // something in the table is a window of it.
            for (uint c = 0u; c < b.items; ++c)
            {
                if (private[0].items[c].kind == ITEM_WINDOW
                    && private[0].items[c].parent == i)
                {
                    running += 1u;
                    break;
                }
            }
        }
        else if (kind == ITEM_APPLET) { applets += 1u; }
        else if (kind == ITEM_CHART)  { charts += 1u; }
    }
    if (entries != private[0].said_dock_entries || running != private[0].said_dock_running)
    {
        private[0].said_dock_entries = entries;
        private[0].said_dock_running = running;
        witness2(WITNESS_INFO, getStringHash("dock: {0} entries ({1} running)"),
                 entries, running);
    }
    if (applets != private[0].said_applets)
    {
        private[0].said_applets = applets;
        witness1(WITNESS_INFO, getStringHash("applets: {0}"), applets);
    }
    if (charts != private[0].said_charts)
    {
        private[0].said_charts = charts;
        witness1(WITNESS_INFO, getStringHash("charts: {0} laid out"), charts);
    }
    if (b.items != private[0].said_built_items
        || b.blocks != private[0].said_built_blocks
        || b.glyphs != private[0].said_built_glyphs)
    {
        private[0].said_built_items = b.items;
        private[0].said_built_blocks = b.blocks;
        private[0].said_built_glyphs = b.glyphs;
        witness3(WITNESS_DEBUG,
                 getStringHash("built: {0} items, {1} blocks, {2} glyphs"),
                 b.items, b.blocks, b.glyphs);
    }
    if (b.dropped != private[0].said_dropped)
    {
        private[0].said_dropped = b.dropped;
        if (b.dropped > 0u)
        {
            witness2(WITNESS_WARN,
                     getStringHash("items: table capped at {0} ({1} dropped); some of the bar is not drawn"),
                     MAX_ITEMS, b.dropped);
        }
    }
}

[shader("compute")]
[numthreads(1, 1, 1)]
void bar_compose(uint3 gid: SV_DispatchThreadID)
{
    if (gid.x != 0u || gid.y != 0u || gid.z != 0u)
    {
        return;
    }
    uint n = live_blocks();
    for (uint b = 0u; b < n; ++b)
    {
        TextBlock blk = private[0].blocks[b];
        if (blk.kind != BLOCK_TEXT || (blk.bflags & BLOCK_COMPOSED) == 0u)
        {
            continue;
        }
        // The range is the CPU's; only what goes in it is this file's. An
        // offset that is not word-aligned is a CPU bug and would write a
        // neighbour's bytes, so it composes nothing instead.
        if ((blk.a & 3u) != 0u)
        {
            private[0].blocks[b].b = 0u;
            continue;
        }
        uint len = 0u;
        switch (blk.hash)
        {
        case COMPOSE_CLOCK_TIME:
            len = compose_store(blk.a, compose_clock_time(inputs.clock_time));
            break;
        case COMPOSE_CLOCK_DATE:
            len = compose_store(blk.a, compose_clock_date(inputs.clock_date));
            break;
        default:
            break;
        }
        private[0].blocks[b].b = len;
    }
}

// ===========================================================================
// bar_shape — the text, shaped. One thread per block, after `bar_build` and
// `bar_compose` and before `bar_apply`, because `layout()` measures what this
// writes.
//
// A label reaches this pass as **bytes**: `bar_build` named a byte range in
// one of three buffers (the strings, a sidecar's rows, the answers), a pixel
// size and a colour. This pass decodes the UTF-8, maps it through the
// font's `cmap`, fires the ligatures, applies the pair kerns and writes
// `(gid, x, y)` per glyph into `runs` — and puts the run's advance and line box
// into the block's own `size`, which is what lets the layout read a text width
// out of a buffer instead of being told one.
//
// A BLOCK_GLYPH or a BLOCK_IMAGE is square and `bar_build` already sized it:
// those are left exactly as they arrived.
// ===========================================================================

[shader("compute")]
// **One thread per workgroup, not `shape`'s own 64.**
//
// The pass is at most MAX_BLOCKS tiny jobs that run when a string changes
// rather than per frame, so a wider group gains nothing measurable, and a
// group of one is a free margin against driver trouble with the shaper's
// wider dispatch. `SHAPE_GROUP` in `bar/src/gpu.rs` mirrors the 1 below.
[numthreads(1, 1, 1)]
void bar_shape(uint3 gid: SV_DispatchThreadID)
{
    uint b = gid.x;
    if (b >= live_blocks())
    {
        return;
    }
    TextBlock blk = private[0].blocks[b];
    if (blk.kind != BLOCK_TEXT)
    {
        return;
    }

    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint limit = min(capacity, MAX_GLYPHS);
    if (blk.first_glyph >= limit || blk.px <= 0.0)
    {
        private[0].blocks[b].size = float2(0.0, 0.0);
        private[0].blocks[b].glyph_count = 0u;
        private[0].blocks[b].flags = SHAPE_RUN_TRUNCATED;
        return;
    }

    // Which buffer this block's bytes are in (docs/abi.md, *The arenas*). The
    // shaper takes its text through `ShapeTables.utf8`, so handing it the
    // arena is the whole of what it takes to shape a sidecar's row label or an
    // app's name out of bytes the embassy never transcribed.
    //
    // **A selector and not an assignment**: under SPIR-V's logical addressing a
    // resource handle cannot be a conditionally written local, so `t.utf8 = …`
    // inside an `if` is a module spirv-val rejects. Three buffers would mean
    // three `shapeRun` call sites — and slangc, which must inline every
    // function taking a descriptor, emits the whole OpenType shaper once per
    // call: three times was 5.4 MB of SPIR-V and 5.3 s of compile for this
    // one entry point. The shaper reads its bytes through a selector, so
    // there is one call.
    uint start = blk.a;
    uint limit_bytes = facts[0].header.string_words * 4u;
    if (blk.source == SRC_ANSWERS)
    {
        limit_bytes = facts[0].header.answer_bytes;
    }
    else if (blk.source < MAX_SLOTS)
    {
        start += blk.source * ROWS_ARENA_BYTES;
        limit_bytes = (blk.source + 1u) * ROWS_ARENA_BYTES;
    }

    ShapeRequest req;
    req.byte_start = start;
    req.byte_len = min(blk.b, limit_bytes > start ? limit_bytes - start : 0u);
    req.px_size = blk.px;
    req.out_start = blk.first_glyph;
    // `bar_build` allocated `block_slots(blk)` for this block (`text_slots` for a
    // label, `body_slots` for a body), and the same expression here is what
    // the shaper may fill. A composed block was
    // reserved at COMPOSE_MAX_BYTES and `blk.b` is what `bar_compose` actually
    // wrote, which is never more — so one expression covers both.
    req.out_cap = min(block_slots(blk), limit - blk.first_glyph);
    // The bar forces no feature: the default shaper's set is what its chrome
    // wants, and the feature seam is for the test harness.
    req.feature_start = 0u;
    req.feature_count = 0u;
    req._rsvd0 = 0u;

    uint source = 0u;
    if (blk.source == SRC_ANSWERS)
    {
        source = 2u;
    }
    else if (blk.source < MAX_SLOTS)
    {
        source = 1u;
    }
    let run = shapeRun(shape_tables(source), shapeParams, req, runs, limit);

    // The three words the embassy does not write and never reads: the metric
    // comes back into the block, and `block_size` is what the layout asks.
    // Clamped, and not as a formality: the paint walks this run per texel and
    // sizes a chip around this width, so a metric that came back absurd — a
    // corrupt table, a font whose flattened kerns are nonsense — would become
    // an absurd rect rather than an ugly one. A bar is never wider than a few
    // thousand px; MAX_METRIC_PX is far past any label and far short of
    // anything that breaks the layout arithmetic.
    float advance = clamp(float(run.advance) / 64.0, 0.0, MAX_METRIC_PX);
    private[0].blocks[b].size = float2(advance, text_line_height(blk.px));
    private[0].blocks[b].glyph_count = run.glyph_count;
    private[0].blocks[b].flags = run.flags;
    // A truncated run is a **bug rather than a size** (`text_slots`), and
    // `bar_apply` names the block — there rather than here, because the
    // witness ring's count is a read-modify-write and this pass is one thread
    // per block.
}

/// Everything the item table depends on in one slot's region, hashed: every
/// word of every row but `value`, and the label and icon bytes their ranges
/// name (docs/abi.md, *The arenas*).
///
/// **The embassy does not make this comparison.** It writes a `Rows` frame's
/// body verbatim and sends one `CMD_ROWS`; whether that is a rebuild or a
/// value-only change is decided here. A
/// slider tick moves `value` and nothing else, so the hash is unchanged and
/// the frame costs no model pass at all.
///
/// A pixmap icon is a content-addressed key (docs/abi.md, *Pixmap*), so a *changed* pixmap is
/// a changed name in the blob and moves this hash like any other icon.
uint rows_shape_of(uint slot)
{
    RowsHeader head = rows_header(slot);
    uint blob = rows_blob(slot);
    uint h = FNV_OFFSET;
    h = (h ^ head.applet) * FNV_PRIME;
    h = (h ^ head.row_count) * FNV_PRIME;
    for (uint r = 0u; r < head.row_count; ++r)
    {
        Row row = row_at(slot, r);
        h = (h ^ row.id) * FNV_PRIME;
        h = (h ^ row.parent) * FNV_PRIME;
        h = (h ^ row.role) * FNV_PRIME;
        h = (h ^ row.flags) * FNV_PRIME;
        h = (h ^ hash_bytes(slot, blob + row.label_off, row.label_len)) * FNV_PRIME;
        h = (h ^ hash_bytes(slot, blob + row.icon_off, row.icon_len)) * FNV_PRIME;
        // The body, as far as `build_body` can look, a
        // word at a time — it is kilobytes where a label is tens of bytes —
        // with its whole length folded in.
        h = (h ^ hash_region_words(slot, blob + row_body_off(row),
                                   min(row_body_len(row), 2u * BODY_SHAPE_BYTES))) * FNV_PRIME;
        h = (h ^ row_body_len(row)) * FNV_PRIME;
        // `min` and `max` are a slider's range and not its position, so they
        // are shape: a volume slider that changed its units is a new row.
        h = (h ^ asuint(row.min)) * FNV_PRIME;
        h = (h ^ asuint(row.max)) * FNV_PRIME;
    }
    // Never zero, which is what `private` holds before any frame has arrived,
    // so the first `CMD_ROWS` for a slot is always a rebuild.
    return h == 0u ? FNV_PRIME : h;
}

/// The chart strip's shape: the kinds of the samples body's rows, in order.
///
/// A comparison the caustic makes, not the embassy. A tick that carries new
/// numbers for the same charts is an upload of the body and a redraw; a
/// different chart set is a rebuild, one frame later.
uint chart_shape_of()
{
    uint n = min(samples[0].chart_count, MAX_CHARTS);
    uint h = FNV_OFFSET;
    h = (h ^ n) * FNV_PRIME;
    for (uint i = 0u; i < n; ++i)
    {
        h = (h ^ samples[0].charts[i].kind) * FNV_PRIME;
    }
    return h == 0u ? FNV_PRIME : h;
}

/// One thread: the action ring, the command ring, the state machine, the
/// layout, the dwell clock, the springs. It lays out *both* targets, so a
/// popup frame never needs a second apply.
[shader("compute")]
[numthreads(1, 1, 1)]
void bar_apply(uint3 gid: SV_DispatchThreadID)
{
    if (gid.x != 0u || gid.y != 0u || gid.z != 0u)
    {
        return;
    }
    ensure_private();

    uint capacity;
    uint stride;
    commands.GetDimensions(capacity, stride);
    uint count = min(min(inputs.command_count, capacity), MAX_COMMANDS);
    bool dump_stall = false;
    // One bit per sidecar slot whose region was rewritten this frame. MAX_SLOTS
    // is 16, so the set is one word.
    uint rewritten = 0u;
    for (uint i = 0u; i < count; ++i)
    {
        // Commands are answered against *last* frame's element table, which is
        // what was on screen when the pointer event happened. Layout comes
        // after, so a click that opens a popup lays that popup out in the same
        // frame it was decided in.
        apply_command(commands[i]);
        dump_stall = dump_stall || commands[i].tag == CMD_DUMP_STALL;
        if (commands[i].tag == CMD_ROWS && commands[i].args.x < MAX_SLOTS)
        {
            rewritten |= 1u << commands[i].args.x;
        }
    }

    // -- what a rewritten region and a new chart set are worth (docs/abi.md, *The arenas*) -------
    //
    // The embassy copied bytes and said which slot; whether that is a rebuild
    // is this file's, because it is the only one that knows what the item
    // table was built from. A shape it has already seen is one `write_buffer`
    // and this frame, and nothing else.
    bool rebuild = false;
    for (uint slot = 0u; slot < MAX_SLOTS; ++slot)
    {
        if ((rewritten & (1u << slot)) == 0u)
        {
            continue;
        }
        uint shape = rows_shape_of(slot);
        if (shape != private[0].rows_shape[slot])
        {
            private[0].rows_shape[slot] = shape;
            rebuild = true;
        }
    }
    uint charts = chart_shape_of();
    if (charts != private[0].chart_shape)
    {
        private[0].chart_shape = charts;
        rebuild = true;
    }

    // After the commands, before the layout: the commands may have set a level
    // and the layout must not be given one that no longer resolves.
    step_menu();
    // The bodies' lines, which the layout measures rows by. After the shape
    // pass, which ran before this one.
    wrap_bodies();
    layout();
    // Every element the previous frame used and this one does not, said once.
    witness_departures();
    // After the layout, so a toast anchors at the chip's rect of this frame;
    // its own rows are laid out from the next. After the
    // wrap, so a toast whose rows were built this frame opens at their size;
    // and told whether a rows frame this frame is a rebuild, so it never opens
    // on rows the next build replaces.
    step_toast(rebuild);

    // NaN or a stalled frame must not fling the springs or the dwell clock.
    float dt = inputs.dt;
    dt = dt > 0.0 ? min(dt, DT_MAX) : 0.0;

    bool dwelling = step_dwell(dt);
    // After the layout, so the strip is this frame's.
    bool resting = step_numbers(dt);
    dwelling = dwelling || resting;
    bool moving = integrate_springs(dt);
    bool stirred = step_wake(dt);

    // -- the surface this file wants -----------------------------------------
    //
    // Pushed as a message (docs/abi.md, *The action ring*). The tunables' edge
    // and height are pushed once — on the frame `ensure_private` ran, because
    // the pair it is compared against is zero then — and again whenever a
    // reload changes either. The embassy's `ACT_ANCHOR` handler re-anchors
    // when it differs from what is live, and says so.
    uint want_edge = EDGE;
    uint want_height = uint(max(BAR_HEIGHT, 1.0) + 0.5);
    if (want_edge != private[0].anchor_edge || want_height != private[0].anchor_height)
    {
        private[0].anchor_edge = want_edge;
        private[0].anchor_height = want_height;
        act2(ACT_ANCHOR, want_edge, want_height);
    }

    // -- how long a shape pass may take ---------------------------------------
    //
    // The same shape of message as the anchor's, and for the same reason: the
    // number is this file's, the machinery is the embassy's, and neither side
    // polls the other. What the embassy does when a pass runs over is fixed —
    // one witness, and no more shaping until the arena is written again — so
    // this pushes a threshold and never a policy.
    uint want_watchdog = max(SHAPE_WATCHDOG_MS, 1u);
    if (want_watchdog != private[0].watchdog_ms)
    {
        private[0].watchdog_ms = want_watchdog;
        act2(ACT_WATCHDOG, want_watchdog, 0u);
    }

    // -- the popup's size, compared here (docs/abi.md, *The action ring*) ---------
    //
    // The layout re-measures the popup every frame; when the want differs from
    // what the compositor gave, one `ACT_POPUP_MOVE`. The comparison is
    // made here, on the side that knows what the popup wants to be.
    if (private[0].popup_state == POPUP_UP)
    {
        uint want_w = uint(max(private[0].popup_want.x, 1.0) + 0.5);
        uint want_h = uint(max(private[0].popup_want.y, 1.0) + 0.5);
        bool differs = want_w != inputs.popup_size.x || want_h != inputs.popup_size.y;
        bool asked = want_w == uint(private[0].popup_sent.x + 0.5)
                     && want_h == uint(private[0].popup_sent.y + 0.5);
        if (differs && !asked && want_w > 1u && want_h > 1u)
        {
            private[0].popup_sent = float2(float(want_w), float(want_h));
            act2(ACT_POPUP_MOVE, want_w, want_h);
        }
    }

    // -- the shaper's notdef runs, and the bytes behind them ----------------
    //
    // `bar_shape` flags a run whose codepoints the merged font lacks; this
    // reads the blocks back and says so once per change, and asks the curves
    // sidecar to cover the **bytes** of each flagged text block whose bytes
    // are the embassy's (docs/abi.md, *The action ring*). Still a witness: nothing
    // here waits for the answer or draws differently because of it.
    uint notdefs = 0u;
    uint texts = 0u;
    uint truncated = NO_INDEX;
    uint stalled = NO_INDEX;
    uint hash = 2166136261u; // FNV-1a 32 over the *set* of flagged blocks
    uint nblocks = live_blocks();
    for (uint b = 0u; b < nblocks; ++b)
    {
        if (private[0].blocks[b].kind != BLOCK_TEXT)
        {
            continue;
        }
        texts += 1u;
        if ((private[0].blocks[b].flags & SHAPE_RUN_NOTDEF) != 0u)
        {
            notdefs += 1u;
            hash = (hash ^ b) * 16777619u;
        }
        // A body block that ran out of its slots is a cut, drawn as one
        // (`body_slots`); the deadline is still the deadline.
        uint cut_flags = (private[0].blocks[b].bflags & BLOCK_BODY) != 0u
                         ? SHAPE_RUN_DEADLINE : (SHAPE_RUN_TRUNCATED | SHAPE_RUN_DEADLINE);
        if ((private[0].blocks[b].flags & cut_flags) != 0u && truncated == NO_INDEX)
        {
            truncated = b;
        }
        if ((private[0].blocks[b].flags & SHAPE_RUN_DEADLINE) != 0u)
        {
            stalled = b;
        }
    }

    // -- a run that ran out of slots ----------------------------------------
    //
    // `bar_build` gives a block `text_slots(bytes)` — four a byte plus eight,
    // `Batch::push`'s own rule — so a substitution that grows a Latin label
    // fits, and a run that is still truncated is a bug in the slot rule or the shaper
    // rather than a label that was always too long. It
    // says so once per change, with the block and what it was given.
    if (truncated != private[0].said_truncated)
    {
        private[0].said_truncated = truncated;
        if (truncated != NO_INDEX && stalled == NO_INDEX)
        {
            witness2(WITNESS_WARN,
                     getStringHash("shape: block {0} ran out of glyph slots ({1} bytes)"),
                     truncated, private[0].blocks[truncated].b);
        }
        // The wall-clock deadline: `shapeRun` reads the device's realtime
        // clock (`VK_KHR_shader_clock`) and stops a run that passes it, the
        // guard against a runaway shader that holds even when the op
        // accounting is wrong. A run that hits
        // it drew short, and the only way that happens with a font the bar
        // actually loaded is an arena that is not one — so it says so.
        if (stalled != NO_INDEX)
        {
            witness1(WITNESS_WARN,
                     getStringHash("shape: block {0} passed the run deadline and stopped"),
                     stalled);
        }
    }
    if (notdefs != private[0].said_notdef)
    {
        private[0].said_notdef = notdefs;
        if (notdefs == 0u)
        {
            witness0(WITNESS_INFO, getStringHash("text: every run has its glyphs"));
        }
        else
        {
            witness1(WITNESS_INFO,
                     getStringHash("text: {0} runs have a glyph the font lacks"), notdefs);
        }
    }
    // **Every** run flagged is not a font missing a script: it is a font that
    // has not landed. The bar starts with no curves and no `cmap` at all (they
    // arrive from `caustics-glyphs` over its pipe),
    // so for the first few hundred milliseconds the shaper answers `.notdef`
    // for every codepoint there is, and asking the sidecar to cover the bar's
    // own Latin labels would be one wire frame and one remembered question
    // each for nothing. One drawable run is enough to make the flagged ones
    // mean what they say. The guard sits here, beside the loop that would
    // otherwise ask.
    if (notdefs > 0u && notdefs < texts && hash != private[0].notdef_hash)
    {
        private[0].notdef_hash = hash;
        for (uint b = 0u; b < nblocks; ++b)
        {
            TextBlock blk = private[0].blocks[b];
            if (blk.kind != BLOCK_TEXT || (blk.flags & SHAPE_RUN_NOTDEF) == 0u)
            {
                continue;
            }
            // A composed block's bytes are this file's — `bar_compose` wrote
            // them into the strings buffer on the GPU and the embassy's copy
            // is zeroes — so there is nothing at that range for it to send.
            if ((blk.bflags & BLOCK_COMPOSED) != 0u)
            {
                continue;
            }
            // Every other block's bytes are the embassy's, in whichever of
            // its three buffers the block was built from — a title or a
            // literal in `strings`, a sidecar's row label in its slot's
            // region of the rows arena, an app's name in `answers` — so the
            // range goes out with the block's own `source`, the word
            // `ACT_WANT_ICON` carries for the same reason (docs/abi.md, *The action ring*). A menu row
            // `🇦🇺 Australia` is a question like any title.
            act3(ACT_COVER, blk.a, blk.b, blk.source);
        }
    }

    // Whether the level this frame holds is the one the table was built for.
    // The embassy answers it exactly as it answers a clock tick — mark the
    // shape submission pending, ask for a frame — and `bar_build` then records
    // the new level. It is the one rebuild the CPU cannot see coming, because
    // the menu is this file's.
    //
    // The chart numbers are the one level whose rows move with no fact
    // moving: a samples tick is an upload and a redraw, and the lines composed
    // from it are a build's. So while they are up, a tick this file has not
    // built for is a rebuild too.
    if (numbers_live() && asuint(samples[0].t) != private[0].numbers_t)
    {
        private[0].numbers_t = asuint(samples[0].t);
        rebuild = true;
    }
    if (rebuild || !menu_level_id().eq(private[0].built_for))
    {
        act(ACT_REBUILD);
    }

    // A pointer *leave* clears the hover here rather than in `bar_sample`,
    // which does not run for a target the pointer is not on.
    witness_hover();

    uint redraw = (moving || dwelling || stirred) ? 1u : 0u;
    if (redraw != private[0].said_redraw)
    {
        private[0].said_redraw = redraw;
        witness0(WITNESS_INFO, redraw != 0u ? getStringHash("redraw: animating")
                                            : getStringHash("redraw: settled"));
    }

    // The embassy's wall clock says the bar has been animating too long and has
    // asked what is holding it awake (docs/abi.md, *The witness ring*). The *trigger* is its, because the
    // pass that would notice from `dt` is the one that stops running when
    // frames do; the answer is this file's, because the numbers are.
    if (dump_stall)
    {
        uint said = 0u;
        uint n = live_elements();
        for (uint i = 0u; i < n && said < 8u; ++i)
        {
            if (i == ELEM_BAR || i == ELEM_POPUP)
            {
                continue; // neither has a hover chip
            }
            if ((private[0].elements[i].flags & EFLAG_PRESENT) == 0u)
            {
                continue;
            }
            float4 anim = private[0].elements[i].anim;
            bool off = abs(anim.x) > 0.002 && abs(anim.x - 1.0) > 0.002;
            off = off || (abs(anim.y) > 0.002 && abs(anim.y - 1.0) > 0.002);
            off = off || abs(anim.z) > 0.02 || abs(anim.w) > 0.02;
            if (!off)
            {
                continue;
            }
            NameRef nm = element_name(i, private[0].elements[i].item);
            Args a = name_args(nm);
            arg4(a, anim);
            witness(WITNESS_WARN, stall_literal(nm.slot), a);
            said += 1u;
        }
        Args dwell = args_new();
        argf(dwell, float(private[0].popup_state));
        argf(dwell, private[0].dwell_secs);
        argf(dwell, private[0].dwell_candidate);
        argf(dwell, private[0].preview_owner);
        witness(WITNESS_WARN,
                getStringHash("stall: popup dwell(state,secs,candidate,owner)={0:f},{1:f},{2:f},{3:f}"),
                dwell);
        uint sources = 0u;
        float oldest = 0.0;
        for (uint w = 0u; w < WAKE_SOURCES; ++w)
        {
            if (private[0].wake[w].w > 0.0)
            {
                sources += 1u;
                oldest = max(oldest, private[0].wake[w].z);
            }
        }
        Args o = args_new();
        arg(o, sources);
        argf(o, oldest);
        argf(o, private[0].wake_track.w);
        witness(WITNESS_WARN,
                getStringHash("stall: wake(sources,oldest s,since last s)={0},{1:f},{2:f}"), o);
    }

    // The one word the embassy acts on without a message (docs/abi.md, *The one word*), and the
    // stamp that says this file has run at all.
    state[0].surface.wants_redraw = redraw;
    state[0].surface.frame = inputs.frame;
}

// ===========================================================================
// bar_visibility — the carve. Exactly one owner per texel, which makes
// hit-testing a texel fetch and hover a texel fetch.
// ===========================================================================

/// The element that is the ground of `target`: it covers the whole surface, so
/// there is always an owner and never a fall-through.
uint slab_of(uint target)
{
    return target == TARGET_POPUP ? ELEM_POPUP : ELEM_BAR;
}

[shader("compute")]
[numthreads(8, 8, 1)]
void bar_visibility(uint3 gid: SV_DispatchThreadID)
{
    uint2 dims;
    ids.GetDimensions(dims.x, dims.y);
    if (gid.x >= dims.x || gid.y >= dims.y)
    {
        return;
    }

    uint target = params.target;
    uint slab = slab_of(target);
    float2 p = float2(gid.xy) + 0.5; // texel centres

    // The slab claims by default — including the texels its own rounded
    // corners give away, which are genuinely nobody's.
    Element ground = private[0].elements[slab];
    uint winner = NO_INDEX;
    if (element_live(ground, target) && element_sdf(ground, p) <= 0.0)
    {
        winner = slab;
    }

    // Index order, later wins. Allocation order is the stacking order: wings
    // first, then the popup's rows, and nothing on the bar overlaps anyway.
    uint n = live_elements();
    for (uint i = 0u; i < n; ++i)
    {
        if (i == slab)
        {
            continue;
        }
        Element e = private[0].elements[i];
        if (!element_live(e, target) || !rect_near(e.rect, p, 0.0) || element_clipped(e, p))
        {
            continue;
        }
        if (element_sdf(e, p) <= 0.0)
        {
            winner = i;
        }
    }

    ids[uint2(gid.xy)] = winner;
}

// ===========================================================================
// bar_sample — the pointer, as a texel fetch.
// ===========================================================================

/// One thread, after the visibility pass of the target the pointer is on: read
/// the id under it and publish it. Texel truth — carved corners and all — so
/// the bar's idea of what the pointer is over is literally what is drawn
/// there. Targets the pointer is not on leave `hover_element` alone, which is what
/// lets the popup keep its hover while the bar's pass runs.
[shader("compute")]
[numthreads(1, 1, 1)]
void bar_sample(uint3 gid: SV_DispatchThreadID)
{
    if (gid.x != 0u || gid.y != 0u || gid.z != 0u)
    {
        return;
    }
    ensure_private();
    if (private[0].pointer_target != params.target)
    {
        return;
    }

    uint2 dims;
    ids.GetDimensions(dims.x, dims.y);

    float2 p = private[0].pointer;
    // Written as a positive test so a NaN pointer (no pointer yet) falls
    // through to "nothing" rather than indexing on garbage.
    bool on_surface = p.x >= 0.0 && p.y >= 0.0
                      && p.x < float(dims.x) && p.y < float(dims.y);
    private[0].hover_element = on_surface ? ids[uint2(uint(p.x), uint(p.y))] : NO_INDEX;
    witness_hover();
}

// ===========================================================================
// bar_draw — paint. Everything written here is PREMULTIPLIED: the surface is
// configured PreMultiplied and the slab's alpha is the bar's translucency.
// ===========================================================================

/// Premultiplied source-over. The only compositing operator in this file.
float4 over(float4 src, float4 dst_col)
{
    return src + dst_col * (1.0 - src.a);
}

/// Coverage of a shape at a texel, from its signed distance: a one-pixel
/// linear ramp across the boundary. Cheap, and enough — every edge here is
/// either axis-aligned or a generous radius.
float coverage(float d)
{
    return saturate(0.5 - d);
}

/// Composite a flat colour at `alpha` coverage, tinted by the reload smoke.
float4 ink(float3 rgb, float alpha, float4 col)
{
    float a = saturate(alpha);
    if (a <= 0.0)
    {
        return col;
    }
    float3 c = rgb * RELOAD_SMOKE_TINT * a;
    return over(float4(c, a), col);
}

/// The hover chip and the pressed dip — the two cues every button-shaped item
/// gets.
float4 draw_chip(Element e, float2 p, float4 col)
{
    float s = scale_px();
    float hover = saturate(e.anim.x);
    float press = saturate(e.anim.y);
    if (hover <= 0.0 && press <= 0.0)
    {
        return col;
    }

    // Press shrinks the chip as well as darkening it: two cues, because a
    // flat bar has no shadow for a button to sink into.
    float d = element_sdf(e, p) + press * PRESS_INSET * s;
    float cov = coverage(d);
    float halo = saturate(1.0 - max(d, 0.0) / max(HOVER_HALO_PX * s, 1.0));
    float a = saturate(hover * (HOVER_CHIP_ALPHA * cov + HOVER_HALO_ALPHA * (1.0 - cov) * halo)
                       + press * PRESS_ALPHA * cov);
    return ink(lerp(HOVER_RGB, PRESS_RGB, press), a, col);
}

/// The running marks and the focus underline. They take opposite sides of the
/// chip — marks against the screen edge, the underline on the free edge — so
/// `EDGE_BOTTOM` swaps both and they still never collide.
float4 draw_dock_marks(Element e, Item it, float2 p, float4 col)
{
    float s = scale_px();
    bool bottom = inputs.edge == EDGE_BOTTOM;

    // Both of these are the layout's answers, not the CPU's: `place` counted
    // the entry's ITEM_WINDOW children once (`dock_flags`), and
    // `draw_element` has already folded `Element.derived` into `it.flags`.
    uint marks = min(e.derived_a, RUNNING_MARK_MAX);
    if (marks > 0u)
    {
        float dot_d = RUNNING_MARK * s;
        float step_x = dot_d * 2.0;
        float span = float(marks) * dot_d + float(marks - 1u) * dot_d;
        float x0 = e.rect.x + e.rect.z * 0.5 - span * 0.5;
        float cy = bottom ? e.rect.y + e.rect.w - MARK_INSET * s - dot_d * 0.5
                          : e.rect.y + MARK_INSET * s + dot_d * 0.5;
        for (uint k = 0u; k < marks; ++k)
        {
            float2 c = float2(x0 + float(k) * step_x + dot_d * 0.5, cy);
            col = ink(MARK_RGB, MARK_ALPHA * coverage(length(p - c) - dot_d * 0.5), col);
        }
    }

    if ((it.flags & IFLAG_FOCUSED) != 0u)
    {
        float uh = FOCUS_UNDERLINE * s;
        float uw = max(e.rect.z * FOCUS_UNDERLINE_FRAC, uh);
        float uy = bottom ? e.rect.y : e.rect.y + e.rect.w - uh;
        float2 org = float2(e.rect.x + floor((e.rect.z - uw) * 0.5), uy);
        col = ink(FOCUS_RGB, coverage(rounded_rect_sdf(p - org, float2(uw, uh), uh * 0.5)), col);
    }
    return col;
}

/// The band a menu row's label, icon and marks are centred in: the whole row,
/// or — for a row with a body under it — the plain row's height at its top,
/// so the label stays where a bodiless row would put it.
float row_band_h(Element e, Item it)
{
    return body_shown(it) > 0u ? min(MENU_ROW_H * scale_px(), e.rect.w) : e.rect.w;
}

/// One body line, drawn: its glyphs walked across its paragraph's chunks from
/// `org` (the line box's top-left), and — when `ell` — only the ones that
/// leave room for the ellipsis, which follows them.
float4 draw_body_line(BodyLine line, bool ell, float2 org, float2 p, float4 col, float dim)
{
    uint nblocks = live_blocks();
    if (line.block >= nblocks)
    {
        return col;
    }
    TextBlock first = private[0].blocks[line.block];
    float px = first.px;
    if (px <= 0.0)
    {
        return col;
    }
    float baseline = org.y + text_baseline(px);
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint limit = min(capacity, MAX_GLYPHS);
    uint want = ell ? line.fit : line.count;
    uint b = line.block;
    uint g = line.glyph;
    uint taken = 0u;
    float shift = -run_pen(first, g);
    float cov = 0.0;
    for (uint j = 0u; j < MAX_BODY_BLOCKS && taken < want && b < nblocks; ++j)
    {
        TextBlock blk = private[0].blocks[b];
        uint n = min(blk.glyph_count, MAX_GLYPHS_PER_BLOCK);
        for (uint k = g; k < n && taken < want; ++k)
        {
            taken += 1u;
            uint gi = blk.first_glyph + k;
            if (gi >= limit)
            {
                break;
            }
            ShapedGlyph gl = runs[gi];
            float2 origin = float2(org.x + shift + float(gl.x) / 64.0,
                                   baseline - float(gl.y) / 64.0);
            float4 rect = textGlyphRect(gl.gid, origin, px);
            if (p.x < rect.x || p.x >= rect.z || p.y < rect.y || p.y >= rect.w)
            {
                continue;
            }
            cov = max(cov, textCoverage(gl.gid, textSampleAt(p, origin, px), px));
        }
        shift += blk.size.x;
        b += 1u;
        g = 0u;
    }
    col = ink(first.color.rgb, cov * first.color.a * dim, col);
    if (ell)
    {
        uint e = private[0].ellipsis_block;
        if (e < nblocks)
        {
            col = draw_block_at(e, float2(org.x + line.fit_w, org.y), p, col, dim);
        }
    }
    return col;
}

/// A menu row's body: the line under the texel, if one is shown there.
float4 draw_body(Element e, Item it, float x, float2 p, float4 col, float dim)
{
    uint shown = body_shown(it);
    if (shown == 0u)
    {
        return col;
    }
    float s = scale_px();
    float top = e.rect.y + MENU_ROW_H * s;
    float lh = MENU_BODY_LINE_H * s;
    if (p.y < top || lh <= 0.0)
    {
        return col;
    }
    uint k = uint((p.y - top) / lh);
    uint at = item_body_first_line(it) + k;
    if (k >= shown || at >= min(private[0].body_line_count, MAX_BODY_POOL))
    {
        return col;
    }
    BodyLine line = private[0].body_lines[at];
    float px = line.block < live_blocks() ? private[0].blocks[line.block].px : 0.0;
    float y = top + float(k) * lh + floor((lh - text_line_height(px)) * 0.5);
    return draw_body_line(line, k + 1u == shown && body_cut(it), float2(floor(x), y), p, col,
                          dim);
}

/// A menu row's gutters: the check column on the left, the submenu chevron on
/// the right. SDF strokes, so the bar needs no symbol font for two shapes.
float4 draw_row_marks(Element e, Item it, float2 p, float4 col)
{
    float s = scale_px();
    float m = MENU_MARK * s;
    float stroke = MARK_STROKE * s * 0.5;
    float gutter = MENU_GUTTER * s;
    float y = e.rect.y + floor((row_band_h(e, it) - m) * 0.5);
    float dim = (it.flags & IFLAG_DISABLED) != 0u ? DISABLED_ALPHA : 1.0;

    if ((it.flags & IFLAG_CHECKABLE) != 0u)
    {
        float2 org = float2(e.rect.x + floor((gutter - m) * 0.5), y);
        if ((it.flags & IFLAG_CHECKED) != 0u)
        {
            col = ink(MARK_INK, dim * coverage(check_sdf(p, org, m) - stroke), col);
        }
        else
        {
            // A hollow box, so the column reads as a column rather than as a
            // stray tick next to one row.
            float d = rounded_rect_sdf(p - org, float2(m, m), 2.0 * s);
            float outline = coverage(abs(d) - stroke);
            col = ink(MARK_INK, dim * MARK_EMPTY_ALPHA * outline, col);
        }
    }

    if ((it.flags & IFLAG_SUBMENU) != 0u)
    {
        float2 org = float2(e.rect.x + e.rect.z - gutter + floor((gutter - m) * 0.5), y);
        col = ink(MARK_INK, dim * coverage(chevron_sdf(p, org, m, 1.0) - stroke), col);
    }
    return col;
}

/// One block, placed with its top-left at `org`: a shaped string, one glyph, or
/// one image slot.
///
/// A string is a walk over the
/// run `bar_shape` wrote — for each glyph, its dilated pixel rect is the cull
/// and `textCoverage` is the answer — and an icon is one glyph of the same
/// buffers. Nothing here samples a glyph bitmap, because there is not one.
///
/// The blend is unchanged in every case: premultiplied over, tinted by
/// RELOAD_SMOKE_TINT, scaled by `dim` for a disabled row. A coverage kind is
/// painted in the block's own colour; an image modulates it.
float4 draw_block_at(uint b, float2 org, float2 p, float4 col, float dim)
{
    if (b >= live_blocks())
    {
        return col;
    }
    TextBlock blk = private[0].blocks[b];
    float px = blk.px;
    if (px <= 0.0)
    {
        return col;
    }

    if (blk.kind == BLOCK_IMAGE)
    {
        // The slot fills the box, so the sample is the box's own fraction: one
        // bilinear fetch, and a scale change is a different fraction of the
        // same texels rather than a re-raster.
        float2 t = (p - org) / max(blk.size, float2(1.0, 1.0));
        if (t.x < 0.0 || t.y < 0.0 || t.x >= 1.0 || t.y >= 1.0)
        {
            return col;
        }
        float4 texel = images.SampleLevel(tex_sampler, float3(t, float(blk.a)), 0.0);
        // The slots hold **straight** RGBA — what a sidecar writes on the wire
        // (docs/abi.md, *Pixmap*) — and the blend below is
        // premultiplied over, so the multiply is here: one per texel, at the
        // sample, instead of one per texel on the CPU on the way in. The
        // bilinear fetch interpolates straight
        // colour, which is the honest thing to do with an icon whose edge is a
        // hard alpha ramp.
        texel.rgb *= texel.a;
        float4 src = texel * blk.color;
        src.rgb *= RELOAD_SMOKE_TINT;
        return over(src * dim, col);
    }

    if (blk.kind == BLOCK_GLYPH)
    {
        // An icon is a glyph with a name, drawn to fill a `px` square. Its
        // contours run up from the box's *bottom* edge (an SVG's y is flipped
        // into em space by the text engine's icon loader), so the pen is down there.
        float2 origin = float2(org.x, org.y + px);
        float4 rect = textGlyphRect(blk.a, origin, px);
        if (p.x < rect.x || p.x >= rect.z || p.y < rect.y || p.y >= rect.w)
        {
            return col;
        }
        float cov = textCoverage(blk.a, textSampleAt(p, origin, px), px);
        return ink(blk.color.rgb, cov * blk.color.a * dim, col);
    }

    // BLOCK_TEXT: the run `bar_shape` wrote, glyph by glyph. Bounded by the
    // block's own count, by the sanity cap, and by the buffer — a stale
    // `first_glyph` from a block table rebuilt since must not read past the end.
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint limit = min(capacity, MAX_GLYPHS);
    uint n = min(blk.glyph_count, MAX_GLYPHS_PER_BLOCK);
    float baseline = org.y + text_baseline(px);
    float cov = 0.0;

    for (uint k = 0u; k < n; ++k)
    {
        uint gi = blk.first_glyph + k;
        if (gi >= limit)
        {
            break;
        }
        ShapedGlyph g = runs[gi];
        // 26.6 fixed point, as the shaper writes it.
        float2 origin = float2(org.x + float(g.x) / 64.0, baseline - float(g.y) / 64.0);
        float4 rect = textGlyphRect(g.gid, origin, px);
        if (p.x < rect.x || p.x >= rect.z || p.y < rect.y || p.y >= rect.w)
        {
            continue;
        }
        // Glyphs in a run do not overlap; where two do (an accent, a ligature
        // that did not fire) the larger coverage is the honest answer for a
        // single-channel mask, and it is also one composite instead of two.
        cov = max(cov, textCoverage(g.gid, textSampleAt(p, origin, px), px));
    }
    return ink(blk.color.rgb, cov * blk.color.a * dim, col);
}

/// An item's icon and label, laid out by exactly the arithmetic
/// `item_content_w` measured — a menu row's content starts after the left
/// gutter, everything else is centred in its chip.
float4 draw_item_content(Element e, Item it, float2 p, float4 col)
{
    float s = scale_px();
    float gap = gap_px();
    float inner = item_content_w(it);
    float dim = (it.flags & IFLAG_DISABLED) != 0u ? DISABLED_ALPHA : 1.0;

    // A line of the chart numbers: the name where a menu row's label starts,
    // the value right-aligned against the right gutter.
    if (it.kind == ITEM_CHART_LINE)
    {
        float gutter = MENU_GUTTER * s;
        float2 name = block_size(it.block);
        float2 value = block_size(it.aux_block);
        col = draw_block_at(it.block, float2(floor(e.rect.x + gutter),
                                             e.rect.y + floor((e.rect.w - name.y) * 0.5)),
                            p, col, 1.0);
        return draw_block_at(it.aux_block,
                             float2(floor(e.rect.x + e.rect.z - gutter - value.x),
                                    e.rect.y + floor((e.rect.w - value.y) * 0.5)),
                             p, col, 1.0);
    }

    // A menu row's content starts after the left gutter; a slider is a menu
    // row with a track under it, so it shares the gutter and centres its
    // content in what is left above the band.
    bool in_menu = it.kind == ITEM_MENU_ROW || it.kind == ITEM_SLIDER;
    float x = in_menu
              ? e.rect.x + MENU_GUTTER * s
              : e.rect.x + floor((e.rect.z - inner) * 0.5);
    float content_h = it.kind == ITEM_SLIDER
                      ? max(e.rect.w - SLIDER_BAND * s, 1.0)
                      : (it.kind == ITEM_MENU_ROW ? row_band_h(e, it) : e.rect.w);

    // Icon, then the aux block (the clock's date), then the label — the exact
    // order `item_content_w` measured.
    float icon_w = item_icon_w(it);
    if (icon_w > 0.0)
    {
        float2 sz = block_size(it.icon_block);
        col = draw_block_at(it.icon_block, float2(floor(x), e.rect.y + floor((content_h - sz.y) * 0.5)),
                            p, col, dim);
        x += icon_w + gap;
    }
    uint aux = item_aux_block(it);
    if (aux != NO_INDEX)
    {
        float2 sz = block_size(aux);
        col = draw_block_at(aux, float2(floor(x), e.rect.y + floor((content_h - sz.y) * 0.5)),
                            p, col, dim);
        x += sz.x + gap;
    }
    float label_w = item_label_w(it);
    if (label_w > 0.0)
    {
        float2 sz = block_size(it.block);
        col = draw_block_at(it.block, float2(floor(x), e.rect.y + floor((content_h - sz.y) * 0.5)),
                            p, col, dim);
    }
    // A body under the label, starting where the label does.
    if (it.kind == ITEM_MENU_ROW)
    {
        col = draw_body(e, it, x, p, col, dim);
    }
    return col;
}

// ---------------------------------------------------------------------------
// Charts. A row of the samples frame, drawn per texel: no CPU arithmetic, no
// history in the item table, nothing about a chart in the ABI beyond its index
// and what it measures. The whole strip is display only.
// ---------------------------------------------------------------------------

/// Valid columns in the frame, bounded by the buffer's own capacity.
uint live_samples()
{
    return min(samples[0].sample_count, CHART_SAMPLES);
}

/// Chart `c`'s row, or an empty one when the frame does not have that many.
ChartRow chart_row(uint c)
{
    if (c < min(samples[0].chart_count, MAX_CHARTS))
    {
        return samples[0].charts[c];
    }
    ChartRow row;
    row.ceiling = 0.0;
    row.series = 1u;
    row.kind = CHART_OTHER;
    row._rsvd0 = 0u;
    return row;
}

/// Column `j` of the *valid tail* (0 = oldest still valid) of series `k` of
/// chart `c`. The tail is right-aligned in the row, so this is where the
/// frame's "oldest first, right-aligned" rule is spent, once, for every reader
/// below. A negative return is a gap and every stage treats it as one.
float chart_sample(uint c, uint k, uint j)
{
    uint sc = live_samples();
    if (c >= MAX_CHARTS || k >= 2u || j >= sc)
    {
        return -1.0;
    }
    return samples[0].samples[(2u * c + k) * CHART_SAMPLES + (CHART_SAMPLES - sc + j)];
}

/// The whole-pixel step between two samples on screen, from the tunable.
float chart_step()
{
    return max(floor(CHART_PX_PER_SAMPLE), 1.0);
}

/// How many of the valid tail's columns fit across `inner_w` device px at
/// `chart_step()` per sample: the *visible window*. The newest column sits at
/// the right edge and the window is what both the ceiling and the drawing
/// see, so an old spike that has scrolled off no longer sets the scale.
uint visible_samples(float inner_w)
{
    uint fit = uint(floor(inner_w / chart_step())) + 1u;
    return min(live_samples(), fit);
}

/// What to divide a sample by: the row's ceiling, or — when it is 0 — the
/// maximum over the valid window, floored by RATE_FLOOR. Both series of a dual
/// chart share the result, so down and up stay comparable. A loop over at most
/// 256 floats per chart per texel is nothing at 90 × 56 px, and it is what
/// keeps the CPU out of presentation entirely.
float chart_ceiling(uint c, ChartRow row, uint visible)
{
    if (row.ceiling > 0.0)
    {
        return row.ceiling;
    }
    uint sc = live_samples();
    uint hidden = sc - min(visible, sc);
    uint series = clamp(row.series, 1u, 2u);
    float top = 0.0;
    for (uint k = 0u; k < series; ++k)
    {
        for (uint j = hidden; j < sc; ++j)
        {
            top = max(top, chart_sample(c, k, j));
        }
    }
    return max(top, RATE_FLOOR);
}

/// One colour per kind, and a second for the kinds the sidecar gives two
/// series. The kind comes from the samples row; nothing else about a chart's
/// look comes from the CPU.
float3 chart_rgb(uint kind, uint series)
{
    switch (kind)
    {
    case CHART_CPU:
        return CHART_CPU_RGB;
    case CHART_TEMP:
        return CHART_TEMP_RGB;
    case CHART_MEM:
        return CHART_MEM_RGB;
    case CHART_NET:
        return series == 0u ? CHART_NET_DOWN_RGB : CHART_NET_UP_RGB;
    case CHART_DISK:
        return series == 0u ? CHART_DISK_READ_RGB : CHART_DISK_WRITE_RGB;
    case CHART_GPU:
        return series == 0u ? CHART_GPU_AMD_RGB : CHART_GPU_RGB;
    default:
        return CHART_OTHER_RGB;
    }
}

/// The heat ramp at a height fraction (0 at the chart's base, 1 at its top).
/// Four stops, so the fill under a hot sample runs blue-green-yellow-red and
/// the fill under a cool one never leaves the blue — the heat-map look of a desktop
/// system monitor, in this file's own colours.
float3 heat_rgb(float h)
{
    float t = saturate(h) * 3.0;
    if (t < 1.0)
    {
        return lerp(HEAT_STOP0, HEAT_STOP1, t);
    }
    if (t < 2.0)
    {
        return lerp(HEAT_STOP1, HEAT_STOP2, t - 1.0);
    }
    return lerp(HEAT_STOP2, HEAT_STOP3, saturate(t - 2.0));
}

/// One series: the segment between the two samples bracketing this texel's x,
/// and the fill under it. A gap on either side draws neither, which is what
/// makes a sleeping dGPU a hole in the line rather than a dive to zero.
///
/// Samples sit `chart_step()` device px apart, the newest on the centre of
/// the inner area's last column, so every tick moves the curve a whole pixel
/// and a spike keeps its width. Only the `visible` newest columns are placed;
/// the rest of the tail is off the left edge.
float4 draw_series(float2 p, float2 org, float2 size, uint visible,
                   uint c, uint k, float ceiling, uint kind, float4 col)
{
    uint sc = live_samples();
    uint hidden = sc - visible;
    float step = chart_step();
    // The newest sample's x: the centre of the last whole pixel column inside
    // the frame. Snapping here is what keeps the sample grid on the pixel
    // grid whatever the layout gave the chart for an origin.
    float x_last = floor(org.x + size.x - 0.5) + 0.5;
    float x0 = x_last - float(visible - 1u) * step;
    float f = clamp((p.x - x0) / step, 0.0, float(visible - 1u));
    uint i0 = min(uint(floor(f)), visible - 2u);
    float t = saturate(f - float(i0));

    float v0 = chart_sample(c, k, hidden + i0);
    float v1 = chart_sample(c, k, hidden + i0 + 1u);
    if (v0 < 0.0 || v1 < 0.0)
    {
        return col;
    }

    float n0 = saturate(v0 / max(ceiling, 1e-6));
    float n1 = saturate(v1 / max(ceiling, 1e-6));
    float2 a = float2(x0 + float(i0) * step, org.y + size.y * (1.0 - n0));
    float2 b = float2(x0 + float(i0 + 1u) * step, org.y + size.y * (1.0 - n1));

    // Heat keys its colour to the texel's own height, so the ramp is the
    // chart's vertical axis and the top of a tall fill is the hot end of it.
    bool heat = kind == CHART_TEMP;
    float3 rgb = heat ? heat_rgb((org.y + size.y - p.y) / size.y)
                      : chart_rgb(kind, k);

    // The fill: everything below the segment at this texel's x, one texel of
    // ramp at the boundary so it meets the line without a seam.
    float y_line = lerp(a.y, b.y, t);
    col = ink(rgb, FILL_ALPHA * saturate(p.y - y_line + 0.5), col);

    // The line itself, from the distance to the segment.
    float half_w = LINE_PX * scale_px() * 0.5;
    return ink(rgb, coverage(sd_segment(p, a, b) - half_w), col);
}

/// One chart: a slab, a hairline frame, and one or two series over it. Never a
/// chip, never a press dip, never an action — `item_is_button` says so and the
/// click policy has no case for the kind.
float4 draw_chart(Element e, Item it, float2 p, float4 col)
{
    float cov = coverage(element_sdf(e, p));
    if (cov <= 0.0)
    {
        return col;
    }

    // The ground and its frame go down for every chart, including one whose
    // row the sidecar could not fill: an empty frame is the honest picture and
    // it holds the strip's shape while a sensor is missing.
    col = ink(CHART_BG, CHART_BG_ALPHA * cov, col);
    float fw = CHART_FRAME_PX * scale_px();
    float band = saturate(element_sdf(e, p) + fw + 0.5) * cov;
    col = ink(CHART_FRAME_RGB, CHART_FRAME_ALPHA * band, col);

    float2 org = e.rect.xy + fw;
    float2 size = max(e.rect.zw - 2.0 * fw, float2(1.0, 1.0));
    if (p.x < org.x || p.x > org.x + size.x || p.y < org.y || p.y > org.y + size.y)
    {
        return col;
    }

    // Fewer than two visible columns is a frame and nothing else — there is
    // no segment to draw through one sample.
    uint visible = visible_samples(size.x);
    if (visible < 2u)
    {
        return col;
    }

    // The item's whole payload is its row index; the kind comes from the row
    // itself, where the sidecar put it, as an integer — never round-tripped
    // through a float.
    uint c = item_chart_row(it);
    ChartRow row = chart_row(c);
    uint kind = row.kind;
    float ceiling = chart_ceiling(c, row, visible);
    uint series = clamp(row.series, 1u, 2u);
    // Ascending, so series 1 lands over series 0 — up over down, write over
    // read, the dGPU over the iGPU.
    for (uint k = 0u; k < series; ++k)
    {
        col = draw_series(p, org, size, visible, c, k, ceiling, kind, col);
    }
    return col;
}

/// Whether a line of the chart numbers is the chart being asked about: the
/// level, which is the chart under the pointer for the hover's popup and the
/// one clicked for the menu.
bool numbers_marked(Item it)
{
    Id level = menu_level_id();
    return id_tag(level) == ID_CHART && id_payload(level) == item_chart_row(it);
}

/// A line of the chart numbers' dressing: the swatch, a
/// MENU_MARK square centred in the left gutter in the chart's line colour —
/// the colour `draw_series` paints that chart with, split down the middle for
/// a two-series chart, a vertical `heat_rgb` ramp for TEMP — filled for the chart being asked about and outlined
/// for the rest; and on that line, the hover chip's wash, so the eye lands on
/// it.
float4 draw_numbers_line(Element e, Item it, float2 p, float4 col)
{
    float s = scale_px();
    bool marked = numbers_marked(it);
    if (marked)
    {
        col = ink(HOVER_RGB, HOVER_CHIP_ALPHA * coverage(element_sdf(e, p)), col);
    }
    uint c = item_chart_row(it);
    ChartRow row = chart_row(c);
    float m = MENU_MARK * s;
    float2 org = float2(e.rect.x + floor((MENU_GUTTER * s - m) * 0.5),
                        e.rect.y + floor((e.rect.w - m) * 0.5));
    float d = rounded_rect_sdf(p - org, float2(m, m), 2.0 * s);
    float stroke = MARK_STROKE * s * 0.5;
    float cov = marked ? coverage(d) : coverage(abs(d + stroke) - stroke);
    if (cov <= 0.0)
    {
        return col;
    }
    uint series = row.series >= 2u ? 1u : 0u;
    uint k = (series == 1u && p.x >= org.x + m * 0.5) ? 1u : 0u;
    // TEMP has no line colour of its own: `draw_series` keys it to height on
    // `heat_rgb`, so its swatch is that ramp, cold at the bottom and hot at
    // the top, the chart's vertical axis in miniature. A flat
    // `CHART_TEMP_RGB` swatch would read as DISK's read half.
    float3 rgb = row.kind == CHART_TEMP ? heat_rgb((org.y + m - p.y) / m)
                                        : chart_rgb(row.kind, k);
    return ink(rgb, cov, col);
}

/// An applet's level, as a hairline bar across the bottom of its chip: a
/// volume, a signal strength. A level below zero is "no level" and draws
/// nothing, which is what a sidecar with nothing to show publishes.
float4 draw_applet_level(Element e, Item it, float2 p, float4 col)
{
    float level = applet_level(it);
    if (level < 0.0)
    {
        return col;
    }
    float s = scale_px();
    float h = APPLET_LEVEL_H * s;
    float side = APPLET_LEVEL_SIDE * s;
    float w = max(e.rect.z - 2.0 * side, 1.0);
    float2 org = float2(e.rect.x + side, e.rect.y + e.rect.w - APPLET_LEVEL_INSET * s - h);
    float dim = (it.flags & IFLAG_DISABLED) != 0u ? DISABLED_ALPHA : 1.0;

    float track = coverage(rounded_rect_sdf(p - org, float2(w, h), h * 0.5));
    if (track <= 0.0)
    {
        return col;
    }
    col = ink(APPLET_LEVEL_BG_RGB, dim * APPLET_LEVEL_BG_ALPHA * track, col);
    // The fill, up to the level, with a one-texel edge so it meets the track
    // as cleanly as the track meets the chip.
    float edge = saturate(org.x + saturate(level) * w - p.x + 0.5);
    return ink(APPLET_LEVEL_RGB, dim * APPLET_LEVEL_ALPHA * track * edge, col);
}

/// A slider row's track, its fill and its knob. The geometry is
/// `slider_track`/`slider_travel` — the same two the drag arithmetic inverts,
/// which is why grabbing the knob does not make it jump.
float4 draw_slider(uint idx, Element e, Item it, float2 p, float4 col)
{
    float s = scale_px();
    float dim = (it.flags & IFLAG_DISABLED) != 0u ? DISABLED_ALPHA : 1.0;
    float4 track = slider_track(e.rect);
    float2 travel = slider_travel(e.rect);
    float knob = SLIDER_KNOB * s;
    // While this slider is held, the knob is where the pointer put it
    // (`drag_slider` wrote `drag_element`/`drag_value`); the item's value is
    // the sidecar's answer, a round trip behind, and the release lets it take
    // over again.
    float value = saturate(slider_position(it));
    if (private[0].pressed_element == idx
        && uint(private[0].drag_element) == idx + 1u)
    {
        value = saturate(private[0].drag_value);
    }
    float knob_x = travel.x + value * travel.y;

    float cov = coverage(rounded_rect_sdf(p - track.xy, track.zw, track.w * 0.5));
    if (cov > 0.0)
    {
        float fill = saturate(knob_x - p.x + 0.5);
        col = ink(lerp(SLIDER_TRACK_RGB, SLIDER_FILL_RGB, fill),
                  dim * SLIDER_TRACK_ALPHA * cov, col);
    }

    float hover = saturate(e.anim.x);
    float press = saturate(e.anim.y);
    float2 korg = float2(knob_x - knob * 0.5, track.y + track.w * 0.5 - knob * 0.5);
    float ka = coverage(rounded_rect_sdf(p - korg, float2(knob, knob), knob * 0.5)
                        + press * PRESS_INSET * s);
    if (ka > 0.0)
    {
        col = ink(saturate(SLIDER_KNOB_RGB + SLIDER_KNOB_HOVER_LIFT * hover
                           - PRESS_ALPHA * press),
                  dim * ka, col);
    }
    return col;
}

/// The captured still, letterboxed into its element and clipped by the same
/// rounded carve the visibility pass used.
float4 draw_preview(Element e, float2 p, float4 col)
{
    float cov = coverage(element_sdf(e, p));
    if (cov <= 0.0)
    {
        return col;
    }

    uint2 tdims;
    preview.GetDimensions(tdims.x, tdims.y);
    float2 src = float2(inputs.preview_size);
    if (src.x <= 0.0 || src.y <= 0.0 || tdims.x == 0u || tdims.y == 0u)
    {
        // The popup is up and no frame has landed yet: a dim plate, never a
        // hole through to the desktop.
        return ink(PREVIEW_PLACEHOLDER_RGB, PREVIEW_PLACEHOLDER_ALPHA * cov, col);
    }

    // The valid extent is a sub-rect of the fixed 2048² slot, so the uv walk
    // stops where the capture does.
    float2 t = saturate((p - e.rect.xy) / max(e.rect.zw, float2(1.0, 1.0)));
    float2 uv = t * (src / float2(tdims));
    float4 texel = preview.SampleLevel(tex_sampler, uv, 0.0);
    // Premultiplied on upload by the embassy, so this is a
    // straight `over` — no unpremultiply anywhere in this file.
    texel.rgb *= RELOAD_SMOKE_TINT;
    return over(texel * cov, col);
}

/// The dressing of one element: chip, urgent wash, dock furniture, menu-row
/// marks, the preview image. Text is a separate pass so it lands over all of it.
float4 draw_element(uint idx, Element e, float2 p, float4 col)
{
    if (idx == ELEM_PREVIEW)
    {
        return draw_preview(e, p, col);
    }

    Item it = item_of(e.item);
    // A dock entry's PINNED/RUNNING/FOCUSED/URGENT are the layout's, derived
    // from its favourite index and its ITEM_WINDOW children and left in the
    // element by `place` — the CPU writes none of the four (`dock_flags`).
    // Folded in here so that everything below reads one `flags` word.
    it.flags |= (it.kind == ITEM_DOCK) ? e.derived : 0u;

    // A chart is its own thing entirely: a slab, a frame and its series, and
    // none of the button dressing below.
    if (it.kind == ITEM_CHART)
    {
        return draw_chart(e, it, p, col);
    }

    // A line of the chart numbers: a swatch and, on the chart being asked
    // about, the hover chip's wash. Not a button, so none of the dressing
    // below; its two blocks are drawn by the text pass.
    if (it.kind == ITEM_CHART_LINE)
    {
        return draw_numbers_line(e, it, p, col);
    }

    // A slider is its own row entirely: a track, a fill and a knob, and none
    // of the button dressing below. Its label is drawn by the text pass, as
    // every other row's is.
    if (it.kind == ITEM_SLIDER)
    {
        return draw_slider(idx, e, it, p, col);
    }

    // A separator is a hairline and nothing else — no chip, no hover.
    if (it.kind == ITEM_MENU_ROW && (it.flags & IFLAG_SEPARATOR) != 0u)
    {
        float s = scale_px();
        float h = SEPARATOR_PX * s;
        float2 org = float2(e.rect.x, e.rect.y + floor((e.rect.w - h) * 0.5));
        return ink(SEPARATOR_RGB,
                   SEPARATOR_ALPHA * coverage(rounded_rect_sdf(p - org, float2(e.rect.z, h), 0.0)),
                   col);
    }

    bool enabled = (it.flags & IFLAG_DISABLED) == 0u;
    if (enabled && (idx == ELEM_BACK || item_is_button(it.kind)))
    {
        col = draw_chip(e, p, col);
    }

    // Urgent is a standing wash, not a hover one: it has to be visible with
    // the pointer nowhere near the bar.
    if ((it.flags & (IFLAG_URGENT | IFLAG_ATTENTION)) != 0u)
    {
        col = ink(URGENT_TINT, URGENT_ALPHA * coverage(element_sdf(e, p)), col);
    }

    if (it.kind == ITEM_DOCK)
    {
        col = draw_dock_marks(e, it, p, col);
    }
    if (it.kind == ITEM_MENU_ROW)
    {
        col = draw_row_marks(e, it, p, col);
    }
    if (it.kind == ITEM_APPLET)
    {
        col = draw_applet_level(e, it, p, col);
    }
    if (idx == ELEM_BACK)
    {
        // The back row's whole content is one left chevron in the gutter it
        // shares with the check column.
        float s = scale_px();
        float m = MENU_MARK * s;
        float2 org = float2(e.rect.x + floor((MENU_GUTTER * s - m) * 0.5),
                            e.rect.y + floor((e.rect.w - m) * 0.5));
        col = ink(MARK_INK, coverage(chevron_sdf(p, org, m, -1.0) - MARK_STROKE * s * 0.5), col);
    }

    return col;
}

[shader("compute")]
[numthreads(8, 8, 1)]
void bar_draw(uint3 gid: SV_DispatchThreadID)
{
    uint2 dims;
    dst.GetDimensions(dims.x, dims.y);
    if (gid.x >= dims.x || gid.y >= dims.y)
    {
        return;
    }

    uint2 texel = uint2(gid.xy);
    float2 p = float2(gid.xy) + 0.5;
    uint target = params.target;
    uint slab = slab_of(target);
    Element ground = private[0].elements[slab];

    // The ground, and with it the surface's translucency. Outside its shape
    // (a popup's rounded corners) the surface is genuinely transparent —
    // written, not skipped, because the swapchain image is not cleared.
    float cov = element_live(ground, target) ? coverage(element_sdf(ground, p)) : 0.0;
    if (cov <= 0.0)
    {
        dst.Store(texel, float4(0.0, 0.0, 0.0, 0.0));
        return;
    }

    bool is_popup = slab == ELEM_POPUP;
    float alpha = (is_popup ? POPUP_OPACITY : slab_alpha()) * cov;
    float3 rgb = (is_popup ? POPUP_RGB : SLAB_RGB) * RELOAD_SMOKE_TINT;
    float4 col = float4(rgb * alpha, alpha);

    if (!is_popup)
    {
        col = wake_light(p, col);
    }

    if (is_popup && POPUP_EDGE_PX > 0.0)
    {
        // A hairline just *inside* the boundary, so it never grows the popup's
        // footprint and never fights the carve.
        float d = element_sdf(ground, p);
        float band = saturate(d + POPUP_EDGE_PX * scale_px() + 0.5) * cov;
        col = ink(POPUP_EDGE_RGB, POPUP_EDGE_ALPHA * band, col);
    }

    uint n = live_elements();
    float halo = HOVER_HALO_PX * scale_px() + 1.0;

    // Dressing first, in index order...
    for (uint i = 0u; i < n; ++i)
    {
        if (i == slab)
        {
            continue;
        }
        Element e = private[0].elements[i];
        if (!element_live(e, target) || !rect_near(e.rect, p, halo) || element_clipped(e, p))
        {
            continue;
        }
        col = draw_element(i, e, p, col);
    }

    // ...then text and icons over all of it. `Element.item` and
    // `Element.block` were resolved once by bar_apply, so this pass never
    // scans a table to find out what an element carries.
    for (uint i = 0u; i < n; ++i)
    {
        Element e = private[0].elements[i];
        if (!element_live(e, target) || !rect_near(e.rect, p, 0.0) || element_clipped(e, p))
        {
            continue;
        }
        if (element_sdf(e, p) > 0.0)
        {
            continue;
        }
        if (e.item < MAX_ITEMS)
        {
            col = draw_item_content(e, item_of(e.item), p, col);
        }
        else if (e.block < MAX_BLOCKS)
        {
            // An element with a block but no item: the preview's title. Its
            // block is centred in its rect when it fits; a title wider than
            // the popup (most of Firefox's) is left-aligned instead and fades
            // out over the rect's last TITLE_FADE logical px, so it reads as
            // cut, not as clipped mid-glyph at both ends.
            float2 sz = block_size(e.block);
            bool wide = sz.x > e.rect.z;
            float2 org = wide
                ? float2(e.rect.x, e.rect.y + floor((e.rect.w - sz.y) * 0.5))
                : e.rect.xy + floor((e.rect.zw - sz) * 0.5);
            float dim = 1.0;
            if (wide)
            {
                float right = e.rect.x + e.rect.z;
                dim = saturate((right - p.x) / max(TITLE_FADE * scale_px(), 1.0));
            }
            col = draw_block_at(e.block, org, p, col, dim);
        }
    }

    dst.Store(texel, col);
}

// ===========================================================================
// The oracle's eighth entry point (docs/abi.md, *Bindings and the oracle*), compiled in only
// when the test asks for it.
//
// `#include` rather than `import`: `bar_check` walks `private` and names what
// it finds with this file's own `element_name`/`item_name` family, and none of
// that is a module's public surface. Production's `ENTRIES` does not list it
// and `bar/build.rs` never defines `CAUSTICS_BAR_CHECK`, so the baked SPIR-V is
// the seven entries of `ENTRIES`.
// ===========================================================================

#ifdef CAUSTICS_BAR_CHECK
#include "bar_check.slang"
#endif
