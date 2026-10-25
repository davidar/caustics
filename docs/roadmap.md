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
   building the set (its Slang release pinned by `Cargo.lock`), the set under
   `/usr/lib/caustics-bar/<hash>/`, the unit gated on the session and
   preset-enabled in place of the previous panel and notification daemon. No
   `caustics/` directory in the image: the reload sidecar has nothing to
   watch without one and idles, and the bar's baked caustic is the one it
   runs; whoever wants another configuration builds a set from their own
   tree. After the top-edge default has been lived with.

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
8. **The five VM-only steps** of the gate stay VM-only (one hangs a hardware
    driver's arena) until a live-GPU run is deliberately tried.
9. **The hot-reload stall.** A swap whose pipelines are new to the driver
    blocks the bar's event loop for several seconds (docs/testing.md). Create
    the pipelines off the loop and swap when they are ready.

## Icons

10. XPM and ICO `Icon=` paths; KDE colour-scheme SVGs recoloured with the
    text ink (two routes, undecided); a Steam game's icon from its manifest;
    a pixmap whose file went away cannot be retracted; a theme installed
    while a sidecar runs is not searched until it restarts.

## Watching the bar

11. **A witness watcher.** An always-on local reader of the journal's
    witnesses that files a report when a stated rule is broken, for the next
    piece of work to pick up. One clause exists: `notify/tools`'
    `caustics-notify-watch`, which reports a notification click that reached no
    window.

## Out-of-tree sidecars

A sidecar that is a chip with a menu already needs no change here: it
writes `Rows` frames under its own applet value, the tunables'
`APPLET_OTHER` pass places it, and `bar/abi/examples/rows_demo.rs` is the
worked example (docs/architecture.md, *Sidecars*). Three things still
stand between that and a sidecar from another tree.

12. **An additive roster.** `SIDECARS` is compiled in and `$CAUSTICS_SIDECARS`
    *replaces* it, so adding one sidecar to an installed bar means naming
    all of them in a unit drop-in. A variable that appends, or a directory
    beside the set whose entries are spawned like the compiled names,
    resolved the same way (docs/abi.md, *Lifecycle*).
13. **The ABI pin, written down.** The bar refuses a sidecar built against
    another ABI at its first frame (docs/abi.md, *Hello*), so a sidecar from
    another tree pins `caustics-bar-abi` to the revision the installed set
    was built from. That is the rule working; it needs saying in *Writing a
    sidecar*, with the way to read the installed set's revision.
14. **A sidecar's own look.** Everything a third-party sidecar shows is drawn
    by `applets`: the generic chip, rows and sliders. A sidecar that wants
    its own drawing (as the charts and the toasts have) needs a subject
    module in the caustic, which today means editing `caustics/`. The design
    that removes that: a sidecar ships a Slang module, the reload sidecar
    links it into the program with the rest, and `bar_dispatch` and the
    `Private` struct are assembled from what the modules declare instead of
    written by hand. A wire change (a `Module` frame from a sidecar other
    than `caustics-reload`; a module's declared kinds and scratch) and a
    build-time generator. Its prerequisite is small and worth doing first:
    the dock preview's dwell fields and the applets' slider drag live in the
    top of `Private` and the dwell is written by `bar_popups`; each belongs
    in its subject's scratch, with `bar_popups` telling the dock its popup
    ended through `dispatch_popup_ending`, as it tells notify today.

## The canvas

15. **Everything else a session paints that is not a window.** The
    wallpaper, the idle fade, the lock screen, the polkit prompt and the
    login greeter as one caustic on a second host of the bar process's
    shape, with the jobs as sidecars; designed in docs/canvas.md, built in
    its *Order*: the bar's mechanism lifted into a crate first, then the
    host and the wallpaper, the fade, the prompt and polkit, the lock, the
    greeter. The test prerequisite is a harness tool that can type
    (docs/testing.md, *The nested gate*).

## Named open once, not re-checked against the tree since

Smaller items left by earlier work; whether later work closed them has not
been verified line by line. Check before building.

- The startup fade's slow convergence (`redraw: still animating after
  10 s`), a caustic matter.
- `CAUSTICS_NETWORK_DRY_RUN` swallows the scan, which is a read; no test drives
  a menu whose item disappears.
- The positioner has no `ResizeY` and there is no scroll indicator; another
  panel's exclusive zone is what `POPUP_MARGIN` absorbs.
- The compiler sends the program's reflection with every entry point,
  identical each time (docs/abi.md, *Module*); one copy per generation is a
  wire change.
- A test that rewords `DOCK_MENU` in the tunables and reads it off the
  screen; a test that edits a binding and watches the reload refuse.
