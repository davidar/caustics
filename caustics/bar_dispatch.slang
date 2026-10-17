// The one place that knows which subjects exist.
//
// The bar's passes (`bar.slang`'s entry points and the bar's own modules)
// name no subject: wherever a pass used to switch on an item's kind, an id's
// tag, a popup's kind or a composition, it calls one function here, and the
// function is that switch, calling the subject's own. Everything else a
// subject does it does in its module, out of the bar's modules below it.
//
// Hand-written for now and kept regular, so that a later step can generate
// it from the set of subjects present: one block per subject, and no logic
// but the switch. The one exception says so where it is.
//
// The subjects' kinds and tags are bound here too. A subject module names
// none of the bar's `ITEM_*` and `ID_*` constants: it declares its own as
// link-time constants (`extern static const uint SYSMON_CHART;`), and the
// `export`s below give them their values, which Slang folds into the
// program as it folds any constant.

module bar_dispatch;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_popups;
import bar_layout;
import bar_paint;
import bar_table;
import sysmon;
import notify;
import clock;
import wake;
import dock;
import applets;

// ---------------------------------------------------------------------------
// The subjects' kinds and tags
// ---------------------------------------------------------------------------

// sysmon: a chart on the strip, a line of the chart numbers.
export static const uint SYSMON_CHART = ITEM_CHART;
export static const uint SYSMON_LINE = ITEM_CHART_LINE;
export static const uint SYSMON_CHART_TAG = ID_CHART;
export static const uint SYSMON_LINE_TAG = ID_NUMBERS;

// notify: the tag a sidecar's row carries, which a toast's rows are.
export static const uint NOTIFY_ROW_TAG = ID_APPLET;

// clock: the clock, a calendar cell.
export static const uint CLOCK_ITEM = ITEM_CLOCK;
export static const uint CLOCK_CELL = ITEM_CAL_CELL;
export static const uint CLOCK_TAG = ID_CLOCK;
export static const uint CLOCK_CELL_TAG = ID_CALENDAR;

// dock: an entry, a window, the overview button, a row of an entry's menu.
export static const uint DOCK_ENTRY = ITEM_DOCK;
export static const uint DOCK_WINDOW = ITEM_WINDOW;
export static const uint DOCK_OVERVIEW = ITEM_OVERVIEW;
export static const uint DOCK_ROW = ITEM_MENU_ROW;
export static const uint DOCK_ENTRY_TAG = ID_ENTRY;
export static const uint DOCK_WINDOW_TAG = ID_WINDOW;
export static const uint DOCK_OVERVIEW_TAG = ID_OVERVIEW;
export static const uint DOCK_ROW_TAG = ID_DOCK_ROW;

// applets: a chip, a slider, a menu row; a sidecar's row, the caffeine chip.
export static const uint APPLETS_CHIP = ITEM_APPLET;
export static const uint APPLETS_SLIDER = ITEM_SLIDER;
export static const uint APPLETS_ROW = ITEM_MENU_ROW;
export static const uint APPLETS_TAG = ID_APPLET;
export static const uint APPLETS_CAFFEINE_TAG = ID_CAFFEINE;

// ---------------------------------------------------------------------------
// bar_build
// ---------------------------------------------------------------------------

/// The visible items, in the bar's order: the dock's (the overview button
/// and the entries), the clock, the applets' chips, the charts last, whose
/// count follows the sidecar's list, so that a sidecar restarted with a
/// different one cannot move the power button or a tray item out from under
/// a click.
public void dispatch_build(inout Build b, BuildSizes z)
{
    dock_build(b, z.icon, z.text[ITEM_DOCK]);
    clock_build(b, z.text[ITEM_CLOCK]);
    applets_build(b, z.icon, z.text[ITEM_APPLET]);
    sysmon_build_charts(b);
}

/// The rows of menu level `level`, by its tag. True when a row has a body.
public bool dispatch_build_level(inout Build b, BuildSizes z, Id level)
{
    switch (id_tag(level))
    {
    case ID_APPLET:
        return applets_build_menu(b, level, z.icon, z.text[ITEM_MENU_ROW], z.text[ITEM_SLIDER]);
    case ID_ENTRY:
        dock_build_menu(b, level, z.text[ITEM_MENU_ROW]);
        return false;
    case ID_CLOCK:
        clock_build_calendar(b, z.text[ITEM_CAL_CELL]);
        return false;
    case ID_CHART:
        sysmon_build_numbers(b, level, z.text[ITEM_CHART_LINE]);
        return false;
    default:
        return false;
    }
}

/// The items past the menu's rows: the dock's windows.
public void dispatch_build_last(inout Build b, BuildSizes z)
{
    dock_build_windows(b, z.text[ITEM_WINDOW]);
}

/// Each subject's witness of the build's first `items`, once per change.
public void dispatch_said_build(uint items)
{
    dock_said_build(items);
    applets_said_build(items);
    sysmon_said_build(items);
}

// ---------------------------------------------------------------------------
// bar_compose
// ---------------------------------------------------------------------------

/// Composition `compose` into the block reserved at byte `at`; how many
/// bytes landed.
public uint dispatch_compose(uint compose, uint at)
{
    switch (compose)
    {
    case COMPOSE_CLOCK_TIME:
        return clock_compose_time(at);
    case COMPOSE_CLOCK_DATE:
        return clock_compose_date(at);
    default:
        return 0u;
    }
}

// ---------------------------------------------------------------------------
// bar_apply: the commands
// ---------------------------------------------------------------------------

/// A click with `button` at `pos` on item `ii` (`it`), laid out as `elem`,
/// by its kind; a sidecar's chip and a menu row by their tags, and a menu
/// row of the toast that is up by the popup's kind.
public void dispatch_click(uint kind, uint ii, Item it, uint elem, uint button, float2 pos)
{
    switch (kind)
    {
    case ITEM_DOCK:
        dock_click_entry(ii, it, elem, button, pos);
        break;
    case ITEM_OVERVIEW:
        dock_click_overview(button);
        break;
    case ITEM_CLOCK:
        clock_click(ii, it, elem, button);
        break;
    case ITEM_CHART:
        sysmon_click(ii, button);
        break;
    case ITEM_APPLET:
        switch (id_tag(it.id))
        {
        case ID_CAFFEINE:
            applets_click_caffeine(button);
            break;
        default:
            applets_click_chip(ii, it, elem, button, pos);
            break;
        }
        break;
    case ITEM_MENU_ROW:
        switch (popup_live_kind())
        {
        case POPUP_TOAST:
            notify_click_row(it, button, pos);
            break;
        default:
            switch (id_tag(it.id))
            {
            case ID_DOCK_ROW:
                dock_click_row(it, button);
                break;
            default:
                applets_click_row(ii, it, button, pos);
                break;
            }
            break;
        }
        break;
    case ITEM_SLIDER:
        applets_click_slider(it, elem, button, pos);
        break;
    default:
        break;
    }
}

/// The pointer moved, or went down, with a button held on `target`: a
/// slider being dragged follows it.
public void dispatch_drag(uint target, float2 pos)
{
    drag_slider(target, pos);
}

/// A button went down on the bar at `pos`: the wake drops a source there.
public void dispatch_bar_press(float2 pos)
{
    wake_click(pos);
}

/// The popup ended without the caustic asking (`CMD_POPUP_DONE`): each
/// subject whose popup it was holds it.
public void dispatch_popup_done()
{
    notify_popup_done();
    sysmon_popup_done();
}

// ---------------------------------------------------------------------------
// bar_apply: the frame
// ---------------------------------------------------------------------------

/// Whether a subject's own frame changed shape this frame, which is a
/// rebuild: the chart set.
public bool dispatch_reshaped()
{
    return sysmon_reshaped();
}

/// What a subject ends of a menu level, before the layout: a calendar a
/// reload turned off.
public void dispatch_step_menu()
{
    clock_step_menu();
}

/// The popup that is up, laid out by the subject it belongs to: by the
/// popup's kind, and for a menu or a preview by its level's tag (the chart
/// numbers ride both kinds) and then its level's kind (the clock's menu is
/// its calendar). Returns the next free element.
public uint dispatch_layout_popup(uint next, bool open, float2 popup, float ppad)
{
    switch (private[0].popup_kind)
    {
    case POPUP_TOAST:
        return layout_toast(next, open, popup, ppad);
    case POPUP_PREVIEW:
        switch (id_tag(menu_level_id()))
        {
        case ID_CHART:
            return layout_numbers(next, open, popup, ppad);
        default:
            return layout_preview(next, open, popup, ppad);
        }
    case POPUP_MENU:
        switch (id_tag(menu_level_id()))
        {
        case ID_CHART:
            return layout_numbers(next, open, popup, ppad);
        default:
            break;
        }
        switch (item_of(menu_level()).kind)
        {
        case ITEM_CLOCK:
            return layout_calendar(next, open, popup, ppad);
        default:
            return layout_menu(next, open, popup, ppad);
        }
    default:
        return next;
    }
}

/// What a subject writes into the elements the layout placed: the dock's
/// derived flags.
public void dispatch_placed()
{
    place_dock_flags();
}

/// The popup a subject opens by itself, once a frame after the layout: the
/// toasts, told whether this frame is a rebuild.
public void dispatch_step_popup(bool rebuild)
{
    step_toast(rebuild);
}

/// Each subject's clock, `dt` seconds on. True while any of them wants
/// frames.
///
/// One exception to "nothing but the switch": the dock's dwell is told
/// whether the popup level is the chart numbers', which ride the preview
/// kind too, because the dock names no other subject and the answer is
/// sysmon's.
public bool dispatch_step(float dt)
{
    bool dwelling = step_dwell(dt, numbers_level());
    bool resting = step_numbers(dt);
    bool stirred = step_wake(dt);
    return dwelling || resting || stirred;
}

/// Whether a subject's rows moved with no fact moving, which is a rebuild:
/// a samples tick under the chart numbers.
public bool dispatch_ticked()
{
    return sysmon_ticked();
}

/// Each subject's line of a `CMD_DUMP_STALL` answer.
public void dispatch_stall()
{
    wake_stall();
}

// ---------------------------------------------------------------------------
// bar_draw
// ---------------------------------------------------------------------------

/// The bar's slab, lit by the subjects that light it: the wake.
public float4 dispatch_draw_slab(float2 p, float4 col)
{
    return wake_light(p, col);
}

/// The preview element, `ELEM_PREVIEW`: the dock's captured still.
public float4 dispatch_draw_preview(Element e, float2 p, float4 col)
{
    return draw_preview(e, p, col);
}

/// An element whose kind is its own thing entirely, drawn and `whole`: a
/// chart, a line of the chart numbers, a slider, a separator row. `whole`
/// false leaves it to the button dressing and `dispatch_draw_marks`.
public float4 dispatch_draw_own(uint kind, uint idx, Element e, Item it, float2 p, float4 col,
                                out bool whole)
{
    whole = true;
    switch (kind)
    {
    case ITEM_CHART:
        return draw_chart(e, it, p, col);
    case ITEM_CHART_LINE:
        return draw_numbers_line(e, it, p, col);
    case ITEM_SLIDER:
        return draw_slider(idx, e, it, p, col);
    case ITEM_MENU_ROW:
        return draw_row_own(e, it, p, col, whole);
    default:
        whole = false;
        return col;
    }
}

/// What a kind draws over the button dressing: a dock entry's running and
/// focus marks, a menu row's check and chevron, an applet chip's level.
public float4 dispatch_draw_marks(uint kind, Element e, Item it, float2 p, float4 col)
{
    switch (kind)
    {
    case ITEM_DOCK:
        return draw_dock_marks(e, it, p, col);
    case ITEM_MENU_ROW:
        return draw_row_marks(e, it, p, col);
    case ITEM_APPLET:
        return draw_applet_level(e, it, p, col);
    default:
        return col;
    }
}

/// An item's text and icons: a line of the chart numbers is its subject's,
/// every other kind's is laid out as `item_content_w` measured it.
public float4 dispatch_draw_content(uint kind, Element e, Item it, float2 p, float4 col)
{
    switch (kind)
    {
    case ITEM_CHART_LINE:
        return draw_numbers_content(e, it, p, col);
    default:
        return draw_item_content(e, it, p, col);
    }
}

// ---------------------------------------------------------------------------
// The witnesses' names
// ---------------------------------------------------------------------------

/// What a witness line calls item `it`, by its tag (docs/abi.md, *The
/// witness ring*).
public NameRef dispatch_name(Item it)
{
    switch (id_tag(it.id))
    {
    case ID_OVERVIEW:
        return dock_overview_name(it);
    case ID_ENTRY:
        return dock_entry_name(it);
    case ID_WINDOW:
        return dock_window_name(it);
    case ID_DOCK_ROW:
        return dock_row_name(it);
    case ID_CLOCK:
        return clock_name(it);
    case ID_CALENDAR:
        return calendar_cell_name(it);
    case ID_CAFFEINE:
        return applets_caffeine_name(it);
    case ID_APPLET:
        return applets_name(it);
    case ID_CHART:
        return sysmon_chart_name(it);
    case ID_NUMBERS:
        return sysmon_line_name(it);
    default:
        return name_plain(NAME_ITEM);
    }
}
