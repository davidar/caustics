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
import bar_popups;
import bar_layout;
import bar_paint;
import bar_table;
import sysmon;
import notify;

export static const uint SYSMON_CHART = ITEM_CHART;
export static const uint SYSMON_LINE = ITEM_CHART_LINE;
export static const uint SYSMON_CHART_TAG = ID_CHART;
export static const uint SYSMON_LINE_TAG = ID_NUMBERS;
export static const uint NOTIFY_ROW_TAG = ID_APPLET;

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
    case ID_CHART:    return sysmon_chart_name(it);
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
    case ID_NUMBERS:  return sysmon_line_name(it);
    default: return name_plain(NAME_ITEM);
    }
}

/// One `layout:` line per element whose `(id, target, rect)` moved, out of
/// the frame that moved it (docs/abi.md, *The witness ring*), in the order
/// `place` placed them. Said after the layout and not inside `place`, because
/// naming an element is its subject's (`element_name`).
///
/// **By id**, not by item index. An unpin moves a running app from the
/// pinned run to the running tail, which swaps which app every slot after
/// it holds while leaving both the rects and the item *indices* exactly as
/// they were — so an index-keyed witness would log nothing, and
/// `layout: <app A>` would go on naming a rect that is now app B's.
void witness_placed()
{
    uint count = min(private[0].placed_count, MAX_ELEMENTS);
    for (uint k = 0u; k < count; ++k)
    {
        uint idx = private[0].placed[k];
        if (idx >= MAX_ELEMENTS)
        {
            continue;
        }
        Element e = private[0].elements[idx];
        Item placed_item = item_of(e.item);
        Id id = placed_item.kind == ITEM_NONE ? id_zero() : placed_item.id;
        bool moved = e.said_present == 0u
                     || !e.said_id.eq(id)
                     || e.said_target != e.target
                     || any(e.said_rect != e.rect);
        if (!moved)
        {
            continue;
        }
        NameRef nm = element_name(idx, e.item);
        Args a = name_args(nm);
        arg(a, target_literal(e.target));
        arg4(a, e.rect);
        witness(WITNESS_DEBUG, layout_literal(nm.slot), a);

        private[0].elements[idx].said_present = 1u;
        private[0].elements[idx].said_id = id;
        private[0].elements[idx].said_target = e.target;
        private[0].elements[idx].said_rect = e.rect;
        private[0].elements[idx].said_slot = nm.slot;
        private[0].elements[idx].said_a0 = nm.a0;
        private[0].elements[idx].said_a1 = nm.a1;
        private[0].elements[idx].said_a2 = nm.a2;
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

/// What the CPU does not say about a dock entry (see `dock_flags`), written
/// into every element the layout placed one in. Counted once per placed
/// element, which is a handful of walks per frame; the draw pass runs per
/// texel and must never walk the item table.
void place_dock_flags()
{
    uint n = live_elements();
    for (uint e = 0u; e < n; ++e)
    {
        uint item = private[0].elements[e].item;
        if (item_of(item).kind != ITEM_DOCK)
        {
            continue;
        }
        uint derived_a = 0u;
        private[0].elements[e].derived = dock_flags(item, derived_a);
        private[0].elements[e].derived_a = derived_a;
    }
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

/// A click landed on `elem` with `button` at `pos`: the bar's entire policy,
/// one function per kind. Everything it decides leaves as an **action** with its own
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
    dispatch_click(it.kind, ii, it, elem, button, pos);
}

/// The click on item `ii`, by its kind: the subject's.
void dispatch_click(uint kind, uint ii, Item it, uint elem, uint button, float2 pos)
{
    switch (kind)
    {
    case ITEM_DOCK:
        click_dock_entry(ii, it, elem, button, pos);
        break;
    case ITEM_OVERVIEW:
        click_overview(button);
        break;
    case ITEM_CLOCK:
        click_clock(ii, it, elem, button);
        break;
    case ITEM_CHART:
        sysmon_click(ii, button);
        break;
    case ITEM_APPLET:
        switch (id_tag(it.id))
        {
        case ID_CAFFEINE:
            click_caffeine(button);
            break;
        default:
            click_chip(ii, it, elem, button, pos);
            break;
        }
        break;
    case ITEM_MENU_ROW:
        // A row of the toast that is up is the toast's; any other row is the
        // menu's, and a dock menu's row is the dock's verb.
        switch (popup_live_kind())
        {
        case POPUP_TOAST:
            notify_click_row(it, button, pos);
            break;
        default:
            switch (id_tag(it.id))
            {
            case ID_DOCK_ROW:
                click_dock_row(it, button);
                break;
            default:
                click_menu_row(ii, it, button, pos);
                break;
            }
            break;
        }
        break;
    case ITEM_SLIDER:
        click_slider(it, elem, button, pos);
        break;
    default:
        break;
    }
}

/// A dock entry: the middle button is "another one of those", the right one
/// its own menu, and every other button the window to focus.
void click_dock_entry(uint ii, Item it, uint elem, uint button, float2 pos)
{
    Id own = it.id;
    if (button == BTN_MIDDLE)
    {
        // Middle click is "another one of those" for a dock entry.
        act_app(ACT_SPAWN, it);
        return;
    }
    if (button == BTN_RIGHT)
    {
        // A dock entry opens its own menu — New window, Close, Pin or Unpin
        // (DOCK_MENU in the tunables). `own` is the *entry's* id: the menu is
        // about the app, and `bar_build` publishes the rows of the level held
        // here.
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
        return;
    }
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
}

/// The overview button: any button but the middle and the right toggles it.
void click_overview(uint button)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT)
    {
        return;
    }
    act(ACT_TOGGLE_OVERVIEW);
}

/// The clock: the calendar, a menu whose level is the clock.
/// Clicking the clock while it is up closes it, as an applet's chip does.
/// The middle and the right button do nothing.
void click_clock(uint ii, Item it, uint elem, uint button)
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

/// The caffeine chip, the one chip that is not a sidecar's at all: its
/// inhibitor is an object on the bar's own Wayland connection. Only the left
/// button (and any but the middle and the right) toggles it.
void click_caffeine(uint button)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT)
    {
        return;
    }
    act(ACT_TOGGLE_CAFFEINE);
}

/// One kind for every chip a sidecar publishes — the tray icons, the
/// battery and the power button among them.
void click_chip(uint ii, Item it, uint elem, uint button, float2 pos)
{
    Id own = it.id;
    if (button == BTN_MIDDLE)
    {
        // For an applet the middle button is the sidecar's to interpret, and
        // the wire carries the kind rather than a meaning — `caustics-tray`
        // turns it into SNI's SecondaryActivate, which is what every tray does.
        act_sidecar(it, ACTION_MIDDLE, button, pos, 0.0);
        return;
    }
    if (button == BTN_RIGHT)
    {
        // An applet's secondary click is the sidecar's to interpret, and the
        // wire carries the kind rather than a meaning.
        act_sidecar(it, ACTION_SECONDARY, button, pos, 0.0);
        return;
    }
    if ((it.flags & IFLAG_HAS_MENU) == 0u)
    {
        // No menu: one `ACTION_ACTIVATE` naming the row, with `pos` the
        // physical px this file saw — the embassy divides by the scale,
        // because that is what SNI's `Activate(x, y)` takes at the
        // sidecar's far end.
        act_sidecar(it, ACTION_ACTIVATE, button, pos, 0.0);
        return;
    }
    if (popup_live() && private[0].popup_kind == POPUP_MENU
        && private[0].popup_id.eq(own))
    {
        // Clicking the item whose menu is up closes it, the way every
        // other tray does.
        close_popup();
        return;
    }
    set_menu_level(ii);
    request_popup(POPUP_MENU, private[0].elements[elem].rect, own,
                  private[0].popup_want);
}

/// A sidecar's menu row, not in a toast. The right button is the sidecar's
/// to interpret (`caustics-notify` reads it as "dismiss without acting"):
/// the menu stays up and the row leaves it with the sidecar's next frame.
void click_menu_row(uint ii, Item it, uint button, float2 pos)
{
    if (button == BTN_MIDDLE || (it.flags & (IFLAG_SEPARATOR | IFLAG_DISABLED)) != 0u)
    {
        return;
    }
    if (button == BTN_RIGHT)
    {
        if (id_tag(it.id) == ID_APPLET)
        {
            act_sidecar(it, ACTION_SECONDARY, button, pos, 0.0);
        }
        return;
    }
    if ((it.flags & IFLAG_SUBMENU) != 0u)
    {
        // Descend. The popup stays mapped; the want follows the new level
        // and `ACT_POPUP_MOVE` goes out when the two differ.
        set_menu_level(ii);
        return;
    }
    act_sidecar(it, ACTION_ACTIVATE, button, pos, 0.0);
    close_popup();
}

/// A dock entry's menu row: the verb the tunable named, with its
/// own operands. `ACT_CLOSE_WINDOW` carries the window the entry's
/// own left click would act on, so Close and "click, then close"
/// cannot disagree; the other three carry the entry's app id range.
/// Only the left button (any but the middle and the right) acts; a dock
/// menu row is never a separator, disabled or a submenu, since the dock
/// builds it with no flag.
void click_dock_row(Item it, uint button)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT)
    {
        return;
    }
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
    close_popup();
}

/// A slider ends its drag where the pointer left it, and the popup stays
/// up: unlike a leaf menu row, moving a volume is not "done with this
/// menu". The press and every motion in between already pushed the same
/// kind, so the sidecar sees a stream ending on the final position. The
/// middle and the right button do nothing.
void click_slider(Item it, uint elem, uint button, float2 pos)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT)
    {
        return;
    }
    if ((it.flags & IFLAG_DISABLED) == 0u)
    {
        Row row = item_row(it);
        float at = slider_value_at(private[0].elements[elem].rect, pos.x);
        act_sidecar(it, ACTION_SET_VALUE, button, pos, lerp(row.min, row.max, at));
    }
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
        notify_popup_done();
        sysmon_popup_done();
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

/// The menu rows of the current level, measured and placed. Returns the next
/// free element index.
///
/// A toast has a layout of its own (`layout_toast`): the
/// rows it opened with, where it opened with them.
uint layout_menu(uint next, bool open, float2 popup, float ppad)
{
    float s = scale_px();
    uint n = live_items();
    uint level = menu_level();
    float gutter = MENU_GUTTER * s;

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

/// Every element of every target: the strip (`layout_strip`), then the popup
/// that is up, laid out by the subject it belongs to.
void layout()
{
    float s = scale_px();
    uint next = layout_strip();

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
    else if (kind == POPUP_TOAST)
    {
        next = layout_toast(next, open, popup, ppad);
    }
    else if (kind == POPUP_MENU)
    {
        next = layout_menu(next, open, popup, ppad);
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
    sysmon_build_charts(b);

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
    else if (tag == ID_CHART)
    {
        sysmon_build_numbers(b, level, text_px[ITEM_CHART_LINE]);
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
    sysmon_said_build(b.items);
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
    if (sysmon_reshaped())
    {
        rebuild = true;
    }

    // After the commands, before the layout: the commands may have set a level
    // and the layout must not be given one that no longer resolves.
    step_menu();
    // The bodies' lines, which the layout measures rows by. After the shape
    // pass, which ran before this one.
    wrap_bodies();
    layout();
    place_dock_flags();
    witness_placed();
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
    // the menu is this file's. A samples tick under the chart numbers is the
    // other (`sysmon_ticked`).
    if (sysmon_ticked())
    {
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

/// An item's text and icons: a line of the chart numbers is its subject's,
/// everything else is laid out as `item_content_w` measured it.
float4 draw_content(Element e, Item it, float2 p, float4 col)
{
    if (it.kind == ITEM_CHART_LINE)
    {
        return draw_numbers_content(e, it, p, col);
    }
    return draw_item_content(e, it, p, col);
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
            col = draw_content(e, item_of(e.item), p, col);
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
