//! The question table: the rules an answer follows, with no socket.

use caustics_pairing::ask::{PIN_MAX, passkey_valid, pin_valid};

#[test]
fn a_typed_passkey_is_exactly_six_digits() {
    assert!(passkey_valid(b"000042"));
    assert!(passkey_valid(b"999999"));
    for wrong in [
        &b""[..],
        b"12345",
        b"1234567",
        b"12345a",
        b" 12345",
        b"12 345",
    ] {
        assert!(
            !passkey_valid(wrong),
            "{:?}",
            String::from_utf8_lossy(wrong)
        );
    }
}

#[test]
fn a_pin_is_one_to_sixteen_bytes() {
    assert!(!pin_valid(""));
    assert!(pin_valid("0"));
    assert!(pin_valid("0000"));
    assert!(pin_valid(&"9".repeat(PIN_MAX)));
    assert!(!pin_valid(&"9".repeat(PIN_MAX + 1)));
    // Bytes, as BlueZ counts them: eight two-byte characters fill it.
    assert!(pin_valid(&"é".repeat(8)));
    assert!(!pin_valid(&"é".repeat(9)));
}
