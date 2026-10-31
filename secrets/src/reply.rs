//! The one thing this agent ever gives NetworkManager: `{ setting: { key:
//! <s> } }`, `a{sa{sv}}`, with the typed text in it.
//!
//! The text lives in a [`Zeroizing`] string that is filled once, with room
//! made up front so it never moves, and zeroed when the reply drops, which
//! is after zbus has serialised it into the method return. Serialising
//! borrows it: the maps built for the serialiser hold pointers, not bytes.
//! The method return's own buffer is zbus's, and the bytes in it (and in
//! the bus's and NetworkManager's copies) are outside this process's reach,
//! as the polkit helper's pipe is outside polkit's.

use std::collections::HashMap;

use zbus::export::serde::ser::{Serialize, SerializeMap, Serializer};
use zbus::zvariant::{Signature, Type, Value};
use zeroize::Zeroizing;

/// The reply to `GetSecrets`.
pub struct Secrets {
    setting: String,
    key: String,
    value: Zeroizing<String>,
}

impl Secrets {
    /// The reply carrying `text` under `setting` and `key`. `text` is copied
    /// once, into a string that never reallocates; the caller's copy is its
    /// own to zero.
    pub fn new(setting: &str, key: &str, text: &str) -> Secrets {
        let mut value = Zeroizing::new(String::with_capacity(text.len()));
        value.push_str(text);
        Secrets {
            setting: setting.to_owned(),
            key: key.to_owned(),
            value,
        }
    }

    /// The setting and the key, never the value.
    pub fn names(&self) -> (&str, &str) {
        (&self.setting, &self.key)
    }

    /// The value, for a test that checks what the reply would carry.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secrets({}.{}: ..)", self.setting, self.key)
    }
}

impl Type for Secrets {
    const SIGNATURE: &'static Signature =
        <HashMap<&str, HashMap<&str, Value<'_>>> as Type>::SIGNATURE;
}

impl Serialize for Secrets {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let inner: HashMap<&str, Value<'_>> =
            HashMap::from([(self.key.as_str(), Value::from(self.value.as_str()))]);
        let mut outer = serializer.serialize_map(Some(1))?;
        outer.serialize_entry(self.setting.as_str(), &inner)?;
        outer.end()
    }
}
