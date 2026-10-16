// The bar's tunables — every knob, in one module, and the bar's only config.
//
// There is no config file beside this one and there will not be one: the bar
// is a shader, so its configuration is a shader module (docs/architecture.md,
// *Configuration is the caustic*). What the bar looks like, how tall it is,
// which edge it takes, what goes in which wing — all of it is a
// `public static const` below, and changing one and saving the file moves the
// running bar within a second or so: `caustics-reload` (`reload/`) sees the save,
// compiles the caustic with slangc and hands the bar the new SPIR-V.
//
// **Editing it.** This file, in the checkout the bar was built from, is the
// one to edit: save it and the running bar moves. A different bar is a fork of
// the code, and an update is a merge into your edits like any other.
//
// The posture is the shader's, because this *is* a shader: a file that does
// not compile keeps the last good bar and prints slangc's diagnostic verbatim
// to the journal. A bad file is never a dead bar.
//
// Lengths are **logical** px unless a name says otherwise; the layout
// multiplies by `inputs.scale` exactly once, so a scale-2 output gets a
// scale-2 bar rather than an upscaled scale-1 one.
//
// Two of these reach the embassy at all, and both as a **message**: `EDGE` and
// `BAR_HEIGHT` become one `ACT_ANCHOR` on the caustic's first frame and again
// whenever a reload changes either, and the embassy re-anchors the layer
// surface to match (docs/abi.md, *The action ring*). The surface is a Wayland object, so
// the embassy has to own it; what size and edge it should be is a decision,
// so the caustic makes it. Everything else here — ICON_LOGICAL, TEXT_PX,
// DOCK_MENU, WEEKDAYS, MONTHS, CLOCK_DATE_ORDER and the rest — is read by the
// caustic and by nothing else: the sizes are snapped and drawn at in
// `bar_build`, the menu rows are published there, and the clock's words are
// composed on the GPU.

module bar_tunables;

// The ITEM_* kinds the wing orders are written in, the WHEN_* a DOCK_MENU row
// asks about and ITEM_KIND_COUNT, which TEXT_PX is as long as — all of it the
// **caustic's own** vocabulary, so the import is `bar_model` and not `bar_abi`
// (docs/abi.md, *The private buffer*). `bar_model` re-exports `bar_abi`, which re-exports
// `bar_wire`, so EDGE_*, the ACT_* verbs and APPLET_* come with it: one
// import reaches every constant this file names.
//
// None of the three declares a binding or an entry point, so importing them
// here costs nothing and keeps one spelling of every constant.
import bar_model;

// -- the surface the embassy is asked for -----------------------------------

/// Which edge of the output the bar takes: EDGE_TOP or EDGE_BOTTOM. Change it
/// and save, and the bar moves to the other edge: the caustic sends one
/// `ACT_ANCHOR`, the embassy re-anchors the layer surface (and moves its
/// one-bar-per-edge lock), and the marks that face the desktop (the focus
/// underline, the running marks) turn to face it from the new side.
public static const uint EDGE = EDGE_TOP;

/// How many milliseconds one `bar_shape` dispatch may take before the embassy
/// calls it a runaway. Published as an `ACT_WATCHDOG`; the embassy wraps the
/// dispatch in a timestamp query pair and, on a pass over this, writes one
/// witness and **stops shaping** until the OpenType arena's front block is
/// written again. What you would see is text that stops changing (new labels
/// stay unshaped) rather than a bar that stutters.
///
/// The shaper already holds a wall-clock deadline inside the kernel and an
/// operation budget per run, so every *walk* is short. This is the other
/// failure: walks that are each short and far too many, which is what a
/// malformed arena produces — about 150 ms a dispatch, every frame, and a
/// stuttering cursor.
///
/// The number is a ceiling on a *healthy* pass on a real GPU and not a frame
/// budget: RADV shapes the whole bar in about a millisecond, and the
/// per-run budget bounds a pathological arena at about 3 ms. A software
/// adapter such as lavapipe gets this times `WATCHDOG_CPU_SCALE` (8,
/// `bar/src/gpu.rs`): 512 ms, over three times the worst steady pass seen
/// there. The first pass after the pipelines are created or swapped is timed
/// but not judged: it times the driver building the shader, not the arena,
/// so it is not held to the ceiling.
public static const uint SHAPE_WATCHDOG_MS = 64u;

// -- the wings --------------------------------------------------------------
//
// Placement is layout, and layout is this file's. Each wing is laid out by
// walking its array of kinds and, per kind, the item table in order — so the
// arrays say both *what* is on the bar and *in which order*. A kind in no
// array is not laid out on the bar at all (ITEM_WINDOW and ITEM_MENU_ROW never
// are: the popup loop keys on ITEM_MENU_ROW and windows are looked up by
// parent).
//
// The right wing is laid out from the outer edge inwards, so its first entry
// is the outermost item on screen.
//
// ITEM_APPLET is one kind for every sidecar's bar item (docs/abi.md, *Rows*),
// so the wing arrays name it once and a new applet needs no edit to them.
// Which applet goes where among the others is a fourth array, below: placement
// is layout, and layout is this file's, right down to this.
//
// The right wing is *only* ITEM_APPLET: the tray icons, the battery and the
// power button are rows of `caustics-tray`'s and `caustics-power`'s frames like any
// other applet's, so their order is APPLET_ORDER's.
//
// Moving a kind from one array to another moves it on screen at the next
// reload; the default puts the overview button and the dock on the left, the
// clock and the chart strip in the middle, and the applets on the right.

public static const uint LEFT_ORDER[] = { ITEM_OVERVIEW, ITEM_DOCK };
public static const uint CENTER_ORDER[] = { ITEM_CLOCK, ITEM_CHART };
public static const uint RIGHT_ORDER[] = { ITEM_APPLET };

/// Where the applets go among themselves, wherever ITEM_APPLET is placed
/// above. Walked in this order, and per entry the item table in publication
/// order, exactly as a wing array is — so this reads the same way and moves
/// the volume applet outside the network one by swapping two words.
///
/// An applet kind this array does not name — a third-party sidecar's own
/// value — is placed in APPLET_OTHER's pass, so nothing a sidecar sends can
/// make its item quietly vanish off the bar. Drop APPLET_OTHER from the array
/// and those applets are not laid out at all, which is the honest way to say
/// "only the ones I named".
///
/// The right wing is laid out from the outer edge inwards, so APPLET_POWER
/// first puts the power button — and the battery beside it, in `caustics-power`'s
/// own publication order — outermost, where most panels (cosmic-panel among
/// them) keep them. APPLET_NOTIFY's bell sits just inside the power button
/// and the battery, APPLET_CAFFEINE inside the bell, then audio, network and
/// Bluetooth, and APPLET_TRAY last puts the tray's icons innermost.
///
/// An order written before a new applet value existed (a fork's, kept through
/// a merge) still compiles and still shows that applet's chip: `item_in_pass` places an applet value
/// no pass names in APPLET_OTHER's pass (`bar.slang`), which is the inner end
/// of the wing. Adding the word here is how it moves to where it belongs.
public static const uint APPLET_ORDER[] = {
    APPLET_POWER, APPLET_NOTIFY, APPLET_CAFFEINE, APPLET_AUDIO, APPLET_NETWORK, APPLET_BLUETOOTH,
    APPLET_TRAY, APPLET_OTHER
};

/// Whether the clock draws its date block (`Item.aux_block`, `%a %-d %b`) to
/// the left of the time. False leaves a bare `HH:MM`, and the clock's width
/// follows either way.
public static const bool CLOCK_SHOW_DATE = true;

/// Whether a click on the clock opens this month's calendar: a menu-kind popup
/// at the clock with the month's title, the weekday initials and the days,
/// today marked. False makes the clock a plain label — no click, no hover
/// chip.
public static const bool CLOCK_SHOW_CALENDAR = true;

/// Whether the calendar's weeks start on Monday (ISO 8601, and what most of
/// the world prints) or, false, on Sunday (WEEKDAYS' own order).
public static const bool CAL_WEEK_STARTS_MONDAY = true;

/// One calendar cell's side and the gap between two cells, logical px. A
/// cell holds a day number at TEXT_PX[ITEM_CAL_CELL], and today's wears the
/// attention wash a menu row does (URGENT_TINT), rounded at MENU_RADIUS. The
/// grid is 7 cells and 6 gaps wide and as many rows tall as the month needs —
/// four (a 28-day February starting on the week's first day) to six — plus the
/// weekday row and a MENU_ROW_H title above it.
public static const float CAL_CELL_PX = 28.0;
public static const float CAL_GAP = 2.0;

// -- what the bar's own items wear ------------------------------------------
//
// Three lists of icon names and one bool. A name is a `getStringHash`
// literal: slangc records the text behind every such call in its reflection,
// the embassy puts the bytes in the strings buffer and indexes them by hash,
// and `bar_build` reads the bytes back out of the pool to ask about. So a name
// is decided here, in one place, and the embassy never learns what it means —
// it copies the bytes out and writes a `Request` (docs/abi.md, *The facts*, *The action ring*).
// The *answer* is a `Glyph` body in the answers arena, joined to the name by
// the caustic's own hash of the same bytes, and the embassy hashes nothing at
// all. A list is tried in order and the first name the installed icon themes
// have wins, so putting a theme's own name first changes the icon you see.

/// What the overview button asks for, in order; the first that resolves wins.
public static const uint OVERVIEW_ICONS[] = {
    getStringHash("view-app-grid-symbolic"),
    getStringHash("view-grid-symbolic"),
    getStringHash("view-paged-symbolic"),
    getStringHash("focus-windows-symbolic"),
    getStringHash("preferences-system-windows-symbolic"),
};

/// What it wears when no installed theme has any of those: one character,
/// drawn at the icon's own size so the button is still the width of one.
public static const uint OVERVIEW_GLYPH = getStringHash("☷");

/// The caffeine chip's two states. Adwaita, AdwaitaLegacy, Cosmic, Pop and
/// hicolor have no caffeine, coffee or inhibit icon (a theme such as Papirus
/// may; put its name first), so this pair is a borrowed metaphor: the sun
/// for "the screen stays awake", the moon for "it may sleep". If neither name
/// of a state resolves, **no chip is published** — an empty square is worse
/// than no chip.
public static const uint CAFFEINE_ON_ICONS[] = {
    getStringHash("weather-clear-symbolic"),
    getStringHash("preferences-desktop-screensaver-symbolic"),
};
public static const uint CAFFEINE_OFF_ICONS[] = {
    getStringHash("weather-clear-night-symbolic"),
    getStringHash("user-idle-symbolic"),
};

/// Whether an app with no window and no favourite index keeps its dock entry.
/// False is the dock every panel has; true is a dock that only ever grows.
public static const bool DOCK_KEEPS_UNPINNED_WITHOUT_WINDOWS = false;

// -- the ink ----------------------------------------------------------------
//
// The three colours `bar_build` gives the blocks it publishes: what a label is
// *coloured* is the caustic's, like what it says.

/// A label on the bar or in a menu: the clock, a menu row's text, a chip's
/// label. Premultiplied linear RGBA, like everything the caustic composites.
public static const float4 LABEL_INK = float4(0.92, 0.94, 0.98, 1.0);

/// A symbolic icon — curves, with no colour of its own. GTK themes draw
/// symbolic icons near-black and leave the recolouring to the toolkit; drawn
/// as written they vanish on a dark bar, so the caustic paints their coverage
/// in this colour instead.
public static const float4 ICON_INK = float4(0.88, 0.92, 0.97, 1.0);

/// An image slot's pixels (a full-colour icon that arrived as pixels rather
/// than curves), multiplied by this: white leaves them as they are.
public static const float4 IMAGE_INK = float4(1.0, 1.0, 1.0, 1.0);

// -- the dock's right-click menu --------------------------------------------

/// The rows of a dock entry's right-click menu: `(label, verb, when)`, in the
/// order they are shown.
///
/// The **label** is a `getStringHash` literal. slangc records the text behind
/// every such call in its reflection, so the bar finds the bytes by hash and
/// never holds a list of strings of its own. Reword a row here, save, and the
/// running bar's menu says the new thing within a second: the reload compiles
/// this file and the table and the literals arrive together.
///
/// The **verb** is an `ACT_*` from `bar_abi`: the action the caustic pushes
/// when the row is clicked, which the embassy forwards without deciding
/// anything (docs/abi.md, *The action ring*). `ACT_NONE` silences a row without
/// shortening the table.
///
/// The **when** is a WHEN_*: whether this entry has a window, and whether it is
/// a favourite. A row whose `when` the entry does not satisfy is not published
/// at all.
///
/// The table is exactly `DOCK_MENU_ROWS` long because `bar_build` walks it as
/// an array; an edit may reword, reorder or silence a row. The
/// fourth word of each row is spare.
public static const uint4 DOCK_MENU[DOCK_MENU_ROWS] = {
    uint4(getStringHash("New window"), ACT_SPAWN,        WHEN_ALWAYS,   0u),
    uint4(getStringHash("Close"),      ACT_CLOSE_WINDOW, WHEN_RUNNING,  0u),
    uint4(getStringHash("Pin"),        ACT_PIN,          WHEN_UNPINNED, 0u),
    uint4(getStringHash("Unpin"),      ACT_UNPIN,        WHEN_PINNED,   0u),
};

// -- the slab ---------------------------------------------------------------

/// Bar height in logical px, and the band the bar's contents are centred in.
/// Sent with EDGE in the `ACT_ANCHOR`: the embassy asks the compositor for a
/// layer surface this tall and takes an exclusive zone to match (windows
/// move to make room), and re-asks when this number changes. The *configure*
/// is still the authority on how
/// tall the surface actually came out, so the layout centres its contents in
/// `min(surface height, BAR_HEIGHT)` rather than in the surface. A compositor
/// that hands back a taller surface gets the bar's chrome in its top band and
/// empty translucency below, which is a legible failure instead of a wrong one.
public static const float BAR_HEIGHT = 32.0;

/// The bar's ground. Near-black blue-slate, so the bar recedes behind what it draws rather than reading as a strip pasted over the desktop.
public static const float3 SLAB_RGB = float3(0.04, 0.05, 0.08);

/// The bar's translucency: the ground's alpha, 0 (clear) to 1 (opaque). This
/// alpha *is* the bar: glyphs and chips composite on top of it at full
/// coverage, so lowering it makes the desktop show through the ground without
/// touching the legibility of anything drawn on it. The charts' ground
/// (CHART_BG_ALPHA) follows it.
public static const float SLAB_OPACITY = 0.5;

/// Corner radius of the bar slab, logical px. 0 for an edge-to-edge bar;
/// raise it and the carve, the paint and the pointer all round together.
public static const float BAR_RADIUS = 0.0;

/// **The hot-reload smoke test.** A multiplier on every colour this file
/// writes — the slab's ground *and* the ink of every glyph and icon. 1 = the
/// colours above. `bar/bar-test.sh` seds this line while the bar is running —
/// e.g. to `float3(1.0, 0.30, 0.45)` — and the next frame proves the pipeline
/// swap without touching a single byte of state. Keep the line's shape
/// (`public static const float3 RELOAD_SMOKE_TINT = …;` on one line) so the
/// sed still finds it.
///
/// Tinting only the slab would move a screenshot of a near-black ground by a
/// couple of levels: technically a change, invisible to a human and hard to
/// probe. Tinting the ink is what makes it loud — the clock is the biggest
/// bright run of texels the bar owns, so it is the clock that changes colour
/// and the clock the gate's texel probe reads it off. Leave it at white.
public static const float3 RELOAD_SMOKE_TINT = float3(1.0, 1.0, 1.0);

// -- wings ------------------------------------------------------------------

/// Padding from the bar's short edges to the first element of a wing.
public static const float EDGE_PADDING = 8.0;

/// Space between two items of the same wing, and between an item's icon and
/// its label.
public static const float ITEM_GAP = 6.0;

/// The nominal icon side: what a dock entry, a tray item and the overview
/// button are sized around, and the floor on a button's width. Raise it and
/// every icon and every button grows with it; themed icons are curves, so
/// they stay sharp at any size and nothing is re-rasterised.
public static const float ICON_LOGICAL = 20.0;

/// Padding inside an item's chip, around its content.
public static const float CHIP_PAD = 5.0;

// -- text -------------------------------------------------------------------
//
// A text size is an **em size**, not a raster size: the glyphs are curves and
// the coverage walk multiplies by this, so a fractional scale and a 2x output
// read the same buffers and nothing is rasterised at either (docs/architecture.md,
// *Text*). The caustic converts each size to *device* px and snaps it, and
// that is what the shape pass is given.
//
// The curve renderer (Slug) is unhinted, and its paper's remedy is to pick a
// size at which the font's cap height lands on a whole pixel, which keeps the
// tops of capitals and the x-height crisp. The caustic does that itself —
// `snap_px` in `bar.slang`, against `params.cap_height`, moving a size by at
// most one pixel — so these stay the plain "how big is the text" knob and the
// awkward arithmetic is nobody's business but the font's. Adwaita Sans's cap
// height is 0.7275 em, so its whole-pixel sizes are multiples of 11 device
// px; Noto Sans's is 0.714 em, so multiples of 7.

/// How big each kind's text is, logical px, indexed by its ITEM_*.
///
/// One table rather than a knob or two, for the reason every other table here
/// exists: which text is big and which is small is layout, and layout is this
/// file's. `bar_build` snaps each entry for the loaded font and draws the
/// block at it; nothing on the embassy's side reads a text size at all.
///
/// The split below: the chrome that says a number or a state is 16 — the
/// clock, the overview, every applet chip (caffeine among them, which is an
/// ITEM_APPLET) — and everything that is *prose* is 14: a dock entry's name,
/// a window's title, a menu row's label, a slider's label. Raise an entry and
/// that kind's text grows; a chip's width follows its text, while a menu
/// row's height is MENU_ROW_H's.
///
/// It is exactly ITEM_KIND_COUNT long and indexed by kind; ITEM_NONE's slot is
/// never read.
public static const float TEXT_PX[ITEM_KIND_COUNT] = {
    14.0,  // ITEM_NONE      (never read)
    14.0,  // ITEM_DOCK      an entry's name, in the preview popup
    14.0,  // ITEM_WINDOW    a title
    16.0,  // ITEM_CLOCK     the time and the date
    16.0,  // ITEM_OVERVIEW
    14.0,  // ITEM_MENU_ROW  a label
    14.0,  // ITEM_CHART
    16.0,  // ITEM_APPLET    a chip's label
    14.0,  // ITEM_SLIDER    a row's label
    14.0,  // ITEM_CAL_CELL  the calendar's title, weekday initials and days
    14.0,  // ITEM_CHART_LINE  a chart's name and its number, in the chart numbers
};

// -- the clock's words ------------------------------------------------------
//
// The embassy publishes two *numbers* — `inputs.clock_time` and
// `inputs.clock_date` — and `bar_compose` writes the strings from the three
// tables below, on the GPU. So the date's wording and its order are tunables, and an
// edit of this file may say `Fre 5 Sep`, or `5 September`, or nothing at all,
// without a line of Rust knowing what a weekday is.

/// The weekday names, Sunday first — `inputs.clock_date`'s weekday field
/// indexes this directly.
public static const uint WEEKDAYS[7] = {
    getStringHash("Sun"), getStringHash("Mon"), getStringHash("Tue"),
    getStringHash("Wed"), getStringHash("Thu"), getStringHash("Fri"),
    getStringHash("Sat"),
};

/// The month names, January first. `inputs.clock_date`'s month field is 1..12,
/// so this is indexed by `month - 1`.
public static const uint MONTHS[12] = {
    getStringHash("Jan"), getStringHash("Feb"), getStringHash("Mar"),
    getStringHash("Apr"), getStringHash("May"), getStringHash("Jun"),
    getStringHash("Jul"), getStringHash("Aug"), getStringHash("Sep"),
    getStringHash("Oct"), getStringHash("Nov"), getStringHash("Dec"),
};

/// What the date block says, in order, one space between each part. DATE_* are `bar_model`'s; a token this bar has no arm for composes nothing, so an
/// unknown token shortens the date rather than corrupting it.
///
/// The default reads `Fri 5 Sep`, which is the C locale's `%a %-d %b`.
/// `{ DATE_DAY, DATE_MONTH }` reads `5 Sep`; with MONTHS spelled out in full
/// it reads `5 September`.
public static const uint CLOCK_DATE_ORDER[] = {
    DATE_WEEKDAY, DATE_DAY, DATE_MONTH,
};

/// Inset from the band's top and bottom to a chip. Small: a bar this short
/// wants its chips nearly full-height or they read as floating.
public static const float CHIP_MARGIN = 2.0;

/// A chip's corner radius. A quarter of a 28 px chip reads as "soft square";
/// half would make it a circle, which the tray may want later.
public static const float CHIP_RADIUS = 7.0;

// -- dock furniture ---------------------------------------------------------

/// Diameter of one running mark, logical px, and how many are ever drawn. The
/// marks sit against the screen-edge side of the chip; the focus underline
/// takes the free side, so the two never collide.
public static const float RUNNING_MARK = 3.0;
public static const uint RUNNING_MARK_MAX = 3u;
public static const float MARK_INSET = 1.5;
public static const float3 MARK_RGB = float3(0.72, 0.86, 0.94);
public static const float MARK_ALPHA = 0.85;

/// The focused entry's underline: thickness in logical px, and the fraction of
/// the chip's width it spans. It sits on the bar's *free* edge — the one
/// facing the desktop — so `EDGE_BOTTOM` puts it on top of the chip.
public static const float FOCUS_UNDERLINE = 2.0;
public static const float FOCUS_UNDERLINE_FRAC = 0.55;
public static const float3 FOCUS_RGB = float3(0.60, 0.90, 0.98);

/// The urgent wash: a standing tint over the whole chip, not a hover one.
/// Also what a tray item in NeedsAttention gets.
public static const float3 URGENT_TINT = float3(1.00, 0.62, 0.28);
public static const float URGENT_ALPHA = 0.30;

/// How long the pointer must rest on a dock entry before a preview is asked
/// for. cosmic-app-list uses 400 ms and it feels right; this is that number.
public static const float HOVER_DWELL_S = 0.4;

/// How long a live preview survives with the pointer on neither its entry nor
/// itself, before it is torn down.
///
/// This exists because "the pointer is on the popup" is
/// the pointer target the caustic keeps, which it sets from the *bar's* leave and
/// the *popup's* enter — two separate Wayland events. They usually arrive in
/// one dispatch and the question never comes up; when they do not, the frame
/// between them sees the pointer on nothing at all and would end a preview the
/// user is in the middle of moving into. A sixth of a second (0.15 s) is far longer than that gap and far shorter than a deliberate move away, so the grace is
/// invisible in both directions.
public static const float PREVIEW_GRACE_S = 0.15;

// -- feedback ---------------------------------------------------------------

/// The hover glow: a pale cyan, the same as the focus underline's (FOCUS_RGB) on purpose, so the bar has one accent colour.
public static const float3 HOVER_RGB = float3(0.60, 0.90, 0.98);

/// Alpha of the chip that fills a hovered element at full hover.
public static const float HOVER_CHIP_ALPHA = 0.16;

/// The halo outside the chip: how far it reaches (logical px) and how strong
/// it starts. This is the whole "soft" in "soft hover glow"; set the alpha to
/// 0 for a hard chip.
public static const float HOVER_HALO_PX = 5.0;
public static const float HOVER_HALO_ALPHA = 0.10;

/// The pressed dip: the chip darkens and shrinks by this much (logical px)
/// while a button is held. Two cheap cues instead of one, because the bar has
/// no shadow to sink into.
public static const float3 PRESS_RGB = float3(0.00, 0.00, 0.00);
public static const float PRESS_ALPHA = 0.28;
public static const float PRESS_INSET = 1.5;

/// Hover and press springs. 1.0 is critically damped: the fastest approach
/// with no overshoot, which is what a button wants — drop the ratio toward
/// 0.6 and the glow bounces, which reads as toy-like on chrome.
public static const float SPRING_STIFFNESS = 260.0;
public static const float SPRING_DAMPING_RATIO = 1.0;

/// Where a spring stops being worth another frame. Below these the value is
/// snapped to its target and `wants_redraw` clears, so an idle bar costs zero
/// frames (docs/architecture.md, *The frame*). Raise them and animations end a little
/// sooner, with a visible last step; lower them and they run on for frames
/// nobody can see.
public static const float SETTLE_EPS = 0.002;
public static const float SETTLE_VEL_EPS = 0.02;

/// The biggest step the springs will take, in seconds. A stalled frame must
/// not fling them: the frame time is clamped on the GPU, never trusted from
/// the CPU, so after a stall an animation resumes where it was instead of
/// jumping.
public static const float DT_MAX = 1.0 / 30.0;

// -- the popup --------------------------------------------------------------

/// The popup's ground. Slightly lighter than the bar and much less
/// transparent: a menu has to be readable over whatever it lands on.
public static const float3 POPUP_RGB = float3(0.06, 0.07, 0.11);
public static const float POPUP_OPACITY = 0.92;
public static const float POPUP_RADIUS = 10.0;

/// A hairline just inside the popup's edge, so it detaches from a dark
/// window behind it. Width 0 switches it off.
public static const float3 POPUP_EDGE_RGB = float3(0.42, 0.52, 0.62);
public static const float POPUP_EDGE_ALPHA = 0.55;
public static const float POPUP_EDGE_PX = 1.0;

/// Inset from the popup's edges to its contents.
public static const float POPUP_PAD = 8.0;

/// Room a menu leaves between itself and the far edge of the output, in
/// logical px: a menu taller than the output less the bar less this is
/// clamped to it and scrolls with the wheel.
public static const float POPUP_MARGIN = 16.0;

/// A toast — an applet's `RFLAG_ATTENTION` rows, shown without a click while
/// its chip carries `RFLAG_SHOW` (docs/architecture.md, *Menus, toasts and popups*) —
/// shows at most this many rows, newest first as the sidecar ordered them.
/// The rest are in the applet's own menu.
public static const uint TOAST_MAX_ROWS = 4u;

// -- menus ------------------------------------------------------------------

/// One row's height, and a separator's. The gutters are the columns a check
/// mark (left) and a submenu chevron (right) live in; a row's label starts
/// after the left one.
public static const float MENU_ROW_H = 26.0;
public static const float MENU_SEP_H = 9.0;
public static const float MENU_GUTTER = 18.0;
public static const float MENU_RADIUS = 5.0;

/// The popup's width is the widest row's content, clamped to this range. Too
/// narrow and a one-word menu looks like an accident; too wide and a long
/// "Copy the thing to the other thing" row drags the whole menu across the
/// screen.
public static const float MENU_MIN_W = 160.0;
public static const float MENU_MAX_W = 420.0;

/// Check marks and chevrons are SDF strokes, not glyphs, so they draw even when no installed font has those two symbols. This is the mark
/// box's side and the stroke's width, logical px.
public static const float MENU_MARK = 11.0;
public static const float MARK_STROKE = 1.6;
public static const float3 MARK_INK = float3(0.88, 0.92, 0.97);
/// A checkable-but-unchecked row gets a faint empty box, so the column reads
/// as a column rather than as a stray tick.
public static const float MARK_EMPTY_ALPHA = 0.22;
/// A disabled row's ink and its chrome, dimmed together.
public static const float DISABLED_ALPHA = 0.40;
/// A separator's hairline.
public static const float3 SEPARATOR_RGB = float3(0.42, 0.50, 0.60);
public static const float SEPARATOR_ALPHA = 0.35;
public static const float SEPARATOR_PX = 1.0;

// -- a row's body -------------------------------------------------------------
//
// A menu row may carry a second string, its **body** (`Row.body` on the wire,
// docs/abi.md, *Rows*): `caustics-notify` sends a notification's summary as the
// label and its whole body beside it. Such a row is drawn as a block — the
// label on the first line in LABEL_INK, the body under it in BODY_INK,
// wrapped to the popup's widest (MENU_MAX_W less the pads, the gutters and
// the row's icon) by greedy breaks at spaces on the shaped glyphs' advances:
// a line breaks at the last space that fits, a word longer than a line is
// cut at the glyph that fits, and the body's own newlines always break. The
// row's height follows its lines; the whole block is the row for hover and
// click. A row with no body is a plain row of MENU_ROW_H.
//
// The two line counts: at most this many body lines in a toast, and in a
// menu. A body with more ends its last shown line in BODY_ELLIPSIS. Lines
// past 8 are never laid out, whatever these say.
public static const uint TOAST_BODY_LINES = 4u;
public static const uint MENU_BODY_LINES = 6u;

/// One body line's height, logical px (the text is TEXT_PX[ITEM_MENU_ROW]),
/// and the room under the last one. A row with N body lines is
/// MENU_ROW_H + N × MENU_BODY_LINE_H + MENU_BODY_PAD tall: the label keeps
/// the band a plain row has, and the body hangs under it.
public static const float MENU_BODY_LINE_H = 18.0;
public static const float MENU_BODY_PAD = 4.0;

/// The body's ink: dimmer than LABEL_INK, so the label reads as the title.
/// Premultiplied linear RGBA.
public static const float4 BODY_INK = float4(0.66, 0.70, 0.76, 1.0);

/// What ends a body's last shown line when there is more of it: one literal,
/// shaped once per build at the body's size.
public static const uint BODY_ELLIPSIS = getStringHash("…");

/// Body bytes shaped per row, at most. A sidecar may send kilobytes
/// (`caustics-notify` sends up to 2 KiB); six lines of Latin at the menu's widest
/// are about 300, and every byte shaped is a glyph slot of the bar's budget.
/// A body cut here ends in BODY_ELLIPSIS like any other.
public static const uint BODY_SHAPE_BYTES = 384u;

// -- sliders ----------------------------------------------------------------
//
// A slider is a menu row a sidecar published (ITEM_SLIDER): its label sits
// where a menu row's would and a track runs across the bottom band of the
// row. Pressing the track seeks, dragging follows the pointer, and the value
// leaves as ACTION_SET_VALUE for the sidecar to apply (a volume, say).

/// A slider row's whole height, and the bottom band of it the track lives in.
/// The label centres in what is left above the band.
public static const float MENU_SLIDER_H = 42.0;
public static const float SLIDER_BAND = 18.0;

/// Track thickness and knob diameter, logical px. The knob's travel is the
/// track inset by half a knob at each end, so the knob never overhangs the
/// track — and the drag arithmetic is the exact inverse of the layout, which
/// is why grabbing the knob does not make it jump.
public static const float SLIDER_TRACK_H = 5.0;
public static const float SLIDER_KNOB = 13.0;

public static const float3 SLIDER_TRACK_RGB = float3(0.15, 0.17, 0.22);
public static const float3 SLIDER_FILL_RGB = float3(0.45, 0.72, 0.90);
public static const float SLIDER_TRACK_ALPHA = 1.0;
public static const float3 SLIDER_KNOB_RGB = float3(0.90, 0.94, 0.98);
/// How much brighter the knob gets at full hover (added, then clamped).
public static const float SLIDER_KNOB_HOVER_LIFT = 0.06;

// -- applets ----------------------------------------------------------------
//
// An applet's level (a volume, a signal strength) is drawn as a hairline bar
// across the bottom of its chip: legible at a glance beside the chip's icon (ICON_LOGICAL), and
// nothing like a second progress widget. A level below zero is "no level" and
// draws nothing at all.

/// The level bar's thickness and its inset from the chip's bottom edge and
/// its sides, logical px.
public static const float APPLET_LEVEL_H = 2.5;
public static const float APPLET_LEVEL_INSET = 3.0;
public static const float APPLET_LEVEL_SIDE = 4.0;
public static const float3 APPLET_LEVEL_RGB = float3(0.45, 0.72, 0.90);
public static const float3 APPLET_LEVEL_BG_RGB = float3(0.30, 0.34, 0.42);
public static const float APPLET_LEVEL_ALPHA = 0.95;
public static const float APPLET_LEVEL_BG_ALPHA = 0.45;

// -- previews ---------------------------------------------------------------

/// A preview title wider than its popup fades out over this many logical px
/// at the right edge instead of being cut mid-glyph.
public static const float TITLE_FADE = 28.0;

// -- the wake ---------------------------------------------------------------
//
// The slab is the floor of a shallow pool. Moving the pointer along the bar
// stirs the water above it, and the light the waves refract onto the floor is
// drawn under the bar's items: a bright net where the waves focus it, pale at
// the cores and faintly coloured on the flanks. A click drops a wave of its
// own. Only motion stirs the water: a pointer at rest excites nothing, and
// WAKE_SETTLE_S after the last motion the water is flat and the bar draws no
// more frames (docs/architecture.md, *The frame*).

/// How bright the light gets over the slab, as an alpha. 0 turns the wake off.
public static const float WAKE_ALPHA = 0.28;
/// Wave height a stroke at WAKE_FULL_SPEED raises, logical px: the strength.
/// Taller waves fold the light sooner and draw a finer, brighter net.
public static const float WAKE_HEIGHT = 1.3;
/// Pointer speed that raises waves WAKE_HEIGHT tall, logical px/s; slower
/// motion raises proportionally less.
public static const float WAKE_FULL_SPEED = 500.0;
/// A click's wave, as a multiple of WAKE_HEIGHT.
public static const float WAKE_CLICK = 1.6;
/// Crest to crest, logical px: the mesh of the net.
public static const float WAKE_WAVELENGTH = 16.0;
/// How fast the waves run out from where they were raised, logical px/s.
public static const float WAKE_SPEED = 140.0;
/// Seconds a wave lives, damping smoothly to flat: the longest the bar keeps
/// drawing after the pointer stops.
public static const float WAKE_SETTLE_S = 0.8;
/// Depth of the pool, logical px. Deeper water focuses the same waves
/// further, into a sharper and busier net.
public static const float WAKE_DEPTH = 60.0;
/// Water's dispersion, as a multiple of the real thing: how far the warm and
/// cool fringes part on a fold's flanks. 0 is white light, one band.
public static const float WAKE_DISPERSION = 4.0;

/// The biggest a dock entry's window preview is drawn, logical px, and its
/// corner radius. The captured window's aspect is preserved inside this box,
/// so a portrait window gets a narrow popup rather than a letterboxed wide
/// one.
public static const float PREVIEW_MAX_W = 320.0;
public static const float PREVIEW_MAX_H = 200.0;
public static const float PREVIEW_RADIUS = 6.0;

/// What is drawn while the popup is up and no frame has landed yet — a dim
/// plate rather than a hole, so the first 100 ms of a preview is not a flash
/// of the desktop.
public static const float3 PREVIEW_PLACEHOLDER_RGB = float3(0.14, 0.16, 0.21);
public static const float PREVIEW_PLACEHOLDER_ALPHA = 0.85;

// -- charts -------------------------------------------------------------------
//
// One strip of samples from a sidecar (`caustics-sysmon`, or anything else that
// writes the samples body, docs/abi.md, *Samples*). The samples body says how
// many charts there are, how many series each has, what to divide by, and
// what each row *measures* (`CHART_*`); everything about how one looks is
// here. There is no config for any of it beyond this file (docs/architecture.md,
// *Configuration is the caustic*).
//
// Colours are seeded from the minimon COSMIC applet's, so the strip reads
// like a system monitor its users already know: CPU #ff0600, memory #1dacd6,
// network #2f8dff / #00ff00, disk #ff6600 / #ffff00, GPU #00ff00, background
// #2b2b2b. They are sRGB values written straight, because the swapchain is
// Rgba8Unorm and this file's other colours are written the same way.

/// A chart's width as a multiple of its height. The height is the band's inner
/// row, so at scale 2 on a 32 px bar that is 56 px tall and 90 px wide — six of them plus the clock is about 830 device px, under a third of a 2560 px bar. Charts that do
/// not fit beside the wings are dropped from the end of the strip.
public static const float CHART_ASPECT = 1.6;

/// Space between two adjacent charts, logical px — minimon's panel spacing, and
/// tighter than ITEM_GAP on purpose: a chart strip reads as one instrument
/// cluster, not as six separate buttons.
public static const float CHART_GAP = 3.0;

/// The plotted line's thickness, logical px.
public static const float LINE_PX = 1.5;

/// Device pixels per sample, a whole number. The history scrolls one sample a
/// second, so this is also how far the curve moves per tick: a whole pixel,
/// never a fraction, or a one-second spike lands across one column on one
/// tick and two on the next and the whole strip visibly shimmers as it
/// scrolls. The chart shows the newest samples that fit at
/// this step — at 1 px and the default CHART_ASPECT that is about 85 s of the
/// sidecar's two minutes; widen the chart for more.
public static const float CHART_PX_PER_SAMPLE = 1.0;

/// Alpha of the area under the line, in the line's own colour. minimon fills
/// solid; a third of the way there keeps six charts from becoming six blocks
/// of colour on a translucent bar.
public static const float FILL_ALPHA = 0.33;

/// The floor on an auto-scaled chart's ceiling, in the row's own units. Rates
/// (network, disk) arrive with `ceiling = 0` and scale to their own window
/// maximum, which on an idle machine would magnify noise into a mountain
/// range; a megabyte a second is the quietest rate that draws a full-height chart.
/// Both series of a dual chart share it, so down and up stay comparable.
public static const float RATE_FLOOR = 1048576.0;

/// The chart's ground and its hairline frame. The slab goes down at the bar's
/// own opacity, so a chart is a slightly lighter box on the bar rather than a
/// hole through it, and the frame is a 1 px inset border over that.
public static const float3 CHART_BG = float3(0.169, 0.169, 0.169);
public static const float CHART_BG_ALPHA = SLAB_OPACITY;
public static const float3 CHART_FRAME_RGB = float3(0.42, 0.50, 0.60);
public static const float CHART_FRAME_ALPHA = 0.45;
public static const float CHART_FRAME_PX = 1.0;

/// The heat ramp: four stops sampled at the texel's own height inside the
/// chart, cool at the base and hot at the top, so the fill under a hot sample
/// runs the whole ramp and the fill under a cool one never leaves the blue.
/// CHART_TEMP is the one kind drawn this way.
public static const float3 HEAT_STOP0 = float3(0.13, 0.42, 0.85);
public static const float3 HEAT_STOP1 = float3(0.15, 0.80, 0.35);
public static const float3 HEAT_STOP2 = float3(1.00, 0.85, 0.10);
public static const float3 HEAT_STOP3 = float3(1.00, 0.10, 0.05);

// One colour per CHART_* kind, and a second for the kinds the sidecar gives
// two series. `bar.slang`'s `chart_rgb(kind, series)` is the only reader.

/// CPU busy: minimon's #ff0600.
public static const float3 CHART_CPU_RGB = float3(1.000, 0.024, 0.000);
/// Memory: minimon's #1dacd6.
public static const float3 CHART_MEM_RGB = float3(0.114, 0.675, 0.839);
/// Network down #2f8dff and up #00ff00.
public static const float3 CHART_NET_DOWN_RGB = float3(0.184, 0.553, 1.000);
public static const float3 CHART_NET_UP_RGB = float3(0.000, 1.000, 0.000);
/// Disk read #ff6600 and write #ffff00.
public static const float3 CHART_DISK_READ_RGB = float3(1.000, 0.400, 0.000);
public static const float3 CHART_DISK_WRITE_RGB = float3(1.000, 1.000, 0.000);
/// GPU: `caustics-sysmon` sends an AMD GPU as series 0 and an NVIDIA one as
/// series 1, as minimon overlays them on one chart (on a hybrid laptop, the
/// iGPU and the dGPU). Series 1 is minimon's #00ff00 and series 0 a teal
/// shift of it, near enough to read as the same instrument and far enough
/// apart to tell which line is which.
public static const float3 CHART_GPU_RGB = float3(0.000, 1.000, 0.000);
public static const float3 CHART_GPU_AMD_RGB = float3(0.000, 0.784, 0.627);
/// A temperature's flat colour. Nothing paints it — the strip and the
/// numbers' swatch both use the heat ramp — but `chart_rgb` stays total.
public static const float3 CHART_TEMP_RGB = float3(1.000, 0.400, 0.200);
/// Anything a third-party sidecar measures that the bar has no name for: a
/// neutral grey, so an unknown row is legible without claiming a meaning.
public static const float3 CHART_OTHER_RGB = float3(0.690, 0.690, 0.690);

// -- the chart numbers ----------------------------------------------------------
//
// The strip is a picture; these are its numbers. Rest the pointer on a chart
// and a popup without a grab opens beside the strip, one line per chart in
// the strip's own order (the sidecar's rows, left to right): a swatch in the
// chart's line colour (CHART_*_RGB above, the colour the strip paints with;
// a two-series chart's swatch is split between its two), the chart's name,
// and its newest sample in the unit its kind implies. The chart under the
// pointer is marked: its swatch filled where the others are outlined, and its
// line in the hover chip's wash. It leaves when the pointer leaves the strip.
// A click on the strip opens the same lines as a menu that stays until a
// second click or a click elsewhere; the lines are not buttons. The chart
// itself is not a button: no hover chip, no press dip, since a
// strip of six instruments lit up under the pointer would read as six
// buttons.
//
// The numbers are composed on the GPU from the samples body, beside the
// clock (`compose_chart_value` in bar.slang), from the words below. How a
// number is spelled, by kind:
//
//   CPU, MEM, GPU   an integer percent of the row's ceiling   `23 %`
//   TEMP            whole degrees                             `61 °C`
//   NET, DISK       bytes per second, binary prefixes, one    `0 B/s`, `9.5 KiB/s`,
//                   decimal below 10 (never for plain bytes)  `12 MiB/s`
//   a third party   the raw number, one decimal below 10      `3.5`
//
// A two-series chart says both, each after its mark (`↓ 1.2 MiB/s ↑ 48
// KiB/s`), or joined by ` / ` when its marks are empty. A gap — no sample
// yet, a sensor missing, the dGPU asleep — is CHART_NO_VALUE, never a 0.
// Memory is a percent: the samples body carries the used fraction and not
// the total.

/// Whether resting the pointer on the strip opens the numbers. False leaves
/// the click: the strip is quiet until asked. The rest is HOVER_DWELL_S, the
/// dock preview's.
public static const bool CHART_NUMBERS_ON_HOVER = true;

/// One line's height, logical px. The text is TEXT_PX[ITEM_CHART_LINE]; the
/// swatch is a MENU_MARK square in the MENU_GUTTER column a menu row's check
/// mark takes, so the numbers read as a menu's sibling.
public static const float CHART_NUMBERS_LINE_PX = 22.0;

/// Each chart kind's name, by the wire's CHART_* (CHART_OTHER first), so no
/// number is ever without the chart it belongs to.
public static const uint CHART_NAMES[7] = {
    getStringHash("Other"), // CHART_OTHER
    getStringHash("CPU"),   // CHART_CPU
    getStringHash("TEMP"),  // CHART_TEMP
    getStringHash("MEM"),   // CHART_MEM
    getStringHash("NET"),   // CHART_NET
    getStringHash("DISK"),  // CHART_DISK
    getStringHash("GPU"),   // CHART_GPU
};

/// The marks before a two-series chart's two values, by CHART_* as above: the
/// network's down and up, the disk's read and write. 0 is no mark, and a
/// chart whose marks are both 0 joins its values with ` / ` (the GPU's two,
/// AMD's first as the sidecar sends them).
public static const uint2 CHART_SERIES_MARKS[7] = {
    uint2(0u, 0u),                                   // CHART_OTHER
    uint2(0u, 0u),                                   // CHART_CPU
    uint2(0u, 0u),                                   // CHART_TEMP
    uint2(0u, 0u),                                   // CHART_MEM
    uint2(getStringHash("↓"), getStringHash("↑")),   // CHART_NET
    uint2(getStringHash("R"), getStringHash("W")),   // CHART_DISK
    uint2(0u, 0u),                                   // CHART_GPU
};

/// The units. A rate climbs the prefixes by 1024 while its rounded number
/// would be 1024 or more, and stops at the last.
public static const uint CHART_UNIT_PERCENT = getStringHash("%");
public static const uint CHART_UNIT_TEMP = getStringHash("°C");
public static const uint CHART_RATE_UNITS[4] = {
    getStringHash("B/s"), getStringHash("KiB/s"), getStringHash("MiB/s"), getStringHash("GiB/s"),
};

/// What a chart with no sample says instead of a number.
public static const uint CHART_NO_VALUE = getStringHash("–");
