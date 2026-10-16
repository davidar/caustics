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
and checked against slangc's reflection; and the wire between the bar and its
sidecars, length-prefixed binary frames read by a reader that skips what it
does not know. One hash over both, carried by every binary and checked on a
sidecar's first frame. `bar/install.sh` installs the bar and its ten sidecars
as one set named by that hash.

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
demand) and `caustics-reload` (the shader compiler, with a cache keyed on the
bytes it compiles). Every D-Bus signal stream is drained by a task that awaits
nothing else.

**The text engine.** Glyphs and symbolic icons drawn from their outlines on
the GPU with the Slug algorithm; OpenType Layout applied on the GPU from the
font's own tables (HarfBuzz's default shaper: GSUB 1–8, GPOS 1–9, GDEF),
matched against harfrust over HarfBuzz's test suite; fallback faces from
fontconfig, with the substitution closure of what they cover; every walk over
font data bounded, with a wall-clock deadline where the device supports one
and a watchdog on the pass. Not yet: bidirectional text, Unicode
normalisation, complex-script shapers, colour glyphs, variable-font instances.

**The tests.** Unit tests in every crate, against recorded and synthesised bus
bodies; a GPU oracle that runs the caustic's own passes on a real device; the
nested gate, about 280 checks against a nested niri on the machine's GPU under
a headless sway with a private session bus; and a smoke run of the same steps
in a virtual machine (docs/testing.md).

**In use.** The bar is its author's only panel and notification daemon in a
niri session, in place of a desktop's own panel and notification daemon.
