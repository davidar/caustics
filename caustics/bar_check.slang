// The oracle, as a GPU pass (docs/abi.md, *Bindings and the oracle*).
//
// **Not a module**: this file is `#include`d at the bottom of `bar.slang`
// under `-DCAUSTICS_BAR_CHECK`, so it sees what that file sees — `private`
// and `item_of` (public in `bar_state`), `element_name` and the whole naming
// family — and imports the one subject it checks (below). Production's
// `ENTRIES` never names `bar_check` and the baked SPIR-V never carries it; the
// oracle test compiles the caustic a second time with the define and loads
// this one entry point beside the seven.
//
// What it does is walk `private` after `bar_build`, `bar_compose` and
// `bar_shape` and push witnesses into the ring the embassy already drains:
//
//   check: <invariant>          one WARN per violated structural rule
//   check: item <name> …        one DEBUG per built item
//   check: block N kind=K px=P  one DEBUG per text and icon block
//
// The invariants come first so a full ring loses a *description* rather than a
// complaint. The Rust half (`bar/src/oracle/tests.rs`) keeps the two checks
// that must not be the caustic checking itself: every block's px against its
// own text parse of `TEXT_PX` and `ICON_LOGICAL`, and every item line
// rendering through the generic printer with no `?` — the check that every built item
// can be named.
//
// There is no second, CPU-built table to compare against: these checks are
// invariants of the caustic's own table.

// The chart checks draw a chart and read the samples body, which are the
// charts' subject's, so this file imports `sysmon` itself: `bar.slang`
// names no subject, and every other name here is in its scope.
import sysmon;

/// `check: item <name> kind={n} parent={n+1}` — one per built item, in the
/// same name family the `layout:` lines use.
///
/// The parent is an **index** and not a name: a name-by-name literal table
/// would be every name family squared, and what the Rust half checks is that
/// the item's own name renders, which is the thing that can be wrong.
uint check_item_literal(uint slot)
{
    switch (slot)
    {
    case NAME_BAR:      return getStringHash("check: item ELEM_BAR kind={0} parent={1}");
    case NAME_POPUP:    return getStringHash("check: item ELEM_POPUP kind={0} parent={1}");
    case NAME_BACK:     return getStringHash("check: item ELEM_BACK kind={0} parent={1}");
    case NAME_PREVIEW:  return getStringHash("check: item ELEM_PREVIEW kind={0} parent={1}");
    case NAME_CLOCK:    return getStringHash("check: item clock kind={0} parent={1}");
    case NAME_OVERVIEW: return getStringHash("check: item overview kind={0} parent={1}");
    case NAME_CAFFEINE: return getStringHash("check: item caffeine kind={0} parent={1}");
    case NAME_WINDOW:
        return getStringHash("check: item window-{0:u64} kind={2} parent={3}");
    case NAME_ENTRY:
        return getStringHash("check: item {0:str} kind={2} parent={3}");
    case NAME_DOCK_ROW:
        return getStringHash("check: item dock-{0:s} kind={1} parent={2}");
    case NAME_CHART:
        return getStringHash("check: item chart-{0:s} kind={1} parent={2}");
    case NAME_CHART_N:
        return getStringHash("check: item chart-{0:s}-{1} kind={2} parent={3}");
    case NAME_APPLET:
        return getStringHash("check: item applet-{0:str} kind={2} parent={3}");
    case NAME_APPLET_N:
        return getStringHash("check: item applet-{0:str}-{2} kind={3} parent={4}");
    case NAME_CAL_TITLE:
        return getStringHash("check: item calendar-{0:s}-{1} kind={2} parent={3}");
    case NAME_CAL_WEEKDAY:
        return getStringHash("check: item calendar-weekday-{0:s} kind={1} parent={2}");
    case NAME_CAL_DAY:
        return getStringHash("check: item calendar-day-{0} kind={1} parent={2}");
    case NAME_NUMBERS:
    case NAME_NUMBERS_BARE:
    case NAME_NUMBERS_NONE:
        return getStringHash("check: item numbers-{0:s} kind={3} parent={4}");
    default:
        return getStringHash("check: item ELEM_ITEM kind={0} parent={1}");
    }
}

[shader("compute")]
[numthreads(1, 1, 1)]
void bar_check(uint3 gid: SV_DispatchThreadID)
{
    if (gid.x != 0u || gid.y != 0u || gid.z != 0u)
    {
        return;
    }
    ensure_private();

    uint items = private[0].built_items;
    uint blocks = private[0].built_blocks;
    uint glyphs = private[0].built_glyphs;

    // -- the capacities, which a table that does not fit *reports* ----------
    //
    // A build that ran out of room is not a failure of this pass: the drop is
    // in the `built:` and `items: table capped` witnesses and the invariant is
    // only that nothing went past a bound.
    if (items > MAX_ITEMS)
    {
        witness1(WITNESS_WARN, getStringHash("check: {0} items is past MAX_ITEMS"), items);
    }
    if (blocks > MAX_BLOCKS)
    {
        witness1(WITNESS_WARN, getStringHash("check: {0} blocks is past MAX_BLOCKS"), blocks);
    }
    if (glyphs > MAX_GLYPHS)
    {
        witness1(WITNESS_WARN, getStringHash("check: {0} glyph slots is past MAX_GLYPHS"),
                 glyphs);
    }
    if (private[0].element_count > MAX_ELEMENTS)
    {
        witness1(WITNESS_WARN, getStringHash("check: {0} elements is past MAX_ELEMENTS"),
                 private[0].element_count);
    }

    // -- the tunables an edit can get wrong without a compile error ---------
    //
    // Slang compiles `TEXT_PX[ITEM_KIND_COUNT]` with fewer initialisers than
    // kinds and zero-fills the rest, and `snap_px` clamps a zero to
    // MIN_TEXT_PX, so an edit missing one row draws that kind's text at ~6 px with no
    // complaint anywhere.
    for (uint k = 0u; k < ITEM_KIND_COUNT; ++k)
    {
        if (!(TEXT_PX[k] > 0.0))
        {
            witness1(WITNESS_WARN, getStringHash("check: TEXT_PX[{0}] is not a size"), k);
        }
    }

    uint n = live_items();
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        // Every window hangs off a dock entry. A window under nothing is a
        // window no click can ever reach and no `layout:` line can name.
        if (it.kind == ITEM_WINDOW)
        {
            bool ok = it.parent < n && private[0].items[it.parent].kind == ITEM_DOCK;
            if (!ok)
            {
                witness1(WITNESS_WARN,
                         getStringHash("check: item {0} is a window hanging off no dock entry"),
                         i);
            }
        }
        // `g` is the payload word no kind declares (`bar_model.slang`). A pass
        // that wrote it has a meaning nothing reads.
        if (it.g != 0.0)
        {
            witness1(WITNESS_WARN,
                     getStringHash("check: item {0} wrote g, which no kind declares"), i);
        }
        // An item whose live value is read from the rows arena names a row
        // that is **in** its slot's region (docs/abi.md, *The arenas*). A
        // `fact_row` past the region's own `row_count` would read a zeroed row
        // — a slider stuck at its minimum, an applet with no level — and
        // nothing else in the frame would say so.
        uint at = item_fact_row(it);
        if (at != NO_INDEX)
        {
            uint slot = at / MAX_ROWS;
            uint index = at % MAX_ROWS;
            if (slot >= MAX_SLOTS || index >= rows_header(slot).row_count)
            {
                witness2(WITNESS_WARN,
                         getStringHash("check: item {0} reads row {1}, which is outside its region"),
                         i, at);
            }
        }
    }

    // -- the TEMP swatch is the strip's ramp ---------------------------------
    //
    // The numbers' swatch for a temperature is `heat_rgb` up its height, cold
    // at the bottom and hot at the top, because that is how `draw_series`
    // paints the chart; a flat orange would read as DISK. Layout has not run here, so the line is drawn into a rect of its
    // own and two texels inside the swatch's outline are compared: the top
    // edge's red outweighs its blue and the bottom edge's blue its red.
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != ITEM_CHART_LINE || chart_row(item_chart_row(it)).kind != CHART_TEMP)
        {
            continue;
        }
        float s = scale_px();
        float m = MENU_MARK * s;
        float h = CHART_NUMBERS_LINE_PX * s;
        Element e = (Element)0;
        e.rect = float4(0.0, 0.0, 200.0 * s, h);
        e.item = i;
        float2 org = float2(floor((MENU_GUTTER * s - m) * 0.5), floor((h - m) * 0.5));
        float inset = MARK_STROKE * s * 0.5;
        float4 top = draw_numbers_line(e, it, org + float2(m * 0.5, inset), float4(0.0));
        float4 bottom = draw_numbers_line(e, it, org + float2(m * 0.5, m - inset), float4(0.0));
        if (!(top.r > top.b && bottom.b > bottom.r))
        {
            witness1(WITNESS_WARN,
                     getStringHash("check: swatch temp item {0} is not the heat ramp"), i);
        }
        else
        {
            witness1(WITNESS_DEBUG, getStringHash("check: swatch temp item {0} is the ramp"), i);
        }
    }

    // -- a chart's fill is its colour under the newest sample ---------------
    //
    // `draw_series` paints the fill below the line in `chart_rgb` and leaves
    // the texels above the line to the ground. Layout has not run here, so
    // each one-series chart is drawn into a rect of its own and two texels of
    // its newest column are compared: one halfway down the fill, one halfway
    // between the line and the top. The first carries the chart's colour and
    // the second does not. A chart whose newest sample is a gap, near zero or
    // near its ceiling has no two such texels and is passed over, as is TEMP,
    // whose ramp the swatch check above reads.
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != ITEM_CHART)
        {
            continue;
        }
        uint c = item_chart_row(it);
        ChartRow row = chart_row(c);
        if (row.kind == CHART_TEMP || row.series != 1u || live_samples() < 2u)
        {
            continue;
        }
        float s = scale_px();
        float fw = CHART_FRAME_PX * s;
        Element e = (Element)0;
        e.rect = float4(0.0, 0.0, 80.0 * s, 24.0 * s);
        e.item = i;
        float2 org = e.rect.xy + fw;
        float2 size = e.rect.zw - 2.0 * fw;
        uint visible = visible_samples(size.x);
        if (visible < 2u)
        {
            continue;
        }
        float ceiling = chart_ceiling(c, row, visible);
        float v = chart_sample(c, 0u, live_samples() - 1u);
        float level = saturate(v / max(ceiling, 1e-6));
        if (v < 0.0 || level < 0.1 || level > 0.9)
        {
            continue;
        }
        // The newest column's x, snapped as `draw_series` snaps it.
        float x = floor(org.x + size.x - 0.5) + 0.5;
        float y_line = org.y + size.y * (1.0 - level);
        float4 lit = draw_chart(e, it, float2(x, lerp(y_line, org.y + size.y, 0.5)), float4(0.0));
        float4 bare = draw_chart(e, it, float2(x, lerp(org.y, y_line, 0.5)), float4(0.0));
        float3 toward = chart_rgb(row.kind, 0u) - CHART_BG;
        if (dot(lit.rgb - bare.rgb, toward) <= 0.0)
        {
            witness1(WITNESS_WARN,
                     getStringHash("check: chart item {0} fill is not its colour under the newest sample"), i);
        }
        else
        {
            witness1(WITNESS_DEBUG,
                     getStringHash("check: chart item {0} fill is its colour under the newest sample"), i);
        }
    }

    // -- the glyph runs: sorted, disjoint, inside the buffer ----------------
    //
    // Two runs overlapping is two labels writing each other's positions, which
    // reads as text that flickers between two strings. The build walks the
    // block table in order and hands out slots end to end, so the check is
    // that each run starts where the last one ended or later.
    uint nblocks = live_blocks();
    uint last_end = 0u;
    for (uint b = 0u; b < nblocks; ++b)
    {
        TextBlock blk = private[0].blocks[b];
        if (blk.kind != BLOCK_TEXT || blk.px <= 0.0)
        {
            continue;
        }
        uint start = blk.first_glyph;
        uint end = start + max(blk.b, 1u);
        if (start < last_end)
        {
            witness2(WITNESS_WARN,
                     getStringHash("check: a glyph run at {0} overlaps the one ending at {1}"),
                     start, last_end);
        }
        if (end > MAX_GLYPHS)
        {
            witness1(WITNESS_WARN,
                     getStringHash("check: a glyph run ends at {0}, past MAX_GLYPHS"), end);
        }
        last_end = max(end, last_end);
    }

    // -- what was built, described ------------------------------------------
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        NameRef nm = item_name(i);
        Args a = witness_begin(WITNESS_DEBUG, check_item_literal(nm.slot));
        name_args(a, nm);
        arg(a, it.kind);
        arg(a, it.parent);
    }
    for (uint b = 0u; b < nblocks; ++b)
    {
        TextBlock blk = private[0].blocks[b];
        // Which *role* the block has, because that is what decides its size
        // and nothing in the block says it: a **label** is sized from
        // `TEXT_PX` by its item's kind, and an icon — curves, a slot, or the
        // placeholder letter a dock entry wears, which is a `BLOCK_TEXT` like
        // any other — is always the icon's own square.
        uint icon = 0u;
        for (uint i = 0u; i < n; ++i)
        {
            if (private[0].items[i].icon_block == b)
            {
                icon = 1u;
                break;
            }
        }
        Args a = witness_begin(
            WITNESS_DEBUG,
            getStringHash("check: block {0} kind={1} px={2:f} icon={3} run={4}+{5} runflags={6}"));
        arg(a, b);
        arg(a, blk.kind);
        argf(a, blk.px);
        arg(a, icon);
        // What the shape pass made of this block: where its run sits, how long
        // it came back and what it says about itself. Three numbers the Rust
        // half needs in order to read the run out of the glyph buffer at all —
        // the block table is the caustic's private buffer and nothing else
        // names it. They are here because the shaper can draw wrong on one GPU
        // and right on another from the same SPIR-V (see `faceFor`'s loop in
        // `shape.slang`), and only the bar's own passes on the bar's own GPU
        // show it.
        arg(a, blk.first_glyph);
        arg(a, blk.glyph_count);
        arg(a, blk.flags);
    }
}
