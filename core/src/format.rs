//! The swapchain formats a destination storage image can be baked for.
//!
//! SPIR-V bakes the storage-image format into the module, so every caustic
//! that writes to the swapchain is compiled once per format the surface may
//! land on, and a reload has to rebuild for the format the session actually
//! got. One table, so the build script and the runtime cannot disagree: otherwise a driver would
//! quietly accept a module written for the other layout.
//!
//! This module is compiled unconditionally, without the `gpu` feature: a build
//! script needs the table and cannot depend on wgpu. The `wgpu::TextureFormat`
//! conversions live in [`crate::gpu`], behind the feature, on the same type.

/// One swapchain format the destination storage image can be baked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DstFormat {
    /// `Rgba8Unorm`: the storage format SPIR-V can name directly.
    Rgba8,
    /// `Bgra8Unorm`: SPIR-V has no BGRA storage-image format, so the image is
    /// declared format-unknown instead — which needs
    /// `-default-image-format-unknown` at compile time and the
    /// `BGRA8UNORM_STORAGE` feature at run time. Images that *are* read (the
    /// R32Uint visibility buffer) carry an explicit `[format(...)]`, which
    /// survives the flag.
    Bgra8,
}

impl DstFormat {
    /// Every variant a build script bakes, in the order the `.spv` files are
    /// named. bar/build.rs and caustics-reload iterate this.
    pub const ALL: [DstFormat; 2] = [DstFormat::Rgba8, DstFormat::Bgra8];

    /// The output-basename suffix: `<entry>.<suffix>.spv` in `OUT_DIR`, which is what the bar `include_bytes!`.
    pub fn suffix(self) -> &'static str {
        match self {
            DstFormat::Rgba8 => "rgba8unorm",
            DstFormat::Bgra8 => "bgra8unorm",
        }
    }

    /// The preprocessor define that selects this format in the caustic.
    pub fn define(self) -> &'static str {
        match self {
            DstFormat::Rgba8 => "DST_FORMAT_RGBA8",
            DstFormat::Bgra8 => "DST_FORMAT_BGRA8",
        }
    }

    /// Extra slangc flags the variant needs beyond its define.
    pub fn extra_args(self) -> &'static [&'static str] {
        match self {
            DstFormat::Rgba8 => &[],
            DstFormat::Bgra8 => &["-default-image-format-unknown"],
        }
    }

    /// The define and the extra flags together — the shape a reload wants, since it passes one to `defines` and the other to `extra_args`.
    pub fn args(self) -> (&'static str, &'static [&'static str]) {
        (self.define(), self.extra_args())
    }
}

#[cfg(test)]
mod tests;
