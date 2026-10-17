// The bar, as a caustic — layout, hit-testing, policy and drawing: the seven
// entry points, each a short sequence of calls into the modules this file
// imports (caustics/README.md, *The files*).
//
// docs/architecture.md's argument for a GPU program that holds the policy,
// applied to a status bar:
// the thing that decides where an element *is* is the same thing that decides
// which texel belongs to it and what colour that texel ends up. Change
// ICON_LOGICAL in bar_tunables.slang and every dock entry grows, the carve grows
// with it, the hover glow traces the new boundary and the pointer starts
// hitting the new edge — with no second definition anywhere to keep in sync,
// and no rebuild (`caustics-reload`, the compiler sidecar, recompiles this file and
// its modules, and the bar swaps every pipeline together in well under a second).
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
// The caustic knows no element by name. It lays out an **item table**: the
// dock's entries and the windows under them, the tray's items, the battery,
// the clock, the rows of an open menu, each tagged with a wing. Everything
// in it lays *that* out: the left wing left-to-right from the edge padding,
// the right wing right-to-left, the centre wing centred on the whole bar, and
// the popup either as menu rows or as a captured still. Elements are allocated
// from ELEM_FIRST_FREE up, one per laid-out item, and `Element.item` is the
// map the CPU reads a hover back through.
//
// The other half is that the caustic *decides*. A click on a dock
// entry picks which window to focus here, from the item table, and pushes an
// `Action`; the CPU carries it out without re-deriving anything. The action
// ring is cleared at the top of every `bar_apply` and filled during it, so a
// frame's decisions appear in the action ring the embassy reads back for
// that frame exactly once — no acknowledgement command, no chance of replaying a click.
//
// State that must survive a dispatch and has no home elsewhere (the menu
// level; the hover-dwell clock) lives in `private` (`Private`, in
// `bar_state`), which is the caustic's alone. Never a `static` — a compute
// dispatch has no memory between runs, and a `static` would silently be
// re-zeroed.
//
// Nothing in here is per-applet, nor per anything: what the bar shows is a
// set of **subjects** (the charts, the toasts, the clock, the wake, the dock,
// the applets), one module each, and this file names none of them. Wherever
// a pass turns on an item's kind it calls `bar_dispatch`, the one module
// that knows which subjects exist.
// ---------------------------------------------------------------------------
//
// ---------------------------------------------------------------------------
// **Text and icons are the caustic's.** There is no glyph atlas: no CPU
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
// the one rule the caustic never bends — a straight-alpha write shows up as
// a bright halo around every glyph. The image slots hold straight alpha as the
// wire carries it and the sample premultiplies; the preview texture is
// premultiplied on upload. So every colour the caustic computes with is
// premultiplied, and there is no unpremultiply step anywhere in it.
//
// The embassy writes `inputs`, `params`, the command ring, the facts, the
// strings up to `compose_base`, the image slots, the preview, and the two
// arenas a sidecar fills (`samples`, `rows`). The caustic writes `state` (the
// action and witness rings, read back one frame late — the only readback),
// `private`, the run buffer, and the strings after `compose_base`
// (docs/abi.md, *Who writes what*).
//
// ---------------------------------------------------------------------------
// Destination format. Storage-image formats are baked into SPIR-V, so the
// caustic compiles once per swapchain format (the define reaches every
// module, `dst` in `bar_state` among them):
//
//   -DDST_FORMAT_RGBA8   rgba8unorm swapchain (an explicit SPIR-V format)
//   -DDST_FORMAT_BGRA8   bgra8unorm swapchain (SPIR-V has no BGRA image
//                        format; the image is declared format-unknown, which
//                        needs -default-image-format-unknown on the command
//                        line and the BGRA8UNORM_STORAGE feature at runtime)
//
// Six of the seven entries never touch `dst`, so a missing define is not a
// hard #error: the image falls back to format-unknown, which still needs
// -default-image-format-unknown. build.rs compiles every entry point once
// per variant.
//
// One toolchain hazard, if you are editing this file: never let a
// `[numthreads]` attribute detach from its entry point and land on a helper
// above it. slangc 2026.14.1 segfaults instead of diagnosing it
// (docs/slangc-numthreads.md).
// ---------------------------------------------------------------------------

// ===========================================================================
// The modules imported (caustics/README.md, *The import graph*).
//
//   bar_state      the bindings of sets 0 and 2, `Private`, the rings, the
//                  witness-name vocabulary, the scale and the item helpers.
//   bar_popups     the popup as a window: asked for, mapped, ended.
//   bar_layout     the element table: the strip, `place`, the hit test, a
//                  row's body wrapped, the springs.
//   bar_paint      the compositing rules and the shared drawing.
//   bar_table      the build's allocators, the stitch, the shape pass's block.
//   bar_dispatch   every subject, behind one function per place a pass turns
//                  on a kind.
//
// Under them all, `bar_abi` and `bar_wire` (the contracts, rendered from
// `bar/abi/`), `bar_model` (the caustic's vocabulary), `bar_tunables` (every
// knob, and the bar's whole config: docs/architecture.md, *Configuration is
// the caustic*), and the text engine's `shape` and `text`. `text` declares
// its four buffers in descriptor **set 1** and `shape` declares none at all,
// gathering its handles through a `ShapeTables` `bar_state` fills from set 2.
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
import bar_dispatch;

// ===========================================================================
// The witnesses' names: what an element is called in a `layout:`, a `hover:`
// and a `stall:` line, by its subject (`dispatch_name`).
// ===========================================================================

/// What to call the item an element lays out.
NameRef item_name(uint item)
{
    Item it = item_of(item);
    if (it.kind == ITEM_NONE)
    {
        return name_plain(NAME_ITEM);
    }
    return dispatch_name(it);
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
    BuildSizes z = build_sizes();

    // ---- the visible items, in this file's order (the dock's own order is the
    // apps table's: docs/abi.md, *The facts*) ----
    dispatch_build(b, z);

    // ---- the open menu's rows, then every window ----
    //
    // Both live past the visible items so that neither can move a visible
    // index, which is what keeps `Element.item` stable across window churn and
    // the hover springs from resetting.
    Id level = menu_level_id();
    bool bodies = dispatch_build_level(b, z, level);
    build_ellipsis(b, bodies, z.text[ITEM_MENU_ROW]);
    dispatch_build_last(b, z);

    // ---- what comes back, and the witnesses ----
    build_publish(b, level);
    dispatch_said_build(b.items);
    witness_build(b);
}

// ===========================================================================
// bar_compose — the composed strings (the clock's two), written on the GPU.
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
// One thread, walking the block table for BLOCK_COMPOSED blocks and asking
// the subject that composes each one (`dispatch_compose`). It runs at
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
        uint len = dispatch_compose(blk.hash, blk.a);
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
    shape_block(gid.x);
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
        dispatch_drag(c.target, c.pos);
        break;

    case CMD_POINTER_DOWN:
        press(c.target, c.pos);
        // Pressing anywhere on a slider's row seeks it and leaves the drag
        // live, so a click on the track is a seek and the drag that may
        // follow is already running.
        dispatch_drag(c.target, c.pos);
        if (c.target == TARGET_BAR)
        {
            dispatch_bar_press(c.pos);
        }
        break;

    case CMD_POINTER_UP:
    {
        uint hit = release(c.target, c.pos);
        if (hit != NO_INDEX)
        {
            // The button code rides on the release, so there is no need to
            // remember it from the press.
            click(hit, c.args.x, c.pos);
        }
        release_end();
        break;
    }

    case CMD_POINTER_LEAVE:
        pointer_leave(c.target);
        break;

    case CMD_POPUP_MAPPED:
        popup_mapped(c.args.xy);
        break;

    case CMD_POPUP_DONE:
        popup_done();
        dispatch_popup_done();
        popup_ended();
        break;

    case CMD_AXIS:
        scroll_menu(c.target, c.args.xy);
        break;

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

    // -- what a rewritten region and a new chart set are worth -----------------
    bool rebuild = rows_reshaped(rewritten);
    rebuild = dispatch_reshaped() || rebuild;

    // After the commands, before the layout: the commands may have set a level
    // and the layout must not be given one that no longer resolves.
    step_menu();
    dispatch_step_menu();
    // The bodies' lines, which the layout measures rows by. After the shape
    // pass, which ran before this one.
    wrap_bodies();
    // Every element of every target: the strip, then the popup that is up,
    // laid out by the subject it belongs to. Immediate mode: the elements
    // are recomputed every frame, and what persists is in `Private`.
    uint next = layout_strip();
    float2 popup;
    bool open = layout_popup(popup);
    layout_end(dispatch_layout_popup(next, open, popup, POPUP_PAD * scale_px()));
    // What the layout leaves to be said: the flags a subject derives from the
    // table, each element's `layout:` line, and every element the previous
    // frame used and this one does not, once.
    dispatch_placed();
    witness_placed();
    witness_departures();
    // After the layout, so a toast anchors at the chip's rect of this frame;
    // its own rows are laid out from the next. After the
    // wrap, so a toast whose rows were built this frame opens at their size;
    // and told whether a rows frame this frame is a rebuild, so it never opens
    // on rows the next build replaces.
    dispatch_step_popup(rebuild);

    // NaN or a stalled frame must not fling the springs or the dwell clock.
    float dt = inputs.dt;
    dt = dt > 0.0 ? min(dt, DT_MAX) : 0.0;

    // The clocks: the preview's dwell, the chart numbers' hover and the wake,
    // after the layout so the strip is this frame's; then the springs.
    resolve_hover_item();
    bool dwelling = dispatch_step(dt);
    bool moving = integrate_springs(dt);

    // -- the surface this file wants, and how long a shape pass may take ----
    push_anchor();
    push_watchdog();

    push_popup_move();

    witness_shaping();

    // Whether the level this frame holds is the one the table was built for.
    // The embassy answers it exactly as it answers a clock tick — mark the
    // shape submission pending, ask for a frame — and `bar_build` then records
    // the new level. It is the one rebuild the CPU cannot see coming, because
    // the menu is this file's. A subject whose rows move with no fact moving
    // is the other (`dispatch_ticked`).
    rebuild = dispatch_ticked() || rebuild;
    if (rebuild || !menu_level_id().eq(private[0].built_for))
    {
        act(ACT_REBUILD);
    }

    // A pointer *leave* clears the hover here rather than in `bar_sample`,
    // which does not run for a target the pointer is not on.
    witness_hover();

    uint redraw = (moving || dwelling) ? 1u : 0u;
    if (redraw != private[0].said_redraw)
    {
        private[0].said_redraw = redraw;
        witness0(WITNESS_INFO, redraw != 0u ? getStringHash("redraw: animating")
                                            : getStringHash("redraw: settled"));
    }

    // The embassy's wall clock says the bar has been animating too long and has
    // asked what is holding it awake (docs/abi.md, *The witness ring*).
    if (dump_stall)
    {
        witness_stall();
    }

    // The one word the embassy acts on without a message (docs/abi.md, *The one word*), and the
    // stamp that says this file has run at all.
    state[0].surface.wants_redraw = redraw;
    state[0].surface.frame = inputs.frame;
}

/// The answer to a `CMD_DUMP_STALL`: what is holding the bar awake.
///
/// The embassy's wall clock says the bar has been animating too long and has
/// asked what is holding it awake (docs/abi.md, *The witness ring*). The *trigger* is its, because the
/// pass that would notice from `dt` is the one that stops running when
/// frames do; the answer is this file's, because the numbers are: every
/// element whose springs have not settled, the popup's dwell, and each
/// subject's own.
void witness_stall()
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
    witness_popup_stall();
    dispatch_stall();
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
        return dispatch_draw_preview(e, p, col);
    }

    Item it = item_of(e.item);
    // The flags a subject derived from the item table and left in the
    // element after the layout (a dock entry's PINNED/RUNNING/FOCUSED/URGENT,
    // `dock_flags`; zero for every other element, which `place` cleared),
    // folded in here so that everything below reads one `flags` word.
    it.flags |= e.derived;

    // A kind that is its own thing entirely (a chart, a line of the numbers,
    // a slider, a separator) draws none of the button dressing below.
    bool whole;
    col = dispatch_draw_own(it.kind, idx, e, it, p, col, whole);
    if (whole)
    {
        return col;
    }
    col = draw_button(idx, e, it, p, col);
    col = dispatch_draw_marks(it.kind, e, it, p, col);
    if (idx == ELEM_BACK)
    {
        col = draw_back(e, p, col);
    }
    return col;
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
        col = dispatch_draw_slab(p, col);
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
            Item it = item_of(e.item);
            col = dispatch_draw_content(it.kind, e, it, p, col);
        }
        else if (e.block < MAX_BLOCKS)
        {
            col = draw_title(e, p, col);
        }
    }

    dst.Store(texel, col);
}

// ===========================================================================
// The oracle's eighth entry point (docs/abi.md, *Bindings and the oracle*), compiled in only
// when the test asks for it.
//
// `#include` rather than `import`: `bar_check` walks `private` and names what
// it finds with this file's own `element_name`/`item_name` family, which no
// module exports. Production's `ENTRIES` does not list it
// and `bar/build.rs` never defines `CAUSTICS_BAR_CHECK`, so the baked SPIR-V is
// the seven entries of `ENTRIES`.
// ===========================================================================

#ifdef CAUSTICS_BAR_CHECK
#include "bar_check.slang"
#endif
