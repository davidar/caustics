# The canvas

Steps 0 and 1 of *Order* are built: the host crate, and the canvas host
with its wallpaper sidecar (`canvas/`, `wallpaper/`, `caustics/canvas*.slang`;
canvas/README.md runs it). The rest is design. The bar's complement: where the bar is a strip along
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
  `status`), image decoding, the schedule's cross-fades.
- **`caustics-lock`**: logind (the `Lock` signal, `LockedHint`) and the PAM
  conversation.
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
(docs/abi.md, *The wire*, once built).

### Surfaces

Every surface is one per output, created when an output appears and
destroyed when it goes, with `wp_fractional_scale_v1` and `wp_viewporter`
from the first commit. Both are where swaylock's size comes from and
neither can be added later.

- **Background** (`zwlr_layer_shell_v1`, anchored to all edges, exclusive
  zone −1, no keyboard): the wallpaper. A compositor may pull this layer
  into its overview backdrop.
- **Overlay**: the idle fade. During a dim the overlay takes pointer and
  keyboard, as chayang's does, so that any input cancels the fade.
- **Top**, with exclusive keyboard interactivity: the polkit prompt's
  panel, over the dimmed wallpaper the host already holds.
- **`ext-session-lock-v1`**: `lock`, then one lock surface per output, each
  configured and committed before the compositor sends `locked`; outputs
  that appear while locked get a surface too; `unlock_and_destroy` only on
  the lock sidecar's say-so. `finished` (another locker holds the lock, or
  locking was refused) is reported to the sidecar and never pretended
  away.

The prompt panel is drawn on whichever per-output surface is up at the
time: the top layer for polkit, the lock surface when locked. So there is
one dim and one owner of it.

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

`Idle → Prompting(message, echo) → Verifying → (Done | Failed(attempts) →
Prompting)`. The keys are the host's: keymap from the seat through
xkbcommon, compose support, backspace, ctrl-u, escape. The buffer is the
host's; the caustic sees the count and the message. A prompt is a request
frame from a sidecar (an id, the message, whether to echo) and the answer
is the typed bytes written to that sidecar's stdin under the same id, then
zeroed. Timeouts and attempt limits belong to the sink; PAM already
enforces them. Failed attempts are counted, never described, in any log.

### Sinks

- **`caustics-polkit`**: spawn `/usr/lib/polkit-1/polkit-agent-helper-1
  <user>`, write the cookie, answer the helper's PAM conversation lines
  (`PAM_PROMPT_ECHO_OFF`, `PAM_PROMPT_ECHO_ON`, `PAM_TEXT_INFO`,
  `PAM_ERROR_MSG`) through the host's prompt. The helper reports to polkitd
  itself (`AuthenticationAgentResponse2`); the agent learns only success or
  failure from its exit. Verify the line protocol against libpolkit-agent's
  `polkitagentsession.c` before writing a byte; soteria is a short Rust
  reimplementation to compare against.
- **`caustics-lock`**: `pam_start` with the distribution's lock service and
  a conversation routed through the prompt. A lock service that keeps the
  fingerprint module off the unlock path avoids its timeout; whether to use
  one is the sidecar's own setting (an environment variable on its unit),
  never the host's.
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
- **fade**: the idle timer's warning stage runs `caustics-canvasctl dim
  --over <warn> --wait` and, on resume, `undim`. The client keeps chayang's
  exit contract (0 completed, 2 cancelled by input, 1 could not run), so a
  broken fade still fails toward a locked screen.
- **polkit**: register `org.freedesktop.PolicyKit1.AuthenticationAgent` on
  the session bus at `/org/freedesktop/PolicyKit1/AuthenticationAgent`, call
  `Authority.RegisterAuthenticationAgent(unix-session subject, locale, path)`
  on the system bus, handle `BeginAuthentication(action_id, message,
  icon_name, details, cookie, identities)` and `CancelAuthentication(cookie)`.
  One prompt at a time; the rest queue. A request that arrives while the
  session is locked waits until it is not. No identity picker at first:
  authenticate as the first identity matching the session user, else the
  first offered.
- **lock**: `loginctl lock-session` (or the idle timer, or lid close) raises
  logind's `Lock` signal; the lock sidecar asks the host to lock and sets
  `SetLockedHint(true)` when the host reports `locked`, clearing it on
  unlock. On start the sidecar reads `LockedHint` and, if it is set,
  requests the lock before the host has painted a frame. `caustics-canvasctl
  lock --wait` returns when `LockedHint` is true, which is the before-sleep
  hold. Nothing unlocks but a sink success: not a signal, not a reload, not
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
- **The hot-reload stall** (docs/roadmap.md, *Wire and harness*) is a
  pipeline compile new to the driver. It costs the canvas a frame or two of
  stale wallpaper, never a missed lock, since the host is resident.

## Failure model

| what dies | when | correct outcome |
|---|---|---|
| the host | unlocked | the compositor's backdrop shows; the unit restarts it; it paints the dim colour until the wallpaper sidecar re-sends the picture |
| the host | locked | the compositor stays locked and blank; the unit restarts the host; the lock sidecar reads `LockedHint` and re-locks before the first frame |
| `caustics-wallpaper` | any time | the last picture stays; the host restarts it with backoff; the scheduler's next `show` lands |
| a fade | mid-fade | the waiting client exits 1; the idle stage locks anyway |
| `caustics-lock` | locked | the host stays locked (it never unlocks on its own); the sidecar restarts, reads `LockedHint`, resumes the conversation |
| `caustics-polkit` | mid-prompt | polkitd times out the request; the caller sees "not authorized"; the sidecar restarts and re-registers |
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
3. **Prompt core + polkit.** The first job that takes a password; a failure
   denies one action, and the previous agent is one unit edit away. Its
   test prerequisite is a harness tool that can type (docs/testing.md).
4. **Lock.** Lock surfaces, the lock sidecar, logind's hint. Keep the
   previous locker installed until the new one has locked and unlocked
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
  module below the subjects imports the bar's bindings, so the first canvas
  carries its own small paint module (`caustics/README.md`).

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
