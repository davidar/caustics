//! The canvas's Wayland side: an output's surfaces, one per role, the seat's
//! input, and the meeting point of everything else the host is made of.
//!
//! Everything here passes the membership test the bar's process does
//! (docs/architecture.md, *The bar process*): it owns a Wayland object, sits
//! on a configure round trip, or routes a sidecar's frame to where its bytes
//! go. Nothing here decides what the canvas looks like: a picture is copied
//! into a slot when its show arrives, a command's head is copied into
//! `facts`, and the caustic says how far a fade or a dim is and whether it
//! wants another frame (docs/abi.md, *The canvas's buffer*).
//!
//! # The surfaces
//!
//! A **pane** is one output's `zwlr_layer_shell_v1` surface in one [`Role`]
//! (`role.rs` holds what differs between roles: the layer, the keyboard, the
//! namespace, the draw target). Every output has its **background** pane from
//! the moment it appears until it goes. While a dim is up it has an
//! **overlay** pane too: made for every output when a `CANVAS_DIM` arrives
//! and for an output that appears mid-dim, and destroyed on `CANVAS_UNDIM`,
//! on input, and when the dim's sidecar goes (docs/canvas.md, *Surfaces*).
//! While a prompt is up it has a **top** pane too: made for every output at a
//! conversation's first ask (`prompt.rs`) and for an output that appears
//! mid-prompt, and destroyed when the conversation ends or is dismissed.
//! While the session is locked it has a **lock** pane instead of any top
//! pane: an `ext_session_lock_v1` lock surface, made for every output as the
//! lock is asked for and for an output that appears while locked, and
//! destroyed with the unlock (`lock.rs`). The prompt is drawn on the lock
//! panes then, and a conversation's raise and lower only redraw them: a top
//! layer surface is hidden under a lock anyway. A pane's surface is a
//! [`Shell`], a layer surface or a lock surface; everything else about a pane
//! is the same for both.
//! Each pane has its own swapchain, frame pacing, `wp_fractional_scale_v1`
//! and `wp_viewport` from its first commit: the buffer is the logical size
//! times the scale the compositor prefers, and the viewport's destination is
//! the logical size, so a 1.5 output gets a buffer of its own pixels rather
//! than a 2× one scaled down. A compositor without the fractional-scale
//! global gets the integer scale, said in one line.
//!
//! # Input
//!
//! The seat's pointer, keyboard and touch are bound as the seat offers them.
//! While a dim is up, **input is a cancel**, as chayang's is: the pointer
//! moving over an overlay, a button or a scroll on one, any key pressed (the
//! overlay holds the keyboard exclusively, so every key the compositor sends
//! is one pressed on it), a touch going down on one. The host writes
//! `CANVAS_EVENT_INPUT` to the dim's sidecar, takes every overlay down, and
//! the outputs are the plain wallpaper again (docs/abi.md, *Canvas event*).
//! The pointer *entering* an overlay is not input: the compositor sends an
//! enter when the overlay maps under a pointer that has not moved, so the
//! cursor is hidden there and only a motion to somewhere else cancels. The
//! keymap the seat sends is compiled by libxkbcommon (`keys`), for the
//! prompt's sake; a cancel needs only the fact of a key.
//!
//! **While a prompt is up, its keys are its own**: they go to the prompt
//! core, and neither a key nor the pointer nor a touch cancels a dim that is
//! also up, which stays as it was when the prompt closes.
//!
//! **The wake's pointer** is apart from all of that: on a background or a
//! lock pane (`Role::stirs`) the pointer's place, in that pane's buffer px,
//! and a count of its presses there are written into `facts` once per
//! dispatch (`pointer.rs`), whatever else the handler does with the event,
//! and the pane draws again if the caustic said of its last frame that the
//! pointer is something it draws there (`CanvasState::wants_pointer`). What
//! the water does is the caustic's (docs/canvas.md, *The wake*).
//!
//! # The frames
//!
//! A frame is one pane's. It is drawn when that pane is dirty (a configure, a
//! command, a reload, the pointer's news on a pane whose caustic wants it)
//! or the caustic said `wants_redraw` for it, and its frame
//! request is answered or [`FRAME_STALL`] old: each pane is paced on its own
//! `wl_surface.frame`, made before the present so it rides the commit
//! (docs/architecture.md, *The frame*). A compositor that sends no callbacks
//! to a surface it does not repaint still gets a frame every stall, from a
//! timer: that is the pacing default docs/canvas.md, *Time*, asks for. After
//! each frame the host reads whether the dim is at its depth
//! (`CanvasState::dim_reached`) and tells the dim's sidecar once.
//!
//! [`FRAME_STALL`]: caustics_host::pacing::FRAME_STALL

use std::ptr::NonNull;
use std::time::Instant;

use anyhow::{Context, Result};
use caustics_bar_abi::canvas::CanvasFacts;
use caustics_bar_abi::wire::{
    self, CANVAS_EVENT_DIMMED, CANVAS_EVENT_INPUT, Canvas as Command, CanvasEventBody, Frame,
    Module, PROGRAM_CANVAS, REQ_COVER, REQ_OUTPUT_SIZE, REQ_PROGRAM, Request,
};
use caustics_bar_abi::{CANVAS_ENTRIES, wire::Image};
use caustics_host::compile::Wake;
use caustics_host::modules::{Collected, Modules};
use caustics_host::pacing::Pacing;
use caustics_host::pipe::{Event, Pipe};
use caustics_host::program::Landed;
use caustics_host::strings::Strings;
use caustics_host::surface::Surface;
use caustics_host::text::Covers;
use caustics_host::witness::{Caustic, Printer};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_session_lock,
    output::{OutputHandler, OutputState},
    reexports::calloop::{
        LoopHandle, RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::GlobalList,
    protocol::{wl_callback, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface, wl_touch},
};

use crate::dim::Dims;
use crate::gpu::{self, Gpu};
use crate::keys::{self, xkb::Xkb};
use crate::lock::{self, Locks};
use crate::pointer::Pointer;
use crate::prompt::{Effect, Prompts};
use crate::role::Role;
use crate::sidecars::{self, GLYPHS, RELOAD, WALLPAPER};
use crate::slots::{Applied, Slots};

/// `wp_fractional_scale_v1`'s unit: a scale of 1.0 is 120.
const SCALE_ONE: u32 = 120;

/// Why the host is leaving.
#[derive(Clone, Debug)]
pub enum Exit {
    /// `--max-runtime` ran out.
    Deadline,
    /// The device would not come up, or a frame failed.
    Fault(String),
}

/// One output's buffer side, px: a logical length times a scale in 120ths,
/// rounded half away from zero as `wp_fractional_scale_v1` asks.
pub fn buffer_px(logical: u32, scale: u32) -> u32 {
    ((u64::from(logical) * u64::from(scale) + u64::from(SCALE_ONE / 2)) / u64::from(SCALE_ONE))
        .max(1) as u32
}

/// Whether a pointer motion to `to` is movement, from where the pointer last
/// was on the overlay (`from`, the enter's or the last motion's position).
/// A motion to where the pointer already is, which a compositor may send
/// when it refocuses a surface, is not.
pub fn moved(from: Option<(f64, f64)>, to: (f64, f64)) -> bool {
    match from {
        Some(from) => (from.0 - to.0).abs() >= 0.5 || (from.1 - to.1).abs() >= 0.5,
        None => true,
    }
}

/// A pane's surface: a layer surface for the background, the overlay and the
/// prompt, a lock surface for the lock. Either is a `wl_surface` with a role,
/// and dropping it destroys the role object and then the surface.
enum Shell {
    Layer(LayerSurface),
    Lock(SessionLockSurface),
}

impl Shell {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Shell::Layer(layer) => layer.wl_surface(),
            Shell::Lock(lock) => lock.wl_surface(),
        }
    }

    /// A commit of the surface's pending state.
    fn commit(&self) {
        match self {
            Shell::Layer(layer) => layer.commit(),
            Shell::Lock(lock) => lock.wl_surface().commit(),
        }
    }
}

/// One output's surface in one role.
struct Pane {
    /// Dropped first, before the surface its raw pointers name.
    target: Option<Surface>,
    role: Role,
    /// `CanvasParams::pane`: ≥ 1, never reused while the host runs.
    id: u32,
    output: wl_output::WlOutput,
    /// The output's name, for the journal.
    name: String,
    shell: Shell,
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    /// The size the compositor configured, logical px.
    logical: (u32, u32),
    /// The scale this surface is drawn at, in 120ths.
    scale: u32,
    configured: bool,
    /// A size or a scale that the swapchain has not caught up with.
    pending: bool,
    /// Something changed that this pane has not drawn.
    dirty: bool,
    pacing: Pacing,
    /// Whether the caustic said, of this pane's last frame, that the pointer
    /// moving on it is something it draws (`CanvasState::wants_pointer`).
    pointer_frames: bool,
    /// Frames drawn since the caustic last began asking for them here.
    run: u32,
}

impl Pane {
    fn buffer(&self) -> (u32, u32) {
        (
            buffer_px(self.logical.0, self.scale),
            buffer_px(self.logical.1, self.scale),
        )
    }
}

/// The host.
pub struct Canvas {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    /// `ext_session_lock_manager_v1`, when the compositor has it (a
    /// `lock` on a state without it is an error, said once per request).
    session_lock: SessionLockState,
    /// The lock object, from the request until the unlock.
    lock: Option<SessionLock>,
    /// The lock's state and owner (`lock.rs`).
    locks: Locks,
    fractional: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    conn: Connection,
    qh: QueueHandle<Canvas>,
    panes: Vec<Pane>,
    /// The device, once the first pane has been configured. Dropped after
    /// every pane's target (declared after `panes`).
    gpu: Option<Gpu>,
    pipes: Vec<Pipe>,
    modules: Modules,
    strings: Strings,
    printer: Printer,
    slots: Slots,
    /// The dim up now, whose it is and what it has been told.
    dims: Dims,
    /// The prompt core: the conversation held or up, and its typed answer.
    prompts: Prompts,
    /// Whether a prompt's text being cut to its buffer has been said, for
    /// the conversation up.
    prompt_cut_said: bool,
    /// The seat's devices, as it offers them.
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    touch: Option<wl_touch::WlTouch>,
    /// The seat's keymap, compiled, with compose: `None` until the first
    /// keymap arrives, and for good when libxkbcommon cannot be opened.
    xkb: Option<Xkb>,
    /// Whether a missing libxkbcommon has been said.
    xkb_missing_said: bool,
    /// Where the pointer last was on an overlay, surface-local: what a
    /// motion is measured from.
    pointer_at: Option<(f64, f64)>,
    /// The pointer as the wake is told it (`pointer.rs`), and whether
    /// `facts` is behind it.
    wake_pointer: Pointer,
    pointer_unwritten: bool,
    /// The facts as last written: what a pointer's news is written over.
    facts: CanvasFacts,
    /// The id the next pane takes.
    next_pane: u32,
    /// Pictures placed before the device existed, by slot.
    unplaced: [Option<Image>; 2],
    /// `Bytes` runs that arrived before there was a device to put them in:
    /// the font goes out the moment `caustics-glyphs` has it, which is often
    /// before the first configure, and every run is sent once.
    pending_bytes: Vec<wire::Bytes>,
    /// The strings a `REQ_COVER` has gone out for, once each.
    covers: Covers,
    /// Literals in the pool the caustic spells from (`CanvasFacts::literal_count`).
    literal_count: u32,
    /// The size last told the wallpaper sidecar, and how many sizes have been.
    told: Option<(u32, u32)>,
    sizes: u32,
    loop_handle: Option<LoopHandle<'static, Canvas>>,
    /// What the compile workers call when a pipeline lands: the loop's own
    /// wake, so it installs the pipeline on its next turn
    /// ([`Canvas::pipelines_landed`]).
    wake: Wake,
    /// A generation of the caustic queued on the compile workers and not
    /// yet swapped in, and what its swap line says.
    reloading: Option<Reloading>,
    stall_token: Option<RegistrationToken>,
    stall_at: Option<Instant>,
    frames: u64,
    pub exit: Option<Exit>,
}

impl Canvas {
    /// Bind the globals and start the sidecars. The surfaces come as the
    /// outputs do (`OutputHandler::new_output`), and the device with the
    /// first one's configure.
    pub fn new(
        conn: Connection,
        globals: &GlobalList,
        qh: QueueHandle<Canvas>,
        events: &smithay_client_toolkit::reexports::calloop::channel::Sender<Event>,
        wake: Wake,
    ) -> Result<Canvas> {
        let compositor =
            CompositorState::bind(globals, &qh).context("wl_compositor is not available")?;
        let layer_shell =
            LayerShell::bind(globals, &qh).context("zwlr_layer_shell_v1 is not available")?;
        let session_lock = SessionLockState::new(globals, &qh);
        let lockable = globals.contents().with_list(|list| {
            list.iter()
                .any(|global| global.interface == "ext_session_lock_manager_v1")
        });
        if lockable {
            tracing::info!("lock: ext_session_lock_manager_v1 is bound");
        } else {
            tracing::warn!(
                "lock: the compositor has no ext_session_lock_manager_v1; a lock will be refused"
            );
        }
        let fractional = globals
            .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
            .ok();
        let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
        match (&fractional, &viewporter) {
            (Some(_), Some(_)) => {
                tracing::info!("scale: wp_fractional_scale_v1 and wp_viewporter are bound")
            }
            _ => tracing::warn!(
                "scale: no {}; drawing at each output's integer scale",
                if fractional.is_none() {
                    "wp_fractional_scale_manager_v1"
                } else {
                    "wp_viewporter"
                }
            ),
        }
        let pipes = sidecars::start(events);
        let strings = Strings::baked(gpu::BAKED_REFLECTION);
        Ok(Canvas {
            registry_state: RegistryState::new(globals),
            output_state: OutputState::new(globals, &qh),
            seat_state: SeatState::new(globals, &qh),
            compositor,
            layer_shell,
            session_lock,
            lock: None,
            locks: Locks::new(),
            fractional,
            viewporter,
            conn,
            qh,
            panes: Vec::new(),
            gpu: None,
            pipes,
            modules: Modules::new(&CANVAS_ENTRIES),
            strings,
            printer: Printer::new(Caustic::Canvas),
            slots: Slots::new(),
            dims: Dims::new(),
            prompts: Prompts::new(),
            prompt_cut_said: false,
            pointer: None,
            keyboard: None,
            touch: None,
            xkb: None,
            xkb_missing_said: false,
            pointer_at: None,
            wake_pointer: Pointer::new(),
            pointer_unwritten: false,
            facts: CanvasFacts::default(),
            next_pane: 1,
            unplaced: [None, None],
            pending_bytes: Vec::new(),
            covers: Covers::new(GLYPHS, REQ_COVER),
            literal_count: 0,
            told: None,
            sizes: 0,
            loop_handle: None,
            wake,
            reloading: None,
            stall_token: None,
            stall_at: None,
            frames: 0,
            exit: None,
        })
    }

    /// The loop the stall timer is armed on.
    pub fn set_loop_handle(&mut self, handle: LoopHandle<'static, Canvas>) {
        self.loop_handle = Some(handle);
    }

    /// True until the first pane's configure has brought the device up.
    pub fn awaiting_device(&self) -> bool {
        self.gpu.is_none() && self.exit.is_none()
    }

    /// Whether any output has a surface yet.
    pub fn has_screens(&self) -> bool {
        !self.panes.is_empty()
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    fn uses_fractional(&self) -> bool {
        self.fractional.is_some() && self.viewporter.is_some()
    }

    fn pipe(&self, name: &str) -> Option<&Pipe> {
        self.pipes.iter().find(|pipe| pipe.name() == name)
    }

    /// `facts`, written whole, when there is a device to write it to: the
    /// slots' picture and dim, the literal pool's size, and the prompt, with
    /// the prompt's text written into its buffer first.
    fn push_facts(&mut self) {
        let (prompt, text, cut) = self.prompts.facts();
        if !cut {
            self.prompt_cut_said = false;
        } else if !std::mem::replace(&mut self.prompt_cut_said, true) {
            tracing::warn!(
                "prompt: conversation {}'s text is past the {} bytes of its buffer; its end \
                 is not drawn",
                prompt.id,
                caustics_bar_abi::canvas::PROMPT_TEXT_BYTES
            );
        }
        let mut facts = CanvasFacts {
            literal_count: self.literal_count,
            prompt,
            ..*self.slots.facts()
        };
        self.wake_pointer.write(&mut facts);
        self.facts = facts;
        self.pointer_unwritten = false;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.write_prompt_text(&text);
            gpu.write_facts(&facts);
        }
    }

    /// The pointer's news, written over the facts as they were last written:
    /// once per dispatch however many motions it carried, and with nothing
    /// else rewritten.
    fn push_pointer(&mut self) {
        if !std::mem::replace(&mut self.pointer_unwritten, false) {
            return;
        }
        self.wake_pointer.write(&mut self.facts);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.write_facts(&self.facts);
        }
    }

    /// The wake's pointer on pane `index` moved, arrived, left or pressed
    /// (`news` says whether that changed anything): the facts are behind,
    /// and the pane draws again if its caustic said the pointer is something
    /// it draws there.
    fn stirred(&mut self, index: usize, news: bool) {
        if !news {
            return;
        }
        self.pointer_unwritten = true;
        let pane = &mut self.panes[index];
        if pane.pointer_frames {
            pane.dirty = true;
        }
    }

    fn mark_dirty(&mut self, role: Role) {
        for pane in self.panes.iter_mut().filter(|pane| pane.role == role) {
            pane.dirty = true;
        }
    }

    fn mark_all_dirty(&mut self) {
        for pane in &mut self.panes {
            pane.dirty = true;
        }
    }

    // -- the surfaces ----------------------------------------------------

    /// A pane for an output, in a role: a layer surface committed with
    /// nothing attached so the compositor configures it, or a lock surface,
    /// which the compositor configures as it is made.
    fn add_pane(&mut self, output: &wl_output::WlOutput, role: Role) {
        if self
            .panes
            .iter()
            .any(|pane| pane.role == role && pane.output == *output)
        {
            return;
        }
        let info = self.output_state.info(output);
        let name = info
            .as_ref()
            .and_then(|info| info.name.clone())
            .unwrap_or_else(|| format!("output {}", output.id().protocol_id()));
        let integer = info.as_ref().map_or(1, |info| info.scale_factor.max(1)) as u32;
        let surface = self.compositor.create_surface(&self.qh);
        let shell = match role.layer() {
            Some(spec) => {
                let layer = self.layer_shell.create_layer_surface(
                    &self.qh,
                    surface,
                    spec.layer,
                    Some(spec.namespace),
                    Some(output),
                );
                layer.set_anchor(spec.anchor);
                layer.set_exclusive_zone(spec.exclusive_zone);
                layer.set_keyboard_interactivity(spec.keyboard);
                layer.set_size(0, 0);
                Shell::Layer(layer)
            }
            None => {
                let Some(lock) = self.lock.as_ref() else {
                    tracing::warn!("output {name}: a lock surface with no lock; not made");
                    surface.destroy();
                    return;
                };
                Shell::Lock(lock.create_lock_surface(surface, output, &self.qh))
            }
        };
        // The scale objects before the first commit, as the protocols want:
        // `wp_fractional_scale_v1` applies to any `wl_surface`, a lock
        // surface's included.
        let (fractional, viewport) = if self.uses_fractional() {
            let wl_surface = shell.wl_surface();
            (
                self.fractional.as_ref().map(|manager| {
                    manager.get_fractional_scale(wl_surface, &self.qh, wl_surface.clone())
                }),
                self.viewporter
                    .as_ref()
                    .map(|viewporter| viewporter.get_viewport(wl_surface, &self.qh, ())),
            )
        } else {
            shell.wl_surface().set_buffer_scale(integer as i32);
            (None, None)
        };
        // A layer surface is committed with nothing attached, which asks for
        // its configure; a lock surface is configured as it is made, and its
        // first commit is its first frame (`Role::commits_before_configure`).
        if role.commits_before_configure() {
            shell.commit();
        }
        tracing::info!(
            "output {name}: a{} {} surface (integer scale {integer} until the compositor \
             says otherwise)",
            if role == Role::Overlay { "n" } else { "" },
            role.name()
        );
        let id = self.next_pane;
        self.next_pane = self.next_pane.wrapping_add(1).max(1);
        self.panes.push(Pane {
            target: None,
            role,
            id,
            output: output.clone(),
            name,
            shell,
            fractional,
            viewport,
            logical: (0, 0),
            scale: integer * SCALE_ONE,
            configured: false,
            pending: false,
            dirty: false,
            pacing: Pacing::new(Instant::now()),
            pointer_frames: false,
            run: 0,
        });
    }

    /// The output went, or the compositor closed the surface, or the dim, the
    /// prompt or the lock is over: the swapchain first, then the objects it
    /// points at, then the layer or lock surface (destroyed as it drops).
    fn remove_pane(&mut self, index: usize) {
        let mut pane = self.panes.remove(index);
        if self.wake_pointer.left(pane.id) {
            self.pointer_unwritten = true;
        }
        pane.target = None;
        if let Some(viewport) = pane.viewport.take() {
            viewport.destroy();
        }
        if let Some(fractional) = pane.fractional.take() {
            fractional.destroy();
        }
        tracing::info!(
            "output {}: its {} surface is gone",
            pane.name,
            pane.role.name()
        );
        let sized = pane.role.sizes_the_picture();
        drop(pane);
        if sized {
            self.tell_size();
        }
    }

    fn pane_of_surface(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.shell.wl_surface() == surface)
    }

    fn role_of_surface(&self, surface: &wl_surface::WlSurface) -> Option<Role> {
        self.pane_of_surface(surface)
            .map(|index| self.panes[index].role)
    }

    /// An overlay on every output the canvas has a background on.
    fn raise_overlays(&mut self) {
        let outputs: Vec<wl_output::WlOutput> = self
            .panes
            .iter()
            .filter(|pane| pane.role == Role::Background)
            .map(|pane| pane.output.clone())
            .collect();
        for output in outputs {
            self.add_pane(&output, Role::Overlay);
        }
    }

    /// The prompt's pane on every output the canvas has a background on.
    fn raise_prompts(&mut self) {
        let outputs: Vec<wl_output::WlOutput> = self
            .panes
            .iter()
            .filter(|pane| pane.role == Role::Background)
            .map(|pane| pane.output.clone())
            .collect();
        for output in outputs {
            self.add_pane(&output, Role::Top);
        }
    }

    /// Every prompt pane down, and the keyboard with them.
    fn drop_prompts(&mut self) {
        while let Some(index) = self.panes.iter().position(|pane| pane.role == Role::Top) {
            self.remove_pane(index);
        }
    }

    /// A lock pane on every output there is: the protocol wants one on each,
    /// configured and committed, before it says `locked`.
    fn raise_locks(&mut self) {
        for output in self.output_state.outputs().collect::<Vec<_>>() {
            self.add_pane(&output, Role::Lock);
        }
    }

    /// Every lock pane down, after the unlock.
    fn drop_locks(&mut self) {
        while let Some(index) = self.panes.iter().position(|pane| pane.role == Role::Lock) {
            self.remove_pane(index);
        }
    }

    /// The panes the prompt is drawn on now have something new to draw.
    fn mark_prompt_dirty(&mut self) {
        for pane in self
            .panes
            .iter_mut()
            .filter(|pane| pane.role.carries_prompt())
        {
            pane.dirty = true;
        }
    }

    /// Every overlay down: the outputs are the plain wallpaper again.
    fn drop_overlays(&mut self) {
        while let Some(index) = self
            .panes
            .iter()
            .position(|pane| pane.role == Role::Overlay)
        {
            self.remove_pane(index);
        }
        self.pointer_at = None;
    }

    /// Bring the device up, or give a pane its swapchain, or resize one, for
    /// whatever the last configures and scales asked — **outside** the
    /// dispatch that delivered them, as the bar does (Mesa's WSI makes a
    /// round trip of its own while a surface is created).
    pub fn service(&mut self) -> Result<()> {
        let mut sized = false;
        for index in 0..self.panes.len() {
            let pane = &mut self.panes[index];
            if !pane.configured || !pane.pending {
                continue;
            }
            pane.pending = false;
            pane.dirty = true;
            sized |= pane.role.sizes_the_picture();
            let (width, height) = pane.buffer();
            if self.gpu.is_none() {
                if let Err(err) = self.bring_up(index) {
                    self.exit = Some(Exit::Fault(format!("the device did not come up: {err:#}")));
                    return Ok(());
                }
                continue;
            }
            let gpu = self.gpu.as_ref().expect("checked above");
            let pane = &mut self.panes[index];
            match pane.target.as_mut() {
                Some(target) => {
                    if gpu.resize(target, width, height) {
                        tracing::info!(
                            "output {}: {width}x{height} px for the {}",
                            pane.name,
                            pane.role.name()
                        );
                    }
                }
                None => {
                    let (display, surface) = handles(&self.conn, pane.shell.wl_surface())?;
                    // SAFETY: the connection and the pane's surface outlive
                    // the target, which `remove_pane` drops first.
                    match unsafe { gpu.attach(display, surface, width, height) } {
                        Ok(target) => {
                            tracing::info!(
                                "output {}: {width}x{height} px at scale {}/120 for the {}",
                                pane.name,
                                pane.scale,
                                pane.role.name()
                            );
                            pane.target = Some(target);
                        }
                        Err(err) => tracing::error!(
                            "output {}: no swapchain for the {} ({err:#}); nothing is drawn there",
                            pane.name,
                            pane.role.name()
                        ),
                    }
                }
            }
        }
        if sized {
            self.tell_size();
        }
        Ok(())
    }

    /// The device, on the first configured pane's surface; then what was
    /// waiting for it, and the two requests only a device can make.
    fn bring_up(&mut self, index: usize) -> Result<()> {
        let pane = &self.panes[index];
        let (width, height) = pane.buffer();
        let (display, surface) = handles(&self.conn, pane.shell.wl_surface())?;
        // SAFETY: as in `service`: the connection and the pane's surface
        // outlive the target, and the device is dropped after every target.
        let (mut gpu, target) =
            unsafe { Gpu::new(display, surface, width, height, self.wake.clone()) }?;
        tracing::info!(
            "output {}: {width}x{height} px at scale {}/120 for the {}; the device is up",
            pane.name,
            pane.scale,
            pane.role.name()
        );
        for slot in 0..2u32 {
            if let Some(image) = self.unplaced[slot as usize].take() {
                gpu.upload(slot, &image)?;
            }
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
        }
        self.literal_count = gpu.write_literals(&self.strings);
        let (define, _) = gpu.dst_format_args();
        self.modules.expect(wire::format_code(define));
        // The program this host runs: the format's define, and the canvas's
        // caustic. Remembered by the pipe and replayed to a restarted
        // compiler (docs/abi.md, *Request*).
        if let Some(pipe) = self.pipe(RELOAD) {
            pipe.request(
                &Frame::Request(Request {
                    kind: REQ_PROGRAM,
                    arg: PROGRAM_CANVAS,
                    name: define.to_owned(),
                }),
                "the canvas's program",
            );
        }
        self.panes[index].target = Some(target);
        self.gpu = Some(gpu);
        self.push_facts();
        Ok(())
    }

    /// Tell the wallpaper sidecar the largest output's buffer size when it
    /// changes: the size it fits the next picture to (docs/abi.md,
    /// *Request*, `REQ_OUTPUT_SIZE`). Only the backgrounds count: the
    /// picture is drawn there.
    fn tell_size(&mut self) {
        let largest = self
            .panes
            .iter()
            .filter(|pane| pane.configured && pane.role.sizes_the_picture())
            .map(Pane::buffer)
            .max_by_key(|(w, h)| u64::from(*w) * u64::from(*h));
        let Some((width, height)) = largest else {
            return;
        };
        if self.told == Some((width, height)) {
            return;
        }
        self.told = Some((width, height));
        self.sizes = self.sizes.wrapping_add(1);
        if let Some(pipe) = self.pipe(WALLPAPER) {
            pipe.request(
                &Frame::Request(Request {
                    kind: REQ_OUTPUT_SIZE,
                    arg: self.sizes,
                    name: format!("{width}x{height}"),
                }),
                "the output size",
            );
            tracing::info!("output size: {width}x{height} px, told to {WALLPAPER}");
        }
    }

    // -- the frames --------------------------------------------------------

    /// After every dispatch: the surfaces, then every pane that has
    /// something to draw and may, then the stall timer.
    pub fn after_dispatch(&mut self) -> Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        self.service()?;
        if self.gpu.is_none() {
            return Ok(());
        }
        self.push_pointer();
        let now = Instant::now();
        for index in 0..self.panes.len() {
            let pane = &self.panes[index];
            let wants = pane.dirty || pane.pacing.animating();
            if pane.target.is_some() && wants && pane.pacing.allowed_at(now) {
                self.render(index)?;
            }
        }
        self.arm_stall();
        Ok(())
    }

    fn render(&mut self, index: usize) -> Result<()> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let pane = &mut self.panes[index];
        let Some(target) = pane.target.as_mut() else {
            return Ok(());
        };
        // The pipelines a frame needs are still being built: the pane stays
        // owed, and is drawn when they land (`pipelines_landed`). A lock pane
        // waits here too, and the compositor keeps the session blank until it
        // has a frame.
        if !gpu.can_draw() {
            pane.dirty = true;
            return Ok(());
        }
        let Some(frame) = gpu.acquire(target)? else {
            // Nothing to draw into: the next configure or the stall tries
            // again.
            pane.dirty = true;
            return Ok(());
        };
        // The frame request rides the commit the present makes.
        let wl_surface = pane.shell.wl_surface();
        wl_surface.frame(&self.qh, wl_surface.clone());
        pane.pacing.requested(Instant::now());
        let stamp = gpu.draw(target, frame, pane.scale, pane.role.target(), pane.id)?;
        pane.dirty = false;
        self.frames += 1;
        let mirror = gpu.mirror();
        // Whether the caustic asks for the next frame here, said once each
        // way: a fade, a dim on its way or a wake, until it settles.
        let animating = mirror.wants_redraw != 0;
        pane.run = if animating {
            pane.run.saturating_add(1)
        } else {
            pane.run
        };
        if animating != pane.pacing.animating() {
            if animating {
                tracing::info!(
                    "output {}: the {} draws frame by frame",
                    pane.name,
                    pane.role.name()
                );
            } else {
                tracing::info!(
                    "output {}: the {} is settled; no more frames after {} of them",
                    pane.name,
                    pane.role.name(),
                    pane.run
                );
                pane.run = 0;
            }
        }
        pane.pacing.set_animating(animating);
        pane.pointer_frames = mirror.wants_pointer != 0;
        for note in mirror.witnesses_of(stamp) {
            self.printer.print(note, &self.strings, &[]);
        }
        if mirror.witness_lost > 0 && mirror.witness_frame == stamp {
            tracing::warn!("witness: {} lines lost this frame", mirror.witness_lost);
        }
        let (reached, id) = (mirror.dim_reached, mirror.dim_id);
        let covers: Vec<String> = mirror
            .covers()
            .iter()
            .filter_map(|cover| gpu.cover_bytes(cover.source, cover.off, cover.len))
            .collect();
        if let Some((sidecar, id)) = self.dims.reached(reached, id) {
            self.tell(&sidecar, CANVAS_EVENT_DIMMED, id, &format!("dim {id}"));
            tracing::info!("dim {id}: at its depth, told to {sidecar}");
        }
        for text in covers {
            self.cover(&text);
        }
        Ok(())
    }

    /// A run the font could not draw, out to the glyphs sidecar as the bytes
    /// it was shaped from (docs/abi.md, *Request*), once per string. The
    /// answer is the curve buffers and the arena growing, the run shaping
    /// again and its flag going by itself. The prompt's text is never a
    /// secret, so neither is anything sent here; and the line names no
    /// string, since a prompt's text is the conversation's.
    fn cover(&mut self, text: &str) {
        let Some((sidecar, kind, name)) = self.covers.ask(text) else {
            return;
        };
        if let Some(pipe) = self.pipe(sidecar) {
            let len = name.len();
            pipe.request(&Frame::Request(Request { kind, arg: 0, name }), "a cover");
            tracing::info!("text: asking {sidecar} to cover a run of {len} bytes");
        }
    }

    /// A frame is owed on a pane whose request is out and unanswered: wake
    /// when the request stops holding it, since a compositor that does not
    /// repaint a covered surface sends nothing else.
    fn arm_stall(&mut self) {
        let deadline = self
            .panes
            .iter()
            .filter(|pane| pane.target.is_some() && (pane.dirty || pane.pacing.animating()))
            .filter_map(|pane| pane.pacing.stall_deadline())
            .min();
        if deadline == self.stall_at {
            return;
        }
        let Some(handle) = self.loop_handle.clone() else {
            return;
        };
        if let Some(token) = self.stall_token.take() {
            handle.remove(token);
        }
        self.stall_at = deadline;
        let Some(deadline) = deadline else {
            return;
        };
        let inserted = handle.insert_source(
            Timer::from_deadline(deadline),
            |_, _, canvas: &mut Canvas| {
                // Nothing to do but wake: `after_dispatch` draws what the
                // stall now allows, and re-arms.
                canvas.stall_token = None;
                canvas.stall_at = None;
                TimeoutAction::Drop
            },
        );
        match inserted {
            Ok(token) => self.stall_token = Some(token),
            Err(err) => tracing::warn!("could not arm the stall timer: {err}"),
        }
    }

    // -- the dim -----------------------------------------------------------

    /// One `CanvasEvent` down the stdin of the sidecar whose dim or lock it
    /// is about; `what` names it for a witness.
    fn tell(&self, sidecar: &str, event: u32, id: u32, what: &str) {
        let Some(pipe) = self.pipe(sidecar) else {
            tracing::warn!("{what}: no pipe to {sidecar} for its event");
            return;
        };
        pipe.send_frame(
            &Frame::CanvasEvent(CanvasEventBody {
                event,
                id,
                ..CanvasEventBody::default()
            }),
            &format!("{what}'s event"),
        );
    }

    /// The dim is over without an undim: `facts` says so, and every overlay
    /// goes.
    fn end_dim(&mut self) {
        self.slots.clear_dim();
        self.push_facts();
        self.drop_overlays();
    }

    /// Input while a dim is up: tell its sidecar, and take the overlays down.
    /// Nothing while no dim is up, so a burst of events cancels once.
    fn input(&mut self, what: &str) {
        let Some(dim) = self.dims.cancel() else {
            return;
        };
        self.tell(
            &dim.sidecar,
            CANVAS_EVENT_INPUT,
            dim.id,
            &format!("dim {}", dim.id),
        );
        tracing::info!(
            "dim {}: cancelled by {what}; told to {}, the overlays go",
            dim.id,
            dim.sidecar
        );
        self.end_dim();
    }

    // -- the keyboard --------------------------------------------------------

    /// The seat's keymap: compiled by libxkbcommon, opened the first time one
    /// arrives. Without the library the canvas still reads the fact of a key,
    /// and the named keys by their codes (`keys::key_of_code`).
    fn keymap(&mut self, fd: std::os::fd::OwnedFd, size: u32) {
        if self.xkb.is_none() {
            self.xkb = Xkb::new();
        }
        let Some(xkb) = self.xkb.as_mut() else {
            if !std::mem::replace(&mut self.xkb_missing_said, true) {
                tracing::warn!(
                    "seat: libxkbcommon could not be opened; keys are read by their codes, \
                     and a prompt can be dismissed or submitted but typed into only with it"
                );
            }
            return;
        };
        match xkb.set_keymap(fd, size) {
            Ok(()) => tracing::info!("seat: the keymap is compiled ({size} bytes)"),
            Err(why) => tracing::warn!("seat: the keymap was refused: {why}"),
        }
    }

    /// One key pressed on one of the canvas's surfaces. What it means is the
    /// keymap's; what is done with it is the dim's: any key is a user who is
    /// back.
    fn key_pressed(&mut self, code: u32) {
        let key = match self.xkb.as_mut().filter(|xkb| xkb.ready()) {
            Some(xkb) => xkb.press(code),
            None => keys::key_of_code(code),
        };
        // While a prompt is up its keys are its own, and a dim also up stays
        // as it is.
        if self.prompts.takes_keys() {
            if let Some(key) = key {
                let effects = self.prompts.key(key);
                self.apply_prompt(effects);
            }
            return;
        }
        self.input("a key");
    }

    // -- the prompt --------------------------------------------------------

    /// What the prompt core said to do (`prompt::Effect`): answers down the
    /// asking sidecar's pipe, the panes up or down, `facts` written again.
    fn apply_prompt(&mut self, effects: Vec<Effect>) {
        let mut changed = false;
        for effect in effects {
            match effect {
                Effect::Answer { sidecar, answer } => {
                    let id = answer.id();
                    match self.pipe(&sidecar) {
                        // The frame owns the answer and drops with this
                        // statement, and its bytes are zeroed with it.
                        Some(pipe) => {
                            pipe.send_frame(
                                &Frame::Answer(answer),
                                &format!("conversation {id}'s answer"),
                            );
                        }
                        None => tracing::warn!(
                            "prompt: no pipe to {sidecar} for conversation {id}'s answer"
                        ),
                    }
                }
                // While the lock holds, the prompt is the lock panes': they
                // are redrawn, and no top pane is made or destroyed (one
                // would be hidden under the lock anyway).
                Effect::Raise | Effect::Lower if self.locks.held() => {
                    self.mark_dirty(Role::Lock);
                }
                Effect::Raise => {
                    self.raise_prompts();
                    // An overlay stands aside while a prompt is up.
                    self.mark_dirty(Role::Overlay);
                }
                Effect::Lower => {
                    self.drop_prompts();
                    self.mark_dirty(Role::Overlay);
                }
                Effect::Changed => changed = true,
                Effect::Note(line) => tracing::info!("{line}"),
            }
        }
        if changed {
            self.push_facts();
            self.mark_prompt_dirty();
        }
    }

    // -- the lock ------------------------------------------------------------

    /// What the lock's state said to do (`lock::Effect`).
    fn apply_lock(&mut self, effects: Vec<lock::Effect>) {
        for effect in effects {
            match effect {
                lock::Effect::Lock => match self.session_lock.lock(&self.qh) {
                    Ok(session_lock) => {
                        self.lock = Some(session_lock);
                        // The prompt moves to the lock panes: a top pane up
                        // now (another sink's, dismissed by the reservation
                        // just before) would be hidden under the lock, and
                        // would outlive it.
                        self.drop_prompts();
                        self.raise_locks();
                        tracing::info!(
                            "lock: ext_session_lock_v1 asked for, with {} lock surfaces",
                            self.panes
                                .iter()
                                .filter(|pane| pane.role == Role::Lock)
                                .count()
                        );
                    }
                    Err(_) => {
                        let more = self.locks.refused();
                        self.apply_lock(more);
                    }
                },
                lock::Effect::Unlock => {
                    if let Some(session_lock) = self.lock.take() {
                        // `unlock_and_destroy`; the drop's `destroy` after it
                        // is no request at all.
                        session_lock.unlock();
                    }
                    self.drop_locks();
                    // The unlock is told when the compositor has read it.
                    self.conn.display().sync(&self.qh, UnlockSync);
                    // A conversation still up goes back to the top layer.
                    if self.prompts.raised() {
                        self.raise_prompts();
                    }
                }
                lock::Effect::Tell { sidecar, event } => {
                    self.tell(&sidecar, event, 0, "the lock");
                }
                lock::Effect::EndDim => {
                    if let Some(dim) = self.dims.end() {
                        tracing::info!(
                            "dim {}: ended by the lock; {} is told nothing, the overlays go",
                            dim.id,
                            dim.sidecar
                        );
                        self.end_dim();
                    }
                }
                lock::Effect::Reserve(sidecar) => {
                    let effects = self.prompts.reserve(sidecar.as_deref());
                    self.apply_prompt(effects);
                }
                lock::Effect::Note(line) => tracing::info!("{line}"),
                lock::Effect::Refused(line) => tracing::warn!("{line}"),
                lock::Effect::Fatal(line) => {
                    if self.exit.is_none() {
                        self.exit = Some(Exit::Fault(line));
                    }
                }
            }
        }
    }

    // -- the sidecars ------------------------------------------------------

    /// A dim without its owner is undone: a sidecar that exits mid-dim, by a
    /// crash or a kill, never leaves a dark screen.
    fn dim_owner_gone(&mut self, name: &str) {
        if let Some(dim) = self.dims.gone(name) {
            tracing::info!("sidecar {name}: gone mid-dim; dim {} goes with it", dim.id);
            self.end_dim();
        }
    }

    /// One event from a sidecar's pipe.
    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::Absent(name) => tracing::info!("sidecar {name} is absent"),
            Event::Gone(name) => {
                // The picture on screen stays: it is the host's now. One
                // received and never shown is not going to be.
                if name == WALLPAPER
                    && let Some(image) = self.slots.forget_waiting()
                {
                    tracing::info!("sidecar {name}: gone; {:?} was never shown", image.key);
                }
                self.dim_owner_gone(&name);
                let effects = self.prompts.gone(&name);
                self.apply_prompt(effects);
                let effects = self.locks.gone(&name);
                self.apply_lock(effects);
            }
            // An ordinary exit, before the restart: the picture stays (it is
            // the host's), a waiting image stays for the show that may still
            // come, and the dim and the prompt go.
            Event::Exited(name) => {
                self.dim_owner_gone(&name);
                let effects = self.prompts.gone(&name);
                self.apply_prompt(effects);
                let effects = self.locks.gone(&name);
                self.apply_lock(effects);
            }
            Event::Frame { sidecar, frame } => self.apply_frame(&sidecar, frame),
        }
    }

    fn apply_frame(&mut self, sidecar: &str, frame: Frame) {
        match frame {
            Frame::Image(image) => {
                tracing::info!(
                    "sidecar {sidecar}: image {:?}, {}x{} ({} KiB), waiting for its show",
                    image.key,
                    image.width,
                    image.height,
                    image.rgba.len() / 1024
                );
                if let Some(unshown) = self.slots.hold(image) {
                    tracing::info!(
                        "sidecar {sidecar}: image {:?} was replaced before its show",
                        unshown.key
                    );
                }
            }
            Frame::Canvas(command) => self.apply_command(sidecar, &command),
            // A sink asking for a typed answer, or showing a line, or ending
            // its conversation (docs/abi.md, *Prompt*).
            Frame::Prompt(prompt) => {
                let effects = self.prompts.frame(sidecar, &prompt);
                self.apply_prompt(effects);
            }
            // The lock sidecar asking to lock or to unlock (docs/abi.md,
            // *Lock*). The lock goes out whether or not the device is up:
            // the compositor holds the session locked and blank until the
            // lock panes have frames.
            Frame::Lock(body) => {
                let effects = self.locks.request(sidecar, body.command);
                self.apply_lock(effects);
            }
            // A run of the text engine's buffers: copied, never read
            // (docs/abi.md, *Bytes*).
            Frame::Bytes(bytes) => match self.gpu.as_mut() {
                Some(gpu) => {
                    gpu.write_bytes(bytes.buffer, bytes.offset, bytes.total, &bytes.bytes);
                    self.mark_prompt_dirty();
                }
                None => self.pending_bytes.push(bytes),
            },
            // A different glyph space: what was covered in the last one is
            // asked again as it comes up. The canvas snaps nothing to the cap
            // height, so the frame is a line and nothing else.
            Frame::Font(font) => {
                tracing::info!(
                    "font: {} ({} glyphs at {} upem) from {sidecar}",
                    font.family,
                    font.glyph_count,
                    font.upem
                );
                self.covers.forget();
            }
            Frame::Module(module) => self.apply_module(sidecar, module),
            other => tracing::info!(
                "sidecar {sidecar}: frame kind {} has no consumer here",
                other.kind()
            ),
        }
    }

    fn apply_command(&mut self, sidecar: &str, command: &Command) {
        match self.slots.apply(command) {
            Ok(Applied::Upload { slot, image }) => {
                let body = command.body();
                tracing::info!(
                    "sidecar {sidecar}: show {:?} into slot {slot} ({}x{}), over {} ms",
                    image.key,
                    image.width,
                    image.height,
                    body.duration_ms
                );
                match self.gpu.as_mut() {
                    Some(gpu) => {
                        if let Err(err) = gpu.upload(slot, &image) {
                            tracing::error!(
                                "sidecar {sidecar}: {err:#}; the slot keeps its picture"
                            );
                        }
                    }
                    None => self.unplaced[slot as usize] = Some(image),
                }
            }
            Ok(Applied::Facts) => tracing::info!(
                "sidecar {sidecar}: canvas command {}",
                command.body().command
            ),
            Err(why) => {
                tracing::warn!("sidecar {sidecar}: {why}");
                return;
            }
        }
        self.push_facts();
        match command {
            Command::Show { .. } => self.mark_dirty(Role::Background),
            Command::Dim { id, .. } => {
                if let Some(replaced) = self.dims.begin(sidecar, *id) {
                    tracing::info!("dim {}: replaced by dim {id}", replaced.id);
                }
                self.raise_overlays();
                self.mark_dirty(Role::Overlay);
            }
            Command::Undim => {
                self.dims.end();
                self.drop_overlays();
            }
        }
    }

    /// One entry point of a fresh compile, and the swap when it completes a
    /// generation (docs/abi.md, *Module*): all or nothing, the canvas's
    /// buffers and so its fade untouched.
    fn apply_module(&mut self, sidecar: &str, module: Module) {
        let ready = match self.modules.collect(module) {
            Collected::Waiting { .. } => return,
            Collected::Refused(why) => {
                tracing::warn!("sidecar {sidecar}: {why}; keeping the old canvas");
                return;
            }
            Collected::Ready(ready) => ready,
        };
        let generation = ready.generation;
        let reflection = match ready.reflection() {
            Ok(reflection) => reflection,
            Err(err) => {
                tracing::error!(
                    "sidecar {sidecar}: generation {generation} refused: {err}; keeping the old canvas"
                );
                return;
            }
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        match gpu.reload(&reflection, &ready.spirv()) {
            Ok(token) => {
                if let Some(old) = self.reloading.take() {
                    tracing::info!(
                        "sidecar {}: generation {} superseded by {generation} before it was built",
                        old.sidecar,
                        old.generation
                    );
                }
                self.reloading = Some(Reloading {
                    token,
                    sidecar: sidecar.to_owned(),
                    generation,
                    modules: ready.len(),
                    bytes: ready.bytes(),
                    reflection,
                    started: Instant::now(),
                });
            }
            Err(err) => tracing::error!(
                "sidecar {sidecar}: generation {generation} refused: {err}; keeping the old canvas"
            ),
        }
    }

    /// The compile workers' wake: install what landed. A pipeline of the
    /// running program owes every pane a frame (the first one drawn is the
    /// first frame there is); a reload swapped in brings its literals and
    /// draws everything again; a refusal keeps the canvas that runs.
    pub fn pipelines_landed(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        for landed in gpu.poll_pipelines() {
            match landed {
                Landed::Live { .. } => self.mark_all_dirty(),
                Landed::Broken(err) => {
                    self.exit = Some(Exit::Fault(format!(
                        "the canvas caustic baked in at build time does not load: {err}"
                    )));
                }
                Landed::Swapped { generation } => {
                    let Some(reload) = self.reloading.take_if(|r| r.token == generation) else {
                        continue;
                    };
                    self.strings.replace(reload.reflection.strings);
                    if let Some(gpu) = self.gpu.as_mut() {
                        self.literal_count = gpu.write_literals(&self.strings);
                    }
                    self.printer.reset();
                    tracing::info!(
                        "sidecar {}: generation {}, {} modules, swapped ({} KiB, {} ms)",
                        reload.sidecar,
                        reload.generation,
                        reload.modules,
                        reload.bytes / 1024,
                        reload.started.elapsed().as_millis()
                    );
                    self.mark_all_dirty();
                    self.push_facts();
                }
                Landed::Refused { generation, error } => {
                    let Some(reload) = self.reloading.take_if(|r| r.token == generation) else {
                        continue;
                    };
                    tracing::error!(
                        "sidecar {}: generation {} refused: {error}; keeping the old canvas",
                        reload.sidecar,
                        reload.generation
                    );
                }
            }
        }
    }
}

/// A generation queued on the compile workers: the program's number for it,
/// and what the swap line and the swap itself need from the frames.
struct Reloading {
    token: u64,
    sidecar: String,
    generation: u32,
    modules: usize,
    bytes: usize,
    /// The compile's reflection: its literals are the ones the mirror's
    /// hashes mean once it is swapped in, and not before.
    reflection: punchcutter_slang::Reflection,
    started: Instant,
}

/// The raw `wl_display` and `wl_surface` a wgpu surface is made from.
fn handles(
    conn: &Connection,
    surface: &wl_surface::WlSurface,
) -> Result<(NonNull<std::ffi::c_void>, NonNull<std::ffi::c_void>)> {
    let display = NonNull::new(conn.backend().display_ptr().cast())
        .context("the wl_display has no pointer")?;
    let surface = NonNull::new(surface.id().as_ptr().cast())
        .context("the pane's wl_surface has no pointer")?;
    Ok((display, surface))
}

impl LayerShellHandler for Canvas {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        let Some(index) = self.pane_of_surface(layer.wl_surface()) else {
            return;
        };
        let role = self.panes[index].role;
        tracing::info!(
            "output {}: the compositor closed its {} surface",
            self.panes[index].name,
            role.name()
        );
        self.remove_pane(index);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.configured(layer.wl_surface(), configure.new_size);
    }
}

impl Canvas {
    /// A pane's configure, a layer surface's or a lock surface's (sctk has
    /// acknowledged it): the size, logical px, which the next frame's
    /// viewport and buffer follow.
    fn configured(&mut self, surface: &wl_surface::WlSurface, size: (u32, u32)) {
        let Some(index) = self.pane_of_surface(surface) else {
            return;
        };
        let pane = &mut self.panes[index];
        let (width, height) = (size.0.max(1), size.1.max(1));
        let first = !pane.configured;
        pane.configured = true;
        if pane.logical != (width, height) || first {
            pane.logical = (width, height);
            pane.pending = true;
        }
        if let Some(viewport) = &pane.viewport {
            viewport.set_destination(width as i32, height as i32);
        }
        pane.dirty = true;
        tracing::info!(
            logical = format!("{width}x{height}"),
            scale = pane.scale,
            first,
            "output {}: {} surface configured",
            pane.name,
            pane.role.name()
        );
    }
}

impl SessionLockHandler for Canvas {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        let effects = self.locks.locked();
        self.apply_lock(effects);
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        let effects = self.locks.finished();
        self.apply_lock(effects);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        self.configured(surface.wl_surface(), configure.new_size);
    }
}

/// The `wl_display.sync` after an `unlock_and_destroy`: its answer is the
/// compositor having read the unlock, which is when `unlocked` is told.
pub struct UnlockSync;

impl Dispatch<wl_callback::WlCallback, UnlockSync> for Canvas {
    fn event(
        canvas: &mut Canvas,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &UnlockSync,
        _: &Connection,
        _: &QueueHandle<Canvas>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            let effects = canvas.locks.synced();
            canvas.apply_lock(effects);
        }
    }
}

/// A host that leaves while the session is locked leaves it locked: the
/// lock object is forgotten rather than dropped, since its drop's `destroy`
/// after `locked` is a protocol error, and a compositor keeps a session
/// locked when its lock client goes (`ext_session_lock_v1`). Locking but not
/// yet locked, the drop's `destroy` is the protocol's own way out.
impl Drop for Canvas {
    fn drop(&mut self) {
        if let Some(session_lock) = self.lock.take()
            && session_lock.is_locked()
        {
            std::mem::forget(session_lock);
        }
    }
}

impl Dispatch<WpFractionalScaleV1, wl_surface::WlSurface> for Canvas {
    fn event(
        canvas: &mut Canvas,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        _: &QueueHandle<Canvas>,
    ) {
        let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else {
            return;
        };
        let Some(index) = canvas.pane_of_surface(surface) else {
            return;
        };
        let pane = &mut canvas.panes[index];
        if pane.scale != scale {
            tracing::info!(
                "output {}: {} preferred scale {scale}/120 (was {})",
                pane.name,
                pane.role.name(),
                pane.scale
            );
            pane.scale = scale.max(1);
            pane.pending = true;
            pane.dirty = true;
        }
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for Canvas {
    fn event(
        _: &mut Canvas,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Canvas>,
    ) {
    }
}

impl Dispatch<WpViewporter, ()> for Canvas {
    fn event(
        _: &mut Canvas,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Canvas>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for Canvas {
    fn event(
        _: &mut Canvas,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Canvas>,
    ) {
    }
}

impl CompositorHandler for Canvas {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        // The integer path only: with the fractional scale the compositor's
        // preferred scale is the one.
        if self.uses_fractional() {
            return;
        }
        let Some(index) = self.pane_of_surface(surface) else {
            return;
        };
        let scale = new_factor.max(1) as u32 * SCALE_ONE;
        let pane = &mut self.panes[index];
        if pane.scale != scale {
            pane.shell.wl_surface().set_buffer_scale(new_factor.max(1));
            pane.scale = scale;
            pane.pending = true;
            pane.dirty = true;
        }
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
        let Some(index) = self.pane_of_surface(surface) else {
            return;
        };
        let pane = &mut self.panes[index];
        // A fade or a dim in progress is a frame per repaint; a settled
        // surface draws nothing until something changes.
        if pane.pacing.answered() {
            pane.dirty = true;
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Canvas {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.add_pane(&output, Role::Background);
        // An output that appears mid-dim is dimmed too, one that appears
        // while locked is given a lock surface (the protocol asks one for
        // every output), and one that appears mid-prompt is given the prompt
        // unless the lock panes carry it.
        if self.dims.is_up() {
            self.add_pane(&output, Role::Overlay);
        }
        if self.locks.held() {
            self.add_pane(&output, Role::Lock);
        } else if self.prompts.raised() {
            self.add_pane(&output, Role::Top);
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        while let Some(index) = self.panes.iter().position(|pane| pane.output == output) {
            self.remove_pane(index);
        }
    }
}

// -- the seat -----------------------------------------------------------------

impl SeatHandler for Canvas {
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
        match capability {
            Capability::Pointer if self.pointer.is_none() => {
                match self.seat_state.get_pointer(qh, &seat) {
                    Ok(pointer) => {
                        tracing::info!("seat: pointer bound");
                        self.pointer = Some(pointer);
                    }
                    Err(err) => tracing::warn!("seat: could not get the pointer: {err}"),
                }
            }
            // Bound plainly: the keymap the compositor sends with it is
            // compiled by libxkbcommon when it arrives (`keymap`), with no
            // repeat, since a held key is one key to a prompt.
            Capability::Keyboard if self.keyboard.is_none() => {
                tracing::info!("seat: keyboard bound");
                self.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            Capability::Touch if self.touch.is_none() => {
                tracing::info!("seat: touch bound");
                self.touch = Some(seat.get_touch(qh, ()));
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    keyboard.release();
                }
            }
            Capability::Touch => {
                if let Some(touch) = self.touch.take() {
                    touch.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Canvas {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let mut cancel = None;
        // While a prompt is up the pointer cancels nothing: a dim also up
        // stays as it is.
        let prompting = self.prompts.raised();
        for event in events {
            let index = self.pane_of_surface(&event.surface);
            let role = index.map(|index| self.panes[index].role);
            // The wake's: where the pointer is on a pane that stirs, and its
            // presses there. Nothing else in this handler reads it.
            if let Some(index) = index
                && self.panes[index].role.stirs()
            {
                let (id, scale) = (self.panes[index].id, self.panes[index].scale);
                let news = match event.kind {
                    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                        self.wake_pointer.at(id, event.position, scale)
                    }
                    PointerEventKind::Leave { .. } => self.wake_pointer.left(id),
                    PointerEventKind::Press { .. } => self.wake_pointer.pressed(id),
                    _ => false,
                };
                self.stirred(index, news);
            }
            if (role == Some(Role::Lock) || prompting && role == Some(Role::Top))
                && let PointerEventKind::Enter { serial } = event.kind
            {
                // No cursor over the prompt or the lock either: neither
                // takes clicks.
                pointer.set_cursor(serial, None, 0, 0);
            }
            let on_overlay = !prompting && role.is_some_and(Role::input_cancels);
            if !on_overlay {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    // No cursor over the dim, as chayang's; and arriving is
                    // not moving.
                    pointer.set_cursor(serial, None, 0, 0);
                    self.pointer_at = Some(event.position);
                }
                PointerEventKind::Leave { .. } => self.pointer_at = None,
                PointerEventKind::Motion { .. } => {
                    if moved(self.pointer_at, event.position) {
                        cancel.get_or_insert("the pointer moving");
                    }
                    self.pointer_at = Some(event.position);
                }
                PointerEventKind::Press { .. } => {
                    cancel.get_or_insert("a button");
                }
                PointerEventKind::Release { .. } => {}
                PointerEventKind::Axis { .. } => {
                    cancel.get_or_insert("a scroll");
                }
            }
        }
        if let Some(what) = cancel {
            self.input(what);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Canvas {
    fn event(
        canvas: &mut Canvas,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Canvas>,
    ) {
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => canvas.keymap(fd, size),
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb) = canvas.xkb.as_mut() {
                    xkb.modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => canvas.key_pressed(key),
            // The keyboard arriving on a prompt's pane, or a lock pane, is
            // when typing can begin; arriving on an overlay, it is what lets
            // a key cancel the dim. Said, once each time, so the journal
            // shows where a key went.
            wl_keyboard::Event::Enter { surface, .. } => {
                if let Some(index) = canvas.pane_of_surface(&surface)
                    && (canvas.panes[index].role.carries_prompt()
                        || canvas.panes[index].role == Role::Overlay)
                {
                    tracing::info!(
                        "seat: the keyboard is on the {} (output {})",
                        canvas.panes[index].role.name(),
                        canvas.panes[index].name
                    );
                }
            }
            // Leave, releases and the repeat rate mean nothing here:
            // the keyboard is only ever on the canvas's own surfaces, and a
            // held key is one key.
            _ => {}
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for Canvas {
    fn event(
        canvas: &mut Canvas,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Canvas>,
    ) {
        if let wl_touch::Event::Down { surface, .. } = event
            && !canvas.prompts.raised()
            && canvas
                .role_of_surface(&surface)
                .is_some_and(Role::input_cancels)
        {
            canvas.input("a touch");
        }
    }
}

impl ProvidesRegistryState for Canvas {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Canvas);
delegate_output!(Canvas);
delegate_layer!(Canvas);
delegate_registry!(Canvas);
delegate_seat!(Canvas);
delegate_pointer!(Canvas);
delegate_session_lock!(Canvas);

#[cfg(test)]
mod tests;
