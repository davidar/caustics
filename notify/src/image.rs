//! An `image-data` hint to the pixels a `Pixmap` frame carries
//! (docs/abi.md, *Pixmap*).
//!
//! The hint is `(iiibiiay)`: width, height, rowstride, has-alpha, bits per
//! sample, channels, data — RGB or RGBA, 8 bits a sample, any stride. The wire
//! wants **straight RGBA8 at `IMAGE_SLOT_PX`²**, fitted by the writer, so this
//! unpacks the rows and fits the result the way `caustics-tray` fits a tray
//! pixmap: scaled to fit, centred, transparent around.

use caustics_bar_abi::IMAGE_SLOT_PX;
use caustics_bar_abi::wire::pixmap_key;
use tiny_skia::{ColorU8, FilterQuality, PixmapPaint, Transform};

/// The hint as it arrives on the bus.
pub type ImageData = (i32, i32, i32, bool, i32, i32, Vec<u8>);

/// The most pixels an `image-data` hint may claim before it is not unpacked
/// at all: 4 Mpx, 16 MiB unpacked. A notification image is an avatar or an
/// album cover; the bound keeps a hostile client's 2³¹-pixel claim from
/// costing anything. It is an **area**, not a side: a
/// 4096-px bound on each side would let one client cost a 64 MiB unpack, and a
/// 6000×200 banner is no threat at all.
pub const MAX_PIXELS: i64 = 4 * 1024 * 1024;

/// A fitted image: its minted key and its texels, straight RGBA8,
/// `IMAGE_SLOT_PX`² of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    pub key: String,
    pub rgba: Vec<u8>,
}

/// The hint's pixels, fitted and keyed `caustics-notify:<fnv1a64>`; `None` when
/// the hint does not describe the bytes it carries.
pub fn fit_hint(image: &ImageData) -> Option<Fitted> {
    let (width, height, rgba) = unpack(image)?;
    let rgba = fit(width, height, &rgba)?;
    Some(Fitted {
        key: pixmap_key(crate::NAME, &rgba),
        rgba,
    })
}

/// RGB or RGBA rows at any stride → tight straight RGBA8.
pub fn unpack(image: &ImageData) -> Option<(u32, u32, Vec<u8>)> {
    let &(width, height, stride, _alpha, bits, channels, ref data) = image;
    if width < 1 || height < 1 || i64::from(width) * i64::from(height) > MAX_PIXELS || bits != 8 {
        return None;
    }
    // `has_alpha` and `channels` say the same thing twice; the channel count
    // is the one the bytes are laid out by.
    let channels = match channels {
        3 | 4 => channels as usize,
        _ => return None,
    };
    let (w, h, stride) = (
        width as usize,
        height as usize,
        usize::try_from(stride).ok()?,
    );
    if stride < w * channels {
        return None;
    }
    // The last row need not be padded to the stride.
    let need = stride * (h - 1) + w * channels;
    if data.len() < need {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &data[y * stride..y * stride + w * channels];
        for px in row.chunks_exact(channels) {
            let alpha = if channels == 4 { px[3] } else { 0xff };
            out.extend_from_slice(&[px[0], px[1], px[2], alpha]);
        }
    }
    Some((width as u32, height as u32, out))
}

/// Straight RGBA8 of any size → straight RGBA8 at `IMAGE_SLOT_PX`², scaled to
/// fit and centred. tiny-skia is premultiplied, so this premultiplies on the
/// way in and demultiplies on the way out, as `caustics-tray`'s `fit_to_slot`.
pub fn fit(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let px = IMAGE_SLOT_PX;
    if width == 0 || height == 0 || rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    if width == px && height == px {
        return Some(rgba.to_vec());
    }
    let scale = (px as f32 / width as f32).min(px as f32 / height as f32);
    let tx = (px as f32 - width as f32 * scale) / 2.0;
    let ty = (px as f32 - height as f32 * scale) / 2.0;

    let mut source = tiny_skia::Pixmap::new(width, height)?;
    for (texel, bytes) in source.pixels_mut().iter_mut().zip(rgba.chunks_exact(4)) {
        *texel = ColorU8::from_rgba(bytes[0], bytes[1], bytes[2], bytes[3]).premultiply();
    }
    let mut out = tiny_skia::Pixmap::new(px, px)?;
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
    let mut straight = Vec::with_capacity((px as usize) * (px as usize) * 4);
    for texel in out.pixels() {
        let c = texel.demultiply();
        straight.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(straight)
}
