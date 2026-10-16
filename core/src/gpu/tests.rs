use super::*;

/// The table that decides which `.spv` a session picks. Getting it wrong
/// hands the driver a module baked for the other storage layout.
#[test]
fn the_format_table_round_trips_through_wgpu() {
    for format in DstFormat::ALL {
        assert_eq!(DstFormat::from_wgpu(format.texture_format()), Some(format));
    }
    assert_eq!(DstFormat::from_wgpu(wgpu::TextureFormat::Rgba16Float), None);
}
