// The canvas's state: its bindings, its private buffer and its witness ring.
//
// The canvas's own module of bindings, as `bar_state` is the bar's: every
// `[[vk::binding]]` of the canvas is here, and the host finds each in Slang's
// reflection by its name (docs/abi.md, *The canvas's buffer*). It imports
// nothing of the bar's. Every module of the bar declares or imports the
// bar's bindings, so none of them can be imported here; what the canvas
// draws with is its own (caustics/README.md, *The canvas*).
//
// The text engine's modules are imported as the bar's caustic imports them:
// `text` declares its four curve buffers in set 1 itself, and `shape`
// declares none, so its tables are gathered here in set 2 under the names
// the bar uses (`shapeParams`, `shapeOt`, `shapeUtf8`), which is what lets
// one host-side table of the engine's buffers serve both caustics
// (`caustics_host::text`).

module canvas_state;

__exported import canvas_abi;
__exported import shape;
__exported import text;

// ---------------------------------------------------------------------------
// Bind group 0, the only one. Bindings an entry does not use are stripped
// from its module, which Vulkan permits.
// ---------------------------------------------------------------------------

/// The one buffer the host reads back: `wants_redraw`, the dim's depth and
/// the witness ring.
[[vk::binding(0, 0)]]
public RWStructuredBuffer<CanvasState> state;

/// This surface's size, its scale, the frame stamp, the time, and which of
/// the output's surfaces it is (`TARGET_*`).
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

/// Every shaped glyph of the canvas's text, `CANVAS_GLYPHS` of them:
/// written by `canvas_shape`, read by `canvas_draw`. The caustic's own, as
/// `private` is; a binding rather than a field of it because `shapeRun`
/// writes through an `RWStructuredBuffer<ShapedGlyph>` handle.
[[vk::binding(8, 0)]]
public RWStructuredBuffer<ShapedGlyph> runs;

/// The literal pool: every `getStringHash` literal of the running caustic as
/// `(hash, off, len)` into the strings buffer, `facts[0].literal_count` of
/// them. How the dots and the caustic's own word for an ask are spelled.
[[vk::binding(9, 0)]]
public StructuredBuffer<Literal> literals;

/// The prompt's own text, as its sidecar sent it: what `facts[0].prompt`'s
/// ranges name. Never a secret (docs/abi.md, *The canvas's buffer*). Words,
/// read-write, because `ShapeTables.utf8_alt0` is a handle of that type; the
/// caustic writes none of it.
[[vk::binding(10, 0)]]
public RWStructuredBuffer<uint> prompt_text;

// ---------------------------------------------------------------------------
// Bind group 2: `shape`'s tables and the strings buffer, gathered.
// ---------------------------------------------------------------------------

[[vk::binding(0, 2)]]
public ConstantBuffer<ShapeParams> shapeParams;

/// The arena `caustics-glyphs` wrote (punchcutter/docs/arena.md, *The arena*).
[[vk::binding(1, 2)]]
public StructuredBuffer<uint> shapeOt;

/// The strings buffer: the literal pool's bytes, four to a word.
[[vk::binding(2, 2)]]
public RWStructuredBuffer<uint> shapeUtf8;

/// The handles, gathered (punchcutter/docs/arena.md, *A shaping dispatch*):
/// every field set, since an unset handle is an undefined descriptor. Source
/// 0 is the strings buffer and 1 `prompt_text` (`TEXT_*`).
public ShapeTables shape_tables(uint source)
{
    ShapeTables t;
    t.ot = shapeOt;
    t.utf8 = shapeUtf8;
    t.utf8_alt0 = prompt_text;
    t.utf8_alt1 = prompt_text;
    t.source = source;
    return t;
}

/// The literal behind a hash, or an empty one when the pool lacks it.
public Literal literal(uint hash)
{
    uint n = min(facts[0].literal_count, MAX_CANVAS_LITERALS);
    uint bound;
    uint stride;
    literals.GetDimensions(bound, stride);
    n = min(n, bound);
    for (uint i = 0u; i < n; ++i)
    {
        if (literals[i].hash == hash)
        {
            return literals[i];
        }
    }
    Literal none;
    none.hash = hash;
    none.off = 0u;
    none.len = 0u;
    none._pad0 = 0u;
    return none;
}

/// One shaped text block: where its bytes are, and what `canvas_shape` made
/// of them. The run's glyphs are `runs[first ..]`, `glyphs` of them, at
/// `SHAPE_PX` (canvas_text.slang).
public struct CanvasRun
{
    /// `TEXT_*`, and the byte range in that buffer, as shaped.
    public uint source;
    public uint off;
    public uint len;
    /// The first of the block's slots in `runs`, and how many it filled.
    public uint first;
    public uint glyphs;
    /// `SHAPE_RUN_*`.
    public uint flags;
    /// The run's advance at `SHAPE_PX`, px.
    public float advance;
    public uint _pad0;
};

/// Where the prompt's panel and its rows are on the surface being drawn,
/// device px: worked out once a frame by `canvas_apply`, read by every
/// thread of `canvas_draw`.
public struct PromptLayout
{
    /// The panel, `(x0, y0, x1, y1)`.
    public float4 panel;
    /// The answer's field, the same way.
    public float4 field;
    /// The title's box: its lines' height, its widest line's width. What
    /// the oracle looks inside.
    public float4 title_box;
    /// The title's lines, 1 to `PROMPT_TITLE_LINES` (at most five): line
    /// `k + 1` starts at glyph `title_starts[k]` (`wrap_from`), drawn
    /// shifted left by `title_shifts[k]`, the pen position of that glyph.
    /// Lines past `title_lines` are `NO_INDEX`.
    public uint title_lines;
    public uint4 title_starts;
    public float4 title_shifts;
    /// The ask's words' box, as the title's.
    public float4 ask_box;
    /// Top-left of the line's row.
    public float2 line_org;
    /// The em sizes of the title and of everything else.
    public float title_px;
    public float text_px;
    /// The right edge text is clipped at.
    public float clip;
    public float radius;
};

/// What the canvas keeps for itself between frames. The host allocates it at
/// the stride Slang reports and never reads or writes a word of it.
public struct CanvasPrivate
{
    /// The cross-fade's alpha this frame, curved: 0 is the old picture, 1
    /// the new. Written by `canvas_apply`, read by `canvas_draw`.
    public float alpha;
    /// How much of the dim colour the overlay is this frame, 0..1.
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
    /// The dim head's id as of the last apply: with its command and start,
    /// what makes a dim new.
    public uint dim_id;
    /// Whether this dim's reaching its depth has been said.
    public uint said_dim_reached;
    /// The prompt's id, state and attempts as of the last witness, so each
    /// step of a conversation is said once.
    public uint said_prompt_id;
    public uint said_prompt_state;
    public uint said_prompt_attempts;
    /// How many runs had a missing glyph as of the last witness.
    public uint said_notdef;
    /// The text blocks, as `canvas_shape` last shaped them.
    public CanvasRun text[CANVAS_TEXT_BLOCKS];
    /// The prompt's layout on the surface this frame draws.
    public PromptLayout prompt;
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
