//! Themed icon lookup, and rasterisation for what is genuinely pixels.
//!
//! This module is why `resvg` is not in `bar/Cargo.toml`. The theme search itself is `caustics_sidecar::icons` — one
//! copy for this sidecar, `caustics-glyphs` and `caustics-tray` — re-exported here:
//! whatever GTK's settings name first, then Adwaita, Cosmic, Papirus,
//! hicolor.
//!
//! Two answers:
//!
//! [`resolve`] is the search on its own — a name in, a **path** out — because
//! what the caller does next depends on *what kind of file* it is. A symbolic
//! SVG is curves and belongs to `caustics-glyphs`, which draws it in the caustic
//! as the theme's foreground colour; anything else is pixels and is this
//! crate's.
//!
//! [`raster`] is resvg on a path, for everything that is really pixels: a
//! colour SVG (Firefox's is gradients), a PNG. It always answers at
//! [`IMAGE_SLOT_PX`] square and in **straight** RGBA8, because that is what a `Pixmap` frame carries (docs/abi.md, *Pixmap*): the caustic premultiplies at the sample, so nothing on either side of the wire touches a texel to convert it.

use std::path::Path;

use caustics_bar_abi::IMAGE_SLOT_PX;
use resvg::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use resvg::usvg;

pub use caustics_sidecar::icons::{FALLBACK_THEMES, is_symbolic, resolve, theme_order};

/// One icon file by path → [`IMAGE_SLOT_PX`]² of **straight** RGBA8, fitted
/// and centred, or `None` if it will not decode. Never panics on a malformed
/// file.
pub fn raster(path: &Path) -> Option<Vec<u8>> {
    let px = IMAGE_SLOT_PX;
    let data = std::fs::read(path).ok()?;
    let is_svg = matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("svg") | Some("svgz")
    );
    let pixmap = if is_svg {
        render_svg(&data, px)?
    } else {
        render_raster(&data, px)?
    };
    Some(straight(&pixmap))
}

/// resvg straight into a `px × px` pixmap, the drawing fitted and centred.
fn render_svg(data: &[u8], px: u32) -> Option<Pixmap> {
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    if !(size.width() > 0.0 && size.height() > 0.0) {
        return None;
    }
    let scale = (px as f32 / size.width()).min(px as f32 / size.height());
    let tx = (px as f32 - size.width() * scale) / 2.0;
    let ty = (px as f32 - size.height() * scale) / 2.0;
    let mut pixmap = Pixmap::new(px, px)?;
    resvg::render(
        &tree,
        Transform::from_translate(tx, ty).pre_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(pixmap)
}

/// A PNG — the only raster format in the searched themes — or a JPEG, which
/// an `Icon=` path may name, decoded by its magic and,
/// if it is the wrong size, resampled to `px × px`.
fn render_raster(data: &[u8], px: u32) -> Option<Pixmap> {
    let source = if data.starts_with(&[0xff, 0xd8, 0xff]) {
        decode_jpeg(data)?
    } else {
        Pixmap::decode_png(data).ok()?
    };
    if source.width() == px && source.height() == px {
        return Some(source);
    }
    if source.width() == 0 || source.height() == 0 {
        return None;
    }
    let scale = (px as f32 / source.width() as f32).min(px as f32 / source.height() as f32);
    let tx = (px as f32 - source.width() as f32 * scale) / 2.0;
    let ty = (px as f32 - source.height() as f32 * scale) / 2.0;
    let mut out = Pixmap::new(px, px)?;
    out.draw_pixmap(
        0,
        0,
        source.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        },
        Transform::from_translate(tx, ty).pre_scale(scale, scale),
        None,
    );
    Some(out)
}

/// A JPEG as a tiny-skia pixmap. Opaque, so premultiplying is a copy.
fn decode_jpeg(data: &[u8]) -> Option<Pixmap> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(data), options);
    let rgba = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    let size = resvg::tiny_skia::IntSize::from_wh(width, height)?;
    Pixmap::from_vec(rgba, size)
}

/// tiny-skia is premultiplied and *Pixmap*'s pixmap is straight, so every raster
/// leaves through here. The low bits of a partly transparent texel are lost in
/// the divide; an icon is not a photograph, and the alternative is a
/// hand-rolled rasteriser.
fn straight(pixmap: &Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.width() as usize * pixmap.height() as usize * 4);
    for texel in pixmap.pixels() {
        let c = texel.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

#[cfg(test)]
mod tests;
