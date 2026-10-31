//! What each of BlueZ's agent methods puts to the user, and what an answer
//! has to be: the lines, the asks, the rules a PIN and a passkey follow, a
//! service's name, and a device's fallback title. Pure, so the whole table
//! is tested without a bus.

/// The ask a confirmation wears. Enter on the empty field confirms (the
/// canvas host sends an empty `ANSWER_TEXT`), Escape rejects (an
/// `ANSWER_CANCELLED`).
pub const CONFIRM: &str = "Enter to confirm, Escape to reject";
/// The error line for a confirmation answered with text.
pub const CONFIRM_EMPTY: &str = "Nothing to type: Enter confirms, Escape rejects";
/// The ask for a legacy PIN.
pub const PIN: &str = "PIN";
/// The error line for a PIN BlueZ would refuse.
pub const BAD_PIN: &str = "A PIN is 1 to 16 characters";
/// The error line for a PIN that is not text.
pub const PIN_NOT_TEXT: &str = "The PIN must be text";
/// The ask for a passkey.
pub const PASSKEY: &str = "Passkey";
/// The error line for a passkey that is not six digits.
pub const BAD_PASSKEY: &str = "A passkey is six digits";
/// The line a `RequestAuthorization` shows.
pub const ALLOW_PAIRING: &str = "Allow pairing";

/// The longest PIN BlueZ takes, in bytes (`RequestPinCode`: "a string of
/// 1-16 characters").
pub const PIN_MAX: usize = 16;

/// `passkey` as BlueZ's documentation and every other device show it: six
/// digits, zero-padded.
pub fn six(passkey: u32) -> String {
    format!("{passkey:06}")
}

/// The line `RequestConfirmation` shows above the ask.
pub fn confirm_passkey(passkey: u32) -> String {
    format!("Confirm passkey {}", six(passkey))
}

/// The line `AuthorizeService` shows: the service by name when it has a
/// well-known one, else the UUID.
pub fn allow_service(uuid: &str) -> String {
    format!("Allow {}", service_name(uuid).unwrap_or(uuid))
}

/// The line a display shows: what to type on the device, and how much of it
/// the device says has been typed (`DisplayPasskey`'s `entered`).
pub fn type_on_device(code: &str, entered: Option<u16>) -> String {
    match entered {
        Some(n) if n > 0 => format!("Type {code} on the device ({n} typed)"),
        _ => format!("Type {code} on the device"),
    }
}

/// A PIN BlueZ would take: 1 to [`PIN_MAX`] bytes of text.
pub fn pin_valid(text: &str) -> bool {
    (1..=PIN_MAX).contains(&text.len())
}

/// A passkey as typed: exactly six ASCII digits, as the other device shows
/// it. Anything else is `None`, and the ask comes again.
pub fn passkey(typed: &[u8]) -> Option<u32> {
    if typed.len() != 6 || !typed.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        typed
            .iter()
            .fold(0u32, |n, digit| n * 10 + u32::from(digit - b'0')),
    )
}

/// The address a BlueZ device path spells (`/org/bluez/hci0/dev_AA_BB_…`
/// is `AA:BB:…`), for a title when the device's `Alias` cannot be read; the
/// path itself when it spells none.
pub fn address_of(path: &str) -> String {
    match path
        .rsplit('/')
        .next()
        .and_then(|leaf| leaf.strip_prefix("dev_"))
    {
        Some(address) if !address.is_empty() => address.replace('_', ":"),
        _ => path.to_owned(),
    }
}

/// The Bluetooth SIG base UUID's tail: a 16-bit assigned number `XXXX` is
/// `0000XXXX` followed by this.
const BASE: &str = "-0000-1000-8000-00805f9b34fb";

/// A well-known service's name, for the profiles a desktop's devices ask to
/// be authorized for. `None` for anything else, which is then named by its
/// UUID.
pub fn service_name(uuid: &str) -> Option<&'static str> {
    let lower = uuid.to_ascii_lowercase();
    let short = lower.strip_suffix(BASE)?.strip_prefix("0000")?;
    Some(match short {
        "1101" => "Serial Port",
        "1103" => "Dial-up Networking",
        "1105" => "Object Push",
        "1106" => "File Transfer",
        "1108" => "Headset",
        "110a" => "Audio Source",
        "110b" => "Audio Sink",
        "110c" => "Remote Control Target",
        "110e" => "Remote Control",
        "1112" => "Headset Audio Gateway",
        "1115" => "Personal Area Network",
        "1116" => "Network Access Point",
        "1117" => "Group Network",
        "111e" => "Hands-Free",
        "111f" => "Hands-Free Audio Gateway",
        "112f" => "Phonebook Access",
        "1132" => "Message Access",
        "1124" => "Human Interface Device",
        "1200" => "Device Identification",
        "1812" => "Human Interface Device",
        "180f" => "Battery",
        "184e" => "Audio Stream Control",
        "1850" => "Published Audio Capabilities",
        _ => return None,
    })
}
