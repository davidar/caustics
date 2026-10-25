use super::*;
use caustics_bar_abi::wire::read;

fn pixels(width: u32, height: u32) -> Pixels {
    Pixels {
        width,
        height,
        rgba: (0..width * height * 4).map(|i| (i % 253) as u8).collect(),
    }
}

/// A picture crosses as one `Image` frame, then one `Canvas` show naming it,
/// stamped after the picture; read back, the host has the picture whole and
/// the show that fades to it. 2880 by 1800 is an output's worth, about 20 MB.
#[test]
fn a_show_is_the_picture_then_its_command() {
    let picture = pixels(2880, 1800);
    let rgba = picture.rgba.clone();
    let mut out = Vec::new();
    let (frames, start) =
        write_show(&mut out, &key(42, 7), picture, 4000, || 123_456).expect("writes");
    assert_eq!(frames, 2, "the picture and its show");
    assert_eq!(start, 123_456);

    let mut cursor = std::io::Cursor::new(out);
    let Some(Frame::Image(image)) = read(&mut cursor).expect("reads") else {
        panic!("the first frame is not the picture");
    };
    assert_eq!(image.key, "caustics-wallpaper:42-7");
    assert_eq!((image.width, image.height), (2880, 1800));
    assert!(image.rgba == rgba, "the pixels came back exact");
    assert_eq!(
        read(&mut cursor).expect("reads"),
        Some(Frame::Canvas(Canvas::Show {
            key: "caustics-wallpaper:42-7".into(),
            fade_start_ms: 123_456,
            fade_duration_ms: 4000,
        }))
    );
    assert_eq!(
        read(&mut cursor).expect("reads"),
        None,
        "nothing after the show"
    );
}

#[test]
fn dim_and_undim_are_canvas_frames() {
    assert_eq!(
        dim(5, 400, 9, 3000),
        Frame::Canvas(Canvas::Dim {
            id: 5,
            alpha_milli: 400,
            start_ms: 9,
            duration_ms: 3000
        })
    );
    assert_eq!(undim(), Frame::Canvas(Canvas::Undim));
}
