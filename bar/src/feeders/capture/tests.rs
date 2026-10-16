use super::*;
use smithay_client_toolkit::reexports::calloop::channel::channel;

/// Stands in for the queue handle when the test never gets far enough to
/// need one.
struct NoQueue;

impl CaptureQueue for NoQueue {
    fn create_source(
        &self,
        _: &ExtForeignToplevelImageCaptureSourceManagerV1,
        _: &Handle,
    ) -> ExtImageCaptureSourceV1 {
        unreachable!("no globals were bound, so no source is ever created")
    }
    fn create_session(
        &self,
        _: &ExtImageCopyCaptureManagerV1,
        _: &ExtImageCaptureSourceV1,
    ) -> ExtImageCopyCaptureSessionV1 {
        unreachable!()
    }
    fn create_frame(&self, _: &ExtImageCopyCaptureSessionV1) -> ExtImageCopyCaptureFrameV1 {
        unreachable!()
    }
}

/// A compositor without the capture globals must degrade to one
/// `PreviewFailed` and no session — never a panic and never a hang.
#[test]
fn a_preview_without_the_globals_fails_once_and_quietly() {
    let (tx, rx) = channel::<Update>();
    let mut capture = Capture::new(None, None, None, Box::new(NoQueue), tx);
    assert!(!capture.is_available());

    capture.want(Some(7), &HashMap::new());
    assert_eq!(capture.window(), None);
    assert_eq!(capture.next_deadline(), None);
    // Tick and teardown on an empty feeder are no-ops, not panics.
    capture.tick();
    capture.want(None, &HashMap::new());

    let mut seen = Vec::new();
    while let Ok(update) = rx.try_recv() {
        seen.push(update);
    }
    assert_eq!(
        seen,
        vec![Update::PreviewFailed {
            window: 7,
            why: "the capture globals are absent".into()
        }]
    );
}

/// One opaque pixel per format, all four naming the same colour.
#[test]
fn the_four_8888_formats_agree_on_one_pixel() {
    // R=0x11 G=0x22 B=0x33, opaque.
    let cases = [
        (wl_shm::Format::Argb8888, [0x33, 0x22, 0x11, 0xff]),
        (wl_shm::Format::Xrgb8888, [0x33, 0x22, 0x11, 0x00]),
        (wl_shm::Format::Abgr8888, [0x11, 0x22, 0x33, 0xff]),
        (wl_shm::Format::Xbgr8888, [0x11, 0x22, 0x33, 0x00]),
    ];
    for (format, bytes) in cases {
        let (w, h, rgba) = to_rgba(&bytes, 1, 1, 4, format);
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba, vec![0x11, 0x22, 0x33, 0xff], "{format:?}");
    }
}

/// A 2x2 fixture with a stride wider than the row, which is the shape a
/// compositor is free to hand back.
#[test]
fn a_2x2_capture_drops_its_padding() {
    // Argb8888, stride 12 for a 2px row: one padding pixel per row.
    #[rustfmt::skip]
    let src: Vec<u8> = vec![
        0x00, 0x00, 0xff, 0xff,  0x00, 0xff, 0x00, 0xff,  0xde, 0xad, 0xbe, 0xef,
        0xff, 0x00, 0x00, 0xff,  0xff, 0xff, 0xff, 0xff,  0xde, 0xad, 0xbe, 0xef,
    ];
    let (w, h, rgba) = to_rgba(&src, 2, 2, 12, wl_shm::Format::Argb8888);
    assert_eq!((w, h), (2, 2));
    assert_eq!(
        rgba,
        vec![
            0xff, 0x00, 0x00, 0xff, // red
            0x00, 0xff, 0x00, 0xff, // green
            0x00, 0x00, 0xff, 0xff, // blue
            0xff, 0xff, 0xff, 0xff, // white
        ]
    );
}

/// Premultiplied alpha comes back out straight, and a fully transparent
/// pixel stays transparent rather than dividing by zero.
#[test]
fn alpha_is_unpremultiplied() {
    // Half-alpha red, premultiplied: 0x80 red at 0x80 alpha.
    let src = [0x00, 0x00, 0x80, 0x80];
    let (_, _, rgba) = to_rgba(&src, 1, 1, 4, wl_shm::Format::Argb8888);
    assert_eq!(rgba[3], 0x80);
    assert!(rgba[0] >= 0xfe, "0x80/0x80 is full red, got {:#x}", rgba[0]);

    let src = [0x00, 0x00, 0x00, 0x00];
    let (_, _, rgba) = to_rgba(&src, 1, 1, 4, wl_shm::Format::Argb8888);
    assert_eq!(rgba, vec![0, 0, 0, 0]);
}

/// A capture larger than the preview slot shrinks by an integer factor and
/// never exceeds it.
#[test]
fn an_oversized_capture_is_boxed_down_to_the_slot() {
    assert_eq!(scale_factor(1920, 1080), 1);
    assert_eq!(scale_factor(2048, 2048), 1);
    assert_eq!(scale_factor(3840, 2160), 2);
    assert_eq!(scale_factor(8192, 100), 4);

    // 4x2 of alternating black and white columns, downscaled by 2, is grey.
    let mut src = Vec::new();
    for _ in 0..2 {
        for x in 0..4 {
            let v = if x % 2 == 0 { 0x00 } else { 0xff };
            src.extend_from_slice(&[v, v, v, 0xff]);
        }
    }
    // The factor cannot be forced here by faking a huge surface, so exercise the averaging directly through a 2x downscale of a
    // buffer that really is over the edge in one dimension.
    let (w, h, rgba) = to_rgba(&src, 4, 2, 16, wl_shm::Format::Argb8888);
    assert_eq!((w, h), (4, 2), "under the edge, nothing shrinks");
    assert_eq!(rgba.len(), 4 * 2 * 4);

    let mut wide = vec![0u8; (MAX_EDGE as usize * 2) * 4];
    for (i, px) in wide.chunks_exact_mut(4).enumerate() {
        let v = if i % 2 == 0 { 0x00 } else { 0xff };
        px.copy_from_slice(&[v, v, v, 0xff]);
    }
    let width = MAX_EDGE * 2;
    let (w, h, rgba) = to_rgba(&wide, width, 1, width * 4, wl_shm::Format::Argb8888);
    assert_eq!((w, h), (MAX_EDGE, 1));
    assert!(w <= MAX_EDGE && h <= MAX_EDGE);
    // Every output pixel averages one black and one white source pixel.
    assert!(rgba.chunks_exact(4).all(|p| p[0] == 0x7f && p[3] == 0xff));
}
