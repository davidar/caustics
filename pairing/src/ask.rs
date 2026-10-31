//! What the sink asks and what an answer has to be: the asks, the error
//! lines, and the rules a PIN and a passkey follow. Pure, so the whole table
//! is tested without a socket.
//!
//! The Bluetooth sidecar checks the reply against the same rules before it
//! goes to BlueZ (`bluetooth/src/ask.rs`); here an answer that fails them is
//! an error line and the ask again, so the person can type it once more
//! before anything reaches BlueZ.

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

/// The longest PIN BlueZ takes, in bytes (`RequestPinCode`: "a string of
/// 1-16 characters").
pub const PIN_MAX: usize = 16;

/// A PIN BlueZ would take: 1 to [`PIN_MAX`] bytes of text.
pub fn pin_valid(text: &str) -> bool {
    (1..=PIN_MAX).contains(&text.len())
}

/// A passkey as typed: exactly six ASCII digits, as the other device shows
/// it. Anything else is `false`, and the ask comes again.
pub fn passkey_valid(typed: &[u8]) -> bool {
    typed.len() == 6 && typed.iter().all(u8::is_ascii_digit)
}
