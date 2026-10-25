# Running caustics-canvas as a user unit

caustics-canvas is the canvas host (docs/canvas.md): the wallpaper on every
output, drawn by a caustic (`caustics/canvas.slang`) and cross-faded on
command, and the idle fade over every window. It is a layer-shell client on
the **background** layer, and on the **overlay** layer while a dim is up, and
replaces swaybg and chayang. In a session it runs as a **systemd user unit**,
`caustics-canvas.service`, which restarts it whatever it exits with and sends
its log to the journal.

## Install

The canvas is part of the bar's set: `bar/install.sh` builds and installs
it with everything else (bar/systemd/README.md) and links
`~/.local/bin/caustics-canvas` and `~/.local/bin/caustics-canvasctl` into the
set. The first time, link the unit and enable it:

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
  screen will be drawn and a session must always get it back.
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

Before any picture
the canvas is the dim colour; the path last shown is kept in
`$XDG_RUNTIME_DIR/caustics-canvas.last`, so a restarted canvas shows it
again. The look (the dim colour, the fade's curve, cover or contain) is
`caustics/canvas_tunables.slang`: save it and the running canvas changes.

## The journal lines worth knowing

```
canvas: show 3 into slot 0, fading over 4000 ms     a fade began
canvas: fade 3 midpoint (t=0.5)                      its midpoint, said once
canvas: fade 3 done
canvas: dim 2 to 1000/1000 over 10000 ms             a dim began
canvas: dim 2 reached 1000/1000                      at its depth, said once
dim 2: cancelled by a key; told to caustics-wallpaper, the overlays go
output winit: background surface configured …       one per output
output winit: overlay surface configured …          one per output, while a dim is up
output size: 2880x1800 px, told to caustics-wallpaper
sidecar caustics-reload: generation 2, 2 modules, swapped …   a tunables save
```
