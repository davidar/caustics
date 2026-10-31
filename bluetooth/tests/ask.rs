//! The question table: the lines the chip's rows say, the rules a typed
//! answer follows, a service's name and a device's fallback title, with no
//! bus.

use caustics_bluetooth::Settings;
use caustics_bluetooth::ask::{
    PIN_MAX, address_of, allow_pairing, allow_service, confirm_passkey, passkey, pin_valid,
    service_name, six, spaced, type_on_device,
};

#[test]
fn a_passkey_is_six_digits_zero_padded_and_shown_in_two_threes() {
    assert_eq!(six(0), "000000");
    assert_eq!(six(42), "000042");
    assert_eq!(six(999_999), "999999");
    assert_eq!(spaced(42), "000 042");
    assert_eq!(spaced(123_456), "123 456");
    assert_eq!(
        confirm_passkey(123_456, "Pixel 9"),
        "Confirm passkey 123 456 for Pixel 9"
    );
    assert_eq!(allow_pairing("Pixel 9"), "Allow Pixel 9 to pair");
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
    assert!(pin_valid(&"9".repeat(PIN_MAX)));
    assert!(!pin_valid(&"9".repeat(PIN_MAX + 1)));
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
        allow_service("00001124-0000-1000-8000-00805f9b34fb", "Pixel 9"),
        "Allow Human Interface Device for Pixel 9"
    );
    let vendor = "6ba1b218-15a8-461f-9fa8-5dcae273eafd";
    assert_eq!(service_name(vendor), None);
    assert_eq!(
        allow_service(vendor, "Pixel 9"),
        format!("Allow {vendor} for Pixel 9")
    );
    assert_eq!(service_name("0000ffff-0000-1000-8000-00805f9b34fb"), None);
}

#[test]
fn a_code_to_type_says_how_much_is_typed() {
    assert_eq!(
        type_on_device("123 456", None, "Pixel 9"),
        "Type 123 456 on Pixel 9"
    );
    assert_eq!(
        type_on_device("123 456", Some(0), "Pixel 9"),
        "Type 123 456 on Pixel 9"
    );
    assert_eq!(
        type_on_device("123 456", Some(2), "Pixel 9"),
        "Type 123 456 on Pixel 9 (2 of 6 typed)"
    );
    assert_eq!(
        type_on_device("0000", None, "Example Keyboard"),
        "Type 0000 on Example Keyboard"
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
fn only_one_is_the_agent_s_dry_run() {
    assert!(Settings::from_vars(Some("1".into())).agent_dry_run);
    for not in [None, Some(""), Some("0"), Some("yes"), Some("1 ")] {
        assert!(
            !Settings::from_vars(not.map(Into::into)).agent_dry_run,
            "{not:?}"
        );
    }
}
