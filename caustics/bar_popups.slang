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
    private[0].popup_pending_secs = 0.0;
    private[0].popup_switch = 0u;
}

/// Is a popup decided, asked for or up?
public bool popup_live()
{
    uint st = private[0].popup_state;
    return st == POPUP_PENDING || st == POPUP_ASKED || st == POPUP_UP;
}

/// Is a grabbed menu's surface mapped: a menu up, or one already being
/// switched to on the surface of the menu that was. What a next menu can be
/// had by moving.
public bool menu_surface_up()
{
    uint st = private[0].popup_state;
    return private[0].popup_kind == POPUP_MENU
           && (st == POPUP_UP
               || ((st == POPUP_PENDING || st == POPUP_ASKED) && private[0].popup_switch != 0u));
}

/// A popup that is live is ending, or being replaced by one of kind `next`
/// (0 when it is closing): its subject's last word, said before the popup's
/// state moves on. A link-time hook, as the subjects' kinds are: this module
/// names no subject, and `bar_dispatch` binds it to the one the popup is.
extern void dispatch_popup_ending(uint next);

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
///
/// **A menu is not asked for here.** The click that decides one is the frame
/// that sets its level, and the table holds no row of that level until the
/// build the level change asks for has run: a size measured now is the last
/// popup's, and a menu asked for at it maps at it and is drawn there until a
/// move lands (docs/architecture.md, *Menus, toasts and popups*). So a menu is
/// `POPUP_PENDING` until `step_popup_ask` sees a table built for its level,
/// and goes out at the size the layout measured from that table. `want` is
/// what a preview or a toast opens at; a menu's is the layout's.
///
/// **A menu in place of a menu is a switch, not a replace.** While a grabbed
/// menu's surface is mapped (up, or already being switched), the next menu
/// moves that surface to its own anchor and size (`ACT_POPUP_SWITCH`), which
/// keeps the grab: a click on another chip, and the pointer moving onto one
/// (`step_menu_hover`), which has no button serial a new grab could be taken
/// with.
public void request_popup(uint kind, float4 anchor, Id id, float2 want)
{
    bool surface = menu_surface_up();
    // The replace path below tears the old surface down, since the bar has
    // one popup: its subject has its last word first (a toast is the one a
    // grabbed menu ends without asking).
    if (popup_live())
    {
        dispatch_popup_ending(kind);
    }
    uint slot = id_tag(id) == ID_APPLET ? id_aux(id) : NO_INDEX;
    uint row = id_tag(id) == ID_APPLET ? id_payload(id) : 0u;
    private[0].popup_switch = kind == POPUP_MENU && surface ? 1u : 0u;

    private[0].popup_kind = kind;
    private[0].popup_id = id;
    private[0].popup_slot = slot;
    private[0].popup_row = row;
    private[0].popup_anchor = anchor;
    private[0].popup_want = want;
    private[0].popup_pending_secs = 0.0;
    if (kind == POPUP_MENU)
    {
        private[0].popup_state = POPUP_PENDING;
        return;
    }
    ask_popup();
}

/// The one `ACT_POPUP_OPEN` for the popup this file holds, at its want, or
/// the `ACT_POPUP_SWITCH` that moves the menu up to it (same operands).
void ask_popup()
{
    float2 want = private[0].popup_want;
    uint w = uint(max(want.x, 1.0) + 0.5);
    uint h = uint(max(want.y, 1.0) + 0.5);
    private[0].popup_state = POPUP_ASKED;
    private[0].popup_sent = float2(float(w), float(h));

    Args a = action_begin(private[0].popup_switch != 0u ? ACT_POPUP_SWITCH : ACT_POPUP_OPEN);
    arg(a, private[0].popup_kind);
    arg(a, private[0].popup_slot);
    arg(a, private[0].popup_row);
    arg4(a, private[0].popup_anchor);
    arg(a, w);
    arg(a, h);
}

/// A pending menu is asked for once the table was built for its level and
/// nothing arriving this frame will build it again (`rebuild`), after the
/// layout, so `popup_want` is what that table's rows measure. A level whose
/// rows never come is a level built with none: it opens at the least a menu
/// is, and grows when they land. `MENU_OPEN_WAIT_S` bounds the wait for a
/// build that keeps being overtaken.
public void step_popup_ask(bool rebuild, float dt)
{
    if (private[0].popup_state != POPUP_PENDING)
    {
        return;
    }
    private[0].popup_pending_secs += dt;
    bool built = !rebuild && private[0].built_for.eq(menu_level_id());
    bool late = private[0].popup_pending_secs >= MENU_OPEN_WAIT_S;
    if (!built && !late)
    {
        return;
    }
    if (!built)
    {
        Args a = witness_begin(WITNESS_INFO,
                               getStringHash("popup: a menu asked for unbuilt after {0:f} s"));
        argf(a, private[0].popup_pending_secs);
    }
    ask_popup();
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
    dispatch_popup_ending(0u);
    private[0].popup_state = POPUP_ENDING;
    act(ACT_POPUP_CLOSE);
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

/// The popup's kind while it is asked for or up, else 0: what a click on
/// one of its rows is about.
public uint popup_live_kind()
{
    return popup_live() ? private[0].popup_kind : 0u;
}

/// The popup's size, compared here (docs/abi.md, *The action ring*).
///
/// The layout re-measures the popup every frame; when the want differs from
/// what the compositor gave, one `ACT_POPUP_MOVE`. The comparison is
/// made here, on the side that knows what the popup wants to be, and in
/// the logical px a positioner is given (the embassy rounds a want to them):
/// a want of an odd number of px at scale 2 comes back one px wider, and is
/// what was asked for, not a size to move to.
public void push_popup_move()
{
    if (private[0].popup_state == POPUP_UP)
    {
        uint want_w = uint(max(private[0].popup_want.x, 1.0) + 0.5);
        uint want_h = uint(max(private[0].popup_want.y, 1.0) + 0.5);
        float s = float(max(inputs.scale, 1u));
        bool differs = uint(float(want_w) / s + 0.5) != uint(float(inputs.popup_size.x) / s + 0.5)
                       || uint(float(want_h) / s + 0.5) != uint(float(inputs.popup_size.y) / s + 0.5);
        bool asked = want_w == uint(private[0].popup_sent.x + 0.5)
                     && want_h == uint(private[0].popup_sent.y + 0.5);
        if (differs && !asked && want_w > 1u && want_h > 1u)
        {
            private[0].popup_sent = float2(float(want_w), float(want_h));
            act2(ACT_POPUP_MOVE, want_w, want_h);
        }
    }
}

/// The popup's line of a `CMD_DUMP_STALL` answer: its state and the dwell.
public void witness_popup_stall()
{
    Args dwell = witness_begin(
        WITNESS_WARN,
        getStringHash("stall: popup dwell(state,secs,candidate,owner)={0:f},{1:f},{2:f},{3:f}"));
    argf(dwell, float(private[0].popup_state));
    argf(dwell, private[0].dwell_secs);
    argf(dwell, private[0].dwell_candidate);
    argf(dwell, private[0].preview_owner);
}

/// The compositor mapped the popup, `size` px (`CMD_POPUP_MAPPED`).
///
/// The size that came back is `inputs.popup_size`; this only says the
/// surface exists. A `CMD_POPUP_DONE` instead means the compositor
/// refused, which `popup_done` answers the same way it answers a
/// dismissal.
public void popup_mapped(uint2 size)
{
    // Only the popup asked for comes up, and one up stays up: a configure
    // that lands while a popup ends, or under a menu still pending, is not
    // the popup this file holds.
    uint st = private[0].popup_state;
    if (st != POPUP_ASKED && st != POPUP_UP)
    {
        return;
    }
    private[0].popup_state = POPUP_UP;
    private[0].popup_switch = 0u;
    private[0].popup_sent = float2(float(size.x), float(size.y));
}

/// The popup is gone (`CMD_POPUP_DONE`), the first half: a preview popup
/// that goes away without this file asking — the
/// compositor dismissed it, or the embassy gave up — still owes the
/// capture feeder a teardown. A leaked session is a screencast light
/// that never goes out.
public void popup_done()
{
    if (private[0].popup_kind == POPUP_PREVIEW && private[0].preview_owner >= 0.0)
    {
        act(ACT_PREVIEW_END);
    }
}

/// The popup is gone, the last half, after each subject has had its say:
/// idle, and everything it was forgotten.
public void popup_ended()
{
    private[0].popup_state = POPUP_IDLE;
    private[0].popup_kind = 0u;
    private[0].popup_id = id_zero();
    private[0].pressed_element = NO_INDEX;
    reset_popup_scratch();
}

/// The wheel over an open menu scrolls it (`CMD_AXIS`). The
/// embassy sends px when the compositor gave px and steps when it gave
/// only steps; a step is one row, which is this file's number.
public void scroll_menu(uint target, uint2 args)
{
    if (target != TARGET_POPUP || private[0].popup_kind != POPUP_MENU)
    {
        return;
    }
    float delta = float(asint(args.x));
    if (delta == 0.0)
    {
        delta = float(asint(args.y)) * MENU_ROW_H * scale_px();
    }
    // Clamped against what the last layout measured, so a burst of ticks
    // past the end followed by one back moves back from the end. A level
    // that fits has no room and ignores the wheel.
    private[0].menu_scroll = clamp(private[0].menu_scroll + delta, 0.0,
                                   private[0].menu_scroll_max);
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
public void step_menu()
{
    if (popup_live() && private[0].popup_kind == POPUP_MENU && menu_level() == NO_INDEX)
    {
        close_popup();
    }
}
