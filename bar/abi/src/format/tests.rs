use super::*;

#[test]
fn every_variant_has_its_own_suffix_and_define() {
    let suffixes: Vec<&str> = DstFormat::ALL.iter().map(|f| f.suffix()).collect();
    assert_eq!(suffixes, vec!["rgba8unorm", "bgra8unorm"]);
    // The BGRA half of the pair is the one that is easy to get wrong: the
    // define alone compiles, and the module is then rejected (or worse,
    // accepted) by the driver.
    assert_eq!(DstFormat::Bgra8.args(), ("DST_FORMAT_BGRA8", true));
    assert_eq!(DstFormat::Rgba8.args(), ("DST_FORMAT_RGBA8", false));
}
