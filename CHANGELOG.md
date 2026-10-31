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
Bluetooth, audio, displays, battery, notifications, power) plus an idle-inhibit toggle.
Popups are the bar's own: menus with submenus, sliders, check and radio rows,
scrolling when a menu is taller than the output, and toasts without a grab.
Configuration is the caustic's tunables module, edited in the checkout and
hot-reloaded on save; the dock's favourites are state. A machine with no
favourites file starts with none, rather than a compiled-in list of
applications it may not have; the right-click Pin row adds the first. One bar per edge, enforced by a
lock. Exit codes the unit understands: restart on a protocol error, stay down
on a refusal or a lost GPU device.

**The ABI** (`caustics-bar-abi`). Two contracts: the buffer between the bar
process and its caustic, declared once in Rust with the Slang side generated
and checked against Slang's reflection; and the wire between the bar and its
sidecars, length-prefixed binary frames read by a reader that skips what it
does not know. One hash over both, carried by every binary and checked on a
sidecar's first frame. `bar/install.sh` installs the bar, its eleven sidecars,
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
stream is drained by a task that awaits nothing else. The power menu's
Lock reads each session's `LockedHint` for three seconds after asking
logind, and says when it stayed false (no locker listens to logind's `Lock`,
or the one that does sets no hint) instead of witnessing a lock. Log Out
without `$NIRI_SOCKET` ends the user's own session through logind's
`Session.Terminate` rather than doing nothing, and says which way it went.
No sidecar launches a settings program: the "Settings…" rows of the network,
Bluetooth, audio, power and notification menus are gone, and with them
`caustics-sidecar`'s choice of program and `$CAUSTICS_SETTINGS`.

**Displays** (`caustics-displays`, displays/README.md). A chip beside the
power chip whose menu lists niri's outputs, each with On, Scale (1 to 3 and
the current one), Mode (niri's list, the preferred one named) and Rotation
(the four rotations), set over niri's IPC socket with the same `Output`
requests `niri msg output` sends; every outcome is one witness. The outputs
are read at start, when the menu opens and every two seconds while it is
up, and never on a clock while it is closed. The only output on cannot be
turned off. A change lasts until niri restarts, and the menu says the
layout is niri's config's. Without `$NIRI_SOCKET` there is no chip. Its
applet value, `APPLET_DISPLAYS`, is new in the wire's contract, which moved
the ABI hash: the bar and every sidecar are rebuilt and installed together.

**The canvas** (`caustics-canvas`, docs/canvas.md). A second host of a
caustic on the same mechanism (`caustics-host`): the wallpaper on every
output, on the background layer with fractional scale and a viewport,
cross-faded between two source pictures at an alpha the caustic computes
from `CLOCK_BOOTTIME`, so a fade lands where the clock says across a
suspend; hot-reloaded from `caustics/canvas*.slang` like the bar. Its
sidecar `caustics-wallpaper` owns a control socket, decodes PNG, JPEG and
JPEG XL and fits them to the largest output, re-shows the last picture when
it or the host restarts and at the first start of a new session (the path
is kept under `$XDG_STATE_HOME` as well as the runtime directory), and gives a socket that still answers a short
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
and are never logged. A dry run touches no system bus, for the tests.
polkit's helper is looked for where distributions install it
(`/usr/lib/polkit-1`, `/usr/libexec/polkit-1`, `/usr/lib/policykit-1`,
`/run/wrappers/bin`), the start's journal line says which it found or that
it found none, and `CAUSTICS_POLKIT_HELPER` still names one outright. The
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
`caustics-canvasctl lock --wait` is the before-sleep hold. The PAM service is
found rather than assumed: the lock's own `caustics-lock` file, else a
locker's the system already has, else `login`, and the witness says which;
`CAUSTICS_LOCK_PAM_SERVICE` still names one outright. A dry run
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
off. The reader is held in windows while someone is at the lock (the lock
going up, input on a lock surface, which the canvas tells the sidecar as
`CANVAS_EVENT_INPUT`, and a resume from suspend) and let go between them,
since libfprint disables a sensor that has run for a few minutes; the
panel's line says whether the reader is live, waits for a key, or is not
available; a reader
that goes right after its verify started is claimed again after a doubling
pause rather than every second (`CAUSTICS_LOCK_FINGERPRINT_WINDOW`). The
canvas journals a line shown beside an ask, without its words.

**The canvas's lock surfaces.** The canvas host now locks: the lock
sidecar's `Lock` takes `ext_session_lock_v1`, a lock surface goes on every
output (and on any that appears while locked), opaque, the wallpaper
darkened by `PROMPT_DARKEN` with the lock's prompt panel on it, and the
host tells the sidecar `locked` when the compositor confirms and
`unlocked` once the compositor has read the unlock. Only the sidecar that
locked unlocks. While locked the prompt is drawn on the lock surfaces and
is the lock's alone (another sink is told no at once), and a dim up at the
lock is ended. The lock is the deepest dim: a dim asked while locked is
reached at once with nothing drawn, so a `dim --wait` is done straight
away, and one still up at the unlock ends with it, so the unlock never
lands on a dimmed desktop. The compositor ending or refusing the lock is fatal, so the
unit restarts the host; a host that dies locked leaves the session locked,
and a restarted host sends its lock before its device is up. The lock
sidecar stays out of the canvas's default set: canvas/README.md says how
to take it on in place of the session's previous locker. No wire or
buffer contract changed. The VM target's set upload gained the canvas's
five binaries, so its install step's sixteen hold there.

**The canvas's network secret agent** (`caustics-secrets`,
secrets/README.md), and joining from the bar. A click on a Wi-Fi network in
range that has no profile no longer opens a settings program: the bar's
network sidecar asks NetworkManager to add and activate a profile for it
(`AddAndActivateConnection2`, kept on disk), with the key management the
access point advertises (WPA-PSK, WPA3's SAE, Enhanced Open or none) and
no secret, so NetworkManager asks its secret agent. That agent is a new
sink of the canvas: registered with NetworkManager's agent manager on the
system bus, it turns each `GetSecrets` that allows interaction into one
prompt conversation (the network's name as the title, a "wrong password"
line first when NetworkManager refused the last answer), and returns what
was typed in the method's reply and nowhere else; a dismissal is
`UserCanceled`, NetworkManager's cancel takes the prompt down, and a call
that may not interact gets `NoSecrets` with nothing shown. A WPA-PSK
passphrase of the wrong length is refused on the panel. The agent stores
nothing, and the reply's copy of the text is zeroed once it is sent. A
join that ends before it ever connected deletes the profile it added, so
a wrong or abandoned password leaves no dead profile; an 802.1X or WEP
network's row says it needs a profile and does nothing. The agent is
opt-in, like the polkit agent (canvas/README.md, *Enabling the secret
agent*). No wire or buffer contract changed; the set is now seventeen
binaries.

**Bluetooth pairing from the bar**, and the canvas's pairing sink for a
PIN to type (`caustics-pairing`, pairing/README.md). The bar's Bluetooth
menu now discovers on every powered adapter while it is open, is
discoverable to other devices for the same while (so a pairing can start
from a phone), and lists the devices in range that are not paired and
have a name, strongest signal first, below the paired ones; a click pairs
one (`Pair`, then `Trusted` and `Connect`, each step witnessed, a failure
by BlueZ's error name), and a middle click on a paired device forgets it.
The Bluetooth sidecar is BlueZ's pairing agent, registered as the default,
and asks each question where it belongs. A yes or a no (a passkey to
compare, a pairing or a service to allow) is three rows on its chip, the
question, Confirm and Reject, shown as a toast without a click and in the
menu when it is open. A code to type on the other device is a toast too,
updated as the device reports digits typed and gone once it pairs. Only a
PIN or a passkey that must be typed on this machine goes to the canvas:
the sidecar asks the pairing sink over
`$XDG_RUNTIME_DIR/caustics-pairing.sock`, the canvas asks on its prompt,
and the answer goes to BlueZ in the method's reply and nowhere else, its
copies zeroed once sent; BlueZ's cancel takes the toast or the prompt
down. The sink is opt-in, like the secret agent (canvas/README.md,
*Enabling the pairing agent*); without it the agent registers as
`DisplayYesNo`, and only a device that insists on a typed PIN is refused.
No wire or buffer contract changed; the set grows by one binary.

**Pipelines off the loop.** Both hosts build their caustic's pipelines on
two compile workers instead of on their event loops, one at a time in the
order a frame needs them, and install each as it lands with a
`pipeline <name>: built in <t>` line. A cold driver cache (after a driver or
caustic change) cost the bar about sixteen seconds on RADV and the canvas
nine before either drew or read anything; now the canvas draws its
wallpaper a fifth of a second in, while its shaper is still building, a
restarted canvas whose session is locked locks before any pipeline exists,
and the bar's whole program is in after about twelve. Two workers rather
than one: with one, the bar's chips came at 8.3 s and its whole program at
16.2 s; with two the chips come at 8.4 s and everything at 11.7 s, the
shaper building beside the rest. The bar draws its chips before its text,
as it does while waiting for a font. A pipeline counts as built once it has
been dispatched once on its worker, over scratch bind groups: lavapipe
compiles at a pipeline's first dispatch, which put 14 s of the canvas's
shaper on its first shaped frame and now puts it on the worker, and the
hosts draw nothing while such a dispatch is on the device's queue, since
lavapipe runs that queue on one thread and a frame behind it waited the
whole compile. A hot reload
builds beside the caustic that runs, which keeps drawing and answering
input, and swaps in whole when its last pipeline is built and validated;
a newer save supersedes a generation still building. No wire or buffer
contract changed.

**A pipeline per canvas surface.** The canvas's draw is built once for each
surface (the background, the overlay, the prompt's pane, the lock), with a
specialisation constant the host writes into the module's words, so the
driver drops the other surfaces' code: the background's draw is 1347
instructions on RADV where the one draw for every surface was 7677, and the
overlay's needs half the registers. A surface whose pipeline is still
building skips its frame there. punchcutter moves to the revision whose
reflection passes over a specialisation constant. No wire or buffer
contract changed.

**A cheaper layout pass to compile.** `bar_apply`, the one-thread pass the
bar's first frame waits for, cost RADV 4.6 s to compile cold and now costs
1.9 s; on lavapipe, creation and first dispatch together went from 12.1 s
to 7.9 s, and the whole bar's seven pipelines on one worker from 13.1 s to
10.1 s on RADV. The driver's time was in unrolling: every inlined witness
and action carried two ten-step loops, each wing's walk over its order
array was unrolled per kind, and every inlined branch that leaves a
function early, such as `rows_word`'s bounds check at hundreds of call
sites, becomes a loop the driver unrolls by rewriting the whole function.
A record is now written straight into its ring slot with one rolled loop,
the wings' walks stay rolled, and the hot helpers have no early return.
`bar_build` gained 14% on RADV and nothing on lavapipe; the pass's own
time per frame is unchanged at 0.18 ms. The lines printed and the actions
pushed are the same words in the same order.

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
