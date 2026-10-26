// The wake: a real caustic under the pointer, the slab's light refracted by
// waves the pointer's motion makes on a shallow pool. A subject with no
// sidecar and no item of its own: the pointer stirs it, a press drops a
// source, `bar_apply` steps it, and `bar_draw` lights the bar's slab with
// it. Its sources and what was last witnessed of them are `Private.wake`
// (`WakeScratch`); `tools/wake/wake.py` previews it off the GPU from the
// constants below, `water.slang`'s and the `WAKE_*` tunables.
//
// It names no other subject, and no item kind at all.

module wake;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_paint;
import water;

// ---------------------------------------------------------------------------
// The wake. The slab is the floor of a shallow pool: a water surface above it
// carries the waves the pointer's motion makes, parallel light from straight
// above is refracted by that surface, and what the slab shows is the caustic
// the surface casts on its floor — a bright net where the refraction folds,
// pale cores, and warm and cool fringes on the flanks, because each colour
// bends by its own index. The physics is `water.slang`'s, which the canvas's
// wake shares; this module is the bar's half: the sources, how the pointer
// drops them, and how the slab is lit.
//
// The surface is a sum of WAKE_SOURCES expanding wave trains, a ring in
// `Private.wake.sources`: the pointer drops one each WAKE_SPACING of travel along the
// bar, as tall as its speed asks, and a click drops one WAKE_CLICK tall. A
// source runs out at WAKE_SPEED and dies, smoothly, WAKE_SETTLE_S after it
// was dropped. So a pointer that rests excites nothing, the last source dies
// within WAKE_SETTLE_S of the last motion, `step_wake` reports nothing moving
// and the bar asks for no frames (docs/architecture.md, *The frame*).
//
// A source takes the slot of the oldest, and is never dropped sooner than
// WAKE_SETTLE_S / WAKE_SOURCES after the last, so the slot it takes is
// already empty; only a click may cut an old train short.
// ---------------------------------------------------------------------------

/// A train's Gaussian envelope, in wavelengths: about two crests and troughs.
public static const float WAKE_TRAIN = 1.3;
/// Logical px of pointer travel between sources.
public static const float WAKE_SPACING = 10.0;
/// A source shorter than this, as a fraction of WAKE_HEIGHT, is not dropped:
/// a hand that trembles on the bar excites nothing.
public static const float WAKE_MIN_HEIGHT = 0.05;
/// The tone map: the floor's light as a multiple of flat water's, capped at
/// WAKE_IMAX (at the fold itself it is unbounded), then a soft shoulder whose
/// gain and power keep the glow dark and the net bright.
public static const float WAKE_IMAX = 6.0;
public static const float WAKE_EXPOSURE = 0.9;
public static const float WAKE_CONTRAST = 1.3;

/// The bar's pool, from `bar_tunables` and the constants above, in bar px.
Water bar_water()
{
    float s = scale_px();
    Water w;
    w.wavelength = WAKE_WAVELENGTH * s;
    w.speed = WAKE_SPEED * s;
    w.settle = WAKE_SETTLE_S;
    w.height = WAKE_HEIGHT * s;
    w.train = WAKE_TRAIN;
    w.depth = WAKE_DEPTH * s;
    w.dispersion = WAKE_DISPERSION;
    w.imax = WAKE_IMAX;
    w.exposure = WAKE_EXPOSURE;
    w.contrast = WAKE_CONTRAST;
    return w;
}

/// The ring in `Private.wake`, as the pool's sources: x and y in bar px, age
/// in seconds, height as a multiple of WAKE_HEIGHT; height 0 is empty.
struct BarSources : IWaterSources
{
    uint slots()
    {
        return WAKE_SOURCES;
    }

    bool source(uint i, out float2 at, out float age, out float height)
    {
        float4 w = private[0].wake.sources[i];
        at = w.xy;
        age = w.z;
        height = w.w;
        return w.w > 0.0;
    }
};

/// Drop a source at bar px `at`, `height` times WAKE_HEIGHT tall.
public void wake_drop(float2 at, float height)
{
    uint i = min(private[0].wake.next, WAKE_SOURCES - 1u);
    private[0].wake.sources[i] = float4(at, 0.0, height);
    private[0].wake.next = (i + 1u) % WAKE_SOURCES;
}

/// A click drops a source of its own, at the click.
public void wake_click(float2 at)
{
    if (WAKE_ALPHA > 0.0)
    {
        wake_drop(at, WAKE_CLICK);
        private[0].wake.track.w = 0.0;
    }
}

/// Age the sources, drop one if the pointer has travelled far enough, and say
/// when the wake stirs and when it is still. True while any source lives.
public bool step_wake(float dt)
{
    float s = scale_px();
    float settle = max(WAKE_SETTLE_S, 0.05);

    // `wake.track`: the pointer as the wake last saw it (x < 0: not on the
    // bar), the px it has travelled since the last source, and the seconds.
    float4 t = private[0].wake.track;
    t.w = min(t.w + dt, 60.0);
    bool on_bar = private[0].pointer_target == TARGET_BAR && private[0].pointer.x >= 0.0;
    bool drop = false;
    float height = 0.0;
    if (!on_bar)
    {
        t.xyz = float3(-1.0, -1.0, 0.0);
    }
    else if (t.x < 0.0)
    {
        // Arriving is not moving: the first position only starts the count.
        t.xyz = float3(private[0].pointer, 0.0);
    }
    else
    {
        float2 p = private[0].pointer;
        t.z += length(p - t.xy);
        t.xy = p;
        if (t.z >= WAKE_SPACING * s && t.w >= settle / float(WAKE_SOURCES))
        {
            // The mean speed since the last source, logical px/s. After a
            // rest it reads low, so the first source after one is gentle.
            float speed = t.z / (max(t.w, 1e-3) * s);
            height = saturate(speed / max(WAKE_FULL_SPEED, 1.0));
            drop = height >= WAKE_MIN_HEIGHT && WAKE_ALPHA > 0.0;
            t.z = 0.0;
            t.w = 0.0;
        }
    }
    private[0].wake.track = t;

    bool alive = false;
    for (uint i = 0u; i < WAKE_SOURCES; ++i)
    {
        float4 w = private[0].wake.sources[i];
        if (w.w <= 0.0)
        {
            continue;
        }
        w.z += dt;
        if (w.z >= settle)
        {
            w = float4(0.0, 0.0, 0.0, 0.0);
        }
        else
        {
            alive = true;
        }
        private[0].wake.sources[i] = w;
    }
    if (drop)
    {
        wake_drop(private[0].pointer, height);
        alive = true;
    }

    uint stirred = alive ? 1u : 0u;
    if (stirred != private[0].wake.said)
    {
        private[0].wake.said = stirred;
        if (alive)
        {
            witness0(WITNESS_INFO, getStringHash("wake: stirred"));
        }
        else
        {
            Args a = args_new();
            argf(a, t.w);
            witness(WITNESS_INFO, getStringHash("wake: still, {0:f} s after its last source"), a);
        }
    }
    return alive;
}

/// The wake's light on the slab colour `col` at bar px `p`: the caustic the
/// surface casts on the slab (`water_floor`), tone-mapped, in the accent.
public float4 wake_light(float2 p, float4 col)
{
    if (WAKE_ALPHA <= 0.0 || private[0].wake.said == 0u)
    {
        return col;
    }
    BarSources sources;
    Water w = bar_water();
    float2 slope;
    float3 light;
    if (!water_floor(sources, w, p, slope, light))
    {
        return col; // no train reaches this pixel
    }
    float3 v = water_glow(w, light);
    float peak = max(v.r, max(v.g, v.b));
    if (peak <= 0.002)
    {
        return col;
    }
    // The accent, turning toward white where the light is hottest, so a
    // crossing reads as light; each channel keeps its own band's share.
    float hot = saturate((dot(light, float3(1.0 / 3.0)) - 2.5) / 3.0);
    float3 tint = lerp(HOVER_RGB, float3(1.0, 1.0, 1.0), 0.5 * hot);
    return ink(tint * v / peak, WAKE_ALPHA * peak, col);
}

// ---------------------------------------------------------------------------
// What the passes ask of the wake, through `bar_dispatch`
// ---------------------------------------------------------------------------

/// The wake's line of a `CMD_DUMP_STALL` answer: how many sources live, the
/// oldest one's age and the seconds since the last was dropped.
public void wake_stall()
{
    uint sources = 0u;
    float oldest = 0.0;
    for (uint w = 0u; w < WAKE_SOURCES; ++w)
    {
        if (private[0].wake.sources[w].w > 0.0)
        {
            sources += 1u;
            oldest = max(oldest, private[0].wake.sources[w].z);
        }
    }
    Args o = args_new();
    arg(o, sources);
    argf(o, oldest);
    argf(o, private[0].wake.track.w);
    witness(WITNESS_WARN,
            getStringHash("stall: wake(sources,oldest s,since last s)={0},{1:f},{2:f}"), o);
}
