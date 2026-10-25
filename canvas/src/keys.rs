//! The keyboard, as a prompt reads it, and the one buffer a typed secret
//! passes through (docs/canvas.md, *Why a caustic*, *The prompt core*).
//!
//! **Keys are the host's.** The seat's keymap is compiled by libxkbcommon
//! (`xkb`), compose included, and every press becomes at most one [`Key`]:
//! text to type, a character off the end, the line cleared, the answer
//! submitted, the prompt dismissed. Nothing repeats: a held key is one key.
//! Which keys those are is [`key_of`], a pure function of the keysym, the
//! text xkbcommon produced and whether Control is down, so it is tested
//! without a keyboard.
//!
//! **The buffer is the host's.** [`Entry`] holds what has been typed for one
//! ask: at most `MAX_ANSWER_LEN` bytes of UTF-8 in one allocation reserved
//! whole up front, so it never moves, and zeroed on every removal, every
//! clear, every submit and when it drops. Its bytes leave it exactly once,
//! as the `Secret` an `Answer` carries down the asking sidecar's pipe. For
//! an echo-off ask the caustic is given [`Entry::chars`], a count; the bytes
//! never enter a GPU buffer, a log line, a witness or a `Debug`. For an
//! echo-on ask (a user name, a code PAM chose to show) the text is shaped
//! like any label, which is PAM's own definition of it.

pub mod xkb;

use caustics_bar_abi::wire::{MAX_ANSWER_LEN, Secret};
use zeroize::{Zeroize, Zeroizing};

/// `XKB_KEY_Return`, `XKB_KEY_KP_Enter`, `XKB_KEY_Escape`,
/// `XKB_KEY_BackSpace`, and `u` and `U`: the keysyms a prompt reads by name.
pub const KEY_RETURN: u32 = 0xff0d;
pub const KEY_KP_ENTER: u32 = 0xff8d;
pub const KEY_ESCAPE: u32 = 0xff1b;
pub const KEY_BACKSPACE: u32 = 0xff08;
pub const KEY_U: u32 = 0x0075;
pub const KEY_U_UPPER: u32 = 0x0055;

/// The Linux input codes of the same four keys, for a seat whose keymap
/// could not be compiled (no libxkbcommon at run time): Escape and Return
/// still work, so a prompt can always be dismissed.
const CODE_ESCAPE: u32 = 1;
const CODE_BACKSPACE: u32 = 14;
const CODE_ENTER: u32 = 28;
const CODE_KP_ENTER: u32 = 96;

/// The most one key press types: one composed sequence's UTF-8.
const TYPED_MAX: usize = 32;

/// Text one key press typed, held inline and zeroed when it drops. Its
/// `Debug` says nothing about it.
#[derive(Clone, PartialEq, Eq)]
pub struct Typed {
    bytes: [u8; TYPED_MAX],
    len: usize,
}

impl Typed {
    /// `text`, if it is UTF-8 of one press's length with no control
    /// character in it.
    pub fn new(text: &[u8]) -> Option<Typed> {
        if text.is_empty() || text.len() > TYPED_MAX || std::str::from_utf8(text).is_err() {
            return None;
        }
        if text.iter().any(|byte| *byte < 0x20 || *byte == 0x7f) {
            return None;
        }
        let mut bytes = [0u8; TYPED_MAX];
        bytes[..text.len()].copy_from_slice(text);
        Some(Typed {
            bytes,
            len: text.len(),
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl Drop for Typed {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Typed(..)")
    }
}

/// One key press, as a prompt reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Text to type.
    Text(Typed),
    /// One character off the end (BackSpace).
    Backspace,
    /// The line cleared (Control-U).
    Clear,
    /// The answer, as it is (Return).
    Submit,
    /// The prompt dismissed (Escape).
    Cancel,
}

/// What a press means: the keysym xkbcommon resolved, the text it (and
/// compose) produced, and whether Control is down. `None` for a key that
/// means nothing to a prompt (a modifier, an arrow, Control with anything
/// but U).
pub fn key_of(keysym: u32, text: &[u8], ctrl: bool) -> Option<Key> {
    match keysym {
        KEY_RETURN | KEY_KP_ENTER => return Some(Key::Submit),
        KEY_ESCAPE => return Some(Key::Cancel),
        KEY_BACKSPACE => return Some(Key::Backspace),
        _ => {}
    }
    if ctrl {
        return matches!(keysym, KEY_U | KEY_U_UPPER).then_some(Key::Clear);
    }
    Typed::new(text).map(Key::Text)
}

/// What a press means with no keymap: the four keys by their codes, and no
/// text.
pub fn key_of_code(code: u32) -> Option<Key> {
    match code {
        CODE_ESCAPE => Some(Key::Cancel),
        CODE_BACKSPACE => Some(Key::Backspace),
        CODE_ENTER | CODE_KP_ENTER => Some(Key::Submit),
        _ => None,
    }
}

/// What has been typed for one ask. See the module's notes; its `Debug`
/// says nothing about its contents.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Entry {
    bytes: Zeroizing<Vec<u8>>,
}

impl Default for Entry {
    fn default() -> Self {
        Entry::new()
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl Entry {
    /// Empty, with all `MAX_ANSWER_LEN` bytes reserved: a push never moves
    /// what is there and leaves a copy behind.
    pub fn new() -> Entry {
        Entry {
            bytes: Zeroizing::new(Vec::with_capacity(MAX_ANSWER_LEN)),
        }
    }

    /// Type `typed` at the end. `false`, and nothing typed, when it would
    /// pass `MAX_ANSWER_LEN`.
    pub fn push(&mut self, typed: &Typed) -> bool {
        let text = typed.as_bytes();
        if self.bytes.len() + text.len() > MAX_ANSWER_LEN {
            return false;
        }
        self.bytes.extend_from_slice(text);
        true
    }

    /// One character off the end, its bytes zeroed. `false` when empty.
    pub fn backspace(&mut self) -> bool {
        let len = self.bytes.len();
        if len == 0 {
            return false;
        }
        let mut cut = len - 1;
        // The bytes are UTF-8 by construction (`Typed::new`); a
        // continuation byte is 10xxxxxx.
        while cut > 0 && self.bytes[cut] & 0xc0 == 0x80 {
            cut -= 1;
        }
        self.bytes[cut..].zeroize();
        self.bytes.truncate(cut);
        true
    }

    /// Everything typed, zeroed.
    pub fn clear(&mut self) {
        self.bytes.zeroize();
    }

    /// How many characters have been typed: what the caustic is given.
    pub fn chars(&self) -> u32 {
        self.bytes
            .iter()
            .filter(|byte| **byte & 0xc0 != 0x80)
            .count() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// What was typed, as text: for an echo-on ask only, whose answer PAM
    /// shows by its own definition.
    pub fn echoed(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }

    /// The answer, once: a `Secret` that zeroes itself when it drops, and
    /// this buffer zeroed.
    pub fn take(&mut self) -> Secret {
        let secret = Secret::new(self.bytes.to_vec());
        self.clear();
        secret
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Entry(..)")
    }
}

#[cfg(test)]
mod tests;
