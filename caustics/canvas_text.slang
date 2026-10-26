// The canvas's text: its few blocks, shaped by the text engine on the device
// and painted one left-aligned line at a time (docs/canvas.md, *Why a
// caustic*). Nothing is rasterised on the CPU and no glyph is a bitmap: a
// block is a run `shapeRun` wrote into `runs`, and a pixel's ink is
// `textCoverage` of the glyph under it, as the bar's.
//
// **The blocks are fixed.** A prompt shows a title, the ask's words, what is
// typed (dots for an echo-off ask, the text for an echo-on one) and one line
// for whatever the conversation said last, so there are five blocks, each
// with its slots in `runs`, and `canvas_shape` shapes them all with one
// thread each. Which bytes a block is comes from `facts[0].prompt` (the
// sidecar's text, in `prompt_text`) or the literal pool (the dot, and the
// word an ask with no text of its own is given). The secret is never one of
// them: an echo-off ask gives the caustic a count, and `BLOCK_DOT` is one
// glyph drawn that many times.
//
// **Shaped once, drawn at any size.** A run is shaped at `SHAPE_PX` and its
// pen positions scaled to the size a surface draws at, so one shape pass
// serves outputs of different scales; at 64 px an em the shaper's 26.6
// rounding is a sixty-fourth of a pixel there, and less below.
//
// Everything a sidecar sent is bounded: a block's range is clamped to the
// buffer bound, its glyphs to its own slots, and its width to the clip edge
// the caller passes.

module canvas_text;

import canvas_abi;
import canvas_tunables;
import canvas_state;

/// The five blocks.
public static const uint BLOCK_TITLE = 0u;
public static const uint BLOCK_ASK = 1u;
public static const uint BLOCK_ECHO = 2u;
public static const uint BLOCK_LINE = 3u;
public static const uint BLOCK_DOT = 4u;

/// The em size every block is shaped at.
public static const float SHAPE_PX = 64.0;

/// What an echo-off ask is drawn with, one per character typed.
public static const uint DOT = getStringHash("•");
/// The ask's words when its sidecar sent none.
public static const uint PASSWORD = getStringHash("Password");

/// Each block's first slot in `runs`, and how many it may fill: a title or
/// a line of 160 glyphs is three times what a panel shows, and every slot
/// together is `CANVAS_GLYPHS`.
uint block_first(uint b)
{
    switch (b)
    {
    case 0u: return 0u;
    case 1u: return 160u;
    case 2u: return 256u;
    case 3u: return 344u;
    default: return 504u;
    }
}

uint block_slots(uint b)
{
    switch (b)
    {
    case 0u: return 160u;
    case 1u: return 96u;
    case 2u: return 88u;
    case 3u: return 160u;
    default: return 8u;
    }
}

/// Which bytes block `b` is now: `(source, off, len)`.
uint3 block_bytes(uint b)
{
    CanvasPrompt pr = facts[0].prompt;
    switch (b)
    {
    case 0u: return uint3(TEXT_PROMPT, pr.title_off, pr.title_len);
    case 1u:
    {
        if (pr.ask_len > 0u)
        {
            return uint3(TEXT_PROMPT, pr.ask_off, pr.ask_len);
        }
        Literal word = literal(PASSWORD);
        return uint3(TEXT_LITERALS, word.off, word.len);
    }
    case 2u: return uint3(TEXT_PROMPT, pr.echo_off, pr.echo_len);
    case 3u: return uint3(TEXT_PROMPT, pr.line_off, pr.line_len);
    default:
    {
        Literal dot = literal(DOT);
        return uint3(TEXT_LITERALS, dot.off, dot.len);
    }
    }
}

/// Shape block `b`: `canvas_shape`'s one thread. Its run goes into its own
/// slots of `runs`, and what came of it into `private[0].text[b]`.
public void shape_block(uint b)
{
    if (b >= CANVAS_TEXT_BLOCKS)
    {
        return;
    }
    uint3 bytes = block_bytes(b);
    uint words;
    uint stride;
    if (bytes.x == TEXT_LITERALS)
    {
        shapeUtf8.GetDimensions(words, stride);
    }
    else
    {
        prompt_text.GetDimensions(words, stride);
    }
    uint limit = words * 4u;
    uint start = min(bytes.y, limit);
    uint len = min(bytes.z, limit - start);

    uint capacity;
    runs.GetDimensions(capacity, stride);
    uint first = block_first(b);
    uint slots = min(block_slots(b), capacity > first ? capacity - first : 0u);

    CanvasRun out;
    out.source = bytes.x;
    out.off = start;
    out.len = len;
    out.first = first;
    out.glyphs = 0u;
    out.flags = 0u;
    out.advance = 0.0;
    out._pad0 = 0u;
    if (len > 0u && slots > 0u)
    {
        ShapeRequest req;
        req.byte_start = start;
        req.byte_len = len;
        req.px_size = SHAPE_PX;
        req.out_start = first;
        req.out_cap = slots;
        req.feature_start = 0u;
        req.feature_count = 0u;
        req._rsvd0 = 0u;
        // One call site: Slang inlines the whole shaper at every call, so the
        // buffer is a selector rather than a branch around two calls.
        let run = shapeRun(shape_tables(bytes.x == TEXT_LITERALS ? 0u : 1u), shapeParams, req,
                           runs, min(capacity, CANVAS_GLYPHS));
        out.glyphs = min(run.glyph_count, slots);
        out.flags = run.flags;
        // Clamped: a corrupt table's metric must make an ugly line, not an
        // absurd one.
        out.advance = clamp(float(run.advance) / 64.0, 0.0, 65536.0);
    }
    private[0].text[b] = out;
}

/// The baseline's distance below a line's top at em size `px`, and the
/// line's height: the font's `hhea` ascender and descender, as the shaper
/// places a pen. Before any font, a line still has a height, so a panel
/// keeps its shape while the font is on its way.
public float text_baseline(float px)
{
    uint upem = shapeParams.font.upem;
    if (upem == 0u)
    {
        return px * 0.95;
    }
    return px * float(shapeParams.font.ascender) / float(upem);
}

public float text_line_height(float px)
{
    uint upem = shapeParams.font.upem;
    if (upem == 0u)
    {
        return px * 1.2;
    }
    return px * float(shapeParams.font.ascender - shapeParams.font.descender) / float(upem);
}

/// Block `b`'s width at em size `px`.
public float text_width(uint b, float px)
{
    if (b >= CANVAS_TEXT_BLOCKS)
    {
        return 0.0;
    }
    return private[0].text[b].advance * px / SHAPE_PX;
}

/// `colour` laid over `col` by `coverage`: the panel is opaque, so this is a
/// plain mix.
public float4 ink(float3 colour, float coverage, float4 col)
{
    return float4(lerp(col.rgb, colour, saturate(coverage)), col.a);
}

/// Block `b` painted at pixel `p`: one line, its top-left at `org`, at em
/// size `px`, in `colour`, clipped at `clip` on the right. Glyphs past the
/// clip are not drawn; a line too long for its panel ends at the edge.
public float4 paint_text(uint b, float2 org, float px, float clip, float3 colour, float2 p,
                         float4 col)
{
    return paint_glyphs(b, 0u, block_slots(b), 0.0, org, px, clip, colour, p, col);
}

/// Glyphs `[g0, g1)` of block `b` as one line from `org`, each moved left by
/// `shift` px: how the second line of a wrapped run is drawn.
public float4 paint_glyphs(uint b, uint g0, uint g1, float shift, float2 org, float px,
                           float clip, float3 colour, float2 p, float4 col)
{
    if (b >= CANVAS_TEXT_BLOCKS || px <= 0.0 || p.x >= clip)
    {
        return col;
    }
    float lh = text_line_height(px);
    if (p.y < org.y - 1.0 || p.y >= org.y + lh + 1.0)
    {
        return col;
    }
    CanvasRun run = private[0].text[b];
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint limit = min(capacity, CANVAS_GLYPHS);
    uint n = min(min(run.glyphs, block_slots(b)), g1);
    float k = px / SHAPE_PX;
    float baseline = org.y + text_baseline(px);
    uint glyph_limit = textGlyphCount();
    float cov = 0.0;
    for (uint i = g0; i < n; ++i)
    {
        uint at = run.first + i;
        if (at >= limit)
        {
            break;
        }
        ShapedGlyph g = runs[at];
        if (g.gid >= glyph_limit)
        {
            continue;
        }
        float2 origin = float2(org.x + float(g.x) / 64.0 * k - shift,
                               baseline - float(g.y) / 64.0 * k);
        float4 rect = textGlyphRect(g.gid, origin, px);
        if (rect.x >= clip)
        {
            break;
        }
        if (p.x < rect.x || p.x >= rect.z || p.y < rect.y || p.y >= rect.w)
        {
            continue;
        }
        cov = max(cov, textCoverage(g.gid, textSampleAt(p, origin, px), px));
    }
    return ink(colour, cov, col);
}

/// Glyph `i` of block `b`'s pen position at em size `px`, px from the run's
/// start.
public float glyph_x(uint b, uint i, float px)
{
    CanvasRun run = private[0].text[b];
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint at = run.first + i;
    if (i >= run.glyphs || at >= min(capacity, CANVAS_GLYPHS))
    {
        return text_width(b, px);
    }
    return float(runs[at].x) / 64.0 * px / SHAPE_PX;
}

/// The byte at `off` of a block's source buffer, or 0 past its end.
uint byte_at(uint source, uint off)
{
    uint words;
    uint stride;
    uint word = 0u;
    if (source == TEXT_LITERALS)
    {
        shapeUtf8.GetDimensions(words, stride);
        if (off / 4u < words)
        {
            word = shapeUtf8[off / 4u];
        }
    }
    else
    {
        prompt_text.GetDimensions(words, stride);
        if (off / 4u < words)
        {
            word = prompt_text[off / 4u];
        }
    }
    return (word >> (8u * (off % 4u))) & 0xffu;
}

/// Where the line of block `b` that starts at glyph `g0` ends, to fit
/// `width` px at em size `px`: the index of the glyph the next line starts
/// at, or `NO_INDEX` when the rest of the run fits on this line. The break
/// is after the last space, slash or hyphen whose line before it fits (a
/// path breaks after a slash); a run with none to break at breaks at the
/// last glyph that fits, so a long word is cut across lines rather than
/// off at the edge. Always past `g0`, so a caller walking the lines
/// terminates.
public uint wrap_from(uint b, uint g0, float px, float width)
{
    if (b >= CANVAS_TEXT_BLOCKS)
    {
        return NO_INDEX;
    }
    CanvasRun run = private[0].text[b];
    uint n = min(run.glyphs, block_slots(b));
    if (g0 + 1u >= n || text_width(b, px) - glyph_x(b, g0, px) <= width)
    {
        return NO_INDEX;
    }
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    float x0 = glyph_x(b, g0, px);
    uint space = NO_INDEX;
    uint fits = g0 + 1u;
    for (uint i = g0 + 1u; i < n; ++i)
    {
        uint at = run.first + i;
        if (at >= min(capacity, CANVAS_GLYPHS))
        {
            break;
        }
        ShapedGlyph g = runs[at];
        if (float(g.x) / 64.0 * px / SHAPE_PX - x0 > width)
        {
            break;
        }
        fits = i;
        uint cluster = g.cluster;
        if (cluster >= run.off && cluster < run.off + run.len)
        {
            uint byte = byte_at(run.source, cluster);
            if (byte == 0x20u || byte == 0x2fu || byte == 0x2du)
            {
                space = i;
            }
        }
    }
    // The next line starts after the space (or slash, or hyphen), or at the
    // first glyph that did not fit; `fits` is the last that did, so it is at
    // least g0 + 1.
    return space != NO_INDEX ? space + 1u : max(fits, g0 + 1u);
}

/// `count` dots in a row from `org`, each the dot block's glyph at em size
/// `px`, `gap` apart beyond its own advance, as many as fit before `clip`.
/// One glyph's coverage a pixel, whatever the count.
public float4 paint_dots(uint count, float2 org, float px, float gap, float clip, float3 colour,
                         float2 p, float4 col)
{
    CanvasRun run = private[0].text[BLOCK_DOT];
    if (count == 0u || run.glyphs == 0u || px <= 0.0 || p.x < org.x || p.x >= clip)
    {
        return col;
    }
    float lh = text_line_height(px);
    if (p.y < org.y - 1.0 || p.y >= org.y + lh + 1.0)
    {
        return col;
    }
    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    if (run.first >= min(capacity, CANVAS_GLYPHS))
    {
        return col;
    }
    ShapedGlyph g = runs[run.first];
    if (g.gid >= textGlyphCount())
    {
        return col;
    }
    float step = max(text_width(BLOCK_DOT, px) + gap, 1.0);
    uint fit = uint(max((clip - org.x) / step, 0.0));
    uint shown = min(count, fit);
    uint at = uint((p.x - org.x) / step);
    if (at >= shown)
    {
        return col;
    }
    float2 origin = float2(org.x + float(at) * step, org.y + text_baseline(px));
    float4 rect = textGlyphRect(g.gid, origin, px);
    if (p.x < rect.x || p.x >= rect.z || p.y < rect.y || p.y >= rect.w)
    {
        return col;
    }
    return ink(colour, textCoverage(g.gid, textSampleAt(p, origin, px), px), col);
}

/// How many blocks are flagged `SHAPE_RUN_NOTDEF` among those with bytes,
/// how many have bytes, and the flagged ones' ranges into `state`: the runs
/// the host is to ask its glyphs sidecar to cover. While **every** block with
/// bytes is flagged, the font has not landed (the canvas starts with no
/// curves and no `cmap`), and nothing is asked: one drawable run is what
/// makes the flagged ones mean what they say.
public uint2 text_covers()
{
    uint flagged = 0u;
    uint texts = 0u;
    for (uint b = 0u; b < CANVAS_TEXT_BLOCKS; ++b)
    {
        CanvasRun run = private[0].text[b];
        if (run.len == 0u)
        {
            continue;
        }
        texts += 1u;
        if ((run.flags & SHAPE_RUN_NOTDEF) != 0u)
        {
            flagged += 1u;
        }
    }
    uint n = 0u;
    if (flagged > 0u && flagged < texts)
    {
        for (uint b = 0u; b < CANVAS_TEXT_BLOCKS; ++b)
        {
            CanvasRun run = private[0].text[b];
            if (run.len == 0u || (run.flags & SHAPE_RUN_NOTDEF) == 0u)
            {
                continue;
            }
            state[0].covers[n].source = run.source;
            state[0].covers[n].off = run.off;
            state[0].covers[n].len = run.len;
            state[0].covers[n]._pad0 = 0u;
            n += 1u;
        }
    }
    state[0].cover_count = n;
    return uint2(flagged, texts);
}
