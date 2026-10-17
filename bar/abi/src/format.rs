//! The swapchain formats a destination storage image can be baked for.
//!
//! SPIR-V bakes the storage-image format into the module, so every caustic
//! that writes to the swapchain is compiled once per format the surface may
//! land on, and a reload has to rebuild for the format the session actually
//! got. One table, so the build script and the runtime cannot disagree: otherwise a driver would
//! quietly accept a module written for the other layout.
//!
//! Here, in the ABI crate, because three parties read the one table:
//! `bar/build.rs` bakes a set of modules per variant, the bar picks the set
//! matching its surface, and `caustics-reload` compiles for the variant a
//! `REQ_PROGRAM` request names. None of them needs wgpu for it; the bar's
//! `wgpu::TextureFormat` conversions are its own (`bar/src/gpu/surface.rs`).

/// One swapchain format the destination storage image can be baked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DstFormat {
    /// `Rgba8Unorm`: the storage format SPIR-V can name directly.
    Rgba8,
    /// `Bgra8Unorm`: SPIR-V has no BGRA storage-image format, so the image is
    /// declared format-unknown instead — which needs the compiler's
    /// default-image-format-unknown option at compile time and the
    /// `BGRA8UNORM_STORAGE` feature at run time. Images that *are* read (the
    /// R32Uint visibility buffer) carry an explicit `[format(...)]`, which
    /// survives the option.
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

    /// Whether the variant compiles with storage images left format-unknown
    /// (`image_format_unknown` in the compiler's options): the BGRA one,
    /// because SPIR-V cannot name its format.
    pub fn image_format_unknown(self) -> bool {
        match self {
            DstFormat::Rgba8 => false,
            DstFormat::Bgra8 => true,
        }
    }

    /// The define and the format-unknown switch together: the two things a
    /// compile for this variant needs beyond the source.
    pub fn args(self) -> (&'static str, bool) {
        (self.define(), self.image_format_unknown())
    }
}

#[cfg(test)]
mod tests;
