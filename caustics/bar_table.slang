// The item and block tables' machinery: what `bar_build` and every subject's
// build fill them with. The four allocators, a text block out of any of the
// three buffers, an icon by name (the stitch), the names a build wants asked
// about, the answers arena indexed, and the bytes the caustic composes for
// itself.
//
// The build's own entry point is `bar.slang`'s; what each subject builds is
// the subject's.

module bar_table;

import bar_abi;
import bar_wire;
import bar_model;
import bar_tunables;
import shape;
import bar_state;

/// One composition under construction: bytes and how many.
public struct Composed
{
    public uint bytes[COMPOSE_MAX_BYTES];
    public uint len;
};

/// An empty one. Written out in full rather than left to a default, because a
/// partially initialised local is exactly the sort of thing that reads
/// differently on two drivers.
public Composed compose_new()
{
    Composed c;
    for (uint i = 0u; i < COMPOSE_MAX_BYTES; ++i)
    {
        c.bytes[i] = 0u;
    }
    c.len = 0u;
    return c;
}

public void compose_byte(inout Composed c, uint byte)
{
    if (c.len < COMPOSE_MAX_BYTES)
    {
        c.bytes[c.len] = byte & 0xffu;
        c.len += 1u;
    }
}

/// A literal of this caustic's own string table, by `getStringHash` value. A
/// hash the pool cannot answer for appends nothing — the honest answer, and
/// the same answer a missing dock-menu literal gets in `bar_build`.
public void compose_literal(inout Composed c, uint hash)
{
    Literal lit = literal(hash);
    // Capped at what the composition could hold anyway. `compose_byte` already
    // refuses to write past the end; the cap is on the *loop*, so a length word
    // that arrived wrong cannot spin one thread of the GPU for four billion
    // iterations with a device-lost at the end of it.
    uint len = min(lit.len, COMPOSE_MAX_BYTES);
    for (uint i = 0u; i < len; ++i)
    {
        compose_byte(c, utf8_byte(lit.off + i));
    }
}

/// `value` in decimal, at least `min_digits` wide, zero-padded.
public void compose_number(inout Composed c, uint value, uint min_digits)
{
    uint digits[10];
    uint n = 0u;
    uint rest = value;
    for (uint i = 0u; i < 10u; ++i)
    {
        digits[i] = 0u;
    }
    while (rest > 0u && n < 10u)
    {
        digits[n] = rest % 10u;
        rest /= 10u;
        n += 1u;
    }
    while (n < max(min_digits, 1u) && n < 10u)
    {
        digits[n] = 0u;
        n += 1u;
    }
    for (uint i = n; i > 0u; --i)
    {
        compose_byte(c, 0x30u + digits[i - 1u]);
    }
}

/// The bytes into the block's reserved, word-aligned range. Returns how many
/// landed, which is what goes back into the block's length.
public uint compose_store(uint byte_offset, Composed c)
{
    uint words;
    uint stride;
    shapeUtf8.GetDimensions(words, stride);
    uint limit = min(words, MAX_STRING_WORDS);
    uint base = byte_offset >> 2;
    uint len = min(c.len, COMPOSE_MAX_BYTES);
    uint nwords = (len + 3u) / 4u;
    for (uint w = 0u; w < nwords; ++w)
    {
        if (base + w >= limit)
        {
            return w * 4u;
        }
        uint v = 0u;
        for (uint k = 0u; k < 4u; ++k)
        {
            uint i = w * 4u + k;
            v |= (i < len ? c.bytes[i] : 0u) << (k * 8u);
        }
        shapeUtf8[base + w] = v;
    }
    return len;
}

/// How many names one build may ask about. The eight-deep list is the
/// caustic's, and so is the preference that fills it: a glyph question stops
/// an icon being drawn at all, where a pixel question is a fallback for a name
/// that already came back "not curves" — so a full list gives a glyph question
/// a pixel question's place rather than starving behind a name nothing will
/// ever answer for. The embassy cannot dedup a want it never saw.
public static const uint WANT_LIST = 8u;

/// Words of the image slots this build drew, one bit a slot — what an
/// `ACT_IMAGES_DRAWN` carries (docs/abi.md, *The action ring*). The embassy ages its
/// slots by this mask and nothing else, because only this file knows which
/// pictures are on the bar: a slot the build stopped drawing ages toward
/// reuse, and one still drawn is never evicted under it.
public static const uint DRAWN_WORDS = (MAX_IMAGE_SLOTS + 31u) / 32u;

/// The four allocators and the two witnesses of one build. Passed `inout`
/// through everything below, because one thread and no statics is the whole
/// shape of this pass (a `static` in a compute dispatch is re-zeroed every run
/// without saying so).
public struct Build
{
    public uint items;      // next item index
    public uint blocks;     // next block index
    public uint glyphs;     // next positioned-glyph slot
    public uint reserved;   // reservations taken from the caustic's own byte region
    public uint dropped;    // items there was no room for
    public uint wants;      // names listed for an ACT_WANT_ICON
    public uint want_key[WANT_LIST];
    public uint want_src[WANT_LIST];
    public uint want_off[WANT_LIST];
    public uint want_len[WANT_LIST];
    public uint want_req[WANT_LIST];
    public uint drawn[DRAWN_WORDS]; // image slots placed in a BLOCK_IMAGE
};

public Build build_new()
{
    Build b;
    b.items = 0u;
    b.blocks = 0u;
    b.glyphs = 0u;
    b.reserved = 0u;
    b.dropped = 0u;
    b.wants = 0u;
    for (uint i = 0u; i < WANT_LIST; ++i)
    {
        b.want_key[i] = 0u;
        b.want_src[i] = SRC_STRINGS;
        b.want_off[i] = 0u;
        b.want_len[i] = 0u;
        b.want_req[i] = 0u;
    }
    for (uint w = 0u; w < DRAWN_WORDS; ++w)
    {
        b.drawn[w] = 0u;
    }
    return b;
}

// -- the allocators ---------------------------------------------------------

/// Glyph slots a text block of `bytes` bytes is allocated — **`Batch::push`'s
/// rule**, which is four a byte plus eight, capped at MAX_RUN_GLYPHS.
///
/// Not one slot per byte, because shaping *does* add glyphs: a MultipleSubst
/// adds them, and a SequenceContext whose nested lookup is one turned two
/// input glyphs into five in HarfBuzz's corpus. A block's slots are allocated
/// here and the next block starts where this one was promised to end, so at
/// one a byte a Latin label that multiplied past its byte count would come
/// back SHAPE_RUN_TRUNCATED, which is safe and wrong.
///
/// With this rule truncation is a **bug rather than a size**, and `bar_shape`
/// witnesses the block that hit it.
public uint text_slots(uint bytes)
{
    return min(bytes * 4u + 8u, MAX_RUN_GLYPHS);
}

/// Glyph slots a **body** block is allocated: one a byte
/// plus eight. A body is hundreds of bytes where a label is tens, forty rows
/// of them are the bar's whole glyph budget at four a byte, and a body block
/// whose substitutions outgrow this comes back SHAPE_RUN_TRUNCATED and is
/// drawn as a cut — the ellipsis — rather than witnessed as a bug.
public uint body_slots(uint bytes)
{
    return min(bytes + 8u, MAX_RUN_GLYPHS);
}

/// The slots a block was allocated, by the rule it was allocated under. The
/// shape pass may fill exactly these: the next block's run starts after them.
public uint block_slots(TextBlock blk)
{
    return (blk.bflags & BLOCK_BODY) != 0u ? body_slots(blk.b) : text_slots(blk.b);
}

/// Append an item. Full is counted in `Private.built_dropped`, which
/// `bar_build` witnesses as one `items: table capped` line, and a missing
/// item, never a panic.
public uint build_push(inout Build b, Item it)
{
    if (b.items >= MAX_ITEMS)
    {
        b.dropped += 1u;
        return NO_INDEX;
    }
    private[0].items[b.items] = it;
    b.items += 1u;
    return b.items - 1u;
}

/// A run of one of the three buffers as a block: the bytes are already there,
/// put there by whoever wrote the thing that names them — the embassy for a
/// title or a literal, a sidecar for a row's label or an app's name.
///
/// `source` is `SRC_STRINGS`, `SRC_ANSWERS` or a sidecar slot; `bar_shape`
/// hands `shapeRun` the matching buffer (docs/abi.md, *The arenas*).
///
/// The slots come from `text_slots` — four a byte plus eight — and out of
/// blocks or out of slots is NO_INDEX and a missing label, never a truncated
/// string drawn as if it were whole.
public uint build_text(inout Build b, uint source, uint off, uint len, uint px, float4 ink, uint hash)
{
    return build_text_as(b, source, off, len, px, ink, hash, 0u);
}

/// `build_text` with the block's `bflags`: BLOCK_BODY allocates `body_slots`
/// rather than `text_slots`.
public uint build_text_as(inout Build b, uint source, uint off, uint len, uint px, float4 ink,
                   uint hash, uint bflags)
{
    if (len == 0u || px == 0u)
    {
        return NO_INDEX;
    }
    uint slots = (bflags & BLOCK_BODY) != 0u ? body_slots(len) : text_slots(len);
    if (b.blocks >= MAX_BLOCKS || b.glyphs + slots > MAX_GLYPHS)
    {
        return NO_INDEX;
    }
    TextBlock blk;
    blk.size = float2(0.0, 0.0);
    blk.kind = BLOCK_TEXT;
    blk.flags = 0u;
    blk.a = off;
    blk.b = len;
    blk.first_glyph = b.glyphs;
    blk.glyph_count = 0u;
    blk.color = ink;
    blk.px = float(px);
    blk.hash = hash;
    blk.bflags = bflags;
    blk.source = source;
    private[0].blocks[b.blocks] = blk;
    b.glyphs += slots;
    b.blocks += 1u;
    return b.blocks - 1u;
}

/// One glyph or one image slot, filling a square box of `px`.
public uint build_icon(inout Build b, uint kind, uint payload, float px, float4 ink)
{
    if (b.blocks >= MAX_BLOCKS || px <= 0.0)
    {
        return NO_INDEX;
    }
    TextBlock blk;
    blk.size = float2(px, px);
    blk.kind = kind;
    blk.flags = 0u;
    blk.a = payload;
    blk.b = 0u;
    blk.first_glyph = 0u;
    blk.glyph_count = 0u;
    blk.color = ink;
    blk.px = px;
    blk.hash = 0u;
    blk.bflags = 0u;
    blk.source = SRC_STRINGS;
    private[0].blocks[b.blocks] = blk;
    b.blocks += 1u;
    // A picture on the bar: its slot is in use, which the embassy hears once
    // per build (`ACT_IMAGES_DRAWN`) and ages its slots by.
    if (kind == BLOCK_IMAGE && payload < MAX_IMAGE_SLOTS)
    {
        b.drawn[payload >> 5u] |= 1u << (payload & 31u);
    }
    return b.blocks - 1u;
}

/// COMPOSE_MAX_BYTES of the region the embassy zeroed and never writes again,
/// word-aligned by construction, or NO_INDEX when they are all taken.
public uint build_reserve(inout Build b)
{
    if (b.reserved >= MAX_COMPOSED)
    {
        return NO_INDEX;
    }
    uint off = facts[0].header.compose_base + b.reserved * COMPOSE_MAX_BYTES;
    b.reserved += 1u;
    return off;
}

/// A block whose bytes `bar_compose` writes: the reservation, and the name of
/// the composition that fills it (docs/abi.md, *The facts*).
public uint build_composed(inout Build b, uint px, uint compose)
{
    if (px == 0u || b.blocks >= MAX_BLOCKS)
    {
        return NO_INDEX;
    }
    // The reservation is the whole region's worth, because `bar_compose` has
    // not written its bytes yet and `blk.b` is 0 until it does.
    uint slots = text_slots(COMPOSE_MAX_BYTES);
    if (b.glyphs + slots > MAX_GLYPHS)
    {
        return NO_INDEX;
    }
    uint off = build_reserve(b);
    if (off == NO_INDEX)
    {
        return NO_INDEX;
    }
    TextBlock blk;
    blk.size = float2(0.0, 0.0);
    blk.kind = BLOCK_TEXT;
    blk.flags = 0u;
    blk.a = off;
    blk.b = 0u;
    blk.first_glyph = b.glyphs;
    blk.glyph_count = 0u;
    blk.color = LABEL_INK;
    blk.px = float(px);
    blk.hash = compose;
    blk.bflags = BLOCK_COMPOSED;
    blk.source = SRC_STRINGS;
    private[0].blocks[b.blocks] = blk;
    b.glyphs += slots;
    b.blocks += 1u;
    return b.blocks - 1u;
}

// -- the text a fact carries ------------------------------------------------

/// Whether a byte is ASCII whitespace, for the trim below.
public bool build_is_space(uint c)
{
    return c == 0x20u || c == 0x09u || c == 0x0bu || c == 0x0cu;
}

/// The first line of a run of bytes, trimmed: dbusmenu rows and window titles
/// both arrive multi-line on the live bus and a row is one line high. The
/// range shrinks; no byte moves.
public void build_first_line(uint source, uint off, uint len, out uint out_off, out uint out_len)
{
    uint start = off;
    uint end = off + len;
    for (uint i = off; i < end; ++i)
    {
        uint c = src_byte(source, i);
        if (c == 0x0au || c == 0x0du)
        {
            end = i;
            break;
        }
    }
    while (start < end && build_is_space(src_byte(source, start)))
    {
        start += 1u;
    }
    while (end > start && build_is_space(src_byte(source, end - 1u)))
    {
        end -= 1u;
    }
    out_off = start;
    out_len = end - start;
}

/// A chip's placeholder: the first character of a name, uppercased, as a text
/// block in the icon's square.
///
/// Two callers, one rule each. A dock entry passes its **app id** with
/// `last_component` set, and wears the initial of the id's last dot-component
/// (`org.mozilla.firefox` → `F`). A tray item passes its **title** — the label
/// of a `RFLAG_LABEL_TITLE` row, in the slot's region of the rows arena — and
/// wears the title's first character. No character at
/// all is `?`, so a chip whose name nothing answers is never empty.
///
/// **ASCII only**, deliberately: only an ASCII letter is uppercased, which
/// affects exactly one thing — the letter a chip
/// wears while nothing has answered for its icon, and only when that initial
/// is non-ASCII. The bytes are this pass's own: it takes a reservation from
/// the caustic's region and writes them, because an uppercased byte is a byte
/// the embassy never wrote.
public uint build_letter(inout Build b, uint source, uint off, uint len, bool last_component, uint px)
{
    uint start = off;
    if (last_component)
    {
        for (uint i = off; i < off + len; ++i)
        {
            if (src_byte(source, i) == 0x2eu) // '.'
            {
                start = i + 1u;
            }
        }
    }
    Composed c = compose_new();
    if (start >= off + len)
    {
        compose_byte(c, 0x3fu); // '?'
    }
    else
    {
        uint lead = src_byte(source, start);
        uint bytes = 1u;
        if (lead >= 0xf0u)      { bytes = 4u; }
        else if (lead >= 0xe0u) { bytes = 3u; }
        else if (lead >= 0xc0u) { bytes = 2u; }
        bytes = min(bytes, off + len - start);
        if (bytes == 1u && lead >= 0x61u && lead <= 0x7au)
        {
            lead -= 32u;
        }
        compose_byte(c, lead);
        for (uint k = 1u; k < bytes; ++k)
        {
            compose_byte(c, src_byte(source, start + k));
        }
    }
    uint at = build_reserve(b);
    if (at == NO_INDEX)
    {
        return NO_INDEX;
    }
    uint wrote = compose_store(at, c);
    return build_text(b, SRC_STRINGS, at, wrote, px, ICON_INK, 0u);
}

// -- what this build wants asked ------------------------------------------

/// List a name nothing has answered for, as the `(offset, length, REQ_*,
/// source)` an `ACT_WANT_ICON` carries (docs/abi.md, *The action ring*).
///
/// **Bytes and the buffer they are in.** A name lives in one of three places —
/// a literal in `strings`, a row's `icon` in its slot's region of the rows
/// arena, an `AppBody.icon` in `answers` — and a range alone cannot say which,
/// so the source travels with it and the embassy reads its own copy of that
/// buffer. `key` is this file's private hash of the same bytes, and it is only
/// ever the dedup within one build.
///
/// A glyph question stops an icon being drawn at all; a pixel question is a
/// fallback for a name that already came back "not curves". So when the list
/// is full a glyph question takes a pixel question's place, rather than
/// starving behind a name nothing will ever answer for. Which wants fit is the
/// caustic's to decide, because the embassy cannot dedup a want it never saw;
/// asking twice for the same name costs nothing, since the embassy asks once
/// per name and remembers (docs/abi.md, *Request*).
public void build_want(inout Build b, uint key, uint source, uint off, uint len, uint req)
{
    if (len == 0u || req == 0u)
    {
        return;
    }
    for (uint i = 0u; i < b.wants; ++i)
    {
        if (b.want_key[i] == key)
        {
            return;
        }
    }
    if (b.wants < WANT_LIST)
    {
        b.want_key[b.wants] = key;
        b.want_src[b.wants] = source;
        b.want_off[b.wants] = off;
        b.want_len[b.wants] = len;
        b.want_req[b.wants] = req;
        b.wants += 1u;
        return;
    }
    if (req == REQ_ICON)
    {
        return;
    }
    for (uint i = 0u; i < WANT_LIST; ++i)
    {
        if (b.want_req[i] == REQ_ICON)
        {
            b.want_key[i] = key;
            b.want_src[i] = source;
            b.want_off[i] = off;
            b.want_len[i] = len;
            b.want_req[i] = req;
            return;
        }
    }
}

// -- the answers arena, indexed (docs/abi.md, *The arenas*) ----------------------

/// Walk the arena once and write `(hash of the echoed key, byte offset)` for
/// every answer into `private`. Called at the top of `bar_build`.
///
/// The glyph bodies come first and the app bodies from `glyph_bytes`, because
/// a body carries no kind word of its own: where an entry *is* says what it
/// is. Each entry is its head, then its bytes, padded to a word.
public void index_answers()
{
    uint live = facts[0].header.answer_bytes;
    uint split = min(facts[0].header.glyph_bytes, live);
    uint count = 0u;
    uint at = 0u;
    // The glyph half: `GlyphBody` then `name_len` bytes.
    while (at + 16u <= split && count < MAX_ANSWERS)
    {
        uint name_len = answer_byte_word(at + 8u);
        uint next = at + 16u + ((name_len + 3u) & ~3u);
        if (name_len == 0u || next > split)
        {
            break;
        }
        private[0].answer_hash[count] = hash_bytes(SRC_ANSWERS, at + 16u, name_len);
        private[0].answer_at[count] = at;
        count += 1u;
        at = next;
    }
    // The app half: `AppBody` then the five fields' bytes in order.
    at = split;
    while (at + 32u <= live && count < MAX_ANSWERS)
    {
        uint app_len = answer_byte_word(at + 4u);
        uint fields = app_len + answer_byte_word(at + 8u) + answer_byte_word(at + 12u)
                      + answer_byte_word(at + 16u) + answer_byte_word(at + 20u);
        uint next = at + 32u + ((fields + 3u) & ~3u);
        if (app_len == 0u || next > live)
        {
            break;
        }
        private[0].answer_hash[count] = hash_bytes(SRC_ANSWERS, at + 32u, app_len);
        private[0].answer_at[count] = at;
        count += 1u;
        at = next;
    }
    private[0].answer_count = count;
}

/// Where the answer for `hash` begins, or NO_INDEX. `glyph` says which half to
/// look in, because an icon name and an app id are different questions and a
/// hash could in principle answer both.
public uint answer_of(uint hash, bool glyph)
{
    uint split = facts[0].header.glyph_bytes;
    uint n = min(private[0].answer_count, MAX_ANSWERS);
    for (uint i = 0u; i < n; ++i)
    {
        if (private[0].answer_hash[i] != hash)
        {
            continue;
        }
        uint at = private[0].answer_at[i];
        if ((at < split) == glyph)
        {
            return at;
        }
    }
    return NO_INDEX;
}

// -- the stitch -------------------------------------------------------------

/// One themed icon name — a literal's bytes, a row's `icon` range, an
/// `AppBody.icon` — as a block: curves if `caustics-glyphs` has them, pixels if
/// `caustics-apps` does, **nothing for one round trip** while an answer is
/// pending, and `stop = false` when no theme has it at all, which is what
/// makes a preference list fall through to its next name.
///
/// A list is a preference order, and settling on a later candidate before the
/// earlier one has answered would take a slot for the wrong picture — which is
/// what `stop` is for.
public uint build_stitch_named(inout Build b, uint source, uint off, uint len, float px, out bool stop)
{
    stop = true;
    if (len == 0u)
    {
        return NO_INDEX;
    }
    uint key = hash_bytes(source, off, len);
    uint at = answer_of(key, true);
    if (at == NO_INDEX)
    {
        // Never asked, or asked and not yet answered. The embassy asks once
        // per name and remembers (docs/abi.md, *Request*), so repeating this costs nothing.
        build_want(b, key, source, off, len, REQ_GLYPH);
        return NO_INDEX;
    }
    uint flags = answer_byte_word(at + 4u);
    if ((flags & GLYPH_MISSING) == 0u)
    {
        return build_icon(b, BLOCK_GLYPH, answer_byte_word(at), px, ICON_INK);
    }
    // Not curves. The pixels may already be in a slot under this name, which
    // is what a `REQ_ICON` answer is.
    uint slot = image_by_name(source, off, len);
    if (slot != NO_INDEX)
    {
        return build_icon(b, BLOCK_IMAGE, slot, px, IMAGE_INK);
    }
    build_want(b, key, source, off, len, REQ_ICON);
    stop = false;
    return NO_INDEX;
}

/// The same, for a name the *tunables* gave: its bytes are in the literal
/// pool.
public uint build_stitch_literal(inout Build b, uint hash, float px, out bool stop)
{
    Literal lit = literal(hash);
    return build_stitch_named(b, SRC_STRINGS, lit.off, lit.len, px, stop);
}

/// How many bytes the UTF-8 character led by `lead` takes.
public uint utf8_char_len(uint lead)
{
    if (lead >= 0xf0u) { return 4u; }
    if (lead >= 0xe0u) { return 3u; }
    if (lead >= 0xc0u) { return 2u; }
    return 1u;
}

/// A row's icon: a **name** whose bytes are in the slot's own region of the
/// rows arena (docs/abi.md, *Pixmap icons*) — a themed one, or a `Pixmap` key the sidecar sent
/// before the frame that names it (docs/abi.md, *Pixmap*). Either way the stitch resolves it:
/// curves if a theme has them, else the slot the key's pixels landed in.
public uint build_row_icon(inout Build b, uint slot, Row row, uint icon_px_u)
{
    if (row.icon_len == 0u)
    {
        return NO_INDEX;
    }
    bool stop = true;
    return build_stitch_named(b, slot, rows_blob(slot) + row.icon_off, row.icon_len,
                              float(icon_px_u), stop);
}

/// A row's label, first line only, at the size its item kind asks for. The
/// bytes are the sidecar's own, in its region of the rows arena.
public uint build_row_label(inout Build b, uint slot, Row row, uint px)
{
    if (row.label_len == 0u)
    {
        return NO_INDEX;
    }
    uint off;
    uint len;
    build_first_line(slot, rows_blob(slot) + row.label_off, row.label_len, off, len);
    return build_text(b, slot, off, len, px, LABEL_INK, 0u);
}

/// Whether a byte ends a line of a body: LF, or the CR of a CRLF.
public bool build_is_newline(uint c)
{
    return c == 0x0au || c == 0x0du;
}

/// Everything the item table depends on in one slot's region, hashed: every
/// word of every row but `value`, and the label and icon bytes their ranges
/// name (docs/abi.md, *The arenas*).
///
/// **The embassy does not make this comparison.** It writes a `Rows` frame's
/// body verbatim and sends one `CMD_ROWS`; whether that is a rebuild or a
/// value-only change is decided here. A
/// slider tick moves `value` and nothing else, so the hash is unchanged and
/// the frame costs no model pass at all.
///
/// A pixmap icon is a content-addressed key (docs/abi.md, *Pixmap*), so a *changed* pixmap is
/// a changed name in the blob and moves this hash like any other icon.
public uint rows_shape_of(uint slot)
{
    RowsHeader head = rows_header(slot);
    uint blob = rows_blob(slot);
    uint h = FNV_OFFSET;
    h = (h ^ head.applet) * FNV_PRIME;
    h = (h ^ head.row_count) * FNV_PRIME;
    for (uint r = 0u; r < head.row_count; ++r)
    {
        Row row = row_at(slot, r);
        h = (h ^ row.id) * FNV_PRIME;
        h = (h ^ row.parent) * FNV_PRIME;
        h = (h ^ row.role) * FNV_PRIME;
        h = (h ^ row.flags) * FNV_PRIME;
        h = (h ^ hash_bytes(slot, blob + row.label_off, row.label_len)) * FNV_PRIME;
        h = (h ^ hash_bytes(slot, blob + row.icon_off, row.icon_len)) * FNV_PRIME;
        // The body, as far as `build_body` can look, a
        // word at a time — it is kilobytes where a label is tens of bytes —
        // with its whole length folded in.
        h = (h ^ hash_region_words(slot, blob + row_body_off(row),
                                   min(row_body_len(row), 2u * BODY_SHAPE_BYTES))) * FNV_PRIME;
        h = (h ^ row_body_len(row)) * FNV_PRIME;
        // `min` and `max` are a slider's range and not its position, so they
        // are shape: a volume slider that changed its units is a new row.
        h = (h ^ asuint(row.min)) * FNV_PRIME;
        h = (h ^ asuint(row.max)) * FNV_PRIME;
    }
    // Never zero, which is what `private` holds before any frame has arrived,
    // so the first `CMD_ROWS` for a slot is always a rebuild.
    return h == 0u ? FNV_PRIME : h;
}

/// Whether a sidecar's region the embassy rewrote this frame (`rewritten`,
/// one bit a slot) is a rebuild (docs/abi.md, *The arenas*).
///
/// The embassy copied bytes and said which slot; whether that is a rebuild
/// is this file's, because it is the only one that knows what the item
/// table was built from. A shape it has already seen is one `write_buffer`
/// and this frame, and nothing else.
public bool rows_reshaped(uint rewritten)
{
    bool rebuild = false;
    for (uint slot = 0u; slot < MAX_SLOTS; ++slot)
    {
        if ((rewritten & (1u << slot)) == 0u)
        {
            continue;
        }
        uint shape = rows_shape_of(slot);
        if (shape != private[0].rows_shape[slot])
        {
            private[0].rows_shape[slot] = shape;
            rebuild = true;
        }
    }
    return rebuild;
}

/// The shaper's notdef runs, and the bytes behind them.
///
/// `bar_shape` flags a run whose codepoints the merged font lacks; this
/// reads the blocks back and says so once per change, and asks the curves
/// sidecar to cover the **bytes** of each flagged text block whose bytes
/// are the embassy's (docs/abi.md, *The action ring*). Still a witness: nothing
/// here waits for the answer or draws differently because of it.
public void witness_shaping()
{
    uint notdefs = 0u;
    uint texts = 0u;
    uint truncated = NO_INDEX;
    uint stalled = NO_INDEX;
    uint hash = 2166136261u; // FNV-1a 32 over the *set* of flagged blocks
    uint nblocks = live_blocks();
    for (uint b = 0u; b < nblocks; ++b)
    {
        if (private[0].blocks[b].kind != BLOCK_TEXT)
        {
            continue;
        }
        texts += 1u;
        if ((private[0].blocks[b].flags & SHAPE_RUN_NOTDEF) != 0u)
        {
            notdefs += 1u;
            hash = (hash ^ b) * 16777619u;
        }
        // A body block that ran out of its slots is a cut, drawn as one
        // (`body_slots`); the deadline is still the deadline.
        uint cut_flags = (private[0].blocks[b].bflags & BLOCK_BODY) != 0u
                         ? SHAPE_RUN_DEADLINE : (SHAPE_RUN_TRUNCATED | SHAPE_RUN_DEADLINE);
        if ((private[0].blocks[b].flags & cut_flags) != 0u && truncated == NO_INDEX)
        {
            truncated = b;
        }
        if ((private[0].blocks[b].flags & SHAPE_RUN_DEADLINE) != 0u)
        {
            stalled = b;
        }
    }

    // -- a run that ran out of slots ----------------------------------------
    //
    // `bar_build` gives a block `text_slots(bytes)` — four a byte plus eight,
    // `Batch::push`'s own rule — so a substitution that grows a Latin label
    // fits, and a run that is still truncated is a bug in the slot rule or the shaper
    // rather than a label that was always too long. It
    // says so once per change, with the block and what it was given.
    if (truncated != private[0].said_truncated)
    {
        private[0].said_truncated = truncated;
        if (truncated != NO_INDEX && stalled == NO_INDEX)
        {
            witness2(WITNESS_WARN,
                     getStringHash("shape: block {0} ran out of glyph slots ({1} bytes)"),
                     truncated, private[0].blocks[truncated].b);
        }
        // The wall-clock deadline: `shapeRun` reads the device's realtime
        // clock (`VK_KHR_shader_clock`) and stops a run that passes it, the
        // guard against a runaway shader that holds even when the op
        // accounting is wrong. A run that hits
        // it drew short, and the only way that happens with a font the bar
        // actually loaded is an arena that is not one — so it says so.
        if (stalled != NO_INDEX)
        {
            witness1(WITNESS_WARN,
                     getStringHash("shape: block {0} passed the run deadline and stopped"),
                     stalled);
        }
    }
    if (notdefs != private[0].said_notdef)
    {
        private[0].said_notdef = notdefs;
        if (notdefs == 0u)
        {
            witness0(WITNESS_INFO, getStringHash("text: every run has its glyphs"));
        }
        else
        {
            witness1(WITNESS_INFO,
                     getStringHash("text: {0} runs have a glyph the font lacks"), notdefs);
        }
    }
    // **Every** run flagged is not a font missing a script: it is a font that
    // has not landed. The bar starts with no curves and no `cmap` at all (they
    // arrive from `caustics-glyphs` over its pipe),
    // so for the first few hundred milliseconds the shaper answers `.notdef`
    // for every codepoint there is, and asking the sidecar to cover the bar's
    // own Latin labels would be one wire frame and one remembered question
    // each for nothing. One drawable run is enough to make the flagged ones
    // mean what they say. The guard sits here, beside the loop that would
    // otherwise ask.
    if (notdefs > 0u && notdefs < texts && hash != private[0].notdef_hash)
    {
        private[0].notdef_hash = hash;
        for (uint b = 0u; b < nblocks; ++b)
        {
            TextBlock blk = private[0].blocks[b];
            if (blk.kind != BLOCK_TEXT || (blk.flags & SHAPE_RUN_NOTDEF) == 0u)
            {
                continue;
            }
            // A composed block's bytes are this file's — `bar_compose` wrote
            // them into the strings buffer on the GPU and the embassy's copy
            // is zeroes — so there is nothing at that range for it to send.
            if ((blk.bflags & BLOCK_COMPOSED) != 0u)
            {
                continue;
            }
            // Every other block's bytes are the embassy's, in whichever of
            // its three buffers the block was built from — a title or a
            // literal in `strings`, a sidecar's row label in its slot's
            // region of the rows arena, an app's name in `answers` — so the
            // range goes out with the block's own `source`, the word
            // `ACT_WANT_ICON` carries for the same reason (docs/abi.md, *The action ring*). A menu row
            // `🇦🇺 Australia` is a question like any title.
            act3(ACT_COVER, blk.a, blk.b, blk.source);
        }
    }
}

/// The sizes one build draws at: the icons' box, and each kind's TEXT_PX in
/// device px, snapped against the loaded font's cap height (`snap_px`).
/// Computed once per build, at its top, and nowhere else, so every block of
/// a kind is one size.
public struct BuildSizes
{
    public uint icon;
    public uint text[ITEM_KIND_COUNT];
};

public BuildSizes build_sizes()
{
    BuildSizes z;
    z.icon = uint(max(icon_px(), 1.0) + 0.5);
    for (uint k = 0u; k < ITEM_KIND_COUNT; ++k)
    {
        z.text[k] = snap_px(uint(max(TEXT_PX[k] * scale_px(), 1.0) + 0.5));
    }
    return z;
}

/// The one ellipsis every cut body ends in, shaped once at the body's size
/// `px`, when any row this build made has a body.
public void build_ellipsis(inout Build b, bool bodies, uint px)
{
    private[0].ellipsis_block = NO_INDEX;
    if (bodies)
    {
        Literal ell = literal(BODY_ELLIPSIS);
        private[0].ellipsis_block = build_text(b, SRC_STRINGS, ell.off, ell.len,
                                               px, BODY_INK, BODY_ELLIPSIS);
    }
}

/// What comes back from a build made for menu level `level`: the tables'
/// counts, the image slots it drew and the names it wants asked about.
public void build_publish(Build b, Id level)
{
    private[0].built_items = b.items;
    private[0].built_blocks = b.blocks;
    private[0].built_glyphs = b.glyphs;
    private[0].built_dropped = b.dropped;
    // The level this table was built for. `bar_apply` compares the live one
    // against it and pushes an `ACT_REBUILD` when they differ, which the
    // embassy answers exactly as it answers a clock tick.
    private[0].built_for = level;
    private[0].build_serial += 1u;

    // ---- the image slots this build drew ----
    //
    // Before the wants, so the embassy marks what is on the bar before it
    // serves a want that may take a slot from something else. Every build
    // says it, an empty mask included: a picture no longer drawn is then one
    // the slots may let go.
    Args drawn = args_new();
    for (uint w = 0u; w < DRAWN_WORDS; ++w)
    {
        arg(drawn, b.drawn[w]);
    }
    push_action(ACT_IMAGES_DRAWN, drawn);

    // ---- the names this build wants resolved ----
    //
    // One action per name, with the bytes and the `REQ_*` (docs/abi.md
    // *The action ring*). The embassy reads the range, writes a `Request` and remembers the
    // name; a want repeated by the next build costs nothing.
    for (uint w = 0u; w < b.wants; ++w)
    {
        act4(ACT_WANT_ICON, b.want_off[w], b.want_len[w], b.want_req[w], b.want_src[w]);
    }
}

/// The build's own witnesses, each against what it last said: its size, and
/// the items it had no room for.
public void witness_build(Build b)
{
    if (b.items != private[0].said_built_items
        || b.blocks != private[0].said_built_blocks
        || b.glyphs != private[0].said_built_glyphs)
    {
        private[0].said_built_items = b.items;
        private[0].said_built_blocks = b.blocks;
        private[0].said_built_glyphs = b.glyphs;
        witness3(WITNESS_DEBUG,
                 getStringHash("built: {0} items, {1} blocks, {2} glyphs"),
                 b.items, b.blocks, b.glyphs);
    }
    if (b.dropped != private[0].said_dropped)
    {
        private[0].said_dropped = b.dropped;
        if (b.dropped > 0u)
        {
            witness2(WITNESS_WARN,
                     getStringHash("items: table capped at {0} ({1} dropped); some of the bar is not drawn"),
                     MAX_ITEMS, b.dropped);
        }
    }
}

/// Block `b`'s text, shaped (`bar_shape`'s one thread): its UTF-8 decoded,
/// mapped through the font's `cmap`, the ligatures fired and the pair kerns
/// applied, `(gid, x, y)` per glyph into `runs`, and the run's advance and
/// line box into the block's own `size`. A BLOCK_GLYPH or a BLOCK_IMAGE is
/// left exactly as it arrived.
public void shape_block(uint b)
{
    if (b >= live_blocks())
    {
        return;
    }
    TextBlock blk = private[0].blocks[b];
    if (blk.kind != BLOCK_TEXT)
    {
        return;
    }

    uint capacity;
    uint stride;
    runs.GetDimensions(capacity, stride);
    uint limit = min(capacity, MAX_GLYPHS);
    if (blk.first_glyph >= limit || blk.px <= 0.0)
    {
        private[0].blocks[b].size = float2(0.0, 0.0);
        private[0].blocks[b].glyph_count = 0u;
        private[0].blocks[b].flags = SHAPE_RUN_TRUNCATED;
        return;
    }

    // Which buffer this block's bytes are in (docs/abi.md, *The arenas*). The
    // shaper takes its text through `ShapeTables.utf8`, so handing it the
    // arena is the whole of what it takes to shape a sidecar's row label or an
    // app's name out of bytes the embassy never transcribed.
    //
    // **A selector and not an assignment**: under SPIR-V's logical addressing a
    // resource handle cannot be a conditionally written local, so `t.utf8 = …`
    // inside an `if` is a module spirv-val rejects. Three buffers would mean
    // three `shapeRun` call sites — and Slang, which must inline every
    // function taking a descriptor, emits the whole OpenType shaper once per
    // call: three times was 5.4 MB of SPIR-V and 5.3 s of compile for this
    // one entry point. The shaper reads its bytes through a selector, so
    // there is one call.
    uint start = blk.a;
    uint limit_bytes = facts[0].header.string_words * 4u;
    if (blk.source == SRC_ANSWERS)
    {
        limit_bytes = facts[0].header.answer_bytes;
    }
    else if (blk.source < MAX_SLOTS)
    {
        start += blk.source * ROWS_ARENA_BYTES;
        limit_bytes = (blk.source + 1u) * ROWS_ARENA_BYTES;
    }

    ShapeRequest req;
    req.byte_start = start;
    req.byte_len = min(blk.b, limit_bytes > start ? limit_bytes - start : 0u);
    req.px_size = blk.px;
    req.out_start = blk.first_glyph;
    // `bar_build` allocated `block_slots(blk)` for this block (`text_slots` for a
    // label, `body_slots` for a body), and the same expression here is what
    // the shaper may fill. A composed block was
    // reserved at COMPOSE_MAX_BYTES and `blk.b` is what `bar_compose` actually
    // wrote, which is never more — so one expression covers both.
    req.out_cap = min(block_slots(blk), limit - blk.first_glyph);
    // The bar forces no feature: the default shaper's set is what its chrome
    // wants, and the feature seam is for the test harness.
    req.feature_start = 0u;
    req.feature_count = 0u;
    req._rsvd0 = 0u;

    uint source = 0u;
    if (blk.source == SRC_ANSWERS)
    {
        source = 2u;
    }
    else if (blk.source < MAX_SLOTS)
    {
        source = 1u;
    }
    let run = shapeRun(shape_tables(source), shapeParams, req, runs, limit);

    // The three words the embassy does not write and never reads: the metric
    // comes back into the block, and `block_size` is what the layout asks.
    // Clamped, and not as a formality: the paint walks this run per texel and
    // sizes a chip around this width, so a metric that came back absurd — a
    // corrupt table, a font whose flattened kerns are nonsense — would become
    // an absurd rect rather than an ugly one. A bar is never wider than a few
    // thousand px; MAX_METRIC_PX is far past any label and far short of
    // anything that breaks the layout arithmetic.
    float advance = clamp(float(run.advance) / 64.0, 0.0, MAX_METRIC_PX);
    private[0].blocks[b].size = float2(advance, text_line_height(blk.px));
    private[0].blocks[b].glyph_count = run.glyph_count;
    private[0].blocks[b].flags = run.flags;
    // A truncated run is a **bug rather than a size** (`text_slots`), and
    // `bar_apply` names the block — there rather than here, because the
    // witness ring's count is a read-modify-write and this pass is one thread
    // per block.
}
