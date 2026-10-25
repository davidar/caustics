// The canvas, as a caustic: the wallpaper on every output and the cross-fade
// between two of them (docs/canvas.md). Two entry points over one descriptor
// set (docs/abi.md, *The canvas's buffer*):
//
//   canvas_apply  1x1x1  — the fade's alpha and the dim from the last show,
//                          the last dim and the time, into `private`; the
//                          witnesses; `wants_redraw`. Once per frame.
//   canvas_draw   8x8    — the two source pictures at that alpha, the dim
//                          over them, onto the output's swapchain image.
//
// **A fade is between the sources, never a compound of the last frame**
// (docs/canvas.md, *Time*): alpha is a function of the time alone, so a
// two-hour fade is at its midpoint at the midpoint, and one that sleeps
// through a suspend lands where `CLOCK_BOOTTIME` says.
//
// Destination format, as the bar's: compiled once with -DDST_FORMAT_RGBA8 and
// once with -DDST_FORMAT_BGRA8, the second declaring `dst` format-unknown.
//
// The hazard the bar's notes name holds here too: never let a `[numthreads]`
// attribute detach from its entry point and land on a helper above it.

import canvas_abi;
import canvas_tunables;
import canvas_state;

/// The fade's curve, from its progress (canvas_tunables.slang, `FADE_CURVE`).
float fade_curve(float t)
{
    if (FADE_CURVE == FADE_SMOOTH)
    {
        return smoothstep(0.0, 1.0, t);
    }
    return t;
}

/// How much of the dim colour lies over the picture now: the last dim's
/// depth, reached over its duration from nothing; nothing after an undim.
float dim_now(CanvasBody dim)
{
    if (dim.command != CANVAS_DIM)
    {
        return 0.0;
    }
    float depth = float(min(dim.alpha_milli, DIM_MAX_MILLI)) / float(DIM_MAX_MILLI);
    return depth * progress_of(dim);
}

[shader("compute")]
[numthreads(1, 1, 1)]
void canvas_apply()
{
    CanvasFacts f = facts[0];
    bool pictured = f.to_slot != NO_INDEX;
    bool shown = pictured && f.show.command == CANVAS_SHOW;
    float t = shown ? progress_of(f.show) : 1.0;

    // A new show is a new fade, and each of its lines is said once.
    if (f.shows != private[0].shows_seen)
    {
        private[0].shows_seen = f.shows;
        private[0].said = 0u;
        Args a = args_new();
        arg(a, f.shows);
        arg(a, f.to_slot);
        arg(a, f.show.duration_ms);
        witness(WITNESS_INFO, getStringHash("canvas: show {0} into slot {1}, fading over {2} ms"), a);
    }
    if (!pictured && private[0].said_blank == 0u)
    {
        private[0].said_blank = 1u;
        witness0(WITNESS_INFO, getStringHash("canvas: no picture yet; the dim colour"));
    }
    bool fading = shown && f.show.duration_ms > 0u;
    if (fading && t >= 0.5 && (private[0].said & SAID_MIDPOINT) == 0u)
    {
        private[0].said |= SAID_MIDPOINT;
        Args a = args_new();
        arg(a, f.shows);
        argf(a, t);
        witness(WITNESS_INFO, getStringHash("canvas: fade {0} midpoint (t={1:f})"), a);
    }
    if (fading && t >= 1.0 && (private[0].said & SAID_DONE) == 0u)
    {
        private[0].said |= SAID_DONE;
        Args a = args_new();
        arg(a, f.shows);
        witness(WITNESS_INFO, getStringHash("canvas: fade {0} done"), a);
    }

    // A new dim or an undim, said once.
    if (f.dim.command != private[0].dim_command || f.dim.start_lo != private[0].dim_start_lo)
    {
        private[0].dim_command = f.dim.command;
        private[0].dim_start_lo = f.dim.start_lo;
        if (f.dim.command == CANVAS_DIM)
        {
            Args a = args_new();
            arg(a, f.dim.alpha_milli);
            arg(a, f.dim.duration_ms);
            witness(WITNESS_INFO, getStringHash("canvas: dim to {0}/1000 over {1} ms"), a);
        }
        else if (f.dim.command == CANVAS_UNDIM)
        {
            witness0(WITNESS_INFO, getStringHash("canvas: undim"));
        }
    }

    float d = dim_now(f.dim);
    bool dimming = f.dim.command == CANVAS_DIM && progress_of(f.dim) < 1.0;
    private[0].progress = t;
    private[0].alpha = fade_curve(t);
    private[0].dim = d;
    state[0].wants_redraw = ((fading && t < 1.0) || dimming) ? 1u : 0u;
}

/// One picture at an output pixel, laid on the output by `FIT`, over the dim
/// colour where it is transparent or does not reach. Straight alpha: the
/// texel's own alpha says how much of it covers the dim colour.
float3 picture(uint slot, float2 p)
{
    uint w;
    uint h;
    if (slot == SLOT_A)
    {
        image_a.GetDimensions(w, h);
    }
    else
    {
        image_b.GetDimensions(w, h);
    }
    float2 size = float2(float(max(w, 1u)), float(max(h, 1u)));
    float2 out_size = float2(float(max(params.width, 1u)), float(max(params.height, 1u)));
    float2 ratio = out_size / size;
    float k = FIT == FIT_CONTAIN ? min(ratio.x, ratio.y) : max(ratio.x, ratio.y);
    float2 uv = ((p - 0.5 * out_size) / k + 0.5 * size) / size;
    if (any(uv < 0.0) || any(uv > 1.0))
    {
        return DIM_COLOUR;
    }
    float4 texel = slot == SLOT_A
        ? image_a.SampleLevel(tex_sampler, uv, 0.0)
        : image_b.SampleLevel(tex_sampler, uv, 0.0);
    return lerp(DIM_COLOUR, texel.rgb, texel.a);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void canvas_draw(uint3 gid: SV_DispatchThreadID)
{
    if (gid.x >= params.width || gid.y >= params.height)
    {
        return;
    }
    float2 p = float2(gid.xy) + 0.5;
    CanvasFacts f = facts[0];
    float3 colour = DIM_COLOUR;
    if (f.to_slot != NO_INDEX)
    {
        float3 to = picture(f.to_slot, p);
        float a = private[0].alpha;
        if (f.from_slot != NO_INDEX && a < 1.0)
        {
            colour = lerp(picture(f.from_slot, p), to, a);
        }
        else
        {
            colour = lerp(DIM_COLOUR, to, a);
        }
    }
    colour = lerp(colour, DIM_COLOUR, private[0].dim);
    // Opaque: the background layer is the bottom of the output.
    dst.Store(gid.xy, float4(colour, 1.0));
}
