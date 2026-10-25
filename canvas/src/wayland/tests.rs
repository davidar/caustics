use super::*;

/// The buffer is the logical size times the preferred scale, rounded half
/// away from zero, as `wp_fractional_scale_v1` says: 1280 px at scale 2 for
/// 640 logical, 1920 at 1.5 for 1280, and never zero.
#[test]
fn a_buffer_is_the_logical_size_times_the_scale() {
    assert_eq!(buffer_px(640, 240), 1280);
    assert_eq!(buffer_px(1280, 180), 1920);
    assert_eq!(buffer_px(1280, 120), 1280);
    // 1.25 of 1101 is 1376.25 and 1.75 of 1101 is 1926.75.
    assert_eq!(buffer_px(1101, 150), 1376);
    assert_eq!(buffer_px(1101, 210), 1927);
    // 0.5 away from zero: 3 at 1.5 is 4.5, which is 5.
    assert_eq!(buffer_px(3, 180), 5);
    assert_eq!(buffer_px(0, 240), 1);
}
