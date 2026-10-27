# Contributing to caustics

Thank you for looking. The rules below are the ones the bar's design depends
on, and each says why. docs/architecture.md is the longer version.

## Where a change goes

Before writing code, decide which of the three parts it belongs in:

- **The caustic** (`caustics/`), if it is computation over data the GPU can
  hold: layout, drawing, what a click means, a label's wording, shaping,
  anything that decides. Most changes land here, and they hot-reload.
- **A sidecar**, if it talks to something outside the bar (D-Bus, a file, a
  device, another program) that is not one of the bar's own Wayland objects.
  A sidecar writes frames (docs/abi.md, *The wire*) and the bar copies them. It never
  gets a hook into the bar's process.
- **The bar process** (`bar/src/`), only if the code owns a Wayland object or
  a file descriptor the bar holds, sits on a configure/acknowledge round trip,
  or must decide before forwarding an event synchronously. Say which of the
  three in the pull request. The bar process translates and forwards. A
  change that makes it *decide* something, parse a sidecar's bytes, or link a
  D-Bus, font, image or compiler library is in the wrong place. The part of
  it any program hosting a caustic would share (the device, the pipelines and
  their reload, the sidecar supervisor) is the `caustics-host` crate in
  `host/`, under the same test, and it names none of the bar's buffers, entry
  points or sidecars: those are parameters the bar passes.

**The GPU-first rule.** The CPU keeps I/O and protocol round trips. Strings,
glyphs, icons that are paths, and shaping are the caustic's, even where the
usual answer is "that stays on the CPU". If you think something cannot be
done in the shader, say why in the pull request. It is often a question worth
asking out loud.

**Configuration is the caustic.** There is no configuration file and there
will not be one. A new knob is a `public static const` in
`caustics/bar_tunables.slang` with a comment saying what it does. If the bar
process must act on it, the caustic sends it as a message (as it does the edge
and the height), and the process never reads the tunables. A list of strings
the user changes at run time is **state** (the dock's favourites), and a
sidecar's own settings are that sidecar's environment variables, never the
bar's.

## Languages and layouts

- **Slang, not WGSL**, for every shader. Shaders are compiled to SPIR-V by
  Slang in process, through `punchcutter-slang`, and loaded through wgpu's
  passthrough.
- **Shared layouts are declared once, in Rust**, in `bar/abi/` for the bar's
  two contracts. The Slang side, `bar_abi.slang` and `bar_wire.slang`, is
  rendered from it wherever a compile needs it and is not in the tree; edit
  the Rust and build. A test compares every struct's offsets with Slang's
  reflection.
- **The text engine is a dependency.** Shaping, glyph coverage and the
  OpenType arena's layout are punchcutter's; a change to them is a change
  there and a dependency bump here, never a copy in this tree.
- Every field of a shared struct is a `uint`, a `float` or an array of them.
  No 3-vectors: std430 aligns them to 16 bytes.
- **Untrusted bytes are bounded.** Anything the caustic reads from a
  sidecar's arena is clamped to what was bound, and every list it states is
  capped and charged against a budget before its first iteration. Do not make a budget charge a loop's exit condition: one
  driver miscompiled exactly that.

## Formatting

Rust is formatted with rustfmt's defaults: `cargo fmt --all` before a commit,
and CI checks it. Slang and shell are formatted by hand, in the style of the
file they are in.

## Licensing

The bar is MIT. Code brought in from elsewhere comes under a licence
compatible with that, keeps its own notice where it lands (a header
comment naming the work, its author and its licence), and is listed in
`NOTICE`.

## Dependencies

`Cargo.lock` is committed and is the source of truth: a plain `cargo build`
or `cargo check` never resolves new versions, and the tests and the harness
build with what the lockfile names. The maintainer gates every dependency
change through a release-age cooldown (`cooldown.toml`: a version younger
than seven days is not taken). If your change adds or updates a crate, open
the pull request as it is; the maintainer re-resolves and reviews the
lockfile. Please do not run `cargo update` as part of an unrelated change.

## Tests

- **Unit tests live beside their file** as a sibling module: `foo.rs`
  declares `#[cfg(test)] mod tests;` and the tests are in `foo/tests.rs`.
  Integration tests with recorded bus bodies or captures are in a crate's
  `tests/`, with their data in `tests/data/`.
- **Before a pull request**: `cargo check --workspace --all-targets` with
  **zero warnings**, and `cargo test --workspace` green. The first build
  downloads the pinned Slang release (README.md, *Requirements*). The
  same two run in CI on every pull request, with the oracle on lavapipe
  (docs/testing.md, *Continuous integration*).
- **The oracle**, if you touched the caustic's build, compose or shape
  passes: `CAUSTICS_BAR_ORACLE=1 cargo test -p caustics-bar oracle -- --nocapture` on
  any Vulkan device (lavapipe will do). New invariants go in
  `caustics/bar_check.slang`; new scenarios in `bar/src/oracle/tests.rs`.
- **The nested gate** (`bar/nested-run.sh`, docs/testing.md) is the merge
  gate for anything that touches the bar's Wayland side, its pipes, the
  install set, or a sidecar's behaviour on a bus. It needs niri and either a
  toolbox container for a headless sway or a niri desktop. docs/testing.md
  has the list, how to read the result, and how to add a step. Paste the
  tally and the timing line into the pull request. If you cannot run it, say
  so and the maintainer will.
- **A test fixture must identify nobody.** Recorded D-Bus bodies, menu dumps
  and captures are invented or scrubbed: made-up SSIDs and names,
  locally-administered MAC addresses (`02:…`), documentation-range IP
  addresses (192.0.2.0/24, 198.51.100.0/24, 2001:db8::/32),
  `example.invalid` hosts. `tools/scrub-check.sh` scans the tracked tree for
  the shapes of private data (MAC and IP addresses outside those ranges,
  email addresses, private keys, home paths) and exits non-zero on a hit.
  Run it before committing a fixture. Pointing `$CAUSTICS_SCRUB_WORDS` at a file
  of your own words (your hostname, your network names), one per line and
  never committed, makes it look for those too. Some sidecars have a recorder
  for capturing a real bus when diagnosing a bug locally; what it records is
  for your machine, never for a commit.

## Logging

Every decision is one line, once per change, never per frame. In the caustic
a witness is one line of Slang (`witness2(WITNESS_INFO,
getStringHash("…{0}…"), …)`, docs/abi.md, *The witness ring*) and needs no Rust. In a
sidecar, write each line to stderr in a single `write_all` (the
`caustics_sidecar::Witness` helper does), never with `eprintln!`, whose several
writes can be split into several journal entries. stdout is the wire: a stray
`println!` in a sidecar is a protocol error the bar answers by restarting it.

## Pointers in comments

A comment that cites a document names its section: `docs/abi.md, *Hello*`,
`docs/architecture.md, *Rules*`. Never a section number, and never a document
that is not in this tree.

## Commits and pull requests

Small, focused commits whose messages say what changed and why. In the pull
request: what the change is, which part it belongs in and (for the bar
process) which of the three tests each new line passes, what you ran, and
anything you found that you did not fix.
