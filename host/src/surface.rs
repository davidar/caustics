//! One target's swapchain and the wgpu helpers around it: the swapchain
//! formats a caustic can be baked for, the visibility buffer, acquiring a
//! swapchain image, and pulling the mirror back out of a readback buffer.
//!
//! Mechanism, like `punchcutter_core::gpu`'s device, pipeline and buffer
//! helpers beside it; the decisions (which bind group, which dispatch shape,
//! when a frame happens) stay with the host (the bar's `bar/src/gpu.rs`).

use std::fmt;
use std::ptr::NonNull;

use anyhow::{Context, Result};
use caustics_bar_abi::DstFormat;
use punchcutter_core::plain::{Plain, write_from_bytes};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};

use crate::device::Presentation;

/// The visibility buffer's format: one `u32` id per output texel,
/// storage-bound only. R32Uint is one of the few formats WebGPU guarantees for
/// read-write storage.
pub const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;

/// The [`DstFormat`] matching a negotiated surface format, or `None` for a
/// format no caustic was baked for.
pub fn dst_format(format: wgpu::TextureFormat) -> Option<DstFormat> {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => Some(DstFormat::Rgba8),
        wgpu::TextureFormat::Bgra8Unorm => Some(DstFormat::Bgra8),
        _ => None,
    }
}

/// The output-sized visibility buffer, as a view. Storage-binding only: it is
/// never sampled, never copied, and — since the visibility pass rewrites every
/// texel every frame — never cleared.
pub fn create_id_texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: ID_FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// How long a readback waits for its map callback after the device poll that
/// should have run it. The compile workers poll the device too (each waits
/// out its warming dispatch, `crate::compile`), and a poll on a worker can
/// take a callback this thread's poll would have run and still be running it
/// when this one returns; the callback is in flight, not lost.
pub const MAP_CALLBACK_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Pull the mirror out of a readback buffer, the device having already been
/// waited out.
///
/// The map costs a *second* `poll`: wgpu-core only drains its ready-to-map
/// list inside `maintain`, so the callback for a buffer whose copy has already
/// completed still needs one more turn of the crank. The wait itself is
/// already satisfied, so this is a formality rather than a stall.
///
/// Failures leave the previous mirror in place and warn. A stale mirror means
/// one frame of stale hit-testing; a panic in the render loop means a dead
/// session.
pub fn refresh_mirror<T: Plain>(device: &wgpu::Device, buffer: &wgpu::Buffer, mirror: &mut T) {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            // The receiver lives on the stack below and outlives this send in
            // every path that reaches it; a closed channel just means we already
            // gave up on this frame.
            let _ = tx.send(result);
        });
    if let Err(err) = device.poll(wgpu::PollType::wait_indefinitely()) {
        tracing::warn!("mirror map poll failed: {err}");
        return;
    }
    match rx.recv_timeout(MAP_CALLBACK_GRACE) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!("mirror readback could not be mapped: {err}");
            return;
        }
        Err(_) => {
            tracing::warn!("mirror map callback did not fire during the device poll");
            return;
        }
    }

    // The view must die before the unmap, hence the block.
    {
        match buffer.slice(..).get_mapped_range() {
            Ok(view) => {
                if !write_from_bytes(mirror, &view) {
                    tracing::warn!(
                        got = view.len(),
                        want = size_of::<T>(),
                        "mirror readback is the wrong size; keeping the previous state"
                    );
                }
            }
            Err(err) => tracing::warn!("mirror range unavailable: {err}"),
        }
    }
    buffer.unmap();
}

/// The same readback as [`refresh_mirror`], over a slice of words: what a test
/// pulls a buffer back with when it is not a `Plain` struct (the bar's oracle
/// reads its strings buffer, its run buffer and its private buffer this way).
/// A failed map leaves `out` as it was.
pub fn read_words(device: &wgpu::Device, buffer: &wgpu::Buffer, out: &mut [u32]) {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    if !matches!(rx.recv_timeout(MAP_CALLBACK_GRACE), Ok(Ok(()))) {
        return;
    }
    {
        if let Ok(view) = buffer.slice(..).get_mapped_range() {
            for (word, chunk) in out.iter_mut().zip(view.chunks_exact(4)) {
                *word = u32::from_le_bytes(chunk.try_into().expect("four bytes"));
            }
        }
    }
    buffer.unmap();
}

/// A wgpu surface on a `wl_surface`, from the raw handles a Wayland client
/// holds.
///
/// # Safety
///
/// `display` and `surface` must be a live `wl_display` and `wl_surface` that
/// outlive the returned surface.
pub unsafe fn create_surface(
    instance: &wgpu::Instance,
    display: NonNull<std::ffi::c_void>,
    surface: NonNull<std::ffi::c_void>,
) -> Result<wgpu::Surface<'static>> {
    let raw_display_handle = Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
        display,
    )));
    let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(surface));
    // SAFETY: the caller's contract, above.
    unsafe {
        instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle,
                raw_window_handle,
            })
            .context("failed to create a Vulkan surface on the wl_surface")
    }
}

/// One swapchain and the per-target resources that hang off it.
pub struct Surface {
    /// `None` only for a headless device — the oracle's, which presents
    /// nothing. Everything else about the target is real,
    /// because the passes bind its `params` and its `ids` like any other.
    pub surface: Option<wgpu::Surface<'static>>,
    pub config: wgpu::SurfaceConfiguration,
    /// The target's visibility buffer ([`create_id_texture`]), or `None` for
    /// a host whose caustic binds none (the canvas): an output-sized
    /// `R32Uint` is 4 MB at 1080p and 33 MB at 4K, per target.
    pub ids: Option<wgpu::TextureView>,
    /// The ids texture's debug label, kept for the resize that remakes it;
    /// `None` when there is none.
    ids_label: Option<String>,
    /// The target's own uniform, of the size the host asked for.
    pub params: wgpu::Buffer,
    /// Has a suboptimal acquire already been answered with a reconfigure
    /// against *this* configuration? Cleared whenever the config changes.
    refit: bool,
}

impl Surface {
    /// Configure `surface` (when there is one) at `width` × `height` for
    /// storage writes, and make the target's `params` uniform of
    /// `params_size` bytes and, when `ids_label` names one, its ids texture.
    #[allow(clippy::too_many_arguments)]
    pub fn configure(
        device: &wgpu::Device,
        surface: Option<wgpu::Surface<'static>>,
        presentation: &Presentation,
        width: u32,
        height: u32,
        ids_label: Option<&str>,
        params_label: &str,
        params_size: u64,
    ) -> Surface {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::STORAGE_BINDING,
            format: presentation.format,
            width: width.max(1),
            height: height.max(1),
            present_mode: presentation.present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: presentation.alpha_mode,
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
        };
        if let Some(surface) = &surface {
            surface.configure(device, &config);
        }
        Surface {
            surface,
            refit: false,
            ids: ids_label
                .map(|label| create_id_texture(device, label, config.width, config.height)),
            ids_label: ids_label.map(str::to_owned),
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(params_label),
                size: params_size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            config,
        }
    }

    /// The configured size, in px.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Resize the swapchain (and its ids texture, if it has one) after a
    /// configure. Returns whether anything changed: the same size is no
    /// reconfigure.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) -> bool {
        if (self.config.width, self.config.height) == (width.max(1), height.max(1)) {
            return false;
        }
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        if let Some(surface) = &self.surface {
            surface.configure(device, &self.config);
        }
        // A new configuration; the next suboptimal acquire deserves an answer.
        self.refit = false;
        if let Some(label) = &self.ids_label {
            self.ids = Some(create_id_texture(
                device,
                label,
                self.config.width,
                self.config.height,
            ));
        }
        true
    }

    /// Answer a suboptimal acquire with one reconfigure, and only one.
    ///
    /// A suboptimal swapchain is still drawable, so the frame is presented
    /// first and the surface rebuilt after it. Rebuilding on *every* frame
    /// would be worse than the warning it replaces — on some drivers every
    /// acquire reports suboptimal — so the answer
    /// happens once per configuration, and a genuine resize re-arms it.
    pub fn refit_once(&mut self, device: &wgpu::Device) {
        let Some(surface) = &self.surface else {
            return;
        };
        if self.refit {
            return;
        }
        self.refit = true;
        tracing::debug!(
            width = self.config.width,
            height = self.config.height,
            "reconfiguring a suboptimal swapchain"
        );
        surface.configure(device, &self.config);
    }
}

/// One acquired swapchain image, and whether the swapchain that produced it
/// wants reconfiguring.
pub struct Frame {
    pub texture: wgpu::SurfaceTexture,
    /// The image is usable but the swapchain no longer matches the surface
    /// exactly. The frame should be drawn and presented, and the surface
    /// reconfigured *once*, after present — see [`acquire`].
    pub suboptimal: bool,
}

/// Why a swapchain image did not come back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquireError {
    /// Outdated or lost: the swapchain no longer describes the surface (the output was resized, say). Reconfigure and let the next frame retry.
    Stale,
    /// Timed out or occluded: there is nothing to draw into right now, and
    /// nothing to fix either. Skip the frame.
    Unavailable,
    /// wgpu rejected the acquire outright. Not recoverable by retrying.
    Validation,
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AcquireError::Stale => f.write_str("the swapchain is outdated or lost"),
            AcquireError::Unavailable => f.write_str("the swapchain timed out or is occluded"),
            AcquireError::Validation => f.write_str("the acquire hit a validation error"),
        }
    }
}

impl std::error::Error for AcquireError {}

/// Acquire one swapchain texture, turning wgpu 30's `CurrentSurfaceTexture`
/// states into something a frame can branch on.
///
/// `Suboptimal` is still a usable texture — a surface that skipped every
/// suboptimal frame would go blank the moment the output was reconfigured — so
/// it comes back with the flag set and a `debug!` line, *not* a warning. Some
/// swapchains report suboptimal on every acquire, and a `warn!` there once
/// wrote 1419 lines into the journal in fifteen seconds. The flag is the fix — the
/// caller reconfigures once, after present, rather than logging forever.
pub fn acquire(surface: &wgpu::Surface<'_>, label: &str) -> Result<Frame, AcquireError> {
    use wgpu::CurrentSurfaceTexture as Cst;
    match surface.get_current_texture() {
        Cst::Success(texture) => Ok(Frame {
            texture,
            suboptimal: false,
        }),
        Cst::Suboptimal(texture) => {
            tracing::debug!(target_name = label, "swapchain texture is suboptimal");
            Ok(Frame {
                texture,
                suboptimal: true,
            })
        }
        Cst::Outdated | Cst::Lost => Err(AcquireError::Stale),
        Cst::Timeout | Cst::Occluded => Err(AcquireError::Unavailable),
        Cst::Validation => Err(AcquireError::Validation),
    }
}

#[cfg(test)]
mod tests;
