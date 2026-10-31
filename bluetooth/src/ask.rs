//! What each of BlueZ's agent methods says on the chip, and what a typed
//! answer has to be: the lines, the rules a PIN and a passkey follow, a
//! service's name, and a device's fallback title. Pure, so the whole table
//! is tested without a bus.
//!
//! The rules for a typed answer are the pairing sink's too
//! (`pairing/src/ask.rs`): the sink asks again until an answer passes them,
//! and this side checks the reply once more before it goes to BlueZ, so a
//! sink that is not this project's cannot hand BlueZ what it would refuse.

/// The longest PIN BlueZ takes, in bytes (`RequestPinCode`: "a string of
/// 1-16 characters").
pub const PIN_MAX: usize = 16;

/// The two answers a yes-or-no question's rows offer.
pub const CONFIRM: &str = "Confirm";
pub const REJECT: &str = "Reject";

/// `passkey` as BlueZ's documentation and every other device show it: six
/// digits, zero-padded.
pub fn six(passkey: u32) -> String {
    format!("{passkey:06}")
}

/// The six digits in two groups of three, as a phone shows the same code:
/// easier to compare at a glance than six in a row.
pub fn spaced(passkey: u32) -> String {
    let digits = six(passkey);
    let (head, tail) = digits.split_at(digits.len() - 3);
    format!("{head} {tail}")
}

/// The question `RequestConfirmation` puts.
pub fn confirm_passkey(passkey: u32, title: &str) -> String {
    format!("Confirm passkey {} for {title}", spaced(passkey))
}

/// The question `RequestAuthorization` puts.
pub fn allow_pairing(title: &str) -> String {
    format!("Allow {title} to pair")
}

/// The question `AuthorizeService` puts: the service by name when it has a
/// well-known one, else the UUID.
pub fn allow_service(uuid: &str, title: &str) -> String {
    format!("Allow {} for {title}", service_name(uuid).unwrap_or(uuid))
}

/// The line a display shows: what to type on the device, and how much of it
/// the device says has been typed (`DisplayPasskey`'s `entered`, out of the
/// code's digits).
pub fn type_on_device(code: &str, entered: Option<u16>, title: &str) -> String {
    let digits = code.chars().filter(char::is_ascii_digit).count();
    match entered {
        Some(n) if n > 0 => format!("Type {code} on {title} ({n} of {digits} typed)"),
        _ => format!("Type {code} on {title}"),
    }
}

/// A PIN BlueZ would take: 1 to [`PIN_MAX`] bytes of text.
pub fn pin_valid(text: &str) -> bool {
    (1..=PIN_MAX).contains(&text.len())
}

/// A passkey as typed: exactly six ASCII digits, as the other device shows
/// it. Anything else is `None`.
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
