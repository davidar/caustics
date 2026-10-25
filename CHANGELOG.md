# Changelog

## Unreleased

What exists today, before a first tagged release.

**The bar** (`caustics-bar`). A Wayland layer-shell status bar whose process owns
its Wayland objects, its GPU device, the compositor's IPC socket and its
sidecars' pipes, and decides nothing. Everything drawn, and every decision
about it, is one Slang program hot-reloaded while the bar runs. Three wings,
ordered by the tunables: a dock (pinned favourites, running windows grouped by
application, click to focus and cycle, a right-click menu with New window,
Close, Pin and Unpin, hover previews captured from the compositor), a clock
with a month calendar, charts, and the sidecars' chips (tray, network,
Bluetooth, audio, battery, notifications, power) plus an idle-inhibit toggle.
Popups are the bar's own: menus with submenus, sliders, check and radio rows,
scrolling when a menu is taller than the output, and toasts without a grab.
Configuration is the caustic's tunables module, edited in the checkout and
hot-reloaded on save; the dock's favourites are state. One bar per edge, enforced by a
lock. Exit codes the unit understands: restart on a protocol error, stay down
on a refusal or a lost GPU device.

**The ABI** (`caustics-bar-abi`). Two contracts: the buffer between the bar
process and its caustic, declared once in Rust with the Slang side generated
and checked against Slang's reflection; and the wire between the bar and its
sidecars, length-prefixed binary frames read by a reader that skips what it
does not know. One hash over both, carried by every binary and checked on a
sidecar's first frame. `bar/install.sh` installs the bar, its ten sidecars,
the canvas host, its sidecar and its client as one set named by that hash.
Every payload crosses as one frame: a compiled entry point, an output-sized
picture and a font's buffer each go whole, with nothing cut into pieces and
put back together. The reader takes a body in bounded steps as it arrives, so
a header that lies about its length costs only what its writer really sent,
and the frame cap is the largest honest body (a picture at the largest texture
size), computed from the wire's own bounds. That change moved the ABI hash: a
set from before it refuses the sidecars of a set after it, and the bar and
every sidecar are rebuilt and installed together.

**The sidecars.** `caustics-sysmon` (CPU, temperature, memory, network, disk and
GPU charts, with a guard that never wakes a suspended NVIDIA GPU), `caustics-tray`
(StatusNotifierItem and DBusMenu; the watcher when none exists), `caustics-power`
(UPower battery with time to empty, logind's power menu, power profiles,
backlight brightness), `caustics-audio` (volume and mute, output and input
devices), `caustics-network` (NetworkManager connections, networks in range while
the menu is open), `caustics-bluetooth` (BlueZ: the adapter on and off, the paired
devices), `caustics-notify` (a Desktop Notifications 1.2 daemon: history that
survives restarts, do-not-disturb, toasts, activation tokens so a click can
raise its application), `caustics-apps` (desktop entries and colour icons),
`caustics-glyphs` (the font and symbolic icons as curves, fallback faces on
demand) and `caustics-reload` (the shader compiler, Slang in its own
process, with a cache keyed on the bytes it compiles). Every D-Bus signal
stream is drained by a task that awaits nothing else.

**The canvas** (`caustics-canvas`, docs/canvas.md). A second host of a
caustic on the same mechanism (`caustics-host`): the wallpaper on every
output, on the background layer with fractional scale and a viewport,
cross-faded between two source pictures at an alpha the caustic computes
from `CLOCK_BOOTTIME`, so a fade lands where the clock says across a
suspend; hot-reloaded from `caustics/canvas*.slang` like the bar. Its
sidecar `caustics-wallpaper` owns a control socket, decodes PNG, JPEG and
JPEG XL and fits them to the largest output; `caustics-canvasctl` is the
client for scripts. Dim and undim cross the wire and darken the wallpaper;
the overlay that will carry them over windows is not built yet.

**Text**, by punchcutter, a dependency: glyphs and symbolic icons drawn from
their outlines on the GPU with the Slug algorithm, and OpenType Layout
applied on the GPU from the font's own tables, both as Slang modules the
caustic imports. The bar adds the font sidecar's wire (the engine's buffers as
`Bytes` frames, fallback faces covered on request over the pipe), symbolic
icons as glyphs, and a watchdog on the shape pass. The font is
`$PUNCHCUTTER_FONT` or fontconfig's choice, and its curves are cached under
`$XDG_CACHE_HOME/punchcutter/`. The engine's gaps
(bidirectional text, complex-script shapers, colour glyphs, variable-font
instances) are on its roadmap.

**The tests.** Unit tests in every crate, against recorded and synthesised bus
bodies; a GPU oracle that runs the caustic's own passes on a real device; the
nested gate, about 280 checks against a nested niri on the machine's GPU under
a headless sway with a private session bus; and a smoke run of the same steps
in a virtual machine (docs/testing.md).

**In use.** The bar is its author's only panel and notification daemon in a
niri session, in place of a desktop's own panel and notification daemon.
