// The clock and its calendar: the time and the date `bar_compose` writes on
// the GPU from two numbers the embassy keeps, and the month the clock's
// click opens as a menu-kind popup. The bar's own subject (no sidecar feeds
// it; the embassy's wall clock does), with nothing kept between dispatches
// but the clock's item index inside one build (`Private.clock`).
//
// It names no other subject. Its item kinds and id tags are bound by
// `bar_dispatch` (`CLOCK_*`, below).

module clock;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_popups;
import bar_layout;
import bar_table;

/// The clock's item kind and a calendar cell's, and the id tags the two
/// carry: the bar's `ITEM_CLOCK`, `ITEM_CAL_CELL`, `ID_CLOCK` and
/// `ID_CALENDAR` today, bound by `bar_dispatch`.
extern static const uint CLOCK_ITEM;
extern static const uint CLOCK_CELL;
extern static const uint CLOCK_TAG;
extern static const uint CLOCK_CELL_TAG;

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
public uint calendar_column(uint weekday)
{
    uint start = CAL_WEEK_STARTS_MONDAY ? 1u : 0u;
    return (weekday + 7u - start) % 7u;
}

/// How many days this month has; 0 when the embassy has no local time.
public uint calendar_days()
{
    return min(inputs.clock_month & 0xffu, 31u);
}

/// The grid position of day `day` (1-based), counted row-major from the first
/// week's first column.
public uint calendar_cell(uint day)
{
    uint first = min((inputs.clock_month >> 8) & 0xffu, 6u);
    return calendar_column(first) + max(day, 1u) - 1u;
}

/// How many weeks this month spans: four to six.
public uint calendar_rows()
{
    uint days = calendar_days();
    return days == 0u ? 0u : calendar_cell(days) / 7u + 1u;
}

/// The calendar popup's size: the grid, or the title if that is wider, inside
/// the popup's pad; a title row, the weekday row and the weeks. Known from the
/// facts alone, so the click asks for the right size before a cell is built.
public float2 calendar_want(float ppad, float title_w)
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
public void witness_calendar()
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

/// The clock: the calendar, a menu whose level is the clock.
/// Clicking the clock while it is up closes it, as an applet's chip does.
/// The middle and the right button do nothing.
public void clock_click(uint ii, Item it, uint elem, uint button)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT || !CLOCK_SHOW_CALENDAR)
    {
        return;
    }
    Id own = it.id;
    if (popup_live() && private[0].popup_kind == POPUP_MENU
        && private[0].popup_id.eq(own))
    {
        close_popup();
        return;
    }
    // A dock preview still in its grace period when the click lands.
    end_preview_for_menu();
    set_menu_level(ii);
    request_popup(POPUP_MENU, private[0].elements[elem].rect, own,
                  calendar_want(POPUP_PAD * scale_px(), 0.0));
    witness_calendar();
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
public uint layout_calendar(uint next, bool open, float2 popup, float ppad)
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
        if (it.kind == CLOCK_CELL && it.parent == level && id_aux(it.id) == CAL_TITLE)
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
        if (it.kind != CLOCK_CELL || it.parent != level)
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

/// `HH:MM` from `hour * 60 + minute`.
public Composed compose_clock_time(uint minutes)
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
public Composed compose_clock_date(uint packed)
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

/// The clock: two reservations `bar_compose` fills, both always taken so that
/// CLOCK_SHOW_DATE stays a width-and-paint decision needing no rebuild.
/// Returns its index, which the calendar's cells hang off.
public uint build_clock(inout Build b, uint px)
{
    uint block = build_composed(b, px, COMPOSE_CLOCK_TIME);
    uint aux = build_composed(b, px, COMPOSE_CLOCK_DATE);
    Item it = item_none();
    it.kind = CLOCK_ITEM;
    it.block = block;
    it.aux_block = aux;
    it.id = id_tagged(CLOCK_TAG, 0u, 0u);
    return build_push(b, it);
}

/// One calendar cell under the clock.
public void build_cal_cell(inout Build b, uint parent, uint block, uint part, uint payload, uint flags)
{
    Item it = item_none();
    it.kind = CLOCK_CELL;
    it.parent = parent;
    it.block = block;
    it.id = id_tagged(CLOCK_CELL_TAG, part, payload);
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
public void build_calendar(inout Build b, uint parent, uint px)
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

// ---------------------------------------------------------------------------
// What the passes ask of the clock, through `bar_dispatch`
// ---------------------------------------------------------------------------

/// What a `layout:` line calls the clock.
public NameRef clock_name(Item it)
{
    return name_plain(NAME_CLOCK);
}

/// What a `layout:` line calls a cell of the calendar: its title by month
/// and year, a weekday initial by its weekday's literal, a day by its number.
public NameRef calendar_cell_name(Item it)
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

/// The time's bytes into the block reserved at byte `at`; how many landed.
public uint clock_compose_time(uint at)
{
    return compose_store(at, compose_clock_time(inputs.clock_time));
}

/// The date's bytes into the block reserved at byte `at`; how many landed.
public uint clock_compose_date(uint at)
{
    return compose_store(at, compose_clock_date(inputs.clock_date));
}

/// The clock's item (`build_clock`), its text at `px`. Its index is kept
/// for the calendar's cells, which this build may publish further on.
public void clock_build(inout Build b, uint px)
{
    private[0].clock.build_item = build_clock(b, px);
}

/// The calendar's cells, while the clock is the menu level, at `px`.
public void clock_build_calendar(inout Build b, uint px)
{
    uint parent = private[0].clock.build_item;
    if (CLOCK_SHOW_CALENDAR && parent != NO_INDEX)
    {
        build_calendar(b, parent, px);
    }
}

/// A reload that turned the calendar off while it was up ends it.
public void clock_step_menu()
{
    if (popup_live() && private[0].popup_kind == POPUP_MENU && !CLOCK_SHOW_CALENDAR
        && id_tag(menu_level_id()) == CLOCK_TAG)
    {
        close_popup();
    }
}
