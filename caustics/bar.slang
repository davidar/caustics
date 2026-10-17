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
import clock;
import wake;
import dock;
import applets;

export static const uint SYSMON_CHART = ITEM_CHART;
export static const uint SYSMON_LINE = ITEM_CHART_LINE;
export static const uint SYSMON_CHART_TAG = ID_CHART;
export static const uint SYSMON_LINE_TAG = ID_NUMBERS;
export static const uint NOTIFY_ROW_TAG = ID_APPLET;
export static const uint CLOCK_ITEM = ITEM_CLOCK;
export static const uint CLOCK_CELL = ITEM_CAL_CELL;
export static const uint CLOCK_TAG = ID_CLOCK;
export static const uint CLOCK_CELL_TAG = ID_CALENDAR;
export static const uint DOCK_ENTRY = ITEM_DOCK;
export static const uint DOCK_WINDOW = ITEM_WINDOW;
export static const uint DOCK_OVERVIEW = ITEM_OVERVIEW;
export static const uint DOCK_ROW = ITEM_MENU_ROW;
export static const uint DOCK_ENTRY_TAG = ID_ENTRY;
export static const uint DOCK_WINDOW_TAG = ID_WINDOW;
export static const uint DOCK_OVERVIEW_TAG = ID_OVERVIEW;
export static const uint DOCK_ROW_TAG = ID_DOCK_ROW;
export static const uint APPLETS_CHIP = ITEM_APPLET;
export static const uint APPLETS_SLIDER = ITEM_SLIDER;
export static const uint APPLETS_ROW = ITEM_MENU_ROW;
export static const uint APPLETS_TAG = ID_APPLET;
export static const uint APPLETS_CAFFEINE_TAG = ID_CAFFEINE;

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
    case ID_OVERVIEW: return dock_overview_name(it);
    case ID_CLOCK:    return clock_name(it);
    case ID_CAFFEINE: return applets_caffeine_name(it);
    case ID_ENTRY:    return dock_entry_name(it);
    case ID_WINDOW:   return dock_window_name(it);
    case ID_DOCK_ROW: return dock_row_name(it);
    case ID_CHART:    return sysmon_chart_name(it);
    case ID_APPLET:   return applets_name(it);
    case ID_CALENDAR: return calendar_cell_name(it);
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
    clock_step_menu();
}

// ---------------------------------------------------------------------------
// Layout. The wings over the item table, then whichever popup is up.
// ---------------------------------------------------------------------------

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
    else if (kind == POPUP_MENU && item_of(menu_level()).kind == ITEM_CLOCK)
    {
        // The clock's menu is its calendar: a grid, not rows.
        next = layout_calendar(next, open, popup, ppad);
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

    // ---- the visible items, in this file's order (the dock's own order is the
    // apps table's: docs/abi.md, *The facts*) ----
    dock_build(b, icon_px_u, text_px[ITEM_DOCK]);

    clock_build(b, text_px[ITEM_CLOCK]);

    applets_build(b, icon_px_u, text_px[ITEM_APPLET]);

    // The charts last of the visible items: their count follows the sidecar's
    // list, and a sidecar restarted with a different one must not move the
    // power button or a tray item out from under a click.
    sysmon_build_charts(b);

    // ---- the open menu's rows, then every window ----
    //
    // Both live past the visible items so that neither can move a visible
    // index, which is what keeps `Element.item` stable across window churn and
    // the hover springs from resetting.
    Id level = menu_level_id();
    uint tag = id_tag(level);
    bool bodies = false;
    if (tag == ID_APPLET)
    {
        bodies = applets_build_menu(b, level, icon_px_u, text_px[ITEM_MENU_ROW],
                                    text_px[ITEM_SLIDER]);
    }
    else if (tag == ID_ENTRY)
    {
        dock_build_menu(b, level, text_px[ITEM_MENU_ROW]);
    }
    else if (tag == ID_CLOCK)
    {
        clock_build_calendar(b, text_px[ITEM_CAL_CELL]);
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

    dock_build_windows(b, text_px[ITEM_WINDOW]);

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
    dock_said_build(b.items);
    applets_said_build(b.items);
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
            len = clock_compose_time(blk.a);
            break;
        case COMPOSE_CLOCK_DATE:
            len = clock_compose_date(blk.a);
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

    resolve_hover_item();
    bool dwelling = step_dwell(dt, numbers_level());
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
        wake_stall();
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
    // element by `place_dock_flags` — the CPU writes none of the four (`dock_flags`).
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
        return draw_separator(e, p, col);
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
