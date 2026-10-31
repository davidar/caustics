# Running caustics-bar as a user unit

caustics-bar is an ordinary layer-shell client. It replaces nothing and can run
beside another panel on another edge. In a session it runs as a **systemd
user unit**, `caustics-bar.service`, which restarts it when it crashes, keeps it
down when it refuses to run, and sends its log to the journal.

## Install

```sh
bar/install.sh
```

The script builds the whole workspace in release mode, checks that all nineteen
binaries came out of *that* build, reads the set's name out of the bar it just
built (`caustics-bar --abi-hash`), installs the ones that changed into
`~/.local/lib/caustics-bar/<hash>/`, moves the `~/.local/bin/caustics-bar` symlink on
to the set atomically (and the `caustics-canvas` and `caustics-canvasctl`
ones beside it; the canvas's unit is canvas/README.md's), and restarts what changed: when the running bar is
this set's and its own binary is unchanged, the unit stays up and only the
sidecars whose binaries changed are ended (the bar respawns each on the new
code, and the panel never blinks); otherwise the unit restarts and the script
prints the two lines worth reading back, `set: …` and the first
`health (startup): …`.

- `--prefix DIR` installs somewhere else and does not restart anything. The
  unit's `ExecStart` is `%h/.local/bin/caustics-bar`, so only the default prefix
  is the one systemd runs.
- `--from DIR` installs binaries that are already built, without cargo.
- `--dropin FILE` also installs a unit drop-in (below).

**A set is a directory.** The bar resolves its own path and looks for its
sidecars beside itself, so a bar started through the symlink runs its own
set's sidecars, and a sidecar of its that crashes respawns out of the same set
even after a newer one has been installed. The directory is named after the
**ABI hash**, not a commit: two commits with the same contract are
interchangeable sets, and the name says so. Older sets are kept deliberately,
since a running bar holds its own. Removing one is `rm -r` on a directory
nothing is running.

**Never install one binary on its own** (for example after
`cargo build -p caustics-bar`, which builds the bar crate and leaves the sidecars
from whatever build came before). A bar and a sidecar from different builds
may agree on every number on the wire and disagree on what one of them means.
The bar refuses such a sidecar on its first frame (`refused — it calls itself
… and was built against ABI …`, docs/abi.md, *Hello*), and the script is there so
that there is no way to make that pairing by hand either.

The first time, link the unit and enable it:

```sh
systemctl --user link "$PWD/bar/systemd/caustics-bar.service"
systemctl --user enable --now caustics-bar
```

`link` rather than `install`: the unit stays in the checkout, so editing it
and `systemctl --user daemon-reload` is the whole edit cycle.

### What the build wants of the machine

- Nothing for Slang. `bar/build.rs` and `caustics-reload` compile
  `caustics/bar.slang` with the Slang compiler carried inside
  `punchcutter-slang`, whose first build downloads the pinned Slang release
  (README.md, *Requirements*). At *run* time only `caustics-reload` compiles, in
  its own process, and needs nothing installed.
- The Wayland client library at run time (`libwayland-client`, loaded by
  name when the bar starts; nothing is linked against it, so no development
  package is needed to build).
- A Vulkan driver whose adapter supports SPIR-V passthrough. Where the
  adapter has `VK_KHR_shader_clock` the shaper holds a wall-clock deadline
  inside each run; the journal says `gpu: VK_KHR_shader_clock is on`, or
  warns that it is not and that the operation budget is the only bound.

## The unit

`caustics-bar.service` is `PartOf` and `WantedBy` `graphical-session.target`, so
the session must reach that target with the display in the user manager's
environment (README.md, *Build and install*): `niri-session` and uwsm do
both; a plain compositor configuration runs `systemctl --user
import-environment WAYLAND_DISPLAY NIRI_SOCKET XDG_CURRENT_DESKTOP` and starts
a session target from its startup. What the unit sets, and why:

- `Restart=always`, `RestartSec=1`, at most ten starts a minute. Exit 1, a
  Wayland protocol error or a fatal dispatch failure, is a restart: a dead bar
  must never linger on screen with no surface.
- `RestartPreventExitStatus=2 3`. **Exit 2** is the bar refusing to run at all
  (an adapter named NVIDIA without `CAUSTICS_BAR_ALLOW_DGPU=1`, an argument it
  does not understand, another caustics-bar already holding this edge). **Exit 3**
  is the GPU device gone (a driver error, a reset ring, a validation failure).
  Neither gets better by trying again, since the sidecars would hand the next
  bar the same bytes within a second, so neither restarts.
- `StandardOutput=journal`, `StandardError=journal`. The bar's witness lines
  are its diagnostics and go to stderr; a user manager's default may not be
  the journal.
- `MemoryMax=1G`. A backstop, not a budget. The bar itself sits far below it,
  but `caustics-reload` compiles the caustic in its own process inside this
  unit's cgroup, and a compile of every entry point peaks at about 400 MB
  (the sidecar gives it back when the compile ends). One compile runs at a
  time, so the peak is one compile plus the set. There is no `CPUQuota`: an
  interaction burst may use a core, and the bar's stall witness is what
  catches a busy loop.
- `RUST_LOG=info`.

### The iGPU-only drop-in, for laptops with a discrete GPU

A status bar must never be the reason a discrete GPU is awake
(docs/architecture.md, *Power and the discrete GPU*). The bar already prefers
a low-power adapter and refuses one named NVIDIA, but on a hybrid laptop
merely letting the Vulkan loader enumerate the discrete GPU's driver can wake
it. The drop-in `bar/systemd/dropins/igpu-only.conf` pins the loader to the
integrated GPU's driver (`VK_DRIVER_FILES`), so the other driver is never
loaded at all. Check the path in it against `/usr/share/vulkan/icd.d/` on your
machine, then install it:

```sh
bar/install.sh --dropin bar/systemd/dropins/igpu-only.conf
```

It goes to `~/.config/systemd/user/caustics-bar.service.d/`. A drop-in already
installed there is kept on later installs, and the script says so. Copying it
there by hand and running `systemctl --user daemon-reload` is the same thing.

## The sidecars

Everything the bar shows that it does not own itself comes from a **sidecar**
(docs/architecture.md, *Sidecars*): a separate process the bar starts, whose
stdout is a stream of binary frames the bar copies to the GPU without
parsing. The eleven, compiled into the bar as `SIDECARS` in
`bar/src/feeders/pipe.rs`: `caustics-sysmon`, `caustics-tray`, `caustics-power`,
`caustics-audio`, `caustics-network`, `caustics-bluetooth`, `caustics-notify`,
`caustics-displays`, `caustics-apps`, `caustics-glyphs`, `caustics-reload`.

Each is spawned with **no arguments**, stdout and stdin piped, stderr
inherited into this unit's journal. Each name resolves, in order: a command
with a `/` in it as written; the binary of that name **beside the bar's own**
(which is why a set is one directory, since a user manager's `PATH` usually
does not include `~/.local/bin`); the bare name on `PATH`. A sidecar that
exits is restarted with a doubling backoff from one to thirty seconds. One
that is not installed is a single `absent: <name>` line and a bar that is
otherwise exactly itself. Without `caustics-glyphs` the bar draws no text at all,
which is why the set is installed whole.

`$CAUSTICS_SIDECARS` **replaces** the compiled list with a whitespace-separated
list of commands, which is how a sidecar is tried without installing it:

```ini
# ~/.config/systemd/user/caustics-bar.service.d/sidecars.conf
[Service]
Environment="CAUSTICS_SIDECARS=caustics-sysmon caustics-glyphs /path/to/target/release/examples/rows_demo"
```

**A sidecar's settings are its own**; the bar passes none on. `caustics-sysmon`
reads `$CAUSTICS_SYSMON_CHARTS` (`sysmon/README.md` lists every chart word), and
`caustics-glyphs`'s text engine reads `$PUNCHCUTTER_FONT`. Set them on the bar's unit, which every
sidecar inherits, and restart it:

```ini
# ~/.config/systemd/user/caustics-bar.service.d/charts.conf
[Service]
Environment=CAUSTICS_SYSMON_CHARTS=cpu cpu-temp mem net disk gpu
```

### Swapping one sidecar without restarting the bar

The bar respawns a sidecar that exits, from beside its own binary, so a
rebuilt sidecar goes live without touching the bar: install it into the
running set's directory, then end the *old* process **by the pid the bar
printed** when it spawned it (`sidecar caustics-audio: started pid=N` in the
journal, or the hourly health line). Never by name: a name may match other
processes, including another bar's sidecars.

```sh
set_dir=$(dirname "$(readlink -f ~/.local/bin/caustics-bar)")
install -Dm755 target/release/caustics-audio "$set_dir/caustics-audio"
kill "$(journalctl --user -u caustics-bar --no-pager -o cat |
    grep -oE 'sidecar caustics-audio: started pid=[0-9]+' | tail -1 | grep -oE '[0-9]+$')"
```

Only a sidecar built against the running bar's ABI hash is accepted; any
other is refused on its first frame. The caustic needs even less: save a
`.slang` file and `caustics-reload` recompiles (`caustics/README.md`, *Hot
reload*). `systemctl --user restart caustics-bar` is for the bar's own binary.

## Watching it

```sh
journalctl --user -u caustics-bar -f
```

Every decision change is one line, and nothing is logged per frame. The ones
worth knowing:

| line | means |
|---|---|
| `set: <dir> (ABI <hash>)` | which install set this bar resolved to, at startup |
| `pipeline bar_apply: built in 6.81 s`, then `pipelines: all 7 built, …` | the caustic's pipelines, one line each as the compile workers build them, in the order the first frame needs them; seconds the first time after a driver or caustic change, milliseconds after that. The bar draws its first frame when the frame submission's four are in, its chips with `bar_build`, its text with `bar_shape` |
| `health (startup): … sidecars=…` | ten seconds in: every sidecar, `running pid=`, `absent` or `refused`; then `health (hourly)` once an hour with what the bar and each sidecar cost |
| `bound ext_foreign_toplevel_list_v1 v1`, `bound niri-ipc at …` | a feeder got its global or socket |
| `<name> absent: …`, `niri-ipc absent: …` | it did not, and the bar goes on without that feature |
| `sidecar <name>: started pid=N` / `exited …; restart in N s` | a sidecar's life |
| `sidecar <name>: hello from "<name>", ABI …` | its first frame said which contract it was built against |
| `sidecar <name>: refused — it calls itself … and was built against ABI …` | a binary from another build: killed, and not started again. Install the set |
| `sidecar <name>: bad magic …; restarting` | the child wrote text to stdout, and stdout is the wire |
| `sidecar <name>: skipped a frame — kind N: unknown kind` | a frame this bar has no use for; once per kind per child |
| `absent: <name>` | that sidecar is not installed; nothing retries |
| `caustics-network: absent: networkmanager` (and the like) | a sidecar's own stderr: its service is not there, so its chip is not either |
| `dock: 7 entries (3 running)`, `applets: 3`, `charts: 6 laid out` | the caustic rebuilt its tables |
| `hover: firefox at=…`, `hover: none` | what the pointer is over |
| `action: … -> niri-ipc`, `action: … -> sidecar` | a decision the caustic made, being carried out |
| `action dropped: …` | one nothing could carry out, and why |
| `popup: requested kind=menu …` → `popup: open …` → `popup: closed (…)` | a popup's whole life |
| `preview: window N 1280x800` | a captured still reached the GPU |
| `tunables: applied (edge=top height=32)` | the caustic asked for a surface and got it |
| `caustics-reload: generation N sent (7 modules, … KiB, … ms, cold)` | the caustic was recompiled (`warm` = read back from the cache) |
| `caustics-reload: failed; keeping the old bar` + Slang's own text (`--> path:line:col`) | a caustic that does not compile; the running one stays |
| `pipeline <name>: built in … (reload M)` | that generation's pipelines building beside the bar that runs, which keeps drawing |
| `sidecar caustics-reload: generation N, 7 modules, swapped (…)` | and the bar put that generation on the GPU, the moment its last pipeline was built |
| `text: N runs have a glyph the font lacks` | the caustic asked `caustics-glyphs` to cover them from a fallback face |
| `redraw: still animating after N s: …` (WARN) | something has not settled; the caustic's `stall:` lines follow, naming what is still moving |
| `items: table capped at …` (WARN) | the caustic could not fit everything it built; some of the bar is not drawn |
| `caffeine: inhibitor held` / `released` | the idle-inhibit chip was clicked |
| `favourites: none yet (… does not exist); Pin on a dock entry adds the first` / `favourites: N read from …` / `favourites: wrote N to …` | where the dock's pinned list came from, and a Pin or Unpin |
| `activation: token for row=…` / `activation: no token (…)` | a notification's click and the compositor's answer |
| `shape watchdog: …` | a runaway shape pass: nothing is shaped until the font arena is written again, and the health line says `shape=STOPPED (watchdog)` |
| `gpu: the device is gone (…)` | exit 3; the unit leaves it down |

### The layout lines

There is one line per element whose rectangle moved, naming the element and
where it went. They are how the test harness knows where to click. At INFO
they would be most of the journal, so they go out at DEBUG under the target
`caustics_bar::layout`. To watch them, raise that one target:

```sh
mkdir -p ~/.config/systemd/user/caustics-bar.service.d
cat > ~/.config/systemd/user/caustics-bar.service.d/zz-layout.conf <<'CONF'
[Service]
Environment=RUST_LOG=info,caustics_bar::layout=debug
CONF
systemctl --user daemon-reload && systemctl --user restart caustics-bar
```

`zz-` so it sorts after every other drop-in and its `Environment=` wins.
`caustics_bar::shape=debug` adds one line per model rebuild with what the three
model passes cost. Delete the file and reload to go back to a quiet journal.

## Configuration

There is no configuration file. The bar's configuration is the caustic's
tunables module, `caustics/bar_tunables.slang` in the checkout the bar was
built from, and a save hot-reloads it (`caustics/README.md`, *The
tunables*). The shipped bar sits on the top edge; `EDGE = EDGE_BOTTOM`
moves it.

**The dock's favourites** are state, not configuration:
`$XDG_STATE_HOME/caustics/bar/favourites` (`~/.local/state/caustics/bar/favourites`),
one app id per line, in dock order, written when you pick **Pin** or
**Unpin** on a dock entry's right-click menu. Until the first pin there is no
file and no favourites: the dock shows the running windows alone
(`DEFAULT_FAVOURITES` in `bar/src/facts.rs` is empty). Once the file exists it
is the whole truth, and deleting it goes back to none. The bar reads it once,
at startup: a hand edit takes effect at the next restart.

## The first start after a bind-group change

A hot reload whose bindings differ from the running bar's (a buffer added,
removed or resized in `bar_state.slang`) is refused, because a dispatch whose
descriptors and whose `[[vk::binding]]` literals disagree is a device loss on
one driver and silently wrong buffers on another. A bar that was running when
such a change was installed refuses the first generation `caustics-reload` sends,
with one line:

```text
sidecar caustics-reload: generation 1 refused: reload refused: bindings changed (…); restart the bar
```

`systemctl --user restart caustics-bar` once, and every reload after it is
ordinary. `bar/install.sh` restarts the unit itself.

## One bar per edge

A second caustics-bar on an edge one already holds is refused. An advisory
`flock` on `$XDG_RUNTIME_DIR/caustics-bar-<edge>.lock` is taken before the
Wayland connection, and losing it is

```
another caustics-bar holds the top edge; exiting
```

with exit 2, which the unit does not restart. The kernel drops the lock when
the process dies however it dies, so a crashed bar leaves nothing to clean up.

## Taking it out

```sh
systemctl --user disable --now caustics-bar
```
