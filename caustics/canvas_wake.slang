// The canvas's wake: the pointer stirring water over the wallpaper, on the
// background wherever the compositor gives the background the pointer, and
// on the lock surfaces, where the pointer is the lock's for the whole time
// (docs/canvas.md, *The wake*). The physics is `water.slang`'s, the bar's
// wake's own; this module is the canvas's half: where the sources come
// from, when they die, and what the pool does to the picture under it.
//
// The host writes where the pointer is, on which pane, and a count of the
// presses there (`CanvasFacts::pointer_*`, `presses`); it decides nothing.
// The frame of the pointer's own pane drops a source each WAKE_SPACING of
// travel, as tall as the pointer's speed asks, and one WAKE_CLICK tall at a
// press. A source is stamped with the `CLOCK_BOOTTIME` it was dropped at and
// with its pane, and is aged from that stamp by every frame, so its age is a
// function of the time alone (docs/canvas.md, *Time*), and every pane sees
// only its own (`CanvasWake`'s note says why one ring). The last source dies
// WAKE_SETTLE_S after the last motion, and then the pane asks for no frames
// (`CanvasState::wants_redraw`).
//
// **Cheap where the water is flat.** `canvas_apply` works out the box its
// pane's live trains can reach; a pixel outside it is the plain picture,
// and one inside it checks its distance to each of the WAKE_SOURCES slots
// before it evaluates any train (`water_field`), so a wake in one corner
// costs that corner.

module canvas_wake;

import canvas_abi;
import canvas_tunables;
import canvas_state;
import water;

/// A train's Gaussian envelope, in wavelengths: about two crests and troughs.
public static const float WAKE_TRAIN = 1.3;
/// Logical px of pointer travel between sources.
public static const float WAKE_SPACING = 12.0;
/// A source shorter than this, as a fraction of WAKE_HEIGHT, is not dropped:
/// a hand that trembles excites nothing.
public static const float WAKE_MIN_HEIGHT = 0.05;
/// The tone map, as the bar's: the floor's light capped at WAKE_IMAX times
/// flat water's, then a soft shoulder that keeps the glow dark and the net
/// bright.
public static const float WAKE_IMAX = 6.0;
public static const float WAKE_EXPOSURE = 0.9;
public static const float WAKE_CONTRAST = 1.3;

/// This surface's logical px, in buffer px.
float wake_scale()
{
    return float(max(params.scale, 1u)) / 120.0;
}

/// How bright the wake is on the surface being drawn: WAKE_ALPHA on the
/// background, that times WAKE_LOCK on a lock surface, and nothing on the
/// overlay or the prompt's pane, where the pointer is a cancel or a panel's.
public float wake_alpha()
{
    if (params.target == TARGET_BACKGROUND)
    {
        return max(WAKE_ALPHA, 0.0);
    }
    if (params.target == TARGET_LOCK)
    {
        return max(WAKE_ALPHA * WAKE_LOCK, 0.0);
    }
    return 0.0;
}

/// The canvas's pool, from `canvas_tunables` and the constants above, in
/// this surface's buffer px.
public Water canvas_water()
{
    float s = wake_scale();
    Water w;
    w.wavelength = WAKE_WAVELENGTH * s;
    w.speed = WAKE_SPEED * s;
    w.settle = max(WAKE_SETTLE_S, 0.05);
    w.height = WAKE_HEIGHT * s;
    w.train = WAKE_TRAIN;
    w.depth = WAKE_DEPTH * s;
    w.dispersion = WAKE_DISPERSION;
    w.imax = WAKE_IMAX;
    w.exposure = WAKE_EXPOSURE;
    w.contrast = WAKE_CONTRAST;
    return w;
}

/// The ring, as the pool of the pane being drawn: its own sources, at the
/// ages this frame's apply gave them.
struct PaneSources : IWaterSources
{
    uint slots()
    {
        return WAKE_SOURCES;
    }

    bool source(uint i, out float2 at, out float age, out float height)
    {
        WakeSource w = private[0].wake.sources[i];
        at = w.at;
        age = w.age;
        height = w.height;
        return w.pane == params.pane && w.height > 0.0;
    }
};

/// A source at buffer px `at` of the pane being drawn, `height` times
/// WAKE_HEIGHT tall, born now. It takes the oldest slot.
void wake_drop(float2 at, float height)
{
    uint i = min(private[0].wake.next, WAKE_SOURCES - 1u);
    WakeSource w;
    w.at = at;
    w.height = height;
    w.age = 0.0;
    w.pane = params.pane;
    w.born_lo = params.now_lo;
    w.born_hi = params.now_hi;
    w._pad0 = 0u;
    private[0].wake.sources[i] = w;
    private[0].wake.next = (i + 1u) % WAKE_SOURCES;
    private[0].wake.travel = 0.0;
    private[0].wake.drop_lo = params.now_lo;
    private[0].wake.drop_hi = params.now_hi;
}

/// The wake's part of `canvas_apply`: drop what the pointer's motion and
/// presses ask for, if this is the pointer's pane; age every source and let
/// the dead go; work out this pane's box; say when the water stirs and when
/// it is still. True while a source lives on the pane being drawn.
public bool step_wake(CanvasFacts f)
{
    float s = wake_scale();
    Water w = canvas_water();
    bool on = wake_alpha() > 0.0;
    float since = elapsed_ms(params.now_lo, params.now_hi, private[0].wake.drop_lo,
                             private[0].wake.drop_hi) / 1000.0;

    if (f.pointer_pane == 0u)
    {
        // Off every pane that stirs: the next arrival starts the count
        // afresh, and a press made elsewhere is not one here.
        private[0].wake.last_pane = 0u;
        private[0].wake.presses_seen = f.presses;
    }
    else if (f.pointer_pane == params.pane)
    {
        float2 p = float2(f.pointer_x, f.pointer_y);
        if (private[0].wake.last_pane != params.pane)
        {
            // Arriving is not moving: the first position only starts the
            // count.
            private[0].wake.last_pane = params.pane;
            private[0].wake.last = p;
            private[0].wake.travel = 0.0;
        }
        else
        {
            private[0].wake.travel += length(p - private[0].wake.last);
            private[0].wake.last = p;
            if (private[0].wake.travel >= WAKE_SPACING * s && since >= w.settle / float(WAKE_SOURCES))
            {
                // The mean speed since the last source, logical px/s. After a
                // rest it reads low, so the first source after one is gentle.
                float speed = private[0].wake.travel / (max(since, 1e-3) * s);
                float height = saturate(speed / max(WAKE_FULL_SPEED, 1.0));
                if (on && height >= WAKE_MIN_HEIGHT)
                {
                    wake_drop(p, height);
                }
                else
                {
                    private[0].wake.travel = 0.0;
                    private[0].wake.drop_lo = params.now_lo;
                    private[0].wake.drop_hi = params.now_hi;
                }
            }
        }
        if (f.presses != private[0].wake.presses_seen)
        {
            private[0].wake.presses_seen = f.presses;
            if (on)
            {
                wake_drop(p, WAKE_CLICK);
            }
        }
    }

    // Every source ages by the clock; this pane's own make its box.
    bool any = false;
    bool mine = false;
    float4 box = float4(1e30, 1e30, -1e30, -1e30);
    float margin = water_reach(w);
    for (uint i = 0u; i < WAKE_SOURCES; ++i)
    {
        WakeSource src = private[0].wake.sources[i];
        if (src.height <= 0.0)
        {
            continue;
        }
        float age = elapsed_ms(params.now_lo, params.now_hi, src.born_lo, src.born_hi) / 1000.0;
        if (age >= w.settle)
        {
            src.height = 0.0;
            src.age = 0.0;
        }
        else
        {
            src.age = age;
            any = true;
            if (src.pane == params.pane)
            {
                mine = true;
                float r = w.speed * age + margin;
                box = float4(min(box.xy, src.at - r), max(box.zw, src.at + r));
            }
        }
        private[0].wake.sources[i] = src;
    }
    private[0].wake.live = mine && on ? 1u : 0u;
    private[0].wake.box = box;

    uint stirred = any ? 1u : 0u;
    if (stirred != private[0].wake.said)
    {
        private[0].wake.said = stirred;
        Args a = args_new();
        if (any)
        {
            arg(a, params.pane);
            arg(a, params.target);
            witness(WITNESS_INFO, getStringHash("wake: stirred on pane {0} (target {1})"), a);
        }
        else
        {
            argf(a, since);
            witness(WITNESS_INFO, getStringHash("wake: still, {0:f} s after its last source"), a);
        }
    }
    return mine && on;
}

/// The water at buffer px `p` of the pane being drawn: false where it is
/// flat (no live train of this pane's reaches `p`, or the wake is off here),
/// and otherwise how far the picture seen there is shifted in each channel
/// (`shift.c · slope`, from `water_shift`) and the floor's light in each,
/// as a multiple of flat water's.
public bool wake_at(float2 p, out float2 slope, out float3 shift, out float3 light)
{
    slope = float2(0.0, 0.0);
    shift = float3(0.0, 0.0, 0.0);
    light = float3(1.0, 1.0, 1.0);
    float4 box = private[0].wake.box;
    if (private[0].wake.live == 0u || any(p < box.xy) || any(p > box.zw))
    {
        return false;
    }
    PaneSources sources;
    Water w = canvas_water();
    if (!water_floor(sources, w, p, slope, light))
    {
        return false;
    }
    shift = water_shift(w);
    return true;
}

/// The floor colour `floor`, seen through the water and lit by it: darker by
/// up to the alpha where the waves spread the light thin, and the caustic's
/// glow, in WAKE_LIGHT_RGB, added on top where they focus it. Light on a
/// dark floor reads as light.
public float3 wake_lit(float3 floor, float3 light)
{
    float a = saturate(wake_alpha());
    float3 glow = water_glow(canvas_water(), light);
    float3 shade = 1.0 - a * (1.0 - min(light, 1.0));
    return saturate(floor * shade + a * glow * WAKE_LIGHT_RGB);
}
