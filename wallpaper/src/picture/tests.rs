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
