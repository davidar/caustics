//! The bar's own wgpu helpers: the swapchain formats it can bake for, the
//! visibility buffer, acquiring a swapchain image, and pulling the mirror back
//! out of a readback buffer.
//!
//! Mechanism, like `punchcutter_core::gpu`'s device, pipeline and buffer
//! helpers beside it; the decisions (which bind group, which dispatch shape,
//! when a frame happens) stay in [`super::Gpu`].

use std::fmt;

use caustics_bar_abi::DstFormat;
use punchcutter_core::plain::{Plain, write_from_bytes};

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
    match rx.try_recv() {
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
