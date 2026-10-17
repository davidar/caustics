// The applets: the generic rows machinery every sidecar's applet rides on.
// A sidecar publishes rows (docs/abi.md, *Rows*), and this module turns
// them into a chip on the bar with its label and its level,
// the chip's menu (menu rows, sliders, a row's body), and what a click,
// a press and a drag on any of them sends back as `ACT_SIDECAR`. The tray,
// the battery, the network, the audio and the notifications are all this
// one kind, told apart only by what their rows say; so is the bar's own
// caffeine chip. Its state between dispatches is `Private.applets`
// (`AppletsScratch`).
//
// The menu machinery here lays out and draws any menu level's rows, a dock
// entry's own menu among them (`bar_dispatch` routes it here); what a click
// on a dock row does is the dock's. It names no other subject, and its item
// kinds and id tags are bound by `bar_dispatch` (`APPLETS_*`, below).

module applets;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_popups;
import bar_layout;
import bar_paint;
import bar_table;

/// The applets' item kinds (a chip on the bar, a slider and a row of a
/// menu) and the id tags they carry (a sidecar's row, the caffeine chip):
/// the bar's `ITEM_APPLET`, `ITEM_SLIDER`, `ITEM_MENU_ROW`, `ID_APPLET` and
/// `ID_CAFFEINE` today, bound by `bar_dispatch`.
extern static const uint APPLETS_CHIP;
extern static const uint APPLETS_SLIDER;
extern static const uint APPLETS_ROW;
extern static const uint APPLETS_TAG;
extern static const uint APPLETS_CAFFEINE_TAG;

// ===========================================================================
// Sliders. One definition of where the track is, read by the layout, by the
// drag arithmetic in `apply_command` and by the paint — which is what makes
// grabbing the knob not jump.
// ===========================================================================

/// The track inside a slider row's rect: the row inset by the menu gutter on
/// each side (so it lines up with the label column above it), across the
/// bottom band.
public float4 slider_track(float4 rect)
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
public float2 slider_travel(float4 rect)
{
    float knob = SLIDER_KNOB * scale_px();
    float4 t = slider_track(rect);
    return float2(t.x + knob * 0.5, max(t.z - knob, 1.0));
}

/// Pointer x (popup px) -> value 0..1. The exact inverse of the knob's
/// placement in `draw_slider`.
public float slider_value_at(float4 rect, float x)
{
    float2 travel = slider_travel(rect);
    return saturate((x - travel.x) / travel.y);
}

/// A press or a motion while pressed, on a slider row: push the value the
/// pointer is at. Called from CMD_POINTER_DOWN (a click on the track is a
/// seek) and from CMD_POINTER_MOTION (the drag), and the release goes through
/// `click` like every other release.
public void drag_slider(uint target, float2 pos)
{
    uint p = private[0].pressed_element;
    if (p >= live_elements() || private[0].elements[p].target != target)
    {
        return;
    }
    Item it = item_of(element_item(p));
    if (it.kind != APPLETS_SLIDER || (it.flags & IFLAG_DISABLED) != 0u)
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

/// The caffeine chip, the one chip that is not a sidecar's at all: its
/// inhibitor is an object on the bar's own Wayland connection. Only the left
/// button (and any but the middle and the right) toggles it.
public void applets_click_caffeine(uint button)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT)
    {
        return;
    }
    act(ACT_TOGGLE_CAFFEINE);
}

/// One kind for every chip a sidecar publishes — the tray icons, the
/// battery and the power button among them.
public void applets_click_chip(uint ii, Item it, uint elem, uint button, float2 pos)
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
public void applets_click_row(uint ii, Item it, uint button, float2 pos)
{
    if (button == BTN_MIDDLE || (it.flags & (IFLAG_SEPARATOR | IFLAG_DISABLED)) != 0u)
    {
        return;
    }
    if (button == BTN_RIGHT)
    {
        if (id_tag(it.id) == APPLETS_TAG)
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

/// A slider ends its drag where the pointer left it, and the popup stays
/// up: unlike a leaf menu row, moving a volume is not "done with this
/// menu". The press and every motion in between already pushed the same
/// kind, so the sidecar sees a stream ending on the final position. The
/// middle and the right button do nothing.
public void applets_click_slider(Item it, uint elem, uint button, float2 pos)
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

/// A menu row's height in this popup: a plain row's, and under it the body
/// lines shown and the pad below the last.
public float level_row_h(Item it)
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
public float level_row_w(Item it)
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
public bool level_row(Item it, uint level)
{
    return menu_row_kind(it.kind) && it.parent == level;
}

/// The menu rows of the current level, measured and placed. Returns the next
/// free element index.
///
/// A toast has a layout of its own (`layout_toast`): the
/// rows it opened with, where it opened with them.
public uint layout_menu(uint next, bool open, float2 popup, float ppad)
{
    float s = scale_px();
    uint n = live_items();
    uint level = menu_level();
    float gutter = MENU_GUTTER * s;

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

/// The caffeine chip, or nothing at all: a state whose icons no theme has
/// would be an empty square, and an empty square is worse than no chip.
public void build_caffeine(inout Build b, uint icon_px_u)
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
    it.kind = APPLETS_CHIP;
    it.icon_block = icon;
    it.id = id_tagged(APPLETS_CAFFEINE_TAG, 0u, 0u);
    it.a = APPLET_CAFFEINE;
    it.f = -1.0;
    build_push(b, it);
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
public uint build_body(inout Build b, uint slot, Row row, uint px, out uint meta)
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
public uint build_applet_flags(uint flags)
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
public uint build_row_flags(uint role, uint flags)
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

/// A menu row's gutters: the check column on the left, the submenu chevron on
/// the right. SDF strokes, so the bar needs no symbol font for two shapes.
public float4 draw_row_marks(Element e, Item it, float2 p, float4 col)
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
public float4 draw_applet_level(Element e, Item it, float2 p, float4 col)
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
public float4 draw_slider(uint idx, Element e, Item it, float2 p, float4 col)
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

public float applet_level(Item it) { return item_level(it, item_row(it)); }
public float slider_position(Item it) { return item_position(it, item_row(it)); }

// ---------------------------------------------------------------------------
// What the passes ask of the applets, through `bar_dispatch`
// ---------------------------------------------------------------------------

/// What a `layout:` line calls the caffeine chip.
public NameRef applets_caffeine_name(Item it)
{
    return name_plain(NAME_CAFFEINE);
}

/// What a `layout:` line calls a sidecar's chip, menu row or slider:
/// `applet-{name}` or `applet-{name}-{row}`.
public NameRef applets_name(Item it)
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
    return private[0].applets.slot_first_item[slot] == row
        ? name_of(NAME_APPLET, sc.name_off, sc.name_len, 0u, 2u)
        : name_of(NAME_APPLET_N, sc.name_off, sc.name_len, row, 3u);
}

/// The chips: caffeine's, then one per `ROW_ITEM` of every present slot,
/// labels at `text_px`, icons at `icon_px_u`. The open menu's slot keeps
/// its chips' item indices for `applets_build_menu`.
public void applets_build(inout Build b, uint icon_px_u, uint text_px)
{
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
    uint level_slot = id_tag(level) == APPLETS_TAG ? id_aux(level) : NO_INDEX;
    // Frame row index -> item index, for the menu pass `applets_build_menu`
    // makes further on in this build. One slot's menu is open at a time, so
    // one array is enough.
    for (uint i = 0u; i < MAX_ROWS; ++i)
    {
        private[0].applets.build_placed[i] = NO_INDEX;
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
            it.kind = APPLETS_CHIP;
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
                it.block = build_row_label(b, slot, row, text_px);
            }
            it.id = id_tagged(APPLETS_TAG, slot, row.id);
            it.a = head.applet;
            it.flags = build_applet_flags(row.flags);
            // The level is the row's, read where it is drawn: a frame that
            // moves only a value is one buffer write and no rebuild at all.
            it.fact_row = slot * MAX_ROWS + r;
            uint index = build_push(b, it);
            if (slot == level_slot && index != NO_INDEX)
            {
                private[0].applets.build_placed[r] = index;
            }
        }
        if (first_item != NO_INDEX)
        {
            private[0].applets.slot_first_item[slot] = first_item;
        }
    }
}

/// The rows of the open menu, while a sidecar's row `level` is the menu
/// level: labels at `row_px` (a slider's at `slider_px`), icons at
/// `icon_px_u`. True when any row has a body, which the build answers with
/// the one ellipsis every cut body ends in.
public bool applets_build_menu(inout Build b, Id level, uint icon_px_u, uint row_px,
                               uint slider_px)
{
    uint slot = id_aux(level);
    bool bodies = false;
    if (slot >= MAX_SLOTS)
    {
        return bodies;
    }
    uint menu_rows = rows_header(slot).row_count;
    for (uint r = 0u; r < menu_rows; ++r)
    {
        Row row = row_at(slot, r);
        if (row.role == ROW_ITEM)
        {
            continue;
        }
        // The frame reader guarantees a parent precedes its children, so
        // one pass places the whole tree. A row whose parent was dropped —
        // the table filled up — is dropped with it rather than hung off
        // item 0.
        if (row.parent >= MAX_ROWS || private[0].applets.build_placed[row.parent] == NO_INDEX)
        {
            continue;
        }
        bool separator = row.role == ROW_SEPARATOR;
        bool slider = row.role == ROW_SLIDER;
        Item it = item_none();
        it.kind = slider ? APPLETS_SLIDER : APPLETS_ROW;
        it.parent = private[0].applets.build_placed[row.parent];
        it.block = separator ? NO_INDEX
                             : build_row_label(b, slot, row, slider ? slider_px : row_px);
        it.icon_block = separator ? NO_INDEX : build_row_icon(b, slot, row, icon_px_u);
        if (row.role == ROW_MENU)
        {
            // The label is the row's title and the body hangs under it.
            uint meta;
            it.aux_block = build_body(b, slot, row, row_px, meta);
            it.body_blocks = it.aux_block == NO_INDEX ? 0u : meta;
            bodies = bodies || it.aux_block != NO_INDEX;
        }
        it.id = id_tagged(APPLETS_TAG, slot, row.id);
        it.flags = build_row_flags(row.role, row.flags);
        if (slider)
        {
            it.fact_row = slot * MAX_ROWS + r;
        }
        uint index = build_push(b, it);
        if (index != NO_INDEX)
        {
            private[0].applets.build_placed[r] = index;
        }
    }
    return bodies;
}

/// `applets: N`, once per change, over the build's first `items`.
public void applets_said_build(uint items)
{
    uint applets = 0u;
    for (uint i = 0u; i < items; ++i)
    {
        if (private[0].items[i].kind == APPLETS_CHIP)
        {
            applets += 1u;
        }
    }
    if (applets != private[0].applets.said)
    {
        private[0].applets.said = applets;
        witness1(WITNESS_INFO, getStringHash("applets: {0}"), applets);
    }
}

/// A separator is a hairline and nothing else — no chip, no hover.
public float4 draw_separator(Element e, float2 p, float4 col)
{
    float s = scale_px();
    float h = SEPARATOR_PX * s;
    float2 org = float2(e.rect.x, e.rect.y + floor((e.rect.w - h) * 0.5));
    return ink(SEPARATOR_RGB,
               SEPARATOR_ALPHA * coverage(rounded_rect_sdf(p - org, float2(e.rect.z, h), 0.0)),
               col);
}
