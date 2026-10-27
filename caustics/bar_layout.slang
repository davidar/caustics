// The layout engine: the element table and how it is filled. What an item's
// rect is made of (its blocks, its kind's chip), the wings of the strip, the
// placement of one element and the witness of it, the hit test, a row's body
// wrapped into lines, and the springs every element's hover and press ride.
//
// The bar's own, and named after no subject: a subject lays its popup out by
// calling `place` here, and the wings walk the item table by kind.

module bar_layout;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import shape;
import bar_state;
import bar_popups;

/// Whether this item shows its text block where it is laid out. A dock entry
/// and a tray item carry a title block too, but it belongs to a preview popup
/// or a tooltip, never to the strip itself.
///
/// An applet's and a slider's labels are *optional* — a sidecar may publish a
/// volume percentage beside its icon or only the icon — so for those two the
/// question is about the item, not the kind, which is why this takes one.
public bool item_has_label(Item it)
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
public bool item_is_button(uint kind)
{
    return kind == ITEM_DOCK || kind == ITEM_OVERVIEW
        || kind == ITEM_MENU_ROW || kind == ITEM_APPLET
        || (kind == ITEM_CLOCK && CLOCK_SHOW_CALENDAR);
}

/// Which kinds are rows of the open menu: a menu row, and a slider a sidecar
/// published under one (docs/abi.md, *Rows*). The popup's whole loop keys on
/// this, so a slider descends, measures and hit-tests exactly as a row does.
public bool menu_row_kind(uint kind)
{
    return kind == ITEM_MENU_ROW || kind == ITEM_SLIDER;
}

/// One menu row's height: a separator's hairline row, a slider's taller row
/// with its track band, or a plain row.
public float menu_row_height(Item it)
{
    float s = scale_px();
    if (it.kind == ITEM_SLIDER)
    {
        return MENU_SLIDER_H * s;
    }
    return ((it.flags & IFLAG_SEPARATOR) != 0u ? MENU_SEP_H : MENU_ROW_H) * s;
}

public float item_icon_w(Item it)
{
    return it.icon_block == NO_INDEX ? 0.0 : block_size(it.icon_block).x;
}

public float item_label_w(Item it)
{
    return item_has_label(it) ? block_size(it.block).x : 0.0;
}

/// An item's *second* text block, when its kind draws one and the tunables say
/// to: the clock's date, composed and shaped beside its time. NO_INDEX otherwise,
/// which is what makes CLOCK_SHOW_DATE a width change as well as a paint one.
public uint item_aux_block(Item it)
{
    return (it.kind == ITEM_CLOCK && CLOCK_SHOW_DATE) ? it.aux_block : NO_INDEX;
}

public float item_aux_w(Item it)
{
    uint block = item_aux_block(it);
    return block == NO_INDEX ? 0.0 : block_size(block).x;
}

/// Icon, aux block and label, laid end to end with a gap between each pair
/// that exists. The layout sizes a chip around this and the draw pass places
/// the same blocks by the same arithmetic, which is why content never drifts
/// out of its own carve.
public float item_content_w(Item it)
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
public uint item_primary_block(Item it)
{
    return item_has_label(it) ? it.block : it.icon_block;
}

/// One item's width on the bar. A button-shaped item is never narrower than a
/// square chip, so an entry whose icon failed to load is still a target the
/// same size as its neighbours.
public float bar_item_w(Item it)
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

/// The `layout: <name> target=none rect=gone` line, one per element the
/// previous frame used and this one does not.
///
/// The name is the one the
/// element was last witnessed under, kept whole in the element — an app id's
/// bytes are no longer in the facts by the time its dock entry leaves.
public void witness_departures()
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
        Args a = witness_begin(WITNESS_DEBUG, gone_literal(nm.slot));
        arg(a, nm.a0);
        arg(a, nm.a1);
        arg(a, nm.a2);
        private[0].elements[i].said_present = 0u;
    }
}

/// Whether `p` is outside the band a scrolled menu's rows are drawn in
/// (`menu_clip`). Only a popup element that lays out an item — a menu
/// row — is ever cut; the back row, the preview and its title carry none. The
/// visibility carve, the draw and the hit test all ask, so a half row at
/// either end of the band is neither drawn nor clicked past it.
public bool element_clipped(Element e, float2 p)
{
    float2 band = private[0].menu_clip;
    if (e.target != TARGET_POPUP || e.item >= MAX_ITEMS || band.y <= band.x)
    {
        return false;
    }
    return p.y < band.x || p.y >= band.y;
}

/// Is `e` drawn into `target` this frame?
public bool element_live(Element e, uint target)
{
    return e.target == target && (e.flags & EFLAG_PRESENT) != 0u;
}

/// The element that owns `p` in `target`, or NO_INDEX. Reverse index order, so
/// the same "later index wins" rule the visibility pass uses decides it.
public uint element_at(uint target, float2 p)
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
public uint press_target(uint target, float2 pos)
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
public float body_wrap_w(Item it)
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
public uint body_max_lines()
{
    return min(max(TOAST_BODY_LINES, MENU_BODY_LINES), MAX_BODY_LINES);
}

/// Body lines this popup shows of a row: TOAST_BODY_LINES in a toast,
/// MENU_BODY_LINES in a menu, never more than the row has.
public uint body_shown(Item it)
{
    uint cap = MENU_BODY_LINES;
    if (private[0].popup_kind == POPUP_TOAST)
    {
        // A toast's row shows the lines it opened with and never more: its
        // rect was sized for them, and the row carries that cap
        // (`item_body_cap`). A re-wrap that finds fewer leaves the rest of
        // the rect empty; one that finds more cuts.
        uint fixed = item_body_cap(it);
        cap = fixed == NO_INDEX ? TOAST_BODY_LINES : fixed;
    }
    return min(item_body_line_count(it), min(cap, MAX_BODY_LINES));
}

/// Whether the last line shown ends in the ellipsis: lines left unshown, or
/// body left unwrapped (unshaped, unbuilt, or past the lines laid out).
public bool body_cut(Item it)
{
    uint shown = body_shown(it);
    return shown > 0u
        && (shown < item_body_line_count(it) || (it.body_lines & BODY_MORE) != 0u);
}

/// The ellipsis' width, px, or 0 while it has none.
public float ellipsis_w()
{
    uint b = private[0].ellipsis_block;
    return b < MAX_BLOCKS ? block_size(b).x : 0.0;
}

/// Pen x of glyph `k` of a block's run, px; the run's whole advance at
/// `k == glyph_count`, so `run_pen(blk, k + 1)` is glyph k's far edge.
public float run_pen(TextBlock blk, uint k)
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
public bool run_space(TextBlock blk, uint k)
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
public void body_line_fit(uint block, uint glyph, uint count, float limit,
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
public bool push_body_line(uint block, uint glyph, uint count, float width, float wrap_w)
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
public void wrap_body(uint i)
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
public void wrap_bodies()
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

/// Say how many body lines a row placed at `elem` shows, once per change of
/// row or count: `body: slot=… row=… lines=… cut=…`, INFO
/// in a toast — one line per notification shown — and DEBUG in a menu, which
/// lays out a dozen at a time. The layout's rect says the same in px; this
/// says it in lines.
public void witness_body(uint elem, Item it, bool open, bool toast)
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
    Args a = witness_begin(toast ? WITNESS_INFO : WITNESS_DEBUG,
                           getStringHash("body: slot={0} row={1} lines={2} cut={3}"));
    arg(a, id_aux(it.id));
    arg(a, id_payload(it.id));
    arg(a, shown);
    arg(a, cut ? 1u : 0u);
}

/// How many ordering passes one kind takes on a wing. Applets order among
/// themselves by `APPLET_ORDER` in `bar_tunables.slang`, so their kind takes
/// one pass per entry of it — which is what lets a wing array name ITEM_APPLET
/// once and every applet sidecar land where the tunables say, with no edit here
/// and none in the wing arrays either. Every other kind takes one pass.
public uint kind_passes(uint kind)
{
    return kind == ITEM_APPLET ? uint(APPLET_ORDER.getCount()) : 1u;
}

/// Whether item `it` is placed on pass `sub` of kind `kind`.
public bool item_in_pass(Item it, uint kind, uint sub)
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
public float center_gap(uint prev_kind, uint kind)
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
public float center_span(uint keep)
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

// ---------------------------------------------------------------------------
// Springs.
// ---------------------------------------------------------------------------

/// One critically damped spring step, semi-implicit (symplectic) Euler:
/// velocity first, then value with the *new* velocity. `sv` is (value,
/// velocity); the return is the stepped pair, parked exactly on the target
/// once the motion stops being visible so an idle bar stops asking for frames.
public float2 spring_step(float2 sv, float target, float dt)
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

/// Step every element's hover and press springs; returns true while any of
/// them is still moving. `Element.anim` is caustic-private (the ABI says so):
/// x = hover 0..1, y = press 0..1, z and w their velocities.
///
/// The two slabs are skipped: neither has a hover chip, so neither has a
/// spring to step.
public bool integrate_springs(float dt)
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

/// The element that is the ground of `target`: it covers the whole surface, so
/// there is always an owner and never a fall-through.
public uint slab_of(uint target)
{
    return target == TARGET_POPUP ? ELEM_POPUP : ELEM_BAR;
}

/// Write one element's layout, preserving its springs. Hover/press flags are
/// re-derived here from the ids the sample pass published, so the hit test, the
/// witness and the tint the draw pass applies always agree.
///
/// Two things about an element are said after the layout rather than here,
/// because they are a subject's to answer: its `layout:` line, which names it
/// (`witness_placed`, in the order this records), and the flags a subject
/// derives from the item table (`Element.derived`, zero here; the dock's
/// running and focus marks are `dock_flags`).
public void place(uint idx, uint target, float4 rect, float radius, uint block, uint item, bool present)
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
    private[0].elements[idx].derived = 0u;
    private[0].elements[idx].derived_a = 0u;

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

    if (!present)
    {
        return; // the departure walk at the end of `layout` says so, once
    }
    uint n = private[0].placed_count;
    if (n < MAX_ELEMENTS)
    {
        private[0].placed[n] = idx;
        private[0].placed_count = n + 1u;
    }
}

/// Every element of every target, from the tunables and the item table. Immediate
/// mode: the elements are recomputed every frame, and what persists is in
/// `Private` (the springs, the popup and toast state, the scroll, the dwell
/// and the drag).
///
/// This is the first half, the bar's: every element starts absent, then the
/// slab and the three wings. Returns the next free element; the popup's half
/// is whichever subject's popup is up, and `layout` in `bar.slang` asks it.
public uint layout_strip()
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
    private[0].placed_count = 0u;

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
    return next;
}

/// What the pointer is on, resolved to an item once a frame, after the
/// layout (`Private.hover_item`), so every step that asks reads one answer.
public void resolve_hover_item()
{
    uint h = private[0].hover_element;
    uint hover_elem = h < live_elements() ? h : NO_INDEX;
    private[0].hover_item = hover_elem == NO_INDEX ? NO_INDEX : private[0].elements[hover_elem].item;
}

/// A button went down at `pos` on `target`: the pointer is kept, and so is
/// what the press landed on, by element and by item.
public void press(uint target, float2 pos)
{
    set_pointer(target, pos);
    uint hit = press_target(target, pos);
    private[0].pressed_element = hit;
    // What the press landed *on*, not just where: a sidecar's frame may
    // re-lay the menu out between the press and the release, and an
    // element index alone would then fire the row that moved into its
    // place. The caustic's own elements — a slab, the back row —
    // carry no item and so record the zero id, which still matches itself.
    set_pressed_id(item_id_of_element(hit));
}

/// The button came up at `pos` on `target`: the element the release clicks,
/// or NO_INDEX. The press is forgotten by `release_end`, after the click.
public uint release(uint target, float2 pos)
{
    set_pointer(target, pos);
    uint hit = press_target(target, pos);
    uint pressed = private[0].pressed_element;
    // The same element *and* the same item: a release across a re-layout
    // fires nothing rather than firing whatever took that slot — which is
    // what lets an open menu survive its rows changing under it.
    bool same_item = pressed_id().eq(item_id_of_element(hit));
    return (pressed != NO_INDEX && pressed == hit && same_item) ? pressed : NO_INDEX;
}

/// Forget the press, once its release has been answered.
public void release_end()
{
    private[0].pressed_element = NO_INDEX;
    set_pressed_id(id_zero());
}

/// The pointer left `target`.
public void pointer_leave(uint target)
{
    // The pointer is on neither surface as far as this target knows. Only
    // the target that was left may say so — the pointer moving from the
    // bar into the popup leaves the bar, and the popup has it.
    if (private[0].pointer_target == target)
    {
        private[0].pointer = float2(-1.0, -1.0);
        private[0].pointer_target = NO_INDEX;
    }
    // Clear hover only if it belongs to the target that was left: the
    // pointer moving from the bar into the popup leaves the bar, and the
    // popup's hover must survive it.
    uint h = private[0].hover_element;
    if (h < live_elements() && private[0].elements[h].target == target)
    {
        private[0].hover_element = NO_INDEX;
    }
    // A button held while the pointer leaves is a button that will never
    // click; releasing it here is what stops a stuck press (docs/architecture.md,
    // *Rules*: a toolkit that drops a pointer-leave).
    uint p = private[0].pressed_element;
    if (p < live_elements() && private[0].elements[p].target == target)
    {
        private[0].pressed_element = NO_INDEX;
        set_pressed_id(id_zero());
    }
}

/// The popup's slab, `ELEM_POPUP`, and whether the popup is open: it exists
/// only while the compositor says it is mapped *and* the
/// CPU has said how big it came out. Anything less and its elements are
/// absent, which is what keeps the carve and the draw off an unmapped
/// surface. `popup` is the size it came out.
public bool layout_popup(out float2 popup)
{
    popup = float2(inputs.popup_size);
    bool open = private[0].popup_state == POPUP_UP && popup.x > 0.0 && popup.y > 0.0;
    place(ELEM_POPUP, TARGET_POPUP, float4(0.0, 0.0, popup), POPUP_RADIUS * scale_px(),
          NO_INDEX, NO_INDEX, open);
    return open;
}

/// The layout is done: `next` is the first element it did not use.
public void layout_end(uint next)
{
    private[0].element_count = max(next, ELEM_FIRST_FREE);
}
