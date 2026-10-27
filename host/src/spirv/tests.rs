use super::*;

/// A module of the instructions the walker reads: the header, `OpDecorate
/// %3 SpecId 0`, `OpTypeInt %2 32 0`, `OpSpecConstant %2 %3 0xffffffff`, and
/// an `OpStore %9 %3` that names the constant second, as a use does.
fn module(spec: &[u32]) -> Vec<u8> {
    let mut words = vec![MAGIC, 0x0001_0300, 0, 16, 0];
    words.extend([(4 << 16) | OP_DECORATE, 3, DECORATION_SPEC_ID, 0]);
    words.extend([(4 << 16) | 21, 2, 32, 0]);
    words.extend(spec);
    words.extend([(3 << 16) | 62, 9, 3]);
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect()
}

/// The literal changes and nothing else does.
#[test]
fn the_constant_s_literal_is_the_value() {
    let before = module(&[(4 << 16) | OP_SPEC_CONSTANT, 2, 3, 0xffff_ffff]);
    let after = specialise(&before, 0, 3)
        .unwrap()
        .expect("SpecId 0 is there");
    let (before, after) = (words(&before), words(&after));
    let changed: Vec<usize> = (0..before.len())
        .filter(|&at| before[at] != after[at])
        .collect();
    assert_eq!(changed, [16]);
    assert_eq!(after[16], 3);
}

/// A module that declares no such constant is left alone.
#[test]
fn no_constant_is_nothing_to_set() {
    let module = module(&[(4 << 16) | OP_SPEC_CONSTANT, 2, 3, 7]);
    assert!(specialise(&module, 1, 3).unwrap().is_none());
}

/// A boolean constant, a constant of two words and a truncated module are
/// refused, never written into.
#[test]
fn what_is_not_one_word_is_refused() {
    let boolean = module(&[(3 << 16) | 48, 2, 3]);
    assert!(specialise(&boolean, 0, 1).is_err());
    let wide = module(&[(5 << 16) | OP_SPEC_CONSTANT, 2, 3, 0, 0]);
    assert!(specialise(&wide, 0, 1).is_err());
    let mut cut = module(&[(4 << 16) | OP_SPEC_CONSTANT, 2, 3, 0]);
    cut.truncate(cut.len() - 8);
    assert!(specialise(&cut, 0, 1).is_err());
    assert!(specialise(b"not spirv at all", 0, 1).is_err());
}
