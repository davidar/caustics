//! caustics-text — glyphs and icons as curves, for a bar with no rasteriser in it.
//!
//! The rule being applied is the bar's own (docs/architecture.md, *The model* and
//! *Rules*): the CPU keeps
//! **I/O and protocol round trips**, and anything that is computation over data
//! the GPU can hold belongs in a caustic. A font file is I/O. Turning its
//! outlines into coverage is computation. So this crate reads a font once,
//! flattens it to quadratic Béziers with Slug's band tables beside them, and
//! hands the whole thing over as four storage buffers; `caustics/text.slang`
//! does the rest, per pixel, at whatever size the caller asks for.
//!
//! So there is no glyph atlas, no packer or eviction, no per-size rasterisation, and nothing blurs when the scale changes. A font size
//! is a multiply here. A fractional scale is free.
//!
//! # The algorithm
//!
//! Slug — Eric Lengyel, *GPU-Centered Font Rendering Directly from Glyph
//! Outlines*, [JCGT 6(2), 2017](https://jcgt.org/published/0006/02/02/). The
//! patent was permanently dedicated to the public domain, and the reference vertex and pixel shaders are published at
//! <https://github.com/EricLengyel/Slug> under MIT OR Apache-2.0, with the
//! condition that distributed use gives credit. `caustics/text.slang` carries
//! that credit in its header and is a port of the reference pixel shader; the preprocessing here ([`bands`]) is this crate's own, written from the paper and the
//! repository's own notes, because the reference publishes no preprocessor.
//!
//! # The shape of it
//!
//! - [`outline`] — anything with a pen into runs of quadratics. `glyf` is
//!   already quadratic; CFF and SVG cubics are split.
//! - [`bands`] — Slug's preprocessing. A glyph's ink box cut into strips, each
//!   strip listing the curves that cross it, sorted so the per-pixel walk can
//!   stop early.
//! - [`layout`] — the four `#[repr(C)]` buffer types, pinned against the Slang
//!   mirror by `tests/layout.rs`.
//! - [`cache`] — those buffers on disk, so the band search is paid once per font
//!   file rather than once per process (the warm start).
//! - [`font`] — a whole font, glyph index = glyph id. No shaping: that is
//!   `caustics-shape`.
//! - [`icon`] — an SVG's filled paths into the same buffers. An icon is a glyph
//!   with a name.
//!
//! This is the bar's text path: `caustics-glyphs` loads a font through [`Font`]
//! and [`cache`] and sends the buffers over its pipe, `caustics/bar.slang`
//! imports the module, and there is no atlas of rasterised glyphs anywhere.

pub mod bands;
pub mod cache;
pub mod closure;
pub mod demo;
pub mod font;
pub mod icon;
pub mod layout;
pub mod outline;

pub use bands::{Builder, CurveSet};
pub use cache::{CACHE_MAGIC, CACHE_VERSION, Cached};
pub use font::Font;
pub use icon::Icon;
pub use layout::{Band, FILL_EVEN_ODD, Glyph, Index, Point};

/// The demo caustic's SPIR-V, baked by `build.rs` through `caustics_core::slang`, the driver the bar's build uses too.
///
/// It exists so the harness (`examples/render.rs`) is plain wgpu and so a
/// `cargo build` fails on a Slang error rather than an example run. The entry
/// point is [`DEMO_ENTRY`].
pub const DEMO_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/text_demo.spv"));

/// The demo caustic's one entry point.
pub const DEMO_ENTRY: &str = "text_demo";

#[cfg(test)]
mod tests;
