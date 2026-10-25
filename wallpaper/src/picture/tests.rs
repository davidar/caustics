use super::*;

/// A PNG with the encoder the decoders are tested against, from raw RGBA.
fn png_of(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(rgba).expect("data");
    }
    out
}

fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Pixels {
    Pixels {
        width,
        height,
        rgba: rgba.repeat((width * height) as usize),
    }
}

#[test]
fn a_png_decodes_to_its_pixels() {
    let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| i as u8).collect();
    let decoded = decode(&png_of(4, 3, &rgba)).expect("decodes");
    assert_eq!((decoded.width, decoded.height), (4, 3));
    assert_eq!(decoded.rgba, rgba);
}

#[test]
fn something_else_is_refused() {
    assert!(decode(b"GIF89a").is_err());
    assert!(
        decode(&[0x89, b'P', b'N', b'G', 0, 0]).is_err(),
        "a truncated PNG"
    );
    assert!(decode(&[]).is_err());
}

/// Fitted to cover the output and no more, the shape kept; a picture no
/// larger than the output is left alone; with no output known, the wire's
/// cap is the bound.
#[test]
fn a_picture_is_fitted_down_to_cover_the_output() {
    // 6000x4000 for 1920x1080: the width ratio (0.32) leaves 1280 rows,
    // more than 1080, so the picture covers the output.
    assert_eq!(fitted_size(6000, 4000, Some((1920, 1080))), (1920, 1280));
    // A tall picture on a wide output: the width decides.
    assert_eq!(fitted_size(3000, 6000, Some((1500, 1000))), (1500, 3000));
    assert_eq!(fitted_size(1280, 800, Some((1280, 800))), (1280, 800));
    assert_eq!(fitted_size(64, 40, Some((1280, 800))), (64, 40), "never up");
    assert_eq!(fitted_size(20000, 10000, None), (8192, 4096));
    assert_eq!(fitted_size(100, 100, None), (100, 100));
    // An output past the cap still cannot make a picture past it.
    assert_eq!(fitted_size(20000, 100, Some((16000, 80))), (8192, 41));
}

/// The fit is `fitted_size` and `shrink`, and a picture already small
/// enough comes back as it went in.
#[test]
fn fit_shrinks_only_what_is_too_large() {
    let small = solid(64, 40, [1, 2, 3, 255]);
    let ptr = small.rgba.as_ptr();
    let kept = fit(small, Some((1280, 800)));
    assert_eq!((kept.width, kept.height), (64, 40));
    assert_eq!(kept.rgba.as_ptr(), ptr, "the same buffer, not a copy");
    let large = fit(solid(400, 200, [9, 8, 7, 255]), Some((100, 40)));
    assert_eq!((large.width, large.height), (100, 50));
    assert!(large.rgba.chunks_exact(4).all(|px| px == [9, 8, 7, 255]));
}

/// The area mean: a two-colour picture halves to the mean of each pair,
/// a solid one stays itself, and transparent pixels lend no colour.
#[test]
fn shrinking_is_the_mean_of_what_is_under_each_pixel() {
    let same = shrink(&solid(9, 7, [10, 200, 30, 255]), 4, 3);
    assert_eq!((same.width, same.height), (4, 3));
    assert!(same.rgba.chunks_exact(4).all(|px| px == [10, 200, 30, 255]));

    // Columns alternate red and blue: halved, every pixel is purple.
    let mut stripes = solid(4, 2, [255, 0, 0, 255]);
    for px in stripes.rgba.chunks_exact_mut(4).skip(1).step_by(2) {
        px.copy_from_slice(&[0, 0, 255, 255]);
    }
    let half = shrink(&stripes, 2, 1);
    for px in half.rgba.chunks_exact(4) {
        assert_eq!(px, [128, 0, 128, 255], "{px:?}");
    }

    // Red beside a transparent black: the colour is red's, the alpha half.
    let mut edge = solid(2, 1, [255, 0, 0, 255]);
    edge.rgba[4..8].copy_from_slice(&[0, 0, 0, 0]);
    let one = shrink(&edge, 1, 1);
    assert_eq!(one.rgba, vec![255, 0, 0, 128]);
}

/// A ratio that is not a whole number shares the pixels on the seams.
#[test]
fn a_fractional_ratio_shares_its_seams() {
    let mut ramp = solid(3, 1, [0, 0, 0, 255]);
    ramp.rgba[4..8].copy_from_slice(&[90, 90, 90, 255]);
    ramp.rgba[8..12].copy_from_slice(&[180, 180, 180, 255]);
    let two = shrink(&ramp, 2, 1);
    // The first output pixel is source 0 and half of 1: (0 + 45) / 1.5.
    assert_eq!(two.rgba[0], 30);
    // The second is half of 1 and all of 2: (45 + 180) / 1.5.
    assert_eq!(two.rgba[4], 150);
}

/// The JPEG XL fixtures beside these tests (`fixtures/README.md` says how each
/// was made): lossless, so every pixel is exact.
const JXL_OPAQUE: &[u8] = include_bytes!("fixtures/opaque.jxl");
const JXL_ALPHA: &[u8] = include_bytes!("fixtures/alpha.jxl");
const JXL_DEEP: &[u8] = include_bytes!("fixtures/deep.jxl");
const JXL_GREY: &[u8] = include_bytes!("fixtures/grey.jxl");
const JXL_LOSSY: &[u8] = include_bytes!("fixtures/lossy.jxl");

/// What `opaque.jxl` and `deep.jxl` hold: red, green, blue, white over
/// black, grey, yellow, cyan.
const OPAQUE_RGBA: [[u8; 4]; 8] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 255, 255],
    [0, 0, 0, 255],
    [128, 128, 128, 255],
    [255, 255, 0, 255],
    [0, 255, 255, 255],
];

fn pixels_of(decoded: &Pixels) -> Vec<[u8; 4]> {
    decoded
        .rgba
        .chunks_exact(4)
        .map(|px| [px[0], px[1], px[2], px[3]])
        .collect()
}

/// A bare codestream, `FF 0A`: wrapped in the ISOBMFF container, the
/// signature box, a `ftyp` and the codestream as one `jxlc` box.
fn container_of(codestream: &[u8]) -> Vec<u8> {
    let mut out = vec![
        0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a,
    ];
    out.extend_from_slice(&20u32.to_be_bytes());
    out.extend_from_slice(b"ftypjxl \0\0\0\0jxl ");
    out.extend_from_slice(&(8 + codestream.len() as u32).to_be_bytes());
    out.extend_from_slice(b"jxlc");
    out.extend_from_slice(codestream);
    out
}

/// No alpha in the file: every pixel's is 255, the colours exact.
#[test]
fn an_opaque_jpeg_xl_decodes_to_its_pixels() {
    assert!(JXL_OPAQUE.starts_with(&[0xff, 0x0a]), "a bare codestream");
    let decoded = decode(JXL_OPAQUE).expect("decodes");
    assert_eq!((decoded.width, decoded.height), (4, 2));
    assert_eq!(pixels_of(&decoded), OPAQUE_RGBA);
}

/// The same picture in the container decodes the same.
#[test]
fn a_jpeg_xl_in_its_container_decodes_the_same() {
    let boxed = container_of(JXL_OPAQUE);
    let decoded = decode(&boxed).expect("decodes");
    assert_eq!(pixels_of(&decoded), OPAQUE_RGBA);
}

/// Alpha comes through straight: a half-transparent green is still full
/// green, and a transparent pixel keeps the colour the file gave it.
#[test]
fn a_jpeg_xl_with_alpha_keeps_it_straight() {
    let decoded = decode(JXL_ALPHA).expect("decodes");
    assert_eq!((decoded.width, decoded.height), (4, 2));
    assert_eq!(
        pixels_of(&decoded),
        [
            [255, 0, 0, 255],
            [0, 255, 0, 128],
            [0, 0, 255, 0],
            [255, 255, 255, 64],
            [0, 0, 0, 255],
            [128, 128, 128, 128],
            [255, 255, 0, 255],
            [0, 255, 255, 191],
        ]
    );
}

/// Sixteen bits a sample are rendered to eight.
#[test]
fn a_sixteen_bit_jpeg_xl_decodes_to_eight() {
    let decoded = decode(JXL_DEEP).expect("decodes");
    assert_eq!(pixels_of(&decoded), OPAQUE_RGBA);
}

/// A grey picture is widened to RGB, as the PNG path does.
#[test]
fn a_grey_jpeg_xl_is_widened() {
    let decoded = decode(JXL_GREY).expect("decodes");
    assert_eq!((decoded.width, decoded.height), (4, 1));
    assert_eq!(
        pixels_of(&decoded),
        [
            [0, 0, 0, 255],
            [64, 64, 64, 255],
            [128, 128, 128, 255],
            [255, 255, 255, 255],
        ]
    );
}

/// A lossy picture rendered in bands is the picture rendered whole, band
/// heights that are not a group's included; and the bands are cut from the
/// picture as shown, after its orientation (this one is stored 48x32 and
/// turned a quarter, so it is shown 32x48).
#[test]
fn a_lossy_jpeg_xl_in_bands_is_the_same_as_whole() {
    let whole = decode_jxl_in_bands(JXL_LOSSY, u32::MAX).expect("decodes whole");
    assert_eq!((whole.width, whole.height), (32, 48));
    for band in [1, 7, 16, JXL_BAND_ROWS] {
        let banded = decode_jxl_in_bands(JXL_LOSSY, band).expect("decodes in bands");
        assert_eq!((banded.width, banded.height), (32, 48));
        let worst = whole
            .rgba
            .iter()
            .zip(&banded.rgba)
            .map(|(a, b)| a.abs_diff(*b))
            .max();
        assert!(worst <= Some(1), "bands of {band}: off by {worst:?}");
    }
    // The corners and the centre as libjxl decodes them (ImageMagick's
    // `-auto-orient`), to within a lossy decoder's rounding.
    for (x, y, want) in [
        (0, 0, [0, 22, 249]),
        (31, 0, [253, 13, 0]),
        (0, 47, [0, 255, 251]),
        (31, 47, [248, 255, 36]),
        (16, 24, [131, 131, 123]),
    ] {
        let px = &whole.rgba[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        let off = (0..3).map(|c| px[c].abs_diff(want[c])).max().unwrap();
        assert!(
            off <= 4 && px[3] == 255,
            "({x}, {y}) is {px:?}, not {want:?}"
        );
    }
}

/// A cut-short JPEG XL is the decoder's error, not a panic or a blank.
#[test]
fn a_truncated_jpeg_xl_is_refused() {
    assert!(
        decode(&[0xff, 0x0a]).is_err(),
        "a signature and nothing else"
    );
    assert!(decode(&JXL_ALPHA[..JXL_ALPHA.len() / 2]).is_err());
    assert!(decode(&container_of(&[])).is_err());
    let err = decode(&[0xff, 0x0a, 0xff, 0xff, 0xff, 0xff]).unwrap_err();
    assert!(err.starts_with("JPEG XL"), "{err}");
}

/// Premultiplied to straight: divided back out, transparent left alone.
#[test]
fn unpremultiplying_divides_by_alpha() {
    let mut rgba = vec![64, 32, 0, 128, 10, 20, 30, 0, 1, 2, 3, 255];
    unpremultiply(&mut rgba);
    assert_eq!(rgba, vec![128, 64, 0, 128, 10, 20, 30, 0, 1, 2, 3, 255]);
}

/// Real pictures, decoded and fitted the way `show` does, with the time it
/// took: `CAUSTICS_WALLPAPER_PROBE=/a.jxl:/b.jxl cargo test --release -p
/// caustics-wallpaper -- --ignored --nocapture probe`. Nothing is checked
/// but that each decodes.
#[test]
#[ignore = "needs CAUSTICS_WALLPAPER_PROBE, a list of picture paths"]
fn probe() {
    let paths = std::env::var_os("CAUSTICS_WALLPAPER_PROBE").expect("CAUSTICS_WALLPAPER_PROBE");
    for path in std::env::split_paths(&paths) {
        let started = std::time::Instant::now();
        let pixels = load(&path).expect("decodes");
        let decoded = started.elapsed();
        let (width, height) = (pixels.width, pixels.height);
        let fitted = fit(pixels, Some((2560, 1600)));
        println!(
            "{}: {}x{} decoded in {} ms; fitted to {}x{} in {} ms more",
            path.display(),
            width,
            height,
            decoded.as_millis(),
            fitted.width,
            fitted.height,
            (started.elapsed() - decoded).as_millis()
        );
    }
}
