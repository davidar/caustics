// The drawing primitives every subject shares. Everything written here is
// PREMULTIPLIED (the note at the top of `bar.slang`): source-over, coverage
// from a signed distance, a tinted ink, the hover chip, one block (a shaped
// run, a glyph, an image slot) and an item's content laid out the way the
// layout measured it, a row's body line by line.

module bar_paint;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import shape;
import text;
import bar_state;
import bar_layout;

/// Distance from `p` to the segment ab. The building block of the two SDF
/// symbols this file draws instead of shipping a symbol font of its own.
public float sd_segment(float2 p, float2 a, float2 b)
{
    float2 pa = p - a;
    float2 ba = b - a;
    float h = saturate(dot(pa, ba) / max(dot(ba, ba), 1e-6));
    return length(pa - ba * h);
}

/// A check mark inscribed in the `m`-sided box at `org`.
public float check_sdf(float2 p, float2 org, float m)
{
    float2 a = org + float2(0.20, 0.52) * m;
    float2 b = org + float2(0.41, 0.74) * m;
    float2 c = org + float2(0.82, 0.26) * m;
    return min(sd_segment(p, a, b), sd_segment(p, b, c));
}

/// A chevron in the `m`-sided box at `org`, pointing right when `dir` is
/// positive and left when it is negative.
public float chevron_sdf(float2 p, float2 org, float m, float dir)
{
    float tip_x = dir > 0.0 ? 0.66 : 0.34;
    float back_x = dir > 0.0 ? 0.36 : 0.64;
    float2 tip = org + float2(tip_x, 0.50) * m;
    float2 top = org + float2(back_x, 0.24) * m;
    float2 bot = org + float2(back_x, 0.76) * m;
    return min(sd_segment(p, top, tip), sd_segment(p, tip, bot));
}

/// Premultiplied source-over. The only compositing operator in this file.
public float4 over(float4 src, float4 dst_col)
{
    return src + dst_col * (1.0 - src.a);
}

/// Coverage of a shape at a texel, from its signed distance: a one-pixel
/// linear ramp across the boundary. Cheap, and enough — every edge here is
/// either axis-aligned or a generous radius.
public float coverage(float d)
{
    return saturate(0.5 - d);
}

/// Composite a flat colour at `alpha` coverage, tinted by the reload smoke.
public float4 ink(float3 rgb, float alpha, float4 col)
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
public float4 draw_chip(Element e, float2 p, float4 col)
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

/// The band a menu row's label, icon and marks are centred in: the whole row,
/// or — for a row with a body under it — the plain row's height at its top,
/// so the label stays where a bodiless row would put it.
public float row_band_h(Element e, Item it)
{
    return body_shown(it) > 0u ? min(MENU_ROW_H * scale_px(), e.rect.w) : e.rect.w;
}

/// One body line, drawn: its glyphs walked across its paragraph's chunks from
/// `org` (the line box's top-left), and — when `ell` — only the ones that
/// leave room for the ellipsis, which follows them.
public float4 draw_body_line(BodyLine line, bool ell, float2 org, float2 p, float4 col, float dim)
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
public float4 draw_body(Element e, Item it, float x, float2 p, float4 col, float dim)
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
public float4 draw_block_at(uint b, float2 org, float2 p, float4 col, float dim)
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
public float4 draw_item_content(Element e, Item it, float2 p, float4 col)
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
