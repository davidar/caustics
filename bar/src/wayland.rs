//! The bar's Wayland side: one layer surface, one popup, one pointer — and
//! the meeting point of everything else the bar is made of.
//!
//! Everything here passes one of the bar process's three membership tests (docs/architecture.md, *The bar process*): it owns a Wayland object, it
//! sits on a configure/ack round trip, or it must decide before forwarding an
//! event synchronously. Nothing here decides what the bar *is* — every pointer
//! event becomes a [`Command`] and the caustic answers; every popup geometry
//! comes out of the mirror; every action in the ring is forwarded by `Bar::forward` to the one method that performs it and handed on
//! without being second-guessed.
//!
//! The one thing it will not obey is a value that would provoke a protocol
//! error: the rule that no 0×0 anchor ever reaches a validator (docs/architecture.md, *Rules*) is
//! enforced in [`Bar::positioner`], because a caustic bug must show up as a
//! wrong-looking popup and never as a killed client.
//!
//! # No policy on this side
//!
//! The caustic rebuilds the item table from the facts, so a rebuild can move what an index
//! into it points at — a sidecar's rows, the chart strip. Closing the popup on
//! every such rebuild would be safe and useless: a menu that shuts whenever its
//! own rows change is no use to a Wi-Fi menu whose rows arrive from a scan
//! while it is open.
//!
//! **The caustic owns the menu's life entirely.** It holds the level as the
//! registry *id* of the item the popup hangs off, resolves it to this frame's
//! index by a scan, and closes the popup itself — through `POPUP_CLOSING`, like
//! every other reason a menu ends — only when the id resolves to nothing, which
//! means the item is gone. It records the pressed item's id too, so a release
//! across a re-layout fires nothing rather than firing whatever moved into that
//! slot. This side just recomposes the facts and uploads them; `prepare_menu` tells the sidecar its menu
//! opened and [`Bar::destroy_popup`] that it closed (docs/abi.md, *Action*), and
//! neither of those is a decision.

use std::fs::File;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::Instant;

use anyhow::{Context, Result};
use smithay_client_toolkit::reexports::protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::{self, XdgActivationV1},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Surface},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_xdg_popup, delegate_xdg_shell,
    output::{OutputHandler, OutputInfo, OutputState},
    reexports::calloop::{
        LoopHandle, RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{AxisScroll, PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        xdg::{
            XdgPositioner, XdgShell,
            popup::{ConfigureKind, Popup, PopupConfigure, PopupHandler},
            window::{Window, WindowConfigure, WindowHandler},
        },
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::GlobalList,
    protocol::{wl_output, wl_pointer, wl_seat, wl_surface},
};

use crate::abi::wire::{self, Frame};
use crate::abi::{self, Command, FrameInputs, Note, State};
use crate::clock;
use crate::facts::{Facts, Wanted};
use crate::feeders::{Feeders, pipe};
use crate::gpu::Gpu;
use crate::health::{self, Health};
use crate::model::{Action, Model, Update, Updates};
use crate::modules::{Collected, Modules};
use crate::witness::Printer;
use caustics_host::images::Images;

/// Which screen edge the bar takes. `FrameInputs::edge` carries it to the
/// caustic (which flips the popup gravity and the focus underline), and the
/// embassy turns it into the layer surface's anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

impl Edge {
    pub fn abi(self) -> u32 {
        match self {
            Edge::Top => abi::EDGE_TOP,
            Edge::Bottom => abi::EDGE_BOTTOM,
        }
    }

    /// `EDGE_*` back to an `Edge`. Anything else is a mirror the caustic has
    /// not written yet (a zeroed state buffer reads as EDGE_TOP), so the
    /// caller checks `frame` before believing it.
    pub fn from_abi(value: u32) -> Option<Edge> {
        match value {
            abi::EDGE_TOP => Some(Edge::Top),
            abi::EDGE_BOTTOM => Some(Edge::Bottom),
            _ => None,
        }
    }

    /// The name the lock file and the witness lines use.
    pub fn name(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Bottom => "bottom",
        }
    }
}

// ---------------------------------------------------------------------------
// The compiled-in surface, and where it comes from.
//
// The bar has to ask for an anchor, a height and an icon size before any frame
// exists — the Wayland connection needs one and the edge lock is taken before
// the connection is even made. These three are that first ask, and they are
// the *shipped* `caustics/bar_tunables.slang`'s values, asserted by
// `the_compiled_defaults_are_the_shipped_tunables` below so the two cannot
// drift. From the first mirror readback on, `Bar::reconcile_tunables` takes
// over and the caustic decides (docs/architecture.md, *Configuration is the caustic*).
// ---------------------------------------------------------------------------

/// `EDGE` in the shipped tunables.
pub const BAR_EDGE: Edge = Edge::Top;

/// `BAR_HEIGHT` in the shipped tunables, logical px.
pub const BAR_HEIGHT: u32 = 32;

/// `ICON_LOGICAL` in the shipped tunables. Device px is this times the scale.
///
/// Nothing in the bar reads it — an icon's box is `bar_build`'s, from
/// the tunables directly. It stays as the subject of
/// `the_compiled_defaults_are_the_shipped_tunables`, which is what keeps this
/// file's three compiled-in numbers and the caustic's from drifting apart.
#[cfg_attr(not(test), allow(dead_code))]
pub const ICON_LOGICAL: u32 = 20;

/// How long a frame request may stay unanswered before the bar draws on an
/// event anyway. Longer than any real frame interval, shorter than a human
/// noticing a click was dropped.
const FRAME_STALL: std::time::Duration = std::time::Duration::from_millis(500);

/// The layer-shell namespace, and the name a compositor rule would match on.
const NAMESPACE: &str = "caustics-bar";

/// The launcher a dock click prefers, when the machine has it: it resolves
/// the Flatpak wrappers and the D-Bus activation path that a bare `Exec=`
/// does not. Absolute, because a launch is not a shell and there is no `PATH`
/// search to inherit.
const GIO: &str = "/usr/bin/gio";

/// The layer-surface anchor for an edge: the bar spans the screen's width and
/// sits against one long edge.
fn anchor_for(edge: Edge) -> Anchor {
    let sides = Anchor::LEFT | Anchor::RIGHT;
    match edge {
        Edge::Top => Anchor::TOP | sides,
        Edge::Bottom => Anchor::BOTTOM | sides,
    }
}

/// Which feeder owns an action, for the witness line. A table, not a decision:
/// `Feeders::perform` is what actually claims them.
///
/// `compositor` is [`feeders::compositor::Compositor::name`] — the seam's own
/// word, so a bar hosted on something other than niri says so here without
/// this table knowing what it is hosted on.
fn action_owner<'a>(action: &Action, compositor: &'a str) -> &'a str {
    match action {
        // `Spawn` and `Launch` are the same click at two stages — the app id
        // the caustic named, and the argv the desktop entry gave for it — and
        // both end at the compositor, which is the thing on this machine
        // whose job is starting clients.
        Action::Activate(_)
        | Action::Close(_)
        | Action::ToggleOverview
        | Action::Spawn { .. }
        | Action::Launch { .. } => compositor,
        Action::PreviewWanted(_) => "capture",
        // The one applet whose click stays in this process, because the object
        // it flips is on the bar's own connection (docs/architecture.md,
        // *Sidecars*; `feeders/caffeine.rs`).
        Action::ToggleCaffeine => "caffeine",
        // Pin and unpin are the embassy's own: no connection owns a list of
        // app ids. They say `dock` rather than `embassy` because the list
        // they change is the dock's state (`crate::favourites`).
        Action::Pin { .. } | Action::Unpin { .. } => "dock",
        // Which sidecar is in the action itself; the pipe feeder matches it
        // against the children it started. That includes every power action
        // and every tray click: the dry run and the witness for those are
        // `caustics-power`'s and `caustics-tray`'s own.
        Action::Sidecar { .. } => "sidecar",
    }
}

fn popup_kind_name(kind: u32) -> &'static str {
    match kind {
        abi::POPUP_MENU => "menu",
        abi::POPUP_PREVIEW => "preview",
        abi::POPUP_TOAST => "toast",
        _ => "none",
    }
}

/// Why the process is stopping. `main` turns this into an exit status.
#[derive(Clone, Debug)]
pub enum Exit {
    /// `--max-runtime` elapsed. Status 0.
    Deadline,
    /// A protocol error or a fatal Wayland dispatch failure. Status 1, which
    /// the unit restarts: a dead bar never lingers (docs/architecture.md, *Rules*).
    Protocol(String),
    /// The compositor closed the layer surface. Status 0 — it asked.
    Closed,
}

pub struct Bar {
    // --- sctk state -------------------------------------------------------
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    xdg_shell: XdgShell,
    conn: Connection,
    qh: QueueHandle<Bar>,

    // --- the surfaces -----------------------------------------------------
    layer: LayerSurface,
    /// Buffer scale, integer only. `wp_fractional_scale_v1` is not bound, so
    /// a fractional output gets the compositor's integer buffer scale.
    scale: u32,
    /// The bar in logical px, as the compositor configured it.
    bar_logical: (u32, u32),
    configured: bool,

    /// Set by the layer-surface configure, acted on after the dispatch
    /// returns: `(width, height)` in px. See [`Bar::after_dispatch`].
    pending_surface: Option<(u32, u32)>,
    /// Likewise for the popup: its px size, once it has been configured.
    pending_popup: Option<(u32, u32)>,

    popup: Option<Popup>,
    popup_configured: bool,
    /// Set while a popup teardown is in flight, so `CLOSING` is acted on once.
    popup_closing: bool,
    /// The kind the live popup is: MENU takes a grab, PREVIEW must not.
    popup_kind: u32,
    /// The anchor rect the live popup was created against, bar px.
    ///
    /// Kept because an `ACT_POPUP_MOVE` carries a size and nothing else: the
    /// anchor has not moved — the chip the menu hangs off is where it was —
    /// and a reposition needs a whole positioner.
    popup_anchor: [f32; 4],
    /// The logical size last asked of the positioner.
    popup_want: (i32, i32),
    /// The reposition token; `xdg_popup.reposition` echoes it back.
    popup_serial: u32,

    // --- input ------------------------------------------------------------
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    /// The most recent button serial, which is what an `xdg_popup.grab` wants
    /// — and what an activation token is asked with.
    last_button_serial: u32,
    /// `xdg_activation_v1`, when the compositor offers it: a token for a
    /// click on a row that asked for one (`RFLAG_ACTIVATION`), so the
    /// application the sidecar hands it to may raise itself.
    activation: Option<XdgActivationV1>,
    /// Which surface the pointer is on, or [`abi::NO_INDEX`]. Kept here only
    /// for the stall line and for routing a pointer event to the right target;
    /// what the caustic draws from is its own copy, set by `CMD_POINTER_*`
    /// (docs/abi.md, *The action ring*).
    pointer_target: u32,

    // --- the surface the caustic asked for ---------------------------------
    /// The edge the layer surface is anchored to, and the height it was asked
    /// for. Compiled-in defaults until the first `reconcile_tunables`.
    edge: Edge,
    height: u32,
    /// The running caustic's `getStringHash` table (docs/architecture.md, *Text*).
    ///
    /// It starts **full**, from the reflection `build.rs` baked beside the
    /// SPIR-V — so the literals answering on the first frame are the ones the
    /// modules running on the first frame were compiled from. The `Frame::
    /// Module` arm replaces it whole when a generation reaches the GPU, and
    /// leaves it alone when one does not.
    pub strings: crate::strings::Strings,
    /// True once a mirror has been reconciled at least once, so the first
    /// witness line is unconditional and the rest are per change.
    tunables_seen: bool,
    /// The edge lock (`main::take_edge_lock`), owned here from `main`'s frame
    /// so a reconcile to another edge can swap it.
    edge_lock: Option<File>,

    // --- what the bar knows -----------------------------------------------
    model: Model,
    /// What the embassy knows, as the tables `bar_build` builds the item and
    /// block tables from (docs/abi.md, *The facts*).
    facts: Facts,
    feeders: Feeders,
    /// The sidecar whose menu is up and the row it hangs off, which is what
    /// `ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE` name (docs/abi.md, *Action*).
    ///
    /// `None` while nothing is open **and** for the dock's own menu: its rows
    /// are the bar's own, so there is no sidecar at the other end to tell.
    open_menu_notice: Option<(String, u32)>,
    /// Where the dock's favourites live (`crate::favourites`). Resolved once
    /// here so nothing further down reads the environment, and so the tests
    /// can be given a temp directory.
    favourites_path: PathBuf,

    // --- content ----------------------------------------------------------
    /// What `caustics-glyphs` said its font is (docs/abi.md, *Font*) — the
    /// cap height the caustic snaps against, the counts the health line
    /// reports, and the family name. `None` until the first `Font` frame, which
    /// is the bar's first seconds and a bar with `caustics-glyphs` not installed.
    ///
    /// **The bar loads no font.** It holds no curves, no tables and no parser:
    /// `Bytes` frames land in the GPU buffers through `Gpu::write_bytes` and
    /// this is the whole of what the bar does with a font.
    font: Option<wire::Font>,
    /// Whether any `Bytes` run has landed. The bar draws its first frames with
    /// empty curve buffers — every index in `text.slang` is bounded, so that is
    /// a bar with no text in it rather than a broken one — and this is what
    /// makes "the font arrived" one witness line and not one per chunk.
    curves_landed: bool,
    /// `Bytes` runs that arrived before there was a GPU to put them in.
    ///
    /// The sidecars are spawned before the layer surface is configured, and the
    /// font goes out the moment `caustics-glyphs` has it — which is often *first*.
    /// Every frame on this wire is whole-state and is sent once, so a run
    /// dropped here is a buffer that stays empty for the life of the process;
    /// they wait here instead and `bring_up_gpu` drains them in order.
    pending_bytes: Vec<wire::Bytes>,
    /// The generation of `Module` frames being collected (docs/abi.md
    /// *Module*).
    ///
    /// **The bar runs no compiler.** `caustics-reload` watches the `.slang` files
    /// and compiles them; one frame of SPIR-V and reflection per entry point ([`ENTRIES`]) arrives here and
    /// `Gpu::reload_all` swaps them in, all or nothing.
    modules: Modules,
    /// Everything that is pixels: [`abi::MAX_IMAGE_SLOTS`] slots, LRU by
    /// generation.
    images: Images,

    /// A capture frame arrived and has not reached the preview slot yet.
    preview_pending: bool,
    /// The valid extent of what is in the preview slot, in texels.
    preview_size: [u32; 2],

    /// The output the layer surface last entered, and its logical size as
    /// `xdg_output` reported it, which the caustic clamps a long menu to.
    /// Zero until known; the
    /// caustic reads a zero `FrameInputs::output_size` as "no bound".
    output: Option<wl_output::WlOutput>,
    output_logical: (u32, u32),
    /// Premultiplied scratch for the upload, kept so a 2 fps preview does not
    /// allocate eight megabytes twice a second.
    preview_scratch: Vec<u8>,

    // --- the GPU ----------------------------------------------------------
    pub gpu: Option<Gpu>,
    commands: Vec<Command>,
    dirty: bool,
    /// A `wl_surface.frame` request rode the last commit and has not come
    /// back. While it is out, dirt waits: events coalesce into one frame per
    /// compositor repaint instead of one frame per pointer motion, which is
    /// the whole of docs/architecture.md's "render on events" under Mailbox.
    frame_pending: bool,
    /// When the pending frame request was made, for the stall fallback.
    frame_requested_at: Instant,
    /// The last mirror said `wants_redraw`: a spring is in flight, or the
    /// dwell clock is running, and the next frame callback is a frame.
    animating: bool,
    started: Instant,
    last_frame: Instant,
    frames: u64,

    // --- the loop ---------------------------------------------------------
    /// Set by `main` once calloop exists. The bar owns exactly one timer of
    /// its own beyond the clock: the capture feeder's,
    /// armed on `Feeders::next_deadline` (feeders/mod.rs, "Timers").
    loop_handle: Option<LoopHandle<'static, Bar>>,
    capture_token: Option<RegistrationToken>,
    capture_deadline: Option<Instant>,

    /// The witness ring, printed (docs/abi.md, *The witness ring*). It holds one thing:
    /// which format literals it could not resolve, so that is one warning per
    /// literal per generation rather than one per frame.
    printer: Printer,
    /// How far each ring has been drained: `(the frame it was stamped, how
    /// many records were taken)`.
    ///
    /// A ring belongs to the frame stamped on it (*The three rules the buffer adds*, rule 1), so a frame that
    /// failed at the swapchain keeps its stamp and the next attempt *appends*
    /// to what it pushed. This pair is what makes that safe to read twice: a
    /// drain takes the records past the count it last took of the same frame,
    /// and everything of a new frame.
    drained_actions: (u32, u32),
    drained_witnesses: (u32, u32),
    /// `Surface::action_lost` as of the last warning about it.
    logged_action_lost: u32,
    /// `Surface::witness_lost` likewise. `bar/bar-test.sh` asserts this line never appears.
    logged_witness_lost: u32,
    /// Whether the last mirror said `wants_redraw`, for the health line.
    logged_redraw: bool,
    /// Whether the model has ever carried a samples frame. The one summary
    /// line the caustic cannot push: a frame that never arrived is not the
    /// same fact as a frame with no rows in it.
    logged_charts: bool,
    /// The stall trigger and the hourly health line (`health.rs`).
    health: Health,

    /// `CAUSTICS_BAR_FAULT=protocol`: provoke `invalid_scale` after the first
    /// presented frame, so the unit's restart path is exercised for real.
    fault_protocol: bool,
    fault_fired: bool,

    pub exit: Option<Exit>,
}

impl Bar {
    /// Bind the globals, start every feeder, create the layer surface and
    /// commit it. The wgpu surface is *not* created here: a layer surface may
    /// not have a buffer attached before its first configure, so nothing
    /// touches the surface until that configure lands.
    pub fn new(
        conn: Connection,
        globals: &GlobalList,
        qh: QueueHandle<Bar>,
        edge_lock: Option<File>,
        updates: Updates,
    ) -> Result<Bar> {
        let compositor =
            CompositorState::bind(globals, &qh).context("wl_compositor is not available")?;
        let layer_shell =
            LayerShell::bind(globals, &qh).context("zwlr_layer_shell_v1 is not available")?;
        let xdg_shell = XdgShell::bind(globals, &qh).context("xdg_wm_base is not available")?;

        // The compiled-in ask. The caustic's own answer arrives with the
        // first mirror, about a hundred milliseconds in.
        let height = BAR_HEIGHT;
        let surface = compositor.create_surface(&qh);
        let layer =
            layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some(NAMESPACE), None);
        layer.set_anchor(anchor_for(BAR_EDGE));
        layer.set_size(0, height);
        layer.set_exclusive_zone(height as i32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();

        // Everything that talks to something outside this process starts here
        // and reports its own absence: a compositor without the ext globals,
        // an IPC socket that is not there, a sidecar that will not spawn. None
        // of it can fail the bar (feeders/mod.rs).
        //
        // After the layer surface, not before: the caffeine feeder hangs its
        // idle inhibitor on that surface, which is why `bind` takes it.
        let feeders = Feeders::bind(globals, &qh, layer.wl_surface(), updates);
        // Membership test (docs/architecture.md, *The bar process*): owns a Wayland object.
        let activation = globals
            .bind::<XdgActivationV1, _, _>(&qh, 1..=1, ())
            .inspect(|_| tracing::info!("bound xdg_activation_v1"))
            .inspect_err(|err| tracing::info!("xdg_activation_v1 absent: {err}"))
            .ok();

        tracing::info!(
            "feeders: toplevels={} {}={} capture={} caffeine={}",
            feeders.toplevels.is_bound(),
            feeders.compositor.name(),
            feeders.compositor.is_bound(),
            feeders.capture.is_available(),
            if feeders.caffeine.is_bound() {
                "bound"
            } else {
                "absent"
            },
        );

        // The dock's favourites: state, read once here and never watched
        // (`crate::favourites`). A missing file is the built-in seed, and every
        // other way of not having a usable one is the seed too.
        let favourites_path = crate::favourites::path();
        let favourites = crate::favourites::load(&favourites_path);

        let now = Instant::now();
        Ok(Bar {
            registry_state: RegistryState::new(globals),
            seat_state: SeatState::new(globals, &qh),
            output_state: OutputState::new(globals, &qh),
            compositor,
            xdg_shell,
            conn,
            qh,
            layer,
            scale: 1,
            bar_logical: (0, height),
            configured: false,
            pending_surface: None,
            pending_popup: None,
            popup: None,
            popup_configured: false,
            popup_closing: false,
            popup_kind: 0,
            popup_anchor: [0.0; 4],
            popup_want: (0, 0),
            popup_serial: 0,
            seat: None,
            pointer: None,
            last_button_serial: 0,
            activation,
            pointer_target: abi::NO_INDEX,
            edge: BAR_EDGE,
            height,
            strings: crate::strings::Strings::baked(),
            tunables_seen: false,
            edge_lock,
            model: Model {
                favourites,
                ..Model::default()
            },
            facts: Facts::new(
                pipe::commands()
                    .iter()
                    .map(|command| pipe::name_of(command))
                    .collect(),
            ),
            feeders,
            open_menu_notice: None,
            favourites_path,
            font: None,
            curves_landed: false,
            pending_bytes: Vec::new(),
            modules: Modules::new(),
            images: Images::new(),
            preview_pending: false,
            preview_size: [0, 0],
            output: None,
            output_logical: (0, 0),
            preview_scratch: Vec::new(),
            gpu: None,
            commands: Vec::new(),
            dirty: false,
            frame_pending: false,
            frame_requested_at: now,
            animating: false,
            started: now,
            last_frame: now,
            frames: 0,
            loop_handle: None,
            capture_token: None,
            capture_deadline: None,
            printer: Printer::new(),
            drained_actions: (0, 0),
            drained_witnesses: (0, 0),
            logged_action_lost: 0,
            logged_witness_lost: 0,
            logged_redraw: false,
            logged_charts: false,
            health: Health::new(),
            fault_protocol: std::env::var("CAUSTICS_BAR_FAULT").as_deref() == Ok("protocol"),
            fault_fired: false,
            exit: None,
        })
    }

    /// Hand the bar the loop it lives in, once `main` has made it. Only the
    /// capture timer needs it — every other source `main` owns outright.
    pub fn set_loop_handle(&mut self, handle: LoopHandle<'static, Bar>) {
        self.loop_handle = Some(handle);
        self.arm_capture();
    }

    /// Queue a command for the next frame and ask for one.
    fn queue(&mut self, command: Command) {
        // Motion on the same target as the previous motion replaces it: only
        // the newest position matters to the caustic, and a frame's worth of
        // pointer events must never push a click off the end of the ring.
        if command.tag == abi::CMD_POINTER_MOTION
            && let Some(last) = self.commands.last_mut()
            && last.tag == abi::CMD_POINTER_MOTION
            && last.target == command.target
        {
            *last = command;
            self.dirty = true;
            return;
        }
        if self.commands.len() >= abi::MAX_COMMANDS {
            tracing::warn!("command ring full ({}); dropping", abi::MAX_COMMANDS);
            return;
        }
        self.commands.push(command);
        self.dirty = true;
    }

    /// True until the first layer-surface configure has been acted on. `main`
    /// pumps the queue on this before handing it to calloop.
    pub fn awaiting_first_surface(&self) -> bool {
        self.gpu.is_none() && self.exit.is_none()
    }

    /// Ask for a frame. The `Frame::Module` arm uses it to show a swapped
    /// caustic immediately rather than waiting for the next pointer event.
    pub fn request_frame(&mut self) {
        self.dirty = true;
    }

    // -- the model ---------------------------------------------------------

    /// One [`Update`] from a feeder. The whole inbound half of the bar goes
    /// through here: `main` inserts the channel's receiving end as a calloop
    /// source whose callback is this.
    pub fn apply_update(&mut self, update: Update) {
        // A captured still changes no item, so it costs an upload and a frame
        // rather than a reshape of every label on the bar.
        if matches!(update, Update::Preview(_)) {
            if self.model.apply(update) {
                self.preview_pending = true;
                self.dirty = true;
            }
            return;
        }
        // A sidecar tick is 8 KiB to a buffer and one frame. Whether the
        // *set* of charts moved is `bar_apply`'s to notice — it reads
        // `samples.charts` already and compares the kinds against the copy it
        // keeps — so this side uploads and asks for a frame, and nothing here
        // parses a chart (docs/abi.md, *The arenas*).
        if matches!(update, Update::Samples { .. }) {
            if !self.model.apply(update) {
                return;
            }
            self.upload_samples();
            self.dirty = true;
            return;
        }
        // A sidecar's rows are whole-state and most frames move only a value:
        // a volume, a signal, a slider following the pointer. **The embassy does not look.** The body goes into that sidecar's region of the rows
        // arena whole and one `CMD_ROWS` says which slot moved; `bar_apply`
        // compares the region's shape against the copy it keeps and asks for a
        // rebuild only if it changed (docs/abi.md, *The arenas*). `Model::apply` is what makes an
        // identical frame no write at all.
        if let Update::Rows { sidecar, .. } = &update {
            let sidecar = sidecar.clone();
            if !self.model.apply(update) {
                return;
            }
            self.publish_rows(&sidecar);
            return;
        }
        // A sidecar that is gone leaves nothing behind it that is still true:
        // its region's header is zeroed, which is what makes `bar_build` skip
        // the slot, and the pixmaps it sent leave the store.
        if let Update::SidecarGone(name) = &update {
            let sidecar = name.clone();
            let changed = self.model.apply(update);
            self.forget_rows(&sidecar);
            if changed {
                self.mark_facts_dirty();
            }
            return;
        }
        // The answer frames: bytes for a buffer, a font's facts, a
        // name resolved, an app id answered, an icon's pixels. Nothing in the
        // model — none of them is a fact about the bar's *content*; they are
        // bytes on their way to a buffer, a slot or a name table.
        if let Update::Frame { sidecar, frame } = update {
            self.apply_frame(&sidecar, frame);
            return;
        }
        if let Update::PreviewFailed { window, why } = &update {
            tracing::info!("preview: window {window} failed ({why})");
            self.preview_size = [0, 0];
            self.dirty = true;
        }
        if let Update::Absent(name) = &update
            && !self.model.absent.contains(name)
        {
            tracing::info!("absent: {name}");
        }
        if self.model.apply(update) {
            self.mark_facts_dirty();
        }
    }

    /// One frame from a sidecar that is not `Samples` or `Rows`
    /// (docs/abi.md, *Bytes*–*Pixmap*). Every one of them is an answer or a run of
    /// bytes: the bar copies and reads none of it, and what it keeps is the `Font` facts, the answers and pixmaps in `Facts`, and a module generation being collected.
    fn apply_frame(&mut self, sidecar: &str, frame: Frame) {
        match frame {
            // A run of a GPU buffer. The one arm that is genuinely a byte
            // router: a buffer number, an offset, a length, and `Gpu` grows and
            // writes without anything here knowing what is in it.
            Frame::Bytes(bytes) => {
                let Some(gpu) = self.gpu.as_mut() else {
                    self.pending_bytes.push(bytes);
                    return;
                };
                gpu.write_bytes(bytes.buffer, bytes.offset, bytes.total, &bytes.bytes);
                if !self.curves_landed {
                    self.curves_landed = true;
                    tracing::info!(
                        "text: the first curves landed {} ms after start; the blocks already \
                         published are shaped on the next frame",
                        self.started.elapsed().as_millis()
                    );
                }
                self.dirty = true;
            }
            // A different glyph space: whatever was learned against the last
            // one is meaningless, and the names are asked again as they come up.
            Frame::Font(font) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.set_cap_height(font.cap_height);
                }
                tracing::info!(
                    "font: {} ({} glyphs at {} upem, cap {:.3} em, icons from {}) from {sidecar}",
                    font.family,
                    font.glyph_count,
                    font.upem,
                    font.cap_height,
                    font.icon_base
                );
                self.font = Some(font);
                self.facts.forget_glyphs();
                self.mark_facts_dirty();
            }
            // A name became an id, or did not. Either answer changes what the
            // build draws for it, so the table is rebuilt — once per name.
            Frame::Glyph(glyph) => {
                let gid = (glyph.flags & wire::GLYPH_MISSING == 0).then_some(glyph.gid);
                if self.facts.learn_glyph(&glyph.name, gid) {
                    self.mark_facts_dirty();
                }
            }
            // The answers to a `REQ_APP` and a `REQ_ICON`. Both are
            // copies, and neither reads a byte of what it copied.
            Frame::App(app) => {
                tracing::info!(
                    "sidecar {sidecar}: app {:?} -> {:?} flags {:#x}",
                    app.app_id,
                    app.name,
                    app.flags
                );
                self.facts.set_app(app);
                self.mark_facts_dirty();
            }
            Frame::Pixmap(pixmap) => {
                // Kept in the store under its key, and filed in a slot under
                // the key the build looks under when it answers something
                // asked (`Facts::learn_pixmap`). `insert` says why when it
                // refuses, so the only line here is the one that says it
                // landed.
                if let Some(slot) =
                    self.facts
                        .learn_pixmap(&mut self.images, sidecar, &pixmap.key, &pixmap.rgba)
                {
                    tracing::info!("sidecar {sidecar}: pixmap {:?} -> slot {slot}", pixmap.key);
                    self.mark_facts_dirty();
                }
            }
            // One entry point of a fresh compile. The bar reads no
            // `.slang` file and runs no compiler — this is SPIR-V on its way
            // to the device, collected until its generation is whole.
            Frame::Module(module) => self.apply_module(sidecar, module),
            other => {
                tracing::info!(
                    "sidecar {sidecar}: frame kind {} has no consumer yet",
                    other.kind()
                )
            }
        }
    }

    /// One `Module` frame — a piece of an entry point — and the swap
    /// when it completes a generation (docs/abi.md, *Module*).
    ///
    /// The swap is `Gpu::reload_all`'s: all-or-nothing,
    /// refused outright if the compile moved a binding, and it never touches
    /// the state buffer — so the bar keeps its popup, its menu level and its
    /// animations across it. The strings go with the modules or not at all: a
    /// hash in the mirror is only meaningful against the caustic that produced
    /// it.
    ///
    /// A failure here is a witness and the running caustic. Bad shader = wrong
    /// pixels, never a dead bar — and since the tunables are part of the caustic, bad *config* = the last good bar, never a dead one.
    fn apply_module(&mut self, sidecar: &str, chunk: wire::ModuleChunk) {
        let ready = match self.modules.collect(chunk) {
            Collected::Waiting {
                generation,
                have,
                count,
            } => {
                tracing::debug!("sidecar {sidecar}: generation {generation}, {have}/{count}");
                return;
            }
            Collected::Refused(why) => {
                tracing::warn!("sidecar {sidecar}: {why}; keeping the old bar");
                return;
            }
            Collected::Ready(ready) => ready,
        };
        let generation = ready.generation;
        let reflection = match ready.reflection() {
            Ok(reflection) => reflection,
            Err(err) => {
                tracing::error!(
                    "sidecar {sidecar}: generation {generation} refused: {err}; \
                     keeping the old bar"
                );
                return;
            }
        };
        let Some(gpu) = self.gpu.as_mut() else {
            tracing::warn!(
                "sidecar {sidecar}: generation {generation} arrived with no device; \
                 keeping the old bar"
            );
            return;
        };
        let started = Instant::now();
        let [build, compose, shape, apply, visibility, sample, draw] = ready.spirv();
        match gpu.reload_all(
            &reflection,
            build,
            compose,
            shape,
            apply,
            visibility,
            sample,
            draw,
        ) {
            Ok(()) => {
                // The modules are live, so the table that came with them is
                // the one the mirror's hashes belong to.
                self.strings.replace(reflection.strings);
                // ...and the witness printer's complaints are about a program
                // that is no longer running (docs/abi.md, *The witness ring*).
                self.printer.reset();
                // ...and the literal pool the next compose publishes is that
                // table's, which is what makes a reload's own words spellable
                // on the GPU. Nothing is reserved against it: the embassy
                // mints no key at all (docs/abi.md, *The facts*).
                // New modules may build a different table from the same
                // facts, so the model passes owe a run.
                gpu.mark_shape_pending();
                tracing::info!(
                    "sidecar {sidecar}: generation {generation}, {} modules, swapped \
                     ({} KiB, {} ms)",
                    ready.len(),
                    ready.bytes() / 1024,
                    started.elapsed().as_millis()
                );
                self.request_frame();
            }
            Err(err) => tracing::error!(
                "sidecar {sidecar}: generation {generation} refused: {err:#}; \
                 keeping the old bar"
            ),
        }
    }

    /// What the embassy knows, written down and uploaded (docs/abi.md
    /// *The facts*).
    ///
    /// One pass over the model into the facts tables, the bytes they point
    /// into, and the answers arena beside them. Nothing here decides anything:
    /// `bar_build` reads these and writes the item and block tables.
    fn mark_facts_dirty(&mut self) {
        self.facts
            .compose(&self.model, &self.strings, &mut self.images);
        self.send_requests();
        if let Some(gpu) = self.gpu.as_mut() {
            for (slot, pixels) in self.images.take_dirty() {
                gpu.upload_image_slot(slot, &pixels);
            }
            gpu.upload_facts(
                self.facts.buffer(),
                self.facts.strings(),
                self.facts.literals(),
                self.facts.answers(),
            );
        }
        // A rebuild that added the chart items — the first frame, or a new
        // chart set — must upload the frame they point at in the same breath,
        // and so must the one that follows the GPU coming up.
        self.upload_samples();
        self.dirty = true;
    }

    /// One `Rows` frame into its sidecar's region of the rows arena
    /// (docs/abi.md, *The arenas*), and one `CMD_ROWS`.
    ///
    /// **The whole of what the embassy does with a sidecar's rows.** The body
    /// is the frame's own bytes — header, rows and blob at the writer's
    /// offsets — and nothing in it is pixels: a pixmap icon is a key whose
    /// texels came as a `Pixmap` frame (docs/abi.md, *Pixmap icons*, *Pixmap*). Whether the frame is
    /// worth a model pass is the caustic's, from the command; a slider tick is
    /// one `write_buffer`, one command and one frame.
    fn publish_rows(&mut self, sidecar: &str) {
        let Some(slot) = self.facts.slot_of(sidecar) else {
            return;
        };
        let Some(rows) = self.model.applets.get(sidecar).cloned() else {
            return;
        };
        let body = match self.facts.rows_region(slot, &rows) {
            Ok(body) => body,
            Err(len) => {
                tracing::warn!(
                    "sidecar {sidecar}: rows frame of {len} bytes does not fit the arena's {}",
                    abi::ROWS_ARENA_BYTES
                );
                return;
            }
        };
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.write_rows_region(slot, &body);
        }
        self.queue(Command::rows(slot));
        self.dirty = true;
    }

    /// A sidecar that exited or was replaced: zero its region's header, so the
    /// build pass skips the slot, and say so with the same `CMD_ROWS`.
    fn forget_rows(&mut self, sidecar: &str) {
        let Some(slot) = self.facts.slot_of(sidecar) else {
            return;
        };
        self.facts.forget_slot(slot);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.zero_rows_header(slot);
        }
        self.queue(Command::rows(slot));
        self.dirty = true;
    }

    /// Every name the build wants resolved, out to the sidecar that resolves
    /// it (docs/abi.md, *Request*).
    ///
    /// The bar never waits: the answer is an `App`, a `Pixmap` or a `Glyph` on
    /// that child's stdout like any other frame, and the rebuild it triggers
    /// is the one that draws it. A sidecar that is not running is one witness
    /// inside `Feeders::request` and a dock entry wearing its app id, which is
    /// exactly what a bar with no `caustics-apps` beside it looks like.
    fn send_requests(&mut self) {
        for (sidecar, kind, name) in self.facts.take_requests() {
            self.feeders.request(sidecar, kind, &name);
        }
    }

    /// The latest sidecar frame to binding 10, exactly as it arrived on the
    /// child's stdout. Nothing here reads a row.
    fn upload_samples(&mut self) {
        if let Some(frame) = self.model.samples.as_deref()
            && let Some(gpu) = self.gpu.as_mut()
        {
            gpu.upload_samples(abi::as_bytes(frame));
        }
    }

    /// The one summary line the caustic cannot push: **no frame at all**.
    ///
    /// `dock:`, `applets:` and `charts: N laid out` are `bar_build`'s
    /// witnesses (docs/abi.md, *The witness ring*): it counts what it built. What is left here is
    /// the fact only this side has: a sidecar that has never sent a `Samples`
    /// frame is not the same thing as a sidecar that sent one with no rows,
    /// and the caustic reads a zeroed buffer for both.
    fn log_charts(&mut self) {
        let have = self.model.samples.is_some();
        if have == self.logged_charts {
            return;
        }
        self.logged_charts = have;
        if !have {
            tracing::info!("charts: none (no frame from the sidecar)");
        }
    }

    /// Re-read the wall clock and publish its three numbers. The minute timer
    /// calls this, and `main` once at startup.
    ///
    /// **Not through `apply_update`**, because a minute is not a rebuild. The
    /// clock is three words of `FrameInputs` and two
    /// composed blocks whose bytes the caustic writes, so nothing about the
    /// item table, the blocks or the strings they name changes when the minute
    /// does: what is owed is one compose-and-shape submission and one frame,
    /// not a whole table rebuilt sixty times an hour to move two digits.
    pub fn tick_clock(&mut self) {
        let (time, date, month) = clock::clock_numbers(std::time::SystemTime::now());
        if !self.model.apply(Update::Clock(time, date, month)) {
            return;
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.mark_shape_pending();
        }
        self.dirty = true;
    }

    /// `ACT_ANCHOR`: the caustic has told the embassy which edge and how tall
    /// (docs/abi.md, *The action ring*). Re-anchoring sits on the configure/acknowledge
    /// round trip, which is why it is the embassy's and not the caustic's.
    ///
    /// **A message, not a word polled every frame.** The caustic has the
    /// "have" side in `inputs.edge`, so it compares for itself and pushes
    /// once — on its first frame, and whenever a reload changes either. What is left here is the
    /// half only the embassy can do: taking the edge lock, asking the
    /// compositor for the surface again, and saying so.
    fn reanchor_to(&mut self, edge: u32, height: u32) {
        let Some(want_edge) = Edge::from_abi(edge) else {
            tracing::warn!("tunables: EDGE is {edge}, which is no edge this bar has");
            return;
        };
        let want_height = height.max(1);
        if want_edge != self.edge || want_height != self.height || !self.tunables_seen {
            if want_edge != self.edge || want_height != self.height {
                self.reanchor(want_edge, want_height);
            }
            self.tunables_seen = true;
            tracing::info!(
                "tunables: applied (edge={} height={})",
                self.edge.name(),
                self.height
            );
        }
    }

    /// The caustic wants a different edge or height: ask the compositor for
    /// the surface again.
    ///
    /// An edge change also moves the lock. The rule is a legible refusal
    /// rather than a fight: take the new edge's lock first, and if another bar
    /// holds it, say whose edge it is and stay on the current edge — the tunable
    /// asked for something this session cannot grant.
    fn reanchor(&mut self, edge: Edge, height: u32) {
        if edge != self.edge {
            match crate::take_edge_lock(edge) {
                Some(lock) => {
                    // The old lock is released by dropping it, and only after
                    // the new one is held: no window where neither is taken.
                    self.edge_lock = Some(lock);
                }
                None => {
                    tracing::error!(
                        "tunables: EDGE asks for the {} edge, which another caustics-bar holds; \
                         staying on the {} edge",
                        edge.name(),
                        self.edge.name()
                    );
                    return;
                }
            }
        }
        self.edge = edge;
        self.height = height;
        self.layer.set_anchor(anchor_for(edge));
        self.layer.set_size(0, height);
        self.layer.set_exclusive_zone(height as i32);
        self.layer.commit();
        tracing::info!(
            "bar: re-anchored to the {} edge, {height} logical px",
            edge.name()
        );
    }

    // -- the loop ----------------------------------------------------------

    /// Everything the event loop must do after a dispatch: bring surfaces up,
    /// act on the mirror's popup state, upload a preview, re-arm the capture
    /// timer, then draw if anything asked for a frame.
    pub fn after_dispatch(&mut self) -> Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        self.service_surfaces()?;
        self.service_preview();
        self.arm_capture();
        if self.dirty && self.frame_allowed() {
            self.render()?;
        }
        Ok(())
    }

    /// May a dirty bar draw right now? Yes when no frame request is out;
    /// otherwise the frame callback is the pacer — with one escape: a
    /// compositor that stops answering frame callbacks (an occluded surface,
    /// a compositor bug) must not freeze the bar's *input* handling, so a
    /// request older than [`FRAME_STALL`] is treated as answered.
    fn frame_allowed(&self) -> bool {
        !self.frame_pending || self.frame_requested_at.elapsed() >= FRAME_STALL
    }

    /// The one timer the bar arms on a feeder's behalf. The capture feeder
    /// wants a still every `capture::MIN_INTERVAL` while a session lives and
    /// says so through `next_deadline`; nothing else in the bar polls.
    fn arm_capture(&mut self) {
        let deadline = self.feeders.next_deadline();
        if deadline == self.capture_deadline {
            return;
        }
        self.capture_deadline = deadline;
        let Some(handle) = self.loop_handle.clone() else {
            return;
        };
        if let Some(token) = self.capture_token.take() {
            handle.remove(token);
        }
        let Some(deadline) = deadline else {
            return;
        };
        let inserted =
            handle.insert_source(Timer::from_deadline(deadline), |_, _, bar: &mut Bar| {
                // Dropped here, re-armed by `after_dispatch`: removing a source
                // from inside its own callback is what `TimeoutAction::Drop` is.
                bar.capture_token = None;
                bar.capture_deadline = None;
                bar.feeders.tick();
                TimeoutAction::Drop
            });
        match inserted {
            Ok(token) => self.capture_token = Some(token),
            Err(err) => tracing::warn!("could not arm the capture timer: {err}"),
        }
    }

    /// Bring wgpu up, or resize it, for whatever the last configure asked
    /// for — **outside** the Wayland dispatch that delivered it.
    ///
    /// This deferral is load-bearing, not tidiness. Mesa's Wayland WSI does a
    /// `wl_display_roundtrip` of its own while answering
    /// `vkGetPhysicalDeviceSurfaceSupportKHR`, and wayland-rs holds the
    /// connection's lock for the duration of a dispatch. Creating the surface
    /// from inside `LayerShellHandler::configure` therefore deadlocks: the bar
    /// hangs silently after logging the configure and never reaches the
    /// adapter. It is intermittent by construction — whether it hangs depends
    /// on what else is pending on the queue — which is exactly the sort of
    /// bug that would otherwise show up once a month on somebody's machine.
    pub fn service_surfaces(&mut self) -> Result<()> {
        if let Some((width, height)) = self.pending_surface.take() {
            match self.gpu.as_mut() {
                Some(gpu) => gpu.resize_bar(width, height),
                None => {
                    // No swapchain means no bar at all; there is nothing to
                    // fall back to and the unit should restart the bar.
                    if let Err(err) = self.bring_up_gpu(width, height) {
                        self.exit = Some(Exit::Protocol(format!("wgpu bring-up failed: {err:#}")));
                        return Ok(());
                    }
                    self.mark_facts_dirty();
                }
            }
            self.dirty = true;
        }
        if let Some((width, height)) = self.pending_popup.take() {
            // A popup that already has a swapchain was *repositioned*, not
            // created: a submenu descent changes the size of a mapped popup.
            let attached = self
                .gpu
                .as_ref()
                .is_some_and(|gpu| gpu.popup_size() != (0, 0));
            if attached {
                if let Some(gpu) = self.gpu.as_mut()
                    && gpu.resize_popup(width, height)
                {
                    self.queue(Command::popup_mapped(width, height));
                }
            } else {
                self.attach_popup_surface(width, height);
            }
            self.dirty = true;
        }
        Ok(())
    }
    /// `ACT_POPUP_OPEN`: a new `xdg_popup` of the kind the caustic named,
    /// anchored at the rect it gave, the size it asked for
    /// (docs/abi.md, *The action ring*).
    ///
    /// **A message, not polled state.** Popup words polled every frame would
    /// need a guard here to stop the request firing again between the ask and
    /// the map. There is nothing to guard: an open is pushed once, rule 1 (docs/abi.md, *The three rules the buffer adds*) is what makes that safe, and this side's only state is the surface it
    /// owns.
    ///
    /// A request arriving while a popup is up is the **replace** case — the
    /// preview-then-right-click: the dwell fires a preview on any entry with a
    /// window, so by the time a right click lands it usually is up. The
    /// caustic has already ended the capture session and moved its own state
    /// machine onto the new popup, so the old surface goes *without* a
    /// `CMD_POPUP_DONE`: sending one would tell the caustic that the popup it
    /// is waiting for had closed.
    fn open_popup_action(&mut self, open: abi::PopupOpen) -> Result<()> {
        if self.popup.is_some() {
            self.replace_popup();
        }
        if open.kind == abi::POPUP_MENU {
            self.prepare_menu(open.slot, open.row);
        }
        let want = [open.want[0] as f32, open.want[1] as f32];
        self.open_popup(open.kind, open.anchor, want)
    }

    /// `ACT_POPUP_MOVE`: the caustic's want has moved past what the compositor
    /// gave it — a menu whose rows have just arrived, or a submenu descent.
    ///
    /// The comparison is the *caustic's*: it has the live size in
    /// `inputs.popup_size` and keeps what it last asked for in its own buffer,
    /// so this arrives only when the two differ.
    fn move_popup(&mut self, size: [u32; 2]) {
        if self.popup.is_none() {
            return;
        }
        let want = [size[0] as f32, size[1] as f32];
        self.reposition_popup(want);
    }

    /// A menu popup was asked for: tell the sidecar whose rows it is.
    ///
    /// **The rows are not this side's business.** The caustic holds its menu
    /// level and `bar_build` publishes the rows of the level it holds, so
    /// there is nothing to fetch — `ACT_REBUILD` is what asks for the pass to
    /// run, and `render` answers it exactly as it answers a clock tick.
    ///
    /// What is left is the notice: an applet's menu opening is a fact its
    /// sidecar wants told (docs/abi.md, *Action*), and a dock entry's menu has
    /// nobody at the other end — which is what `slot == NO_INDEX` says, on the
    /// side that knows, rather than an id tag decoded here.
    fn prepare_menu(&mut self, slot: u32, row: u32) {
        if slot == abi::NO_INDEX {
            return;
        }
        let Some(name) = self.facts.sidecar(slot).map(str::to_owned) else {
            tracing::info!("popup: menu asked for slot {slot}, which is no sidecar's");
            return;
        };
        self.open_menu_notice = Some((name.clone(), row));
        // The sidecar is told its menu opened before its rows reach the table,
        // so a frame answering the notice finds the menu up.
        self.notify_menu(&name, row, abi::ACTION_MENU_OPEN);
    }

    /// One `ACTION_MENU_OPEN` or `ACTION_MENU_CLOSE` to the sidecar that owns a menu
    /// (docs/abi.md, *Action*). **These two kinds are the embassy's**: they are
    /// facts about the popup it owns, and the caustic never pushes one.
    ///
    /// Straight to the pipe feeder rather than through [`Self::perform`]: no
    /// other owner could claim it, and `perform`'s INFO line per menu open
    /// would be journal noise for a notice most sidecars ignore. The one
    /// witness is DEBUG under `caustics_bar::menu`.
    fn notify_menu(&mut self, name: &str, row: u32, kind: u32) {
        tracing::debug!(
            target: "caustics_bar::menu",
            sidecar = name,
            row,
            what = if kind == abi::ACTION_MENU_OPEN { "opened" } else { "closed" },
            "menu"
        );
        let action = Action::Sidecar {
            name: name.to_owned(),
            body: abi::wire::ActionBody {
                row,
                kind,
                ..abi::wire::ActionBody::default()
            },
        };
        let _ = self.feeders.perform(&action);
    }

    /// Tell the sidecar whose menu was open that it closed, if there was one.
    ///
    /// Takes the notice, so the close is sent exactly once however many ways
    /// the popup's life ends — and so the bar cannot tell a sidecar its menu
    /// closed twice.
    fn menu_closed(&mut self) {
        if let Some((name, row)) = self.open_menu_notice.take() {
            self.notify_menu(&name, row, abi::ACTION_MENU_CLOSE);
        }
    }

    /// The positioner for a popup, with every value the caustic supplied
    /// clamped: a zero-size anchor or a want of 0×0 is a protocol error, and
    /// the embassy never lets one reach the compositor (docs/architecture.md, *Rules*). Returns
    /// the positioner and the logical size actually asked for.
    fn positioner(
        &self,
        anchor_px: [f32; 4],
        want_px: [f32; 2],
    ) -> Result<(XdgPositioner, i32, i32)> {
        use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner::{
            Anchor as XdgAnchor, ConstraintAdjustment, Gravity,
        };

        let scale = self.scale.max(1) as f32;
        let bar_w = self.bar_logical.0.max(1) as i32;
        let bar_h = self.bar_logical.1.max(1) as i32;

        let mut x = (anchor_px[0] / scale).round() as i32;
        let mut y = (anchor_px[1] / scale).round() as i32;
        let mut w = (anchor_px[2] / scale).round() as i32;
        let mut h = (anchor_px[3] / scale).round() as i32;
        // Inside the bar's logical rect, and never smaller than one px.
        x = x.clamp(0, (bar_w - 1).max(0));
        y = y.clamp(0, (bar_h - 1).max(0));
        w = w.clamp(1, bar_w - x);
        h = h.clamp(1, bar_h - y);

        let (want_w, want_h) = self.logical_want(want_px);

        let positioner =
            XdgPositioner::new(&self.xdg_shell).context("could not create an xdg_positioner")?;
        positioner.set_size(want_w, want_h);
        positioner.set_anchor_rect(x, y, w, h);
        // A bar on the bottom edge puts its popups *above* the anchor, and one
        // on the top edge below it. The caustic gets the same fact through
        // `FrameInputs::edge` and uses it for the focus underline; here it is
        // the popup's gravity.
        let (anchor, gravity) = match self.edge {
            Edge::Bottom => (XdgAnchor::Top, Gravity::Top),
            Edge::Top => (XdgAnchor::Bottom, Gravity::Bottom),
        };
        positioner.set_anchor(anchor);
        positioner.set_gravity(gravity);
        positioner
            .set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::FlipY);
        Ok((positioner, want_w, want_h))
    }

    /// A `popup_want` in bar px, clamped into the logical size a positioner
    /// will accept.
    fn logical_want(&self, want_px: [f32; 2]) -> (i32, i32) {
        let scale = self.scale.max(1) as f32;
        (
            ((want_px[0] / scale).round() as i32).clamp(1, 4096),
            ((want_px[1] / scale).round() as i32).clamp(1, 4096),
        )
    }

    /// Create the popup, take the grab if it is a menu, and commit.
    ///
    /// The grab is what dismisses a menu when the pointer goes elsewhere, and
    /// it goes in **before the first commit, every time**: `xdg_popup.grab`
    /// after the popup is mapped is a protocol error (docs/architecture.md, *Why it is
    /// shaped this way*). A
    /// preview must not take one: a grabbed popup swallows the pointer, and
    /// the whole point of a hover preview is that the pointer keeps moving
    /// along the dock underneath it.
    fn open_popup(&mut self, kind: u32, anchor_px: [f32; 4], want_px: [f32; 2]) -> Result<()> {
        let (positioner, want_w, want_h) = self.positioner(anchor_px, want_px)?;

        let surface = Surface::new(&self.compositor, &self.qh)
            .context("could not create the popup's wl_surface")?;
        surface.wl_surface().set_buffer_scale(self.scale as i32);
        let popup = Popup::from_surface(None, &positioner, &self.qh, surface, &self.xdg_shell)
            .context("could not create the xdg_popup")?;
        // The parent is the layer surface, not an xdg_surface, so this is what
        // stands in for `from_surface`'s parent argument. It must happen
        // before the first commit or the compositor answers with
        // `invalid_popup_parent`.
        self.layer.get_popup(popup.xdg_popup());

        let grabbed = if kind == abi::POPUP_MENU {
            match (&self.seat, self.last_button_serial) {
                (Some(seat), serial) if serial != 0 => {
                    popup.xdg_popup().grab(seat, serial);
                    true
                }
                _ => {
                    tracing::warn!("no pointer serial yet; the menu opens without a grab");
                    false
                }
            }
        } else {
            false
        };
        popup.wl_surface().commit();

        tracing::info!(
            "popup: requested kind={} anchor={},{},{},{} size={}x{} grab={}",
            popup_kind_name(kind),
            anchor_px[0] as i32,
            anchor_px[1] as i32,
            anchor_px[2] as i32,
            anchor_px[3] as i32,
            want_w,
            want_h,
            grabbed,
        );
        self.popup = Some(popup);
        self.popup_kind = kind;
        self.popup_anchor = anchor_px;
        self.popup_want = (want_w, want_h);
        self.popup_configured = false;
        Ok(())
    }

    /// Drop the live popup's surface because the caustic has asked for another
    /// one, and **without** telling the caustic it closed.
    ///
    /// [`Self::destroy_popup`]'s twin, and deliberately not a flag on it: that
    /// one ends a popup's life and queues `CMD_POPUP_DONE`, which resets the
    /// caustic's popup state and its scratch. Here the caustic is already one
    /// step ahead — it has set the kind, the id and the menu level for the
    /// popup it wants — so a `popup_done` would wipe exactly that. The rows of
    /// whatever menu was open are left for `prepare_menu` to replace.
    fn replace_popup(&mut self) {
        // The popup is going, so whatever menu it was is over — the sidecar
        // hears that even though the caustic's own state machine has moved on
        // (docs/abi.md, *Action*).
        self.menu_closed();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.drop_popup();
        }
        let was = popup_kind_name(self.popup_kind);
        if self.popup.take().is_some() {
            tracing::info!("popup: replaced (a {was} popup the caustic has moved on from)");
        }
        self.popup_configured = false;
        self.popup_closing = false;
        self.popup_kind = 0;
        self.popup_want = (0, 0);
    }

    /// Resize a mapped popup in place with `xdg_popup.reposition` (xdg_wm_base
    /// v3; sctk binds v6). Destroying and recreating would work too, but it
    /// would also need a *fresh* button serial for the menu's grab, and there
    /// is none — the click that opened the menu is the newest serial there is.
    fn reposition_popup(&mut self, want_px: [f32; 2]) {
        let anchor_px = self.popup_anchor;
        let (positioner, want_w, want_h) = match self.positioner(anchor_px, want_px) {
            Ok(parts) => parts,
            Err(err) => {
                tracing::warn!("popup: could not reposition: {err:#}");
                return;
            }
        };
        self.popup_serial = self.popup_serial.wrapping_add(1);
        self.popup_want = (want_w, want_h);
        if let Some(popup) = self.popup.as_ref() {
            popup.reposition(&positioner, self.popup_serial);
            tracing::info!("popup: repositioned to {want_w}x{want_h}");
        }
    }

    /// Tear the popup down in the one safe order: the wgpu surface (which
    /// holds a raw pointer to the `wl_surface`), then the popup itself, then
    /// tell the caustic. A popup object is never reused.
    fn destroy_popup(&mut self, why: &str) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.drop_popup();
        }
        let had = self.popup.take().is_some();
        self.popup_configured = false;
        self.popup_closing = false;
        self.popup_kind = 0;
        self.popup_want = (0, 0);
        if had {
            tracing::info!("popup: closed ({why})");
        }
        self.queue(Command::popup_done());
        // The sidecar hears that its menu closed before its rows leave the
        // table, whichever of the several ways this popup's life ended.
        self.menu_closed();
        // The rows belong to the popup: with it gone the caustic drops its
        // menu level, `bar_apply` says `wants_rebuild`, and they leave the
        // table on the next frame. Nothing here decides that.
    }

    // -- the preview -------------------------------------------------------

    /// A captured still into the preview slot. Straight RGBA in, premultiplied
    /// texels out: the caustic composites with `over` and never
    /// unpremultiplies, so the conversion happens exactly here.
    fn service_preview(&mut self) {
        if !std::mem::take(&mut self.preview_pending) {
            return;
        }
        let Some(preview) = self.model.preview.as_ref() else {
            self.preview_size = [0, 0];
            return;
        };
        let (window, width, height) = (preview.window, preview.width, preview.height);
        premultiply_into(&preview.rgba, &mut self.preview_scratch);

        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        if gpu.upload_preview(&self.preview_scratch, width, height) {
            self.preview_size = [width, height];
            tracing::info!("preview: window {window} {width}x{height}");
        } else {
            self.preview_size = [0, 0];
            tracing::warn!("preview: window {window} {width}x{height} does not fit the slot");
        }
        self.dirty = true;
    }
    // -- the two rings -----------------------------------------------------

    /// Which records of a ring are new, given what was taken of it last time.
    ///
    /// A ring belongs to the frame stamped on it (docs/abi.md, *The three rules the buffer adds*, rule 1),
    /// and a frame that failed at the swapchain keeps its stamp, so the next
    /// attempt *appends* to what it already pushed. So "what is new" is: the
    /// whole ring when the stamp moved, and the tail past the last count when
    /// it did not. Nothing is acknowledged and nothing is replayed.
    fn undrained(
        frame: u32,
        count: u32,
        taken: &mut (u32, u32),
        capacity: usize,
    ) -> (usize, usize) {
        let count = (count as usize).min(capacity);
        let start = if taken.0 == frame {
            (taken.1 as usize).min(count)
        } else {
            0
        };
        *taken = (frame, count as u32);
        (start, count)
    }

    /// Every decision the caustic made this frame, forwarded.
    ///
    /// **Forwarded, not resolved** (docs/abi.md, *The action ring*): a window id is the compositor's own, an app id is a range of the strings buffer this side
    /// uploaded, a sidecar action is the wire body's words, a popup is its
    /// geometry. There is no table lookup behind any of it and nothing for a stale id to name wrongly.
    fn drain_actions(&mut self, mirror: &State) {
        let surface = mirror.surface;
        if surface.action_lost != self.logged_action_lost {
            self.logged_action_lost = surface.action_lost;
            if surface.action_lost > 0 {
                tracing::warn!(
                    "actions: {} past the ring's {}; something upstream is pushing per texel",
                    surface.action_lost,
                    abi::MAX_ACTIONS
                );
            }
        }
        if surface.action_frame == 0 {
            return;
        }
        let (start, end) = Bar::undrained(
            surface.action_frame,
            surface.action_count,
            &mut self.drained_actions,
            abi::MAX_ACTIONS,
        );
        for note in mirror.actions[start..end].to_vec() {
            self.forward(&note);
        }
    }

    /// One record of the action ring, handed to whoever owns it.
    ///
    /// One arm per `ACT_*`, each a read of the operands docs/abi.md, *The action ring*, names and a call to `perform`, `launch` or `set_pinned`.
    fn forward(&mut self, note: &Note) {
        match note.head {
            abi::ACT_ACTIVATE_WINDOW => self.perform(Action::Activate(note.window_id())),
            abi::ACT_CLOSE_WINDOW => self.perform(Action::Close(note.window_id())),
            abi::ACT_PREVIEW_BEGIN => self.perform(Action::PreviewWanted(Some(note.window_id()))),
            abi::ACT_PREVIEW_END => self.perform(Action::PreviewWanted(None)),
            abi::ACT_SPAWN | abi::ACT_PIN | abi::ACT_UNPIN => {
                let (offset, len) = note.string_range();
                let Some(app_id) = self.facts.text_at(offset, len) else {
                    tracing::info!(
                        "action dropped: kind {} names bytes {offset}..{} of a strings buffer \
                         that is not this one",
                        note.head,
                        offset as usize + len as usize
                    );
                    return;
                };
                self.perform(match note.head {
                    abi::ACT_SPAWN => Action::Spawn { app_id },
                    abi::ACT_PIN => Action::Pin { app_id },
                    _ => Action::Unpin { app_id },
                });
            }
            abi::ACT_TOGGLE_OVERVIEW => self.perform(Action::ToggleOverview),
            abi::ACT_TOGGLE_CAFFEINE => self.perform(Action::ToggleCaffeine),
            abi::ACT_SIDECAR => {
                let args = note.sidecar();
                let Some(name) = self.facts.sidecar(args.slot).map(str::to_owned) else {
                    tracing::info!("action dropped: slot {} is no sidecar's", args.slot);
                    return;
                };
                // The one conversion on this path, and it is a unit change the
                // wire specifies: `pos` is physical px in the target the click
                // landed on, and an `ActionBody` carries logical px because
                // that is what SNI's `Activate(x, y)` takes at the far end.
                let scale = self.scale.max(1) as f32;
                // Only for a row that asked (`RFLAG_ACTIVATION`, which the
                // caustic read off the row and put in `token`). The row's flag is the whole gate.
                if args.kind == wire::ACTION_ACTIVATE && args.token {
                    self.ask_activation(&name, args.row);
                }
                self.perform(Action::Sidecar {
                    name,
                    body: wire::ActionBody {
                        row: args.row,
                        kind: args.kind,
                        button: args.button,
                        pos: [args.pos[0] / scale, args.pos[1] / scale],
                        value: args.value,
                        ..wire::ActionBody::default()
                    },
                });
            }
            abi::ACT_POPUP_OPEN => {
                if let Err(err) = self.open_popup_action(note.popup_open()) {
                    tracing::warn!("popup: could not open: {err:#}");
                }
            }
            abi::ACT_POPUP_MOVE => self.move_popup(note.popup_size()),
            abi::ACT_POPUP_CLOSE => {
                if !self.popup_closing {
                    self.popup_closing = true;
                    self.destroy_popup("the caustic asked for it");
                }
            }
            abi::ACT_ANCHOR => {
                let (edge, height) = note.anchor();
                self.reanchor_to(edge, height);
            }
            // The tunables' shape-pass threshold (`SHAPE_WATCHDOG_MS`). One
            // number handed to the side that holds the query set; what happens
            // when a pass runs over it is fixed and is `gpu.rs`'s.
            abi::ACT_WATCHDOG => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.set_watchdog_ms(note.watchdog_ms());
                }
            }
            // The one rebuild this side cannot see coming: the caustic's menu
            // level has moved past the table it was built for. Answered
            // exactly as a clock tick is — mark the model passes pending, ask
            // for a frame — because the facts have not changed and nothing
            // here decides which rows are owed.
            abi::ACT_REBUILD => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.mark_shape_pending();
                    self.dirty = true;
                }
            }
            abi::ACT_COVER => self.cover(note.cover()),
            // What the build drew: the caustic's mask, copied whole into the
            // slot array, because only the caustic knows what is on the bar
            // (docs/architecture.md, *Images and icons*). It arrives before the
            // build's wants, so a want that evicts never takes a drawn slot.
            abi::ACT_IMAGES_DRAWN => self.images.touch_drawn(note.images_drawn()),
            // The store answers first: a key a sidecar already sent lands a
            // slot, and the compose that names it is what the facts owe.
            abi::ACT_WANT_ICON => match self.facts.want_icon(&mut self.images, note.want_icon()) {
                Wanted::Landed => self.mark_facts_dirty(),
                Wanted::Asked => self.send_requests(),
                Wanted::Nothing => {}
            },
            other => tracing::info!("action dropped: kind {other} has no forwarder"),
        }
    }

    /// Every witness the caustic pushed this frame, printed in order.
    ///
    /// This side dedups nothing: "once per change" is the caustic's duty, and
    /// it discharges it by comparing against what it persisted in its own
    /// buffer (docs/abi.md, *The witness ring*). `witness.rs` resolves the format
    /// literal through the running generation's string table and substitutes.
    fn drain_witnesses(&mut self, mirror: &State) {
        let surface = mirror.surface;
        if surface.witness_lost != self.logged_witness_lost {
            self.logged_witness_lost = surface.witness_lost;
            if surface.witness_lost > 0 {
                tracing::warn!(
                    "witness lost: {} past the ring's {}",
                    surface.witness_lost,
                    abi::MAX_WITNESSES
                );
            }
        }
        if surface.witness_frame == 0 {
            return;
        }
        let (start, end) = Bar::undrained(
            surface.witness_frame,
            surface.witness_count,
            &mut self.drained_witnesses,
            abi::MAX_WITNESSES,
        );
        for note in mirror.witnesses[start..end].to_vec() {
            self.printer
                .print(&note, &self.strings, self.facts.strings());
        }
    }

    /// `ACT_COVER`: a text block whose run the shaper could not draw, out to
    /// `caustics-glyphs` as the **bytes** it was built from (docs/abi.md, *Request*), so a fallback
    /// face can be found for it (docs/architecture.md, *Text*).
    ///
    /// The range is the caustic's and so is the buffer it names — a title in
    /// `strings`, a sidecar's row label in its slot's region, an app's name in
    /// `answers` — read against the copy this side
    /// uploaded, which is the one that was live when the block was shaped (docs/abi.md, *The two contracts*,
    /// rule 2). Nothing here decodes it as text: the `String` is the wire's
    /// own field type and this side never looks at a character of it. One
    /// request per distinct string for the life of the process
    /// (`Facts::ask_cover`); the answer is the curve buffers and the `cmap`
    /// growing, the run re-shaping and the flag going away by itself.
    fn cover(&mut self, cover: abi::Cover) {
        if let Some(text) = self.facts.cover(cover) {
            tracing::info!("text: asking caustics-glyphs to cover {text:?}");
            self.send_requests();
        }
    }

    /// Hand one action to whoever owns it. A table lookup: the feeders claim
    /// theirs — the compositor seam's, the capture session's, and every
    /// sidecar's — and what is left over is the dock's favourites, which are
    /// a file and belong to no connection.
    ///
    /// [`Action::Spawn`] is the one that is neither: the app id has to become
    /// an argv before anything can start it, and reading the desktop entry is
    /// I/O the embassy is the right place for. It resolves here and leaves
    /// immediately as an [`Action::Launch`].
    fn perform(&mut self, action: Action) {
        if matches!(action, Action::PreviewWanted(None)) {
            // The still on screen is about to be stale; stop drawing it in
            // the same frame the session is torn down.
            self.preview_size = [0, 0];
            self.dirty = true;
        }
        if self.feeders.perform(&action) {
            let owner = action_owner(&action, self.feeders.compositor.name());
            tracing::info!("action: {action:?} -> {owner}");
            return;
        }
        let owner = action_owner(&action, self.feeders.compositor.name());
        match &action {
            Action::Spawn { app_id } => {
                // `launch` logs its own line, with the argv it resolved.
                let app_id = app_id.clone();
                self.launch(&app_id);
                return;
            }
            Action::Pin { app_id } | Action::Unpin { app_id } => {
                tracing::info!("action: {action:?} -> {owner}");
                let pin = matches!(action, Action::Pin { .. });
                let app_id = app_id.clone();
                self.set_pinned(&app_id, pin);
                return;
            }
            _ => {}
        }
        tracing::info!("action dropped: {action:?} (nothing claimed it)");
    }

    /// Pin or unpin one app id: change the dock's favourites, write the file,
    /// and rebuild so the dock shows it.
    ///
    /// A pin appends — the favourites are an order, and the newly pinned entry
    /// goes where a person who pinned it would look for it, at the end of the
    /// pinned run rather than wherever the app happened to be running. An
    /// unpinned app with no window leaves the dock on this very rebuild; a
    /// pinned one stays when its last window closes.
    fn set_pinned(&mut self, app_id: &str, pin: bool) {
        let present = self.model.favourites.iter().any(|fav| fav == app_id);
        if present == pin {
            tracing::info!(
                "favourites: {app_id:?} is already {}; nothing written",
                if pin { "pinned" } else { "unpinned" }
            );
            return;
        }
        if pin {
            self.model.favourites.push(app_id.to_owned());
        } else {
            self.model.favourites.retain(|fav| fav != app_id);
        }
        crate::favourites::store(&self.favourites_path, &self.model.favourites);
        self.mark_facts_dirty();
    }

    /// Resolve a dock entry's app id to an argv and ask the compositor to
    /// start it.
    ///
    /// `gio launch` is preferred when it is there because it handles the
    /// Flatpak exports (the exported `*.desktop` files are wrappers whose
    /// `Exec=` is a `flatpak run` line, and gio knows the D-Bus activation
    /// path too). Without it the entry's own `Exec=` is the argv — already
    /// split into words with the field codes stripped by `caustics-apps`, which is
    /// so the bar holds no desktop-entry parser. Neither is ever run through
    /// a shell.
    ///
    /// Both are fields of the `App` frame (docs/abi.md, *App*), so this reads a
    /// table and never a file: the entry was resolved when the dock drew it.
    ///
    /// The bar does not fork the child: the compositor is the process whose
    /// job starting clients is, it puts the child in its own scope with the
    /// session's environment, and the bar owns no child but its sidecars.
    fn launch(&mut self, app_id: &str) {
        let Some(app) = self.facts.app(app_id) else {
            tracing::info!(
                "action dropped: Spawn {app_id:?} (caustics-apps has not answered for it yet)"
            );
            return;
        };
        if app.flags & wire::APP_MISSING != 0 {
            tracing::info!("action dropped: Spawn {app_id:?} (no .desktop file for it)");
            return;
        }
        // `gio` is looked up per launch rather than once at startup: it costs
        // one `stat` on a click, and a bar that outlives its installation
        // should not be the reason a launch fails.
        let argv = if std::path::Path::new(GIO).is_file() && !app.path.is_empty() {
            vec![GIO.to_owned(), "launch".to_owned(), app.path.clone()]
        } else if !app.exec.is_empty() {
            app.exec.clone()
        } else {
            tracing::info!(
                "action dropped: Spawn {app_id:?} ({} has no Exec=)",
                app.path
            );
            return;
        };

        let owner = self.feeders.compositor.name();
        let action = Action::Launch { argv };
        if self.feeders.perform(&action) {
            let Action::Launch { argv } = &action else {
                unreachable!()
            };
            tracing::info!("action: Spawn {{ app_id: {app_id:?} }} -> {owner} (argv {argv:?})");
        } else {
            // No implementation behind the seam at all — a bar on a compositor
            // whose IPC nobody has written. There is nothing left to try, and
            // forking the child here instead is deliberately not an option.
            tracing::info!("action dropped: Launch (no compositor to spawn it)");
        }
    }

    // -- the frame ---------------------------------------------------------

    /// One frame. Everything the CPU knows goes in; the two rings come out,
    /// and what the caustic decided in them is carried out before the next
    /// dispatch.
    fn render(&mut self) -> Result<()> {
        let Some(gpu) = self.gpu.as_mut() else {
            self.dirty = false;
            return Ok(());
        };
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        // The pacing request goes out *before* the present so it rides the
        // commit Mesa makes inside `vkQueuePresentKHR`; a request after it
        // would wait for a commit that only the next frame makes. Same thread
        // as Mesa's commit, so there is no race with it.
        let wl_surface = self.layer.wl_surface();
        wl_surface.frame(&self.qh, wl_surface.clone());
        self.frame_pending = true;
        self.frame_requested_at = now;

        let bar_size = gpu.bar_size();
        let popup_size = gpu.popup_size();
        let inputs = FrameInputs {
            // Filled in by `Gpu::render`, which owns the stamp: it has to be
            // the same for the shape submission and the frame's, and it may
            // only advance once the readback has completed (*The three rules the buffer adds*, rule 1).
            frame: 0,
            dt,
            time: (now - self.started).as_secs_f32(),
            command_count: self.commands.len().min(abi::MAX_COMMANDS) as u32,
            scale: self.scale.max(1),
            // The in-process inhibitor's state, which is a Wayland object this
            // side owns and not a fact about content (docs/abi.md, *Structs*):
            // a toggle is one frame, never a rebuild.
            caffeine: match self.model.caffeine {
                crate::model::Caffeine::Absent => abi::CAFFEINE_ABSENT,
                crate::model::Caffeine::Off => abi::CAFFEINE_OFF,
                crate::model::Caffeine::On => abi::CAFFEINE_ON,
            },
            bar_size: [bar_size.0, bar_size.1],
            popup_size: [popup_size.0, popup_size.1],
            // The edge the surface *is* anchored to. The caustic draws against
            // what is and asks for what it wants with one `ACT_ANCHOR`, and
            // the two agree from the frame after the re-anchor.
            edge: self.edge.abi(),
            preview_size: self.preview_size,
            // The output the bar is on, in the surface's px: the caustic
            // clamps a menu to it.
            output_size: output_px(self.output_logical, self.scale),
            // What time it is, as three numbers: the caustic writes the clock's
            // digits, the date and the calendar from these and from the
            // tunables' own words. Asking the C library is I/O and stays here;
            // spelling is not.
            clock_time: self.model.clock,
            clock_date: self.model.clock_date,
            // The month the calendar lays out.
            clock_month: self.model.clock_month,
            ..FrameInputs::default()
        };
        let mirror = *gpu.render(&inputs, &self.commands)?;
        self.commands.clear();
        self.frames += 1;
        self.dirty = false;
        self.animating = mirror.surface.wants_redraw != 0;
        // **In this order**: the witnesses first, so a `layout:` line and the
        // `action:` line the same click produced read in the order they
        // happened; then the actions, which may open a popup or ask for a
        // rebuild; then the two things only this side knows.
        self.drain_witnesses(&mirror);
        self.drain_actions(&mirror);
        self.log_charts();
        self.watch_redraw();

        if self.fault_protocol && !self.fault_fired {
            self.fault_fired = true;
            tracing::warn!(
                "CAUSTICS_BAR_FAULT=protocol: sending wl_surface.set_buffer_scale(0), which is \
                 an invalid_scale protocol error, to prove the exit path"
            );
            self.layer.wl_surface().set_buffer_scale(0);
            self.layer.wl_surface().commit();
            let _ = self.conn.flush();
        }
        Ok(())
    }

    /// The stall trigger: `wants_redraw` has been held longer than this side's
    /// wall clock likes (health.rs).
    ///
    /// **The trigger stays here and the answer does not** (docs/abi.md
    /// *The witness ring*). A stall is a bar that has been animating for seconds, and the
    /// pass that would notice it from `dt` is the one that stops running when
    /// frames do — so this side keeps the clock, says what it has, and queues
    /// a `CMD_DUMP_STALL`; the caustic's `stall:` lines follow on the next
    /// frame, one per element still moving, with the numbers it holds.
    fn watch_redraw(&mut self) {
        let redraw = self.animating;
        self.logged_redraw = redraw;
        let Some(stretch) = self.health.observe_redraw(redraw, Instant::now()) else {
            return;
        };
        tracing::warn!(
            "redraw: still animating after {:.0} s: pointer_target={} popup={}",
            stretch.as_secs_f64(),
            match self.pointer_target {
                abi::TARGET_BAR => "bar",
                abi::TARGET_POPUP => "popup",
                _ => "none",
            },
            popup_kind_name(self.popup_kind),
        );
        self.queue(Command::dump_stall());
        self.health_report("stall");
    }

    /// One line with what the process is costing: frames and CPU since the
    /// last such line, resident memory, the image slots and pixmap store, the curves, the capture session and
    /// the popup. Hourly from `main`, and on every stall warning.
    pub fn health_report(&mut self, why: &str) {
        let (secs, frames, cpu) = self.health.take_interval(self.frames, Instant::now());
        let fps = if secs > 0.0 {
            frames as f64 / secs
        } else {
            0.0
        };
        let cpu_pct = if secs > 0.0 { cpu / secs * 100.0 } else { 0.0 };
        // Two facts: how many image slots are taken of how many there
        // are, and how many curves there are — the font's glyphs plus the icons
        // that joined them. The one number worth watching is `images`: a bar
        // that is evicting slots every rebuild is a bar whose tray has outgrown
        // the array.
        // And the pixmap store behind the slots: what the
        // sidecars sent, and how many keys its bound has let go.
        // `drawn` is how many of the taken slots the caustic's last build put
        // on the bar: the rest are what an eviction may take. After the
        // `N/M slots` the harness greps for (bar-test.sh).
        let images = format!(
            "{}/{} slots, {} drawn, gen {}, {} evicted{}, {} pixmaps stored{}",
            self.images.len(),
            Images::capacity(),
            self.images.drawn(),
            self.images.generation(),
            self.images.evictions(),
            if self.images.refusals() > 0 {
                format!(", {} refused", self.images.refusals())
            } else {
                String::new()
            },
            self.facts.pixmaps_stored(),
            match self.facts.pixmaps_evicted() {
                0 => String::new(),
                n => format!(" ({n} let go)"),
            }
        );
        // The font's own facts come from its `Font` frame and the sizes from the
        // `Bytes` runs that filled the buffers — the bar counts nothing itself.
        let curves = match (self.font.as_ref(), self.gpu.as_ref()) {
            (Some(font), Some(gpu)) => format!(
                "{} {} glyphs + {} icons, {} KiB",
                font.family,
                font.glyph_count,
                gpu.curve_glyphs().saturating_sub(font.icon_base),
                gpu.curve_bytes() / 1024
            ),
            _ => "none".to_owned(),
        };
        let capture = match self.feeders.capture.window() {
            Some(id) => format!("window {id}"),
            None => "none".to_owned(),
        };
        // The one place an inhibitor left on for a week shows up.
        let caffeine = match self.feeders.caffeine.state() {
            crate::model::Caffeine::Absent => "absent",
            crate::model::Caffeine::Off => "free",
            crate::model::Caffeine::On => "held",
        };
        let caffeine = format!("{caffeine} toggles={}", self.feeders.caffeine.toggles());
        // Every sidecar is another process to watch, and this is where their
        // cost shows up beside the bar's. Each one's own `/proc/<pid>/stat` —
        // the whole life of the child, not an interval, because a child may be
        // younger than the report.
        let sidecars = self
            .feeders
            .sidecars
            .iter()
            .map(|pipe| {
                let stats = pipe.stats();
                let state = match (stats.absent() || stats.refused(), stats.pid()) {
                    // "refused" and "absent" are both "nothing is trying any
                    // more", and the difference is worth a word: one is a
                    // binary that is not installed, the other one installed
                    // from another tree (docs/abi.md, *Hello*).
                    (true, _) if stats.refused() => "refused".to_owned(),
                    (true, _) => "absent".to_owned(),
                    (false, None) => {
                        format!("down ticks={} restarts={}", stats.ticks(), stats.restarts())
                    }
                    (false, Some(pid)) => {
                        let (cpu, rss) = health::proc_cost(pid).unwrap_or((0.0, 0));
                        format!(
                            "running pid={pid} ticks={} restarts={} cpu={cpu:.2} s rss={:.1} MB",
                            stats.ticks(),
                            stats.restarts(),
                            rss as f64 / 1_048_576.0,
                        )
                    }
                };
                let skips = stats.skips();
                let skipped = if skips > 0 {
                    format!(" skipped={skips}")
                } else {
                    String::new()
                };
                format!("{}({state}{skipped})", pipe.name())
            })
            .collect::<Vec<_>>()
            .join(" ");
        let sidecars = if sidecars.is_empty() {
            "none".to_owned()
        } else {
            sidecars
        };
        // Said only while it is true, which it should never be: a bar whose
        // shape pass the watchdog has stopped keeps drawing the last shaped text until the arena's front block is written again.
        let shaping = match self.gpu.as_ref().is_some_and(|gpu| gpu.shape_blocked()) {
            true => " shape=STOPPED (watchdog)",
            false => "",
        };
        // No `notdefs=` on this line: the count is the caustic's
        // `text:` witness, pushed by the pass that has the flags, and a second
        // copy of it here would be a number this side had to be told.
        tracing::info!(
            "health ({why}):{shaping} frames={} (+{frames} in {secs:.0} s, {fps:.2} fps) cpu={cpu:.2} s ({cpu_pct:.2}%) \
             rss={:.1} MB images={images} curves={curves} \
             capture={capture} caffeine={caffeine} popup={} redraw={} sidecars={sidecars}",
            self.frames,
            health::rss_bytes() as f64 / 1_048_576.0,
            popup_kind_name(self.popup_kind),
            if self.logged_redraw {
                "animating"
            } else {
                "settled"
            },
        );
    }

    /// Which target a `wl_surface` is, for pointer routing.
    fn target_of(&self, surface: &wl_surface::WlSurface) -> u32 {
        if surface == self.layer.wl_surface() {
            abi::TARGET_BAR
        } else if self
            .popup
            .as_ref()
            .is_some_and(|popup| popup.wl_surface() == surface)
        {
            abi::TARGET_POPUP
        } else {
            abi::NO_INDEX
        }
    }

    /// Bring wgpu up on the layer surface. Called only from
    /// [`Bar::service_surfaces`], never from a handler.
    fn bring_up_gpu(&mut self, width: u32, height: u32) -> Result<()> {
        let display = NonNull::new(self.conn.backend().display_ptr().cast())
            .context("the wl_display has no pointer")?;
        let surface = NonNull::new(self.layer.wl_surface().id().as_ptr().cast())
            .context("the layer's wl_surface has no pointer")?;
        // SAFETY: the connection and the layer surface are owned by this
        // struct and dropped after `gpu` (declared later in the struct), so
        // both handles outlive the wgpu surface.
        let mut gpu = unsafe { Gpu::new(display, surface, width, height) }?;
        // Everything `caustics-glyphs` sent while there was nowhere to put it
        // — which on a warm start is the whole font, since it is read and sent
        // before the compositor has answered the first configure.
        if let Some(font) = self.font.as_ref() {
            gpu.set_cap_height(font.cap_height);
        }
        if !self.pending_bytes.is_empty() {
            let bytes: usize = self.pending_bytes.iter().map(|run| run.bytes.len()).sum();
            tracing::info!(
                "gpu: {} buffer runs ({} KiB) were waiting for a device",
                self.pending_bytes.len(),
                bytes / 1024
            );
            for run in std::mem::take(&mut self.pending_bytes) {
                gpu.write_bytes(run.buffer, run.offset, run.total, &run.bytes);
            }
            self.curves_landed = true;
            tracing::info!(
                "text: the first curves landed {} ms after start; the blocks already \
                 published are shaped on the next frame",
                self.started.elapsed().as_millis()
            );
        }
        // The one thing the device knows that a separately built compiler
        // cannot guess: SPIR-V bakes the storage-image format in, so the
        // caustic is compiled per swapchain format and this is the format
        // this session landed on. Sent once, through the same `Request` path
        // an icon name takes — the pipe remembers it and replays it to every
        // child, so a restarted compiler learns it again with no code here
        // for that (docs/abi.md, *Request*).
        let (define, _) = gpu.dst_format_args();
        self.gpu = Some(gpu);
        self.modules.expect(wire::format_code(define));
        self.feeders
            .request(pipe::RELOAD_SIDECAR, wire::REQ_PROGRAM, define);
        Ok(())
    }

    /// Give the popup a swapchain, and tell the caustic it is mapped. Same
    /// deferral rule as [`Bar::bring_up_gpu`].
    fn attach_popup_surface(&mut self, width: u32, height: u32) {
        let Some(popup) = self.popup.as_ref() else {
            return;
        };
        let display = self.conn.backend().display_ptr().cast();
        let surface = popup.wl_surface().id().as_ptr().cast();
        let (Some(display), Some(surface)) = (NonNull::new(display), NonNull::new(surface)) else {
            tracing::error!("the popup's handles are null; not attaching a swapchain");
            return;
        };
        let Some(gpu) = self.gpu.as_mut() else { return };
        // SAFETY: the popup is owned by this struct and torn down by
        // `destroy_popup`, which drops the wgpu surface first.
        match unsafe { gpu.attach_popup(display, surface, width, height) } {
            Ok(()) => self.queue(Command::popup_mapped(width, height)),
            Err(err) => tracing::error!("popup swapchain creation failed: {err:#}"),
        }
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }
}

/// Straight RGBA8 into premultiplied RGBA8, reusing `out`'s allocation. The
/// capture feeder hands over straight alpha (model.rs's contract) and the preview slot is premultiplied, unlike the image slots, whose straight texels the caustic multiplies at the sample.
fn premultiply_into(straight: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(straight.len());
    for texel in straight.chunks_exact(4) {
        let a = texel[3] as u32;
        out.push(((texel[0] as u32 * a + 127) / 255) as u8);
        out.push(((texel[1] as u32 * a + 127) / 255) as u8);
        out.push(((texel[2] as u32 * a + 127) / 255) as u8);
        out.push(texel[3]);
    }
}

// ---------------------------------------------------------------------------
// Handlers. Every one of them translates and none of them decides.
// ---------------------------------------------------------------------------

impl LayerShellHandler for Bar {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        tracing::info!("the compositor closed the layer surface");
        self.exit = Some(Exit::Closed);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // A zero dimension means "you choose"; the bar chooses full width and
        // its own height.
        let width = if configure.new_size.0 == 0 {
            self.bar_logical.0.max(1)
        } else {
            configure.new_size.0
        };
        let height = if configure.new_size.1 == 0 {
            self.height.max(1)
        } else {
            configure.new_size.1
        };
        let first = !self.configured;
        self.configured = true;
        self.bar_logical = (width, height);
        self.layer.wl_surface().set_buffer_scale(self.scale as i32);

        let px = (width * self.scale, height * self.scale);
        tracing::info!(
            logical = format!("{width}x{height}"),
            scale = self.scale,
            px = format!("{}x{}", px.0, px.1),
            first,
            "layer surface configured"
        );

        // Recorded, not acted on: see `Bar::service_surfaces`.
        self.pending_surface = Some(px);
        self.dirty = true;
    }
}

impl PopupHandler for Bar {
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        popup: &Popup,
        configure: PopupConfigure,
    ) {
        if self.popup.as_ref().map(|p| p.xdg_popup()) != Some(popup.xdg_popup()) {
            return;
        }
        let width = configure.width.max(1) as u32;
        let height = configure.height.max(1) as u32;
        let px = (width * self.scale.max(1), height * self.scale.max(1));
        tracing::info!(
            "popup: open kind={} {}x{} px={}x{} at {},{} ({})",
            popup_kind_name(self.popup_kind),
            width,
            height,
            px.0,
            px.1,
            configure.position.0,
            configure.position.1,
            match configure.kind {
                ConfigureKind::Initial => "initial",
                ConfigureKind::Reactive => "reactive",
                ConfigureKind::Reposition { .. } => "reposition",
                _ => "unknown",
            },
        );

        // Every configure counts, not only the first: a submenu descent
        // repositions a *mapped* popup, and its new size arrives this way.
        // Recorded, not acted on: see `Bar::service_surfaces`.
        self.popup_configured = true;
        self.pending_popup = Some(px);
        self.dirty = true;
    }

    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Popup) {
        self.destroy_popup("the compositor dismissed it");
    }
}

impl PointerHandler for Bar {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let scale = self.scale.max(1) as f32;
        for event in events {
            let target = self.target_of(&event.surface);
            if target == abi::NO_INDEX {
                continue;
            }
            let pos = [
                (event.position.0 as f32 * scale),
                (event.position.1 as f32 * scale),
            ];
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_target = target;
                    self.queue(Command::pointer_motion(target, pos));
                }
                PointerEventKind::Leave { .. } => {
                    if self.pointer_target == target {
                        self.pointer_target = abi::NO_INDEX;
                    }
                    self.queue(Command::pointer_leave(target));
                }
                PointerEventKind::Press { button, serial, .. } => {
                    // The serial an `xdg_popup.grab` needs is the *button*
                    // serial, and it has to still be the newest one when the
                    // grab is issued — hence keeping it rather than passing it.
                    self.last_button_serial = serial;
                    self.pointer_target = target;
                    self.queue(Command::pointer_down(target, pos, button));
                }
                PointerEventKind::Release { button, serial, .. } => {
                    self.last_button_serial = serial;
                    self.pointer_target = target;
                    self.queue(Command::pointer_up(target, pos, button));
                }
                PointerEventKind::Axis { vertical, .. } => {
                    // The wheel, translated: px and steps
                    // as they came, and what they scroll is the caustic's.
                    if let Some(command) = axis_command(target, pos, &vertical, scale) {
                        self.queue(command);
                    }
                }
            }
        }
    }
}

impl SeatHandler for Bar {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    tracing::info!("pointer acquired");
                    self.pointer = Some(pointer);
                    self.seat = Some(seat);
                }
                Err(err) => tracing::warn!("could not get the seat's pointer: {err}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {
        self.seat = None;
    }
}

impl CompositorHandler for Bar {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if surface != self.layer.wl_surface() {
            return;
        }
        let scale = new_factor.max(1) as u32;
        if scale == self.scale {
            return;
        }
        tracing::info!(from = self.scale, to = scale, "buffer scale changed");
        self.scale = scale;
        self.layer.wl_surface().set_buffer_scale(scale as i32);
        if self.configured {
            self.pending_surface = Some((self.bar_logical.0 * scale, self.bar_logical.1 * scale));
        }
        // A scale change moves every size the caustic asks for, and the sizes
        // are `bar_build`'s — so what is owed is a run of the model passes and
        // not a rebuild of anything on this side. (Safe from inside the
        // dispatch: it touches no Wayland object and no swapchain.)
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.mark_shape_pending();
        }
        self.dirty = true;
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if surface != self.layer.wl_surface() {
            return;
        }
        self.frame_pending = false;
        // A spring in flight is a frame per repaint; a settled bar draws
        // nothing until an event dirties it.
        if self.animating {
            self.dirty = true;
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        // Membership test: the bar owns the `wl_output` and its surface's enter;
        // this copies one size out of the event and decides nothing with it.
        if surface != self.layer.wl_surface() {
            return;
        }
        tracing::info!(
            known = self.output_state.info(output).is_some(),
            "output: the bar's surface entered an output"
        );
        self.output = Some(output.clone());
        self.learn_output_size(output);
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if surface != self.layer.wl_surface() || self.output.as_ref() != Some(output) {
            return;
        }
        self.output = None;
        self.output_logical = (0, 0);
        self.dirty = true;
    }
}

impl Bar {
    /// Copy `output`'s logical size into what the next frame reports, if it is
    /// the output the bar is on. `xdg_output`'s logical size when the
    /// compositor sent one, else the `wl_output`'s current mode over its
    /// scale — the same number `xdg_output` would have carried for an
    /// unscaled-fraction output, read off the same object. Nothing known is
    /// (0, 0): "no bound".
    fn learn_output_size(&mut self, output: &wl_output::WlOutput) {
        if self.output.as_ref() != Some(output) {
            return;
        }
        let info = self.output_state.info(output);
        let logical = info.as_ref().map(logical_of).unwrap_or((0, 0));
        if logical != self.output_logical {
            tracing::info!(
                width = logical.0,
                height = logical.1,
                from = match info.as_ref() {
                    None => "nothing yet",
                    Some(info) if info.logical_size.is_some() => "xdg_output",
                    Some(_) => "the mode",
                },
                "output: logical size"
            );
            self.output_logical = logical;
            self.dirty = true;
        }
    }
}

/// One wheel or touchpad frame as a `CMD_AXIS`, or nothing for a frame that
/// moved only horizontally.
///
/// `absolute` is in surface-local logical px and goes over times the scale;
/// the discrete steps go over as they came, because what a step is worth
/// (`MENU_ROW_H`) is the caustic's tunable and not this side's to know. The
/// stop event of a touchpad fling carries neither and is dropped.
fn axis_command(target: u32, pos: [f32; 2], vertical: &AxisScroll, scale: f32) -> Option<Command> {
    let px = (vertical.absolute as f32 * scale).round() as i32;
    let steps = vertical.discrete;
    if px == 0 && steps == 0 {
        return None;
    }
    Some(Command::axis(target, pos, px, steps))
}

/// An output's logical size: `xdg_output`'s, or the current mode divided by
/// the integer scale, turned for a 90° or 270° transform.
fn logical_of(info: &OutputInfo) -> (u32, u32) {
    let mode = info
        .modes
        .iter()
        .find(|mode| mode.current)
        .map(|mode| mode.dimensions);
    let turned = matches!(
        info.transform,
        wl_output::Transform::_90
            | wl_output::Transform::_270
            | wl_output::Transform::Flipped90
            | wl_output::Transform::Flipped270
    );
    logical_from(info.logical_size, mode, info.scale_factor, turned)
}

/// `logical_of`'s arithmetic, on the numbers alone (sctk's `OutputInfo` is
/// `#[non_exhaustive]`, so a test cannot build one).
fn logical_from(
    logical: Option<(i32, i32)>,
    mode: Option<(i32, i32)>,
    scale: i32,
    turned: bool,
) -> (u32, u32) {
    if let Some((w, h)) = logical {
        return (w.max(0) as u32, h.max(0) as u32);
    }
    let Some((w, h)) = mode else {
        return (0, 0);
    };
    let scale = scale.max(1);
    let (w, h) = ((w.max(0) / scale) as u32, (h.max(0) / scale) as u32);
    if turned { (h, w) } else { (w, h) }
}

/// The output's size in the surface's px: logical × the integer buffer scale,
/// which is the arithmetic every other size in `FrameInputs` is in. (0, 0)
/// stays (0, 0) — "no bound" to the caustic.
fn output_px(logical: (u32, u32), scale: u32) -> [u32; 2] {
    let scale = scale.max(1);
    [
        logical.0.saturating_mul(scale),
        logical.1.saturating_mul(scale),
    ]
}

impl OutputHandler for Bar {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // An output whose description finished after the surface entered it.
        self.learn_output_size(&output);
    }

    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // A mode change or a new scale on the output the bar is on moves the
        // bound; any other output is none of the bar's business.
        self.learn_output_size(&output);
    }

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.output.as_ref() == Some(&output) {
            self.output = None;
            self.output_logical = (0, 0);
            self.dirty = true;
        }
    }
}

impl ProvidesRegistryState for Bar {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

/// The bar creates no xdg toplevels, but `delegate_xdg_shell!` — which is
/// what binds `xdg_wm_base`, and therefore what makes the popup possible —
/// carries the window dispatches with it. These two are unreachable.
impl WindowHandler for Bar {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {}

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        _: WindowConfigure,
        _: u32,
    ) {
    }
}

/// The three lines `feeders/mod.rs` asks for ("How the embassy
/// plugs this in"): one `AsMut`, one macro, and `Feeders::bind` in
/// [`Bar::new`]. Ten interfaces reach the feeders through this
/// and the embassy's state type appears in none of their signatures.
impl AsMut<Feeders> for Bar {
    fn as_mut(&mut self) -> &mut Feeders {
        &mut self.feeders
    }
}

crate::delegate_feeders!(Bar);

delegate_compositor!(Bar);
delegate_output!(Bar);
delegate_seat!(Bar);
delegate_pointer!(Bar);
delegate_layer!(Bar);
delegate_xdg_shell!(Bar);
delegate_xdg_popup!(Bar);
smithay_client_toolkit::delegate_xdg_window!(Bar);
delegate_registry!(Bar);

/// What one `xdg_activation_token_v1` is for: the sidecar
/// and the row its `done` goes back to.
pub struct TokenFor {
    sidecar: String,
    row: u32,
}

impl Bar {
    /// Ask for a token for the click that activated `row` — the click's serial
    /// and the bar's own layer surface (the popup the click landed on may be
    /// gone by `done`; niri checks the serial). The action itself has already
    /// gone: the sidecar waits a grace for the token and never longer, and the
    /// token follows as a `REQ_ACTIVATION` when `done` arrives.
    fn ask_activation(&mut self, sidecar: &str, row: u32) {
        let why = match (&self.activation, &self.seat, self.last_button_serial) {
            (None, _, _) => "no xdg_activation_v1",
            (_, None, _) | (_, _, 0) => "no click serial",
            (Some(activation), Some(seat), serial) => {
                let for_row = TokenFor {
                    sidecar: sidecar.to_owned(),
                    row,
                };
                let token = activation.get_activation_token(&self.qh, for_row);
                token.set_serial(serial, seat);
                token.set_surface(self.layer.wl_surface());
                token.commit();
                return;
            }
        };
        tracing::info!("activation: no token ({why}) for {sidecar} row={row}");
    }
}

/// `xdg_activation_v1` has no events.
impl Dispatch<XdgActivationV1, ()> for Bar {
    fn event(
        _: &mut Self,
        _: &XdgActivationV1,
        _: xdg_activation_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// `done(token)`, the one event: down the pipe, and the object destroyed.
impl Dispatch<XdgActivationTokenV1, TokenFor> for Bar {
    fn event(
        bar: &mut Self,
        proxy: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        for_row: &TokenFor,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let xdg_activation_token_v1::Event::Done { token } = event else {
            return;
        };
        proxy.destroy();
        let (sidecar, row) = (&for_row.sidecar, for_row.row);
        if bar.feeders.activation_token(sidecar, row, &token) {
            tracing::info!("activation: token for row={row} ({} bytes)", token.len());
        } else {
            tracing::info!("activation: no token (the pipe refused it) for {sidecar} row={row}");
        }
    }
}

#[cfg(test)]
mod tests;
