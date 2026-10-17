// The dock: the overview button, one entry per app (the favourites, then
// what is running), the windows under each, the entry's own menu, and the
// hover preview of its most recent window. The facts are the embassy's (the
// windows niri reports, the apps table) and every answer about an app is
// `caustics-apps`'s; what an entry *is* (running, focused, urgent, which
// window a click raises) is decided here, from the item table. Its state
// between dispatches is `Private.dock` (`DockScratch`).
//
// It names no other subject. Its item kinds and id tags are bound by
// `bar_dispatch` (`DOCK_*`, below).

module dock;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_popups;
import bar_layout;
import bar_paint;
import bar_table;

/// The dock's item kinds (an entry, a window under one, the overview button,
/// a row of an entry's menu) and the id tags they carry: the bar's
/// `ITEM_DOCK`, `ITEM_WINDOW`, `ITEM_OVERVIEW` and `ITEM_MENU_ROW`, and
/// `ID_ENTRY`, `ID_WINDOW`, `ID_OVERVIEW` and `ID_DOCK_ROW` today, bound by
/// `bar_dispatch`. An entry's menu rows are the bar's menu rows, laid out and
/// drawn as any menu's; only what a click on one does is the dock's.
extern static const uint DOCK_ENTRY;
extern static const uint DOCK_WINDOW;
extern static const uint DOCK_OVERVIEW;
extern static const uint DOCK_ROW;
extern static const uint DOCK_ENTRY_TAG;
extern static const uint DOCK_WINDOW_TAG;
extern static const uint DOCK_OVERVIEW_TAG;
extern static const uint DOCK_ROW_TAG;

/// A dock entry's IFLAG_*, derived from what exists rather than read off the
/// item: the embassy reports windows and favourites and decides nothing, so
/// whether an entry is running, focused or urgent is this file's question to
/// answer from the table. `windows` comes back as the number of window
/// children, which is what the running marks count.
///
/// PINNED is "the entry has a favourite index"; RUNNING, FOCUSED and URGENT
/// are its children's. One walk of the item table per *placed* dock entry —
/// never per texel, which is why `place_dock_flags` stores the answer in the element and
/// `draw_element` reads it from there.
public uint dock_flags(uint entry, out uint windows)
{
    windows = 0u;
    uint flags = item_favourite(item_of(entry)) != NO_INDEX ? IFLAG_PINNED : 0u;
    uint n = live_items();
    for (uint i = 0u; i < n; ++i)
    {
        Item it = private[0].items[i];
        if (it.kind != DOCK_WINDOW || it.parent != entry)
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
public void place_dock_flags()
{
    uint n = live_elements();
    for (uint e = 0u; e < n; ++e)
    {
        uint item = private[0].elements[e].item;
        if (item_of(item).kind != DOCK_ENTRY)
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
public bool window_newer(Item a, Item b)
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
public uint first_window(uint entry)
{
    uint n = live_items();
    uint best = NO_INDEX;
    for (uint i = 0u; i < n; ++i)
    {
        if (private[0].items[i].kind != DOCK_WINDOW || private[0].items[i].parent != entry)
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
public uint pick_window_index(uint entry)
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
        if (it.kind != DOCK_WINDOW || it.parent != entry)
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
            if (it.kind != DOCK_WINDOW || it.parent != entry || i == focused)
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
public Id pick_window(uint entry, Id fallback)
{
    uint pick = pick_window_index(entry);
    if (pick == NO_INDEX)
    {
        return fallback;
    }
    return private[0].items[pick].id;
}

/// A dock entry: the middle button is "another one of those", the right one
/// its own menu, and every other button the window to focus.
public void dock_click_entry(uint ii, Item it, uint elem, uint button, float2 pos)
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
public void dock_click_overview(uint button)
{
    if (button == BTN_MIDDLE || button == BTN_RIGHT)
    {
        return;
    }
    act(ACT_TOGGLE_OVERVIEW);
}

/// A dock entry's menu row: the verb the tunable named, with its
/// own operands. `ACT_CLOSE_WINDOW` carries the window the entry's
/// own left click would act on, so Close and "click, then close"
/// cannot disagree; the other three carry the entry's app id range.
/// Only the left button (any but the middle and the right) acts; a dock
/// menu row is never a separator, disabled or a submenu, since the dock
/// builds it with no flag.
public void dock_click_row(Item it, uint button)
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

/// The title block a preview popup shows: the captured window's, or the dock
/// entry's when the window has since gone.
public uint preview_title(uint entry)
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
public float2 preview_want(uint entry, float ppad)
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
public uint layout_preview(uint next, bool open, float2 popup, float ppad)
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

// ---------------------------------------------------------------------------
// Hover dwell and the preview's life. The one piece of policy that is a clock
// rather than an event.
// ---------------------------------------------------------------------------

/// Advance the dwell timer, ask for a preview when it fills, and end one when
/// the pointer has left both the entry and the popup. Returns true while the
/// clock is still running, so the frame loop knows to come back.
///
/// `numbers` is whether the popup level is the chart numbers', which ride
/// the same popup kind without a capture behind them: the dispatch asks
/// their subject, because this one names no other.
public bool step_dwell(float dt, bool numbers)
{
    uint n = live_items();

    // What the pointer is on, resolved to an item once per frame by
    // `resolve_hover_item`, so every branch below reads the same answer.
    uint h = private[0].hover_element;
    uint hover_elem = h < live_elements() ? h : NO_INDEX;
    uint hitem = private[0].hover_item;

    // A dock entry's preview: the chart numbers ride the same kind without a
    // capture behind them, and their own subject owns their life.
    bool preview_live = private[0].popup_kind == POPUP_PREVIEW && popup_live()
                        && !numbers;

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
        && private[0].items[hitem].kind == DOCK_ENTRY
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

/// The overview button: the first of `OVERVIEW_ICONS` anything can draw, and
/// `OVERVIEW_GLYPH`'s one character when nothing can.
public void build_overview(inout Build b, uint icon_px_u)
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
    it.kind = DOCK_OVERVIEW;
    it.icon_block = icon;
    it.id = id_tagged(DOCK_OVERVIEW_TAG, 0u, 0u);
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
public uint build_entry(inout Build b, uint app_row, uint favourite, uint text_px, uint icon_px_u)
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
    it.kind = DOCK_ENTRY;
    it.block = block;
    it.icon_block = icon;
    // The entry's name, as this file names it: its own hash of the app id's
    // bytes (docs/abi.md, *The arenas*). No number here crosses the buffer — the
    // level the menu is held at is this id, and a click carries the range.
    it.id = id_tagged(DOCK_ENTRY_TAG, 0u, hash_bytes(SRC_STRINGS, app.id_off, app.id_len));
    it.a = favourite;
    // The operands every action about this entry carries: the app id's own
    // bytes, which an `ACT_SPAWN`, `ACT_PIN` or `ACT_UNPIN` hands the embassy
    // whole (docs/abi.md, *The action ring*), and which the `layout:` line names it by.
    it.arg0 = app.id_off;
    it.arg1 = app.id_len;
    return build_push(b, it);
}

/// Whether a dock menu row's `WHEN_*` holds. The two facts it asks about —
/// is the app running, is it pinned — are both in the facts buffer, so the
/// caustic answers them itself.
public bool build_row_applies(uint when, bool running, bool pinned)
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

/// The running marks and the focus underline. They take opposite sides of the
/// chip — marks against the screen edge, the underline on the free edge — so
/// `EDGE_BOTTOM` swaps both and they still never collide.
public float4 draw_dock_marks(Element e, Item it, float2 p, float4 col)
{
    float s = scale_px();
    bool bottom = inputs.edge == EDGE_BOTTOM;

    // Both of these are the layout's answers, not the CPU's: `place_dock_flags` counted
    // the entry's window children once (`dock_flags`), and
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

/// The captured still, letterboxed into its element and clipped by the same
/// rounded carve the visibility pass used.
public float4 draw_preview(Element e, float2 p, float4 col)
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

// ---------------------------------------------------------------------------
// What the passes ask of the dock, through `bar_dispatch`
// ---------------------------------------------------------------------------

/// What a `layout:` line calls the overview button.
public NameRef dock_overview_name(Item it)
{
    return name_plain(NAME_OVERVIEW);
}

/// What a `layout:` line calls an entry: the app id's own bytes, which are
/// the operand words an `ACT_SPAWN` carries too — one range, read twice.
public NameRef dock_entry_name(Item it)
{
    return name_of(NAME_ENTRY, it.arg0, it.arg1, 0u, 2u);
}

/// What a `layout:` line calls a window: `window-{id}`.
public NameRef dock_window_name(Item it)
{
    return name_of(NAME_WINDOW, it.arg0, it.arg1, 0u, 2u);
}

/// What a `layout:` line calls a row of an entry's menu: `dock-pin`, not
/// `dock-<app id>-pin`: exactly one dock menu can be open,
/// so the verb alone names the row, and it is what `bar/bar-test.sh` clicks.
public NameRef dock_row_name(Item it)
{
    return name_of(NAME_DOCK_ROW, dock_verb_literal(item_dock_verb(it)), 0u, 0u, 1u);
}

/// The visible half of the dock: the overview button, then one entry per
/// app the dock shows, with labels at `text_px` and icons at `icon_px_u`.
/// Each app's window count and entry index are kept for the rest of this
/// build (`DockScratch`).
public void dock_build(inout Build b, uint icon_px_u, uint text_px)
{
    // Both arrays initialised in full: the menu and the windows further on
    // in this build read them, and an entry this build did not make must
    // read as none.
    for (uint i = 0u; i < MAX_APPS; ++i)
    {
        private[0].dock.build_windows[i] = 0u;
        private[0].dock.build_entry[i] = NO_INDEX;
    }

    uint windows = fact_window_count();
    uint apps = fact_app_count();
    for (uint w = 0u; w < windows; ++w)
    {
        uint row = facts[0].windows[w].app_row;
        if (row < apps)
        {
            private[0].dock.build_windows[row] += 1u;
        }
    }

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
        if (!pinned && private[0].dock.build_windows[a] == 0u
            && !DOCK_KEEPS_UNPINNED_WITHOUT_WINDOWS)
        {
            continue;
        }
        uint place = pinned ? favourites : NO_INDEX;
        favourites += pinned ? 1u : 0u;
        private[0].dock.build_entry[a] = build_entry(b, a, place, text_px, icon_px_u);
    }
}

/// A dock entry's own menu, while entry `level` is the menu level: the
/// `DOCK_MENU` table's rows, where the `WHEN_*` holds, at `px`. Both facts
/// it asks about (running, pinned) are in the facts buffer (docs/abi.md,
/// *The facts*).
public void dock_build_menu(inout Build b, Id level, uint px)
{
    uint apps = fact_app_count();
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
    uint parent = row_of == NO_INDEX ? NO_INDEX : private[0].dock.build_entry[row_of];
    if (parent != NO_INDEX)
    {
        bool running = private[0].dock.build_windows[row_of] > 0u;
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
            uint block = build_text(b, SRC_STRINGS, lit.off, lit.len, px, LABEL_INK, spec.x);
            if (block == NO_INDEX)
            {
                continue;
            }
            Item it = item_none();
            it.kind = DOCK_ROW;
            it.parent = parent;
            it.block = block;
            it.id = id_tagged(DOCK_ROW_TAG, spec.y, hash);
            it.a = spec.y;
            // The entry's app id range, so `ACT_SPAWN`, `ACT_PIN` and
            // `ACT_UNPIN` carry it straight off the row.
            it.arg0 = facts[0].apps[row_of].id_off;
            it.arg1 = facts[0].apps[row_of].id_len;
            build_push(b, it);
        }
    }
}

/// Every window under an entry this build made, last of all the items so
/// that window churn never moves a visible index, its title at `px`.
public void dock_build_windows(inout Build b, uint px)
{
    uint windows = fact_window_count();
    uint apps = fact_app_count();
    for (uint w = 0u; w < windows; ++w)
    {
        WindowFact win = facts[0].windows[w];
        if (win.app_row >= apps || private[0].dock.build_entry[win.app_row] == NO_INDEX)
        {
            continue;
        }
        uint off;
        uint len;
        build_first_line(SRC_STRINGS, win.title_off, win.title_len, off, len);
        Item it = item_none();
        it.kind = DOCK_WINDOW;
        it.parent = private[0].dock.build_entry[win.app_row];
        it.block = build_text(b, SRC_STRINGS, off, len, px, LABEL_INK, 0u);
        it.id = id_tagged(DOCK_WINDOW_TAG, win.id_hi, win.id_lo);
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
}

/// `dock: N entries (M running)`, once per change, over the build's first
/// `items`.
public void dock_said_build(uint items)
{
    uint entries = 0u;
    uint running = 0u;
    for (uint i = 0u; i < items; ++i)
    {
        uint kind = private[0].items[i].kind;
        if (kind == DOCK_ENTRY)
        {
            entries += 1u;
            // The caustic's own rule, said out loud: an entry is running when
            // something in the table is a window of it.
            for (uint c = 0u; c < items; ++c)
            {
                if (private[0].items[c].kind == DOCK_WINDOW
                    && private[0].items[c].parent == i)
                {
                    running += 1u;
                    break;
                }
            }
        }
    }
    if (entries != private[0].dock.said_entries || running != private[0].dock.said_running)
    {
        private[0].dock.said_entries = entries;
        private[0].dock.said_running = running;
        witness2(WITNESS_INFO, getStringHash("dock: {0} entries ({1} running)"),
                 entries, running);
    }
}
