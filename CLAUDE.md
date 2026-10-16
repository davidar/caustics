# caustics development notes

A status bar for Wayland compositors that renders everything in one Slang
compute shader and hosts its data sources as sidecar processes. README.md has
the overview, the build and the install. The text engine (shaping, glyph
coverage, fonts) is punchcutter, a dependency; its Slang modules are imported
through `punchcutter_core::slang_dir()`, never copied into `caustics/`.

**References** (living, in `docs/`): architecture.md the parts and the
reasons for their shape; abi.md the contracts between the bar, its caustic
and its sidecars; testing.md the tests, the oracle, the nested gate
(`bar/nested-run.sh`, minutes on the host's own GPU) and the VM target
(`bar/vm-run.sh`); roadmap.md what is not built yet; caustics/README.md the
shaders; bar/systemd/README.md running the bar as a user unit;
sysmon/README.md the sampling sidecar. `docs/check-links.sh` checks every
`docs/….md` pointer.

**The bar on this machine**: installed only through `bar/install.sh` (one set
under `~/.local/lib/caustics-bar/<hash>/`, the bar and every sidecar built
together; never by hand). The unit `~/.config/systemd/user/caustics-bar.service`
is a symlink into this checkout; the iGPU-only drop-in is installed beside it.
`caustics-notify-watch.service` (notify/tools) files a report when a
notification's click goes nowhere. The bar is the session's panel. Editing
`caustics/*.slang` on main hot-reloads the live bar, which reads the caustics
of the tree it was built from, `caustics/bar_tunables.slang` included.

## Ground rules

- **Public.** The repository is https://github.com/davidar/caustics; adding
  collaborators is the owner's call. Everything written here is written for a reader who has
  never met us: no personal names, machine names, set hashes or dates as
  narrative in source comments or the documents, and no labels from the
  private chronology (package letters, step codes). Before every push:
  `tools/scrub-check.sh ':!LICENSE'` (private words from
  `~/.config/caustics/scrub-words`, never committed; coincidental substring hits
  are read and judged, never suppressed) and `docs/check-links.sh` both
  clean.
- **Daily-driver safety**: the live bar is the session's panel. Tests run in
  nested sessions (`bar/nested-run.sh`, a headless host and a nested niri)
  and never touch `~/.config/caustics`, `~/.local/state` or the running unit.
  Never kill processes by name (the nested kitty and the live terminal share
  a binary); kill tracked PIDs only. Builders never run `bar/vm-run.sh` or
  qemu; they ask the main session, which runs one guest and relays results.
- **Slang, not WGSL**, for every shader. Shaders live in `caustics/`,
  compiled by `bar/build.rs` via slangc to SPIR-V and loaded through wgpu's
  passthrough. Building needs slangc on PATH (`$SLANGC` overrides);
  `spirv-val` is used when present.
- **The bar copies bytes and parses nothing**: a sidecar writes ABI frames
  over a pipe, the caustic lays them out. New data sources are sidecars; new
  behaviour is the caustic's. There is no config file: the tunables module is
  the configuration.

## Formatting

`cargo fmt --all` before every commit; CI runs `cargo fmt --all -- --check`.

## Dependency hygiene

- **Never run bare `cargo update` or `cargo add`.** Anything that can change
  `Cargo.lock` goes through `cargo cooldown update` / `cargo cooldown add
  <crate>` (policy in `cooldown.toml`: registry versions younger than 7 days
  are rejected; versions already in the lockfile are exempt). `cargo cooldown
  add` cannot carry feature flags: add plain, hand-edit the features, then
  `cargo cooldown check`.
- `cargo cooldown` copies the workspace into `$TMPDIR`, and `/tmp` here is a
  quota'd tmpfs backed by RAM: run it as
  `TMPDIR=$HOME/.cache/cooldown-tmp cargo cooldown check`. Scratch builds of
  any size go under `~/.cache`, never `/tmp`.
- `Cargo.lock` is committed and is the source of truth; the first build after
  a dependency change goes `cargo fetch`, review, `cargo build --offline`.

## Host constraints

- The host is an immutable image without the unversioned `.so` symlinks the
  linker wants. `cargo check` is fine; `cargo build` and `cargo test` need
  `LIBRARY_PATH=/var/home/linuxbrew/.linuxbrew/lib` (the gate and the
  installer set it themselves when that directory exists).
- Two GPUs: the drop-in `bar/systemd/dropins/igpu-only.conf` pins the
  integrated one's driver for the unit, so the discrete GPU stays asleep.

## Git

- Stage by explicit path (`git status --short`, then `git add <file> …`);
  `git add -A` / `--all` / `.` are denied on purpose, so that nothing reaches
  a commit by a wildcard.
- A builder works in its own worktree under `~/.cache` from a brief that
  stands on its own; remove worktrees promptly after merging.
