# caustics

The bar's GPU programs, in [Slang](https://shader-slang.org/). A **caustic**
is a compute program that holds policy: here, everything the bar draws and
every decision about what it draws (docs/architecture.md, *The caustic*). The
programs are compiled to SPIR-V by slangc, at build time by `bar/build.rs`,
which bakes the caustic into the bar binary, and at run time by the
`caustics-reload` sidecar, which recompiles on every save. They are loaded through
wgpu's SPIR-V passthrough. Slang rather than WGSL because Slang has modules,
generics, interfaces, string literals whose hashes reach the host through
reflection, and a reflection API that lets the Rust side check every shared
struct's layout against the compiler's.

## The files

| file | what it is | written by |
|---|---|---|
| `bar.slang` | the bar caustic: the seven entry points (`bar_build`, `bar_compose`, `bar_shape`, `bar_apply`, `bar_visibility`, `bar_sample`, `bar_draw`), every binding, the layout, the springs, the popups, the witnesses | hand |
| `bar_tunables.slang` | **the bar's configuration**: every knob as a `public static const` | hand; edit it to configure the bar |
| `bar_model.slang` | the caustic's private vocabulary: item, block and element kinds and flags, the `Id` type, the dock-menu verbs, the item accessors. Not a contract: no Rust reads it | hand |
| `bar_check.slang` | the oracle's invariants, a test-only eighth entry point (docs/testing.md) | hand |

**The text engine's modules are imported, not kept here.** `text` (glyph and
icon coverage, the Slug algorithm), `shape` (the shaper), `ot` (OpenType's
binary format, walked in place) and `shape_abi` (the arena's header) are
punchcutter's, compiled into `punchcutter-core` (`punchcutter_core::modules`);
punchcutter is the text engine, a dependency. Every compile puts a directory
written from those copies on the include path after this one: `bar/build.rs`
writes it under `OUT_DIR`, and `caustics-reload`, the oracle and the layout
tests take `punchcutter_core::slang_dir()`, a cache directory written once.

**The bar's two contracts are rendered, not kept here.** `bar_abi` (the
buffer contract between the bar process and the caustic, docs/abi.md, *The
buffer*) and `bar_wire` (the frames a sidecar writes that the caustic reads in
place, *The wire*) are rendered from the Rust declarations in
`bar/abi/src/buffer.rs` and `bar/abi/src/wire.rs` by
`caustics_bar_abi::rendered()`. They go on the include path the same way as
the engine's: `bar/build.rs` writes them into the same directory under
`OUT_DIR`, and the processes that compile at run time take
`caustics_bar_abi::rendered_dir()`. To read them after a build, look in
`~/.cache/caustics/rendered/<abi-hash>/`. To change them, edit the Rust.

## The import graph

```
bar.slang ─┬─ bar_abi ── bar_wire        (bar_abi re-exports bar_wire)
           ├─ bar_wire
           ├─ bar_model ── bar_abi        (re-exports bar_abi)
           ├─ bar_tunables ── bar_model
           ├─ shape                       (punchcutter's; imports ot and shape_abi)
           ├─ text                        (punchcutter's)
           └─ #include "bar_check.slang"  (only under -DCAUSTICS_BAR_CHECK)
```

`bar_tunables` imports only `bar_model`, which re-exports `bar_abi`, which
re-exports `bar_wire`, so a tunables file reaches every constant it can name
(`EDGE_*`, `ITEM_*`, `APPLET_*`, the `ACT_*` verbs) through one import.
`shape` and `text` know nothing about the bar: `text` declares its four curve
buffers in descriptor set 1 and `shape` declares no bindings, and the bar binds
their buffers and passes the handles in (punchcutter/docs/arena.md, *A shaping
dispatch*).

## Conventions

- **A module declares no bindings and no entry points** unless it is the
  program. `bar_abi`, `bar_wire`, `bar_model` and `bar_tunables` are
  importable anywhere at no cost; `bar.slang` owns every `[[vk::binding]]` of
  its own and every `[numthreads]`.
- **Bindings are named, not numbered, on the host side.** The binding numbers
  are literals in `bar.slang` (an attribute cannot take a constant). The bar
  builds its bind groups by reading each binding's name out of slangc's
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
- **Two swapchain formats.** `bar.slang` compiles once with
  `-DDST_FORMAT_RGBA8` and once with `-DDST_FORMAT_BGRA8` (SPIR-V has no BGRA
  storage-image format, so that variant declares the image format-unknown).
  The bar picks the pair matching the surface it negotiated.

## The tunables

`bar_tunables.slang` is the only configuration the bar has, and its own
documentation: each constant carries a comment saying what it does. To change
anything, edit it in your checkout and save; the running bar moves. It is a
module like the others in this directory, and the compiler is its validator:
an edit that does not compile keeps the last good bar and prints slangc's
diagnostic, naming this file, to the journal. Your edits live in your
checkout, so an update is a merge like any other.

Two tunables reach the bar process, both as one message on the caustic's
first frame and again whenever a reload changes them: `EDGE` (`EDGE_TOP`,
the shipped default, or `EDGE_BOTTOM`) and `BAR_HEIGHT`. Everything else
(colours, sizes, the wings' contents and order, the dock menu's rows, the
clock's words, the springs, the shape watchdog) is read by the caustic alone.

## Hot reload

`caustics-reload` watches this directory, the tunables among it. The text
engine's modules are not watched: an engine change is a dependency bump and a
rebuild. On a save it compiles every entry point with slangc, caches the
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
slangc's diagnostics go to the journal. A reload that changes a binding, or
the size of the caustic's private buffer, is refused with "restart the bar".

Where `caustics-reload` looks: `caustics/` in its working directory, then
`$CAUSTICS_DIR`, then the `caustics/` of the checkout the binary was
built from. An installed bar built from a checkout therefore hot-reloads from
that checkout. With none of the three present it runs the caustic baked into
it and never reloads.
