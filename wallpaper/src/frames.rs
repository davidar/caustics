//! What this sidecar writes to the host: a picture as one `Image` frame, the
//! show that fades to it, a dim, an undim (docs/abi.md, *Image*, *Canvas*).

use std::io::{self, Write};

use caustics_bar_abi::wire::{self, Canvas, Frame, Image};

use crate::picture::Pixels;

/// The key a picture crosses under: this sidecar's name, its pid and a
/// serial, unique among the images it has sent and named again by the show.
pub fn key(pid: u32, serial: u32) -> String {
    format!("caustics-wallpaper:{pid}-{serial}")
}

/// Write `pixels` as `key`'s one `Image` frame, then the show that fades to it
/// over `fade_ms` from the moment the picture has gone. Returns the frames
/// written and the show's start (`clock`'s ms).
///
/// The pixels are taken, not borrowed: an output-sized picture is tens of
/// mebibytes, and the frame owns them rather than a copy.
///
/// The start is taken after the picture is on the pipe and not before the
/// decode, so that a fade's first frame is not spent on the host still
/// reading its bytes.
pub fn write_show(
    out: &mut impl Write,
    key: &str,
    pixels: Pixels,
    fade_ms: u32,
    clock: impl Fn() -> u64,
) -> io::Result<(usize, u64)> {
    let image = Image {
        key: key.to_owned(),
        width: pixels.width,
        height: pixels.height,
        rgba: pixels.rgba,
    };
    wire::write(out, &Frame::Image(image))?;
    let start = clock();
    wire::write(
        out,
        &Frame::Canvas(Canvas::Show {
            key: key.to_owned(),
            fade_start_ms: start,
            fade_duration_ms: fade_ms,
        }),
    )?;
    Ok((2, start))
}

/// Dim `id` towards `alpha_milli` from `start_ms` over `over_ms`.
pub fn dim(id: u32, alpha_milli: u32, start_ms: u64, over_ms: u32) -> Frame {
    Frame::Canvas(Canvas::Dim {
        id,
        alpha_milli,
        start_ms,
        duration_ms: over_ms,
    })
}

/// No dim, now.
pub fn undim() -> Frame {
    Frame::Canvas(Canvas::Undim)
}

#[cfg(test)]
mod tests;
