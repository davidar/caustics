// The charts, the subject `caustics-sysmon` feeds (sysmon/README.md): the
// strip of charts on the bar, each a row of the samples body drawn per
// texel, and the chart numbers, the popup of one line per chart that the
// strip's hover and click open. This module is the one reader of `samples`, and its
// between-dispatch state is `Private.sysmon` (`SysmonScratch`).
//
// It names no other subject. Its item kinds and id tags are not named here
// but bound by `bar_dispatch` (`SYSMON_*`, below), which is what calls it.

module sysmon;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_popups;
import bar_layout;
import bar_paint;
import bar_table;

/// The item kind of a chart on the strip and of a line of the numbers, and
/// the id tags the two carry: the bar's `ITEM_CHART`, `ITEM_CHART_LINE`,
/// `ID_CHART` and `ID_NUMBERS` today, bound by `bar_dispatch` (link-time
/// constants, folded into the program like any other), so this module never
/// names a kind of the bar's.
extern static const uint SYSMON_CHART;
extern static const uint SYSMON_LINE;
extern static const uint SYSMON_CHART_TAG;
extern static const uint SYSMON_LINE_TAG;

/// The chart kind's literal for a samples row, and whether that kind has
/// appeared earlier in the frame — which is what puts the row index on the
/// second and later charts of a kind and leaves the first bare.
public uint chart_name(uint row, out bool repeat)
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
public bool numbers_level()
{
    return id_tag(menu_level_id()) == SYSMON_CHART_TAG;
}

/// Is a numbers popup asked for or up?
public bool numbers_live()
{
    return popup_live() && numbers_level()
        && (private[0].popup_kind == POPUP_PREVIEW || private[0].popup_kind == POPUP_MENU);
}

/// The strip: the union of every chart laid out on the bar this frame, which
/// is the popup's anchor and what "on the strip" means — the gaps between two
/// charts included, so crossing one does not end the popup. False when no
/// chart is laid out.
public bool chart_strip(out float4 strip)
{
    strip = float4(0.0, 0.0, 0.0, 0.0);
    float2 lo = float2(0.0, 0.0);
    float2 hi = float2(0.0, 0.0);
    bool any = false;
    uint n = live_elements();
    for (uint i = ELEM_FIRST_FREE; i < n; ++i)
    {
        Element e = private[0].elements[i];
        if (!element_live(e, TARGET_BAR) || item_of(e.item).kind != SYSMON_CHART)
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
public uint chart_under_pointer()
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
    return item_of(item).kind == SYSMON_CHART ? item : NO_INDEX;
}

/// How many lines the numbers hold: one per chart the samples body carries,
/// laid out on the strip or not.
public uint numbers_count()
{
    return min(samples[0].chart_count, MAX_CHARTS);
}

/// The numbers popup's size: the width the last one measured (the lines are
/// built a frame after the open, and shaped after that), and the lines' height
/// from the samples body alone.
public float2 numbers_want()
{
    float s = scale_px();
    float ppad = POPUP_PAD * s;
    float w = max(private[0].sysmon.numbers_w, MENU_MIN_W * s);
    float h = 2.0 * ppad + float(max(numbers_count(), 1u)) * CHART_NUMBERS_LINE_PX * s;
    return float2(w, h);
}

/// Ask for the numbers as a popup of `kind` about chart item `chart`, anchored
/// at the strip, and say so.
public void open_numbers(uint kind, uint chart, float4 strip)
{
    set_menu_level(chart);
    private[0].sysmon.numbers_grow = 0u;
    private[0].sysmon.numbers_dwell = 0.0;
    request_popup(kind, strip, item_of(chart).id, numbers_want());
    bool repeat;
    Args a = witness_begin(WITNESS_INFO,
                           getStringHash("numbers: open kind={0:s} chart={1:s} lines={2}"));
    arg(a, kind == POPUP_MENU ? getStringHash("menu") : getStringHash("preview"));
    arg(a, chart_name(item_chart_row(item_of(chart)), repeat));
    arg(a, numbers_count());
}

/// End the numbers popup for `why`, held until the pointer leaves the strip
/// so the hover does not ask for it straight back.
public void close_numbers(uint why)
{
    close_popup();
    private[0].sysmon.numbers_hold = 1u;
    private[0].sysmon.numbers_dwell = 0.0;
    witness1(WITNESS_INFO, getStringHash("numbers: closed ({0:s})"), why);
}

/// The strip's hover, once a frame after the layout (so the strip is this
/// frame's): the dwell, the open, the mark following the pointer across the
/// charts, and the end when the pointer leaves the strip. A menu-kind numbers
/// popup is the click's and stays; only a second click or the compositor ends
/// it. Returns true while the dwell clock is running, so the frame loop comes
/// back for it.
public bool step_numbers(float dt)
{
    float4 strip;
    bool any = chart_strip(strip);
    float2 p = private[0].pointer;
    bool on_strip = any && private[0].pointer_target == TARGET_BAR
                    && p.x >= strip.x && p.x < strip.x + strip.z
                    && p.y >= strip.y && p.y < strip.y + strip.w;
    if (!on_strip)
    {
        private[0].sysmon.numbers_hold = 0u;
        private[0].sysmon.numbers_dwell = 0.0;
    }
    uint chart = on_strip ? chart_under_pointer() : NO_INDEX;

    if (numbers_live() && private[0].popup_kind == POPUP_PREVIEW)
    {
        if (!on_strip)
        {
            close_numbers(getStringHash("left"));
            // Leaving is not a dismissal: the next rest on the strip opens it.
            private[0].sysmon.numbers_hold = 0u;
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
    if (!idle || !CHART_NUMBERS_ON_HOVER || private[0].sysmon.numbers_hold != 0u || !on_strip
        || chart == NO_INDEX)
    {
        private[0].sysmon.numbers_dwell = 0.0;
        return false;
    }
    private[0].sysmon.numbers_dwell += dt;
    if (private[0].sysmon.numbers_dwell < HOVER_DWELL_S)
    {
        return true;
    }
    open_numbers(POPUP_PREVIEW, chart, strip);
    return false;
}

/// The widest name and the widest value among the chart numbers' lines, px,
/// and how many lines there are.
public uint numbers_columns(out float name_w, out float value_w)
{
    name_w = 0.0;
    value_w = 0.0;
    uint lines = 0u;
    uint n = live_items();
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != SYSMON_LINE)
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
public uint layout_numbers(uint next, bool open, float2 popup, float ppad)
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
        private[0].sysmon.numbers_w = private[0].sysmon.numbers_grow == 0u
            ? measured
            : max(private[0].sysmon.numbers_w, measured);
        private[0].sysmon.numbers_grow = 1u;
        want = float2(private[0].sysmon.numbers_w,
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
        if (it.kind != SYSMON_LINE)
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

// ---------------------------------------------------------------------------
// The chart numbers' values, composed beside the clock's:
// a sample in the samples body's raw units becomes digits, at most one
// decimal, and the unit's literal from the pool. The rules, and the words,
// are the tunables' (`CHART_NAMES` and below); the arithmetic is here.
// ---------------------------------------------------------------------------

/// One number as a line shows it: the value already rounded to what will be
/// printed, how many decimals it is printed with (0 or 1), the unit's literal
/// (0 for none), or a gap.
public struct ChartReading
{
    public float value;
    public uint decimals;
    public uint unit;
    public bool gap;
};

/// The newest sample of series `k` of chart `c`: the last column of the
/// valid tail, or -1 (a gap) when there is none.
public float chart_latest(uint c, uint k)
{
    uint sc = live_samples();
    return sc == 0u ? -1.0 : chart_sample(c, k, sc - 1u);
}

/// `v` rounded to `decimals` (0 or 1) and capped where the digits would
/// outgrow `compose_number`'s ten.
public float chart_round(float v, uint decimals)
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
public ChartReading chart_reading(uint kind, float ceiling, float v)
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
public void compose_reading(inout Composed c, ChartReading r)
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
public Composed compose_chart_value(uint c)
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

/// One chart per row of the samples body: the row's index, and that is the
/// whole item. Everything else a chart draws is read there. Returns the first
/// chart's item index, or NO_INDEX — the charts are consecutive, so row `r`
/// is that plus `r`.
public uint build_charts(inout Build b)
{
    uint n = min(samples[0].chart_count, MAX_CHARTS);
    uint first = NO_INDEX;
    for (uint i = 0u; i < n; ++i)
    {
        Item it = item_none();
        it.kind = SYSMON_CHART;
        it.id = id_tagged(SYSMON_CHART_TAG, 0u, i);
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
public void build_numbers(inout Build b, uint parent, uint px)
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
        it.kind = SYSMON_LINE;
        it.parent = parent;
        it.block = name;
        it.aux_block = value;
        it.id = id_tagged(SYSMON_LINE_TAG, 0u, c);
        it.a = c;
        build_push(b, it);
    }
}

/// The chart strip's shape: the kinds of the samples body's rows, in order.
///
/// A comparison the caustic makes, not the embassy. A tick that carries new
/// numbers for the same charts is an upload of the body and a redraw; a
/// different chart set is a rebuild, one frame later.
public uint chart_shape_of()
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

// ---------------------------------------------------------------------------
// Charts. A row of the samples frame, drawn per texel: no CPU arithmetic, no
// history in the item table, nothing about a chart in the ABI beyond its index
// and what it measures. The whole strip is display only.
// ---------------------------------------------------------------------------

/// Valid columns in the frame, bounded by the buffer's own capacity.
public uint live_samples()
{
    return min(samples[0].sample_count, CHART_SAMPLES);
}

/// Chart `c`'s row, or an empty one when the frame does not have that many.
public ChartRow chart_row(uint c)
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
public float chart_sample(uint c, uint k, uint j)
{
    uint sc = live_samples();
    if (c >= MAX_CHARTS || k >= 2u || j >= sc)
    {
        return -1.0;
    }
    return samples[0].samples[(2u * c + k) * CHART_SAMPLES + (CHART_SAMPLES - sc + j)];
}

/// The whole-pixel step between two samples on screen, from the tunable.
public float chart_step()
{
    return max(floor(CHART_PX_PER_SAMPLE), 1.0);
}

/// How many of the valid tail's columns fit across `inner_w` device px at
/// `chart_step()` per sample: the *visible window*. The newest column sits at
/// the right edge and the window is what both the ceiling and the drawing
/// see, so an old spike that has scrolled off no longer sets the scale.
public uint visible_samples(float inner_w)
{
    uint fit = uint(floor(inner_w / chart_step())) + 1u;
    return min(live_samples(), fit);
}

/// What to divide a sample by: the row's ceiling, or — when it is 0 — the
/// maximum over the valid window, floored by RATE_FLOOR. Both series of a dual
/// chart share the result, so down and up stay comparable. A loop over at most
/// 256 floats per chart per texel is nothing at 90 × 56 px, and it is what
/// keeps the CPU out of presentation entirely.
public float chart_ceiling(uint c, ChartRow row, uint visible)
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
public float3 chart_rgb(uint kind, uint series)
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
public float3 heat_rgb(float h)
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
public float4 draw_series(float2 p, float2 org, float2 size, uint visible,
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
public float4 draw_chart(Element e, Item it, float2 p, float4 col)
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
public bool numbers_marked(Item it)
{
    Id level = menu_level_id();
    return id_tag(level) == SYSMON_CHART_TAG && id_payload(level) == item_chart_row(it);
}

/// A line of the chart numbers' dressing: the swatch, a
/// MENU_MARK square centred in the left gutter in the chart's line colour —
/// the colour `draw_series` paints that chart with, split down the middle for
/// a two-series chart, a vertical `heat_rgb` ramp for TEMP — filled for the chart being asked about and outlined
/// for the rest; and on that line, the hover chip's wash, so the eye lands on
/// it.
public float4 draw_numbers_line(Element e, Item it, float2 p, float4 col)
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

// ---------------------------------------------------------------------------
// What the passes ask of the charts, through `bar_dispatch`
// ---------------------------------------------------------------------------

/// What a `layout:` line calls a chart: `chart-{kind}`, and `chart-{kind}-{n}`
/// for the second and later chart of a kind.
public NameRef sysmon_chart_name(Item it)
{
    bool repeat;
    uint hash = chart_name(id_payload(it.id), repeat);
    return repeat ? name_of(NAME_CHART_N, hash, id_payload(it.id), 0u, 2u)
                  : name_of(NAME_CHART, hash, 0u, 0u, 1u);
}

/// What a `layout:` line calls a line of the numbers: the chart's word and
/// its first series' number as the line composes it.
public NameRef sysmon_line_name(Item it)
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

/// A click on chart item `ii` with `button`.
///
/// The chart numbers as a menu: it takes the grab, so
/// a click elsewhere ends it, and a second click on the strip closes it as
/// the clock's does. The hover's preview, if up, is replaced by the menu —
/// the embassy's replace path — and the click is the only thing a chart
/// answers: no hover chip, no press dip (`item_is_button`), and nothing for
/// the middle or the right button.
public void sysmon_click(uint ii, uint button)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT)
    {
        return;
    }
    if (numbers_live() && private[0].popup_kind == POPUP_MENU)
    {
        close_numbers(getStringHash("click"));
        return;
    }
    float4 strip;
    if (!chart_strip(strip))
    {
        return;
    }
    end_preview_for_menu();
    open_numbers(POPUP_MENU, ii, strip);
}

/// The popup ended without this module asking (`CMD_POPUP_DONE`).
///
/// The chart numbers the compositor ended — a click elsewhere under the
/// menu's grab, or a refusal — are held while the pointer stays on the
/// strip, or a refused preview would be asked for again every dwell.
public void sysmon_popup_done()
{
    if (numbers_level() && private[0].popup_state != POPUP_ENDING)
    {
        private[0].sysmon.numbers_hold = 1u;
        witness1(WITNESS_INFO, getStringHash("numbers: closed ({0:s})"),
                 getStringHash("dismissed"));
    }
}

/// Whether the chart set is not the one the table was built from, which is
/// a rebuild: the samples body's row kinds, compared against the last.
public bool sysmon_reshaped()
{
    uint charts = chart_shape_of();
    if (charts != private[0].sysmon.chart_shape)
    {
        private[0].sysmon.chart_shape = charts;
        return true;
    }
    return false;
}

/// Whether a samples tick has arrived under an open numbers popup.
///
/// The chart numbers are the one level whose rows move with no fact
/// moving: a samples tick is an upload and a redraw, and the lines composed
/// from it are a build's. So while they are up, a tick this file has not
/// built for is a rebuild too.
public bool sysmon_ticked()
{
    if (numbers_live() && asuint(samples[0].t) != private[0].sysmon.numbers_t)
    {
        private[0].sysmon.numbers_t = asuint(samples[0].t);
        return true;
    }
    return false;
}

/// The strip's items, one per row of the samples body (`build_charts`).
/// The first one's index is kept for the numbers' lines, which this build
/// may publish further on.
public void sysmon_build_charts(inout Build b)
{
    private[0].sysmon.build_first = build_charts(b);
}

/// The numbers' lines, while chart `level` is the menu level, at `px`.
public void sysmon_build_numbers(inout Build b, Id level, uint px)
{
    uint first = private[0].sysmon.build_first;
    if (first != NO_INDEX && id_payload(level) < min(samples[0].chart_count, MAX_CHARTS))
    {
        build_numbers(b, first + id_payload(level), px);
    }
}

/// `charts: N laid out`, once per change, over the build's first `items`.
public void sysmon_said_build(uint items)
{
    uint charts = 0u;
    for (uint i = 0u; i < items; ++i)
    {
        if (private[0].items[i].kind == SYSMON_CHART)
        {
            charts += 1u;
        }
    }
    if (charts != private[0].sysmon.said_charts)
    {
        private[0].sysmon.said_charts = charts;
        witness1(WITNESS_INFO, getStringHash("charts: {0} laid out"), charts);
    }
}

/// A line of the chart numbers' text: the name where a menu row's label
/// starts, the value right-aligned against the right gutter.
public float4 draw_numbers_content(Element e, Item it, float2 p, float4 col)
{
    float s = scale_px();
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
