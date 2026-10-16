# The ABI

The contracts between the bar process, its caustic and its sidecars, byte for
byte.

The bar has **two** contracts. **The wire** (*The wire*) is what a sidecar, a
separately built binary, writes to the bar over a pipe and what the bar
writes back: byte-stable, self-describing, length-prefixed frames. **The
buffer** (*The buffer*) is what the bar process and its caustic share on the GPU. One
principle draws the buffer: **the bar process is a byte router with a Wayland
surface.** It writes what it originates (frame inputs, pointer and popup
commands, the windows the compositor reports, the applications and
favourites, which image slot holds which pixmap, which binary sits in which
sidecar slot, the strings and the literal pool), and it copies what sidecars
send (samples, rows, answers) byte for byte into arenas the caustic reads
through the wire's own layout. The caustic reads all of it, builds whatever it
needs in a private buffer the ABI does not describe, and talks back through
three channels: **actions** (operands the process forwards without
resolving), **witnesses** (format literals the process prints), and **one
word** the process polls (`wants_redraw`). No ids cross the buffer. No table
is mirrored. The Rust crate `caustics-bar-abi` is the one declaration, and both
Slang twins (`bar_abi.slang` for the buffer, `bar_wire.slang` for the frames
the caustic reads whole) are rendered from it wherever a compile needs them;
neither is in the tree.

## The two contracts

| | the buffer | the wire |
|---|---|---|
| parties | `caustics-bar` and `caustics/bar.slang` | `caustics-bar` and any sidecar |
| built | together, from one tree; the caustic from source at build and on reload | separately, by anyone, at any time |
| stability | none needed across builds; both halves change in one commit | byte-stable framing; a reader that skips what it does not know |
| version | `BAR_ABI_VERSION` (u32), a courtesy for buffer dumps | `WIRE_MAGIC` in every frame header, and one `ABI_HASH` per child (*The ABI hash*) |
| declared | once, in Rust (`caustics_bar_abi::buffer`); `bar_abi.slang` is rendered from it | once, in Rust (`caustics_bar_abi::wire`); the frames the caustic reads are rendered into `bar_wire.slang` |
| enforced by | the layout test (every struct's `offset_of!` against slangc's reflection, both ways) | the reader: magic, kind, length, bounds, or the frame is skipped with one witness |
| what crosses | CPU facts and sidecar bytes in; actions, witnesses and one word out | whole-state frames |

The **tunables module** (`caustics/bar_tunables.slang`) is a third thing: a
*source* API, enforced by the compiler (docs/architecture.md, *Configuration
is the caustic*). It needs no version number because the compiler is the
version check. It imports `bar_model.slang`, the caustic's own vocabulary
(*The private buffer*), never the ABI.

### Layout rules

std430. Every struct is 16-byte aligned and a multiple of 16 bytes. Every
field is a `uint` or a `float` or an array of them, so every bit pattern is
valid and the plain byte views are sound. No vectors in shared structs: a
3-vector aligns to 16, and slangc's reflection once put one at the wrong
offset. **Padding is named `_padN`, is written as zero, and is read by
neither side.** It exists only to reach the next multiple of 16, never to
hold a place for later. Nothing is reserved: a retired word is deleted and
the rest renumbered, because both halves compile from one tree. The wire's
and the shaping arena's structs name their padding `_rsvdN` instead: a wire
body keeps its shape within a hash, so such a word may be given a meaning in
a later contract without moving a byte, as `Row::body` was.

`NO_INDEX = 0xffffffff` means "nothing" wherever an index is optional, on
both contracts.

### The three rules the buffer adds

1. **A ring belongs to the frame stamped on it.** The bar process writes
   `FrameInputs.frame` before the first dispatch of a frame and advances it
   only after that frame's readback has completed. A pass that pushes into a
   ring compares the ring's `*_frame` with `inputs.frame`: different means
   reset the count and restamp; same means append. So `bar_build` in the
   shape submission and `bar_apply`/`bar_sample` in the frame submission
   append to the same rings with no clear-at-top and no dispatch flag, and a
   frame that fails at the swapchain (its readback never runs) keeps its
   stamp, so the next attempt appends to what it pushed. **No action or
   witness is ever lost.** Only the three single-thread passes push. A full
   ring drops the newest and counts it in `*_lost`.
2. **Ranges are read against the bytes that were live when they were
   pushed.** An action or witness may name a string range (offset, length
   into the strings buffer). The process recomposes strings and arenas only
   between one frame's drain and the next frame's upload, never between a
   submission and its drain, so the range names what the caustic saw.
3. **The readback is one frame late, never synchronous.** The process reads
   `State` from a readback of the previous frame.

## The buffer

### Who writes what

| binding (set 0, by name) | struct / bytes | writer | reader |
|---|---|---|---|
| `inputs` | `FrameInputs`, 80 B | the process, once per frame before any dispatch | every pass |
| `params` | `Params`, 16 B | the process, once per target | `bar_visibility`, `bar_sample`, `bar_draw` |
| `commands` | `Command[MAX_COMMANDS]`, live prefix | the process | `bar_apply` |
| `facts` | `Facts`, 10528 B, whole | the process, on any change to what it knows | `bar_build`, `bar_apply` |
| `strings` (set 2, `shapeUtf8`) | UTF-8 words | the process up to `compose_base`; the caustic after | `bar_build`, `bar_compose`, `bar_shape` |
| `images`, `tex_sampler`, `preview` | image array + one slot | the process (`write_texture`) | `bar_draw` |
| `samples` | one `Samples` body, verbatim | the process | `bar_build`, `bar_draw` |
| `rows` | `MAX_SLOTS × ROWS_ARENA_BYTES` | the process, one slot per `Rows` frame | `bar_apply`, `bar_build` |
| `answers` | `ANSWERS_BYTES` | the process, whole, with the facts | `bar_build` |
| `state` | `State`: `Surface` + two rings, 21536 B | the caustic | the process, one frame late (the only readback) |
| `private` | one `Private`, stride from reflection | the caustic | the caustic; tests by reflection |
| `ids`, `dst` | storage textures | `bar_visibility`, `bar_draw` | `bar_sample`, the swapchain |

Set 1 is the text engine's four curve buffers, declared by its `text` module,
and set 2 the shape tables, both filled by `Bytes` frames (*Bytes*); the bar
never reads a byte of either. Their layouts are the engine's
(punchcutter/docs/arena.md, *The curve buffers*, *A shaping dispatch*). The process writes `state` and `private` **never**.
Both start zero (wgpu zero-initialises a buffer at creation) and the caustic
initialises itself (*The private buffer*).

The seven entry points and their order (`ENTRIES` in
`bar/abi/src/entries.rs`): `bar_build`, `bar_compose`, `bar_shape` in the
shape submission when something built is owed; `bar_apply`, then
`bar_visibility`/`bar_sample`/`bar_draw` per mapped target, every frame.

### Structs

Eleven structs in the buffer contract. Offsets in bytes. The Rust declaration
spells a few pairs as `uint2`/`float2`; the layout is the same, and the
offsets below are the ones the tests assert.

```
struct FrameInputs {     // 80 B — the process's per-frame facts
    uint  frame;         // 0   ≥ 1; advanced after a readback completes (rule 1); 0 never occurs
    float dt;            // 4   seconds since the last frame the caustic ran
    float time;          // 8   seconds since the bar started
    uint  scale;         // 12  the surface's integer scale
    uint  edge;          // 16  EDGE_TOP / EDGE_BOTTOM: where the bar actually is
    uint  caffeine;      // 20  CAFFEINE_ABSENT / CAFFEINE_OFF / CAFFEINE_ON: the in-process inhibitor's state
    uint  command_count; // 24  live entries in `commands`
    uint  _pad0;         // 28
    uint  bar_w, bar_h;         // 32  the bar surface, physical px
    uint  popup_w, popup_h;     // 40  the popup surface as mapped, physical px; 0,0 = none
    uint  preview_w, preview_h; // 48  the preview slot's live size; 0,0 = none
    uint  clock_time;    // 56  hour * 60 + minute, local
    uint  clock_date;    // 60  weekday << 16 | month << 8 | day (weekday 0 = Sunday, month 1..12)
    uint  output_w, output_h;   // 64  the output the bar is on, physical px; 0,0 = unknown, no bound
    uint  clock_month;   // 72  year << 16 | weekday of the 1st << 8 | days in the month (the calendar)
    uint  _pad1;         // 76
}
struct Params {          // 16 B — per dispatch target
    uint  target;        // 0   TARGET_BAR / TARGET_POPUP
    float cap_height;    // 4   the font's cap height in em, from the Font frame (*Font*)
    uint  _pad0, _pad1;  // 8
}
struct Command {         // 32 B — the process → the caustic, one frame deep
    uint  tag;           // 0   CMD_*
    uint  target;        // 4   TARGET_* the pointer event belongs to
    float pos_x, pos_y;  // 8   physical px in that target
    uint  arg[4];        // 16  CMD_POINTER_DOWN/UP: button (BTN_*); CMD_POPUP_MAPPED: w, h; CMD_ROWS: slot;
                         //     CMD_AXIS: vertical px (int bits, logical × scale, + down), discrete steps (int bits)
}

struct FactsHeader {     // 32 B
    uint window_count;   // 0   live `windows`
    uint app_count;      // 4   live `apps`
    uint image_count;    // 8   live `images`
    uint literal_count;  // 12  live `literals`
    uint string_words;   // 16  live words of `strings`, the composed region included
    uint compose_base;   // 20  byte offset where the caustic's composed region begins
    uint answer_bytes;   // 24  live bytes of `answers`
    uint glyph_bytes;    // 28  where the App answers begin in it (*The arenas*)
}
struct WindowFact {      // 32 B — one window the compositor reports, in its order
    uint id_lo, id_hi;   // 0   the compositor's window id, 64 bits, forwarded back verbatim in an action
    uint app_row;        // 8   index into `apps`, or NO_INDEX
    uint title_off;      // 12  UTF-8 title in `strings`
    uint title_len;      // 16
    uint flags;          // 20  WFLAG_FOCUSED, WFLAG_URGENT
    uint focus_stamp;    // 24  higher = focused more recently
    uint _pad0;          // 28
}
struct AppFact {         // 16 B — one dock entry, in dock order
    uint id_off;         // 0   the app id, UTF-8 in `strings`
    uint id_len;         // 4
    uint flags;          // 8   AFLAG_FAVOURITE
    uint _pad0;          // 12
}
struct ImageFact {       // 16 B — which image slot holds which pixmap
    uint name_off;       // 0   the key the pixmap arrived under (an app id, an icon name, a sidecar's minted key, *Pixmap*)
    uint name_len;       // 4
    uint slot;           // 8   layer of `images`
    uint _pad0;          // 12
}
struct SlotFact {        // 16 B — one sidecar slot
    uint name_off;       // 0   the sidecar's name (the file stem it was spawned as), UTF-8 in `strings`; len 0 = no child
    uint name_len;       // 4
    uint _pad0, _pad1;   // 8
}
struct Literal {         // 16 B — one getStringHash literal of the running caustic
    uint hash;           // 0   the value getStringHash gave it
    uint off;            // 4   its UTF-8 bytes in `strings`
    uint len;            // 8
    uint _pad0;          // 12
}
struct Facts {                            // 10528 B, uploaded whole
    FactsHeader header;                   // 0
    WindowFact  windows[MAX_WINDOWS];     // 32     128 × 32
    AppFact     apps[MAX_APPS];           // 4128   64 × 16
    ImageFact   images[MAX_IMAGE_SLOTS];  // 5152   64 × 16
    SlotFact    slots[MAX_SLOTS];         // 6176   16 × 16
    Literal     literals[MAX_LITERALS];   // 6432   256 × 16
}

struct Surface {         // 32 B — the caustic's words the process polls
    uint frame;          // 0   FrameInputs.frame as last seen by bar_apply; 0 = the caustic has not run
    uint wants_redraw;   // 4   1 while something animates; the process schedules another frame
    uint action_frame;   // 8   rule 1
    uint action_count;   // 12  live `actions`
    uint action_lost;    // 16  pushes past MAX_ACTIONS in this frame
    uint witness_frame;  // 20
    uint witness_count;  // 24
    uint witness_lost;   // 28
}
struct Note {            // 48 B — one ring record
    uint head;           // 0   action: ACT_*; witness: getStringHash of the format literal
    uint aux;            // 4   action: 0; witness: WITNESS_DEBUG / INFO / WARN
    uint arg[10];        // 8   per kind (*The action ring*) or per format (*The witness ring*); a float travels as its bit pattern
}
struct State {                            // 21536 B — the readback
    Surface surface;                      // 0
    Note    actions[MAX_ACTIONS];         // 32     64 × 48
    Note    witnesses[MAX_WITNESSES];     // 3104   384 × 48
}
```

What is **not** here, on purpose: the item, text-block and element tables,
the scratch state and the `Id` type. Those are the caustic's private memory
and private naming (*The private buffer*).

### Constants

All `uint`, all rendered into `bar_abi.slang` from the Rust declaration.

- **Capacities.** `MAX_WINDOWS` 128, `MAX_APPS` 64, `MAX_IMAGE_SLOTS` 64,
  `MAX_SLOTS` 16, `MAX_LITERALS` 256, `MAX_STRING_WORDS` 16384,
  `MAX_COMMANDS` 256, `MAX_ACTIONS` 64, `MAX_WITNESSES` 384 (a first
  laid-out frame witnesses every element, so this is the caustic's
  `MAX_ELEMENTS` of 256 plus room for a frame's summary and build lines),
  `ROWS_ARENA_BYTES` 65536, `ANSWERS_BYTES` 131072, `MAX_COMPOSED` 64,
  `COMPOSE_MAX_BYTES` 96, `IMAGE_SLOT_PX` 64, `PREVIEW_MAX_DIM` 2048,
  `MAX_GLYPHS` 32768 (*The private buffer*).
- **Inputs.** `EDGE_TOP` 0, `EDGE_BOTTOM` 1; `TARGET_BAR` 0, `TARGET_POPUP` 1
  (neither is `NO_INDEX`, which is what the caustic's private pointer target
  holds when there is no pointer); `CAFFEINE_ABSENT` 0, `CAFFEINE_OFF` 1,
  `CAFFEINE_ON` 2; `WFLAG_FOCUSED` 1, `WFLAG_URGENT` 2; `AFLAG_FAVOURITE` 1;
  `BTN_LEFT`, `BTN_RIGHT`, `BTN_MIDDLE` (evdev codes, shared with the wire's
  `ActionBody`).
- **Commands.** `CMD_NONE` 0, `CMD_POINTER_MOTION` 1, `CMD_POINTER_DOWN` 2,
  `CMD_POINTER_UP` 3, `CMD_POINTER_LEAVE` 4, `CMD_POPUP_MAPPED` 5,
  `CMD_POPUP_DONE` 6, `CMD_ROWS` 7, `CMD_DUMP_STALL` 8, `CMD_AXIS` 9 (a
  wheel or touchpad scroll, vertical only, carried as the compositor sent it;
  what a step is worth and what scrolls are the caustic's).
- **Actions.** *The action ring*'s table, `ACT_NONE` 0 through `ACT_IMAGES_DRAWN` 19;
  `POPUP_MENU` 1, `POPUP_PREVIEW` 2, `POPUP_TOAST` 3; `SRC_STRINGS` =
  `MAX_SLOTS`, `SRC_ANSWERS` = `MAX_SLOTS + 1` (`ACT_WANT_ICON`'s fourth
  operand, `ACT_COVER`'s third).
- **Witnesses.** `WITNESS_DEBUG` 0, `WITNESS_INFO` 1, `WITNESS_WARN` 2.
- `NO_INDEX`, `BAR_ABI_VERSION`.

About fifty in all. Everything else the caustic uses (item kinds and flags,
element slots and flags, block kinds, compose kinds, id tags, dock verbs,
`WHEN_*`) is in `bar_model.slang` (*The private buffer*), and everything a frame carries
(`ROW_*`, `RFLAG_*`, `APPLET_*`, `CHART_*`, `GLYPH_*`, `APP_*`, `REQ_*`, the
wire's `ACTION_*`) is the wire's and is rendered into `bar_wire.slang`
(*The arenas*).

### The facts

What the process writes is what it *originates*: things it learned over a
socket or a file it owns, or decisions the membership rule leaves it (which
slot a pixmap went into, which slot a sidecar got). Everything is positional,
every string is a range into `strings`, and nothing is hashed on this side.

- **`windows`**: the compositor's window list, in its order, with titles, the
  two flags and the focus stamp. `app_row` is the join to `apps` the process
  makes, because it is the one that knows both lists.
- **`apps`**: the dock's entries **in dock order**: the favourites file's
  entries first, in file order, then every running app id not among them, in
  the order first seen. `AppFact.id` and `SlotFact.name` are ranges into the
  **head region** of the strings buffer (`Facts::put_stable`, 4 KiB), where a
  name is placed once and **never moves for the life of the process**,
  because a `rect=gone` witness quotes a range one frame after the compose
  that dropped what it names. Titles and literal bytes stay in the
  recomposed part. What an application is called and what its icon is are
  **not** here: they are the apps sidecar's answers, in `answers` (*The arenas*),
  joined by the app id string.
- **`images`**: the fixed-slot image array's directory. A `Pixmap` frame
  (*Pixmap*) is pixels for one slot; the process chose the slot, so it says
  which key sits there. A listing, not a draw: being named here does not keep
  a slot from being evicted. Only `ACT_IMAGES_DRAWN` does (*The action ring*).
- **`slots`**: the sidecar slots, by name. Exists so a witness can say
  `applet-caustics-power-3` without the process naming anything.
- **`literals`**: every `getStringHash` literal of the running caustic, from
  the reflection's `hashedStrings`, with its bytes in `strings`. This is how
  text the process never reads reaches the GPU: the dock menu's labels, the
  clock's words, the icon lists.
- **`strings`**: the UTF-8 the above address, padded to a word, then the
  **composed region** from `compose_base`: `MAX_COMPOSED × COMPOSE_MAX_BYTES`
  bytes the process reserves and never writes, where `bar_compose` spells the
  clock. `string_words` counts the whole buffer, the composed region included;
  `compose_base` is where the process's own part ends.
- **`images` (the texture), `preview`**: straight RGBA8 slots; the caustic
  premultiplies at the sample.

`Facts` is uploaded **whole** on every change (10.5 KB).

### The arenas

Three buffers hold frames as they crossed the wire. The caustic reads them
through `bar_wire.slang`, rendered from `caustics_bar_abi::wire`'s Rust structs
and constants: `Samples`/`ChartRow`/`CHART_*`, `RowsHeader`/`Row`/`ROW_*`/
`RFLAG_*`/`APPLET_*`/`MAX_ROWS`, `GlyphBody`/`GLYPH_*`, `AppBody`/`APP_*`,
`REQ_*`, and the wire's `ACTION_*` kinds. **The wire is declared once for
three parties**: the sidecar that writes a frame, the process that frames it,
the caustic that reads it. The layout test checks these structs against
slangc's reflection like any other.

- **`samples`**: one `Samples` body (*Samples*).
- **`rows`**: `MAX_SLOTS` regions of `ROWS_ARENA_BYTES` (1 MiB in all). A
  `Rows` frame goes into its sidecar's region **whole** (`RowsHeader`, the
  rows, the blob, at the offsets the writer gave, verbatim), and nothing in a
  region is pixels, because a row's icon is a name (*Pixmap icons*). A frame whose
  bytes exceed the region is refused with one witness and the region is left
  as it was. A sidecar that exits has its region's header zeroed. The process
  then sends `CMD_ROWS {slot}`. **`bar_apply` decides whether that is a
  rebuild**: it hashes the region's shape (every word of every row but
  `value`, plus the label and icon bytes their ranges name), compares with
  the copy it keeps in `private`, and pushes `ACT_REBUILD` only if they
  differ. So a slider tick is one `write_buffer` and no model pass, and a
  shape change rebuilds one frame later. The chart strip's kinds are compared
  the same way.
- **`answers`** (128 KiB): every `Glyph` (*Glyph*) and `App` (*App*) frame the
  process has received, concatenated, header then bytes, each padded to a
  word. The glyph bodies come first and the app bodies after, and
  `FactsHeader.glyph_bytes` says where the split is, because an answer body
  carries no kind word of its own and the two bodies are different sizes.
  Rewritten whole with `facts`, with `answer_bytes` in the header. The
  caustic indexes it once per build into `private` and joins by name with
  its own hash of the bytes: an app's `AppFact` id against an `AppBody`'s
  echoed app id, an icon name (from a literal, a row, or an `AppBody.icon`)
  against a `GlyphBody`'s echoed name or an `ImageFact`'s key. A name with no
  answer is pending and draws as nothing. An answer whose flags say
  `GLYPH_MISSING`/`APP_MISSING` is missing and draws the fallback. When the
  arena would overflow, the process drops the oldest answers; a want the
  caustic pushes again is asked again.

The arenas are `RWStructuredBuffer<uint>`, not `ByteAddressBuffer`, because a
row's label and an app's name are shaped from the arena and the shaper takes
its text through a word-buffer handle. So the caustic reads `RowsHeader`,
`Row`, `GlyphBody` and `AppBody` word by word at literal offsets, and a test
in `bar/src/abi.rs` walks every field of all four through `offset_of!` to pin
those offsets.

The process's rows-side code is then: frame it (*Framing*'s reader), find the
pixmaps, write bytes. An identical frame is no write. The line `sidecar X:
rows v1, applet=N rows=M rev=R` is the wire reader's witness, like
`sidecar X: started`; what the caustic does with the region shows in its
`built:` line, or in the absence of one.

### The action ring

`bar_apply` and `bar_build` push. The process drains
`actions[..action_count]` one frame late and **forwards** each. No payload is
resolved through a table the process keeps: a window id is the compositor's,
an app id is a string range, a sidecar action is the wire body's words, a
popup is its geometry. The kind table is the contract, and the Rust side has
one typed accessor per kind.

| `ACT_*` | | `arg[0..]` | forwarded to |
|---|---|---|---|
| 0 | `NONE` | — | — |
| 1 | `ACTIVATE_WINDOW` | `id_lo, id_hi` | the compositor: focus that window |
| 2 | `CLOSE_WINDOW` | `id_lo, id_hi` | the compositor: close it |
| 3 | `PREVIEW_BEGIN` | `id_lo, id_hi` | the capture session |
| 4 | `PREVIEW_END` | — | the capture session |
| 5 | `SPAWN` | `app_off, app_len` | `launch`, then the compositor's spawn |
| 6 | `PIN` | `app_off, app_len` | the favourites file |
| 7 | `UNPIN` | `app_off, app_len` | the favourites file |
| 8 | `TOGGLE_OVERVIEW` | — | the compositor |
| 9 | `TOGGLE_CAFFEINE` | — | the in-process inhibitor |
| 10 | `SIDECAR` | `slot, row, kind, button, pos_x(f), pos_y(f), value(f), token` | that slot's stdin as an `ActionBody` (*Action*), word for word. `kind` is the **wire's** `ACTION_*`, and `pos` is divided by `scale` on the way. `token` is 1 when the kind is `ACTION_ACTIVATE` and the row carries `RFLAG_ACTIVATION`: the process asks for an activation token for that click (*Request*). The caustic reads the flag because the process reads no row |
| 11 | `POPUP_OPEN` | `kind, slot, row, ax(f), ay(f), aw(f), ah(f), w, h` | a new `xdg_popup` of `POPUP_MENU` (with a grab), `POPUP_PREVIEW` or `POPUP_TOAST`, anchored at the rect, `w × h` physical px. For a menu, `slot ≠ NO_INDEX` also sends that sidecar `ACTION_MENU_OPEN {row}`; a toast is not its menu being read |
| 12 | `POPUP_MOVE` | `w, h` | reposition the open popup |
| 13 | `POPUP_CLOSE` | — | destroy it; `ACTION_MENU_CLOSE` to the slot that got the open |
| 14 | `ANCHOR` | `edge, height` | the layer surface's anchor and size |
| 15 | `REBUILD` | — | the shape submission runs next frame |
| 16 | `COVER` | `off, len, source` | `REQ_COVER` to the curves sidecar with those bytes (*Request*). `source` says which buffer the bytes are in: a title or literal in `SRC_STRINGS`, a sidecar's row label in its slot's region, an app's name in `SRC_ANSWERS` |
| 17 | `WANT_ICON` | `off, len, kind, source` | `REQ_GLYPH` / `REQ_ICON` / `REQ_APP` with those bytes, `kind` the wire's `REQ_*`. `source` says **which buffer** they are in, as for `COVER`, because a name the caustic wants lives in one of three places and a range alone cannot say which |
| 18 | `WATCHDOG` | `ms` | how long one `bar_shape` dispatch may take before the process calls it a runaway. `SHAPE_WATCHDOG_MS` in the tunables. The *response* (one witness, and no shaping until the OpenType arena's front block is written again) is fixed and the process's, because only it holds the queue and the timestamp query pair |
| 19 | `IMAGES_DRAWN` | `mask[0..(MAX_IMAGE_SLOTS + 31) / 32]` | the image slots this build placed in an image block, slot `n` at bit `n % 32` of word `n / 32`. Pushed by `bar_build` once per build, an empty mask included, **before** its `WANT_ICON`s, so the drain marks what is on the bar before it serves a want that may evict. The process copies the mask into the slot array whole: a set bit is drawn and stamped with the compose's generation, a clear one is not drawn. An eviction takes the least recently stamped slot that is neither drawn nor filled this generation, and is refused only when none is. The slots' age is the caustic's word, because only the caustic lays the blocks out |

**Messages, not state.** The popup, the edge and the height are messages,
because the caustic has the "have" side in `inputs` (`popup_w/h`, `edge`) and
can compare for itself; rule 1 is what makes a message safe to send once.
The popup's life: `POPUP_OPEN` → `CMD_POPUP_MAPPED {w, h}` (or
`CMD_POPUP_DONE` at once if the compositor refused) → zero or more
`POPUP_MOVE` when the caustic's want differs from `inputs.popup_w/h` →
`POPUP_CLOSE` → `CMD_POPUP_DONE`. The compositor dismissing it is a
`CMD_POPUP_DONE` without a `POPUP_CLOSE`. `ANCHOR` is pushed on the caustic's
first frame and whenever a reload changes its tunables' answer; `inputs.edge`
reports what the process could do, and a refusal ("another caustics-bar holds
that edge") is the process's witness. `WATCHDOG` is pushed the same way and
for the same reason.

**Requests.** `COVER` is pushed by `bar_apply` when the *set* of notdef runs
changes (it keeps the set's hash in `private`), once per run, skipping the
composed region. The "font has not landed" case is the same loop's `every
text block is notdef`, and pushes nothing. `WANT_ICON` is pushed by
`bar_build` for a name it has no answer for; which wants fit when there are
many (a glyph question over a pixel question) is the caustic's, because the
process cannot dedup a want it never saw. The process asks once per name and
remembers (*Request*); a want repeated by the next build costs nothing.

**Buttons.** `SIDECAR` carries the button the caustic saw, so a
StatusNotifierItem `Activate(x, y)` on a side button still reaches the tray.
The bar's own kinds carry none: `ACTIVATE_WINDOW` is a left click by
definition, and the dock menu's rows are `SPAWN`/`CLOSE_WINDOW`/`PIN`/`UNPIN`
directly, since a `DOCK_MENU` tunable row names an `ACT_*`.

### The witness ring

`printf` for a caustic. (Real Slang `printf` is the validation layer's
debug-printf, which wgpu cannot reach.)

A witness is a **format literal** in the caustic and up to ten words:

```slang
witness(WITNESS_INFO, getStringHash("dock: {0} entries ({1} running)"), count, running);
```

slangc records the literal in the reflection's `hashedStrings`. The process
resolves `head` through that table and substitutes:

| spec | reads | prints |
|---|---|---|
| `{n}` | `arg[n]` | unsigned decimal |
| `{n:x}` | `arg[n]` | `0x` + hex, 8 digits |
| `{n:f}` | `arg[n]` as f32 | as Rust's `{}` for `f32` (`724`, `0.5`, never `724.00`) |
| `{n:s}` | `arg[n]` as a hash | that literal's text (`target={2:s}` with `getStringHash("bar")`) |
| `{n:str}` | `arg[n]`, `arg[n+1]` | UTF-8 at that range of `strings`, lossily |
| `{n:u64}` | `arg[n]`, `arg[n+1]` | `lo + (hi << 32)`, decimal |

`{{` and `}}` are braces. A spec that reaches past `arg[9]`, a hash the table
lacks, or a range past `string_words` prints `?` and is one process witness
per literal per generation. **Level** is `aux`. **Target** is the word before
the line's first colon, as a fixed match: `layout:` → `caustics_bar::layout`,
`shape:` → `caustics_bar::shape`, everything else → `caustics_bar`. (`tracing`'s
`target:` must be a literal at the macro's call site, so a target is three
arms in `witness.rs`, and a fourth one is a line there as well as a line in
Slang.) Those are exactly what the unit drop-in's
`RUST_LOG=info,caustics_bar::layout=debug,caustics_bar::shape=debug` turns on. The
process prints every record it reads, in order, and dedups nothing. **Once
per change is the caustic's duty**, which it discharges by comparing against
what it persisted in `private`. There is no per-witness Rust; a new witness
is one Slang line.

What the caustic witnesses, and from which pass:

- **`layout: <name> target=<t> rect=x,y,w,h`**, the load-bearing family
  (`bar/bar-test.sh` parses it for click positions). One literal per element
  kind, the name composed from what the caustic has: `window-{0:u64}`,
  `{0:str}` for an app entry (its id), `dock-{0:s}` for a dock menu row (the
  verb literal), `chart-{0:s}` / `chart-{0:s}-{1}` (the chart kind's literal,
  the repeat index), `applet-{0:str}` / `applet-{0:str}-{2}` (the slot's name
  from `slots`, the row id), `calendar-{0:s}-{1}` /
  `calendar-weekday-{0:s}` / `calendar-day-{0}` for the clock's calendar, and
  `clock`, `overview`, `caffeine`, `ELEM_BAR`, `ELEM_POPUP`, `ELEM_BACK`,
  `ELEM_PREVIEW` as their own literals; then ` target={n:s} rect={..:f},…`.
  Pushed by `bar_apply`'s `place` when an element's `(id, target, rect)`
  differs from the words it persisted, and by a walk at the end of `layout`
  over the slots the previous frame used and this one does not: `… 
  target=none rect=gone`, which also clears the element's present flag.
- **`hover: <name> at=x,y`** and **`hover: none`**: `bar_sample` on a change
  of the hovered element; `bar_apply`'s pointer-leave arm for `none`.
- **`redraw: animating`** / **`redraw: settled`**: `bar_apply`, on the edge.
- **`wake: stirred`** / **`wake: still, {0:f} s after its last source`**:
  `bar_apply`, on the edge of the pointer's wake having any live source; the
  number is the seconds since the last one was dropped.
- **`dock: {0} entries ({1} running)`**, **`applets: {0}`**, **`charts: {0}
  laid out`**, **`built: {0} items, {1} blocks, {2} glyphs`**, **`items:
  table capped at {0} ({1} dropped); some of the bar is not drawn`** (WARN):
  `bar_build`, each against the value it persisted.
- **`text: every run has its glyphs`** / **`text: {0} runs have a glyph the
  font lacks`**: `bar_apply`, on the edge, beside `COVER`.
- **`toast: open slot={0} rows={1} ids={2},…`** / **`toast: closed
  ({0:s})`** / **`toast: held (new row while up) row={0}`**: `bar_apply`'s
  toast step and the paths that end a toast. `ids` are the row ids the toast
  fixed on at its open, in the order it draws them; the why is `flag`,
  `gone`, `empty` (none of its own rows is left flagged), `click`, `menu` or
  `dismissed`; `held` is said once for each attention row that arrives while
  a toast is up and waits for it to end.
- **`calendar: open {0:s} {1} ({2} days, the 1st a {3:s}, {4} weeks)`**:
  `bar_apply`'s click, when the clock's click asks for the calendar.
- **`stall: <name> anim={..:f}`** (WARN), one per moving element, then the
  popup's dwell and **`stall: wake(sources,oldest s,since last s)`**:
  `bar_apply`, in answer to `CMD_DUMP_STALL`. The *trigger* stays on the process: a stall
  is `wants_redraw` held for longer than its wall clock likes, and the pass
  that would notice from `dt` is the one that stops running when frames do.
  So the process keeps `redraw: still animating after N s` and asks the
  caustic to explain itself.

What the process still witnesses, because only it knows: `shape: model passes
ran in N ms` (its own timer), `charts: none (no frame from the sidecar)` (a
frame that never arrived is not a frame with no rows), `sidecar X:
started/exited/absent`, `favourites:`, the popup's Wayland side (`popup: open
…`, `popup: closed (why)`), `action: … -> niri-ipc`, `tunables: applied`,
`re-anchored to the … edge`, the reload family, `health (`. `witness_lost !=
0` is itself a WARN from the process and a test failure.

### The one word

`Surface.wants_redraw` is the only GPU state the process acts on without a
message. While it is 1 the process requests another frame callback; when it
falls to 0 the bar goes idle. Everything else in `Surface` is bookkeeping for
the rings. `frame` doubles as "has the caustic run at all" (0 until the first
`bar_apply`), which is what the first-frame guards key on.

### The private buffer

`RWStructuredBuffer<Private> private`, one element, declared in `bar.slang`,
holding whatever the passes want in whatever layout they want: the element
table, the item and block tables, the last pointer position, the popup want,
the anchor last pushed, the notdef set's hash, each rows region's shape copy,
the menu level, the springs, the pointer's wake, and every value a witness is
compared against.

**`runs` is private in the same sense** and is a binding only because
the engine's `shapeRun` writes through an `RWStructuredBuffer<ShapedGlyph>`
handle, and an array inside a struct is not one. The process allocates
`MAX_GLYPHS × 32` bytes for it and never writes or reads one of them, which
is why `MAX_GLYPHS` is the one capacity of the caustic's own memory that
stays in the ABI (*Constants*). It is 32768 (1 MiB): `bar_build` gives a text block
`min(bytes * 4 + 8, MAX_RUN_GLYPHS)` slots, because a multiple substitution
adds glyphs, and a row's body is shaped too (up to `BODY_SHAPE_BYTES` of it a
row).

What the ABI promises about it is one paragraph. It is **one binding**,
`private`, of the stride slangc's reflection reports for `Private`. The
process allocates that many bytes, **never writes it and never reads it in
production** (the usage flags say so: `STORAGE` alone in a shipped bar, with
`COPY_DST` added only in a test build, where `set_menu_level` writes one
field so a scenario can name an open menu without a click). It starts as zero
(wgpu's creation guarantee), and the caustic initialises itself:
`ensure_private()` at the top of every single-thread pass writes the non-zero
defaults when `Private.magic` is 0. A caustic reload keeps it, so the popup,
the menu level and the springs survive a reload. A reload that changes
`Private`'s layout changes the stride, and the bindings diff refuses it with
"restart the bar".

The vocabulary the caustic uses for it (`ITEM_*`, `IFLAG_*`, `ELEM_*`,
`EFLAG_*`, `BLOCK_*`, `COMPOSE_*`, `ID_*` and the `Id` type, `DOCK_*`,
`WHEN_*`, and the item and id helpers) lives in **`caustics/bar_model.slang`**,
imported by `bar.slang` and by `bar_tunables.slang` (which needs
`ITEM_KIND_COUNT` for `TEXT_PX` and `WHEN_*` for `DOCK_MENU`). It is the
caustic's own module, hand-written, hot-reloadable, and **not a contract**: no
Rust reads it. No id crosses the buffer. Tests reach into `private` by name
through `punchcutter_core::slang::struct_offsets` (*Bindings and the oracle*), which is legibility, not a
promise.

### Bindings and the oracle

**Bindings.** The three bind group layouts, the pipeline layout and the bind
groups are built from the reflection: `Gpu::bind_group` maps each reflected
`Binding.name` to its resource and never writes a number. `[[vk::binding(n,
s)]]` literals stay in `bar.slang` because an attribute cannot take a
constant; they are the caustic's, pinned by the reflection the layouts come
from. Set 0 must be dense: wgpu-hal numbers a Vulkan binding by its entry's
position, so a gap is a panic at startup. A reload whose bindings differ in
name, set, index, kind or stride is refused.

**The oracle** (`CAUSTICS_BAR_ORACLE=1`) is a GPU pass. `caustics/bar_check.slang`
is a test-only eighth entry point, not in `ENTRIES`, `#include`d into
`bar.slang` under `-DCAUSTICS_BAR_CHECK` and compiled by the oracle test alone.
It walks `private` after `bar_build`/`bar_compose`/`bar_shape` and pushes
witnesses: `check: …` for each violated structural invariant (every window
hangs off a dock entry; glyph runs sorted, disjoint, inside their buffer;
nothing past a capacity; every item that names a row names one inside its
region's `row_count`), `check: block {0} kind={1} px={2:f}` for every text
and icon block, and one `layout:`-shaped witness per built item. The Rust
test keeps the scenario corpus (`bar/src/oracle/tests.rs`), runs the passes
headless, reads the ring, and keeps the two checks that must not be the
caustic checking itself: every block's size against its own text parse of
`TEXT_PX`/`ICON_LOGICAL`, and every item witness rendering through the
generic printer with no `?`. The oracle guards invariants, not a twin
implementation.

### Declared once

`caustics_bar_abi::buffer` and the GPU-read part of `caustics_bar_abi::wire` declare
every struct and constant through two macros:

```rust
abi_struct! {
    /// The process's per-frame facts.
    pub struct FrameInputs {
        /// ≥ 1; advanced after a readback completes.
        pub frame: u32,
        pub dt: f32,
        …
    }
}
abi_consts! {
    /// Where the bar actually is.
    pub const EDGE_TOP: u32 = 0;
    …
}
```

Each expands to the Rust item **and** an entry in a `Decl` table: the
struct's name, its fields with their Slang type names and its `///` text, or
the constant's name, value and text. `rendered()` renders the tables into
`bar_abi.slang` and `bar_wire.slang`, doc comments forwarded, under a header
that says where they came from. Neither file is **in the tree**: every compile
gets them the way it gets the text engine's modules. `bar/build.rs` writes
them (`write_rendered`) into the directory under `OUT_DIR` it puts on the
include path, and `caustics-reload`, the oracle and the reflection tests take
`rendered_dir()`, written once per contract into
`$XDG_CACHE_HOME/caustics/rendered/<ABI_HASH>/`. So there is no second copy to
fall out of step, and nothing to regenerate by hand. The macros and the renderer are the text engine's
(`punchcutter_core::decl`), which declares the OpenType arena's header the same
way and checks in its own rendering, `shape_abi.slang`
(punchcutter/docs/arena.md, *Declared once*).

Two tests remain on the buffer side. The **layout test**: the macro also
emits each struct's `offsets()` from `offset_of!`, and the test compares every
struct in both tables against `punchcutter_core::slang::struct_offsets` of the
reflection, both ways (a field on one side only fails). The **compile test**:
every entry point in every swapchain-format variant, zero warnings. slangc
lays out only the types a binding's layout needs, so a struct nothing binds
is marked `bound: false` in the table and the test asserts it is absent
rather than comparing it.

No functions live in the ABI, and the process mints no hash at all.

## The wire

### What the caustic reads whole

The process **frames** a sidecar's stdout (*Framing*'s reader) and **routes**
bodies; it interprets only what it must to route. Four kinds go to the GPU
verbatim and are read there through `bar_wire.slang`: `Samples`
(*Samples*), `Rows` (*Rows*, into the sidecar's region of the rows arena), `Glyph`
(*Glyph*) and `App` (*App*, both into the answers arena). `Bytes` (*Bytes*) goes to
a named buffer untouched. `Pixmap` (*Pixmap*) is pixels for a texture, `Font`
(*Font*) is two numbers the process needs for `Params` and a witness, `Module`
(*Module*) is SPIR-V for the device, and `Action`/`Request` (*Action*, *Request*) are
what the process writes.

### Framing

A sidecar's stdout, and its stdin, carry a sequence of frames. Every frame is
one `WireHeader` followed by `len` bytes of body:

```
struct WireHeader {     // 12 B
    uint magic;         // 0   WIRE_MAGIC = 0x53554143, the bytes "CAUS"
    uint kind;          // 4   WIRE_*: which body follows
    uint len;           // 8   body bytes following this header
}
```

`WIRE_SAMPLES` 1 (*Samples*), `WIRE_ROWS` 2 (*Rows*), `WIRE_ACTION` 3 (*Action*),
`WIRE_BYTES` 4 (*Bytes*), `WIRE_FONT` 5 (*Font*), `WIRE_GLYPH` 6 (*Glyph*),
`WIRE_REQUEST` 7 (*Request*, the bar → a sidecar), `WIRE_APP` 8 (*App*),
`WIRE_PIXMAP` 9 (*Pixmap*), `WIRE_MODULE` 10 (*Module*), `WIRE_HELLO` 11 (*Hello*).

`WIRE_MAX_BODY` = **1 MiB** (`1 << 20`): a body longer than that is a broken
writer. Every fixed body is small (`Samples` is 8336 B), so this bounds only
the variable-size kinds, and it bounds them before a byte of one is
allocated. A `Module` larger than the cap is chunked (*Module*), and so is a
`Bytes` run (*Bytes*).

**Reader rules** (`wire::read`, used by the bar and available to sidecars for
their stdin):

1. Read 12 bytes. EOF exactly here is a clean close: the other end is gone.
   EOF inside a header or a body is an error.
2. `magic` wrong: the stream is not ours. Error, and the bar restarts the
   child. (A sidecar that prints to stdout has confused stdout with stderr,
   which is the one mistake this catches on the first byte.)
3. `len > WIRE_MAX_BODY`: error, as above.
4. Read `len` bytes.
5. `kind` unknown: skip, with one witness per kind per child. A sidecar from
   a tree with more kinds in it may write one this bar has not got; it
   passed *Hello*'s gate, so the kind is genuinely additional.
6. `kind` known, `len` wrong for a fixed-size body or inconsistent with the
   counts in a variable one: skip, one witness.
7. Otherwise decode the body and hand it up.

And one rule above all of them, *Hello*'s: **the first frame off a child must
be a `Hello` whose hash is this bar's**, or the child is killed and not
started again. Nothing below rule 5 runs before that.

A writer (`wire::write`) emits header and body in **one** `write_all` and
flushes, so a reader never sees half a frame from a writer that was killed
mid-tick. Every frame is whole-state.

### Lifecycle

The bar spawns each sidecar **with no arguments** (a sidecar's own settings
are its own affair: `CAUSTICS_SYSMON_CHARTS` is the sampler's, read by the
sampler), with stdout and stdin piped and stderr inherited, so a sidecar's
stderr is a journal line under the bar's unit. A sidecar exits when its stdin
reaches EOF: the bar is gone. The bar restarts a sidecar that exits, with a
doubling backoff from 1 s to 30 s, reset by any child that delivered a
frame. A command that cannot be spawned at all (`NotFound`) is one `absent`
witness and no retry. On the bar's exit it closes stdin, waits one second,
and kills what is left.

**Which sidecars.** The bar has no configuration, so the set is compiled in
(`SIDECARS` in `bar/src/feeders/pipe.rs`, ten names), and each name resolves
as `$CAUSTICS_SIDECARS` (a whitespace-separated list of commands that *replaces*
the compiled set, for the test targets and for trying a new sidecar) → the
name beside the bar's own binary → the bare name on `PATH`. A sidecar's
**name** for witnesses and for `SlotFact` (*The facts*) is the file stem of what was
spawned: `sidecar caustics-sysmon: started pid=N`, `absent: caustics-sysmon`.

### Samples: `WIRE_SAMPLES`

The chart strip's history. Fixed size, **8336 B**; uploaded to the `samples`
arena as-is (*The arenas*), so this body is also in `bar_wire.slang`.

```
struct ChartRow {        // 16 B
    float ceiling;       // 0   what the caustic divides by; 0 = auto (window max, floored by RATE_FLOOR)
    uint  series;        // 4   1 or 2
    uint  kind;          // 8   CHART_*
    uint  _rsvd0;        // 12
}
struct Samples {         // 8336 B
    uint  sample_count;  // 0   valid columns 0..=128, the same for every row
    uint  chart_count;   // 4   valid rows 0..=8
    float t;             // 8   CLOCK_MONOTONIC seconds at the tick
    uint  _rsvd0;        // 12
    ChartRow charts[8];  // 16
    float samples[8 * 2 * 128]; // 144: chart c, series k, column i at (2c + k) * 128 + i; oldest first, right-aligned; negative = gap
}
```

`CHART_OTHER` 0, `CHART_CPU` 1, `CHART_TEMP` 2, `CHART_MEM` 3, `CHART_NET` 4,
`CHART_DISK` 5, `CHART_GPU` 6. A negative sample is a gap and breaks the line
on both sides. A row the sidecar could not fill still occupies its index, all
gaps and `series = 1`, so indices never shift under the item table.

### Rows: `WIRE_ROWS`

An applet: what a sidecar wants on the bar and in its menu. Variable size: a
header, `row_count` rows, then `bytes_len` bytes of blob.

```
struct RowsHeader {      // 16 B
    uint applet;         // 0   APPLET_*: what this applet is (ordering, and the caustic's icon fallbacks)
    uint row_count;      // 4   ≤ MAX_ROWS (256)
    uint bytes_len;      // 8   blob bytes following the rows
    uint revision;       // 12  the writer's own counter; appears in the bar's witness, means nothing else
}
struct Row {             // 48 B
    uint  id;            // 0   the sidecar's id for this row: unique in the frame, stable across frames, what comes back in an Action
    uint  parent;        // 4   index of the parent row in this frame, or NO_INDEX for a bar item
    uint  role;          // 8   ROW_*
    uint  flags;         // 12  RFLAG_*
    uint  label_off;     // 16  UTF-8 label in the blob (len 0 = none)
    uint  label_len;     // 20
    uint  icon_off;      // 24  icon in the blob (len 0 = none): a name in UTF-8, themed or a Pixmap key (*Pixmap icons*)
    uint  icon_len;      // 28
    float value;         // 32  ROW_SLIDER: the position; ROW_ITEM: a level to draw on the icon, or −1
    float min;           // 36  ROW_SLIDER: the range value lives in (min < max)
    float max;           // 40
    uint  body;          // 44  a second UTF-8 range of the blob, packed: offset in the low 16 bits, length in the high 16 (0 = none)
}
```

**Roles.** `ROW_ITEM` 1, a bar item (`parent` must be `NO_INDEX`);
`ROW_MENU` 2, a menu row under a bar item or under a submenu row;
`ROW_SEPARATOR` 3; `ROW_SLIDER` 4, a slider row in the menu.

**Flags.** `RFLAG_DISABLED` 1, `RFLAG_CHECKABLE` 2, `RFLAG_CHECKED` 4,
`RFLAG_SUBMENU` 8 (this row has children), `RFLAG_ATTENTION` 16,
`RFLAG_SHOW` 32, `RFLAG_HAS_MENU` 64 (a bar item whose click opens its rows;
without it a click on the bar item is an `ACTIVATE` sent back), `RFLAG_RADIO`
128, `RFLAG_LABEL_TITLE` 256 (a bar item whose label is its **title**: the
caustic never draws it beside the icon, and wears its first character,
uppercased, as the chip's placeholder while the icon is unresolved, `?` when
there is no title; the tray sets it on every item), `RFLAG_ACTIVATION` 512
(an `ACTION_ACTIVATE` on this row wants an activation token, *Request*; the
caustic says so in the `ACT_SIDECAR` it pushes).

**Body.** On a `ROW_MENU`, `body` is a second text drawn wrapped under the
label, which is then the row's title: the label on the first line, the body
under it in a dimmer ink, wrapped at spaces on the shaped glyphs' advances to
the popup's widest, its own newlines kept, at most `TOAST_BODY_LINES` lines
in a toast and `MENU_BODY_LINES` in a menu, the last ending in an ellipsis
when there is more. The row is as tall as its lines and the whole block is
the row for hover and click. `caustics-notify` sends a notification's summary as
the label and its body here. The range is validated like a label's on every
role and means nothing on the others. Sixteen bits each, because the region a
frame lands in is `ROWS_ARENA_BYTES` (64 KiB): no blob offset reaches 2^16,
and `RowsBuilder::body` refuses one that would.

**Toasts.** `RFLAG_SHOW` on a `ROW_ITEM` means "show my rows without a
click". `caustics-notify` sets it on its chip while a notification younger than
its toast time exists and do-not-disturb is off, and clears it with a fresh
frame when the youngest expires. The caustic opens that item's
`RFLAG_ATTENTION` rows (direct children, no separators, at most
`TOAST_MAX_ROWS`, the frame's order) as a `POPUP_TOAST` without a grab while
the bit is set and the popup is idle, and ends it when the bit or the frame
goes or a row is clicked. `RFLAG_ATTENTION` on a menu row means nothing in a
menu; it is what a toast picks its rows by, and `caustics-notify` sets it on a
row exactly while that notification is toasting (unread, never shown, within
its toast time), not while it is merely unread. **A toast's rows are fixed
while it is up**: the caustic waits for the rows to be built, opens at their
size (as wide as `MENU_MAX_W`), keeps their ids, order and heights until it
ends, and ends it when none of them is left flagged. A row that arrives while
it is up waits for the next toast, a row that leaves is a gap, and the popup
never moves.

**Applets.** `RowsHeader.applet` is one of `APPLET_OTHER` 0, `APPLET_AUDIO`
1, `APPLET_NETWORK` 2, `APPLET_BLUETOOTH` 3, `APPLET_TRAY` 4, `APPLET_POWER`
5, `APPLET_CAFFEINE` 6 (the bar's own, never on this wire) and
`APPLET_NOTIFY` 7. The caustic orders the applets by `APPLET_ORDER` in the
tunables; a value it does not name is laid out in `APPLET_OTHER`'s pass.

**Validation** (the reader rejects the frame with one witness): a row count
over `MAX_ROWS`; any offset+length past `bytes_len` (a label's, an icon's, a
body's); a parent index ≥ its own index (parents precede children, so the
tree is acyclic by construction) or naming a row that is not an item or a
submenu row; a `ROW_ITEM` with a parent; a slider with `min >= max`. A label
that is not UTF-8 is replaced with U+FFFD, not rejected.

**What the bar does with it**: nothing but route. The frame goes whole into
its sidecar's region of the rows arena (*The arenas*), then `CMD_ROWS {slot}`. The
caustic reads the region and publishes: `ROW_ITEM` → an applet chip;
`ROW_MENU`/`ROW_SEPARATOR` → menu rows under it; `ROW_SLIDER` → a slider
whose position it reads live from `value`, `min`, `max` at measure and paint
time. The menu rows are published only while that applet's menu is open, and
**a sidecar's rows may change while its menu is open: the caustic re-lays the
menu out in place**, holding its menu level as the row's own id, so a row
arriving above it moves nothing it is holding. A new frame from the same
sidecar replaces that sidecar's region. The caustic rebuilds when the shape
changed and merely repaints when only a value did. In the journal the chip is
`applet-<name>` and a row of its menu is `applet-<name>-<row id>`, both
composed by the caustic from `SlotFact` and the row's id (*The witness ring*).

#### Pixmap icons

A row's icon is a **name**, never pixels. A sidecar whose icon is pixels sends
them as a `Pixmap` frame (*Pixmap*) under a key it minted, **before** the first
`Rows` frame that names the key, and the row carries the key. `caustics-tray`
mints `caustics-tray:<fnv1a64 of the fitted texels, 16 hex digits>`.

Pixels inside a rows frame were tried and withdrawn: four 64² icons put a
tray past its 64 KiB region before a single row, every frame was refused, and
the menu froze on the last one that fitted. The same tray is about 10 KB of
names now.

#### For the next wire version

Recorded, not built. Not worth moving a byte of this kind on its own; it goes
in when something else forces one.

- **Raise `MAX_ROWS`.** The tray sends every item's menu in one frame, and
  three ordinary tray items (a VPN client with an exit-node list, a file-sync
  client, a phone bridge) can use over 200 of the 256, so a fourth chatty
  application truncates. The truncation is honest (one witness, the rest of
  the frame published) but it is a menu with rows missing from the bottom.
  1024 costs 48 KB of rows in the worst case, well inside `WIRE_MAX_BODY`,
  but not inside `ROWS_ARENA_BYTES` with labels beside it, so the region
  grows with it (64 KiB holds about 900 of today's rows).

### Action: `WIRE_ACTION`

What the bar writes to a sidecar's stdin when the caustic decided something
about one of its rows. Fixed size, 32 B. The caustic names the kinds directly:
an `ACT_SIDECAR` (*The action ring*) carries `slot, row, kind, button, pos, value`, and the
process copies the last six into this body (dividing `pos` by the scale)
without looking at them.

```
struct ActionBody {      // 32 B
    uint  row;           // 0   the Row.id the sidecar gave
    uint  kind;          // 4   ACTION_*
    uint  button;        // 8   BTN_* for a click, 0 otherwise
    uint  _rsvd0;        // 12
    float2 pos;          // 16  logical px in the bar surface (a StatusNotifierItem Activate(x, y) wants it)
    float value;         // 24  ACTION_SET_VALUE: the new value, in the row's own min..max
    uint  _rsvd1;        // 28
}
```

**Kinds.** `ACTION_ACTIVATE` 1, `ACTION_SECONDARY` 2, `ACTION_MIDDLE` 3 (a
click on a row, by button), `ACTION_SET_VALUE` 6 (a slider moved),
`ACTION_MENU_OPEN` 7, `ACTION_MENU_CLOSE` 8, `ACTION_TOAST_SHOWN` 9.
(Kinds 4 and 5 are unused: a preview is between the caustic and the bar
process and never reaches a sidecar.)

**`ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`** are the two kinds the process
writes rather than copies, on `ACT_POPUP_OPEN`/`ACT_POPUP_CLOSE` with a slot
(*The action ring*): the bar tells the sidecar that owns a menu when it opens and when it
closes. `row` is the item whose menu it is, the same row both times; `pos`,
`button` and `value` are zero. **A sidecar may ignore both**, and
`caustics-tray`, `caustics-audio` and `caustics-bluetooth` do, without a witness. Two
sidecars want them: `caustics-network`, which scans for the access points in
range while its menu is up and stops when it closes, and `caustics-power`, which
reads the backlight when the battery's menu opens and once a second while it
stays up, and never while it is closed. The dock's own right-click menu sends
neither: its rows are the bar's, and there is no sidecar at the other end.
`ACTION_MENU_CLOSE` is sent however the popup's life ends (a click elsewhere,
a leaf row performing, the compositor dismissing it, the caustic moving to
another popup) and exactly once. A bar that exits instead closes the pipe,
which is the EOF every sidecar already reads.

**`ACTION_TOAST_SHOWN`** is the caustic's: a toast (*The action ring*, `POPUP_TOAST`) that
the compositor **mapped** has ended, and `row` was one of the rows it showed.
One action per row, for every row the toast took when it opened, sent in the
frame it ends by any route. A toast that never mapped (refused, or replaced
before it was) sends none, and a row that arrived while the toast was up is
never among them: it was held behind it and not on screen. `caustics-notify`
marks exactly those notifications shown, so the toast does not come back with
them and a notification held behind it is the next toast. A sidecar that
keeps no such state ignores the kind.

The bar never waits for a reply; the sidecar's reply is its next `Rows`
frame. A sidecar that ignores stdin still gets EOF on it when the bar exits,
which is the one thing every sidecar must read.

`Pipe::send` is called **from the render thread**, so it may never wait. The
child's stdin is taken out of the `Child` at spawn and put in non-blocking
mode, and the frame is 44 bytes, under `PIPE_BUF`, which POSIX writes whole or
not at all, so the call is one syscall that either delivers the action or
says why not. A child that is not reading (`WouldBlock`), a child that has
gone (`BrokenPipe`), a name with no live child: each is one `action dropped:`
witness and nothing else. A sidecar that wants its actions therefore has to
read its stdin.

### Writing a sidecar

```
caustics_bar_abi::buffer   // *The buffer*: the structs and constants through abi_struct!/abi_consts!, the Decl tables
caustics_bar_abi::wire     // *The wire*: WireHeader, Samples, ChartRow, Rows/RowsHeader/Row, ActionBody, …
                       //     Frame, read(&mut impl Read) -> io::Result<Option<Frame>>,
                       //     write(&mut impl Write, &Frame) -> io::Result<()>, RowsBuilder, greet()
pub use buffer::*;
rendered()             // *Declared once*: renders bar_abi.slang and bar_wire.slang, named by RENDERED
write_rendered(dir)    //     writes them into dir; rendered_dir() writes them once into a cache directory
```

`Frame` is `Samples | Rows | Action | … | Hello | Skipped(Skipped)`. A skipped
frame is a **variant**, because `read`'s `Ok(None)` already means a clean
close and its `Err` already means "restart the child", so rules 5–7 have
nowhere else to land. `Skipped { kind, why }` carries `why` as an enum
(`Skip::{Kind, Length, Body}`), which is what lets the reader's caller key
"one witness per kind per child" on it and count skips.

The crate depends on `punchcutter-core` for the plain byte views and the
declare-once macros, and on nothing else. `SAMPLES_SIZE` (8336) is the `Samples` body's size, the constant a
reader checks a frame's `len` against. The crate has its own tests for the
wire (round trips, every reader rule above with a crafted stream, the
builder); the buffer's layout and compile tests stay in `bar/src/abi/tests.rs`
because they need slangc; the test that the rendered modules on disk are the
rendering is this crate's.

**Writing a sidecar** is: depend on `caustics-bar-abi`; greet (*Hello*); build a
`Frame` and `wire::write(&mut stdout, &frame)` whenever the picture changes;
read stdin with `wire::read` on a thread and exit on `Ok(None)`. Write each
stderr witness as **one `write_all` of a whole line**, never `eprintln!`:
`Stderr` is unbuffered, a format string is several writes, the fd is the
bar's own, and journald makes an entry of whatever lands between two
newlines, so one witness can become three. `sysmon/` is the worked example
for samples. `bar/abi/examples/rows_demo.rs` is the worked example for rows: a
hundred-line applet with an icon, two menu rows and a slider that echoes
every action to stderr and republishes on each one, and the test harness's
proof that the rows path works without any real applet.

### Bytes: `WIRE_BYTES`

A run of bytes for one of the caustic's buffers, at an offset. This is the
frame that lets a sidecar *fill a GPU buffer* without the bar understanding
what is in it: the curves sidecar writes the font this way, and appends an
icon's curves the same way later. A body larger than `WIRE_MAX_BODY` is
chunked by the writer into several frames at consecutive offsets.

```
struct BytesHeader {    // 16 B
    uint buffer;        // 0   BUF_*: which buffer
    uint offset;        // 4   byte offset the run lands at
    uint len;           // 8   bytes following this header
    uint total;         // 12  the buffer's live length once this run has landed (≥ offset + len)
}
// then `len` bytes
```

| `BUF_*` | | binding | the engine's buffer |
|---|---|---|---|
| 1 | `BUF_POINTS` | set 1, 0 | points |
| 2 | `BUF_BANDS` | set 1, 1 | bands |
| 3 | `BUF_BAND_CURVES` | set 1, 2 | band curves |
| 4 | `BUF_GLYPHS` | set 1, 3 | glyph records |
| 5 | `BUF_SHAPE_FONT` | set 2, 0 (the `font` half of `ShapeParams`) | `ShapeFont` |
| 6 | `BUF_OT` | set 2, 1 | the arena |

`BUF_LAST` = 6. The six are the text engine's buffers, byte for byte: their
element types, strides and contents are punchcutter/docs/arena.md, *The curve
buffers* and *The arena*, and this frame only says which one a run lands in.
`BUF_OT` is the arena: a face directory and behind it every face's `cmap`,
`hmtx`, `GDEF`, `GSUB` and `GPOS` exactly as they sit in the font file.
Nothing on the bar's side of the wire, and nothing in `caustics-glyphs`
either, reads a lookup. The arena's header (`shape_abi.slang`) is a wire
contract too, since `caustics-glyphs` fills that arena over this frame, so it
is one of the files the ABI hash covers (*The ABI hash*).

**Reader rules**: `buffer` 0 or past `BUF_LAST` → skip (body); `len` ≠ the
bytes present → skip (length); `offset + len > total` → skip (body). **The
bar's rules**: the GPU buffer is grown to `total` (with headroom) before the
write and its bind group re-created; the caustic's counts (`glyph_capacity`,
the `ShapeFont` scalars, the arena's face count) come from the writer's own
`total`, the `BUF_SHAPE_FONT` body and the arena's own header, never from the
bar reading a byte of a table. The bar never validates the contents: a
malformed table draws wrong and stalls nothing, because every index the
engine's modules take is bounded and the arena's stated length is clamped to
the words the driver actually bound, so a header that arrives before its
tables cannot send a walk off the end (punchcutter/docs/arena.md, *Bounded in
time*).

After the first full write, `caustics-glyphs` sends only what changed, the
way punchcutter/docs/arena.md, *Keeping a GPU copy current*, describes: each
curve buffer's tail, the glyph records a cover filled in place below what was
already sent, and for `BUF_OT` three pieces: the front block at offset 0
(rewritten whenever a face is opened), a face's fill bitmap in the middle,
and the new face's tables as a tail. The tails go first and everything that
names a glyph after them, because the wire has no transaction.

### Font: `WIRE_FONT`

What the font is, sent once by the curves sidecar before its `Bytes` and
again if the font changes: the facts the text engine's loaded font carries
that the caustic sizes text by, and nothing about the buffers' contents
(punchcutter/docs/arena.md, *The curve buffers*, has those).

```
struct FontBody {       // 32 B
    uint  upem;         // 0   units per em
    float cap_height;   // 4   in em: what the caustic snaps a size against (Params.cap_height)
    uint  glyph_count;  // 8   glyph ids 0..glyph_count are the font's own
    uint  icon_base;    // 12  ids from here are icons the same sidecar appends (≥ glyph_count)
    uint  family_len;   // 16  UTF-8 family name following
    uint  _rsvd0, _rsvd1, _rsvd2; // 20
}
// then `family_len` bytes
```

Reader rules: `upem` 0, `cap_height` ≤ 0, `icon_base < glyph_count` → skip
(body); a family name past `MAX_NAME_LEN` (1024) → skip (body).

### Glyph: `WIRE_GLYPH`

The curves sidecar's answer to a `Request` of kind `REQ_GLYPH`: the name
became a glyph id (its curves arrived, or are arriving, as `Bytes`), or it
could not. A symbolic icon's filled paths go through the text engine's icon
loader into the same curve buffers as the font's glyphs, so an icon is one
more record in the merged glyph space (punchcutter/docs/arena.md, *The merged
glyph space*).

```
struct GlyphBody {      // 16 B
    uint gid;           // 0   the glyph id (meaningless under GLYPH_MISSING)
    uint flags;         // 4   GLYPH_MISSING = 1: no theme has it, or it is not curves
    uint name_len;      // 8   the name asked for, UTF-8, following
    uint _rsvd0;        // 12
}
// then `name_len` bytes
```

Reader rules: an empty name → skip (body). The frame goes whole into the
answers arena (*The arenas*); the caustic joins it to the name it wanted; the bar
asks once per name and remembers (*Request*).

### Request: `WIRE_REQUEST`

**The bar → a sidecar**, on its stdin, beside `Action` (*Action*): resolve this
name. The bar never waits. The answer is a `Glyph`, an `App` or a `Pixmap` on
the sidecar's stdout like any other frame, and until it arrives the bar draws
without it.

```
struct RequestBody {    // 16 B
    uint kind;          // 0   REQ_*
    uint name_len;      // 4   UTF-8 following
    uint arg;           // 8   the kind's one word: REQ_ACTIVATION's row id; 0 for the others
    uint _rsvd1;        // 12
}
// then `name_len` bytes
```

| `REQ_*` | | name | answer |
|---|---|---|---|
| 1 | `REQ_GLYPH` | a themed icon name | `Glyph` |
| 2 | `REQ_APP` | an app id | `App` |
| 3 | `REQ_ICON` | a themed icon name that is not curves | `Pixmap` keyed by the name |
| 4 | `REQ_COVER` | the bytes of a string | the curves sidecar appends what its font lacks, and answers in `Bytes` |
| 5 | `REQ_PROGRAM` | the swapchain format's slangc define | a generation of `Module`s |
| 6 | `REQ_ACTIVATION` | an xdg-activation token | none |

`REQ_LAST` is 6; a kind past it is skipped.

**`REQ_COVER`** is the one whose `name` is not a name: it is the UTF-8 of a
text block the caustic flagged as having a glyph the font lacks (an
`ACT_COVER` naming a range and the buffer it is in, *The action ring*), copied out of the
bar's own copy of those bytes, capped like every other name by
`MAX_NAME_LEN`. The bar decodes none of it. Which codepoints are missing,
which face on the fontconfig chain has them, and what they cost is the
sidecar's business (the text engine's cover, punchcutter/docs/arena.md, *The
merged glyph space*), and the answer is the curve buffers, the arena and
`BUF_SHAPE_FONT` growing under *Bytes*'s incremental rule. The run then
re-shapes against the bigger font, loses its flag, and leaves the list.

A sidecar answers every request it understands, including a repeat (the bar
may have restarted, or the sidecar may have); one it does not is skipped like
any unknown frame. Written with the same one-syscall rule as an action: a
child not reading its stdin costs a witness, never a wait. **Unlike an
action, a request is remembered by the bar's pipe** (`Pipe::request`,
deduplicated, at most 4096). One written before the child has a stdin (the
bar's first build runs a millisecond before its sidecars do) is written when
the child starts, and every child after a restart is sent the whole list
first, because its predecessor's answers named glyph ids and slots it knows
nothing about. An action is never replayed: a click for a child that is gone
is stale.

**An answer may come again, unasked.** The bar asks once per name, so a miss
would otherwise stand for the life of the process. `caustics-glyphs` and
`caustics-apps` look again at what they answered every 30 s (a name no theme had,
a file an answer was read from whose mtime moved, an `APP_MISSING` id when an
application directory changed) and write the new `Glyph`, `App` or `Pixmap`
when it differs. Nothing in the bar forgets: a `Glyph` replaces the answer
held for its name, an `App` the one for its id, and a `Pixmap` overwrites the
slot under its key. A pixmap cannot be taken back; an icon whose file went
away keeps its last picture.

**`REQ_ACTIVATION`** is not a question either, and nothing answers it: it is
an **`xdg_activation_v1` token** for a click. A sidecar asks for them row by
row with `RFLAG_ACTIVATION` (*Rows*); `caustics-notify` flags only the rows whose
click invokes an action, so do-not-disturb, clear-all and settings cost no
token. When the caustic sends a sidecar an `ACT_SIDECAR` of kind
`ACTION_ACTIVATE` with `token` set, the process forwards the action as always
and, beside it, asks the compositor for a token (`get_activation_token`,
`set_serial` with the click's button serial and the seat, `set_surface` with
the bar's layer surface, `commit`). On the token's `done` the process writes
`REQ_ACTIVATION` with `arg` the row the action named and `name` the token,
and destroys the token object. So the token always arrives **after** its
action on the same stdin, one compositor round trip later. The sidecar
decides how long to wait for it (`caustics-notify` holds the click's
`ActionInvoked` for 100 ms at most, so a compositor that grants nothing costs
the click nothing) and what to do with it (emit `ActivationToken` before
`ActionInvoked`, as Desktop Notifications 1.2 orders). Unlike every other
request it is **never remembered or replayed**: a token is the click's, and a
click for a child that has gone is stale. The process's witnesses are
`activation: token for row={} ({} bytes)` and `activation: no token ({why})
…`.

**`REQ_PROGRAM`** names not a thing to look up but the **program** the bar
can run: slangc's swapchain-format define, exactly as `Gpu::dst_format_args`
spells it. The bar sends it once, when the device comes up and it first knows
the format; `caustics-reload` compiles for it on receipt and on every save
thereafter, and compiles nothing before it. The replay above is what makes a
restarted compiler work with no code in the bar for restarting it.

### App: `WIRE_APP`

The apps sidecar's answer to `REQ_APP`: what the desktop entry says.

```
struct AppBody {        // 32 B
    uint flags;         // 0   APP_*
    uint app_len;       // 4   the app id asked for, echoed
    uint name_len;      // 8   Name=, or the app id under APP_MISSING
    uint icon_len;      // 12  Icon=: a themed name (APP_ICON_SYMBOLIC: ask for it as REQ_GLYPH), or empty
    uint path_len;      // 16  the .desktop file, for `gio launch`; empty under APP_MISSING
    uint exec_len;      // 20  Exec= as argv words separated by NUL, field codes stripped; empty if none
    uint _rsvd0, _rsvd1; // 24
}
// then the five fields' bytes in that order
```

The frame goes whole into the answers arena (*The arenas*); the caustic joins it to
the dock entry whose app id it echoes, and takes the label and the icon name
from it. The process reads `path` and `exec` off the same typed frame when it
launches, which is I/O and its own. `APP_MISSING` 1: no entry (the dock shows
the app id). `APP_ICON_SYMBOLIC` 2: `icon` is a symbolic name and is curves,
so the bar asks the curves sidecar. `APP_ICON_PIXMAP` 4: the icon is pixels,
and a `Pixmap` keyed by the app id is sent by the same sidecar (before or
after this frame). Reader rules: an empty app id → skip (body); any field
past `MAX_NAME_LEN` → skip (body).

### Pixmap: `WIRE_PIXMAP`

A keyed image for one slot of the fixed-slot array (`ImageFact`, *The facts*): a
colour application icon, a themed icon that is not curves, a row's pixmap
icon (*Pixmap icons*). **Fitted by the writer** to `IMAGE_SLOT_PX`² (64²) and
**straight** RGBA8, not premultiplied: the caustic premultiplies at the
sample, one multiply per texel, so the bar touches no pixel.

```
struct PixmapBody {     // 16 B
    uint key_len;       // 0   UTF-8 key following: the app id (for an App), the icon name (for REQ_ICON), or a minted key
    uint width;         // 4   = IMAGE_SLOT_PX
    uint height;        // 8   = IMAGE_SLOT_PX
    uint _rsvd0;        // 12
}
// then `key_len` bytes, then width * height * 4 bytes of straight RGBA8
```

Reader rules: `width` or `height` ≠ `IMAGE_SLOT_PX` → skip (body); an empty
key → skip (body); the rgba length wrong → skip (length). A body is 16 + key
+ 16384 bytes.

**Keys.** A key is unique bar-wide. An answer to a request is keyed by what
was asked; a key a sidecar **mints** leads with its own name, and the
reference form is `<sidecar>:<fnv1a64 of the rgba, 16 hex digits>`
(`caustics_bar_abi::wire::pixmap_key`): content-addressed, so an icon that comes
back unchanged is the same key and is never re-sent, and one that changed is a
new key. A sidecar sends a minted key's `Pixmap` once per process, before the
first frame that names it.

**The store.** The process keeps every `Pixmap` by key, asked for or not (a
minted key arrives before any build has named it). A want for a stored key
(`ACT_WANT_ICON` with `REQ_ICON`, or `REQ_APP` whose `App` answer is held)
lands a slot and **sends no request**. A slot evicted from the image array
takes its name's asked mark with it, themed name and app id alike, so the
caustic's next want for a picture it still draws lands it from the store.
Bounded at 256 keys and 4 MiB, the oldest inserted let go past it with one
witness; a sidecar that exits takes its keys with it. The caustic's
resolution order: a name is asked of `caustics-glyphs` first (`GLYPH_MISSING`,
fast), then `REQ_ICON`, which the store answers.

### Module: `WIRE_MODULE`

The compiler sidecar's answer to `REQ_PROGRAM`, and to every save thereafter:
**one piece of one entry point of a fresh compile of the bar caustic**. The
bar reads no `.slang` file, watches no mtime and links no compiler. SPIR-V and
reflection arrive as bytes like a font does, and go to the device.

```
struct ModuleHeader {   // 40 B
    uint generation;    // 0   which compile; complete when `count` entry points have arrived whole
    uint index;         // 4   this entry point's place in the caustic's entry list; < count
    uint count;         // 8   entry points in this generation (`ENTRIES`, seven)
    uint format;        // 12  wire::format_code() of the slangc define this was built for
    uint spirv_len;     // 16  the whole module's runs, repeated in every piece
    uint reflection_len;// 20
    uint name_len;      // 24  the entry point's name, from the entry list
    uint offset;        // 28  byte offset of this piece within the payload
    uint total;         // 32  name_len + spirv_len + reflection_len
    uint _rsvd0;        // 36
}
// then this piece of the payload, which is `name_len` bytes of entry
// point, then `spirv_len` bytes of SPIR-V, then `reflection_len` bytes
// of slangc's reflection JSON, in that order
```

**Chunked.** A module is one frame where it fits under `WIRE_MAX_BODY` and
several, **in order**, where it does not (`bar_shape` with its reflection is
over 1.7 MiB). A writer cuts it (`Module::chunks`), a reader puts it back
together (`wire::ModuleAssembly`), and a piece whose `offset` is not where
the last one ended starts the payload over rather than splicing two compiles
together.

The entry list is `ENTRIES` in `caustics-bar-abi`, the ABI crate rather than the
bar, because it is a thing two separately built processes must agree on.
Both ends read the same one: `bar/build.rs` bakes it, `caustics-reload` compiles
it, `bar/src/gpu.rs` names the pipelines from it.

**A generation is all or nothing.** `Gpu::reload_all` rebuilds every pipeline
or none, so the bar collects entry points by generation and swaps only when
`count` have arrived **whole**, with the reflection of index 0 (the
*parameters* in slangc's reflection are the program's, not an entry point's)
and the `getStringHash` literals merged from all of them (a literal table is
per entry point, because reflection reports only what an entry point
reaches). A generation left incomplete when a newer one starts is dropped: a
compiler only abandons one by being killed mid-write or by recompiling on a
newer save, and either way the older one is stale.

**A failed compile sends nothing.** Silence is the answer: the running
caustic stays, and slangc's diagnostics go to the sidecar's *stderr*,
verbatim, which the bar's unit inherits, so they land in the journal.

Reader rules: `count` 0, past `MAX_MODULES` (16), or `index >= count` → skip
(body); `spirv_len` 0 or not a multiple of four → skip (body); a `name_len`
of 0 or past `MAX_NAME_LEN` → skip (body); the three runs not adding up to
`total`, or `offset + len` past `total` → skip (body). Then the bar's own
three, each one witness and no swap, applied to the assembled module: a
`format` that is not this session's, a `count` that is not this bar's entry
count, a name that is not `ENTRIES[index]`.

### The ABI hash

Per-kind version words cannot say the one thing that matters: whether the two
ends mean the same thing by the *same* bytes. A bar once ran beside a curves
sidecar from an older build, which wrote a flattened `cmap` array as a `Bytes`
frame under a buffer number the newer bar had given the OpenType arena. The
version words agreed on both sides and the frame was well-formed. The shaper
read a table directory out of a sorted list of `(codepoint, gid)` pairs, took
152 ms a dispatch, and the desktop's cursor stuttered until the bar was
stopped.

So there is **one number for the whole contract**: `ABI_HASH`
(`caustics-bar-abi`), hashed at build time over the four inputs that *are*
the contract:

1. `bar/abi/src/buffer.rs`, the buffer contract's declaration, which
   `bar_abi.slang` is rendered from;
2. `bar/abi/src/wire.rs`, the wire's declaration: the part the GPU reads,
   rendered into `bar_wire.slang`, and the framing, the kinds, the caps and
   the `BUF_*` numbers, which have no Slang half;
3. `shape_abi.slang`, the OpenType arena's header, rendered by the text
   engine from the same kind of tables and taken as the copy compiled into
   the engine (`punchcutter_core::modules::SHAPE_ABI`, a build-dependency of
   `caustics-bar-abi`), so a dependency bump that changes the arena is a new
   set;
4. `bar/abi/src/entries.rs`, the entry list a `Module` frame is named by.

The bar's own files are hashed as source, comments and all, which is a
superset of hashing what they render: coarse in the safe direction, since a
comment edit makes a new set, and a set is installed whole
(`bar/install.sh`).

What is left permanently stable is exactly two things, `WireHeader` and
*Hello*'s body, because they are what a bar reads *before* it knows whether
the child is its own.

### Hello: `WIRE_HELLO`

**A sidecar's first frame**, and the only kind it may write first.

```
struct HelloBody {      // 16 B
    uint hash_lo;       // 0   ABI_HASH, low 32 bits
    uint hash_hi;       // 4   ABI_HASH, high 32 bits
    uint name_len;      // 8   the name that follows, ≤ MAX_NAME_LEN
    uint flags;         // 12  reserved: written 0, read by nobody
}
// then name_len bytes of UTF-8: what the sidecar calls itself
```

The hash crosses as two words because the plain byte views are a promise
about `u32` leaves and no padding, and because every other number on this
wire is a word.

The bar's rule, in `bar/src/feeders/pipe.rs`:

- `Hello` with this bar's `ABI_HASH` → one `info` line naming the child and
  the hash, and the stream is read from there on;
- `Hello` with any other hash → one `error` naming the child, the name it
  calls itself and **both** numbers; the child is killed; **no restart**, and
  its rows leave the bar;
- any other kind before a `Hello` → the same, with the kind in the line;
- a second `Hello` later → one `warn` and nothing else.

**No restart** is the part that matters. A sidecar from another tree is not a
crash: the binary is installed and simply is not this bar's, and the ordinary
doubling backoff would replay the same refusal every second for the life of
the session. It is not `absent` either, and the health line says `refused`
rather than `absent` for it.

`name` is for the witness alone. It is what the sidecar calls itself, which
need not be the file stem the bar spawned.

Writing one is one line in a sidecar's `main`, before its `run`:

```rust
caustics_bar_abi::wire::greet(env!("CARGO_BIN_NAME"));
```
