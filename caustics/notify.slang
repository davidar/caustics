// The toasts, the subject `caustics-notify` is the first to use: a sidecar's
// chip that carries `RFLAG_SHOW` has its `RFLAG_ATTENTION` rows shown
// without a click, as a popup without a grab. Any sidecar's chip may ask;
// the notifications are the ones that do. Its between-dispatch state is
// `Private.notify` (`NotifyScratch`): the toast's own rows, fixed while it
// is up, and the hold that keeps an ended toast from reopening.
//
// It names no other subject. The id tag a sidecar's rows carry is not named
// here but bound by `bar_dispatch` (`NOTIFY_ROW_TAG`, below).

module notify;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_popups;
import bar_layout;

/// The id tag every sidecar row's item carries, which a toast's chip and its
/// rows are named by: the bar's `ID_APPLET` today, bound by `bar_dispatch`.
extern static const uint NOTIFY_ROW_TAG;

// ---------------------------------------------------------------------------
// Toasts: a sidecar's chip that carries `RFLAG_SHOW`
// has its `RFLAG_ATTENTION` rows shown without a click, as a popup without a
// grab. Whether a chip asks, and which rows it would show, is read off the rows
// arena, where the sidecar's frame is verbatim.
//
// **A toast's rows are fixed while it is up.** When a chip first asks, its
// level is set and nothing opens: the toast waits a frame for `bar_build` to
// build those rows, the shaper to shape them and `wrap_bodies` to wrap them,
// and then opens at exactly their size — ids, order, body lines and heights
// taken there and kept in `Private` until it ends. A frame that arrives while it
// is up changes none of that: a new attention row waits for this toast to end
// (`toast: held`), a row the sidecar removed leaves a gap rather than moving
// the rows under the pointer, and the toast ends when none of its own rows is
// still in the frame and still flagged. The size it opened at is the size it
// keeps, so it never moves. And a click on one of its rows holds it as a
// refusal does, so the next frame cannot reopen it with "the rest".
// ---------------------------------------------------------------------------

/// The frame index of slot `slot`'s `ROW_ITEM` whose id is `id`, or NO_INDEX
/// when the frame no longer carries it (or the slot has no frame at all).
public uint chip_row(uint slot, uint id)
{
    if (slot >= MAX_SLOTS)
    {
        return NO_INDEX;
    }
    uint n = rows_header(slot).row_count;
    for (uint r = 0u; r < n; ++r)
    {
        uint at = 16u + r * 48u;
        if (rows_word(slot, at + 8u) == ROW_ITEM && rows_word(slot, at) == id)
        {
            return r;
        }
    }
    return NO_INDEX;
}

/// Is frame row `row` one a toast of chip `chip` shows: a direct child of the
/// chip, flagged `RFLAG_ATTENTION`, and a row or a slider — never a separator.
public bool toast_row(Row row, uint chip)
{
    return row.parent == chip && (row.flags & RFLAG_ATTENTION) != 0u
        && (row.role == ROW_MENU || row.role == ROW_SLIDER);
}

/// Rows one toast holds: TOAST_MAX_ROWS, within what `Private` keeps.
public uint toast_cap()
{
    return min(TOAST_MAX_ROWS, TOAST_ROWS_CAP);
}

/// How many rows a toast of chip row `chip` in `slot` would show, at most
/// `toast_cap()`, in the frame's own order (newest first, as the sidecar
/// ordered them); `newest` is the first one's id, NO_INDEX for none.
public uint toast_rows(uint slot, uint chip, out uint newest)
{
    newest = NO_INDEX;
    if (chip == NO_INDEX)
    {
        return 0u;
    }
    uint n = rows_header(slot).row_count;
    uint count = 0u;
    for (uint r = chip + 1u; r < n && count < toast_cap(); ++r)
    {
        Row row = row_at(slot, r);
        if (!toast_row(row, chip))
        {
            continue;
        }
        if (count == 0u)
        {
            newest = row.id;
        }
        count += 1u;
    }
    return count;
}

/// Whether row id `id` is one of the open toast's own rows.
public bool toast_holds(uint id)
{
    uint count = min(private[0].notify.toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count; ++k)
    {
        if (private[0].notify.toast_ids[k] == id)
        {
            return true;
        }
    }
    return false;
}

/// End the toast, saying why: the chip stopped asking, its frame went, none
/// of its rows is left, or a row in it was clicked. (A menu replacing it says
/// so in `request_popup`; the compositor ending it, in `CMD_POPUP_DONE`.)
public void end_toast(uint why)
{
    witness1(WITNESS_INFO, getStringHash("toast: closed ({0:s})"), why);
    close_popup();
}

/// Hold the toast hanging off `id` until its news changes: no toast for that
/// chip until it stops asking or a newer row than the newest it has now is
/// first. A refusal, a dismissal and a click all hold, so the
/// frame that follows cannot reopen what the user just ended. It
/// also lifts once none of the ended toast's rows still asks: the sidecar
/// heard `ACTION_TOAST_SHOWN`, and what asks now is what it never showed.
public void hold_toast(Id id)
{
    uint slot = id_aux(id);
    uint chip = chip_row(slot, id_payload(id));
    uint newest;
    toast_rows(slot, chip, newest);
    private[0].notify.toast_hold_slot = slot;
    private[0].notify.toast_hold_chip = id_payload(id);
    private[0].notify.toast_hold_newest = newest;
}

/// The bar element a chip is laid out as — present, on the bar — or NO_INDEX.
public uint bar_element_of(uint item)
{
    uint n = live_elements();
    for (uint e = ELEM_FIRST_FREE; e < n; ++e)
    {
        Element el = private[0].elements[e];
        if (el.item == item && el.target == TARGET_BAR && (el.flags & EFLAG_PRESENT) != 0u)
        {
            return e;
        }
    }
    return NO_INDEX;
}

/// `toast: open slot={0} rows={1} ids=…`: the ids the toast fixed on, in the
/// order it draws them. One literal per count, because a format is a
/// compile-time string.
public void witness_toast_open(uint slot, uint count)
{
    Args a = args_new();
    arg(a, slot);
    arg(a, count);
    for (uint k = 0u; k < count && k < TOAST_ROWS_CAP; ++k)
    {
        arg(a, private[0].notify.toast_ids[k]);
    }
    uint head;
    switch (count)
    {
    case 1u: head = getStringHash("toast: open slot={0} rows={1} ids={2}"); break;
    case 2u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3}"); break;
    case 3u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4}"); break;
    case 4u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5}"); break;
    case 5u: head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6}"); break;
    case 6u:
        head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6},{7}");
        break;
    case 7u:
        head = getStringHash("toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6},{7},{8}");
        break;
    default:
        head = getStringHash(
            "toast: open slot={0} rows={1} ids={2},{3},{4},{5},{6},{7},{8},{9}");
        break;
    }
    witness(WITNESS_INFO, head, a);
}

/// A toast asked for and not open yet: open it once its rows are built,
/// shaped and wrapped, at exactly their size. True when this frame is
/// accounted for — opened, or still waiting; false when the chip that asked
/// has stopped (or another popup took the level), and the caller looks again.
public bool step_toast_pending(bool rebuild)
{
    uint slot = private[0].notify.toast_slot;
    uint chip_id = private[0].notify.toast_chip;
    uint chip = chip_row(slot, chip_id);
    Id id = id_tagged(NOTIFY_ROW_TAG, slot, chip_id);
    bool held = private[0].notify.toast_hold_slot == slot && private[0].notify.toast_hold_chip == chip_id;
    bool asking = chip != NO_INDEX && (row_at(slot, chip).flags & RFLAG_SHOW) != 0u && !held;
    if (!asking || !menu_level_id().eq(id))
    {
        private[0].notify.toast_pending = 0u;
        return false;
    }
    // Built for this level, from the arena as it is now: a frame that arrived
    // this frame with a new shape is built next frame, and the rows are taken
    // from that build.
    uint level = index_of_id(id);
    uint elem = level == NO_INDEX ? NO_INDEX : bar_element_of(level);
    if (elem == NO_INDEX || rebuild || !private[0].built_for.eq(id))
    {
        return true;
    }

    float s = scale_px();
    uint n = rows_header(slot).row_count;
    uint count = 0u;
    float rows_h = 0.0;
    for (uint r = chip + 1u; r < n && count < toast_cap(); ++r)
    {
        Row row = row_at(slot, r);
        if (!toast_row(row, chip))
        {
            continue;
        }
        uint ri = index_of_id(id_tagged(NOTIFY_ROW_TAG, slot, row.id));
        if (ri == NO_INDEX || private[0].items[ri].parent != level)
        {
            return true; // not built yet: the next frame's build has it
        }
        Item it = private[0].items[ri];
        uint lines = min(item_body_line_count(it), min(TOAST_BODY_LINES, MAX_BODY_LINES));
        float h = menu_row_height(it);
        if (lines > 0u)
        {
            h += (float(lines) * MENU_BODY_LINE_H + MENU_BODY_PAD) * s;
        }
        private[0].notify.toast_ids[count] = row.id;
        private[0].notify.toast_lines[count] = lines;
        private[0].notify.toast_h[count] = h;
        rows_h += h;
        count += 1u;
    }
    if (count == 0u)
    {
        private[0].notify.toast_pending = 0u;
        return false; // nothing to show is not a toast
    }

    // As wide as a menu may be — every toast the same width, as a
    // notification's toast is anywhere else, and the width a body wraps to —
    // and as tall as its rows, within the output. Nothing it shows can change
    // either, so the popup never moves.
    float ppad = POPUP_PAD * s;
    float want_h = rows_h + 2.0 * ppad;
    float output_h = float(inputs.output_size.y);
    if (output_h > 0.0)
    {
        float room = output_h - float(inputs.bar_size.y) - POPUP_MARGIN * s;
        want_h = min(want_h, max(room, 2.0 * ppad + MENU_ROW_H * s));
    }
    float2 size = float2(floor(MENU_MAX_W * s + 0.5), floor(want_h + 0.5));
    private[0].notify.toast_count = count;
    private[0].notify.toast_size = size;
    private[0].notify.toast_pending = 0u;
    private[0].notify.toast_said_held = NO_INDEX;
    request_popup(POPUP_TOAST, private[0].elements[elem].rect, id, size);
    witness_toast_open(slot, count);
    return true;
}

/// The toast's whole life, once a frame, after the layout (so the chip's rect
/// is this frame's).
///
/// **Up**: end it when its chip no longer carries `RFLAG_SHOW`, when the chip
/// or its whole frame is gone, or when none of its own rows is still in the
/// frame and flagged; say `toast: held` once for each row that arrives while it
/// is up. **Pending**: open it when its rows are built. **Idle**: for the first
/// chip, in slot order, that asks and has something to show, set the level and
/// wait. Anything else up — a menu, a preview, a popup still ending — and the
/// bit is ignored: a toast never replaces a popup, only a menu replaces a
/// toast.
public void step_toast(bool rebuild)
{
    uint newest;

    if (popup_live() && private[0].popup_kind == POPUP_TOAST)
    {
        Id id = private[0].popup_id;
        uint slot = id_aux(id);
        uint chip = chip_row(slot, id_payload(id));
        if (chip == NO_INDEX)
        {
            end_toast(getStringHash("gone"));
            return;
        }
        if ((row_at(slot, chip).flags & RFLAG_SHOW) == 0u)
        {
            end_toast(getStringHash("flag"));
            return;
        }
        uint live = 0u;
        uint fresh = NO_INDEX;
        uint n = rows_header(slot).row_count;
        for (uint r = chip + 1u; r < n; ++r)
        {
            Row row = row_at(slot, r);
            if (!toast_row(row, chip))
            {
                continue;
            }
            if (toast_holds(row.id))
            {
                live += 1u;
            }
            else if (fresh == NO_INDEX)
            {
                fresh = row.id;
            }
        }
        if (live == 0u)
        {
            end_toast(getStringHash("empty"));
            return;
        }
        if (fresh != NO_INDEX && fresh != private[0].notify.toast_said_held)
        {
            private[0].notify.toast_said_held = fresh;
            witness1(WITNESS_INFO, getStringHash("toast: held (new row while up) row={0}"),
                     fresh);
        }
        return;
    }
    if (private[0].popup_state != POPUP_IDLE)
    {
        // A menu or a preview took the popup (and with a menu, the level).
        private[0].notify.toast_pending = 0u;
        return;
    }

    // The hold lifts when its chip stops asking, when something newer is
    // first, or when none of the ended toast's own rows is still asking:
    // the sidecar has heard `ACTION_TOAST_SHOWN` and dropped them, so what
    // still asks is news the toast never showed — a row that arrived behind
    // it — and it toasts now, alone. Until that frame the toast's rows still
    // ask, and the hold is what keeps them from reopening with it. The ended
    // toast's ids are still `toast_ids`: only the next toast's open rewrites
    // them.
    uint held = private[0].notify.toast_hold_slot;
    if (held != NO_INDEX)
    {
        uint chip = chip_row(held, private[0].notify.toast_hold_chip);
        bool asking = chip != NO_INDEX && (row_at(held, chip).flags & RFLAG_SHOW) != 0u;
        toast_rows(held, chip, newest);
        bool kept = false;
        if (asking)
        {
            uint n = rows_header(held).row_count;
            for (uint r = chip + 1u; r < n && !kept; ++r)
            {
                Row row = row_at(held, r);
                kept = toast_row(row, chip) && toast_holds(row.id);
            }
        }
        if (!asking || newest != private[0].notify.toast_hold_newest || !kept)
        {
            private[0].notify.toast_hold_slot = NO_INDEX;
        }
    }

    if (private[0].notify.toast_pending != 0u && step_toast_pending(rebuild))
    {
        return;
    }

    for (uint slot = 0u; slot < MAX_SLOTS; ++slot)
    {
        uint n = rows_header(slot).row_count;
        for (uint r = 0u; r < n; ++r)
        {
            // Two words a row, not twelve: this scan runs every frame over
            // every slot, and almost every row is not a chip that asks.
            uint at = 16u + r * 48u;
            if (rows_word(slot, at + 8u) != ROW_ITEM
                || (rows_word(slot, at + 12u) & RFLAG_SHOW) == 0u)
            {
                continue;
            }
            Row row = row_at(slot, r);
            if (private[0].notify.toast_hold_slot == slot && private[0].notify.toast_hold_chip == row.id)
            {
                continue;
            }
            if (toast_rows(slot, r, newest) == 0u)
            {
                continue; // nothing to show is not a toast
            }
            // The chip has to be on the bar to anchor anything: a frame that
            // has just arrived is built and laid out a frame from now, and the
            // bit is still there then.
            Id id = id_tagged(NOTIFY_ROW_TAG, slot, row.id);
            uint item = index_of_id(id);
            if (item == NO_INDEX || bar_element_of(item) == NO_INDEX)
            {
                continue;
            }
            // The level is the chip, so `bar_build` publishes its rows; the
            // toast opens once they are built (`step_toast_pending`) — this
            // frame, when they already are.
            set_menu_level(item);
            private[0].notify.toast_pending = 1u;
            private[0].notify.toast_slot = slot;
            private[0].notify.toast_chip = row.id;
            step_toast_pending(rebuild);
            return;
        }
    }
}

/// A toast's rows, placed. Returns the next free element.
///
/// Nothing here is measured: the rows, their order and their heights are the
/// ones `step_toast_pending` took when the toast opened, and the popup's want
/// is the size it opened at. Each row's rect is where it was when the toast
/// opened, whatever frames have arrived since — a new attention row is not
/// one of them, and a row the sidecar removed is a gap, not a reason for the
/// rows below to move up under the pointer. A row that no longer fits the
/// surface the compositor gave is not placed.
public uint layout_toast(uint next, bool open, float2 popup, float ppad)
{
    uint level = menu_level();
    uint slot = private[0].notify.toast_slot;
    float2 size = private[0].notify.toast_size;
    private[0].popup_want = size;
    private[0].menu_scroll = 0.0;
    private[0].menu_scroll_max = 0.0;
    private[0].menu_clip = float2(0.0, 0.0);

    float view_h = open ? popup.y : size.y;
    float inner_w = max((open ? popup.x : size.x) - 2.0 * ppad, 1.0);
    float bottom = view_h - ppad;
    place(ELEM_BACK, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0, NO_INDEX, NO_INDEX, false);
    float y = ppad;
    uint count = min(private[0].notify.toast_count, TOAST_ROWS_CAP);
    for (uint k = 0u; k < count && next < MAX_ELEMENTS; ++k)
    {
        float h = private[0].notify.toast_h[k];
        float at = y;
        y += h;
        uint i = index_of_id(id_tagged(NOTIFY_ROW_TAG, slot, private[0].notify.toast_ids[k]));
        if (i == NO_INDEX || private[0].items[i].parent != level || at + h > bottom + 0.5)
        {
            continue;
        }
        Item it = private[0].items[i];
        place(next, TARGET_POPUP, float4(ppad, floor(at), inner_w, h), MENU_RADIUS * scale_px(),
              it.block, i, open);
        witness_body(next, it, open, true);
        next += 1u;
    }
    place(ELEM_PREVIEW, TARGET_POPUP, float4(0.0, 0.0, 0.0, 0.0), 0.0,
          NO_INDEX, NO_INDEX, false);
    return next;
}

// ---------------------------------------------------------------------------
// What the passes ask of the toasts, through `bar_dispatch`
// ---------------------------------------------------------------------------

/// A click with `button` at `pos` on row `it` of the toast that is up.
///
/// A toast's row is a leaf whatever it carries: the click is the
/// `ACT_SIDECAR` the same row sends from the menu, and a row with a
/// submenu (a notification with actions) is activated rather than
/// descended into — a toast has no back row to come up by. The
/// click holds the toast as a refusal does: the
/// frames before the sidecar's answer still ask, and must not
/// reopen it with the rows the user just walked away from. The right
/// button is the sidecar's to interpret (`caustics-notify` reads it as
/// "dismiss without acting") and ends the toast the same way; the middle
/// does nothing.
public void notify_click_row(Item it, uint button, float2 pos)
{
    if (button == BTN_MIDDLE || (it.flags & (IFLAG_SEPARATOR | IFLAG_DISABLED)) != 0u)
    {
        return;
    }
    if (button == BTN_RIGHT)
    {
        if (id_tag(it.id) != NOTIFY_ROW_TAG)
        {
            return;
        }
        act_sidecar(it, ACTION_SECONDARY, button, pos, 0.0);
    }
    else
    {
        act_sidecar(it, ACTION_ACTIVATE, button, pos, 0.0);
    }
    hold_toast(private[0].popup_id);
    end_toast(getStringHash("click"));
}

/// The popup ended without this module asking (`CMD_POPUP_DONE`).
///
/// A toast this file did not end — the compositor dismissed it, or
/// refused it outright — is held until its news changes, or the next
/// frame would ask for the same one again.
public void notify_popup_done()
{
    if (private[0].popup_kind == POPUP_TOAST && private[0].popup_state != POPUP_ENDING)
    {
        say_toast_shown();
        hold_toast(private[0].popup_id);
        witness1(WITNESS_INFO, getStringHash("toast: closed ({0:s})"),
                 getStringHash("dismissed"));
    }
}
