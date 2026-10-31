# Testing

How the bar is tested, from a unit test to a whole session: what each tier
proves, what it needs, how to run it, how to read what it prints, and how to
add a step. The merge gate for anything that touches the bar's Wayland side
is the third tier, **the nested gate**.

## Three tiers

| tier | command | proves | needs | takes |
|---|---|---|---|---|
| unit tests | `cargo test --workspace` | every crate's logic: the wire's reader rules, each sidecar's frames against recorded bus bodies, the ABI layouts against Slang's reflection, the rendered Slang modules on disk against their Rust declaration | Rust (the first build downloads the pinned Slang release, README.md, *Requirements*); fontconfig for the text tests (they skip without it) | a minute or two |
| the oracle | `CAUSTICS_BAR_ORACLE=1 cargo test -p caustics-bar oracle -- --nocapture` | the caustic's own passes (`bar_build`, `bar_compose`, `bar_shape`) on a real GPU with a real font: the tables they build obey `caustics/bar_check.slang`'s invariants, every block is the size its tunable says, every built item can be named, no glyph is a notdef | a Vulkan adapter (Mesa's lavapipe will do) | about a minute, most of it compiling |
| the nested gate | `bar/nested-run.sh` | the bar as a process in a session: the layer surface and its configure, popups and their grabs, the dock against real windows, every sidecar on a real (private) session bus, clicks and typing, hot reload, the install set's refusals, the watchdog, restarts | the list under *What the gate needs* | eight to ten minutes |

The oracle is gated on `CAUSTICS_BAR_ORACLE=1` so that `cargo test --workspace`
never creates a Vulkan instance or compiles `bar_check` beside the rest of the suite
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

**The lock by eye.** `bar/lock-try.sh` is the gate's companion for what the
steps cannot do: lock and unlock with the developer's real password and real
fingerprint reader. It builds the workspace, starts the same nested niri as
a window on the desktop and the canvas inside it with `caustics-lock` in its
set, under a private runtime directory but on the real session and system
buses, and locks the window. The developer types into it while the terminal
shows the lock's witness lines and a verdict at each unlock; closing the
window ends it, and nothing in the session was ever locked. Its options and
its two side effects on the real session (the session's `LockedHint`, and a
real `loginctl lock-session` locking the window too) are in
canvas/README.md, *Trying it first*. `--headless --dry-run <secret>` runs the
same machinery on the gate's headless sway with no system bus, which is how
the script itself is checked.

### What one run does

1. **Independent of any other run.** Each run has its own run directory,
   runtime dir, session bus, host sway and nested niri, so two runs may
   overlap (two builders gating at once, say); the GPU and the toolbox
   container are all they share. A run does wait while any VM target slot
   is held: a lavapipe guest loads every core, and the steps' time bounds
   assume a machine that is not rendering a second desktop in software.
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
  `bar/harness/nested-niri.sh`, which starts the host and the nested niri.
  With `CAUSTICS_NESTED_HOST=desktop` no container is
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
- `dbus-run-session`, `gdbus` and
  `busctl`, `flock`, and a terminal emulator for the steps that need a window
  (the first of `cosmic-term`, `kitty`, `foot`, `alacritty`, `xterm`).

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

1. copying the install set, the unit and lavapipe into a guest;
2. the half of the notification step that hands the bus name over from
   another daemon (on a private bus nothing else holds it);
3. audio devices, which loads null sinks into the session's PipeWire and
   moves its default sink;
4. `bar/install.sh` end to end, even into a throwaway prefix;
5. a known-bad OpenType arena handed to a running bar on the live GPU.

### Known behaviour the steps account for

- **Every pipeline in a gate run is built cold.** Mesa's shader cache is
  under the run's scratch `$XDG_CACHE_HOME`, so each host's start-up and
  each swap pays the driver's full compile: on RADV about 12 s for the bar's
  seven pipelines on its two compile workers (16.2 s on one, with the chips
  no sooner: 8.3 s there, 8.4 s on two), 9 s for the canvas's three
  (the shaper nearly all of it), a few hundred ms for a tint change, and a
  few ms for a generation the driver has compiled before. A user pays it once
  after a driver or caustic change. None of it is on the hosts' loops
  (docs/architecture.md, *The caustic*): a host reads its pipes, answers
  input and keeps drawing the caustic that runs while the next one builds,
  so what a step waits for is the swap line, never a loop that came back.
  The bar's first frame waits for `bar_apply`, its layout: 1.9 s of RADV's
  time on its own (it was 4.6 s, and about seven in a gate run beside the
  other worker, before its witnesses and actions were written straight into
  their ring slots and its loops of constant length stopped being unrolled
  at every inlined copy; the comment on `Args` in `caustics/bar_state.slang`
  says why that was the cost). The canvas's first frame comes after its two
  small pipelines, and step 27 checks it is drawn before the shaper is
  built.
  Each pipeline build is one journal line, `pipeline <entry>: built in N s`,
  so a slow start says which pipeline it waited on; and
  `host/examples/pipeline_times.rs` times a program's pipelines on the
  headless device from its baked reflection and modules (its header has the
  command), cold or warm, with a first dispatch for a driver that compiles
  on first use, as llvmpipe does.
- **A layout witness can name a row's departure after its arrival in the
  same frame** when the row changes element slot (both lines carry the same
  frame, microseconds apart, in slot order). The steps therefore read
  "placed now" per frame, not per line.
- **niri's winit output is `Flipped180`**, and niri maps an absolute pointer
  through the output transform, so a virtual pointer's y comes out inverted
  on a nested niri. `caustics-pointer` sends the device point whose image is the
  pixel asked for.
- **`caustics-keyboard` uploads a keymap of its own**, a US layout written
  out from the same table that says which key and level types each
  character, so the lookup and the keymap cannot disagree and no XKB library
  is linked. `caustics-keyboard keymap` prints it. The focused client
  receives that keymap with the first key, as it would on any switch of
  keyboards.
- **One step's bound has little room on a busy machine.** The tunables step
  waits 40 s for a cold compile and a cold swap; under heavy parallel
  compilation it has missed once. It is recorded rather than raised.
  Overlapping gate runs load the same CPU and GPU, so this is the bound to
  watch when two run at once.

## The steps and their targets

`bar/bar-test.sh` is the steps. It reaches the bar only through a **transport
surface** that a target file provides (`bar/targets/$CAUSTICS_TARGET.sh`), and
nothing in the steps knows which target it is on:

| name | what it does on the nested target |
|---|---|
| `on_target <deadline> <cmd>` | runs a shell command with the run's environment (and refuses the commands listed above) |
| `to_target`, `target_kill <pid>` | `cp`; `kill`, but only a pid the run started or one in the bar's process group |
| `niri_msg …`, `shot <name>` | the nested niri's IPC; `niri msg action screenshot-screen` into the run directory, re-encoded to the 8-bit RGB PNG the pixel helpers read. Its two halves, `shot_capture` and `shot_develop`, let a step take several screenshots a fraction of a second apart and re-encode them after (the re-encoding is seconds of Python) |
| `pointer_to <x> <y>`, `button <down>` | `caustics-pointer` (a small client of `zwlr_virtual_pointer_manager_v1`, in `harness/pointer/`; not a sidecar and never installed) |
| `keys <text>`, `key <[mod+…]keysym> [press\|release\|tap]` | `caustics-keyboard` (its sibling in `harness/keyboard/`, a client of `zwp_virtual_keyboard_manager_v1`): `keys` types a string into whatever niri has given keyboard focus, a newline as Return; `key` is one named key or chord (`Return`, `Escape`, `ctrl+u`) |
| `journal`, `mark_journal`, `wait_for_journal*` | the bar's log from the cursor on; `bar_restart` moves a cursor to the new instance's first line, so the old bar's last lines are never read as the new one's |
| `bar_env <group> K=V…`, `bar_env_clear <group>` | an environment file per group, sourced at each start (on a systemd target, a drop-in) |
| `bar_restart`, `bar_stop`, `bar_active`, `bar_restarts`, `bar_pid` | the supervisor |
| `fixture_start/stop`, `client_spawn` | `setsid`, tracked by pid |
| `T_HOME T_CONFIG T_STATE T_CACHE T_BIN T_CAUSTICS …` | the run's scratch tree, `target/debug`, the copy of `caustics/` |

**The canvas's steps** (27 to 35) run last: the canvas host as a fixture
with `CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload"`, its log in
`$T_HOME/canvas.log`, on an empty workspace so a screenshot below the bar is
the background layer. They check its first frame against the tunables' dim
colour, a shown picture, the midpoint of a four-second fade, a hot reload
of `canvas_tunables.slang` that the bar does not follow, and a killed
wallpaper sidecar coming back with the picture unchanged. Then the fade:
a waited three-second dim whose screenshots darken across its time (by
`DIM_DARKEN`, from a shot of the bare wallpaper) and end at the wallpaper
darkened, and whose client exits 0 at its depth; a pointer motion and then a key about a
second into a six-second dim, each answered with exit 2 within half a
second; and the wallpaper sidecar killed mid-dim, after which its waiting
client exits 1, the overlay is gone and the sidecar restarts. Each of
those ends with no overlay left and the picture as it was; the last stops
the canvas. The canvas's own alpha(t) and its dim's, and its text (a title
and a literal shaped against a real font and drawn on the prompt's pane, ink
inside each run's box and bare panel around it), are tested on a device
beside the bar's oracle:
`CAUSTICS_CANVAS_ORACLE=1 cargo test -p caustics-canvas -- --test-threads=1`.

**The canvas's wake step** (35a) starts the canvas again, with its lock
sidecar in the dry run the lock steps use, and sweeps the pointer across an
empty workspace: the caustic says the background's water stirred and went
still within `WAKE_SETTLE_S` of the last move, the host says the background
stopped asking for frames, a screenshot mid-sweep differs from the resting
picture along the pointer's path, and one after it matches the resting
picture. Then it locks and sweeps the lock surface, with the same lines for
the lock; niri writes no screenshot while locked, so the lock's pixels are
the device test's (`the_wake_stirs_the_lock_…`: the water around the path
and nowhere far from it, on its own pane only, a press's wave, and the
resting picture pixel for pixel once settled). With
`CAUSTICS_CANVAS_WAKE_DUMP=<dir>` the device tests also draw a sweep at
1920x1080 for looking at, and time its frames.

**The polkit sink's step** (35b) runs `caustics-polkit` in its dry run, with
no system bus at all: the agent object on the run's private session bus,
`polkit/tests/fake-helper.sh` as its helper, and `bar/abi/examples/fake_host.rs`
on its pipes in the canvas host's place, answering from a script. The step
plays polkitd with `busctl --user call … BeginAuthentication`, and checks the
prompt frames the fake host logged, the method's return, that the fake
helper read exactly the run's secret, that a dismissed prompt fails the
call, that no log holds the secret, and that the sink leaves with its host.

**The canvas's prompt steps** (35c to 35g) start the canvas again, with
`caustics-glyphs` and `caustics-polkit` in its set and the sink in the same
dry run, so the canvas host is what answers. They play polkitd with
`busctl` and the user with the harness's keyboard, and check: the prompt's
pane up and the keyboard on it; a screenshot whose corner is the wallpaper
darkened by `PROMPT_DARKEN` (read from the tunables) and whose panel is not;
a wrong secret, an error line counted as an attempt and a fresh ask in the
same call (the sink runs its helper again, with the same cookie, up to
three attempts); the right secret, the call returned and the fake helper's
record exactly the wrong secret then the right one under one cookie;
Escape on a second call, which fails as dismissed;
no prompt pane left after each; and neither secret in the canvas's log (the
sink's lines included) or the bar's journal. The canvas is stopped at the
end, as step 35 left it.

**The lock sidecar's step** (35h) runs `caustics-lock` in its dry run with no
system bus, so there is no logind and the socket is its only trigger, under
the same fake host, which confirms each lock and unlock as the host will and
answers a wrong secret and then the right one. The step waits in
`caustics-canvasctl lock --wait` and checks its exit and its `locked`, the
frames in order (the lock, the title, the ask, PAM's error and a failed end,
a fresh ask, a successful end, the unlock), `status`'s `attempts=1` and
`locked=no`, the witness of no logind, that no log holds either secret, and
that the sidecar leaves with its host and takes its socket.

**The canvas's lock steps** (35i and 35j) start the canvas with
`caustics-glyphs` and `caustics-lock` in its set, the sidecar in the same
dry run, so the canvas host locks the nested niri through
`ext_session_lock_v1` and the harness's keyboard types into the lock's
prompt. niri refuses every action over its socket while locked,
screenshots included, so a screenshot that never arrives is the proof that
it is locked and one that does is the proof that it is not; the lock
screen's look is the caustic's witness here and the oracle's. They check:
`lock --wait` returning 0 with `locked`; one lock surface configured and the
lock, `locked` and the sidecar's word in that order; no screenshot; the
caustic drawing conversation 1 asking on the lock and no top-layer pane;
the keyboard on the lock surface; a wrong secret, an error line counted as
an attempt and conversation 2 asking, still locked; a `dim 1 over 30
--wait` while locked exiting 0 well inside the dim's time, the sidecar's
`dim=dimmed`, the host's line that the dim was reached at once, and no
overlay made; the right secret, the
unlock in order (success, the unlock asked, the lock surface gone,
`unlocked` told); a screenshot again, the output what it was before the
lock, so nothing dimmed stayed under it; `status`'s `locked=no attempts=1`; neither secret in any log. Then the
host is killed while locked: niri stays locked, and a restarted host's lock
(driven through the socket, as `LockedHint` would drive it with logind)
is given `locked` at once in place of the dead one, with no `finished`;
the right secret unlocks it, and the canvas is stopped at the end.

**The fingerprint step** (35k) starts the canvas the same way, but with the
lock sidecar's system bus a private `dbus-daemon` on which
`lock/examples/fake_fprintd.rs` plays fprintd, scripted to answer the first
verify with a finger that does not match and the second with one that
does; logind is not on that bus, so the socket is still the only trigger,
and the real fprintd is never reached. The reader's windows are short
(`CAUSTICS_LOCK_FINGERPRINT_WINDOW=8,60`). Nothing is typed in the first
lock. It checks: the sidecar's lines in order (a window open at the lock,
the reader claimed, not recognised, matched, unlocked); the host's in order (the ready line, the no-match's error line
beside the ask and not counted as an attempt, success, the unlock asked,
`unlocked` told); fprintd's calls in order (claim, verify, no-match, stop,
verify, match, stop, release). Then a second lock, whose verify waits for
a finger that never comes: its conversation carries the ready line; with
nobody there its window closes on idle, the reader is released while the
session is still locked, and the panel's line becomes the wake line;
straight after, the first key typed is told to the sidecar as input at the
lock, a window opens at once and verifies again, and the ready line
replaces the wake line;
the right secret unlocks, and the reader is released all the same, three
claims in two locks.
The bus and the fake are stopped by PID with the canvas.

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
   Keys go to the focused window, so a step that types focuses its window by
   niri id (`niri_msg action focus-window --id`) and waits until
   `focused-window` names it before the first `keys`. Both targets type the
   same characters: printable ASCII, newline and tab, on a US layout. Either
   refuses anything else before sending a key, and `keys` returns non-zero.
4. **Wait for a positive line, never a fixed sleep.** `wait_for_journal
   <pattern> <bound>` returns as soon as the line is there; `wait_until
   <what> <bound> <command>` does the same for anything else (a file on the
   target, a window's focus). A step that
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
scrub check. It caches the Slang release the first build downloads
(`~/.cache/shader-slang/`) and installs libwayland, fontconfig with a sans-serif font, lavapipe, `spirv-val` and
`dbus-daemon` from the distribution. The nested gate is not in it: it needs
niri, a headless sway and a GPU.

## The VM target

`bar/targets/vm.sh` drives the same steps in a virtual machine: a topaz-os
guest (the maintainer's own Fedora-based image for a niri session, a separate
project) booted by that project's test harness, reached over ssh, driven with QMP for the
pointer and the keyboard and screendumped through VNC. Its `keys` and `key`
send QMP key names (`send-key` per character, `input-send-event` for a press
or release held across calls), and the guest's own keymap turns them into
characters, so they type what the nested target types only while the
guest's layout is US. `bar/vm-run.sh` is its runner. It takes
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
image with a niri session at scale 2, sshd, a session bus, UPower and
lavapipe. Both are topaz-os's today. Making the VM route this
project's own, an owned launcher over a stock cloud image, is on the roadmap
(docs/roadmap.md).
