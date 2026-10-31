# caustics-displays

The bar's displays applet: niri's outputs as one chip, and each output's
on/off, scale, mode and rotation set from its menu over niri's IPC socket.
A sidecar of the bar like `caustics-power` (docs/architecture.md,
*Sidecars*): it owns no Wayland object, speaks niri over `$NIRI_SOCKET`, and
writes one rows frame (docs/abi.md, *Rows*) with `applet = APPLET_DISPLAYS`.

```
caustics-displays
```

No arguments. The first frame on stdout is `Hello`; then a `Rows` frame
whenever the outputs change, `Action` frames are read from stdin, every
witness is one line on stderr, and the process exits 0 when stdin closes.

## The menu

- **The chip**: `video-display-symbolic`, beside the power chip
  (`APPLET_ORDER` in `caustics/bar_tunables.slang`).
- **One row per output**, labelled by its connector and the make and model
  niri reports (`DP-1 · Example Inc. EX2720`), checked while it is on. Its
  submenu is **On** (a check row), then three submenus of radio rows, the
  current one marked:
  - **Scale**: 1, 1.25, 1.5, 1.75, 2, 2.5, 3, and the current scale when it
    is none of those;
  - **Mode**: niri's list in niri's order, `2880×1800 @ 120.001 Hz`, the
    preferred one named;
  - **Rotation**: Normal, 90°, 180°, 270°. The flipped transforms are not
    offered; an output that has one shows it in the submenu's title with no
    row marked.

  An output that is off has the On row and nothing else.
- **A last, disabled row**: "Until niri restarts", under it "The layout
  lives in niri's config." niri applies an IPC change until it restarts or
  its config file changes that output. Positions and anything that should
  last are niri's config file's (or kanshi's), and this menu does not
  write either.

## Behaviour

- **Reading**: niri's `Outputs` request once at start, so the chip is there
  before the menu is ever opened; again when the menu opens
  (`ACTION_MENU_OPEN`) and every two seconds while it stays up; and after
  every change. niri's event stream has no output event, so nothing is read
  on a clock while the menu is closed. A failed read is one witness per
  distinct failure, and the picture stays as last read.
- **A click**: On or Off, a scale, a mode or a rotation becomes niri's
  `Output` request with `On`, `Off`, `Scale`, `Mode` or `Transform`, the
  same JSON `niri msg output` sends. Its outcome is one witness:
  `eDP-1 scale 1.5: ok`, or niri's words.
- **The only output on stays on.** Its On row is still checked and still
  answers a click, with a witness that says why nothing was asked of niri.
- **Blocking I/O off the loop**: every round trip is one connection on the
  worker thread, two-second timeouts each way; the loop only routes. A burst
  of reads waiting behind a slow one is one read.
- **Without `$NIRI_SOCKET`** (another compositor, or none): one witness, `no
  $NIRI_SOCKET; nothing to show`, and no frame, so no chip. The process
  still waits for stdin's EOF, as every sidecar must.
- **Row ids** are an output's slot (kept for as long as niri lists it, so an
  open submenu holds while another monitor comes or goes) times `0x100`,
  plus the row's offset (`src/rows.rs`). At most eight outputs are listed,
  and the frame's 256 rows are shared among their mode lists.

## Witnesses

```
caustics-displays: published rev=1 outputs=eDP-1 on 2880x1800@120.001 scale=1.75 Normal
caustics-displays: eDP-1 scale 1.5: ok
caustics-displays: eDP-1 off: refused, it is the only output on and the session would have no screen
caustics-displays: outputs: Connection refused (os error 111)
caustics-displays: no $NIRI_SOCKET; nothing to show
```

## Tests

`cargo test -p caustics-displays`: the replies parsed and the requests
compared with what `niri msg output` writes (`tests/niri.rs`), the frame
read back through the wire's own reader (`tests/frames.rs`), the dispatch
table (`tests/actions.rs`), and the binary against a socket standing in
for niri's (`tests/fake_niri.rs`). The nested gate's displays step opens
the chip on the nested niri's one output, sets its scale through the menu,
checks niri reports it, and sets it back the same way (docs/testing.md).
