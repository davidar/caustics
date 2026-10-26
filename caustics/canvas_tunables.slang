// The canvas's tunables: every knob of the wallpaper, in one module, and the
// canvas's only configuration (docs/canvas.md; the rule is the bar's,
// docs/architecture.md, *Configuration is the caustic*).
//
// Save this file in the checkout the canvas was built from and the running
// canvas changes within a second: `caustics-reload` compiles the canvas's
// caustic (`canvas.slang` and the `canvas_*.slang` modules beside it) and the
// host swaps it in. A file that does not compile keeps the last good canvas
// and prints Slang's diagnostic to the journal.
//
// Colours are the swapchain's own encoding, the one an image's bytes are in:
// `float3(0.5, 0.5, 0.5)` is the grey whose bytes are 128.

module canvas_tunables;

// ---------------------------------------------------------------------------
// The vocabulary the knobs below are written in.
// ---------------------------------------------------------------------------

/// alpha(t) = t: a dissolve at a constant rate.
public static const uint FADE_LINEAR = 0u;
/// alpha(t) = smoothstep(t): eased in and out. Symmetric, so the midpoint of
/// the fade is still at the midpoint of its time.
public static const uint FADE_SMOOTH = 1u;

/// The picture fills the output and the overhang is cropped (swaybg's `fill`).
public static const uint FIT_COVER = 0u;
/// The whole picture is shown and the rest of the output is the dim colour
/// (swaybg's `fit`).
public static const uint FIT_CONTAIN = 1u;

// ---------------------------------------------------------------------------
// The knobs.
// ---------------------------------------------------------------------------

/// What the canvas is before any picture has arrived, what a first picture
/// fades in from, what a transparent picture is laid over, and what the
/// dim's wallpaper is darkened towards (`DIM_DARKEN`).
public static const float3 DIM_COLOUR = float3(0.07, 0.08, 0.10);

/// The shape of a cross-fade between two pictures.
public static const uint FADE_CURVE = FADE_SMOOTH;

/// How a picture whose shape is not the output's is laid on it.
public static const uint FIT = FIT_COVER;

/// How far the dim's wallpaper is darkened towards `DIM_COLOUR` when the dim
/// is all the way in, 0..1. A dim dissolves the windows into the wallpaper;
/// this is how much darker that wallpaper is than the background's. 0 is the
/// windows simply going away, the desktop's picture left as it is; more
/// reads as a screen gone to sleep, and gives a prompt drawn over the dim a
/// quieter backdrop to be legible on.
public static const float DIM_DARKEN = 0.3;

// ---------------------------------------------------------------------------
// The prompt (docs/canvas.md, *The prompt core*). Sizes are logical px, so a
// prompt is the same size on a scale-2 output as on a scale-1 one.
// ---------------------------------------------------------------------------

/// How far the prompt's wallpaper is darkened towards `DIM_COLOUR`, 0..1: the
/// dim's rule, a little darker than `DIM_DARKEN`, so the panel reads as the
/// one thing on the screen.
public static const float PROMPT_DARKEN = 0.45;

/// The panel's width; it is narrower only on an output narrower than it.
public static const float PROMPT_WIDTH = 440.0;
/// The space inside the panel's edge, and between its rows.
public static const float PROMPT_PADDING = 24.0;
public static const float PROMPT_GAP = 12.0;
/// The corners' radius.
public static const float PROMPT_RADIUS = 14.0;
/// How many lines a long title may take before it ends at the panel's
/// edge: a polkit message names the command, and a command's path is one
/// long word, cut across lines. At most five.
public static const uint PROMPT_TITLE_LINES = 4u;
/// The em sizes of the title and of every other line.
public static const float PROMPT_TITLE_PX = 17.0;
public static const float PROMPT_TEXT_PX = 14.0;
/// The space inside the answer's field, around its dots or its text.
public static const float PROMPT_FIELD_PADDING = 8.0;
/// The space between two dots, on top of the dot's own advance.
public static const float PROMPT_DOT_GAP = 3.0;

/// The panel, its answer field, and the inks: the title and the typed text,
/// the ask's own words and an info line, an error line.
public static const float3 PROMPT_PANEL_COLOUR = float3(0.13, 0.14, 0.17);
public static const float3 PROMPT_FIELD_COLOUR = float3(0.08, 0.09, 0.11);
public static const float3 PROMPT_INK = float3(0.92, 0.93, 0.95);
public static const float3 PROMPT_MUTED_INK = float3(0.62, 0.64, 0.69);
public static const float3 PROMPT_ERROR_INK = float3(0.95, 0.45, 0.42);

// ---------------------------------------------------------------------------
// The wake (docs/canvas.md, *The wake*). The wallpaper is the floor of a
// shallow pool. Moving the pointer over the bare desktop, or over a lock
// screen, stirs the water above it: the picture is seen through the waves,
// bent by them, and the light they refract onto the floor is drawn on it, a
// bright net where the waves focus it, faintly coloured on its flanks. A
// click drops a wave of its own. Only motion stirs the water: a pointer at
// rest excites nothing, and WAKE_SETTLE_S after the last motion the water is
// flat and the surface draws no more frames. The names are the bar's
// (bar_tunables.slang, *the wake*); the values are the canvas's own. Sizes
// are logical px.
// ---------------------------------------------------------------------------

/// How bright the refracted light gets on the desktop, as an alpha; also how
/// far the floor darkens where the waves spread the light thin. 0 turns the
/// wake off everywhere, and then the pointer costs no frame at all.
public static const float WAKE_ALPHA = 0.26;
/// The lock screen's light, as a multiple of WAKE_ALPHA: its floor is the
/// darkened wallpaper, where the same net reads brighter. 0 keeps the lock's
/// water still.
public static const float WAKE_LOCK = 1.5;
/// The light's colour: sunlight through water, a little warm.
public static const float3 WAKE_LIGHT_RGB = float3(1.0, 0.97, 0.90);
/// Wave height a stroke at WAKE_FULL_SPEED raises: the strength. Taller
/// waves fold the light sooner, into a finer and brighter net, and bend the
/// picture further.
public static const float WAKE_HEIGHT = 2.2;
/// Pointer speed that raises waves WAKE_HEIGHT tall, logical px/s; slower
/// motion raises proportionally less.
public static const float WAKE_FULL_SPEED = 900.0;
/// A click's wave, as a multiple of WAKE_HEIGHT.
public static const float WAKE_CLICK = 1.6;
/// Crest to crest: the mesh of the net.
public static const float WAKE_WAVELENGTH = 28.0;
/// How fast the waves run out from where they were raised, logical px/s.
public static const float WAKE_SPEED = 170.0;
/// Seconds a wave lives, damping smoothly to flat: the longest a surface
/// keeps drawing after the pointer stops.
public static const float WAKE_SETTLE_S = 1.6;
/// Depth of the pool. Deeper water focuses the same waves further, into a
/// sharper and busier net, and shifts the picture seen through it further.
public static const float WAKE_DEPTH = 90.0;
/// Water's dispersion, as a multiple of the real thing: how far the warm and
/// cool fringes part on a fold's flanks, and the picture's colours where the
/// water bends it. 0 is white light, one band.
public static const float WAKE_DISPERSION = 4.0;

/// How many milliseconds one `canvas_shape` pass may take on the device
/// before the host stops shaping until the font's arena is written again: a
/// prompt's few runs take well under one, so this is far past a healthy pass
/// and far short of a hostile arena's cost. On a software adapter the host
/// allows eight times it.
public static const uint SHAPE_WATCHDOG_MS = 32u;
