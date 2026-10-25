# Running caustics-canvas as a user unit

caustics-canvas is the canvas host (docs/canvas.md): the wallpaper on every
output, drawn by a caustic (`caustics/canvas.slang`) and cross-faded on
command. It is a layer-shell client on the **background** layer and replaces
swaybg. In a session it runs as a **systemd user unit**,
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
caustics-canvasctl show ~/Pictures/night.png fade 7200  # a two-hour fade
caustics-canvasctl status   # fade=fading progress=0.37 duration=4.000 image=…
caustics-canvasctl dim 0.6 over 10
caustics-canvasctl undim
```

It exits 0 when the canvas answered `ok` and 1 otherwise (2 is kept for a
dim that input cancels, which the overlay will report). Before any picture
the canvas is the dim colour; the path last shown is kept in
`$XDG_RUNTIME_DIR/caustics-canvas.last`, so a restarted canvas shows it
again. The look (the dim colour, the fade's curve, cover or contain) is
`caustics/canvas_tunables.slang`: save it and the running canvas changes.

## The journal lines worth knowing

```
canvas: show 3 into slot 0, fading over 4000 ms     a fade began
canvas: fade 3 midpoint (t=0.5)                      its midpoint, said once
canvas: fade 3 done
output winit: background surface configured …       one per output
output size: 2880x1800 px, told to caustics-wallpaper
sidecar caustics-reload: generation 2, 2 modules, swapped …   a tunables save
```
