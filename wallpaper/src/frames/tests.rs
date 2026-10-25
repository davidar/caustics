use super::*;
use caustics_bar_abi::wire::{ImageAssembly, read};

fn pixels(width: u32, height: u32) -> Pixels {
    Pixels {
        width,
        height,
        rgba: (0..width * height * 4).map(|i| (i % 253) as u8).collect(),
    }
}

/// A picture crosses as `Image` pieces under the cap, then one `Canvas` show
/// naming it, stamped after the last piece; read back, the host has the
/// picture whole and the show that fades to it.
#[test]
fn a_show_is_the_picture_then_its_command() {
    let picture = pixels(800, 600);
    let mut out = Vec::new();
    let (frames, start) =
        write_show(&mut out, &key(42, 7), &picture, 4000, || 123_456).expect("writes");
    assert!(frames > 2, "an 800x600 picture is more than one frame");
    assert_eq!(start, 123_456);

    let mut cursor = std::io::Cursor::new(out);
    let mut assembly = ImageAssembly::new();
    let mut whole = None;
    let mut read_frames = 0;
    let mut show = None;
    while let Some(frame) = read(&mut cursor).expect("reads") {
        read_frames += 1;
        match frame {
            Frame::Image(chunk) => {
                assert!(show.is_none(), "a piece after the show");
                if let Some(image) = assembly.push(chunk) {
                    whole = Some(image);
                }
            }
            Frame::Canvas(command) => show = Some(command),
            other => panic!("an unexpected frame {other:?}"),
        }
    }
    assert_eq!(read_frames, frames);
    let image = whole.expect("the picture arrived whole");
    assert_eq!(image.key, "caustics-wallpaper:42-7");
    assert_eq!((image.width, image.height), (800, 600));
    assert_eq!(image.rgba, picture.rgba);
    assert_eq!(
        show,
        Some(Canvas::Show {
            key: "caustics-wallpaper:42-7".into(),
            fade_start_ms: 123_456,
            fade_duration_ms: 4000,
        })
    );
}

#[test]
fn dim_and_undim_are_canvas_frames() {
    assert_eq!(
        dim(400, 9, 3000),
        Frame::Canvas(Canvas::Dim {
            alpha_milli: 400,
            start_ms: 9,
            duration_ms: 3000
        })
    );
    assert_eq!(undim(), Frame::Canvas(Canvas::Undim));
}
