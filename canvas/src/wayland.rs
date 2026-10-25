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
//! cursor is hidden there and only a motion to somewhere else cancels. No
//! keymap is read: the fact of a key is all a cancel needs.
//!
//! # The frames
//!
//! A frame is one pane's. It is drawn when that pane is dirty (a configure, a
//! command, a reload) or the caustic said `wants_redraw` for it, and its frame
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
use caustics_bar_abi::wire::{
    self, CANVAS_EVENT_DIMMED, CANVAS_EVENT_INPUT, Canvas as Command, CanvasEventBody, Frame,
    Module, PROGRAM_CANVAS, REQ_OUTPUT_SIZE, REQ_PROGRAM, Request,
};
use caustics_bar_abi::{CANVAS_ENTRIES, wire::Image};
use caustics_host::modules::{Collected, Modules};
use caustics_host::pacing::Pacing;
use caustics_host::pipe::{Event, Pipe};
use caustics_host::strings::Strings;
use caustics_host::surface::Surface;
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
    delegate_seat,
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
    shell::{
        WaylandSurface,
        wlr_layer::{LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::GlobalList,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface, wl_touch},
};

use crate::dim::Dims;
use crate::gpu::{self, Gpu};
use crate::role::Role;
use crate::sidecars::{self, RELOAD, WALLPAPER};
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

/// One output's surface in one role.
struct Pane {
    /// Dropped first, before the surface its raw pointers name.
    target: Option<Surface>,
    role: Role,
    output: wl_output::WlOutput,
    /// The output's name, for the journal.
    name: String,
    layer: LayerSurface,
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
    /// The seat's devices, as it offers them.
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    touch: Option<wl_touch::WlTouch>,
    /// Where the pointer last was on an overlay, surface-local: what a
    /// motion is measured from.
    pointer_at: Option<(f64, f64)>,
    /// Pictures placed before the device existed, by slot.
    unplaced: [Option<Image>; 2],
    /// The size last told the wallpaper sidecar, and how many sizes have been.
    told: Option<(u32, u32)>,
    sizes: u32,
    loop_handle: Option<LoopHandle<'static, Canvas>>,
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
    ) -> Result<Canvas> {
        let compositor =
            CompositorState::bind(globals, &qh).context("wl_compositor is not available")?;
        let layer_shell =
            LayerShell::bind(globals, &qh).context("zwlr_layer_shell_v1 is not available")?;
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
            pointer: None,
            keyboard: None,
            touch: None,
            pointer_at: None,
            unplaced: [None, None],
            told: None,
            sizes: 0,
            loop_handle: None,
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

    /// A pane for an output, in a role, committed with nothing attached so
    /// the compositor configures it.
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
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            role.layer(),
            Some(role.namespace()),
            Some(output),
        );
        layer.set_anchor(role.anchor());
        layer.set_exclusive_zone(role.exclusive_zone());
        layer.set_keyboard_interactivity(role.keyboard());
        layer.set_size(0, 0);
        let (fractional, viewport) = if self.uses_fractional() {
            let wl_surface = layer.wl_surface();
            (
                self.fractional.as_ref().map(|manager| {
                    manager.get_fractional_scale(wl_surface, &self.qh, wl_surface.clone())
                }),
                self.viewporter
                    .as_ref()
                    .map(|viewporter| viewporter.get_viewport(wl_surface, &self.qh, ())),
            )
        } else {
            layer.wl_surface().set_buffer_scale(integer as i32);
            (None, None)
        };
        layer.commit();
        tracing::info!(
            "output {name}: a{} {} surface (integer scale {integer} until the compositor \
             says otherwise)",
            if role == Role::Overlay { "n" } else { "" },
            role.name()
        );
        self.panes.push(Pane {
            target: None,
            role,
            output: output.clone(),
            name,
            layer,
            fractional,
            viewport,
            logical: (0, 0),
            scale: integer * SCALE_ONE,
            configured: false,
            pending: false,
            dirty: false,
            pacing: Pacing::new(Instant::now()),
        });
    }

    /// The output went, or the compositor closed the surface, or the dim is
    /// over: the swapchain first, then the objects it points at, then the
    /// layer surface (destroyed as it drops).
    fn remove_pane(&mut self, index: usize) {
        let mut pane = self.panes.remove(index);
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
            .position(|pane| pane.layer.wl_surface() == surface)
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
                    let (display, surface) = handles(&self.conn, &pane.layer)?;
                    // SAFETY: the connection and the layer surface outlive
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
        let (display, surface) = handles(&self.conn, &pane.layer)?;
        // SAFETY: as in `service`: the connection and the layer surface
        // outlive the target, and the device is dropped after every target.
        let (mut gpu, target) = unsafe { Gpu::new(display, surface, width, height) }?;
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
        gpu.write_facts(self.slots.facts());
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
        let Some(frame) = gpu.acquire(target)? else {
            // Nothing to draw into: the next configure or the stall tries
            // again.
            pane.dirty = true;
            return Ok(());
        };
        // The frame request rides the commit the present makes.
        let wl_surface = pane.layer.wl_surface();
        wl_surface.frame(&self.qh, wl_surface.clone());
        pane.pacing.requested(Instant::now());
        let stamp = gpu.draw(target, frame, pane.scale, pane.role.target())?;
        pane.dirty = false;
        self.frames += 1;
        let mirror = gpu.mirror();
        pane.pacing.set_animating(mirror.wants_redraw != 0);
        for note in mirror.witnesses_of(stamp) {
            self.printer.print(note, &self.strings, &[]);
        }
        if mirror.witness_lost > 0 && mirror.witness_frame == stamp {
            tracing::warn!("witness: {} lines lost this frame", mirror.witness_lost);
        }
        let (reached, id) = (mirror.dim_reached, mirror.dim_id);
        if let Some((sidecar, id)) = self.dims.reached(reached, id) {
            self.tell(&sidecar, CANVAS_EVENT_DIMMED, id);
            tracing::info!("dim {id}: at its depth, told to {sidecar}");
        }
        Ok(())
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

    /// One `CanvasEvent` down the stdin of the sidecar whose dim it is about.
    fn tell(&self, sidecar: &str, event: u32, id: u32) {
        let Some(pipe) = self.pipe(sidecar) else {
            tracing::warn!("dim {id}: no pipe to {sidecar} for its event");
            return;
        };
        pipe.send_frame(
            &Frame::CanvasEvent(CanvasEventBody {
                event,
                id,
                ..CanvasEventBody::default()
            }),
            &format!("dim {id}'s event"),
        );
    }

    /// The dim is over without an undim: `facts` says so, and every overlay
    /// goes.
    fn end_dim(&mut self) {
        self.slots.clear_dim();
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.write_facts(self.slots.facts());
        }
        self.drop_overlays();
    }

    /// Input while a dim is up: tell its sidecar, and take the overlays down.
    /// Nothing while no dim is up, so a burst of events cancels once.
    fn input(&mut self, what: &str) {
        let Some(dim) = self.dims.cancel() else {
            return;
        };
        self.tell(&dim.sidecar, CANVAS_EVENT_INPUT, dim.id);
        tracing::info!(
            "dim {}: cancelled by {what}; told to {}, the overlays go",
            dim.id,
            dim.sidecar
        );
        self.end_dim();
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
            }
            // An ordinary exit, before the restart: the picture stays (it is
            // the host's), a waiting image stays for the show that may still
            // come, and the dim goes.
            Event::Exited(name) => self.dim_owner_gone(&name),
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
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.write_facts(self.slots.facts());
        }
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
        let started = Instant::now();
        match gpu.reload(&reflection, &ready.spirv()) {
            Ok(()) => {
                self.strings.replace(reflection.strings);
                self.printer.reset();
                tracing::info!(
                    "sidecar {sidecar}: generation {generation}, {} modules, swapped ({} KiB, {} ms)",
                    ready.len(),
                    ready.bytes() / 1024,
                    started.elapsed().as_millis()
                );
                self.mark_all_dirty();
            }
            Err(err) => tracing::error!(
                "sidecar {sidecar}: generation {generation} refused: {err}; keeping the old canvas"
            ),
        }
    }
}

/// The raw `wl_display` and `wl_surface` a wgpu surface is made from.
fn handles(
    conn: &Connection,
    layer: &LayerSurface,
) -> Result<(NonNull<std::ffi::c_void>, NonNull<std::ffi::c_void>)> {
    let display = NonNull::new(conn.backend().display_ptr().cast())
        .context("the wl_display has no pointer")?;
    let surface = NonNull::new(layer.wl_surface().id().as_ptr().cast())
        .context("the layer's wl_surface has no pointer")?;
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
        let Some(index) = self.pane_of_surface(layer.wl_surface()) else {
            return;
        };
        let pane = &mut self.panes[index];
        let (width, height) = (configure.new_size.0.max(1), configure.new_size.1.max(1));
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
            pane.layer.wl_surface().set_buffer_scale(new_factor.max(1));
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
        // An output that appears mid-dim is dimmed too.
        if self.dims.is_up() {
            self.add_pane(&output, Role::Overlay);
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
            // Bound plainly: the fact of a key is all the canvas reads, so
            // there is no keymap to compile and no xkbcommon.
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
        for event in events {
            let on_overlay = self
                .role_of_surface(&event.surface)
                .is_some_and(Role::input_cancels);
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
        // The keymap's fd closes as the event drops; enter, leave, the
        // modifiers and releases mean nothing here. Only the overlay takes
        // the keyboard, so a key pressed is a key pressed on the dim.
        if let wl_keyboard::Event::Key {
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            canvas.input("a key");
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

#[cfg(test)]
mod tests;
