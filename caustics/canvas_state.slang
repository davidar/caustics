// The canvas's state: its bindings, its private buffer and its witness ring.
//
// The canvas's own module of bindings, as `bar_state` is the bar's: every
// `[[vk::binding]]` of the canvas is here, and the host finds each in Slang's
// reflection by its name (docs/abi.md, *The canvas's buffer*). It imports
// nothing of the bar's. Every module of the bar declares or imports the
// bar's bindings, so none of them can be imported here; what the canvas
// draws with is its own (caustics/README.md, *The canvas*).

module canvas_state;

__exported import canvas_abi;

// ---------------------------------------------------------------------------
// Bind group 0, the only one. Bindings an entry does not use are stripped
// from its module, which Vulkan permits.
// ---------------------------------------------------------------------------

/// The one buffer the host reads back: `wants_redraw` and the witness ring.
[[vk::binding(0, 0)]]
public RWStructuredBuffer<CanvasState> state;

/// This output's size, its scale, the frame stamp and the time.
[[vk::binding(1, 0)]]
public ConstantBuffer<CanvasParams> params;

/// The last show and the last dim, as they crossed the wire, and which slot
/// holds which picture.
[[vk::binding(2, 0)]]
public StructuredBuffer<CanvasFacts> facts;

#if defined(DST_FORMAT_RGBA8)
[[vk::binding(3, 0)]]
[format("rgba8")]
public WTexture2D<float4> dst;
#else
[[vk::binding(3, 0)]]
public WTexture2D<float4> dst; // format unknown: SPIR-V has no BGRA storage format
#endif

/// The two pictures, **straight** RGBA8 as the wire carries them, each the
/// size of the image in it. Which is the old and which the new is
/// `facts.from_slot` and `facts.to_slot`.
[[vk::binding(4, 0)]]
public Texture2D<float4> image_a;

[[vk::binding(5, 0)]]
public Texture2D<float4> image_b;

[[vk::binding(6, 0)]]
public SamplerState tex_sampler;

/// What the canvas keeps for itself between frames. The host allocates it at
/// the stride Slang reports and never reads or writes a word of it.
public struct CanvasPrivate
{
    /// The cross-fade's alpha this frame, curved: 0 is the old picture, 1
    /// the new. Written by `canvas_apply`, read by `canvas_draw`.
    public float alpha;
    /// How much of the dim colour is laid over the picture this frame, 0..1.
    public float dim;
    /// `facts.shows` as of the last apply: a new value is a new fade.
    public uint shows_seen;
    /// Which of this fade's witnesses have been said (`SAID_*`).
    public uint said;
    /// The dim head's command and start as of the last apply, so a new dim
    /// is said once.
    public uint dim_command;
    public uint dim_start_lo;
    /// The fade's progress through its time, 0..1, before the curve.
    public float progress;
    /// Whether the blank canvas has been said.
    public uint said_blank;
};

[[vk::binding(7, 0)]]
public RWStructuredBuffer<CanvasPrivate> private;

/// `CanvasPrivate::said`'s bits.
public static const uint SAID_MIDPOINT = 1u;
public static const uint SAID_DONE = 2u;

// ---------------------------------------------------------------------------
// Time: `CLOCK_BOOTTIME` ms as two words.
// ---------------------------------------------------------------------------

/// `now - start` in ms, from two pairs of words: 0 when `start` is still
/// ahead, and saturated past 2^32 ms (49 days), which every fade has long
/// finished by. No 64-bit integers, which not every device has.
public float elapsed_ms(uint now_lo, uint now_hi, uint start_lo, uint start_hi)
{
    if (now_hi < start_hi || (now_hi == start_hi && now_lo < start_lo))
    {
        return 0.0;
    }
    uint borrow = now_lo < start_lo ? 1u : 0u;
    uint hi = now_hi - start_hi - borrow;
    if (hi != 0u)
    {
        return 4294967295.0;
    }
    return float(now_lo - start_lo);
}

/// How far through its time a show or a dim is, 0..1, at `params.now`. A
/// duration of 0 is already there.
public float progress_of(CanvasBody body)
{
    if (body.duration_ms == 0u)
    {
        return 1.0;
    }
    float t = elapsed_ms(params.now_lo, params.now_hi, body.start_lo, body.start_hi);
    return saturate(t / float(body.duration_ms));
}

// ---------------------------------------------------------------------------
// The witness ring (docs/abi.md, *The witness ring*).
// ---------------------------------------------------------------------------

/// Operands under construction: up to ten words, the rest dropped.
public struct Args
{
    public uint w[10];
    public uint n;
};

public Args args_new()
{
    Args a;
    for (uint i = 0u; i < 10u; ++i)
    {
        a.w[i] = 0u;
    }
    a.n = 0u;
    return a;
}

public void arg(inout Args a, uint value)
{
    if (a.n < 10u)
    {
        a.w[a.n] = value;
        a.n += 1u;
    }
}

public void argf(inout Args a, float value)
{
    arg(a, asuint(value));
}

/// One line of the host's journal: a format literal's hash and its words.
/// Once per change is the caller's duty, against what it kept in `private`.
public void witness(uint level, uint head, Args a)
{
    if (state[0].witness_frame != params.frame)
    {
        state[0].witness_frame = params.frame;
        state[0].witness_count = 0u;
        state[0].witness_lost = 0u;
    }
    uint n = state[0].witness_count;
    if (n >= MAX_CANVAS_WITNESSES)
    {
        state[0].witness_lost += 1u;
        return;
    }
    state[0].witnesses[n].head = head;
    state[0].witnesses[n].aux = level;
    for (uint i = 0u; i < 10u; ++i)
    {
        state[0].witnesses[n].arg[i] = a.w[i];
    }
    state[0].witness_count = n + 1u;
}

public void witness0(uint level, uint head)
{
    witness(level, head, args_new());
}
