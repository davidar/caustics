# The canvas

Steps 0 to 4 of *Order* are built: the host crate, the canvas host with its
wallpaper sidecar (`canvas/`, `wallpaper/`, `caustics/canvas*.slang`;
canvas/README.md runs it), the fade on its overlay, the prompt core with
its polkit sink (`polkit/`, docs/abi.md, *Prompt*, *Answer*), and the lock:
its sidecar (`lock/`), the wire it locks over (docs/abi.md, *Lock*) and the
host's lock surfaces. Neither sink is in the canvas's default set yet;
canvas/README.md says how to add each and why the default waits. The
greeter and what follows it are design. The bar's complement: where the bar is a strip along
one edge, the canvas is everything else a session paints that is not a
window. docs/architecture.md is the bar; this document borrows its model
and says where the canvas departs from it. docs/roadmap.md, *The canvas*,
is the entry that points here.

## One paragraph

Five things in a session paint an image across every output and repaint
when something changes, and three of them also take a password: the
wallpaper, the idle fade, the lock screen, the polkit prompt and the login
greeter. Today each is a separate program with its own Wayland plumbing and
its own drawing (swaybg, chayang, swaylock, a desktop's polkit agent, a
terminal greeter). The canvas is one caustic for all five: a Slang program
that draws the wallpaper, the cross-fade between two wallpapers, the dim, a
prompt panel and a session list, hot-reloaded while it runs, hosted by one
resident process of the bar process's shape. The jobs are sidecars on top:
which image, which dim, whether to lock, what to ask, and where a typed
secret goes. Two things fall out for free: the lock screen shows the
wallpaper the desktop had, dimmed, instead of a flat colour; and the idle
fade is the canvas dimming, so the lock continues from where the fade left
off, because both are the same process's state.

## Why a caustic

The bar's reasons hold here with more force. A wallpaper cross-fade, a dim,
a blur behind a prompt and a scaled image on a surface with real alpha are
all one texture sample and one blend per pixel; text for the prompt and the
session list is punchcutter's, shaped and cut on the device like the bar's.
Hot reload gives the same loop: save the module, the running wallpaper
changes its look.

The objection to drawing a lock screen on the GPU is traditional rather
than observed. The compositor itself renders every frame on the GPU,
including the locked frame before sleep and the first frame after resume;
the bar, a Vulkan client, survives suspend cycles without a restart; and
where there is no device, lavapipe draws through the same code path, as the
VM target already shows for the bar. There is no second, software renderer.

What does hold is narrower, and it is a rule rather than a renderer: **the
secret never enters the caustic.** A typed password lives in one CPU buffer
in the host, zeroed when done, and is handed down the pipe of the one
sidecar that asked for it, which zeroes its copy after use. The caustic
receives a count of typed characters and draws dots. No string the host was
asked to hide (`PAM_PROMPT_ECHO_OFF`) is shaped, uploaded or logged. Echoed
input (`PAM_PROMPT_ECHO_ON`: a user name, a code PAM chose to show) is not
a secret by PAM's own definition and is shaped like any label.

## The model

### One host, sink sidecars

The bar process stays small because a line of code belongs in it only if
it owns a Wayland object or a file descriptor the process holds, or sits on
a configure round trip (docs/architecture.md, *The bar process*). The
canvas keeps that rule and the sidecar rule beside it (*Sidecars*): I/O
that is not the host's own is a child process over a pipe, writing wire
frames the host copies and the caustic lays out.

- **`caustics-canvas`**, the host, one per session, resident. It owns every
  full-output surface (background, overlay, the prompt's top layer, the
  lock surfaces), the device, the caustic and its hot reload, the keyboard
  while a prompt is up, and the one buffer a typed secret passes through. It
  decides nothing: a sidecar asks for a picture, a dim, a lock or a prompt,
  and the host's answers are frames too (`locked`, `input during a dim`,
  the typed bytes down the asking sidecar's stdin).
- **`caustics-wallpaper`**: the control socket (`show`, `dim`, `undim`,
  `status`), image decoding (PNG, JPEG and JPEG XL), the schedule's
  cross-fades.
- **`caustics-lock`**: logind (the `Lock` signal, `LockedHint`), the PAM
  conversation and the fingerprint reader.
- **`caustics-polkit`**: the authentication agent's registration and the
  polkit helper.
- **`caustics-greet`**: greetd's protocol, the session list, the last choice.

The greeter is the same host started by the display manager with only the
greet sidecar in its roster.

One process rather than a daemon and a separate locker, because four things
the two-process design had to arrange arrive on their own:

- The lock continues from the fade: same process, same dim state, no
  status protocol between them.
- No cold start before sleep: the device and the pipelines are warm when
  the lock request arrives, so the before-sleep hold's assumption of a fast
  lock holds without a resident locker of its own.
- A crash after `locked` is covered twice: the compositor keeps the session
  locked when its lock client disconnects, and the host set logind's
  `LockedHint` when it locked and reads it back on start, so a restarted
  host re-locks before it paints anything. The unit is plain
  `Restart=always`.
- One surface-per-output implementation, one prompt panel, one dim.

The one thing traded away is a secret that lives only in a short-lived
process. Here it lives in a long-lived one for the length of a prompt,
which is what every polkit agent and swaylock already do; the zeroed buffer
and the rule above are the protection, not the process lifetime.

The departure from the bar is in the roster: the host's sidecars are not
data sources that publish rows but *jobs* that ask the host for a surface
and an answer, so the wire gains request frames in both directions
(docs/abi.md, *Prompt*, *Answer*).

### Surfaces

Every surface is one per output, created when an output appears and
destroyed when it goes, with `wp_fractional_scale_v1` and `wp_viewporter`
from the first commit. Both are where swaylock's size comes from and
neither can be added later.

- **Background** (`zwlr_layer_shell_v1`, anchored to all edges, exclusive
  zone −1, no keyboard): the wallpaper. A compositor may pull this layer
  into its overview backdrop.
- **Overlay** (`zwlr_layer_shell_v1` on the overlay layer, anchored to all
  edges, exclusive zone −1, exclusive keyboard): the idle fade, over every
  window and panel. It exists only while a dim is up: made on every output
  when the dim arrives and on an output that appears mid-dim, gone on an
  undim, on input, or when the sidecar that asked for the dim goes. It
  takes the pointer and the keyboard, as chayang's does, so that any input
  cancels the fade: the pointer moving (arriving under a pointer at rest is
  not moving, and the cursor is hidden there), a button, a scroll, a touch,
  any key. Only the fact of a key is read; there is no keymap. What it
  draws is the wallpaper again, the background's own picture at the same
  fit and cross-fade, at the dim's alpha: the windows dissolve and the
  desktop's picture is what remains, darkened towards the dim colour by
  `DIM_DARKEN` in `canvas_tunables.slang` (0 leaves it as it is). The
  dim's alpha is how far the windows have gone, not the darkening.
- **Top** (`zwlr_layer_shell_v1` on the top layer, anchored to all edges,
  exclusive zone −1, exclusive keyboard, namespace
  `caustics-canvas-prompt`): the prompt. It exists only while a prompt is
  up: made on every output at a conversation's first ask and on an output
  that appears mid-prompt, gone when the conversation ends or is
  dismissed. It is opaque: the wallpaper from the slots, darkened towards
  the dim colour by `PROMPT_DARKEN` (a little more than `DIM_DARKEN`), and
  the panel on it. While it is up, an overlay that is also up draws nothing,
  so the panel is what shows, and comes back as it was when the prompt
  goes. No decorations and no buttons: the keys are the whole interaction.
- **`ext-session-lock-v1`**: `lock`, then at once one lock surface per
  output, so each can be configured and committed before the compositor
  sends `locked` (a compositor waits a while for them, then locks with the
  outputs blank); outputs that appear while locked get a surface too. A lock
  surface is not a layer surface: it has no layer, namespace or keyboard
  mode, is configured as it is made, and is never committed without a
  buffer, so its first commit is its first frame. It is opaque and draws
  what the top pane draws: the wallpaper darkened by `PROMPT_DARKEN`, and
  the panel while a conversation is up; its own target (`TARGET_LOCK`), so
  that the wake (*The wake*) is drawn under the panel there and not on the
  top pane. `locked` is told to the lock sidecar;
  `unlock_and_destroy` comes only on that sidecar's say-so, the lock
  surfaces go with it, and `unlocked` is told once a `wl_display.sync` says
  the compositor has read it. `finished` (another locker holds the lock,
  locking was refused, or the compositor ended it) is never pretended
  away: the host says so in one line and exits 1, and the unit restarts it;
  the sidecar learns of it by its pipe closing.

The prompt panel is drawn on whichever per-output surface is up at the
time: the top layer for polkit, the lock surface when locked. While the
session is locked no top pane is made: a conversation's raise and lower
redraw the lock surfaces, and the compositor gives them the keyboard. A dim
that is up when the lock is asked for stays up until the compositor says
`locked`, then is ended (its overlays go, unseen, since a locked compositor
shows no layer surface; its sidecar is told nothing, as with an undim it
sent itself): ended at the request, the overlays would leave the windows on
screen for the frame before the lock surfaces' first. The lock surface
then darkens the wallpaper from where the dim had it (the host writes the
lock's moment into `facts`, the caustic keeps the dim's depth from its last
frame) to the prompt's darkening over `LOCK_DARKEN_MS`, so the fade's last
frame runs into the lock's first with no step. The prompt
core is reserved for the lock's sidecar: another sink's conversation up at
the lock is dismissed, and another sink's ask while locked is cancelled at
once, so the lock screen's panel is only ever the lock's. So there is one
dim and one owner of it.

The lock is the deepest dim. A dim asked for while the session is locked
(after the compositor's `locked`, before the unlock is asked) is reached at
once: its sidecar hears `dimmed` straight away, and no overlay is made and
nothing drawn. The idle daemon's timer keeps running under the lock, since a
key at the lock's prompt restarts it, and its `dim --wait && lock` then
finds the dark screen it asked for and a session already locked. A dim made
there would be unseen, and the prompt's keys cancel no dim, so it would
outlive the lock and the unlock would land on a dimmed desktop. A dim still
up when the unlock is asked for (one that got in while locking) ends with
it, before the lock surfaces go, and as at `locked` its sidecar is told
nothing: it heard `dimmed` long ago, and a waiting client has had its answer.

### The wake

The bar's wake (`caustics/wake.slang`), over the wallpaper: the pointer's
motion drops expanding wave trains on a shallow pool whose floor is the
picture, a press drops a taller one, and the surface bends the picture
seen through it, each colour by its own index, and lights it with the
caustic it casts, darker where the waves spread the light and a warm net
where they focus it. The physics is one module both caustics import,
`caustics/water.slang`; the canvas's half is `canvas_wake.slang`, and its
knobs are the `WAKE_*` block of `canvas_tunables.slang` (`WAKE_ALPHA` 0
turns it off, and then a motion costs no frame).

It is drawn on the **background**, wherever the compositor gives the
background layer the pointer, and on the **lock** surfaces, where the
pointer is the lock's for the whole time and the net reads as light on the
darkened picture, under the panel. Not on the overlay, where motion is a
cancel, nor on the top pane, which is a panel. The host writes the pointer's
pane, its place in that pane's buffer px and a count of its presses into
`facts`, and draws the pane again on that news only when the caustic said
it wants the pointer there; what a motion does to the water is the
caustic's. Each source is stamped with `CLOCK_BOOTTIME` and its pane, so it
ages by the clock like a fade and shows on its own pane only. A resting
pointer drops nothing, the last source dies `WAKE_SETTLE_S` after the last
motion, and then the pane's `wants_redraw` is 0 and it asks for no frames.

**niri gives the background layer the pointer** where no window, panel or
popup is over it: on an empty workspace and in the gaps between windows,
not in the overview, where its background and bottom layers take no input
(measured with `WAYLAND_DEBUG=1` in a nested niri 26.04: enter and motion
events on the background's `wl_surface` once it has a buffer). A surface
with no buffer yet is not mapped and gets nothing. While the session is
locked, every pointer event goes to the lock surface.

### Time

A cross-fade interpolates between the *source* images at alpha(t), never a
compound of the current frame, so a two-hour fade is at its midpoint at the
midpoint (a per-frame compound blend arrives early and stalls on small
steps). The sidecar's command carries the fade's start and duration in
`CLOCK_BOOTTIME`, the host writes the current boot time into the buffer
once per frame, and the caustic computes alpha; a fade that sleeps through
a suspend lands where the clock says. The host draws nothing when nothing
changes.

**Pacing.** Each output is paced on its own `wl_surface.frame`, made before
the present, as the bar's surface is. niri answers a background surface it
shows at the output's refresh, including one only a maximised window's gaps
leave visible; one that a fullscreen window covers entirely is answered only
by niri's one-second fallback for surfaces it does not repaint (measured with
`WAYLAND_DEBUG=1` under both, a 4 s fade: about 250 callbacks under the
maximised window, 6 under the fullscreen one). So the default is the
callbacks, with the host's 500 ms stall (`caustics_host::pacing`) as a timer
beside them: a covered fade still advances, at about two frames a second,
which is all a picture nobody sees needs, and when the fade is done the
caustic stops asking and the host stops drawing.

### The prompt core

`canvas/src/prompt.rs` is the state machine, and `canvas/src/keys.rs` the
keys and the buffer:

```
Held ─ask→ Asking ─Return→ Verifying ─error line→ Failed ─ask→ Asking …
  done or cancel from the sink: the panes go, the conversation ends
  Escape while the panes are up: ANSWER_CANCELLED, the panes go
```

- **A conversation** is a sink's `Prompt` frames under one id (docs/abi.md,
  *Prompt*). Its first frame, the title usually, raises the pane on every
  output and takes the keyboard; until the first ask the conversation is
  **held**: the panel shows the title and any line, and no field. A PAM
  stack that tries a fingerprint reader before it asks for a password asks
  only after that module's timeout, and a user who sees nothing for those
  seconds thinks nothing happened; held, they see what is wanted and can
  touch the reader or wait. The cost is a panel for a helper that fails
  before it asks, which is rare and brief. The first `PROMPT_SECRET` or
  `PROMPT_ECHO` is **Asking**: the field appears, and keys type into it.
- **Asking**: keys type into the host's buffer. Return writes
  `Answer::Text` down the asking sidecar's stdin, one write, and the buffer
  is zeroed as the answer leaves it; **Verifying** follows, with the dots
  frozen and every key but Escape ignored. An error line then is a failed
  attempt, **Failed**, counted (`attempts`) and never described; the next
  ask is asking again, with the error line still shown. `PROMPT_DONE` or
  `PROMPT_CANCEL` ends the conversation and the panes go.
- **Escape**, any time the panes are up, writes `ANSWER_CANCELLED` and the
  panes go; the conversation waits for the sink's end, and an ask under it
  meanwhile is dismissed again.
- **One conversation at a time per host.** A `Prompt` under another id, or
  from another sink, while one is held or up is answered `ANSWER_CANCELLED`
  at once and changes nothing on screen (the sinks queue their own; this is
  the backstop). An end for a conversation that is not this one is ignored.
- **The sink's pipe closing** ends its conversation and takes the panes
  down (*Failure model*).
- **The keys are the host's**: the seat's keymap compiled by libxkbcommon,
  opened at run time, with compose from the locale and no repeat (a held
  key is one key). Return or keypad Enter submits, Escape dismisses,
  BackSpace takes one character (not one byte), Control-U clears; text is
  whatever the keymap and compose produce, up to `MAX_ANSWER_LEN` bytes.
  Without libxkbcommon the host reads Return, Escape and BackSpace by their
  codes and types nothing, so a prompt can still be dismissed.
- **Input priority.** While the panes are up, keys go to the prompt and the
  pointer and touch cancel nothing: a dim that is also up stays as it was,
  and is as it was when the prompt closes.
- **What the caustic sees** (docs/abi.md, *The canvas's buffer*): the
  conversation's state, whether the ask echoes, the attempts, and ranges
  into `prompt_text` for the title, the ask's words, the last line (and
  whether it is an error), and for an echo-on ask what was typed. For an
  echo-off ask it sees a count of characters and draws that many dots,
  each one shaped glyph; the bytes never enter a GPU buffer, a log line, a
  witness or a `Debug`. The caustic's own words are literals: the dot, and
  "Password" for an ask that has no words of its own.
- **What is drawn**: the panel, centred, `PROMPT_WIDTH` wide: the title
  (wrapped once at a space if it is too long, then cut at the edge), the
  ask's words, the answer's field with the dots or the echoed text, and
  the last line under it, red when it is an error. Sizes and colours are
  `canvas_tunables.slang`'s.

Timeouts and attempt limits belong to the sink; PAM already enforces them.

### Sinks

- **`caustics-polkit`**: spawn `/usr/lib/polkit-1/polkit-agent-helper-1
  <user>`, write the cookie, answer the helper's PAM conversation lines
  (`PAM_PROMPT_ECHO_OFF`, `PAM_PROMPT_ECHO_ON`, `PAM_TEXT_INFO`,
  `PAM_ERROR_MSG`) through the host's prompt. The helper reports to polkitd
  itself (`AuthenticationAgentResponse2`); the agent learns only success or
  failure from its exit. Verify the line protocol against libpolkit-agent's
  `polkitagentsession.c` before writing a byte; soteria is a short Rust
  reimplementation to compare against.
- **`caustics-lock`**: two ways in, at once. One is `pam_start` with the
  distribution's lock service and a conversation routed through the
  prompt. The other is the fingerprint reader, spoken to on fprintd's
  D-Bus interface by the sidecar itself, not through PAM: PAM is
  serialised, and pam_fprintd's manual says under its limitations that one
  stack cannot offer a password and a fingerprint together. So while the
  session is locked and someone is at it, the sidecar claims the default
  reader and verifies beside the conversation, in **windows**: one opens as
  the lock goes up, when input reaches a lock surface (the host tells the
  sidecar, at most once in five seconds) and on a resume from suspend, and
  closes after half a minute without one, or a minute and a half at most;
  the panel's line then says a key wakes the reader, and the next trigger
  opens a window at once. The reason is libfprint's temperature
  model: a sensor that is not always-on is taken to heat while it runs and
  to cool while idle, and once hot it is disabled for minutes, so a
  verification kept going for a whole lock leaves the reader dead for most
  of it. GNOME likewise reads a finger only while its unlock dialog is up.
  A match ends the conversation on screen as a success and unlocks, and the
  password works throughout. GNOME's way is two PAM stacks, each in a
  worker process of its own, with the display manager choosing between
  them; one D-Bus client beside one PAM stack is less, and the reader has
  one owner. The reader is claimed as a window opens and released (the
  verify stopped, the device let go) when it closes and when the lock is
  over, so a polkit helper's `pam_fprintd` finds it free after the unlock. A lock
  service that keeps the fingerprint module off the unlock path avoids its
  timeout; that, and the reader itself, are the sidecar's own settings
  (environment variables on its unit), never the host's.
- **`caustics-greet`**: length-prefixed JSON on `$GREETD_SOCK`:
  `create_session{username}` → `auth_message{type, text}` round trips via
  `post_auth_message_response{response}` → `start_session{cmd, env}`. greetd
  runs PAM; the greeter relays. The session list comes from
  `/usr/share/wayland-sessions`; the last choice per user is remembered the
  way tuigreet does it.

### How each job is wired

- **wallpaper**: the session's wallpaper unit runs the host. Whatever
  schedules the wallpaper (a script that follows the schedule the
  distribution ships) sends `show <image> fade <seconds>` to
  `caustics-wallpaper`'s socket, with the duration the schedule names:
  seconds for a dissolve, hours for a real slow fade. `caustics-canvasctl`
  is the client for scripts.
- **fade**: the idle timer's warning stage runs

  ```sh
  caustics-canvasctl dim 1 over 10 --wait && systemctl suspend
  ```

  and, on resume, `caustics-canvasctl undim`. The client keeps chayang's exit
  contract: **0** when the dim reached its depth, **2** when input cancelled
  it (the user came back, and the stage should not suspend or lock), **1**
  when it could not run or the canvas went away mid-dim, so a broken fade
  still fails toward a locked screen. A newer dim, or an undim, ends a waited
  one with 2 as well. While the session is locked the dim is reached at
  once, so the client exits 0 straight away (*Surfaces*). A completed dim
  stays at its depth until undim or
  input, which is what a suspend hook wants: the screen stays dark while the
  machine goes down. Underneath, the wallpaper sidecar sends a `CANVAS_DIM`
  under an id of its own, and the host answers on the sidecar's stdin with a
  `CanvasEvent` for that id: dimmed once, or cancelled by input, after which
  the sidecar undims (docs/abi.md, *Canvas event*). The background never
  darkens: one dim, on the overlay, with one owner.
- **polkit**: call `Authority.RegisterAuthenticationAgent(unix-session
  subject, locale, path)` on the system bus, and serve
  `org.freedesktop.PolicyKit1.AuthenticationAgent` at
  `/org/freedesktop/PolicyKit1/AuthenticationAgent` on that same connection:
  it is the one polkitd calls the agent back on. Handle
  `BeginAuthentication(action_id, message, icon_name, details, cookie,
  identities)` and `CancelAuthentication(cookie)`.
  One prompt at a time; the rest queue. A request that arrives while the
  session is locked waits until it is not. No identity picker at first:
  authenticate as the first identity matching the session user, else the
  first offered.
- **lock**: `loginctl lock-session` (or the idle timer, or lid close) raises
  logind's `Lock` signal; the lock sidecar asks the host to lock and sets
  `SetLockedHint(true)` when the host reports `locked`, clearing it on
  unlock. On start the sidecar reads `LockedHint` and, if it is set,
  requests the lock before the host has painted a frame: the host applies
  its sidecars' frames from the moment its globals are bound, before its
  device is up, so the lock goes out at once and the lock surfaces get
  their swapchains when the device comes, like any pane. Bringing the device
  up builds no pipeline (docs/architecture.md, *The caustic*), so nothing
  holds the loop after that either: the lock panes are drawn when
  `canvas_apply` and `canvas_draw` are built, and their text when the
  shaper is. The compositor keeps the session locked and blank in between. Only the sidecar that
  locked may unlock; a lock asked for while locked changes nothing but is
  answered `locked` again, which is what a restarted sidecar waits for.
  `caustics-canvasctl lock --wait` returns when the host has said `locked`, the moment the hint
  is set, which is the before-sleep hold. Nothing unlocks but a sink success: not a signal, not a reload, not
  a compositor event.
- **greet**: the display manager's `default_session.command` runs the host
  as the greeter user inside the compositor it starts, with
  `$CAUSTICS_SIDECARS` naming only the greet sidecar. Needs nothing from a
  user session and no file a user owns.

### The bar's view

`caustics-wallpaper`'s `status` (locked, fading, prompt pending) can also be
a bar sidecar frame later. The bar shows state and never more; the one
process holding a secret is the canvas host, and the bar is not it.

## Constraints that survive

- **The greeter's device.** The greeter user needs render-node access for
  the device, or runs on lavapipe. Decide, don't discover.
- **The hybrid laptop.** Pin the integrated device the way the bar's
  iGPU drop-in does (`bar/systemd/dropins/igpu-only.conf`); the discrete
  device's post-resume behaviour is the one Vulkan story with teeth.
- **Restart policy differs from the bar's.** The bar's unit does not
  restart on a lost device (status 3), because a new bar would only lose it
  again; the canvas host's unit restarts on everything, because a locked
  session must get its lock client back, and a lost device after resume is
  exactly when it is locked.
- **A reload's compile** runs on the compile workers beside the canvas that
  draws (docs/architecture.md, *The caustic*), so a pipeline new to the
  driver costs the new caustic its seconds, never the old one a frame and
  never a lock.

## Failure model

| what dies | when | correct outcome |
|---|---|---|
| the host | unlocked | the compositor's backdrop shows; the unit restarts it; it paints the dim colour until the wallpaper sidecar re-sends the picture |
| the host | locked | the compositor stays locked (niri keeps the session locked with its lock client dead); the unit restarts the host; the lock sidecar reads `LockedHint` and re-locks before the first frame, before any pipeline is built (they build off the loop, so a cold driver cache delays the lock panes' first frame and not the lock), and niri gives the new lock `locked` at once in place of the dead one (the nested gate drives the same restart through the socket) |
| the host | told `finished` | one line and exit 1; the unit restarts it; the lock sidecar's pipe closes, so a waiting `lock --wait` fails, and the restarted sidecar locks again if logind's hint says so |
| `caustics-wallpaper` | any time | the last picture stays; the host restarts it with backoff; the restarted sidecar re-shows the path it kept, and takes the socket back once the old listener has gone (a listener still answering a probe is given a short window to die before it is read as another wallpaper's) |
| `caustics-wallpaper` | mid-dim | the overlay goes with it (a dim without its owner is undone, so a crashed sidecar never leaves a dark screen); the picture stays; the host restarts it, and it comes back with no dim |
| a fade | mid-fade | the waiting client gets EOF on the socket and exits 1; the idle stage locks anyway |
| `caustics-lock` | locked | the host stays locked (it never unlocks on its own); the sidecar restarts, reads `LockedHint`, resumes the conversation |
| the fingerprint reader | locked | the password keeps working; the sidecar releases what it held and claims the reader again after a pause that doubles from 1 s to 30 s while its window stays open (a resume from suspend is the usual cause; fprintd reports a sensor disabled for heat the same way, so the pause starts again from 1 s only after a verify that was live for a minute); fprintd itself dying releases its claim, and the next claim starts it again |
| `caustics-polkit` | mid-prompt | the host takes the prompt down with its pipe; polkitd times out the request; the caller sees "not authorized"; the sidecar restarts and re-registers |
| the host | greeting | the display manager restarts it; nothing was logged in |

The nested gate (docs/testing.md, *The nested gate*) covers the wallpaper,
the fade, the polkit prompt and the lock surfaces with a dry-run sink; the
VM target (*The VM target*) gains steps for the real PAM conversation, a
suspend cycle and a relogin.

## Order

0. **The host crate.** The bar process's mechanism (device, caustic
   loading and hot reload, the sidecar supervisor, frame pacing) lifted
   into a library both the bar and the canvas are clients of. No behaviour
   change to the bar; its SPIR-V byte-identical before and after.
1. **Host + wallpaper.** The safest first target: a bug is a wrong picture.
   Replaces swaybg behind the session's wallpaper unit and gives the
   schedule its cross-fade.
2. **Fade.** `dim`/`undim` on the overlay, cancelled by input, replacing
   chayang.
3. **Prompt core + polkit.** Built. The first job that takes a password; a
   failure denies one action, and the previous agent is one unit edit away.
   The nested gate types into it with the harness's keyboard
   (docs/testing.md).
4. **Lock.** Built: lock surfaces, the lock sidecar, logind's hint. Keep
   the previous locker installed until the new one has locked and unlocked
   through a suspend.
5. **Greeter.** The greet sidecar, the session chooser, last-session
   memory.
6. **Later.** A NetworkManager secret-agent sidecar: joining a new Wi-Fi
   network is the one thing left in the session that needs a settings
   program. The bar lists networks, NetworkManager asks the agent for the
   passphrase, the bar never holds it.

## Open questions

- ~~Does the compositor send frame callbacks to a fully covered background
  layer?~~ Answered for niri (*Time*, *Pacing*): at the refresh rate while
  any of it shows, from a one-second fallback while a fullscreen window
  covers it; the host paces on callbacks with its stall timer beside them.
- Does anything on a typical machine ask for root-only polkit
  authentication (`pkaction --verbose`)? Decides whether the identity
  picker is needed before the first release.
- Which compositor hosts the greeter: cage, or the session's own compositor
  with a greeter configuration.
- Whether the canvas and the bar can share paint modules: today every bar
  module below the subjects imports the bar's bindings, so the canvas
  carries its own small paint and text modules (`caustics/README.md`); the
  text engine's modules are the one thing both import.

## References

- swaybg (`main.c`) for the background layer and image loading; chayang
  (`main.c`) for the overlay fade; swaylock (`main.c`, `seat.c`,
  `render.c`) for session-lock and multi-output handling.
- awww (`daemon/src/animations/transitions.rs`), the cautionary example of
  a compounding per-frame blend.
- polkit: `docs/polkit/` in the polkit tree, `src/polkitagent/
  polkitagentsession.c` and `polkitagenthelper-pam.c`; soteria.
- `ext-session-lock-v1.xml`, `wlr-layer-shell-unstable-v1.xml`,
  `viewporter.xml`, `fractional-scale-v1.xml`; `greetd-ipc(7)`; tuigreet.
