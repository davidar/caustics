//! The device: a wgpu device on Vulkan, opened for a caustic, and what its
//! surface will take.
//!
//! **The device is the host's file descriptor, and its loss is the host's
//! exit** (docs/architecture.md, *The bar process*): a lost device or an
//! uncaptured validation error is one witness and [`EXIT_DEVICE`], never a
//! panic, which under `Restart=always` would be a process that comes back, is
//! handed the same bytes by the same sidecars, and dies again.
//!
//! Three refusals are made here, each because a caustic cannot run without
//! what it refuses over:
//!
//! - **an adapter named NVIDIA** (docs/architecture.md, *Power and the
//!   discrete GPU*), unless `$CAUSTICS_BAR_ALLOW_DGPU` is set. The name is the
//!   bar's and predates this crate; the installed bar's drop-ins set it. The
//!   refusal is exit 2, which the bar's unit does not restart.
//! - **no `PASSTHROUGH_SHADERS`**: the caustic is Slang compiled to SPIR-V,
//!   loaded without wgpu's own translation.
//! - **a surface with no storage-writable format**: the caustic draws the
//!   swapchain image from a compute pass, and there is no fallback.

use anyhow::{Context, Result, bail};
use punchcutter_core::gpu::ShaderClock;
use punchcutter_slang::Binding;

use crate::program::binding_counts;

/// The exit status of a host whose device is gone. 3, which the bar's unit
/// lists in `RestartPreventExitStatus`: a lost device is not a transient
/// fault, since whatever produced it is in the buffers a sidecar will replay
/// to the next process the moment it starts.
pub const EXIT_DEVICE: i32 = 3;

/// The environment variable that lets a host run on an adapter named NVIDIA.
/// Historical: the bar's name, kept so the drop-ins that set it keep working.
pub const ALLOW_DGPU_ENV: &str = "CAUSTICS_BAR_ALLOW_DGPU";

/// A Vulkan-only instance, as every host's device is.
pub fn instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

/// An open device and what it was opened with.
pub struct Device {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Whether the device came up with `VK_KHR_shader_clock`, which decides
    /// whether the shaper gets a wall-clock deadline.
    pub shader_clock: ShaderClock,
    /// The features the device was opened with: `PASSTHROUGH_SHADERS`, and
    /// `BGRA8UNORM_STORAGE` and `TIMESTAMP_QUERY` where the adapter has them.
    pub features: wgpu::Features,
}

impl Device {
    /// Open a device for a caustic whose reflected bindings are `bindings`,
    /// on the adapter that can present to `surface`, or with no surface on
    /// the first integrated or CPU adapter whose name does not say NVIDIA.
    ///
    /// `label` is the device's debug label.
    pub fn open(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'static>>,
        bindings: &[Binding],
        label: &str,
    ) -> Result<Device> {
        let adapter = match surface {
            Some(surface) => {
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    // A status bar is a handful of texels a second. Never the
                    // dGPU.
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: Some(surface),
                    ..Default::default()
                }))
                .context("no Vulkan adapter can present to the host's surface")?
            }
            // A CPU adapter is taken for the same reason the discrete one is
            // refused: a software rasteriser cannot be the dGPU, which is what
            // lets the oracle run in a VM whose only Vulkan driver is lavapipe.
            None => pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
                .into_iter()
                .find(|adapter| {
                    let info = adapter.get_info();
                    let usable = info.device_type == wgpu::DeviceType::IntegratedGpu
                        || info.device_type == wgpu::DeviceType::Cpu;
                    if !usable || info.name.to_ascii_uppercase().contains("NVIDIA") {
                        tracing::info!(adapter = %info.name, device_type = ?info.device_type,
                                       "adapter refused");
                        return false;
                    }
                    true
                })
                .context(
                    "no integrated or CPU Vulkan adapter; refusing to run the caustic on a \
                     discrete GPU. Pin the Vulkan loader to the integrated GPU's ICD with \
                     VK_DRIVER_FILES (bar/systemd/dropins/igpu-only.conf shows how); \
                     CAUSTICS_BAR_ALLOW_DGPU does not apply to the headless device",
                )?,
        };

        let info = adapter.get_info();
        tracing::info!(
            adapter = %info.name,
            driver = %info.driver_info,
            device_type = ?info.device_type,
            backend = ?info.backend,
            "wgpu adapter"
        );

        // docs/architecture.md, *Power and the discrete GPU*: the unit pins VK_DRIVER_FILES at
        // the integrated ICD, and this is the belt to those braces. Waking a
        // discrete GPU to draw a status bar is the one failure the bar must
        // never commit on a hybrid laptop.
        if info.name.to_ascii_uppercase().contains("NVIDIA")
            && std::env::var_os(ALLOW_DGPU_ENV).is_none()
        {
            tracing::error!(
                adapter = %info.name,
                "refusing to run a caustic on a discrete GPU: it would keep the dGPU awake for \
                 the life of the session. Pin VK_DRIVER_FILES at the integrated ICD, or set \
                 CAUSTICS_BAR_ALLOW_DGPU=1 to override."
            );
            // The refusal status, which the unit does not restart.
            std::process::exit(2);
        }

        if !adapter
            .features()
            .contains(wgpu::Features::PASSTHROUGH_SHADERS)
        {
            bail!(
                "adapter '{}' lacks PASSTHROUGH_SHADERS; a caustic compiled from Slang to \
                 SPIR-V cannot be loaded here",
                info.name
            );
        }

        let mut features = wgpu::Features::PASSTHROUGH_SHADERS;
        if adapter
            .features()
            .contains(wgpu::Features::BGRA8UNORM_STORAGE)
        {
            features |= wgpu::Features::BGRA8UNORM_STORAGE;
        }
        // The shape pass's watchdog. Both adapters the bar is tested on have it — RADV and lavapipe — and one that does not gets a
        // warning and no watchdog rather than no bar.
        if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            features |= wgpu::Features::TIMESTAMP_QUERY;
        }

        // The bar binds more storage buffers than wgpu's default per-stage limit
        // of eight allows — the bar's own nine, `text.slang`'s four curves and
        // `shape.slang`'s tables — so it asks for what the adapter has rather
        // than for a number: RADV's ceiling is in the millions and lavapipe's is
        // its own business, and a bar that asked for 32 would fail on a driver
        // that offered 31 for no reason. The counts come from Slang's
        // reflection of the caustic itself, so a binding added in Slang is
        // counted here without anyone editing this file.
        let (total, groups, storage_buffers) = binding_counts(bindings);
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffers_per_shader_stage = adapter
            .limits()
            .max_storage_buffers_per_shader_stage
            .max(limits.max_storage_buffers_per_shader_stage);
        tracing::info!(
            storage_buffers_allowed = limits.max_storage_buffers_per_shader_stage,
            bind_groups = limits.max_bind_groups,
            bindings = total,
            storage_buffers,
            "device limits (the caustic declares {total} bindings across {groups} groups, \
             {storage_buffers} of them storage buffers)"
        );

        // **Owns a file descriptor** (the first membership test, docs/architecture.md, *The bar process*): the device is the host's,
        // and it is opened through the text engine's device helper
        // rather than `request_device` — because wgpu will not ask for
        // `VK_KHR_shader_clock` and the shaper's SPIR-V carries the
        // capability. The shaper holds a wall-clock deadline inside the shader with it, which is what stands between a bar fed
        // a bad arena and a reset graphics ring.
        let (device, queue, clock) = punchcutter_core::gpu::open_device(
            &adapter,
            &wgpu::DeviceDescriptor {
                label: Some(label),
                required_features: features,
                required_limits: limits,
                ..Default::default()
            },
        )
        .context("Vulkan device creation failed")?;
        // **The device is the host's, and its loss is the host's exit.**
        // wgpu's default for both of these is a panic,
        // which under `Restart=always` is a bar that comes back, is handed the
        // same bytes by the same sidecars, and dies again. One witness and
        // status 3 is what the unit's `RestartPreventExitStatus` can act on.
        //
        // `Destroyed` is the ordinary teardown and says nothing.
        device.set_device_lost_callback(|reason, message| {
            if reason == wgpu::DeviceLostReason::Destroyed {
                tracing::info!("gpu: the device was destroyed ({message})");
                return;
            }
            tracing::error!(
                "gpu: the device is gone ({reason:?}): {message}. Exiting with status 3."
            );
            std::process::exit(EXIT_DEVICE);
        });
        device.on_uncaptured_error(std::sync::Arc::new(|error| {
            tracing::error!("gpu: {error}. Exiting with status 3.");
            std::process::exit(EXIT_DEVICE);
        }));

        match clock {
            ShaderClock::Device => {
                tracing::info!("gpu: VK_KHR_shader_clock is on; the shaper holds a deadline")
            }
            // Never silent: a bar without it runs on the operation budget alone,
            // and the operation budget is only as good as its accounting.
            ShaderClock::Absent => tracing::warn!(
                "gpu: this adapter has no VK_KHR_shader_clock — the shaper's run deadline \
                 is off and the caps and the operation budget are the only bound"
            ),
        }

        Ok(Device {
            instance,
            adapter,
            device,
            queue,
            shader_clock: clock,
            features,
        })
    }
}

/// What a surface will take: the swapchain format, the alpha mode and the
/// present mode. Every target of one host uses the same three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presentation {
    /// The negotiated swapchain format; it decides which SPIR-V variant the
    /// pipelines are built from.
    pub format: wgpu::TextureFormat,
    pub alpha_mode: wgpu::CompositeAlphaMode,
    /// Mailbox where the surface offers it, Fifo otherwise. Fifo is the
    /// trap: Mesa's Wayland WSI implements it by blocking the *next* present
    /// on the previous commit's frame callback, and a popup the compositor
    /// has just unmapped never gets one, so the bar hangs (seen on niri).
    /// Under Mailbox nothing in `present`
    /// waits on the compositor; pacing is the host's own frame callback
    /// ([`crate::pacing`]).
    pub present_mode: wgpu::PresentMode,
}

impl Presentation {
    /// A headless device's: the oracle fixes all three instead of
    /// negotiating, since it compares a table and not a picture, and
    /// `Rgba8Unorm` is the variant the baked SPIR-V declares explicitly.
    pub const HEADLESS: Presentation = Presentation {
        format: wgpu::TextureFormat::Rgba8Unorm,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        present_mode: wgpu::PresentMode::Fifo,
    };

    /// What `surface` will take on `adapter`, for a device opened with
    /// `features`: Rgba8Unorm, else Bgra8Unorm where storage-writable;
    /// PreMultiplied alpha where offered; Mailbox where offered.
    pub fn negotiate(
        surface: &wgpu::Surface<'_>,
        adapter: &wgpu::Adapter,
        features: wgpu::Features,
    ) -> Result<Presentation> {
        let caps = surface.get_capabilities(adapter);
        if !caps.usages.contains(wgpu::TextureUsages::STORAGE_BINDING) {
            bail!(
                "the surface does not support STORAGE_BINDING (usages: {:?}); a caustic \
                 draws the swapchain from a compute pass and has no fallback",
                caps.usages
            );
        }
        let format = if caps.formats.contains(&wgpu::TextureFormat::Rgba8Unorm) {
            wgpu::TextureFormat::Rgba8Unorm
        } else if caps.formats.contains(&wgpu::TextureFormat::Bgra8Unorm)
            && features.contains(wgpu::Features::BGRA8UNORM_STORAGE)
        {
            wgpu::TextureFormat::Bgra8Unorm
        } else {
            bail!(
                "no storage-writable surface format available (formats: {:?}, \
                 BGRA8UNORM_STORAGE: {})",
                caps.formats,
                features.contains(wgpu::Features::BGRA8UNORM_STORAGE)
            );
        };

        // The slab is translucent, so the alpha the caustic writes has to mean
        // something. PreMultiplied is what the caustic writes; anything else
        // and the bar will look wrong, which is worth a warning and not worth
        // refusing to run over.
        let alpha_mode = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            let fallback = *caps
                .alpha_modes
                .first()
                .context("the surface offers no alpha mode at all")?;
            tracing::warn!(
                offered = ?caps.alpha_modes,
                chosen = ?fallback,
                "the surface does not offer PreMultiplied alpha; anything translucent will \
                 composite wrong, but the caustic still runs"
            );
            fallback
        };

        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            tracing::warn!(
                offered = ?caps.present_modes,
                "the surface does not offer Mailbox presentation; falling back to Fifo, \
                 which can block on an unmapped popup's frame callback"
            );
            wgpu::PresentMode::Fifo
        };

        tracing::info!(
            ?format,
            ?alpha_mode,
            ?present_mode,
            offered_formats = ?caps.formats,
            offered_alpha = ?caps.alpha_modes,
            offered_present = ?caps.present_modes,
            "surface format negotiated"
        );
        Ok(Presentation {
            format,
            alpha_mode,
            present_mode,
        })
    }
}
