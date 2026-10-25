//! The canvas's Wayland side: one background surface per output, and the
//! meeting point of everything else the host is made of.
//!
//! Everything here passes the membership test the bar's process does
//! (docs/architecture.md, *The bar process*): it owns a Wayland object, sits
//! on a configure round trip, or routes a sidecar's frame to where its bytes
//! go. Nothing here decides what the canvas looks like: a picture is copied
//! into a slot when its show arrives, a command's head is copied into
//! `facts`, and the caustic says how far a fade is and whether it wants
//! another frame (docs/abi.md, *The canvas's buffer*).
//!
//! # The surfaces
//!
//! One `zwlr_layer_shell_v1` surface per output on the **background** layer,
//! anchored to all four edges, exclusive zone −1 (it never moves another
//! surface), no keyboard, namespace `caustics-canvas`: made when the output
//! appears and gone when it goes. Each has a `wp_fractional_scale_v1` and a
//! `wp_viewport` from its first commit (docs/canvas.md, *Surfaces*): the
//! buffer is the logical size times the scale the compositor prefers, and the
//! viewport's destination is the logical size, so a 1.5 output gets a buffer
//! of its own pixels rather than a 2× one scaled down. A compositor without
//! the fractional-scale global gets the integer scale, said in one line.
//!
//! # The frames
//!
//! A frame is one output's. It is drawn when that output is dirty (a
//! configure, a command, a reload) or the caustic said `wants_redraw`, and
//! its frame request is answered or [`FRAME_STALL`] old: each output is paced
//! on its own `wl_surface.frame`, made before the present so it rides the
//! commit (docs/architecture.md, *The frame*). A compositor that sends no
//! callbacks to a surface it does not repaint still gets a frame every
//! stall, from a timer: that is the pacing default docs/canvas.md, *Time*,
//! asks for.
//!
//! [`FRAME_STALL`]: caustics_host::pacing::FRAME_STALL

use std::collections::HashMap;
use std::ptr::NonNull;
use std::time::Instant;

use anyhow::{Context, Result};
use caustics_bar_abi::wire::{
    self, Frame, ImageAssembly, ModuleChunk, PROGRAM_CANVAS, REQ_OUTPUT_SIZE, REQ_PROGRAM, Request,
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
    delegate_compositor, delegate_layer, delegate_output, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::calloop::{
        LoopHandle, RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::GlobalList,
    protocol::{wl_output, wl_surface},
};

use crate::gpu::{self, Gpu};
use crate::sidecars::{self, RELOAD, WALLPAPER};
use crate::slots::{Applied, Slots};

/// The layer surfaces' namespace: what a compositor's rules and `niri msg
/// layers` call the canvas.
pub const NAMESPACE: &str = "caustics-canvas";

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

/// One output and its background surface.
struct Screen {
    /// Dropped first, before the surface its raw pointers name.
    target: Option<Surface>,
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
    /// Something changed that this output has not drawn.
    dirty: bool,
    pacing: Pacing,
}

impl Screen {
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
    compositor: CompositorState,
    layer_shell: LayerShell,
    fractional: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    conn: Connection,
    qh: QueueHandle<Canvas>,
    screens: Vec<Screen>,
    /// The device, once the first output has been configured. Dropped after
    /// every screen's target (declared after `screens`).
    gpu: Option<Gpu>,
    pipes: Vec<Pipe>,
    modules: Modules,
    strings: Strings,
    printer: Printer,
    slots: Slots,
    /// Each sidecar's image in pieces.
    assemblies: HashMap<String, ImageAssembly>,
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
            compositor,
            layer_shell,
            fractional,
            viewporter,
            conn,
            qh,
            screens: Vec::new(),
            gpu: None,
            pipes,
            modules: Modules::new(&CANVAS_ENTRIES),
            strings,
            printer: Printer::new(Caustic::Canvas),
            slots: Slots::new(),
            assemblies: HashMap::new(),
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

    /// True until the first output's configure has brought the device up.
    pub fn awaiting_device(&self) -> bool {
        self.gpu.is_none() && self.exit.is_none()
    }

    /// Whether any output has a surface yet.
    pub fn has_screens(&self) -> bool {
        !self.screens.is_empty()
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

    fn mark_all_dirty(&mut self) {
        for screen in &mut self.screens {
            screen.dirty = true;
        }
    }

    // -- the surfaces ----------------------------------------------------

    /// A background surface for a new output, committed with nothing
    /// attached so the compositor configures it.
    fn add_screen(&mut self, output: wl_output::WlOutput) {
        let info = self.output_state.info(&output);
        let name = info
            .as_ref()
            .and_then(|info| info.name.clone())
            .unwrap_or_else(|| format!("output {}", output.id().protocol_id()));
        let integer = info.as_ref().map_or(1, |info| info.scale_factor.max(1)) as u32;
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Background,
            Some(NAMESPACE),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
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
            "output {name}: a background surface (integer scale {integer} until the compositor \
             says otherwise)"
        );
        self.screens.push(Screen {
            target: None,
            output,
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

    /// The output went, or the compositor closed its surface: the swapchain
    /// first, then the objects it points at.
    fn remove_screen(&mut self, index: usize) {
        let mut screen = self.screens.remove(index);
        screen.target = None;
        if let Some(viewport) = screen.viewport.take() {
            viewport.destroy();
        }
        if let Some(fractional) = screen.fractional.take() {
            fractional.destroy();
        }
        tracing::info!("output {}: its surface is gone", screen.name);
        drop(screen);
        self.tell_size();
    }

    fn screen_of_surface(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.screens
            .iter()
            .position(|screen| screen.layer.wl_surface() == surface)
    }

    /// Bring the device up, or give an output its swapchain, or resize one,
    /// for whatever the last configures and scales asked — **outside** the
    /// dispatch that delivered them, as the bar does (Mesa's WSI makes a
    /// round trip of its own while a surface is created).
    pub fn service(&mut self) -> Result<()> {
        let mut changed = false;
        for index in 0..self.screens.len() {
            let screen = &mut self.screens[index];
            if !screen.configured || !screen.pending {
                continue;
            }
            screen.pending = false;
            screen.dirty = true;
            changed = true;
            let (width, height) = screen.buffer();
            if self.gpu.is_none() {
                if let Err(err) = self.bring_up(index) {
                    self.exit = Some(Exit::Fault(format!("the device did not come up: {err:#}")));
                    return Ok(());
                }
                continue;
            }
            let gpu = self.gpu.as_ref().expect("checked above");
            let screen = &mut self.screens[index];
            match screen.target.as_mut() {
                Some(target) => {
                    if gpu.resize(target, width, height) {
                        tracing::info!("output {}: {width}x{height} px", screen.name);
                    }
                }
                None => {
                    let (display, surface) = handles(&self.conn, &screen.layer)?;
                    // SAFETY: the connection and the layer surface outlive
                    // the target, which `remove_screen` drops first.
                    match unsafe { gpu.attach(display, surface, width, height) } {
                        Ok(target) => {
                            tracing::info!(
                                "output {}: {width}x{height} px at scale {}/120",
                                screen.name,
                                screen.scale
                            );
                            screen.target = Some(target);
                        }
                        Err(err) => tracing::error!(
                            "output {}: no swapchain ({err:#}); nothing is drawn there",
                            screen.name
                        ),
                    }
                }
            }
        }
        if changed {
            self.tell_size();
        }
        Ok(())
    }

    /// The device, on the first configured output's surface; then what was
    /// waiting for it, and the two requests only a device can make.
    fn bring_up(&mut self, index: usize) -> Result<()> {
        let screen = &self.screens[index];
        let (width, height) = screen.buffer();
        let (display, surface) = handles(&self.conn, &screen.layer)?;
        // SAFETY: as in `service`: the connection and the layer surface
        // outlive the target, and the device is dropped after every target.
        let (mut gpu, target) = unsafe { Gpu::new(display, surface, width, height) }?;
        tracing::info!(
            "output {}: {width}x{height} px at scale {}/120; the device is up",
            screen.name,
            screen.scale
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
        self.screens[index].target = Some(target);
        self.gpu = Some(gpu);
        Ok(())
    }

    /// Tell the wallpaper sidecar the largest output's buffer size when it
    /// changes: the size it fits the next picture to (docs/abi.md,
    /// *Request*, `REQ_OUTPUT_SIZE`).
    fn tell_size(&mut self) {
        let largest = self
            .screens
            .iter()
            .filter(|screen| screen.configured)
            .map(Screen::buffer)
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

    /// After every dispatch: the surfaces, then every output that has
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
        for index in 0..self.screens.len() {
            let screen = &self.screens[index];
            let wants = screen.dirty || screen.pacing.animating();
            if screen.target.is_some() && wants && screen.pacing.allowed_at(now) {
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
        let screen = &mut self.screens[index];
        let Some(target) = screen.target.as_mut() else {
            return Ok(());
        };
        let Some(frame) = gpu.acquire(target)? else {
            // Nothing to draw into: the next configure or the stall tries
            // again.
            screen.dirty = true;
            return Ok(());
        };
        // The frame request rides the commit the present makes.
        let wl_surface = screen.layer.wl_surface();
        wl_surface.frame(&self.qh, wl_surface.clone());
        screen.pacing.requested(Instant::now());
        let stamp = gpu.draw(target, frame, screen.scale)?;
        screen.dirty = false;
        self.frames += 1;
        let mirror = gpu.mirror();
        screen.pacing.set_animating(mirror.wants_redraw != 0);
        for note in mirror.witnesses_of(stamp) {
            self.printer.print(note, &self.strings, &[]);
        }
        if mirror.witness_lost > 0 && mirror.witness_frame == stamp {
            tracing::warn!("witness: {} lines lost this frame", mirror.witness_lost);
        }
        Ok(())
    }

    /// A frame is owed on an output whose request is out and unanswered: wake
    /// when the request stops holding it, since a compositor that does not
    /// repaint a covered surface sends nothing else.
    fn arm_stall(&mut self) {
        let deadline = self
            .screens
            .iter()
            .filter(|screen| screen.target.is_some() && (screen.dirty || screen.pacing.animating()))
            .filter_map(|screen| screen.pacing.stall_deadline())
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

    // -- the sidecars ------------------------------------------------------

    /// One event from a sidecar's pipe.
    pub fn apply_event(&mut self, event: Event) {
        match event {
            Event::Absent(name) => tracing::info!("sidecar {name} is absent"),
            Event::Gone(name) => {
                // Whatever it had half sent is not coming. The picture on
                // screen stays: it is the host's now.
                self.assemblies.remove(&name);
                if name == WALLPAPER
                    && let Some(image) = self.slots.forget_waiting()
                {
                    tracing::info!("sidecar {name}: gone; {:?} was never shown", image.key);
                }
            }
            Event::Frame { sidecar, frame } => self.apply_frame(&sidecar, frame),
        }
    }

    fn apply_frame(&mut self, sidecar: &str, frame: Frame) {
        match frame {
            Frame::Image(chunk) => {
                let assembly = self.assemblies.entry(sidecar.to_owned()).or_default();
                let Some(image) = assembly.push(chunk) else {
                    return;
                };
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
            Frame::Module(chunk) => self.apply_module(sidecar, chunk),
            other => tracing::info!(
                "sidecar {sidecar}: frame kind {} has no consumer here",
                other.kind()
            ),
        }
    }

    fn apply_command(&mut self, sidecar: &str, command: &wire::Canvas) {
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
        self.mark_all_dirty();
    }

    /// One piece of a fresh compile, and the swap when it completes a
    /// generation (docs/abi.md, *Module*): all or nothing, the canvas's
    /// buffers and so its fade untouched.
    fn apply_module(&mut self, sidecar: &str, chunk: ModuleChunk) {
        let ready = match self.modules.collect(chunk) {
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
        if let Some(index) = self
            .screens
            .iter()
            .position(|screen| screen.layer.wl_surface() == layer.wl_surface())
        {
            tracing::info!(
                "output {}: the compositor closed its surface",
                self.screens[index].name
            );
            self.remove_screen(index);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(index) = self.screen_of_surface(layer.wl_surface()) else {
            return;
        };
        let screen = &mut self.screens[index];
        let (width, height) = (configure.new_size.0.max(1), configure.new_size.1.max(1));
        let first = !screen.configured;
        screen.configured = true;
        if screen.logical != (width, height) || first {
            screen.logical = (width, height);
            screen.pending = true;
        }
        if let Some(viewport) = &screen.viewport {
            viewport.set_destination(width as i32, height as i32);
        }
        screen.dirty = true;
        tracing::info!(
            logical = format!("{width}x{height}"),
            scale = screen.scale,
            first,
            "output {}: background surface configured",
            screen.name
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
        let Some(index) = canvas.screen_of_surface(surface) else {
            return;
        };
        let screen = &mut canvas.screens[index];
        if screen.scale != scale {
            tracing::info!(
                "output {}: preferred scale {scale}/120 (was {})",
                screen.name,
                screen.scale
            );
            screen.scale = scale.max(1);
            screen.pending = true;
            screen.dirty = true;
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
        let Some(index) = self.screen_of_surface(surface) else {
            return;
        };
        let scale = new_factor.max(1) as u32 * SCALE_ONE;
        let screen = &mut self.screens[index];
        if screen.scale != scale {
            screen
                .layer
                .wl_surface()
                .set_buffer_scale(new_factor.max(1));
            screen.scale = scale;
            screen.pending = true;
            screen.dirty = true;
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
        let Some(index) = self.screen_of_surface(surface) else {
            return;
        };
        let screen = &mut self.screens[index];
        // A fade in progress is a frame per repaint; a settled canvas draws
        // nothing until something changes.
        if screen.pacing.answered() {
            screen.dirty = true;
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
        self.add_screen(output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(index) = self
            .screens
            .iter()
            .position(|screen| screen.output == output)
        {
            self.remove_screen(index);
        }
    }
}

impl ProvidesRegistryState for Canvas {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(Canvas);
delegate_output!(Canvas);
delegate_layer!(Canvas);
delegate_registry!(Canvas);

#[cfg(test)]
mod tests;
