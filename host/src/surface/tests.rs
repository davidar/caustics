use super::*;

/// The wgpu format a [`DstFormat`] variant was baked for: the other direction
/// of [`dst_format`], which only this test needs.
fn texture_format(format: DstFormat) -> wgpu::TextureFormat {
    match format {
        DstFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        DstFormat::Bgra8 => wgpu::TextureFormat::Bgra8Unorm,
    }
}

/// The table that decides which `.spv` a session picks. Getting it wrong
/// hands the driver a module baked for the other storage layout.
#[test]
fn the_format_table_round_trips_through_wgpu() {
    for format in DstFormat::ALL {
        assert_eq!(dst_format(texture_format(format)), Some(format));
    }
    assert_eq!(dst_format(wgpu::TextureFormat::Rgba16Float), None);
}
