use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd};

use super::xkb::Xkb;
use super::*;

fn typed(text: &str) -> Typed {
    Typed::new(text.as_bytes()).expect("printable text")
}

/// The named keys by keysym, whatever text came with them; Control with U
/// clears and with anything else means nothing; text is typed only when it
/// is printable.
#[test]
fn each_keysym_is_its_key() {
    assert_eq!(key_of(KEY_RETURN, b"\r", false), Some(Key::Submit));
    assert_eq!(key_of(KEY_KP_ENTER, b"\r", false), Some(Key::Submit));
    assert_eq!(key_of(KEY_ESCAPE, b"\x1b", false), Some(Key::Cancel));
    assert_eq!(key_of(KEY_BACKSPACE, b"\x08", false), Some(Key::Backspace));
    assert_eq!(key_of(KEY_U, b"\x15", true), Some(Key::Clear));
    assert_eq!(key_of(KEY_U_UPPER, b"\x15", true), Some(Key::Clear));
    assert_eq!(
        key_of(0x0061, b"\x01", true),
        None,
        "Control-A types nothing"
    );
    assert_eq!(key_of(0x0061, b"a", false), Some(Key::Text(typed("a"))));
    assert_eq!(
        key_of(0x00e9, "é".as_bytes(), false),
        Some(Key::Text(typed("é")))
    );
    assert_eq!(key_of(0xffe1, b"", false), None, "Shift alone");
    assert_eq!(key_of(0xff51, b"", false), None, "an arrow");
    assert_eq!(key_of(0x0074, b"\t", false), None, "a control character");
    assert_eq!(key_of_code(1), Some(Key::Cancel));
    assert_eq!(key_of_code(28), Some(Key::Submit));
    assert_eq!(key_of_code(30), None, "no keymap types nothing");
}

/// The buffer: typing, one character off the end (a character, not a
/// byte), clearing, the bound, and the count the caustic is given.
#[test]
fn the_entry_types_and_takes_back_characters() {
    let mut entry = Entry::new();
    for text in ["p", "é", "€", "x"] {
        assert!(entry.push(&typed(text)));
    }
    assert_eq!(entry.chars(), 4);
    assert!(entry.backspace());
    assert_eq!(entry.echoed(), "pé€");
    assert!(entry.backspace(), "a three-byte character goes whole");
    assert_eq!(entry.echoed(), "pé");
    assert_eq!(entry.chars(), 2);
    entry.clear();
    assert!(entry.is_empty());
    assert!(!entry.backspace());

    let three = typed("€");
    let mut pushed = 0;
    while entry.push(&three) {
        pushed += 1;
    }
    assert_eq!(
        pushed,
        MAX_ANSWER_LEN / 3,
        "never past MAX_ANSWER_LEN bytes"
    );
    assert!(entry.push(&typed("a")), "a byte that still fits");
}

/// Taking the answer hands over the bytes once and leaves the buffer empty.
#[test]
fn the_answer_is_taken_once() {
    let mut entry = Entry::new();
    for text in ["s", "3", "c"] {
        entry.push(&typed(text));
    }
    let secret = entry.take();
    assert_eq!(secret.as_bytes(), b"s3c");
    assert!(entry.is_empty());
    assert_eq!(entry.take().as_bytes(), b"");
}

/// Nothing typed reaches a `Debug`: not the buffer's, not a key's.
#[test]
fn no_debug_shows_what_was_typed() {
    let mut entry = Entry::new();
    for text in ["h", "u", "n", "t", "e", "r", "2"] {
        entry.push(&typed(text));
    }
    let shown = format!("{entry:?} {:?} {:?}", Key::Text(typed("q")), typed("z"));
    for text in ["hunter2", "h", "q", "z", "7"] {
        assert!(!shown.contains(text), "{shown:?} shows {text:?}");
    }
}

/// The keymap path end to end, where this machine has libxkbcommon and its
/// rules: a US keymap as a compositor would send it, then presses by kernel
/// code. Skipped elsewhere.
#[test]
fn a_seat_s_keymap_turns_codes_into_keys() {
    let Some(mut xkb) = Xkb::new() else {
        eprintln!("skipping: no libxkbcommon");
        return;
    };
    let Some(text) = xkb.us_keymap_text() else {
        eprintln!("skipping: no xkb rules");
        return;
    };
    // SAFETY: a fresh anonymous file this test owns.
    let fd = unsafe { libc::memfd_create(c"keymap".as_ptr(), 0) };
    assert!(fd >= 0);
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.write_all(text.as_bytes()).expect("writes");
    file.write_all(&[0]).expect("writes");
    xkb.set_keymap(file.into(), text.len() as u32 + 1)
        .expect("compiles");
    assert!(xkb.ready());
    assert_eq!(xkb.press(30), Some(Key::Text(typed("a"))));
    assert_eq!(xkb.press(28), Some(Key::Submit));
    assert_eq!(xkb.press(1), Some(Key::Cancel));
    assert_eq!(xkb.press(14), Some(Key::Backspace));
    let shift = xkb.mod_mask("Shift");
    xkb.modifiers(shift, 0, 0, 0);
    assert_eq!(xkb.press(30), Some(Key::Text(typed("A"))));
    let ctrl = xkb.mod_mask("Control");
    xkb.modifiers(ctrl, 0, 0, 0);
    assert_eq!(xkb.press(22), Some(Key::Clear), "Control-U");
    assert_eq!(xkb.press(30), None, "Control-A");
}
