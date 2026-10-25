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
/// fades in from, what a transparent picture is laid over, and what a dim
/// goes towards.
public static const float3 DIM_COLOUR = float3(0.07, 0.08, 0.10);

/// The shape of a cross-fade between two pictures.
public static const uint FADE_CURVE = FADE_SMOOTH;

/// How a picture whose shape is not the output's is laid on it.
public static const uint FIT = FIT_COVER;
