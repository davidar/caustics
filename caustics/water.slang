// Water: the physics of a shallow pool whose surface carries expanding wave
// trains, lit by parallel light from straight above. The height field a set
// of sources raises, its slope and its curvature, the light the surface
// refracts onto the pool's floor, and the tone map that turns that light
// into something a screen can show. `tools/logo/caustic.py` traces the same
// physics offline, ray by ray; here it is per pixel, from the surface's
// curvature, and `tools/wake/wake.py` previews it.
//
// **Binding-free, so both caustics import it.** It declares no buffer and
// reads no tunables module: every knob is a field of `Water`, which each
// caller fills from its own tunables, and the sources come through
// `IWaterSources`, which each caller implements over its own state. The
// bar's half is `wake.slang` (the pointer's wake on the slab), the canvas's
// is `canvas_wake.slang` (the pointer's wake over the wallpaper); neither
// copies a formula of this one (caustics/README.md, *Conventions*).
//
// A source is a radial train `h(r) = a · env(u) · cos(k·u)`, u = r − R: a
// Gaussian envelope `Water.train` wavelengths wide whose centre R runs out
// at `Water.speed`, as tall as the source asked, damped smoothly to nothing
// at `Water.settle`, risen over WATER_RISE_S so it is never born as a
// point, and spread thinner as its ring grows.

module water;

/// Water's index for green light, and red's and blue's as offsets from it
/// (water runs about 1.331 to 1.337 across the visible range);
/// `Water.dispersion` scales the offsets.
public static const float WATER_IOR = 1.333;
public static const float3 WATER_IOR_SPREAD = float3(-0.002, 0.0, 0.004);
/// Seconds a new source takes to rise, so a train is never born as a point.
public static const float WATER_RISE_S = 0.06;

/// Every knob of the pool, in the caller's device px and seconds: the
/// physics' parameters, each caller's own tunables, scaled to its surface.
public struct Water
{
    /// Crest to crest, px (at least a px).
    public float wavelength;
    /// How fast a train's centre runs out from its source, px/s.
    public float speed;
    /// Seconds a source lives (at least 0.05), damping to flat.
    public float settle;
    /// The height a source of height 1 raises, px.
    public float height;
    /// A train's envelope, in wavelengths.
    public float train;
    /// The pool's depth, px: how far below the surface the floor is.
    public float depth;
    /// Water's dispersion, as a multiple of the real thing.
    public float dispersion;
    /// The tone map: the floor's light as a multiple of flat water's is
    /// capped at `imax`, then a soft shoulder of gain `exposure` and power
    /// `contrast` keeps the glow dark and the net bright.
    public float imax;
    public float exposure;
    public float contrast;
};

/// The sources a caller's pool is stirred by: a fixed number of slots, each
/// empty or a source at a place, of an age, of a height (a multiple of
/// `Water.height`).
public interface IWaterSources
{
    public uint slots();
    /// Slot `i`: false when it is empty, or its source is not this pool's.
    public bool source(uint i, out float2 at, out float age, out float height);
}

/// A train's envelope's half-width at which it is below e^-9: past this
/// from its centre a train raises nothing anywhere.
public float water_reach(Water w)
{
    return 3.0 * w.train * w.wavelength;
}

/// Whether a source of `age` can raise anything at distance `r` from it:
/// the train's ring, run out to `speed · age`, with its envelope either side.
public bool water_touches(Water w, float age, float r)
{
    return abs(r - w.speed * age) <= water_reach(w);
}

/// The surface at px `q`: its slope `grad` and its curvature `hess` (the
/// Hessian's xx, yy and xy), summed over the live sources.
///
/// With h' and h'' a train's radial derivatives and n the unit vector from
/// its source, the slope is h'·n and the Hessian h''·n nᵀ + (h'/r)·(I − n nᵀ):
/// the radial curvature, and the ring's own bend. r is kept off zero, where
/// the train has not risen yet anyway (WATER_RISE_S).
public void water_field<S : IWaterSources>(S sources, Water w, float2 q, out float2 grad,
                                           out float3 hess)
{
    float settle = max(w.settle, 0.05);
    float lambda = max(w.wavelength, 1.0);
    float k = 6.2831853 / lambda;
    float sigma = w.train * lambda;
    float inv_s2 = 1.0 / (sigma * sigma);
    grad = float2(0.0, 0.0);
    hess = float3(0.0, 0.0, 0.0);
    for (uint i = 0u; i < sources.slots(); ++i)
    {
        float2 at;
        float age;
        float height;
        if (!sources.source(i, at, age, height) || height <= 0.0)
        {
            continue;
        }
        float run = w.speed * age;
        float2 d = q - at;
        float r = length(d);
        float u = r - run;
        if (abs(u) > 3.0 * sigma)
        {
            continue; // the train's envelope is below e^-9 here
        }
        // Height: the source's share, damped to nothing at `settle`, risen
        // over WATER_RISE_S, and spread thinner as the ring grows.
        float fade = 1.0 - age / settle;
        float a = height * w.height * fade * fade * saturate(age / WATER_RISE_S)
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

/// How far each colour's ray, refracted by a unit slope, travels sideways on
/// its way to the floor, px: depth·(1 − 1/n) for near-vertical light, with
/// each channel's own index.
public float3 water_shift(Water w)
{
    float3 ior = WATER_IOR + w.dispersion * WATER_IOR_SPREAD;
    return max(w.depth, 0.0) * (1.0 - 1.0 / ior);
}

/// The floor at px `p`: false when no train reaches it (the water there is
/// flat). Otherwise `slope`, the surface's slope straight above `p`, and
/// `light`, the floor's light in each channel as a multiple of flat water's.
///
/// A ray through surface point q, refracted, reaches the floor at
/// q + shift·∇h(q), so the light there is 1/|det(I + shift·H)| of flat
/// water's: the reciprocal of the map's Jacobian, unbounded where the map
/// folds. Floor point p is lit from the q with q + shift·∇h(q) = p, found by
/// one fixed-point step from q = p; each channel then takes the determinant
/// with its own index, which moves its fold a little and parts the colours on
/// a fold's flanks. Capped at `Water.imax`.
public bool water_floor<S : IWaterSources>(S sources, Water w, float2 p, out float2 slope,
                                           out float3 light)
{
    float3 hess;
    water_field(sources, w, p, slope, hess);
    light = float3(1.0, 1.0, 1.0);
    if (all(slope == 0.0) && all(hess == 0.0))
    {
        return false; // no train reaches this pixel
    }
    float3 shift = water_shift(w);
    float2 q = p - shift.y * slope;
    float2 grad;
    water_field(sources, w, q, grad, hess);
    float3 det = (1.0 + shift * hess.x) * (1.0 + shift * hess.y) - shift * shift * hess.z * hess.z;
    light = min(1.0 / max(abs(det), 1e-4), w.imax);
    return true;
}

/// The tone map: how much of the caustic's glow a floor light of `light`
/// (as `water_floor` gives it) shows, 0..1 in each channel. Flat water's
/// light, and any darker, is 0.
public float3 water_glow(Water w, float3 light)
{
    return pow(1.0 - exp(-w.exposure * max(light - 1.0, 0.0)), w.contrast);
}
