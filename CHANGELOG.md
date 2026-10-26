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
the canvas host, its sidecar, its client, its polkit sink and its lock
sidecar as one set named by that hash.
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
JPEG XL and fits them to the largest output, re-shows the last picture when
it or the host restarts, and gives a socket that still answers a short
window to go away before reading it as another wallpaper's (the previous
sidecar of a restarted set can answer a probe while it is torn down);
`caustics-canvasctl` is the client for scripts. The idle fade replaces chayang: a dim is an overlay per
output, over every window and the bar, that takes the pointer and the
keyboard and is cancelled by any input. It draws the wallpaper itself at the
dim's alpha, so the windows dissolve into the desktop's picture, darkened a
little (`DIM_DARKEN`, 0 for none); the background never darkens. `caustics-canvasctl dim 1 over 10 --wait` exits 0 when the
dim reached its depth, 2 when input cancelled it and 1 when it could not
run, so an idle daemon's warning stage can tell the user coming back from a
fade that ran out. A crashed wallpaper sidecar takes its dim with it. The
host answers the sidecar with a new wire kind, `CanvasEvent`, and a dim now
carries an id in what was a reserved word; both moved the ABI hash, so the
bar, its sidecars and the canvas are rebuilt and installed together. The
canvas's surfaces no longer allocate the visibility buffer only the bar's
caustic binds. The bar's wake reaches the canvas: the pointer moving over the
bare desktop or the lock screen stirs a pool over the wallpaper, the picture
bent by the waves and lit by the caustic they cast, from one module of the
water's physics both caustics import (`caustics/water.slang`), its knobs the
canvas's own `WAKE_*` tunables; the pointer's pane and place and a lock
surface's own target moved the ABI hash.

**The canvas's polkit sink** (`caustics-polkit`, polkit/README.md). The
session's polkit authentication agent as a sidecar: it registers with
polkitd, turns each request into a prompt conversation with the canvas host
over two new wire kinds, `Prompt` and `Answer`, drives polkit's own helper
with what was typed, and reports the outcome; one conversation at a time,
the rest queued. The typed bytes are zeroed by every copy the wire makes
and are never logged. A dry run touches no system bus, for the tests. The
two kinds moved the ABI hash, so the whole set (now fifteen binaries) is
rebuilt and installed together. The canvas answers it since its prompt
(below), with the sink added to the canvas's set by a drop-in.

**The canvas's prompt.** The canvas now answers its sinks: a conversation's
first frame puts a pane on the top layer of every output, opaque, the
wallpaper darkened by `PROMPT_DARKEN` with a panel on it (the title, the
ask's words, the answer's field, the last line, an error in red), and takes
the keyboard. Until the first ask the panel is held, title and no field
(`PROMPT_HELD`): a PAM stack that tries a fingerprint reader first asks for
the password only after that module gives up, and a panel that waited for
the ask showed nothing for those seconds. That state moved the ABI hash. A
title too long for the panel takes up to `PROMPT_TITLE_LINES` lines, broken
at a space or, in a word with none (a command's path), inside it, instead of
ending at the panel's edge after two. The seat's keymap is compiled by libxkbcommon, opened at run
time, with compose; Return answers, Escape dismisses, BackSpace takes a
character, Control-U clears. What is typed lives in one zeroing buffer in
the host and leaves it once, down the asking sidecar's pipe; the caustic is
given a count for an echo-off ask and draws dots. One conversation at a
time; a failed attempt is counted, never described; the sink's pipe closing
takes the prompt down; while a prompt is up a dim stays as it was. The
canvas draws text as the bar does, shaped and cut on the device, from its
own `caustics-glyphs`, now in its default set; the engine's buffers, the
cover requests and the shape watchdog moved from the bar into
`caustics-host` so both hosts run the same code. `caustics-polkit` is not
in the canvas's default set yet (canvas/README.md says how to add it). The
canvas's buffer contract and entry list grew, which moved the ABI hash, so
the set is rebuilt and installed together.

**The canvas's lock sidecar** (`caustics-lock`, lock/README.md). The
session's lock without its surfaces: it locks on logind's `Lock` signal or
`caustics-canvasctl lock`, sets `LockedHint` when the canvas host says the
compositor locked, holds each attempt as one PAM transaction turned into a
prompt conversation, and asks the host to unlock only after a success; a
failure or an Escape starts a fresh attempt at once, and logind's `Unlock`
is ignored. A hint already set at start re-locks before anything else.
`caustics-canvasctl lock --wait` is the before-sleep hold. A dry run
replaces PAM with one ask, for the tests. A new wire kind, `Lock`, and two
canvas events, `locked` and `unlocked`, moved the ABI hash, so the whole
set (now sixteen binaries) is rebuilt and installed together; the canvas
does not start the sidecar until it has lock surfaces. Its socket gives a
previous lock still on its way out two seconds to let go, as the
wallpaper's does, instead of running without a socket; `caustics-canvasctl
status` says on stderr when the lock refused to run rather than printing
nothing; and the canvas journals the keyboard arriving on an overlay. The
sidecar also verifies a fingerprint beside the password, speaking to
fprintd on the system bus itself rather than through PAM (which asks one
thing at a time): a match unlocks, a no-match is a line on the panel and a
fresh verify, a reader that goes is claimed again with a backoff, and the
reader is released at the unlock; `CAUSTICS_LOCK_FINGERPRINT=0` turns it
off. The canvas journals a line shown beside an ask, without its words.

**The canvas's lock surfaces.** The canvas host now locks: the lock
sidecar's `Lock` takes `ext_session_lock_v1`, a lock surface goes on every
output (and on any that appears while locked), opaque, the wallpaper
darkened by `PROMPT_DARKEN` with the lock's prompt panel on it, and the
host tells the sidecar `locked` when the compositor confirms and
`unlocked` once the compositor has read the unlock. Only the sidecar that
locked unlocks. While locked the prompt is drawn on the lock surfaces and
is the lock's alone (another sink is told no at once), and a dim up at the
lock is ended. The compositor ending or refusing the lock is fatal, so the
unit restarts the host; a host that dies locked leaves the session locked,
and a restarted host sends its lock before its device is up. The lock
sidecar stays out of the canvas's default set: canvas/README.md says how
to take it on in place of the session's previous locker. No wire or
buffer contract changed. The VM target's set upload gained the canvas's
five binaries, so its install step's sixteen hold there.

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
