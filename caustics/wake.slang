// The wake: a real caustic under the pointer, the slab's light refracted by
// waves the pointer's motion makes on a shallow pool. A subject with no
// sidecar and no item of its own: the pointer stirs it, a press drops a
// source, `bar_apply` steps it, and `bar_draw` lights the bar's slab with
// it. Its sources and what was last witnessed of them are `Private.wake`
// (`WakeScratch`); `tools/wake/wake.py` previews it off the GPU from the
// constants below and the `WAKE_*` tunables.
//
// It names no other subject, and no item kind at all.

module wake;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import bar_state;
import bar_paint;

// ---------------------------------------------------------------------------
// The wake. The slab is the floor of a shallow pool: a water surface above it
// carries the waves the pointer's motion makes, parallel light from straight
// above is refracted by that surface, and what the slab shows is the caustic
// the surface casts on its floor — a bright net where the refraction folds,
// pale cores, and warm and cool fringes on the flanks, because each colour
// bends by its own index. `tools/logo/caustic.py` traces the same physics
// offline, ray by ray; here it is per pixel, from the surface's curvature.
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

/// Water's index for green light, and red's and blue's as offsets from it
/// (water runs about 1.331 to 1.337 across the visible range);
/// WAKE_DISPERSION scales the offsets.
public static const float WAKE_IOR = 1.333;
public static const float3 WAKE_IOR_SPREAD = float3(-0.002, 0.0, 0.004);
/// A train's Gaussian envelope, in wavelengths: about two crests and troughs.
public static const float WAKE_TRAIN = 1.3;
/// Seconds a new source takes to rise, so a train is never born as a point.
public static const float WAKE_RISE_S = 0.06;
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

/// The wake's surface at bar px `q`: its slope `grad` and its curvature
/// `hess` (the Hessian's xx, yy and xy), summed over the live sources.
///
/// A source is a radial train `h(r) = a · env(u) · cos(k·u)`, u = r − R, the
/// envelope a Gaussian and R the distance it has run. With h' and h'' its
/// radial derivatives and n the unit vector from the source, the slope is
/// h'·n and the Hessian h''·n nᵀ + (h'/r)·(I − n nᵀ): the radial curvature,
/// and the ring's own bend. r is kept off zero, where the train has not
/// risen yet anyway (WAKE_RISE_S).
public void wake_field(float2 q, out float2 grad, out float3 hess)
{
    float s = scale_px();
    float settle = max(WAKE_SETTLE_S, 0.05);
    float lambda = max(WAKE_WAVELENGTH, 1.0) * s;
    float k = 6.2831853 / lambda;
    float sigma = WAKE_TRAIN * lambda;
    float inv_s2 = 1.0 / (sigma * sigma);
    grad = float2(0.0, 0.0);
    hess = float3(0.0, 0.0, 0.0);
    for (uint i = 0u; i < WAKE_SOURCES; ++i)
    {
        float4 w = private[0].wake.sources[i];
        if (w.w <= 0.0)
        {
            continue;
        }
        float run = WAKE_SPEED * s * w.z;
        float2 d = q - w.xy;
        float r = length(d);
        float u = r - run;
        if (abs(u) > 3.0 * sigma)
        {
            continue; // the train's envelope is below e^-9 here
        }
        // Height: the source's share, damped to nothing at WAKE_SETTLE_S,
        // risen over WAKE_RISE_S, and spread thinner as the ring grows.
        float fade = 1.0 - w.z / settle;
        float a = w.w * WAKE_HEIGHT * s * fade * fade * saturate(w.z / WAKE_RISE_S)
                  / sqrt(1.0 + run / lambda);
        float g = exp(-u * u * inv_s2);
        float g1 = -2.0 * u * inv_s2 * g;
        float g2 = (4.0 * u * u * inv_s2 - 2.0) * inv_s2 * g;
        float c = cos(k * u);
        float sn = sin(k * u);
        float h1 = g1 * c - g * k * sn;
        float h2 = g2 * c - 2.0 * g1 * k * sn - g * k * k * c;
        float rr = max(r, 0.5 * sigma);
        float2 n = d / rr;
        float bend = h1 / rr;
        grad += a * h1 * n;
        hess += a * float3(h2 * n.x * n.x + bend * n.y * n.y,
                           h2 * n.y * n.y + bend * n.x * n.x,
                           (h2 - bend) * n.x * n.y);
    }
}

/// The wake's light on the slab colour `col` at bar px `p`.
///
/// A ray through surface point q, refracted, reaches the floor at
/// q + reach·∇h(q), with reach = depth·(1 − 1/n) for near-vertical light, so
/// the light there is 1/|det(I + reach·H)| of flat water's: the reciprocal of
/// the map's Jacobian, unbounded where the map folds. Floor point p is lit
/// from the q with q + reach·∇h(q) = p, found by one fixed-point step from
/// q = p; each channel then takes the determinant with its own index, which
/// moves its fold a little and parts the colours on a fold's flanks.
public float4 wake_light(float2 p, float4 col)
{
    if (WAKE_ALPHA <= 0.0 || private[0].wake.said == 0u)
    {
        return col;
    }
    float2 grad;
    float3 hess;
    wake_field(p, grad, hess);
    if (all(grad == 0.0) && all(hess == 0.0))
    {
        return col; // no train reaches this pixel
    }
    float s = scale_px();
    float3 ior = WAKE_IOR + WAKE_DISPERSION * WAKE_IOR_SPREAD;
    float3 reach = max(WAKE_DEPTH, 0.0) * s * (1.0 - 1.0 / ior);
    float2 q = p - reach.y * grad;
    wake_field(q, grad, hess);

    float3 det = (1.0 + reach * hess.x) * (1.0 + reach * hess.y) - reach * reach * hess.z * hess.z;
    float3 light = min(1.0 / max(abs(det), 1e-4), WAKE_IMAX);
    float3 v = pow(1.0 - exp(-WAKE_EXPOSURE * max(light - 1.0, 0.0)), WAKE_CONTRAST);
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
