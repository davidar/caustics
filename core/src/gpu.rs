//! The wgpu helpers the bar and the shaping harnesses share.
//!
//! Everything here is mechanism: creating a pipeline from a SPIR-V module,
//! allocating the id texture and the storage buffers, acquiring a swapchain
//! image, pulling a mirror back out of a readback buffer. The decisions —
//! which bind group, which dispatch shape, when a frame happens — stay in the
//! crate that owns the surface.

use std::fmt;

use crate::format::DstFormat;
use crate::plain::{Plain, write_from_bytes};

/// The visibility buffer's format: one `u32` id per output
/// texel, storage-bound only.
pub const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;

impl DstFormat {
    /// The variant matching a negotiated surface format, or `None` for a
    /// format no caustic was baked for.
    pub fn from_wgpu(format: wgpu::TextureFormat) -> Option<Self> {
        match format {
            wgpu::TextureFormat::Rgba8Unorm => Some(DstFormat::Rgba8),
            wgpu::TextureFormat::Bgra8Unorm => Some(DstFormat::Bgra8),
            _ => None,
        }
    }

    /// The wgpu format this variant was baked for.
    pub fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            DstFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
            DstFormat::Bgra8 => wgpu::TextureFormat::Bgra8Unorm,
        }
    }
}

/// Whether a device came up with `VK_KHR_shader_clock`, which is the
/// difference between a shaper that can hold a deadline and one that can only
/// count its own steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderClock {
    /// The extension is on and `shaderDeviceClock` is enabled: `getRealtimeClock`
    /// in a caustic reads the device's wall clock.
    Device,
    /// The adapter does not advertise it. The caustic's SPIR-V carries the
    /// capability, so this is **not** a silent fallback: the caller says so and
    /// runs on the operation budget alone, and a pipeline that reads the clock
    /// will be refused by the device rather than drawing wrong.
    Absent,
}

/// Open a Vulkan device with `VK_KHR_shader_clock` enabled where the adapter
/// has it — **the one device-creation path the bar and the harnesses share.**
///
/// wgpu has no feature bit for this extension and will not ask for it, so the
/// device goes up through wgpu-hal's `open_with_callback`, which hands out the
/// extension list and the `VkDeviceCreateInfo` before `vkCreateDevice`, and
/// comes back as a wgpu `Device` through `create_device_from_hal`.
///
/// Why any of it: a malformed font arena (random bytes under a valid directory) took the shaper to 1.39 s a
/// dispatch on lavapipe and hung RADV's graphics ring outright
/// (`shape/examples/garbage.rs` reproduces it). The shaper's caps and
/// operation budget bound that by *counting*, and counting is only ever as
/// good as the accounting. A clock read inside the kernel is the guardrail
/// that holds when the accounting is wrong: a runaway shader must not take
/// down the whole GPU.
///
/// A non-Vulkan backend, or an adapter without the extension, comes back
/// [`ShaderClock::Absent`] and the caller must say so.
pub fn open_device(
    adapter: &wgpu::Adapter,
    descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue, ShaderClock), wgpu::RequestDeviceError> {
    let wanted = ash::khr::shader_clock::NAME;
    let hal = unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() };
    let opened = match &hal {
        Some(hal)
            if hal
                .physical_device_capabilities()
                .supports_extension(wanted) =>
        {
            // The features struct has to outlive the callback that chains it
            // into the create info, so it is declared here and borrowed in.
            let mut clock =
                ash::vk::PhysicalDeviceShaderClockFeaturesKHR::default().shader_device_clock(true);
            unsafe {
                hal.open_with_callback(
                    descriptor.required_features,
                    &descriptor.required_limits,
                    &descriptor.memory_hints,
                    Some(Box::new(
                        |args: wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
                            args.extensions.push(wanted);
                            let info = std::mem::take(args.create_info);
                            *args.create_info = info.push_next(&mut clock);
                        },
                    )),
                )
                .ok()
            }
        }
        _ => None,
    };
    let Some(opened) = opened else {
        drop(hal);
        let (device, queue) = pollster::block_on(adapter.request_device(descriptor))?;
        return Ok((device, queue, ShaderClock::Absent));
    };
    drop(hal);
    let (device, queue) =
        unsafe { adapter.create_device_from_hal::<wgpu::hal::api::Vulkan>(opened, descriptor)? };
    Ok((device, queue, ShaderClock::Device))
}

/// Create one compute pipeline from a passthrough SPIR-V module.
///
/// The caller is trusted to have produced `spv` from this project's own Slang sources
/// — a build script's slangc output, or a runtime compile that
/// [`crate::slang`] has run `spirv-val` over — with bindings written against
/// `layout`. The passthrough path performs no validation of its own, so this
/// must be called inside a validation error scope: a module wgpu *can* reject
/// then surfaces as an `Err` rather than reaching the device's
/// uncaptured-error handler, which panics.
pub fn passthrough_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    label: &str,
    entry: &'static str,
    spv: &[u8],
    workgroup: (u32, u32, u32),
) -> wgpu::ComputePipeline {
    // SAFETY: as documented above — the module is this project's own slangc output,
    // and wgpu-hal numbers the Vulkan bindings by position in `layout`'s bind
    // group, which is exactly what the `[[vk::binding(n)]]` literals in the
    // Slang sources are written against.
    let module = unsafe {
        device.create_shader_module_passthrough(wgpu::ShaderModuleDescriptorPassthrough {
            label: Some(label),
            entry_points: std::borrow::Cow::Owned(vec![wgpu::PassthroughShaderEntryPoint {
                name: std::borrow::Cow::Borrowed(entry),
                // Metal-only; SPIR-V carries its own LocalSize.
                workgroup_size: workgroup,
            }]),
            spirv: Some(wgpu::util::make_spirv_raw(spv)),
            ..Default::default()
        })
    };
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        module: &module,
        entry_point: Some(entry),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
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

/// A storage buffer the CPU writes and the caustic reads.
pub fn storage_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
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
