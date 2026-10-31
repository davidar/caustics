//! The one secret this sidecar ever gives BlueZ: the PIN `RequestPinCode`
//! returns, as `s`.
//!
//! The text lives in a [`Zeroizing`] string that is filled once, with room
//! made up front so it never moves, and zeroed when the reply drops, which
//! is after zbus has serialised it into the method return. Serialising
//! borrows it. The method return's own buffer is zbus's, and the bytes in it
//! (and in the bus's and BlueZ's copies) are outside this process's reach.
//!
//! A passkey goes back as a `u32`, a value on the stack with nothing behind
//! it to zero; the digits it was read from came in a zeroing buffer
//! ([`crate::sink`]).

use zbus::export::serde::ser::{Serialize, Serializer};
use zbus::zvariant::{Signature, Type};
use zeroize::Zeroizing;

/// The reply to `RequestPinCode`.
pub struct Pin(Zeroizing<String>);

impl Pin {
    /// The reply carrying `text`, copied once into a string that never
    /// reallocates; the caller's copy is its own to zero.
    pub fn new(text: &str) -> Pin {
        let mut value = Zeroizing::new(String::with_capacity(text.len()));
        value.push_str(text);
        Pin(value)
    }

    /// The value, for a test that checks what the reply would carry.
    pub fn value(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pin(..)")
    }
}

impl Type for Pin {
    const SIGNATURE: &'static Signature = <&str as Type>::SIGNATURE;
}

impl Serialize for Pin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
