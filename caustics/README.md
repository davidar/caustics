# caustics

The bar's GPU programs, and the canvas's, in [Slang](https://shader-slang.org/). A **caustic**
is a compute program that holds policy: here, everything the bar draws and
every decision about what it draws (docs/architecture.md, *The caustic*). The
programs are compiled to SPIR-V by Slang, in process through the text
engine's `punchcutter-slang`: at build time by `bar/build.rs`, which bakes the caustic into the bar binary, and at run time by the
`caustics-reload` sidecar, which recompiles on every save. They are loaded through
wgpu's SPIR-V passthrough. Slang rather than WGSL because Slang has modules,
generics, interfaces, string literals whose hashes reach the host through
reflection, and a reflection API that lets the Rust side check every shared
struct's layout against the compiler's.

## The files

The caustic is one program in several modules, every one of them hand-written
and hot-reloaded. They come in three layers: the entry points, the bar's own
machinery, and one module per **subject**, a thing the bar shows, named after
the sidecar (or the part of the bar) that feeds it. A subject's module holds
everything about it, its size, its rows, its pixels, its clicks and its
names, so that a subject is found in one place.

| file | what it is |
|---|---|
| `bar.slang` | the seven entry points (`bar_build`, `bar_compose`, `bar_shape`, `bar_apply`, `bar_visibility`, `bar_sample`, `bar_draw`), each a short sequence of calls into the modules below, the witnesses' naming, and the oracle's `#include` |
| `bar_state.slang` | what every module reads and writes: the bindings of sets 0 and 2, the `Private` struct and its buffer (with one scratch struct per subject), `ensure_private`, the action and witness rings, the witness-name vocabulary, the scale and the item and id helpers, the rows arena's readers |
| `bar_popups.slang` | the popup as a window: asked for, mapped, moved, ended, and the menu level it walks |
| `bar_layout.slang` | the layout engine: what an item's rect is made of, the strip's wings, `place`, the hit test, a row's body wrapped into lines, the springs |
| `bar_paint.slang` | the drawing every subject shares: the premultiplied rules, the chip, one block (a run, a glyph, an image), an item's content, a row's body |
| `bar_table.slang` | the item and block tables' machinery: the build's allocators, the stitch, the wants, the answers index, composed bytes, the shape pass's block, the build's own witnesses |
| `bar_dispatch.slang` | the one module that knows which subjects exist: one function per place a pass turns on a kind, each a switch calling the subject's own, and the subjects' kinds and tags bound as values |
| `sysmon.slang` | the charts (`caustics-sysmon`): the strip, each chart drawn from the samples body, the chart numbers' popup. The one reader of `samples` |
| `notify.slang` | the toasts: a sidecar chip's attention rows shown without a click (`caustics-notify` is the sidecar that asks) |
| `clock.slang` | the clock and its calendar |
| `wake.slang` | the pointer's wake on the slab |
| `dock.slang` | the overview button, the dock's entries and windows, an entry's menu, the hover preview |
| `applets.slang` | the generic rows machinery every sidecar's applet rides on: chips, their levels, menus, menu rows, sliders, a row's body |
| `bar_tunables.slang` | **the bar's configuration**: every knob as a `public static const`; edit it to configure the bar |
| `bar_model.slang` | the caustic's private vocabulary: item, block and element kinds and flags, the `Id` type, the dock-menu verbs, the item accessors. Not a contract: no Rust reads it |
| `bar_check.slang` | the oracle's invariants, a test-only eighth entry point (docs/testing.md) |

Three rules keep the subjects apart. A subject module reads only its own
frames and its own scratch (`Private.sysmon`, `Private.notify`, …). It names
no other subject: what two subjects share is the bar's, in one of the
`bar_*` modules. And it names none of the bar's kinds or id tags: it declares
its own as link-time constants (`extern static const uint SYSMON_CHART;`),
and `bar_dispatch` binds them (`export static const uint SYSMON_CHART =
ITEM_CHART;`), which Slang folds like any constant. The bar's own modules
still name kinds where the bar's policy is about one (the wings drop charts
from the end; `item_is_button` lists the kinds that take a click).

**The text engine's modules are imported, not kept here.** `text` (glyph and
icon coverage, the Slug algorithm), `shape` (the shaper), `ot` (OpenType's
binary format, walked in place) and `shape_abi` (the arena's header) are
punchcutter's, compiled into `punchcutter-slang` (`punchcutter_slang::modules`);
punchcutter is the text engine, a dependency. Every compile through that
crate serves them from memory, searched last, after this directory and the
bar's rendered contracts; `punchcutter_slang::slang_dir()` writes the same
copies to a cache directory, once per version of the engine, for whatever
wants them as files.

**The bar's two contracts are rendered, not kept here.** `bar_abi` (the
buffer contract between the bar process and the caustic, docs/abi.md, *The
buffer*) and `bar_wire` (the frames a sidecar writes that the caustic reads in
place, *The wire*) are rendered from the Rust declarations in
`bar/abi/src/buffer.rs` and `bar/abi/src/wire.rs` by
`caustics_bar_abi::rendered()`. Every compile puts them on its search path
after this directory: `bar/build.rs` writes them into a directory under
`OUT_DIR`, and the processes that compile at run time take
`caustics_bar_abi::rendered_dir()`. To read them after a build, look in
`~/.cache/caustics/rendered/<abi-hash>/`. To change them, edit the Rust.

## The import graph

```
bar.slang ── bar_dispatch ─┬─ sysmon, notify, clock, wake, dock, applets
     │                     └─ (every bar_* module below)
     │      each subject ─── bar_table, bar_paint, bar_layout, bar_popups, bar_state
     │                       as it needs, and never another subject
     ├─ bar_table ── bar_state
     ├─ bar_paint ── bar_layout ── bar_popups ── bar_state
     ├─ bar_state ─┬─ bar_model ── bar_abi ── bar_wire
     │             ├─ bar_tunables ── bar_model
     │             └─ shape, text                (punchcutter's)
     └─ #include "bar_check.slang"               (only under -DCAUSTICS_BAR_CHECK;
                                                  imports sysmon for its chart checks)
```

Slang refuses a cycle, so the layers are an order: `bar_state` first, then
`bar_popups`, `bar_layout`, `bar_paint` and `bar_table`, then the subjects,
then `bar_dispatch`, then `bar.slang`. That order is what shapes the code
where a pass of the bar's needed a subject's answer: the pass is split, and
`bar.slang` calls the bar's half and `bar_dispatch` for the rest (the layout
places the strip, then asks the popup's subject; `place` records an
element, and its `layout:` line is said after the layout, by `bar.slang`,
which can name it). Where a bar module must give a subject its word in the
middle of its own work, it declares the function `extern` and `bar_dispatch`
defines it with `export`, as it binds the kinds: `close_popup` and
`request_popup`, which every subject calls, end a toast through
`dispatch_popup_ending`. A fact the bar's layers need about one subject's
rows rides the rows themselves: a toast's row carries the body lines it
opened with (`item_body_cap`), written by `notify` before the layout.

`bar_tunables` imports only `bar_model`, which re-exports `bar_abi`, which
re-exports `bar_wire`, so a tunables file reaches every constant it can name
(`EDGE_*`, `ITEM_*`, `APPLET_*`, the `ACT_*` verbs) through one import.
`shape` and `text` know nothing about the bar: `text` declares its four curve
buffers in descriptor set 1 and `shape` declares no bindings, and the bar binds
their buffers and passes the handles in (punchcutter/docs/arena.md, *A shaping
dispatch*).

## Conventions

- **One module declares the bindings, one the entry points.** Every
  `[[vk::binding]]` of the caustic's own is in `bar_state`, and a binding
  declared there is the same global parameter in every module that imports
  it; every `[numthreads]` is in `bar.slang`. `bar_abi`, `bar_wire`,
  `bar_model` and `bar_tunables` are importable anywhere at no cost.
- **Bindings are named, not numbered, on the host side.** The binding numbers
  are literals in `bar_state.slang` (an attribute cannot take a constant). The bar
  builds its bind groups by reading each binding's name out of Slang's
  reflection, and refuses a hot reload whose bindings changed.
- **No vectors in a shared struct.** Every field of anything in `bar_abi` or
  `bar_wire` is a `uint`, a `float` or an array of them. A 3-vector aligns to
  16 bytes in std430, and reflection once put one at the wrong offset.
- **Strings are literals.** A label the bar shows that is not someone else's
  data is `getStringHash("…")`. The reflection carries the literal's text and
  the bar copies it into the strings buffer. Witness lines are format literals
  the same way (docs/abi.md, *The witness ring*).
- **Everything a sidecar sends is untrusted.** The caustic bounds every read
  of what a sidecar wrote by what was bound, and the engine treats the
  OpenType arena the same way: every offset clamped, every list capped and
  charged against a budget before its first iteration
  (punchcutter/docs/arena.md, *Bounded in time*). A loop never uses a budget
  charge as its exit condition: one driver miscompiled exactly that shape.
- **Two swapchain formats.** The caustic compiles once with
  `-DDST_FORMAT_RGBA8` and once with `-DDST_FORMAT_BGRA8` (SPIR-V has no BGRA
  storage-image format, so that variant declares the image format-unknown).
  The bar picks the pair matching the surface it negotiated.

## The tunables

`bar_tunables.slang` is the only configuration the bar has, and its own
documentation: each constant carries a comment saying what it does. To change
anything, edit it in your checkout and save; the running bar moves. It is a
module like the others in this directory, and the compiler is its validator:
an edit that does not compile keeps the last good bar and prints Slang's
diagnostic, naming this file, to the journal. Your edits live in your
checkout, so an update is a merge like any other.

Two tunables reach the bar process, both as one message on the caustic's
first frame and again whenever a reload changes them: `EDGE` (`EDGE_TOP`,
the shipped default, or `EDGE_BOTTOM`) and `BAR_HEIGHT`. Everything else
(colours, sizes, the wings' contents and order, the dock menu's rows, the
clock's words, the springs, the shape watchdog) is read by the caustic alone.

## The canvas

The directory holds a second caustic: the canvas's (docs/canvas.md), which
draws the wallpaper on every output, cross-fades between two pictures, draws
the idle dim on the overlay the host puts over every window while a dim is
up, and draws the prompt's panel on the pane the host puts up while a prompt
is. It is four files, all named `canvas*`, and imports nothing above but the
text engine's modules:

| file | what it is |
|---|---|
| `canvas.slang` | the three entry points (`canvas_shape`, before a frame when the text changed: one text block a thread; `canvas_apply`, once per frame: the fade's alpha and the dim's from the last show, the last dim and the time, the witnesses, `wants_redraw` for the surface drawn, whether the dim is at its depth, the prompt's layout and the runs to cover; `canvas_draw`, over one surface by `params.target`: on the background the two source pictures at that alpha, opaque; on the overlay the same wallpaper, darkened towards the dim colour by `DIM_DARKEN`, at the dim's alpha, premultiplied, and nothing while a prompt is up; on the prompt's pane the wallpaper darkened by `PROMPT_DARKEN` and the panel) |
| `canvas_state.slang` | the canvas's bindings (`state`, `params`, `facts`, `dst`, `image_a`, `image_b`, `tex_sampler`, `private`, `runs`, `literals`, `prompt_text`, and in set 2 the shaper's `shapeParams`, `shapeOt`, `shapeUtf8`), the gathered `ShapeTables`, the literal lookup, `CanvasPrivate` with the shaped runs and the prompt's layout, the witness ring, the two-word time arithmetic |
| `canvas_text.slang` | the text: five fixed blocks (title, ask, echoed text, line, dot), shaped by the engine at one reference size into the canvas's own small arena, painted one left-aligned line at a position in a colour, a long line wrapped once at a space, an echo-off answer as one dot glyph drawn a count of times, and the runs the font lacks a glyph for |
| `canvas_tunables.slang` | **the canvas's configuration**: the dim colour, how far a dim and a prompt darken the wallpaper, the fade's curve, how a picture is fitted to an output, the prompt's sizes and inks, the shape pass's watchdog |

Its contract is `canvas_abi`, rendered from `bar/abi/src/canvas.rs` as the
bar's two are rendered (docs/abi.md, *The canvas's buffer*). `caustics-reload`
compiles it when the canvas host asks for its program, and watches only the
`canvas*` files for it; the bar's caustic is every other file, so a save of one
never recompiles the other.

**The secret never enters it.** An echo-off ask reaches `canvas_draw` as a
count of characters, and the dots are one shaped glyph drawn that many
times; the typed bytes stay in the host (docs/canvas.md, *Why a caustic*).

**Why it shares no module with the bar.** Every `bar_*` module, the paint
rules in `bar_paint` included, imports `bar_state`, which declares the bar's
bindings; a module that imports it binds the bar's whole descriptor set. So
the canvas carries its own small paint (one sample and one blend a pixel,
a rounded rect, in `canvas.slang`) and its own text layout
(`canvas_text.slang`); what the two caustics share is the text engine's
`text` and `shape`, which declare no bindings of the caustic's and are
handed their buffers under the same names in both. A paint module both
could share would have to be **binding-free** the same way: functions of
their arguments alone (colours, coverage, a rounded rect's distance, the
premultiplied rules), with every buffer and texture passed in as a
parameter, and with its constants either passed in or read from a tunables
module both import. The bar's `bar_paint` is not that today: its functions
read `params`, `private` and `images` as globals. Splitting it is a
refactor of the bar.

## Hot reload

`caustics-reload` watches this directory: every `*.slang` file of the caustic
its host asked for (the bar's, or the canvas's `canvas*` files), the tunables
among them, listed when the host's program arrives (a module added later is
compiled, because the module that imports it changed, but is not watched
until the sidecar restarts). `bar/build.rs` and the harness's copy into a
guest take the same list. The text engine's modules are not watched: an engine change is a dependency bump and a
rebuild. On a save it compiles every entry point in one link, with Slang in
the sidecar's own process, caches the
result under `$XDG_CACHE_HOME/caustics/reload/` keyed on the bytes that reach
the compiler (the engine's modules included) and the compiler's version (a cached generation is milliseconds; a cold one is
seconds), and sends the SPIR-V and the reflection to the bar as one
generation, which the bar swaps whole or not at all. The caustic's private
state survives the swap, so an open menu stays open.

**A start compiles nothing.** The bar starts on the SPIR-V baked into it
from this directory, and when it tells `caustics-reload` its swapchain
format, the sidecar sends a cached generation for the directory's present
bytes if there is one and otherwise waits for the next save. A restart after
a live edit therefore shows the edited bar within a second.

**A file that does not compile sends nothing.** The running caustic stays and
Slang's diagnostics go to the journal. A reload that changes a binding, or
the size of the caustic's private buffer, is refused with "restart the bar".

Where `caustics-reload` looks: `caustics/` in its working directory, then
`$CAUSTICS_DIR`, then the `caustics/` of the checkout the binary was
built from. An installed bar built from a checkout therefore hot-reloads from
that checkout. With none of the three present it runs the caustic baked into
it and never reloads.
