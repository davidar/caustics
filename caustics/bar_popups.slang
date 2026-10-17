// The popup, as a window mechanism: asking for one, ending one, what is
// forgotten when it ends, and the menu level it walks. What a popup *shows*
// (a menu's rows, a toast, the calendar, the chart numbers, a preview) is the
// subject's that asked for it; this module is the life of the surface.
//
// The popup's states (`POPUP_IDLE` to `POPUP_ENDING`) are `bar_state`'s,
// beside the `Private` fields they are kept in.

module bar_popups;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import shape;
import bar_state;

/// Forget everything the popup was: called when it closes for any reason.
public void reset_popup_scratch()
{
    private[0].dwell_secs = 0.0;
    private[0].dwell_candidate = -1.0;
    private[0].preview_owner = -1.0;
    private[0].menu_level = id_zero();
    private[0].popup_slot = NO_INDEX;
    private[0].popup_row = 0u;
    private[0].popup_sent = float2(0.0, 0.0);
    private[0].menu_scroll = 0.0;
    private[0].menu_scroll_max = 0.0;
    private[0].menu_clip = float2(0.0, 0.0);
}

/// Is a popup asked for or up?
public bool popup_live()
{
    uint st = private[0].popup_state;
    return st == POPUP_ASKED || st == POPUP_UP;
}

/// A toast that was on screen is ending: tell its sidecar which rows it
/// showed, one `ACTION_TOAST_SHOWN` per row (an `ActionBody` names one row),
/// so the sidecar marks exactly those shown.
/// Only a toast the compositor **mapped** showed anything — one refused, or
/// replaced before it mapped, says nothing, and the sidecar's own clock is
/// what retires its rows. Called from every route that ends a toast while it
/// is still `POPUP_UP`: `close_popup` (the chip stopped asking, its rows went,
/// a click), `request_popup` (a menu replacing it) and `CMD_POPUP_DONE` (the
/// compositor dismissing it); the state moving on is what keeps it to once.
public void say_toast_shown()
{
    if (private[0].popup_kind != POPUP_TOAST || private[0].popup_state != POPUP_UP)
    {
        return;
    }
    uint slot = private[0].toast_slot;
    uint count = min(private[0].toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count; ++k)
    {
        Args a = args_new();
        arg(a, slot);
        arg(a, private[0].toast_ids[k]);
        arg(a, ACTION_TOAST_SHOWN);
        arg(a, 0u);              // no button
        arg2(a, float2(0.0, 0.0));
        argf(a, 0.0);
        arg(a, 0u);              // no token
        push_action(ACT_SIDECAR, a);
    }
    witness2(WITNESS_INFO, getStringHash("toast: shown slot={0} rows={1}"), slot, count);
}

/// Ask the embassy for a popup, as one message (docs/abi.md, *The action ring*).
///
/// `kind` is what decides whether the popup takes a grab: a menu does, a
/// preview must not, or the pointer could never reach the next dock entry.
/// `slot` and `row` are the sidecar and the row the open belongs to, or
/// NO_INDEX for a dock entry's own menu, which has nobody at the other end;
/// the embassy sends that sidecar an `ACTION_MENU_OPEN` and remembers who to
/// tell when it closes.
///
/// A request while a popup is up is the replace case — the preview-then-right-
/// click — and the embassy tears the old one down without a `CMD_POPUP_DONE`,
/// because this file has already moved on to the new one.
public void request_popup(uint kind, float4 anchor, Id id, float2 want)
{
    // A toast is the one popup a grabbed menu ends without asking: the
    // replace path below tears its surface down, since the bar has one popup.
    if (popup_live() && private[0].popup_kind == POPUP_TOAST && kind != POPUP_TOAST)
    {
        witness1(WITNESS_INFO, getStringHash("toast: closed ({0:s})"),
                 kind == POPUP_MENU ? getStringHash("menu") : getStringHash("replaced"));
        say_toast_shown();
    }
    uint slot = id_tag(id) == ID_APPLET ? id_aux(id) : NO_INDEX;
    uint row = id_tag(id) == ID_APPLET ? id_payload(id) : 0u;
    uint w = uint(max(want.x, 1.0) + 0.5);
    uint h = uint(max(want.y, 1.0) + 0.5);

    private[0].popup_state = POPUP_ASKED;
    private[0].popup_kind = kind;
    private[0].popup_id = id;
    private[0].popup_slot = slot;
    private[0].popup_row = row;
    private[0].popup_anchor = anchor;
    private[0].popup_want = want;
    private[0].popup_sent = float2(float(w), float(h));

    Args a = args_new();
    arg(a, kind);
    arg(a, slot);
    arg(a, row);
    arg4(a, anchor);
    arg(a, w);
    arg(a, h);
    push_action(ACT_POPUP_OPEN, a);
}

/// End the popup: one `ACT_POPUP_CLOSE`, and the `CMD_POPUP_DONE` that comes
/// back is what resets the rest. Idempotent, so every route to "this popup is
/// over" can call it.
public void close_popup()
{
    if (!popup_live())
    {
        return;
    }
    say_toast_shown();
    private[0].popup_state = POPUP_ENDING;
    act(ACT_POPUP_CLOSE);
}

/// The open toast's index of item `it`, or NO_INDEX when it is none of its
/// rows (or no toast is up).
public uint toast_index_of(Item it)
{
    if (private[0].popup_kind != POPUP_TOAST || id_tag(it.id) != ID_APPLET
        || id_aux(it.id) != private[0].toast_slot)
    {
        return NO_INDEX;
    }
    uint count = min(private[0].toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count; ++k)
    {
        if (private[0].toast_ids[k] == id_payload(it.id))
        {
            return k;
        }
    }
    return NO_INDEX;
}

/// A hover preview that is up when a click opens a menu instead: end its
/// capture session and forget the dwell here — the embassy swaps the surface
/// under the `ACT_POPUP_OPEN` that names something else — because nothing
/// downstream would: once the kind is a menu, `step_dwell` no longer sees a
/// preview to tear down.
public void end_preview_for_menu()
{
    if (popup_live() && private[0].popup_kind == POPUP_PREVIEW)
    {
        if (private[0].preview_owner >= 0.0)
        {
            act(ACT_PREVIEW_END);
        }
        private[0].dwell_secs = 0.0;
        private[0].dwell_candidate = -1.0;
        private[0].preview_owner = -1.0;
    }
}

/// Up one menu level. The back row is present only while there is one to go
/// up to, so this cannot walk off the top.
public void menu_ascend()
{
    uint level = menu_level();
    if (level < live_items())
    {
        set_menu_level(private[0].items[level].parent);
    }
}

/// Where the pointer is, as this file keeps it (docs/abi.md, *The action ring*). The
/// position and its target arrive as commands and live in `private`, which
/// is what makes the pointer a message like every other input.
public void set_pointer(uint target, float2 pos)
{
    private[0].pointer = pos;
    private[0].pointer_target = target;
}
