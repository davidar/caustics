# Running caustics-canvas as a user unit

caustics-canvas is the canvas host (docs/canvas.md): the wallpaper on every
output, drawn by a caustic (`caustics/canvas.slang`) and cross-faded on
command, the idle fade over every window, the prompt a sink sidecar
asks for (a polkit authentication), and the lock screen. It is a layer-shell client on the
**background** layer, on the **overlay** layer while a dim is up and on the
**top** layer while a prompt is, and an `ext_session_lock_v1` client while
the session is locked; it replaces swaybg and chayang, and can replace
swaylock. In a session it runs as a **systemd user unit**,
`caustics-canvas.service`, which restarts it whatever it exits with and sends
its log to the journal.

## Install

The canvas is part of the bar's set: `bar/install.sh` builds and installs
it with everything else (bar/systemd/README.md) and links
`~/.local/bin/caustics-canvas` and `~/.local/bin/caustics-canvasctl` into the
set. Every install after the first settles this unit as it settles the
bar's: the unit restarts when the canvas's own binary changed, and
otherwise only the sidecars whose binaries changed are ended, which the
canvas respawns on the new code (a lock sidecar ended while the session is
locked comes back locked). The first time, link the unit and enable it:

```sh
systemctl --user link "$PWD/canvas/systemd/caustics-canvas.service"
systemctl --user enable --now caustics-canvas
journalctl --user -u caustics-canvas -f
```

Stop whatever drew the wallpaper before (`swaybg`'s unit, or the line in the
compositor's startup that runs it): two background clients on one output
draw over each other.

## The unit

- `Restart=always`, `RestartSec=1`, at most ten starts a minute, and **no
  `RestartPreventExitStatus`**: unlike the bar's, this unit restarts on a
  refusal and on a lost device too, because the canvas is where the lock
  screen is drawn and a session must always get it back.
- Exit codes are the bar's: 1 a protocol error or a failed frame, 2 a
  refusal (an argument, a discrete GPU), 3 the device gone.
- `RUST_LOG=info`; the journal gets stdout and stderr, the sidecars' lines
  included.
- `MemoryMax=1G`, the bar's backstop for the bar's reason (bar/systemd/README.md):
  a hot reload's compile runs inside the unit and peaks near 400 MB; the
  canvas's own pictures are tens of megabytes.
- **The iGPU-only drop-in applies the same way.** On a laptop with a discrete
  GPU, copy `bar/systemd/dropins/igpu-only.conf` into
  `~/.config/systemd/user/caustics-canvas.service.d/` and
  `systemctl --user daemon-reload`: it pins `VK_DRIVER_FILES` at the
  integrated GPU's driver, so the discrete GPU's is never loaded.

## The socket and the client

`caustics-wallpaper`, the canvas's sidecar, listens on
`$XDG_RUNTIME_DIR/caustics-canvas.sock`. `caustics-canvasctl` speaks it:

```sh
caustics-canvasctl show ~/Pictures/sea.jpg              # a cut
caustics-canvasctl show ~/Pictures/dusk.png fade 4      # a four-second dissolve
caustics-canvasctl show ~/Pictures/night.jxl fade 7200  # a two-hour fade
caustics-canvasctl status   # fade=fading progress=0.37 duration=4.000 image=…
caustics-canvasctl status   # dim=none fade=done image=…
caustics-canvasctl dim 0.6 over 10
caustics-canvasctl dim 1 over 10 --wait && systemctl suspend
caustics-canvasctl undim
```

A picture is a PNG, a JPEG or a JPEG XL, the formats a distribution ships
its wallpapers in; the sidecar knows each by its first bytes, whatever the
file is called; one it cannot decode is an `error` reply with the decoder's
reason.

A dim is drawn on an overlay over every window, the bar included: the
wallpaper itself, at the dim's alpha, so the windows dissolve and the
desktop's picture is what remains. `DIM_DARKEN` in the tunables is how much
darker than the background that picture is at full dim (0.3 shipped);
**0 is the windows-dissolve look**, the wallpaper exactly as it is. The
`dim` alpha is how far the windows have gone, not the darkening. Any
input ends a dim: the pointer moving, a button, a scroll, a touch, a key. It
goes on an `undim` too, and with the sidecar if that dies. A dim that has
reached its depth stays there until one of those. `status` begins with
`dim=none`, `dim=dimming progress=0.42` or `dim=dimmed`.

`caustics-canvasctl` exits 0 when the canvas answered `ok` and 1 otherwise.
`dim … --wait` is chayang's contract, for an idle daemon's warning stage:

| exit | the dim |
|---|---|
| 0 | reached its depth |
| 2 | was cancelled by input (the user is back), or ended first by an `undim` or a newer `dim` |
| 1 | could not be asked for, or the canvas went away before it was over |

While the session is locked, a dim is reached at once: the lock is darker
than any dim, so `dim … --wait` is `done` straight away, exits 0, and
nothing is drawn, and the unlock never lands on a dimmed desktop.

Before any picture
the canvas is the dim colour; the path last shown is kept in
`$XDG_RUNTIME_DIR/caustics-canvas.last`, so a restarted canvas shows it
again. The look (the dim colour, the fade's curve, cover or contain) is
`caustics/canvas_tunables.slang`: save it and the running canvas changes.

Moving the pointer over the bare desktop, or over the lock screen, stirs a
shallow pool over the picture, the bar's wake: the picture bends under the
waves and the light they focus is drawn on it, and the water is flat and
the canvas draws nothing again `WAKE_SETTLE_S` after the pointer stops
(docs/canvas.md, *The wake*). Its look is the `WAKE_*` block of the
tunables; `WAKE_ALPHA` 0 turns it off.

## The prompt

A sink sidecar asks the canvas for a typed answer over the wire
(docs/abi.md, *Prompt*, *Answer*); the canvas takes the keyboard, draws a
panel over the darkened wallpaper on every output from the conversation's
first frame (the title alone, with no field, while a fingerprint module or
whatever PAM runs first is at work), and writes what was typed down that
sidecar's stdin, then zeroes it (docs/canvas.md, *The prompt core*). Return answers, Escape dismisses, BackSpace takes a character,
Control-U clears the line. One prompt at a time: a second sink asking while
one is up is told no at once. The panel's sizes and inks are
`canvas_tunables.slang`'s (`PROMPT_*`). The keymap is the seat's, compiled by
libxkbcommon, which the canvas opens at run time.

The text is drawn by the text engine, from the canvas's own
`caustics-glyphs` child, which is in the default set.

### Enabling the polkit agent

`caustics-polkit` (polkit/README.md) is the session's polkit agent as a sink
of the canvas. It is installed with the set but is **not** in the canvas's
default sidecars yet: a session's polkit prompts are how it installs
software and changes settings, and a new agent should be taken on by
choice, with the previous one a unit edit away, until it has been used for
a while. To take it on, stop the desktop's own agent (two agents cannot be
registered for one session) and name the canvas's sidecars in a drop-in,
the default set and the sink:

```ini
# ~/.config/systemd/user/caustics-canvas.service.d/polkit.conf
[Service]
Environment=CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload caustics-glyphs caustics-polkit"
```

then `systemctl --user daemon-reload && systemctl --user restart
caustics-canvas`. Removing the file (and starting the previous agent again)
undoes it.

## The lock

`caustics-lock` (lock/README.md) is the session's locker as a sidecar of
the canvas: it locks on logind's `Lock` signal (`loginctl lock-session`, an
idle timer, the lid) or `caustics-canvasctl lock`, and the canvas puts a
lock surface on every output, draws the wallpaper darkened by
`PROMPT_DARKEN` on it, and the PAM conversation's panel when it asks. Only
a conversation PAM says succeeded, or a fingerprint fprintd says matched,
unlocks; Escape starts a fresh one, and the screen is never dismissed. The
fingerprint reader is the sidecar's own: it verifies beside the password
while someone is at the lock, in windows that the lock going up, input on
a lock surface (which the canvas tells the sidecar, at most once in five
seconds) and a resume open; its lines show on the panel under the
conversation's title, and it is released when a window closes and at the
unlock (lock/README.md, *Behaviour*). While locked, the prompt is the lock's
alone: another sink asking (a polkit request) is told no at once. A dim up
when the lock comes is ended.

```sh
caustics-canvasctl lock                 # lock now
caustics-canvasctl lock --wait && systemctl suspend   # the before-sleep hold
caustics-canvasctl status               # a second line, the lock's: locked=yes attempts=0
```

If the compositor ends the lock or will not give it (another locker holds
it), the canvas says so in one line and exits 1, and the unit restarts it.
If the canvas dies while locked, the compositor keeps the session locked,
and the restarted canvas's lock sidecar takes the lock again on start
(logind's `LockedHint`).

### Enabling the lock

Like the polkit agent, `caustics-lock` is installed with the set but is
**not** in the canvas's default sidecars: two lockers on one session fight
for the lock, and a session usually has one already, often hooked to
logind's `Lock` signal by an idle daemon (`swayidle`'s `lock` event, say)
or a unit. To take it on:

1. **Unhook the previous locker from the lock signal** first (the idle
   daemon's `lock` and `before-sleep` lines, or whatever unit runs it), so
   that one `loginctl lock-session` starts one locker. Keep it
   **installed**.
2. Name the canvas's sidecars in a drop-in, the default set and the lock
   (with `caustics-polkit` too if it is taken on, as one list):

   ```ini
   # ~/.config/systemd/user/caustics-canvas.service.d/lock.conf
   [Service]
   Environment=CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload caustics-glyphs caustics-lock"
   ```

   then `systemctl --user daemon-reload && systemctl --user restart
   caustics-canvas`.
3. Point the idle daemon's lock and before-sleep commands at
   `caustics-canvasctl lock` and `caustics-canvasctl lock --wait`, or leave
   them to logind's signal, which the sidecar hears itself.
4. Lock and unlock once by hand, then **through a suspend**, before the
   previous locker is removed (docs/canvas.md, *Order*). Until then it is a
   drop-in removal and a hook away.

The PAM service is the sidecar's own setting, `CAUSTICS_LOCK_PAM_SERVICE`
in the same drop-in, and so is the fingerprint reader,
`CAUSTICS_LOCK_FINGERPRINT=0` to leave it out (lock/README.md,
*Settings*).

#### Trying it first

`bar/lock-try.sh` tries the lock with the real password and the real
fingerprint reader without locking the session. It builds the workspace,
opens a nested niri as a 1280x800 window on the desktop, starts this
checkout's canvas inside it with the set above, shows a picture (the path
given as its argument, else a gradient), and locks the window. Click into
the window, then type the password and press Return, or touch the reader;
Escape starts a fresh attempt. The terminal shows the host's `lock:`,
`prompt:` and `seat:` lines and the sidecar's `caustics-lock:` lines as they
come, then `unlocked by password after N attempts` or `unlocked by
fingerprint`. Then `l` and Return on the terminal locks again and `q` and
Return quits. Closing the window, or Ctrl-C, ends it at any moment, and
everything it started is stopped by pid.

```sh
bar/lock-try.sh [picture]                # real PAM, the reader on
bar/lock-try.sh --dry-run <secret>       # no PAM: <secret> unlocks
bar/lock-try.sh --fingerprint off        # CAUSTICS_LOCK_FINGERPRINT=0
bar/lock-try.sh --pam-service <name>     # CAUSTICS_LOCK_PAM_SERVICE
```

The run's sockets are under a private runtime directory, so they never meet
the live set's, but the buses are the real ones, so that logind, PAM and
fprintd behave as they will for the real lock. The sidecar is the real
session's locker while it runs, which has two side effects:

- **`LockedHint`**: while the window is locked the sidecar sets the real
  session's `LockedHint`, a hint only, and clears it on the unlock. A run
  that ends while locked would leave it set, so the script clears a hint
  its own run left behind.
- **The `Lock` signal**: a real `loginctl lock-session` (or the idle timer,
  or the lid) locks the window too, since the sidecar hears the real
  session's signal; the session's own locker locks the session as before.

A `LockedHint` already set when the run starts locks the window at once,
as it would a restarted canvas.

## The journal lines worth knowing

```
canvas: show 3 into slot 0, fading over 4000 ms     a fade began
canvas: fade 3 midpoint (t=0.5)                      its midpoint, said once
canvas: fade 3 done
canvas: dim 2 to 1000/1000 over 10000 ms             a dim began
canvas: dim 2 reached 1000/1000                      at its depth, said once
dim 2: cancelled by a key; told to caustics-wallpaper, the overlays go
output winit: background surface configured …       one per output
pipeline canvas_apply: built in 65 ms                one per pipeline, built off the loop; canvas_draw next, canvas_shape last
pipelines: all 3 built, 8.38 s after they were queued   seconds after a driver or caustic change, milliseconds after
output winit: overlay surface configured …          one per output, while a dim is up
output size: 2880x1800 px, told to caustics-wallpaper
sidecar caustics-reload: generation 2, 3 modules, swapped …   a tunables save
prompt: conversation 1 from caustics-polkit began                a sink's conversation; the panel comes up
prompt: conversation 1 held, no ask yet                          the caustic drew the title, no field
prompt: conversation 1 asking (echo 0, attempts 0)               the caustic drew its ask
output winit: prompt surface configured …                        one per output, while a prompt is up
seat: the keyboard is on the prompt (output winit)
prompt: conversation 1 answered; told to caustics-polkit         Return
prompt: conversation 1: an error line after the answer (attempts 1)
prompt: conversation 1: an info line                             a line beside the ask (the reader is live)
prompt: conversation 1: an error line, no answer pending         shown, not counted (a finger not recognised)
prompt: conversation 1 done, success
prompt: conversation 1 dismissed; told to caustics-polkit        Escape
lock: ext_session_lock_manager_v1 is bound                       at start
lock: caustics-lock asked to lock; locking
lock: ext_session_lock_v1 asked for, with 1 lock surfaces
output winit: lock surface configured …                          one per output, while locked
lock: the compositor locked the session; told to caustics-lock
seat: the keyboard is on the lock (output winit)
lock: input at the lock (a key); told to caustics-lock           at most once in 5 s, while locked
lock: caustics-lock asked to unlock; unlocking
lock: the session is unlocked; told to caustics-lock
caustics-lock: fingerprint: window open (locked)                 the lock sidecar's own, relayed
caustics-lock: fingerprint: reader … claimed for …; verifying
caustics-lock: fingerprint matched; unlocking
caustics-lock: fingerprint: window closed (idle); released       nobody at the lock
caustics-lock: fingerprint: released                             the reader let go at the end of the lock
exit: lock: the compositor ended caustics-lock's session lock …  `finished`: exit 1
```

No line carries what was typed, how much was typed, or a prompt's message.
