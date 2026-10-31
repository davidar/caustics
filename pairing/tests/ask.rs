//! The question table: the lines, the rules an answer follows, a service's
//! name and a device's fallback title, with no bus.

use caustics_pairing::Settings;
use caustics_pairing::ask::{
    PIN_MAX, address_of, allow_service, confirm_passkey, passkey, pin_valid, service_name, six,
    type_on_device,
};

#[test]
fn a_passkey_is_six_digits_zero_padded() {
    assert_eq!(six(0), "000000");
    assert_eq!(six(42), "000042");
    assert_eq!(six(999_999), "999999");
    assert_eq!(confirm_passkey(1234), "Confirm passkey 001234");
}

#[test]
fn a_typed_passkey_is_exactly_six_digits() {
    assert_eq!(passkey(b"000042"), Some(42));
    assert_eq!(passkey(b"999999"), Some(999_999));
    for wrong in [
        &b""[..],
        b"12345",
        b"1234567",
        b"12345a",
        b" 12345",
        b"12 345",
    ] {
        assert_eq!(passkey(wrong), None, "{:?}", String::from_utf8_lossy(wrong));
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

#[test]
fn a_well_known_service_is_named_and_any_other_is_its_uuid() {
    assert_eq!(
        service_name("0000110b-0000-1000-8000-00805f9b34fb"),
        Some("Audio Sink")
    );
    assert_eq!(
        service_name("0000111E-0000-1000-8000-00805F9B34FB"),
        Some("Hands-Free"),
        "case does not matter"
    );
    assert_eq!(
        allow_service("00001124-0000-1000-8000-00805f9b34fb"),
        "Allow Human Interface Device"
    );
    let vendor = "6ba1b218-15a8-461f-9fa8-5dcae273eafd";
    assert_eq!(service_name(vendor), None);
    assert_eq!(allow_service(vendor), format!("Allow {vendor}"));
    assert_eq!(service_name("0000ffff-0000-1000-8000-00805f9b34fb"), None);
}

#[test]
fn a_code_to_type_says_how_much_is_typed() {
    assert_eq!(type_on_device("000042", None), "Type 000042 on the device");
    assert_eq!(
        type_on_device("000042", Some(0)),
        "Type 000042 on the device"
    );
    assert_eq!(
        type_on_device("000042", Some(3)),
        "Type 000042 on the device (3 typed)"
    );
}

#[test]
fn a_device_path_spells_its_address() {
    assert_eq!(
        address_of("/org/bluez/hci0/dev_02_5E_00_00_0B_10"),
        "02:5E:00:00:0B:10"
    );
    assert_eq!(address_of("/somewhere/else"), "/somewhere/else");
    assert_eq!(address_of("/org/bluez/hci0/dev_"), "/org/bluez/hci0/dev_");
}

#[test]
fn only_one_is_a_dry_run() {
    assert!(Settings::from_vars(Some("1".into())).dry_run);
    for not in [None, Some(""), Some("0"), Some("yes"), Some("1 ")] {
        assert!(!Settings::from_vars(not.map(Into::into)).dry_run, "{not:?}");
    }
}
