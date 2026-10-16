# Architecture

caustics-bar is a Wayland status bar whose drawing, layout, hit-testing, text
shaping and glyph rendering run in Slang compute shaders on the GPU. This
document is its architecture: the parts, how a frame runs, and the reason
behind each decision. The byte-level contract between the parts is
docs/abi.md; the tests are docs/testing.md; installing and running it is
`README.md` and `bar/systemd/README.md`.

## One paragraph

A small CPU process, **the bar process**, owns the Wayland objects (one
layer-shell surface, its popups, the pointer), the GPU device, the
compositor's IPC socket and a set of pipes, and decides nothing. Everything
that talks to the rest of the system (D-Bus, `/proc`, fonts, icon themes,
desktop entries, the shader compiler) is a **sidecar**: a separate process
that writes length-prefixed binary frames to its stdout, which the bar process
copies into GPU buffers without parsing. Everything you can see, and every
decision about it (layout, the dock's order, which menu is open, what a click
means, how a string is shaped, how a glyph is filled), is **the caustic**: one
Slang program, `caustics/bar.slang` and the modules it imports, compiled to
SPIR-V and hot-reloaded while the bar runs. A broken caustic keeps the last
good one. A crashed sidecar is restarted and its feature is absent meanwhile.
A protocol error ends the process and the unit restarts it. A bad change is
wrong pixels, never a dead bar.

## Why it is shaped this way

The bar was written to replace a desktop panel whose applets were separate
Wayland clients, each with its own connection to the panel, which proxied
their surfaces and popups to the real compositor one message at a time. That
architecture can fail in a way no one party can fix. The panel acknowledged a
popup's configure on dispatch; an applet's grab request was forwarded only
when the panel next read that applet's socket; and when an applet's request
batch split across two reads, the grab arrived after the configure had been
acknowledged, which a strict compositor treats as a protocol error. The
compositor killed the panel's connection, and the panel did not exit on the
fatal error and sat with no surfaces until something killed it. A proxy layer
meant to isolate applets from each other had become the single shared failure
point.

So the bar is **one process with no nested clients**. Its own popups are
created by itself with the grab issued before the first commit, every time.
There is nothing to proxy and no second party to race. Third-party content
reaches the bar as data over a pipe, never as a Wayland client of the bar.

A bar's content is also unusually **shader-shaped**: rectangles, icons, line
charts, window thumbnails and text on a translucent ground. A chart is a
buffer of samples read per pixel; a preview is a texture sampled in place;
hit-testing is a texel fetch from an id buffer the same pass wrote; blending
against the compositor's blur is free on a surface with real alpha. And a
shader can be replaced while the program runs: save a `.slang` file and the
running bar swaps its pipelines with its state intact. That loop, live tuning
of a running bar, is also why the configuration is Slang (*Configuration is
the caustic*, below).

The design comes from a GPU-resident compositor in which everything that runs
per frame is a compute pass (a "caustic") and the CPU is a small kernel. The
bar applies the same split to a layer-shell client. Nothing in the bar depends
on that compositor.

## The model

### The bar process

The code calls this process **the embassy**: the bar's one representative
in the compositor's world, which carries messages and makes no policy. The
bar process is governed like a kernel. **A line of code belongs in it only
if it owns a Wayland object or a file descriptor the bar holds, or sits on a
configure/acknowledge round trip, or must decide before forwarding an event
synchronously.** Everything else is the caustic's (computation over data) or a
sidecar's (I/O that is not the bar's own). The process translates and
forwards; it never decides.

What passes those tests, and so stays:

- **Surfaces.** One `zwlr_layer_shell_v1` surface on the Top layer with an
  exclusive zone and no keyboard interactivity, on whichever output the
  compositor gives it; and at most one `xdg_popup` parented to it at a time
  (a menu, a hover preview or a toast). The caustic decides what the popup is
  and how big. The process creates it, issues the grab before the first
  commit, and reports the compositor's answer back as a command.
- **The GPU.** A wgpu device on Vulkan. SPIR-V passthrough is required,
  since the shaders are Slang compiled to SPIR-V, not WGSL. The device is the
  bar's file descriptor, so its loss is the bar's exit (status 3, which the
  unit does not restart: the sidecars would hand the same bytes to the next
  bar within a second).
- **Input.** Pointer events become commands in a ring the caustic reads next
  frame; a scroll is a command carrying the compositor's own numbers. What a
  click hits and what it means are the caustic's.
- **Wayland feeders.** `ext_foreign_toplevel_list_v1` for the window list;
  `ext_image_copy_capture_v1` over a foreign-toplevel source for one hover
  preview at a time; `zwp_idle_inhibit_manager_v1` for the caffeine toggle
  (an inhibitor on the bar's own surface, which is why that one applet is in
  process); `xdg_activation_v1` for activation tokens on a notification's
  click. Each optional global, if absent, is one `absent` line and a feature
  the bar does without.
- **The compositor's IPC.** `ext-foreign-toplevel-list` carries no focus or
  urgency, and Wayland gives a client no way to focus, close or spawn. The
  bar reads both facts and sends its four requests (focus, close, toggle the
  overview, spawn an argv) through one seam, `bar/src/feeders/compositor.rs`,
  whose one implementation today is niri's socket (`$NIRI_SOCKET`). A dock
  launch is the compositor's `Spawn`: the bar resolves the app id to an argv
  from the desktop entry and starts no child process of its own besides its
  sidecars.
- **The pipes.** It spawns the sidecars, frames their stdout, routes each
  body to its GPU buffer, and writes actions and requests to their stdin.
- **Two files.** The dock's favourites (state, written by the Pin and Unpin
  rows) and an advisory lock per edge, so two bars never take the same one.

The bar process links no D-Bus library, no async runtime, no font stack, no
image decoder and no shader compiler. It is about twelve thousand lines of
Rust before its tests, most of it the surface, the GPU plumbing and the pipe
router, which a drawing process cannot shed.

### Sidecars

A feeder lives in the bar's process **if and only if** it owns a Wayland
object on the bar's connection, or writes the bar's shm pool or GPU memory
directly. Everything else is a **child process over a pipe**, and the wire is
an ABI of its own (docs/abi.md, *The wire*): the child writes length-prefixed,
self-describing frames (samples for the charts, rows for an applet, answers
to the bar's questions) and reads actions and requests the other way. The bar
copies bytes and parses nothing. stderr is inherited, so a sidecar's witness
lines land in the bar's journal. stdin closing means the bar is gone, and
every sidecar exits on it.

The sidecars the bar starts, compiled in as `SIDECARS` in
`bar/src/feeders/pipe.rs`:

| sidecar | talks to | gives the bar | without it |
|---|---|---|---|
| `caustics-sysmon` | `/proc`, hwmon, drm sysfs, NVML | the chart samples | no charts |
| `caustics-tray` | StatusNotifierWatcher/Item, `com.canonical.dbusmenu` (session bus) | the tray's chips and menus | no tray |
| `caustics-power` | UPower, logind, power-profiles-daemon, the backlight | the battery chip, its menu, the power menu | no battery, no power menu |
| `caustics-audio` | `pactl subscribe`, `wpctl` | the volume chip, its slider, output and input devices | no audio chip |
| `caustics-network` | NetworkManager (system bus) | the network chip, its menu, networks in range while the menu is open | no network chip |
| `caustics-bluetooth` | BlueZ (system bus) | the Bluetooth chip and its devices | no Bluetooth chip |
| `caustics-notify` | `org.freedesktop.Notifications` (it *is* the daemon) | the bell, its history menu, toasts | no notifications from the bar |
| `caustics-apps` | desktop entries, icon themes | app names, launch argv, colour icons as pixels | the dock shows app ids, letters for icons |
| `caustics-glyphs` | fontconfig, the font files, symbolic icon themes | the font's curves and OpenType tables, symbolic icons as curves, fallback faces | **no text at all** |
| `caustics-reload` | `caustics/`, the user's tunables, slangc | the compiled caustic, on every save | no hot reload; the bar runs the caustic built into it |

Each name resolves to the binary beside the bar's own executable, then to the
name on `PATH`. `$CAUSTICS_SIDECARS` replaces the whole list with a
whitespace-separated list of commands, for trying a new sidecar. A sidecar
that exits is restarted with a doubling backoff from one to thirty seconds;
one that is not installed is one `absent:` line and no retry; one built
against a different ABI is refused on its first frame and not restarted
(docs/abi.md, *Hello*). A sidecar whose service is missing (no NetworkManager,
no BlueZ, no session bus) stays up for its stdin, says `absent: <service>`
once, and publishes an empty frame, so its chip is simply not there.

**A sidecar's settings are its own.** The bar passes none: `caustics-sysmon`
reads `$CAUSTICS_SYSMON_CHARTS`, `caustics-glyphs`'s engine reads
`$PUNCHCUTTER_FONT`, and the bar has no opinion about either. This is also the extension story. A third-party
widget is a program that writes `Rows` frames; `bar/abi/examples/rows_demo.rs`
is a hundred-line one with an icon, two menu rows and a slider. It is not a
script over a text protocol: the wire is binary and the caustic reads it
directly, and a text adapter is the widget author's to write.

Why so strict: the bar's process is the one that must not die, stall or wake
the discrete GPU. D-Bus clients block on replies, font stacks and image
decoders parse untrusted files, NVML opens device nodes that wake a GPU. Each
of those, out of process, can only cost its own feature.

### The caustic

`caustics/bar.slang` is seven compute entry points, in dispatch order
(`ENTRIES` in `bar/abi/src/entries.rs`):

- **The shape submission**, only when something built is owed (a new window,
  a sidecar's rows changing shape, a new minute, a reload). `bar_build` turns
  the facts and the sidecars' bytes into the item and text-block tables;
  `bar_compose` spells the clock into the strings buffer from numbers, the
  tunables' weekday and month literals and their order; `bar_shape` shapes
  every block, one thread per string, into glyphs and pen positions.
- **The frame submission**, every frame the bar draws. `bar_apply` reads the
  commands, lays out the three wings and any open popup, runs the springs,
  decides what a click or a hover means and pushes actions. Then, per mapped
  target (the bar, the popup), `bar_visibility` writes which element owns
  each texel, `bar_sample` reads the id under the pointer, and `bar_draw`
  paints.

Layout is immediate-mode arithmetic over flat arrays. The wings' contents and
order come from the tunables (`LEFT_ORDER`, `CENTER_ORDER`, `RIGHT_ORDER`,
`APPLET_ORDER`), each element's rect is written to the caustic's private
memory, and a `layout:` witness line goes out when one moves. Popups are the
same caustic drawing into another target; lists and sliders are layout in
Slang too. Writing a small immediate-mode layout here, rather than using a
widget toolkit, is deliberate: the layer that failed the old panel was a
toolkit and a proxy, and this one is a file that reloads in under a second.
If the popup layout ever wants a retained widget tree, that is the moment to
ask what the popup is for.

The caustic keeps its own state (the element table, the item and block tables,
the open menu's level, the springs, the last pointer position) in a **private
buffer the ABI does not describe**. The bar process allocates it from the
reflection's stride and never reads or writes it. It survives a hot reload, so
an open menu stays open across a save. A reload that changes the private
buffer's layout is refused with "restart the bar".

## The frame

1. The bar process writes the frame's inputs (time, scale, edge, the clock,
   the output's size), the commands since the last frame, and, when something
   it knows changed, the facts: windows, dock entries, which image slot holds
   which picture, which sidecar is in which slot, the caustic's string
   literals. Sidecars' bytes are already in their arenas: a `Rows` frame is
   copied whole into that sidecar's region and announced with one command.
2. If the caustic asked for a rebuild last frame, the shape submission runs.
3. The frame submission runs, and the bar presents.
4. The bar process reads back one small buffer from **the previous frame**: a
   word saying whether anything is still animating, a ring of actions and a
   ring of witnesses. It forwards each action without resolving it (a window
   id to the compositor, seven words to a sidecar's stdin, a rect and a size
   to `xdg_popup`) and prints each witness. Nothing is read back
   synchronously.

**Render on events.** The bar draws a frame only when something asked for
one: a command, a sidecar's frame, a capture frame, or the caustic's
`wants_redraw` word, which it holds at 1 while a spring is moving and drops to
0 when everything has settled. An idle bar asks for no frames at all. The
clock's minute is a rebuild the process owes on the minute. A bar that has
been animating for more than a few seconds gets a warning line naming what is
still moving, and another each minute it stays that way.

**Rings, not state.** Actions and witnesses are messages pushed into rings
stamped with the frame number, so a frame that fails to present keeps its
stamp and the next attempt appends rather than losing what was pushed. Popup
geometry and the surface's edge and height are messages too: the caustic has
the "have" side in its inputs and compares for itself.

**Witnesses are the caustic's `printf`.** A witness is a format literal in
Slang (`witness(WITNESS_INFO, getStringHash("dock: {0} entries ({1}
running)"), count, running)`) and up to ten words. slangc's reflection maps
each literal's hash to its text, and the process substitutes and prints. A
new log line is one line of Slang and no Rust. The caustic logs once per
change, comparing against what it persisted, so the journal is a list of
decisions, not of frames.

## The ABI

There are two contracts, both in docs/abi.md, byte for byte:

- **The buffer**, between the bar process and the caustic. Built together
  from one tree and changed in one commit; declared once in Rust
  (`caustics-bar-abi`), with the Slang side (`bar_abi.slang`,
  `bar_wire.slang`) rendered from the declaration wherever a compile needs it,
  never kept in the tree. A test compares every struct's Rust offsets with slangc's reflection of the
  Slang, both ways.
- **The wire**, between the bar and any sidecar, which may be built
  separately. Byte-stable framing, and a reader that skips what it does not
  know.

**The install set.** The two contracts and the entry list are hashed at build
time into one `ABI_HASH`, which rides in every binary. A sidecar's first
frame is a `Hello` carrying its hash, and a bar refuses a child whose hash is
not its own. So the eleven binaries are one **set**: `bar/install.sh` builds
them together and installs them into a directory named after the hash, with
`~/.local/bin/caustics-bar` a symlink into it. A bar resolves its sidecars beside
its own executable, so a running bar keeps using its own set even after a
newer one is installed. The reason is concrete. A bar and a sidecar from two
different builds once agreed on every version number and disagreed on what a
buffer index meant: the shaper read a sorted list of codepoints as an
OpenType table directory, each dispatch took 150 ms, and the desktop's cursor
stuttered. Per-kind version words could not have caught it, because the bytes
were well-formed. One hash over the whole contract can.

## Text

The text engine is **punchcutter** (README.md links it), a dependency:
OpenType shaping and Slug glyph coverage in Slang compute shaders, with a CPU
side that finds fonts through fontconfig and parses them into bytes. The bar's caustic imports its `text` and `shape` modules, and the
bar's binary links only its byte views, its shaping structs and its slangc
driver. Its contract (the curve buffers, the arena, the merged glyph space,
the shaping dispatch) is `punchcutter/docs/arena.md`; its gaps (bidirectional
text, complex-script shapers, colour fonts, variable instances) are on its
own roadmap.

What the bar adds on top:

- **The font arrives over a pipe.** `caustics-glyphs` loads the font through
  `punchcutter_fonts` (`$PUNCHCUTTER_FONT`, else the first UI family
  fontconfig has that carries the glyphs the caustic composes into runs of
  its own: the chart arrows, the degree sign, the dash for no value, the
  ellipsis; the list is the sidecar's `COMPOSED`, and a test keeps it equal to
  the tunables' literals) and writes the engine's six buffers to the bar as `Font` and
  `Bytes` frames. The bar copies them into the bindings the engine's modules
  declare and reads none of it (docs/abi.md, *Font*, *Bytes*).
- **Covers go back over the same pipe.** When the caustic finds a run with a
  glyph the font lacks, it pushes that run's bytes, and the bar sends them to
  `caustics-glyphs` as a `REQ_COVER` request. The engine opens the fallback
  faces that draw them into the arena; the sidecar answers with the curve
  tails, the records filled in place, the arena's changed ranges and the new
  shape scalars, and the run reshapes (docs/abi.md, *Request*).
- **Symbolic icons are glyphs** in the same buffers (*Images and icons*).
- **A watchdog above the engine's bounds.** The engine treats the arena as
  hostile: every list is capped and charged to an operation budget, and each
  run holds a wall-clock deadline where `VK_KHR_shader_clock` is available.
  Above that, the bar times the shape pass with a timestamp query and, past
  `SHAPE_WATCHDOG_MS`, stops shaping until the arena's front block is written
  again. A malformed font draws wrong; it never hangs the graphics ring.
- **Strings are the caustic's.** Every label the bar shows that is not
  someone else's data (a dock-menu row, the clock's weekday and month names,
  an icon name) is a string literal in Slang, named by `getStringHash`. The
  reflection carries each literal's text, the process copies those bytes
  into the strings buffer, and the caustic shapes them. A label's wording is
  a tunable like a colour, and no CPU code knows what a menu says.

## Images and icons

- **Symbolic icons are curves.** Adwaita-style symbolic icons are plain filled
  paths, which Slug fills like a glyph. `caustics-glyphs` resolves the name
  through the icon theme (I/O) and appends the outline to the same curve
  buffers, so a symbolic icon is a glyph id.
- **Everything else is pixels in a fixed-slot array**: 64 slots of 64×64
  straight RGBA8, premultiplied by the caustic at the sample. Colour
  application icons come from `caustics-apps` (keyed by the app id), a tray
  item's pixmap from `caustics-tray` (keyed by a content hash, so an unchanged
  icon is never sent twice), a notification's image from `caustics-notify`. Each
  arrives as a `Pixmap` frame before the first frame that names its key. The
  process keeps every pixmap in a bounded store (256 keys, 4 MiB) and lands
  one in a slot when the caustic wants it.
- **What is drawn is the caustic's word.** Each build reports the slots it
  placed. A slot that is on the bar is never evicted, and an eviction takes
  the least recently drawn slot. (Marking a slot as used merely because its
  name had once been asked for filled the array within a day, after which
  every new icon was refused.)
- **The resolution order** for a name: curves first (`caustics-glyphs`, which
  answers fast with "missing" for anything not symbolic), then pixels. A chip
  whose icon has not resolved wears its title's first letter.
- **Hover previews** are stills of the window under the pointer, captured
  over `ext-image-copy-capture` into shm at no more than two frames a second,
  one capture session at a time, created on hover and torn down on leave. The
  bar never becomes the screen recorder it is drawing.

## Menus, toasts and popups

- **A sidecar's menu is its rows.** A `Rows` frame is a small tree: chips on
  the bar, menu rows, separators, sliders, submenus, with labels, icon names,
  flags (checkable, radio, disabled, attention) and a slider's value and
  range. The caustic lays it out and decides what a click means. A click goes
  back to that sidecar's stdin as a 32-byte action naming the row, and the
  sidecar's answer is its next frame.
- **A menu stays open while its rows change.** The caustic holds the open
  level by the row's id, not its index, so a network list that reshapes while
  you are reading it moves nothing you are holding. The sidecar is told when
  its menu opens and closes: the network sidecar scans for access points only
  while its menu is up, and the power sidecar reads the backlight only then.
- **Long menus scroll.** The caustic knows the output's size and clamps a
  menu to it, less the bar and a margin. The wheel scrolls rows within the
  band while the back row stays pinned.
- **Toasts** are popups without a grab. A sidecar flags its chip "show my
  rows without a click"; the caustic opens the flagged rows as a toast, holds
  those rows fixed while it is up (a row that arrives meanwhile waits for the
  next toast, one that leaves is a gap, and the popup never moves under the
  pointer), and tells the sidecar which rows it actually showed when it
  closes.
- **The calendar** is the clock's menu: the month laid out in the caustic
  from the date words the process writes.
- **The dock's menu** is the bar's own rows. `DOCK_MENU` in the tunables
  lists them as (label hash, action, when) triples, so its wording and its
  rows are configuration.

## Notifications

`caustics-notify` is a complete `org.freedesktop.Notifications` daemon (Desktop
Notifications 1.2) as a sidecar. It requests the name allowing replacement and
replacing an existing owner, without `DoNotQueue`: where another daemon allows
replacement it takes over at once, and otherwise it waits in the queue,
publishing nothing, and serves from the moment the name is its own.

- The bell chip carries the unread count. Its menu is do-not-disturb, the
  notifications newest first (summary as the title, the body wrapped beneath
  it), and clear-all.
- A notification's click asks the compositor for an `xdg_activation_v1`
  token through the bar (the bar owns the surface the token is tied to) and
  emits `ActivationToken` before `ActionInvoked`, as the specification
  orders, so the application may raise itself. The click is held at most
  100 ms for the token, so a compositor that grants none costs nothing.
- `ActionInvoked` and `ActivationToken` are sent to the notification's
  sender, not broadcast. Applications match those signals by id alone, ids
  repeat across daemon restarts, and a broadcast can focus an unrelated
  application that happens to hold the same id.
- The id counter and every non-transient notification persist in
  `$XDG_STATE_HOME/caustics/notify/history.json`, so a restart (every install is
  one) neither reuses ids nor re-toasts what was already shown.

## Configuration is the caustic

There is no configuration file, and there will not be one. Every knob is a
`public static const` in `caustics/bar_tunables.slang`: colours, sizes,
opacity, the edge and height, which kinds sit in which wing and in what order,
spring constants, the hover dwell, the dock menu's rows, the clock's words and
date order, the shape watchdog. To change the bar, edit that file in the
checkout the bar was built from and save: `caustics-reload` recompiles, and
the running bar moves within a second, keeping its popup, its menu level and
its springs. A file that does not compile keeps the last good bar and prints
slangc's diagnostic, naming the file. A different bar is a fork of the code,
and an update is a merge into it like any other.

Why Slang rather than a configuration file: with a file, every new knob is a
field in the file, a field in a parser, a field in a struct copied to the GPU
and a read in the shader. The caustic is already the thing that is
hot-reloaded, compiled and checked. Making it the configuration deletes the
other three and lets any constant (a colour, an ordering, a menu's wording,
an easing) be tuned live. The facts the process must act on (the edge and the
height) the caustic sends as one message on its first frame and again
whenever a reload changes them, and the process re-anchors the surface to
match.

What cannot be Slang is two lists of strings, and neither is configuration.
The dock's favourites are **state** (`$XDG_STATE_HOME/caustics/bar/favourites`,
written by Pin and Unpin; a compiled-in list seeds it until the first pin),
and a sidecar's own choices (which charts `caustics-sysmon` samples) are that
sidecar's affair, reaching the bar only as a self-describing frame.

The settings program a sidecar's "Settings…" row opens is likewise not the
bar's. One function in `caustics-sidecar` picks it: `$CAUSTICS_SETTINGS` if set, else
the desktop's own by `$XDG_CURRENT_DESKTOP` (`cosmic-settings`,
`gnome-control-center`, `systemsettings`), else the first of those on `PATH`,
else no settings row at all.

## Power and the discrete GPU

A status bar must never be the reason a laptop's discrete GPU is awake.

- **Never enumerate a sleeping discrete GPU's driver.** On a hybrid laptop,
  merely enumerating the NVIDIA Vulkan driver can wake the device. The bar
  asks wgpu for a low-power adapter and **refuses outright to run on an
  adapter named NVIDIA** (exit 2), unless `CAUSTICS_BAR_ALLOW_DGPU=1`. On a hybrid
  laptop the unit should also pin the Vulkan loader to the integrated GPU's
  driver, so the other driver is never loaded at all;
  `bar/systemd/dropins/igpu-only.conf` is that drop-in
  (`bar/systemd/README.md`). An NVIDIA-only machine sets the override, or
  runs on Mesa's lavapipe.
- **NVML stays out of the bar's address space.** The GPU-usage chart needs
  NVML on NVIDIA, and opening NVML opens `/dev/nvidia*`, which wakes a
  runtime-suspended device and keeps it awake while the handle is held.
  `caustics-sysmon` opens it only when the device is already `active`, gives
  it back once nothing else has used the device for a few seconds, and then
  rests until the device has suspended or has shown it is in use
  (`sysmon/README.md`, *The NVML lease*). If that logic is ever
  wrong, it is wrong in a sidecar that can be killed, not in the bar.
- **Render on events, not on vblank** (*The frame*, above). An idle bar
  draws nothing.
- **Memory.** The unit caps the bar at 1 GB. The bar itself sits far below
  that; the headroom is for `caustics-reload`'s slangc, which runs in the unit's
  cgroup and peaks at 350–450 MB compiling one entry point. (A 256 MB cap
  killed the whole unit on every uncached reload.)

## Rules

Each rule below answers a failure that was seen, in the panel the bar
replaced or in the bar's own first months.

| Rule | The failure behind it |
|---|---|
| One process. The bar's own surfaces, its own popups, the grab before the first commit. Nothing proxied, ever. | A nested compositor proxying applets' popups raced their grabs against their configures. |
| A fatal protocol error exits (status 1) and the unit restarts the bar. The new one starts from the facts and its sidecars' frames, which are all it needs. A dead bar never lingers. | A panel kept running on a dead Wayland connection, with no surfaces. |
| The bar never depends on lenient validation. Positioner values are computed by the caustic and clamped by the process; no client input reaches a validator through the bar, so no 0×0 anchor rect ever reaches the compositor. | A compositor's validator stricter than the protocol killed the client; a client-supplied 0×0 anchor rect crashed every applet at once. |
| Standard `ext-*` and `wlr` protocols only. Every optional global degrades to "feature absent" with one line. | Applets hard-bound vendor-specific globals, and the dock was empty on another compositor. |
| No third-party widget toolkit in the frame path. Hover is a texel fetch plus a timer, both the bar's. | A toolkit dropped a pointer-leave and a popup stayed on screen. |
| Logs go to the journal directly (the unit sets `StandardError=journal`). | A panel's logs were swallowed by its session manager. |
| Watchdogs are backstops, not fixes, and are labelled so. | "Fixed" had come to mean "recovered faster". |
| A feature is done when it is seen working on the screen. The journal's witnesses are how a test sees it. | Features verified from a configuration file instead of on screen. |
| The CPU keeps I/O and protocol round trips; computation over data the GPU can hold is the caustic's. Applied in order of payoff: strings, glyphs, icons, shaping. | Each step retired more bar code than it added: the rasteriser, the atlas, the font stack and the shaper all left the bar process. |
| A name a witness may quote later must name bytes that do not move. Those names live in a region of the strings buffer written once per process. | A witness that quoted a string range one frame later read a recomposed buffer and printed nonsense. |
| Ids, not indices. Anything that must survive a rebuild (the open menu level, an element's item, a toast's rows) is held by the id it was built from, never by its position in a table the next build re-indexes. | Each violation was a click landing on the wrong row. |
| Every zbus signal stream is handed, the moment it exists, to a task that awaits nothing else (`caustics_sidecar::drain` or `caustics_sidecar::forward`), and each D-Bus sidecar has a test that reproduces the hang against a private `dbus-daemon`. | A zbus signal stream is a bounded channel the connection's socket reader blocks on. A task that held a stream without polling it while awaiting a method reply deadlocked the whole connection. |
| A budget charge lives in the loop body, never in the exit condition. The oracle runs the bar's own passes on the machine's own GPU with a real font and asserts zero notdefs. | One driver compiled `for (i < n && charge(1u))` deep in an inlined shader to run no iteration at all, while lavapipe ran it correctly. The oracle is the only test that would have seen it. |
| A row's icon is a name. Pixels travel as their own keyed frames. | Pixels inside a rows frame pushed a tray's frame 32 bytes past its 64 KiB region, and every frame after was refused. |
| Install a set, never a binary (*The ABI*, above). | A bar and a sidecar from different builds disagreed on what a buffer index meant. |
| Witness once per change, at the pass that knows. The caustic compares against what it persisted; the process dedups nothing. | Without it a soak's journal was unreadable. |

**Accessibility: none.** There is no accessibility tree and no screen-reader
support. Said up front so nobody discovers it later.

## Open questions

- **Other compositors.** The bar has run only on niri. The Wayland side uses
  standard protocols, but focus, urgency and the four window actions go
  through niri's IPC. Another compositor needs another implementation of the
  compositor seam, and a window key its IPC can address. Hover previews need
  a compositor that offers image capture over a foreign-toplevel source.
- **The hot-reload stall.** A swap whose pipelines are new to the driver
  blocks the bar's event loop for several seconds on RADV, because pipeline
  creation runs on the loop (docs/testing.md). Moving it off the loop, and
  swapping when the new pipelines are ready, is the obvious fix and not yet
  done.
- **The rest of the text engine**: bidi and Unicode properties, the
  complex-script shapers, colour glyphs, variable fonts (*Text*, *Not yet*).
- **`MAX_ROWS`.** A sidecar frame carries at most 256 rows. A tray with a few
  chatty items approaches that, and the truncation, while honest (one line,
  the rest published), is a menu missing its last rows. Raising it means
  growing each sidecar's region of the rows arena with it.
