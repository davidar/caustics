# Roadmap

What is not built yet, in one place. Each entry stands on its own. Order
within a section is the order the work is meant to happen. What exists is
CHANGELOG.md.

## The tree

1. **The VM target as this project's own.** `bar/targets/vm.sh` and
   `bar/vm-run.sh` source topaz-os's test harness and boot its image
   (docs/testing.md, *The VM target*). Two steps, in order. First, move every
   step whose evidence never needed a compositor (a click cycles a dock
   entry, a menu row opens a level, a slider tick is one write, an applet's
   rows become an item, the watchdog fires on its tunable) into the oracle,
   which runs in milliseconds on any Vulkan device; the VM then keeps about
   ten steps. Second, an owned launcher under `bar/vm/`: a stock cloud image
   (any distribution with a recent niri) plus cloud-init for a test user,
   sshd, niri, autologin, UPower and lavapipe, provisioned once into a disk
   that is rebuilt from the recipe and never hand-made, with the QMP, VNC and
   screendump pieces written here.
2. **Small rough edges.** A network in range with no saved profile still
   opens the settings program on click; it should prompt or do nothing. The
   oracle's module comment and the drop-in quote one vendor's Vulkan driver
   file by name. `DEFAULT_FAVOURITES` seeds a
   COSMIC application set; it is documented, but a first-run default for
   other desktops would be kinder.

## Packaging

3. **The bar as a distribution image's panel by default.** A build stage
   with slangc pinned, the set under `/usr/lib/caustics-bar/<hash>/`, `caustics/`
   at `/usr/share/caustics/`, the unit gated on the session and preset-enabled in
   place of the previous panel and notification daemon. After the top-edge
   default has been lived with.

## Left by the replacement of a desktop panel

4. **Known and small.** The GPU chart's ease stalls ten seconds after a toast
   closes (a settle threshold, caustic-only). `caustics-tray`'s journal volume on
   icon flips (debug level, or a health-line count). A suspend cycle has
   never been exercised by a test. `ACTION_TOAST_SHOWN` names rows a short
   output clipped. `bar_check`'s `TEXT_PX` check is the oracle's, not the
   live reload's. The no-battery desktop has no power menu rows of its own.
   A third `wpctl` child per slider drag. A tray frame that changes only an
   icon name costs a full build, compose and shape (the shape decision should
   hash text bytes only). An evicted image whose pixels the store let go
   falls back to the letter. The bar's resident memory after a cold swap (the
   driver's compile heap stays in-process).
5. **Not built, by design or for later.** Toasts' hover-hold, close button,
   per-toast progress, sound and top-centre placement; a max-charge toggle,
   keyboard backlight and a discrete-GPU warning; an application library
   button (recommended against); MPRIS media controls; notifications grouped
   by application; the calendar's previous and next arrows.

## Text

The text engine's own gaps (colour fonts, Unicode itemisation and bidi,
complex-script shapers, variable instances, a faster shaper, a fallback for a
device without the shader clock) are punchcutter's roadmap.

## Wire and harness

6. **Raise `MAX_ROWS`** at the next wire version, with the rows arena that
    must grow with it (docs/abi.md, *For the next wire version*).
7. **A `rows: slot …` witness** from the caustic, designed and not built;
    the wire reader's line stands in for it (docs/abi.md, *The arenas*).
8. **A layout-line race in the gate's chart-set step**: a chart the sidecar
    was not asked for is laid out, seen twice, not understood.
9. **The five VM-only steps** of the gate stay VM-only (one hangs a hardware
    driver's arena) until a live-GPU run is deliberately tried.
10. **The hot-reload stall.** A swap whose pipelines are new to the driver
    blocks the bar's event loop for several seconds (docs/testing.md). Create
    the pipelines off the loop and swap when they are ready.

## Icons

11. XPM and ICO `Icon=` paths; KDE colour-scheme SVGs recoloured with the
    text ink (two routes, undecided); a Steam game's icon from its manifest;
    a pixmap whose file went away cannot be retracted; a theme installed
    while a sidecar runs is not searched until it restarts.

## Watching the bar

12. **A witness watcher.** An always-on local reader of the journal's
    witnesses that files a report when a stated rule is broken, for the next
    piece of work to pick up. One clause exists: `notify/tools`'
    `caustics-notify-watch`, which reports a notification click that reached no
    window.

## Named open once, not re-checked against the tree since

Smaller items left by earlier work; whether later work closed them has not
been verified line by line. Check before building.

- The startup fade's slow convergence (`redraw: still animating after
  10 s`), a caustic matter.
- `CAUSTICS_NETWORK_DRY_RUN` swallows the scan, which is a read; no test drives
  a menu whose item disappears.
- The positioner has no `ResizeY` and there is no scroll indicator; another
  panel's exclusive zone is what `POPUP_MARGIN` absorbs.
- The compiler sends one reflection per entry point, near-identical.
- A test that rewords `DOCK_MENU` in the tunables and reads it off the
  screen; a test that edits a binding and watches the reload refuse.
