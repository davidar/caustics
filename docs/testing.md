# Testing

How the bar is tested, from a unit test to a whole session: what each tier
proves, what it needs, how to run it, how to read what it prints, and how to
add a step. The merge gate for anything that touches the bar's Wayland side
is the third tier, **the nested gate**.

## Three tiers

| tier | command | proves | needs | takes |
|---|---|---|---|---|
| unit tests | `cargo test --workspace` | every crate's logic: the wire's reader rules, each sidecar's frames against recorded bus bodies, the ABI layouts against slangc's reflection, the generated Slang files against their Rust declaration | Rust, slangc; fontconfig for the text tests (they skip without it) | a minute or two |
| the oracle | `CAUSTICS_BAR_ORACLE=1 cargo test -p caustics-bar oracle -- --nocapture` | the caustic's own passes (`bar_build`, `bar_compose`, `bar_shape`) on a real GPU with a real font: the tables they build obey `caustics/bar_check.slang`'s invariants, every block is the size its tunable says, every built item can be named, no glyph is a notdef | a Vulkan adapter (Mesa's lavapipe will do) | about a minute, most of it slangc |
| the nested gate | `bar/nested-run.sh` | the bar as a process in a session: the layer surface and its configure, popups and their grabs, the dock against real windows, every sidecar on a real (private) session bus, clicks, hot reload, the install set's refusals, the watchdog, restarts | the list under *What the gate needs* | eight to ten minutes |

The oracle is gated on `CAUSTICS_BAR_ORACLE=1` so that `cargo test --workspace`
never creates a Vulkan instance or runs slangc beside the rest of the suite
(whose sidecar tests time child processes). A machine with no usable adapter
runs the suite green and simply never runs the oracle. The oracle lives
inside the `caustics-bar` crate (`bar/src/oracle.rs`, `bar/src/oracle/tests.rs`)
because it needs the bar's own `Gpu` and `Facts`, and the bar is one binary
with no library to expose them through.

The split is deliberate. Anything whose evidence is *the caustic's state* (a
table, a size, a witness) belongs in the oracle, where it runs in
milliseconds. The gate keeps what only a compositor can show.

A fourth route, the **VM target** (below), runs the gate's steps in a virtual
machine instead of a nested compositor. It covers a few things the nested
gate skips, at the cost of a guest image and a harness that are not yet this
project's own.

## The nested gate

`bar/nested-run.sh` runs the steps in `bar/bar-test.sh` against a **nested
niri**: a niri compositor running as a window inside another compositor, with
the bar as an ordinary layer-shell client of the nested one. The bar runs on
the machine's real GPU, against a private session bus, under a private
runtime directory, so nothing a step does reaches the desktop it runs on.

```sh
bar/nested-run.sh                            # headless: nothing appears on screen
CAUSTICS_NESTED_HOST=desktop bar/nested-run.sh   # the nested niri as a window, to watch by eye
```

By default the nested niri's host is a **headless sway** (`WLR_BACKENDS=headless`,
one 1280×800 output), so a run opens no window. A nested niri on a hidden
workspace would not do: niri sends frame callbacks to an invisible window only
from a slow fallback timer, so the bar under test would draw at about one
frame a second. And niri's own headless backend is not selectable from its
command line. A headless wlroots compositor is the smallest host that keeps
the timings real.

### What one run does

1. **One at a time.** It takes `~/.cache/caustics-nested.lock` (waiting up to two
   hours for another run to finish), and waits while any VM target slot is
   held.
2. **Builds** the workspace in debug, the examples the steps use
   (`rows_demo`, the fake UPower) and the oracle's test binary. The "install
   set" is `target/debug/`: the bar finds its sidecars beside its own
   executable, so nothing is installed and `bar/install.sh` is never run.
3. **Re-runs itself under `dbus-run-session`**, so the session bus is the
   run's alone: `caustics-notify` takes `org.freedesktop.Notifications` on it and
   no test notification reaches the desktop. The *system* bus stays the
   machine's (NetworkManager, BlueZ, UPower), except where a step points
   `DBUS_SYSTEM_BUS_ADDRESS` at a fake UPower.
4. **Starts the host compositor** (the headless sway in a toolbox container,
   or the desktop), then the nested niri from `bar/harness/niri.kdl` on it,
   under a private `$XDG_RUNTIME_DIR`. Both sockets are read from the
   compositors' own logs, never assumed.
5. **Runs `bar/bar-test.sh`** with `CAUSTICS_TARGET=nested`, capped at ten
   minutes from the compositor's start. The bar runs under a small
   **supervisor**, a bash loop standing in for the systemd unit
   (`Restart=always`, `RestartSec=1`, `RestartPreventExitStatus=2 3`, a start
   limit, a restart count), with `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and
   `XDG_CACHE_HOME` pointed into the run directory and `CAUSTICS_DIR` at
   a **copy** of `caustics/` (the steps that edit the tunables do it in
   place and put back a snapshot taken at the start, and that must never be
   a tracked file). `HOME` stays the real one so `caustics-apps`
   finds the machine's desktop entries.
6. **Tears down on every exit path**, by tracked pid and in order: the
   clients the steps spawned, the fixtures, the supervisor, the bar's process
   group, the nested niri, the host compositor. Never by process name: a test
   terminal or bar shares its binary with the one the developer is using.

`on_target`, the one way a step runs a shell command on the nested target,
refuses any command that names `systemctl`, `journalctl`, `loginctl`,
`systemd-run`, `pkill`, `killall`, `sudo`, `~/` or a path under the user's
real config, state or cache. A step that tried would be a step that
could touch the developer's own bar.

### What the gate needs

- A Linux desktop session with a **Vulkan** GPU that wgpu can drive. The bar
  refuses an adapter named NVIDIA unless `CAUSTICS_BAR_ALLOW_DGPU=1`
  (docs/architecture.md, *Power and the discrete GPU*).
- **niri** on `PATH` (the nested compositor). The steps were written against
  niri 26.04. The hover-preview steps need a niri that offers
  `ext-image-copy-capture` over a foreign-toplevel source; on one that does
  not, those checks report the feature absent.
- For the headless host: **`toolbox`** and a Fedora toolbox image, in which
  the script installs `sway` on first use and says so. The container shares
  `$XDG_RUNTIME_DIR`, `$HOME` and `/dev/dri` with the host, so sway's socket
  appears where the host-side niri can reach it; only sway runs inside it.
  The image and the container's name are variables at the top of
  `bar/nested-run.sh`. With `CAUSTICS_NESTED_HOST=desktop` no container is
  needed, but the desktop must itself be niri (the script uses `niri msg` to
  float the window and hand focus back).
- The render node the headless sway draws on, and the Vulkan driver the bar
  is pinned to during a run, are environment-overridable
  (`CAUSTICS_NESTED_RENDER_NODE` for `bar/nested-run.sh`, `CAUSTICS_NESTED_VK_ICD`
  for `bar/targets/nested.sh`; the sway container's name and image are
  `CAUSTICS_NESTED_TOOLBOX` and `CAUSTICS_NESTED_TOOLBOX_IMAGE`; each script's header
  says how). By default the first render node whose driver is not NVIDIA's is
  used and the bar takes whatever adapter Vulkan offers it. On a laptop with
  a discrete GPU, point both at the integrated one.
- `slangc` on `PATH` (or `$SLANGC`), `dbus-run-session`, `gdbus` and
  `busctl`, `flock`, and a terminal emulator for the steps that need a window
  (the first of `cosmic-term`, `kitty`, `foot`, `alacritty`, `xterm`).
- On an immutable host whose `libwayland`/`libxkbcommon` lack the development
  symlinks, the script sets `LIBRARY_PATH` to a Homebrew copy if that
  directory exists; a normal distribution needs nothing.

### Reading the output

The script prints the log's path first. The whole run goes there, and the
screenshots and the bar's log go into the same run directory. Every step
opens with a header and every check is one line:

```
=== +0:07:31.902  25. one bar per edge
  PASS  +0:07:32.611  a second bar on the same edge refused to start
  PASS  +0:07:32.611  and it exited 2, which RestartPreventExitStatus=2 makes final
```

`FAIL` lines say what was expected and what was read instead; `SKIP` lines
say why the step does not run on this target; `....` lines are notes (what
was read, where a screenshot went).

The `+H:MM:SS.mmm` stamp is time since the run started. At the end come a
timing table (the total, the time spent in bounded waits, how many waits ran
their whole bound, and the ten slowest steps) and the tally:

```
=== timing: +0:08:20.241 total, 49 steps, 175.6 s in bounded waits, 0 wait(s) ran their bound
nested run: all checks passed (279 passed, 0 failed, 6 skipped)
```

A clean run is **0 failed**. A wait that ran its whole bound says so where it
happened. A step that passes only at its bound is a step that will fail on a
busier machine, and is worth a look even when it is green.

The bar's own journal for the run is the run directory's `bar.log` (its
stderr and its sidecars', which inherit it), with `bar.ts.log` beside it
carrying each line's arrival time. Every decision the bar makes is one line
there (docs/abi.md, *The witness ring* for the caustic's; `bar/systemd/README.md` lists the
common ones), and almost every check in the steps is a read of it.

**Skipped, on purpose.** A few steps exercise a guest image rather than the
bar, or would touch the machine running the gate. On the nested target each
is one `SKIP` line, counted as skipped and never as passed:

1. copying the install set, the unit, slangc and lavapipe into a guest (the
   gate asserts slangc runs instead);
2. the half of the notification step that hands the bus name over from
   another daemon (on a private bus nothing else holds it);
3. audio devices, which loads null sinks into the session's PipeWire and
   moves its default sink;
4. `bar/install.sh` end to end, even into a throwaway prefix;
5. a known-bad OpenType arena handed to a running bar on the live GPU.

### Known behaviour the steps account for

- **A hot reload whose pipelines are new to the driver blocks the bar's
  loop** for several seconds on RADV (7–9 s for the whole caustic, a few
  hundred ms for a tint change, a few ms for a generation the driver has
  compiled before). The swap runs on the bar's event loop, so for those
  seconds the bar draws nothing and answers no input. Every swap in a gate
  run is cold (Mesa's shader cache is under the run's scratch
  `$XDG_CACHE_HOME`), so the gate pays it at every swap; a user pays it once
  after a driver or caustic change. Moving the swap off the loop is an open
  item (docs/roadmap.md).
- **A layout witness can name a row's departure after its arrival in the
  same frame** when the row changes element slot (both lines carry the same
  frame, microseconds apart, in slot order). The steps therefore read
  "placed now" per frame, not per line.
- **niri's winit output is `Flipped180`**, and niri maps an absolute pointer
  through the output transform, so a virtual pointer's y comes out inverted
  on a nested niri. `caustics-pointer` sends the device point whose image is the
  pixel asked for.
- **One step's bound has little room on a busy machine.** The tunables step
  waits 40 s for a cold compile and a cold swap; under heavy parallel
  compilation it has missed once. It is recorded rather than raised.

## The steps and their targets

`bar/bar-test.sh` is the steps. It reaches the bar only through a **transport
surface** that a target file provides (`bar/targets/$CAUSTICS_TARGET.sh`), and
nothing in the steps knows which target it is on:

| name | what it does on the nested target |
|---|---|
| `on_target <deadline> <cmd>` | runs a shell command with the run's environment (and refuses the commands listed above) |
| `to_target`, `target_kill <pid>` | `cp`; `kill`, but only a pid the run started or one in the bar's process group |
| `niri_msg …`, `shot <name>` | the nested niri's IPC; `niri msg action screenshot-screen` into the run directory, re-encoded to the 8-bit RGB PNG the pixel helpers read |
| `pointer_to <x> <y>`, `button <down>` | `caustics-pointer` (a small client of `zwlr_virtual_pointer_manager_v1`, in `harness/pointer/`; not a sidecar and never installed) |
| `journal`, `mark_journal`, `wait_for_journal*` | the bar's log and a line count as the cursor |
| `bar_env <group> K=V…`, `bar_env_clear <group>` | an environment file per group, sourced at each start (on a systemd target, a drop-in) |
| `bar_restart`, `bar_stop`, `bar_active`, `bar_restarts`, `bar_pid` | the supervisor |
| `fixture_start/stop`, `client_spawn` | `setsid`, tracked by pid |
| `T_HOME T_CONFIG T_STATE T_CACHE T_BIN T_CAUSTICS …` | the run's scratch tree, `target/debug`, the copy of `caustics/` |

### Adding a step

1. **Decide the tier first.** If what you want to prove is a property of the
   caustic's tables, sizes or witnesses, write an oracle scenario in
   `bar/src/oracle/tests.rs` (or an invariant in `caustics/bar_check.slang`)
   instead: it runs in milliseconds and on any Vulkan device. A step belongs
   in `bar-test.sh` when its evidence needs a compositor: a surface, a
   popup's grab, a click's round trip, a sidecar on a bus, a restart.
2. **Open it with `say "<n>. <what it proves>"`**, numbered into the sequence
   where its preconditions hold. Steps share one bar and one session, so a
   step inherits what the ones before it left: put the state back at the end.
3. **Reach the bar only through the transport surface** above. Never `ssh`,
   `systemctl` or a path under `$HOME` directly; use `T_*` and `on_target`.
4. **Wait for a positive line, never a fixed sleep.** `wait_for_journal
   <pattern> <bound>` returns as soon as the line is there. A step that
   proves something did *not* happen waits for the line that says the
   decision was made (every such decision has a witness; add one in the
   caustic or the sidecar if it is missing) and then asserts nothing followed
   it. A check whose expected outcome is a timeout costs its whole bound on
   every run.
5. **One `pass`/`fail` per claim**, each saying what was read. Use `note` for
   context and `shot <name>.png` where a picture is evidence; the pixel
   helpers (`png_mean`, `png_pixel_diff`) read those files.
6. **If the step cannot run on the nested target** (it would touch the
   developer's audio, install into their home, or exercise an image rather
   than the bar), guard it with `if vm_only "<why>"; then …; fi` so it is
   counted as skipped, with its reason, rather than silently passing.
7. **Do not raise a bound to make a step pass on a real GPU.** The point of
   the nested target is that the timings are real; a step that fails there is
   a finding about the bar.

Check the script with `bash -n bar/bar-test.sh bar/targets/*.sh
bar/nested-run.sh`, then run the gate.

## Continuous integration

`.github/workflows/ci.yml` runs the first two tiers on a hosted runner for
every push and pull request: the build with warnings as errors, the whole
test suite, the oracle on Mesa's software Vulkan, the link check and the
scrub check. It downloads slangc from shader-slang's releases and installs
libwayland, fontconfig with a sans-serif font, lavapipe, `spirv-val` and
`dbus-daemon` from the distribution. The nested gate is not in it: it needs
niri, a headless sway and a GPU.

## The VM target

`bar/targets/vm.sh` drives the same steps in a virtual machine: a topaz-os
guest booted by that project's test harness, reached over ssh, driven with QMP for the
pointer and screendumped through VNC. `bar/vm-run.sh` is its runner. It takes
one of `$CAUSTICS_VM_SLOTS` lock files (default 2, so two guests can run side by
side), boots that slot's guest unless it is already up, runs `bar/bar-test.sh`
with `CAUSTICS_TARGET=vm` and the whole output to a log whose path it prints
first, and stops only a guest it started.

What the VM target covers that the nested gate skips: a fresh boot of the
whole set on a stock image through the unit and the install script, the
notification name handover from a daemon that already holds it, a guest
without a battery, and the known-bad arena. It runs on lavapipe, so its timings are not the bar's real
ones, and a run takes twenty minutes or more. It is a smoke run before an
install, not the per-change gate.

What it needs, and does not yet ship: the harness it sources (`$TOPAZ_TESTS`,
a directory holding that harness's `lib.sh` and `run.sh`, which boot the
guest and provide ssh, QMP and VNC access keyed by a run id) and a guest
image with a niri session at scale 2, sshd, a session bus, UPower, lavapipe
and slangc. Both are topaz-os's today. Making the VM route this
project's own, an owned launcher over a stock cloud image, is on the roadmap
(docs/roadmap.md).
