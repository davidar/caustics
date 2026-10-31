# caustics development notes

A status bar for Wayland compositors that renders everything in one Slang
compute shader and hosts its data sources as sidecar processes. README.md has
the overview, the build and the install. The text engine (shaping, glyph
coverage, fonts) is punchcutter, a dependency; its Slang modules travel in
`punchcutter-slang` and are on every compile's search path, never copied
into `caustics/`.

**References** (living, in `docs/`): architecture.md the parts and the
reasons for their shape; abi.md the contracts between the bar, its caustic
and its sidecars; testing.md the tests, the oracle, the nested gate
(`bar/nested-run.sh`, minutes on the host's own GPU) and the VM target
(`bar/vm-run.sh`); roadmap.md what is not built yet; wire.md the
append-only wire that replaces the hash (design); canvas.md the
wallpaper, fade, lock, polkit and greeter host (design); caustics/README.md
the shaders; bar/systemd/README.md running the bar as a user unit;
sysmon/README.md the sampling sidecar. `docs/check-links.sh` checks every
`docs/….md` pointer.

**Running it.** A bar or canvas you live with is installed only through
`bar/install.sh` (one set under `~/.local/lib/caustics-bar/<hash>/`, the bar
and every sidecar built together; never by hand); bar/systemd/README.md has
the user units and the drop-ins. Editing `caustics/*.slang` hot-reloads a
running bar built from that tree, `caustics/bar_tunables.slang` included.

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
- **Daily-driver safety**: the bar under test may be the host's own panel.
  Tests run in nested sessions (`bar/nested-run.sh`, a headless host and a
  nested niri) and never touch `~/.config/caustics`, `~/.local/state` or the
  running unit. Never kill processes by name (a nested terminal and the live
  one may share a binary); kill tracked PIDs only. The VM target
  (`bar/vm-run.sh`) is run by one session at a time, never by a builder.
- **Slang, not WGSL**, for every shader. Shaders live in `caustics/`,
  compiled to SPIR-V by Slang in process (`punchcutter-slang`, which carries
  the pinned release; the first build downloads it into
  `~/.cache/shader-slang/`) and loaded through wgpu's passthrough. Nothing
  to install; `spirv-val` is used when present.
- **The bar copies bytes and parses nothing**: a sidecar writes ABI frames
  over a pipe, the caustic lays them out. New data sources are sidecars; new
  behaviour is the caustic's. There is no config file: the tunables module is
  the configuration.

## Formatting

`cargo fmt --all` before every commit; CI runs `cargo fmt --all -- --check`.

## Dependency hygiene

- **Never run bare `cargo update` or `cargo add`.** Anything that can change
  `Cargo.lock` goes through `cargo cooldown` (policy in `cooldown.toml`:
  registry versions younger than 7 days are rejected; versions already in the
  lockfile are exempt). Its own subcommands are `check`, `build`, `test`,
  `run` and `update`; any other cargo command is forwarded through it, so
  `cargo cooldown add <crate>` works but is not in its `--help`. Add plain,
  hand-edit feature flags, then `cargo cooldown check`, which is the step
  that proves the lockfile against the policy.
- `cargo cooldown` copies the workspace into `$TMPDIR`; where `/tmp` is a
  small tmpfs, run it as `TMPDIR=$HOME/.cache/cooldown-tmp cargo cooldown
  check`, and keep scratch builds under `~/.cache` for the same reason.
- `Cargo.lock` is committed and is the source of truth; the first build after
  a dependency change goes `cargo fetch`, review, `cargo build --offline`.

## Git

- Stage by explicit path (`git status --short`, then `git add <file> …`);
  `git add -A` / `--all` / `.` are denied on purpose, so that nothing reaches
  a commit by a wildcard.
- A builder works in its own worktree under `~/.cache` from a brief that
  stands on its own; remove worktrees promptly after merging.
