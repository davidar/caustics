// The canvas, as a caustic: the wallpaper on every output, the cross-fade
// between two of them, the idle dim over everything, and the prompt's panel
// (docs/canvas.md). Three entry points over three descriptor sets: the
// canvas's own, and the text engine's two (docs/abi.md, *The canvas's
// buffer*):
//
//   canvas_shape  128x1x1, one thread per text block — the prompt's text, shaped
//                          (canvas_text.slang). Its own submission, before a
//                          frame, whenever the text or the font changed.
//   canvas_apply  1x1x1  — the fade's alpha and the dim from the last show,
//                          the last dim and the time, into `private`; the
//                          witnesses; `wants_redraw` for the surface being
//                          drawn; whether the dim is at its depth. Once per
//                          frame.
//   canvas_draw   8x8    — onto one surface's swapchain image, by the
//                          surface its pipeline was built for
//                          (`surface_target()`, one pipeline per surface,
//                          caustics/README.md, *Conventions*): on the
//                          background, the two source
//                          pictures at that alpha, opaque, seen through the
//                          pointer's wake; on the overlay, the same
//                          wallpaper, darkened by `DIM_DARKEN`, at the dim's
//                          alpha, premultiplied; on the prompt's pane, the
//                          wallpaper darkened by `PROMPT_DARKEN` and the
//                          panel on it; on a lock surface, the prompt's
//                          look, seen through the wake.
//
// **The secret never enters the caustic** (docs/canvas.md, *Why a caustic*).
// What an echo-off ask has typed reaches here as a count, and the panel draws
// that many dots; the bytes stay in the host.
//
// **One dim, one owner.** The dim is drawn only on the overlay, the layer
// above every window that the host puts up while a dim is and takes down on
// an undim or on input; the background never darkens. The overlay is the
// wallpaper again, from the same slots with the same fit and the same
// cross-fade, so as its alpha rises the windows dissolve and the desktop's
// picture is what remains; at `DIM_DARKEN` 0 an overlay at full alpha is
// pixel for pixel the background with no windows.
//
// **The wake** is `canvas_wake.slang`'s, over `water.slang`'s physics, on
// the background and the lock surfaces only: on the overlay the pointer is a
// cancel, and on the prompt's pane the panel's.
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
import canvas_text;
import canvas_wake;

/// One text block a thread, in a group of two waves: as the bar's `bar_shape`
/// walks its blocks, and for the same reason (the note beside its attribute in
/// `bar.slang`: a single-wave group puts the shaper's spills on a Mesa path
/// that reorders them). `SHAPE_GROUP` in `canvas/src/gpu.rs` mirrors the 128.
[shader("compute")]
[numthreads(128, 1, 1)]
void canvas_shape(uint3 gid: SV_DispatchThreadID)
{
    shape_block(gid.x);
}

/// The fade's curve, from its progress (canvas_tunables.slang, `FADE_CURVE`).
float fade_curve(float t)
{
    if (FADE_CURVE == FADE_SMOOTH)
    {
        return smoothstep(0.0, 1.0, t);
    }
    return t;
}

/// How much of the dim colour the overlay is now: the last dim's depth,
/// reached over its duration from nothing; nothing after an undim.
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
    if (f.dim.command != private[0].dim_command || f.dim.start_lo != private[0].dim_start_lo
        || f.dim.id != private[0].dim_id)
    {
        private[0].dim_command = f.dim.command;
        private[0].dim_start_lo = f.dim.start_lo;
        private[0].dim_id = f.dim.id;
        private[0].said_dim_reached = 0u;
        if (f.dim.command == CANVAS_DIM)
        {
            Args a = args_new();
            arg(a, f.dim.id);
            arg(a, f.dim.alpha_milli);
            arg(a, f.dim.duration_ms);
            witness(WITNESS_INFO, getStringHash("canvas: dim {0} to {1}/1000 over {2} ms"), a);
        }
        else if (f.dim.command == CANVAS_UNDIM)
        {
            witness0(WITNESS_INFO, getStringHash("canvas: undim"));
        }
    }

    // The dim at its depth: said once, and stated every frame in `state`
    // for the host, which tells the dim's sidecar (docs/abi.md, *Canvas
    // event*). A completed dim stays there until an undim or input.
    bool dimmed = f.dim.command == CANVAS_DIM;
    bool reached = dimmed && progress_of(f.dim) >= 1.0;
    if (reached && private[0].said_dim_reached == 0u)
    {
        private[0].said_dim_reached = 1u;
        Args a = args_new();
        arg(a, f.dim.id);
        arg(a, f.dim.alpha_milli);
        witness(WITNESS_INFO, getStringHash("canvas: dim {0} reached {1}/1000"), a);
    }
    state[0].dim_reached = reached ? 1u : 0u;
    state[0].dim_id = f.dim.id;

    private[0].progress = t;
    private[0].alpha = fade_curve(t);
    private[0].dim = dim_now(f.dim);
    // Another frame for the surface being drawn, and only for it: the
    // background while its fade runs, the overlay while its alpha moves. A
    // settled surface draws nothing until something changes; the prompt's
    // pane changes only when the host writes something new.
    // The background and a lock surface while a wake lives on them, too.
    bool stirred = step_wake(f);
    bool more = false;
    if (surface_target() == TARGET_OVERLAY)
    {
        more = dimmed && !reached;
    }
    else if (surface_target() == TARGET_BACKGROUND)
    {
        more = (fading && t < 1.0) || stirred;
    }
    else if (surface_target() == TARGET_LOCK)
    {
        more = stirred;
    }
    state[0].wants_redraw = more ? 1u : 0u;
    state[0].wants_pointer = wake_alpha() > 0.0 ? 1u : 0u;

    apply_prompt(f.prompt);
    apply_text();
}

/// The prompt's steps, each said once: a conversation held (begun, no ask
/// yet), asking (and on which attempt), verifying, failed, closed. The typed
/// count is never said.
void apply_prompt(CanvasPrompt pr)
{
    if (pr.id != private[0].said_prompt_id || pr.state != private[0].said_prompt_state
        || pr.attempts != private[0].said_prompt_attempts)
    {
        uint was = private[0].said_prompt_state;
        private[0].said_prompt_id = pr.id;
        private[0].said_prompt_state = pr.state;
        private[0].said_prompt_attempts = pr.attempts;
        Args a = args_new();
        arg(a, pr.id);
        if (pr.state == PROMPT_HELD)
        {
            witness(WITNESS_INFO, getStringHash("prompt: conversation {0} held, no ask yet"), a);
        }
        else if (pr.state == PROMPT_ASKING)
        {
            arg(a, pr.echo);
            arg(a, pr.attempts);
            witness(WITNESS_INFO, getStringHash("prompt: conversation {0} asking (echo {1}, attempts {2})"), a);
        }
        else if (pr.state == PROMPT_VERIFYING)
        {
            witness(WITNESS_INFO, getStringHash("prompt: conversation {0} verifying"), a);
        }
        else if (pr.state == PROMPT_FAILED)
        {
            arg(a, pr.attempts);
            witness(WITNESS_INFO, getStringHash("prompt: conversation {0} failed (attempts {1})"), a);
        }
        else if (was != PROMPT_IDLE)
        {
            witness0(WITNESS_INFO, getStringHash("prompt: closed"));
        }
    }
    if (carries_prompt())
    {
        layout_prompt(pr);
    }
}

/// Whether the surface being drawn carries the prompt's panel: the prompt's
/// pane, and a lock surface.
bool carries_prompt()
{
    uint target = surface_target();
    return target == TARGET_PROMPT || target == TARGET_LOCK;
}

/// The text's witnesses and the runs to cover, and the shape pass's
/// threshold for the host's watchdog.
void apply_text()
{
    state[0].shape_watchdog_ms = max(SHAPE_WATCHDOG_MS, 1u);
    uint2 notdef = text_covers();
    if (notdef.x != private[0].said_notdef)
    {
        private[0].said_notdef = notdef.x;
        Args a = args_new();
        arg(a, notdef.x);
        arg(a, notdef.y);
        witness(WITNESS_INFO, getStringHash("text: {0} of {1} runs have a glyph the font lacks"), a);
    }
}

/// Where the panel and its rows are on this surface: centred, `PROMPT_WIDTH`
/// wide or the output's width less its padding, as tall as its rows. A
/// title, the ask's words, the answer's field, and one line under it for
/// whatever the conversation said last.
void layout_prompt(CanvasPrompt pr)
{
    float s = float(max(params.scale, 1u)) / 120.0;
    float w = float(params.width);
    float h = float(params.height);
    float pad = PROMPT_PADDING * s;
    float gap = PROMPT_GAP * s;
    float title_px = PROMPT_TITLE_PX * s;
    float text_px = PROMPT_TEXT_PX * s;
    float title_lh = text_line_height(title_px);
    float lh = text_line_height(text_px);
    float field_h = lh + 2.0 * PROMPT_FIELD_PADDING * s;
    float pw = min(PROMPT_WIDTH * s, max(w - 2.0 * pad, 1.0));
    // A title too long for the panel takes more lines, up to
    // PROMPT_TITLE_LINES, broken at a space or, failing one, inside the
    // word; the last line ends at the edge like any other.
    float title_w_max = pw - 2.0 * pad;
    uint max_lines = clamp(PROMPT_TITLE_LINES, 1u, 5u);
    uint4 starts = uint4(NO_INDEX, NO_INDEX, NO_INDEX, NO_INDEX);
    float4 shifts = float4(0.0, 0.0, 0.0, 0.0);
    uint lines = 1u;
    uint start = 0u;
    float title_w = 0.0;
    for (uint k = 0u; k + 1u < max_lines; ++k)
    {
        uint next = wrap_from(BLOCK_TITLE, start, title_px, title_w_max);
        if (next == NO_INDEX)
        {
            break;
        }
        float x0 = glyph_x(BLOCK_TITLE, start, title_px);
        title_w = max(title_w, glyph_x(BLOCK_TITLE, next, title_px) - x0);
        starts[k] = next;
        shifts[k] = glyph_x(BLOCK_TITLE, next, title_px);
        start = next;
        lines += 1u;
    }
    title_w = max(title_w, text_width(BLOCK_TITLE, title_px) - glyph_x(BLOCK_TITLE, start, title_px));
    float title_h = float(lines) * title_lh;
    float ph = pad + title_h + gap + lh + 0.5 * gap + field_h + gap + lh + pad;
    float x0 = floor(0.5 * (w - pw));
    float y0 = floor(0.5 * (h - ph));
    PromptLayout l;
    l.panel = float4(x0, y0, x0 + pw, y0 + ph);
    l.title_px = title_px;
    l.text_px = text_px;
    l.clip = x0 + pw - pad;
    l.radius = PROMPT_RADIUS * s;
    float left = x0 + pad;
    float y = y0 + pad;
    l.title_lines = lines;
    l.title_starts = starts;
    l.title_shifts = shifts;
    title_w = min(title_w, l.clip - left);
    l.title_box = float4(left, y, left + title_w, y + title_h);
    y += title_h + gap;
    float ask_w = min(text_width(BLOCK_ASK, text_px), l.clip - left);
    l.ask_box = float4(left, y, left + ask_w, y + lh);
    y += lh + 0.5 * gap;
    l.field = float4(left, y, l.clip, y + field_h);
    y += field_h + gap;
    l.line_org = float2(left, y);
    private[0].prompt = l;
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

/// The wallpaper at an output pixel: the two source pictures at the fade's
/// alpha, or the dim colour before any. What the background is, and what the
/// overlay dims to.
float3 wallpaper(float2 p)
{
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
    return colour;
}

/// How much of a pixel at `p` a rounded rect `r` (`(x0, y0, x1, y1)`, corner
/// radius `radius`) covers, antialiased over a pixel.
float rounded(float4 r, float radius, float2 p)
{
    float2 centre = 0.5 * (r.xy + r.zw);
    float2 half_size = 0.5 * (r.zw - r.xy);
    float rad = min(radius, min(half_size.x, half_size.y));
    float2 q = abs(p - centre) - (half_size - rad);
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - rad;
    return saturate(0.5 - d);
}

/// The wallpaper's `colour` as the prompt's pane and the lock show it:
/// darkened towards `DIM_COLOUR` by `PROMPT_DARKEN`.
float3 prompt_floor(float3 colour)
{
    return lerp(colour, DIM_COLOUR, saturate(PROMPT_DARKEN));
}

/// The wallpaper at `p` seen through the wake, each channel at its own
/// shift along the surface's slope, as `prompt_floor` darkens it when
/// `darkened`, and lit by the water; the plain one where the water is flat.
float3 through_water(float2 p, bool darkened)
{
    float2 slope;
    float3 shift;
    float3 light;
    if (!wake_at(p, slope, shift, light))
    {
        float3 plain = wallpaper(p);
        return darkened ? prompt_floor(plain) : plain;
    }
    float3 bent = float3(wallpaper(p + shift.r * slope).r, wallpaper(p + shift.g * slope).g,
                         wallpaper(p + shift.b * slope).b);
    return wake_lit(darkened ? prompt_floor(bent) : bent, light);
}

/// The prompt's pane at `p`, over `floor`, the darkened wallpaper there
/// (`prompt_floor`): the panel, its four rows. Held (no ask yet) the panel
/// has its title and line and no field: the same size, so nothing moves when
/// the ask comes.
float3 draw_prompt(CanvasPrompt pr, float3 floor, float2 p)
{
    float4 col = float4(floor, 1.0);
    PromptLayout l = private[0].prompt;
    if (pr.state == PROMPT_IDLE || any(p < l.panel.xy) || any(p >= l.panel.zw))
    {
        return col.rgb;
    }
    col = ink(PROMPT_PANEL_COLOUR, rounded(l.panel, l.radius, p), col);
    bool asked = pr.state != PROMPT_HELD;
    if (asked)
    {
        col = ink(PROMPT_FIELD_COLOUR, rounded(l.field, 0.5 * l.radius, p), col);
    }

    // The title's lines: line k is the glyphs from its start to the next
    // line's, moved left by its start's pen position; the last runs to the
    // end of the block.
    {
        float title_lh = text_line_height(l.title_px);
        uint start = 0u;
        float shift = 0.0;
        for (uint k = 0u; k < l.title_lines; ++k)
        {
            uint end = k < 4u && k + 1u < l.title_lines ? l.title_starts[k] : 0xffffffffu;
            float2 org = float2(l.title_box.x, l.title_box.y + float(k) * title_lh);
            col = paint_glyphs(BLOCK_TITLE, start, end, shift, org, l.title_px, l.clip, PROMPT_INK,
                               p, col);
            if (end == 0xffffffffu)
            {
                break;
            }
            start = end;
            shift = l.title_shifts[k];
        }
    }
    col = paint_text(BLOCK_ASK, l.ask_box.xy, l.text_px, l.clip, PROMPT_MUTED_INK, p, col);

    // The answer, inside the field: the dots of an echo-off ask, the text of
    // an echo-on one, its end kept in view when it is longer than the field.
    // Verifying, they stay as they were, in the muted ink.
    float s = float(max(params.scale, 1u)) / 120.0;
    float fpad = PROMPT_FIELD_PADDING * s;
    float2 inside = float2(l.field.x + fpad, l.field.y + fpad);
    float inner_right = l.field.z - fpad;
    float3 typed_ink = pr.state == PROMPT_VERIFYING ? PROMPT_MUTED_INK : PROMPT_INK;
    if (asked && p.x >= l.field.x && p.x < l.field.z)
    {
        if (pr.echo != 0u && p.x >= inside.x)
        {
            float overflow = max(text_width(BLOCK_ECHO, l.text_px) - (inner_right - inside.x), 0.0);
            col = paint_text(BLOCK_ECHO, float2(inside.x - overflow, inside.y), l.text_px,
                             inner_right, typed_ink, p, col);
        }
        else
        {
            col = paint_dots(pr.typed, inside, l.text_px, PROMPT_DOT_GAP * s, inner_right,
                             typed_ink, p, col);
        }
    }

    float3 line_ink = pr.line_error != 0u ? PROMPT_ERROR_INK : PROMPT_MUTED_INK;
    col = paint_text(BLOCK_LINE, l.line_org, l.text_px, l.clip, line_ink, p, col);
    return col.rgb;
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
    CanvasPrompt pr = facts[0].prompt;
    if (surface_target() == TARGET_LOCK)
    {
        // The prompt's look, the water over its floor and the panel over
        // the water.
        dst.Store(gid.xy, float4(draw_prompt(pr, through_water(p, true), p), 1.0));
        return;
    }
    if (surface_target() == TARGET_BACKGROUND)
    {
        // Opaque: the background layer is the bottom of the output.
        dst.Store(gid.xy, float4(through_water(p, false), 1.0));
        return;
    }
    float3 colour = wallpaper(p);
    if (surface_target() == TARGET_PROMPT)
    {
        dst.Store(gid.xy, float4(draw_prompt(pr, prompt_floor(colour), p), 1.0));
        return;
    }
    if (surface_target() == TARGET_OVERLAY && pr.state != PROMPT_IDLE)
    {
        // A prompt is up over the dim: the pane under this overlay is the
        // one to see, so the dim stands aside while it is, and is as it was
        // when the prompt goes.
        dst.Store(gid.xy, float4(0.0, 0.0, 0.0, 0.0));
        return;
    }
    if (surface_target() == TARGET_OVERLAY)
    {
        // The wallpaper, darkened, at the dim's alpha, premultiplied:
        // transparent where the alpha is 0, so a dim on its way in shows
        // the windows under it.
        float d = saturate(private[0].dim);
        float3 dimmed = lerp(colour, DIM_COLOUR, saturate(DIM_DARKEN));
        dst.Store(gid.xy, float4(dimmed * d, d));
        return;
    }
    dst.Store(gid.xy, float4(0.0, 0.0, 0.0, 0.0));
}
