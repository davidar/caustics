//! A file to straight RGBA8, fitted to the size the canvas covers.
//!
//! **Why fit here, on the CPU, rather than ship the file's own pixels.** A
//! photograph is often 6000×4000, which is 96 MB of RGBA8: through the pipe
//! in a hundred frames, into the host's memory, onto the device, and held
//! there as long as it is on screen, to be sampled down to an output that
//! shows a quarter of those pixels or less. Decoding is already this
//! process's work; scaling down to the largest output while the pixels are
//! in hand costs one pass over them, and then every byte the host copies and
//! the device holds is one an output can show, and the largest output samples
//! the picture texel for texel. A smaller output samples it down further on
//! the GPU, by a factor small enough for bilinear. A picture already no larger
//! than the output is sent as it is: scaling up adds bytes and no detail,
//! and the GPU's sample does it as well.
//!
//! The fit keeps the picture's shape: it is scaled so that it **covers** the
//! size (the smaller of the two ratios is 1), never cropped, so the caustic's
//! own fit (`canvas_tunables.slang`, `FIT`) still has the whole picture to
//! crop or to letterbox.

use std::path::Path;

use caustics_bar_abi::wire::IMAGE_MAX_PX;

/// The most pixels a file may decode to: 16384², 1 GiB of RGBA8. A larger
/// file is refused before its pixels are allocated where the decoder says.
pub const MAX_DECODED_PIXELS: u64 = 16384 * 16384;

/// Straight RGBA8, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Read and decode a PNG or a JPEG, by its first bytes.
pub fn load(path: &Path) -> Result<Pixels, String> {
    let data =
        std::fs::read(path).map_err(|err| format!("could not read {}: {err}", path.display()))?;
    decode(&data).map_err(|err| format!("{}: {err}", path.display()))
}

/// Decode a PNG or a JPEG.
pub fn decode(data: &[u8]) -> Result<Pixels, String> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        decode_png(data)
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        decode_jpeg(data)
    } else {
        Err("not a PNG or a JPEG".to_owned())
    }
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("an image with no pixels".to_owned());
    }
    if u64::from(width) * u64::from(height) > MAX_DECODED_PIXELS {
        return Err(format!(
            "{width}x{height} is more pixels than a wallpaper is decoded to"
        ));
    }
    Ok(())
}

fn decode_png(data: &[u8]) -> Result<Pixels, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    // Palettes expanded, 16-bit channels dropped to 8, tRNS made a real alpha
    // channel: one of the four layouts below is left.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|err| format!("PNG: {err}"))?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check_size(width, height)?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("PNG: an output too large to hold")?
    ];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|err| format!("PNG: {err}"))?;
    let pixels = info.width as usize * info.height as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => {
            buf.truncate(pixels * 4);
            buf
        }
        png::ColorType::Rgb => widen(&buf, pixels, 3, |px| [px[0], px[1], px[2], 0xff]),
        png::ColorType::GrayscaleAlpha => widen(&buf, pixels, 2, |px| [px[0], px[0], px[0], px[1]]),
        png::ColorType::Grayscale => widen(&buf, pixels, 1, |px| [px[0], px[0], px[0], 0xff]),
        png::ColorType::Indexed => return Err("PNG: a palette left unexpanded".to_owned()),
    };
    if rgba.len() != pixels * 4 {
        return Err("PNG: a frame shorter than its size".to_owned());
    }
    Ok(Pixels {
        width: info.width,
        height: info.height,
        rgba,
    })
}

fn widen(buf: &[u8], pixels: usize, stride: usize, to_rgba: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels * 4);
    for px in buf.chunks_exact(stride).take(pixels) {
        out.extend_from_slice(&to_rgba(px));
    }
    out
}

fn decode_jpeg(data: &[u8]) -> Result<Pixels, String> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(16384)
        .set_max_height(16384);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(data), options);
    decoder
        .decode_headers()
        .map_err(|err| format!("JPEG: {err:?}"))?;
    let info = decoder.info().ok_or("JPEG: no frame header")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check_size(width, height)?;
    let rgba = decoder.decode().map_err(|err| format!("JPEG: {err:?}"))?;
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("JPEG: a frame shorter than its size".to_owned());
    }
    Ok(Pixels {
        width,
        height,
        rgba,
    })
}

/// The size `width`×`height` is fitted to for an output of `target` (or,
/// with no output known yet, for the largest image the wire carries): scaled
/// down, keeping its shape, until it no more than covers the target; left
/// alone when it already does not exceed it.
pub fn fitted_size(width: u32, height: u32, target: Option<(u32, u32)>) -> (u32, u32) {
    let (w, h) = (f64::from(width), f64::from(height));
    let scale = match target {
        // The ratio at which the picture just covers the target.
        Some((tw, th)) => (f64::from(tw.max(1)) / w).max(f64::from(th.max(1)) / h),
        // No output yet: just inside the wire's cap.
        None => (f64::from(IMAGE_MAX_PX) / w).min(f64::from(IMAGE_MAX_PX) / h),
    };
    // Never up, and never past the wire's cap whatever the output says.
    let cap = (f64::from(IMAGE_MAX_PX) / w).min(f64::from(IMAGE_MAX_PX) / h);
    let scale = scale.min(1.0).min(cap);
    if scale >= 1.0 {
        return (width, height);
    }
    (
        ((w * scale).round() as u32).max(1),
        ((h * scale).round() as u32).max(1),
    )
}

/// `pixels` scaled down to `width`×`height` by area: every output pixel the
/// alpha-weighted mean of the source pixels under it, fractions at the edges
/// included. Weighted by alpha so that a transparent pixel's colour, which
/// means nothing, does not bleed into its neighbours; the result is straight
/// alpha again. Only ever down, which is all [`fitted_size`] asks for.
pub fn shrink(pixels: &Pixels, width: u32, height: u32) -> Pixels {
    if (width, height) == (pixels.width, pixels.height) {
        return pixels.clone();
    }
    let (sw, sh) = (pixels.width as usize, pixels.height as usize);
    let (dw, dh) = (width.max(1) as usize, height.max(1) as usize);
    let sx = sw as f64 / dw as f64;
    let sy = sh as f64 / dh as f64;
    // Per output column: the source columns under it and each one's share.
    let columns: Vec<Vec<(usize, f32)>> = (0..dw).map(|x| spans(x, sx, sw)).collect();
    let mut out = vec![0u8; dw * dh * 4];
    // One source row at a time is folded into a full-width accumulator for
    // the output row it lands in: memory is two rows, whatever the size.
    let mut row = vec![0f32; sw * 4];
    for y in 0..dh {
        row.iter_mut().for_each(|v| *v = 0.0);
        let mut weight_y = 0f32;
        for (sy_index, share) in spans(y, sy, sh) {
            weight_y += share;
            let src = &pixels.rgba[sy_index * sw * 4..(sy_index + 1) * sw * 4];
            for (acc, px) in row.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                let a = f32::from(px[3]) / 255.0;
                let w = share * a;
                acc[0] += w * f32::from(px[0]);
                acc[1] += w * f32::from(px[1]);
                acc[2] += w * f32::from(px[2]);
                acc[3] += share * a;
            }
        }
        for (x, cols) in columns.iter().enumerate() {
            let (mut r, mut g, mut b, mut a, mut weight) = (0f32, 0f32, 0f32, 0f32, 0f32);
            for &(sx_index, share) in cols {
                let acc = &row[sx_index * 4..sx_index * 4 + 4];
                r += share * acc[0];
                g += share * acc[1];
                b += share * acc[2];
                a += share * acc[3];
                weight += share;
            }
            let total = (weight * weight_y).max(f32::EPSILON);
            let o = &mut out[(y * dw + x) * 4..(y * dw + x) * 4 + 4];
            if a > 0.0 {
                o[0] = (r / a).round().clamp(0.0, 255.0) as u8;
                o[1] = (g / a).round().clamp(0.0, 255.0) as u8;
                o[2] = (b / a).round().clamp(0.0, 255.0) as u8;
            }
            o[3] = (a / total * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    Pixels {
        width: dw as u32,
        height: dh as u32,
        rgba: out,
    }
}

/// The source indices under output index `i` at `scale` source px per output
/// px, each with the share of it that falls inside.
fn spans(i: usize, scale: f64, len: usize) -> Vec<(usize, f32)> {
    let start = i as f64 * scale;
    let end = ((i + 1) as f64 * scale).min(len as f64);
    let first = start.floor() as usize;
    let last = (end.ceil() as usize).min(len).max(first + 1);
    (first..last)
        .map(|s| {
            let lo = (s as f64).max(start);
            let hi = ((s + 1) as f64).min(end);
            (s, (hi - lo).max(0.0) as f32)
        })
        .filter(|(_, share)| *share > 0.0)
        .collect()
}

#[cfg(test)]
mod tests;
