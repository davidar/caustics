//! `caustics-keyboard`: the nested gate's keyboard (docs/testing.md, *The nested gate*).
//!
//! The pointer's sibling. A virtual machine target types into its guest with
//! QMP `send-key`; the nested target asks the compositor itself, through
//! `zwp_virtual_keyboard_manager_v1`, which niri offers to every client of a
//! nested session. One invocation is one action and exits once the compositor
//! has seen it (a roundtrip), so a shell step that types and then presses a
//! key gets them in that order.
//!
//! ```text
//! caustics-keyboard type TEXT...            # one argument per word, joined by spaces
//! caustics-keyboard key NAME [press|release|tap]
//! caustics-keyboard keymap                  # print the keymap it uploads, and exit
//! ```
//!
//! `NAME` is an XKB keysym name (`Return`, `Escape`, `BackSpace`, `a`, `A`,
//! `slash`), optionally with modifiers in front: `ctrl+u`, `ctrl+shift+t`,
//! `alt+F4`. `tap` (the default) is a press and a release. In `type`, a
//! newline is Return and a tab is Tab.
//!
//! **The keymap is the tool's own**: a US layout written out from [`KEYS`],
//! the same table that answers which key and which level a character is. A
//! keymap read back from a C library could disagree with a table kept here;
//! one generated from it cannot. The keys are the physical keys of a US
//! keyboard (evdev codes), so a step that types `a` presses the key a QMP
//! `send-key a` presses on the VM target. A character the table lacks is
//! refused before anything is sent.
//!
//! Not a sidecar: it is in no `SIDECARS` list, `bar/install.sh` does not ship
//! it, and it speaks no ABI. It is a harness tool that happens to be Rust.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

/// One key of the layout: its XKB name, its evdev code
/// (`linux/input-event-codes.h`; XKB's keycode is this plus 8), and its
/// levels as (keysym name, the character it types). A second level is the
/// key with Shift held; `""` is no second level, `'\0'` no character.
struct Key {
    xkb: &'static str,
    evdev: u32,
    levels: [(&'static str, char); 2],
}

const fn key(xkb: &'static str, evdev: u32, sym: &'static str, ch: char) -> Key {
    Key {
        xkb,
        evdev,
        levels: [(sym, ch), ("", '\0')],
    }
}

const fn key2(
    xkb: &'static str,
    evdev: u32,
    lower: (&'static str, char),
    upper: (&'static str, char),
) -> Key {
    Key {
        xkb,
        evdev,
        levels: [lower, upper],
    }
}

/// The US layout: every printable ASCII character, the editing and motion
/// keys, F1 to F12 and the four modifiers.
const KEYS: &[Key] = &[
    key2("TLDE", 41, ("grave", '`'), ("asciitilde", '~')),
    key2("AE01", 2, ("1", '1'), ("exclam", '!')),
    key2("AE02", 3, ("2", '2'), ("at", '@')),
    key2("AE03", 4, ("3", '3'), ("numbersign", '#')),
    key2("AE04", 5, ("4", '4'), ("dollar", '$')),
    key2("AE05", 6, ("5", '5'), ("percent", '%')),
    key2("AE06", 7, ("6", '6'), ("asciicircum", '^')),
    key2("AE07", 8, ("7", '7'), ("ampersand", '&')),
    key2("AE08", 9, ("8", '8'), ("asterisk", '*')),
    key2("AE09", 10, ("9", '9'), ("parenleft", '(')),
    key2("AE10", 11, ("0", '0'), ("parenright", ')')),
    key2("AE11", 12, ("minus", '-'), ("underscore", '_')),
    key2("AE12", 13, ("equal", '='), ("plus", '+')),
    key2("AD01", 16, ("q", 'q'), ("Q", 'Q')),
    key2("AD02", 17, ("w", 'w'), ("W", 'W')),
    key2("AD03", 18, ("e", 'e'), ("E", 'E')),
    key2("AD04", 19, ("r", 'r'), ("R", 'R')),
    key2("AD05", 20, ("t", 't'), ("T", 'T')),
    key2("AD06", 21, ("y", 'y'), ("Y", 'Y')),
    key2("AD07", 22, ("u", 'u'), ("U", 'U')),
    key2("AD08", 23, ("i", 'i'), ("I", 'I')),
    key2("AD09", 24, ("o", 'o'), ("O", 'O')),
    key2("AD10", 25, ("p", 'p'), ("P", 'P')),
    key2("AD11", 26, ("bracketleft", '['), ("braceleft", '{')),
    key2("AD12", 27, ("bracketright", ']'), ("braceright", '}')),
    key2("BKSL", 43, ("backslash", '\\'), ("bar", '|')),
    key2("AC01", 30, ("a", 'a'), ("A", 'A')),
    key2("AC02", 31, ("s", 's'), ("S", 'S')),
    key2("AC03", 32, ("d", 'd'), ("D", 'D')),
    key2("AC04", 33, ("f", 'f'), ("F", 'F')),
    key2("AC05", 34, ("g", 'g'), ("G", 'G')),
    key2("AC06", 35, ("h", 'h'), ("H", 'H')),
    key2("AC07", 36, ("j", 'j'), ("J", 'J')),
    key2("AC08", 37, ("k", 'k'), ("K", 'K')),
    key2("AC09", 38, ("l", 'l'), ("L", 'L')),
    key2("AC10", 39, ("semicolon", ';'), ("colon", ':')),
    key2("AC11", 40, ("apostrophe", '\''), ("quotedbl", '"')),
    key2("AB01", 44, ("z", 'z'), ("Z", 'Z')),
    key2("AB02", 45, ("x", 'x'), ("X", 'X')),
    key2("AB03", 46, ("c", 'c'), ("C", 'C')),
    key2("AB04", 47, ("v", 'v'), ("V", 'V')),
    key2("AB05", 48, ("b", 'b'), ("B", 'B')),
    key2("AB06", 49, ("n", 'n'), ("N", 'N')),
    key2("AB07", 50, ("m", 'm'), ("M", 'M')),
    key2("AB08", 51, ("comma", ','), ("less", '<')),
    key2("AB09", 52, ("period", '.'), ("greater", '>')),
    key2("AB10", 53, ("slash", '/'), ("question", '?')),
    key("SPCE", 57, "space", ' '),
    key("RTRN", 28, "Return", '\n'),
    key("TAB", 15, "Tab", '\t'),
    key("BKSP", 14, "BackSpace", '\0'),
    key("ESC", 1, "Escape", '\0'),
    key("DELE", 111, "Delete", '\0'),
    key("INS", 110, "Insert", '\0'),
    key("HOME", 102, "Home", '\0'),
    key("END", 107, "End", '\0'),
    key("PGUP", 104, "Prior", '\0'),
    key("PGDN", 109, "Next", '\0'),
    key("UP", 103, "Up", '\0'),
    key("DOWN", 108, "Down", '\0'),
    key("LEFT", 105, "Left", '\0'),
    key("RGHT", 106, "Right", '\0'),
    key("FK01", 59, "F1", '\0'),
    key("FK02", 60, "F2", '\0'),
    key("FK03", 61, "F3", '\0'),
    key("FK04", 62, "F4", '\0'),
    key("FK05", 63, "F5", '\0'),
    key("FK06", 64, "F6", '\0'),
    key("FK07", 65, "F7", '\0'),
    key("FK08", 66, "F8", '\0'),
    key("FK09", 67, "F9", '\0'),
    key("FK10", 68, "F10", '\0'),
    key("FK11", 87, "F11", '\0'),
    key("FK12", 88, "F12", '\0'),
    key("LFSH", 42, "Shift_L", '\0'),
    key("LCTL", 29, "Control_L", '\0'),
    key("LALT", 56, "Alt_L", '\0'),
    key("LWIN", 125, "Super_L", '\0'),
];

/// A modifier: the key that holds it and its bit among the core modifiers,
/// whose order XKB fixes (Shift, Lock, Control, Mod1 … Mod5).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Modifier {
    xkb: &'static str,
    real: &'static str,
    mask: u32,
}

const SHIFT: Modifier = Modifier {
    xkb: "LFSH",
    real: "Shift",
    mask: 1 << 0,
};
const CONTROL: Modifier = Modifier {
    xkb: "LCTL",
    real: "Control",
    mask: 1 << 2,
};
const ALT: Modifier = Modifier {
    xkb: "LALT",
    real: "Mod1",
    mask: 1 << 3,
};
const SUPER: Modifier = Modifier {
    xkb: "LWIN",
    real: "Mod4",
    mask: 1 << 6,
};
const MODIFIERS: [Modifier; 4] = [SHIFT, CONTROL, ALT, SUPER];

fn evdev_of(xkb: &str) -> u32 {
    KEYS.iter()
        .find(|k| k.xkb == xkb)
        .map(|k| k.evdev)
        .expect("every modifier's key is in KEYS")
}

/// The keymap the compositor is handed, in XKB's text format. The types and
/// the compatibility map are the stock `complete` sets from the compositor's
/// own XKB data; only the keycodes and the symbols are this table's.
fn keymap() -> String {
    let mut codes = String::new();
    let mut syms = String::new();
    for k in KEYS {
        codes.push_str(&format!("        <{}> = {};\n", k.xkb, k.evdev + 8));
        let levels: Vec<&str> = k
            .levels
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| !s.is_empty())
            .collect();
        syms.push_str(&format!(
            "        key <{}> {{ [ {} ] }};\n",
            k.xkb,
            levels.join(", ")
        ));
    }
    for m in MODIFIERS {
        syms.push_str(&format!(
            "        modifier_map {} {{ <{}> }};\n",
            m.real, m.xkb
        ));
    }
    format!(
        "xkb_keymap {{\n    xkb_keycodes \"caustics\" {{\n        minimum = 8;\n        maximum = 255;\n{codes}    }};\n    xkb_types \"caustics\" {{ include \"complete\" }};\n    xkb_compat \"caustics\" {{ include \"complete\" }};\n    xkb_symbols \"caustics\" {{\n{syms}    }};\n}};\n"
    )
}

/// One stroke: a key, at a level, with these modifiers held around it.
#[derive(Debug, PartialEq)]
struct Stroke {
    evdev: u32,
    mods: Vec<Modifier>,
}

/// The key and level that type `c`, or `None` when the layout has none.
fn stroke_for_char(c: char) -> Option<Stroke> {
    KEYS.iter().find_map(|k| {
        let level = k
            .levels
            .iter()
            .position(|&(s, ch)| !s.is_empty() && ch == c)?;
        Some(Stroke {
            evdev: k.evdev,
            mods: if level == 1 { vec![SHIFT] } else { vec![] },
        })
    })
}

/// `ctrl+shift+t`, `Return`, `A`: modifiers by short name, then one keysym.
fn stroke_for_name(spec: &str) -> Result<Stroke, String> {
    let mut parts: Vec<&str> = spec.split('+').collect();
    // A trailing `+` is the key that types it, not an empty chord.
    let name = match parts.pop() {
        Some("") => {
            // `ctrl++` and `+` split with one more empty part; `ctrl+` does not.
            if parts.last() == Some(&"") {
                parts.pop();
            }
            "plus"
        }
        Some(n) => n,
        None => return Err("empty key name".to_owned()),
    };
    let mut mods = Vec::new();
    for part in parts {
        let m = match part.to_ascii_lowercase().as_str() {
            "shift" => SHIFT,
            "ctrl" | "control" => CONTROL,
            "alt" => ALT,
            "super" | "logo" => SUPER,
            other => {
                return Err(format!(
                    "unknown modifier '{other}' (shift, ctrl, alt, super)"
                ));
            }
        };
        if !mods.contains(&m) {
            mods.push(m);
        }
    }
    let (k, level) = KEYS
        .iter()
        .find_map(|k| {
            k.levels
                .iter()
                .position(|&(s, _)| s == name)
                .map(|level| (k, level))
        })
        .ok_or_else(|| format!("no key in the layout is the keysym '{name}'"))?;
    if level == 1 && !mods.contains(&SHIFT) {
        mods.insert(0, SHIFT);
    }
    Ok(Stroke {
        evdev: k.evdev,
        mods,
    })
}

#[derive(Debug, PartialEq)]
enum Phase {
    Press,
    Release,
    Tap,
}

#[derive(Debug, PartialEq)]
enum Action {
    Type(Vec<Stroke>),
    Key(Stroke, Phase),
}

const USAGE: &str =
    "usage: caustics-keyboard (type TEXT... | key [MOD+...]KEYSYM [press|release|tap] | keymap)";

fn parse(args: &[String]) -> Result<Action, String> {
    match args.first().map(String::as_str) {
        Some("type") => {
            let text = args[1..].join(" ");
            let mut strokes = Vec::new();
            let mut missing = Vec::new();
            for c in text.chars() {
                match stroke_for_char(c) {
                    Some(s) => strokes.push(s),
                    None if !missing.contains(&c) => missing.push(c),
                    None => {}
                }
            }
            if !missing.is_empty() {
                return Err(format!(
                    "the US layout cannot type {}",
                    missing
                        .iter()
                        .map(|c| format!("{c:?} (U+{:04X})", u32::from(*c)))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok(Action::Type(strokes))
        }
        Some("key") => {
            let stroke = stroke_for_name(args.get(1).ok_or("key needs a keysym name")?)?;
            let phase = match args.get(2).map(String::as_str) {
                None | Some("tap") => Phase::Tap,
                Some("press") => Phase::Press,
                Some("release") => Phase::Release,
                Some(other) => {
                    return Err(format!(
                        "key phase must be press, release or tap, not '{other}'"
                    ));
                }
            };
            if args.len() > 3 {
                return Err(USAGE.to_owned());
            }
            Ok(Action::Key(stroke, phase))
        }
        _ => Err(USAGE.to_owned()),
    }
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// The keymap as an unlinked file, NUL-terminated as the protocol asks: the
/// compositor maps the descriptor, and nothing is left on disk.
fn keymap_file(text: &str) -> Result<(File, u32), String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("caustics-keyboard-{}.xkb", std::process::id()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("keymap file {}: {e}", path.display()))?;
    let _ = std::fs::remove_file(&path);
    file.write_all(text.as_bytes())
        .and_then(|()| file.write_all(&[0]))
        .map_err(|e| format!("keymap file: {e}"))?;
    Ok((file, text.len() as u32 + 1))
}

/// Milliseconds for the protocol's `time`, strictly increasing within one
/// invocation. Only order matters to the compositor.
struct Clock(u32);

impl Clock {
    fn new() -> Self {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Clock((ms & 0xffff_ffff) as u32)
    }
    fn tick(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
}

struct Board {
    keyboard: ZwpVirtualKeyboardV1,
    clock: Clock,
    held: u32,
}

impl Board {
    fn key(&mut self, evdev: u32, pressed: bool) {
        let state = if pressed {
            wl_keyboard::KeyState::Pressed
        } else {
            wl_keyboard::KeyState::Released
        };
        let time = self.clock.tick();
        self.keyboard.key(time, evdev, state.into());
    }

    /// The modifier's key, and the state that says it is held: a client reads
    /// modifiers from `wl_keyboard.modifiers`, not from the keys.
    fn modifier(&mut self, m: Modifier, pressed: bool) {
        self.key(evdev_of(m.xkb), pressed);
        if pressed {
            self.held |= m.mask;
        } else {
            self.held &= !m.mask;
        }
        self.keyboard.modifiers(self.held, 0, 0, 0);
    }

    fn press(&mut self, s: &Stroke) {
        for &m in &s.mods {
            self.modifier(m, true);
        }
        self.key(s.evdev, true);
    }

    fn release(&mut self, s: &Stroke) {
        self.key(s.evdev, false);
        for &m in s.mods.iter().rev() {
            self.modifier(m, false);
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("keymap") {
        print!("{}", keymap());
        return Ok(());
    }
    let action = parse(args)?;
    let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let mut state = State;

    let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).map_err(|e| {
        format!("the compositor offers no zwp_virtual_keyboard_manager_v1 v1 to this client: {e}")
    })?;
    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("wl_seat: {e}"))?;
    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    let (file, size) = keymap_file(&keymap())?;
    keyboard.keymap(wl_keyboard::KeymapFormat::XkbV1.into(), file.as_fd(), size);
    let mut board = Board {
        keyboard,
        clock: Clock::new(),
        held: 0,
    };
    board.keyboard.modifiers(0, 0, 0, 0);

    match action {
        Action::Type(strokes) => {
            for s in &strokes {
                board.press(s);
                board.release(s);
            }
        }
        Action::Key(s, Phase::Tap) => {
            board.press(&s);
            board.release(&s);
        }
        Action::Key(s, Phase::Press) => board.press(&s),
        Action::Key(s, Phase::Release) => {
            // A release from a fresh keyboard: the key, then the modifiers a
            // `press` left held, so the client's state ends with none.
            board.held = s.mods.iter().fold(0, |acc, m| acc | m.mask);
            board.release(&s);
        }
    }
    // The compositor has handled every request above once this returns, so
    // the next invocation's events land after these.
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip: {e}"))?;
    board.keyboard.destroy();
    drop(file);
    conn.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("caustics-keyboard: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn every_printable_ascii_character_has_one_key() {
        for c in (0x20u8..0x7f).map(char::from) {
            assert!(stroke_for_char(c).is_some(), "no key types {c:?}");
        }
        assert_eq!(
            stroke_for_char('\n'),
            Some(Stroke {
                evdev: 28,
                mods: vec![]
            })
        );
    }

    #[test]
    fn the_second_level_holds_shift() {
        assert_eq!(stroke_for_char('a').unwrap().mods, vec![]);
        assert_eq!(stroke_for_char('A').unwrap().mods, vec![SHIFT]);
        assert_eq!(stroke_for_char('>').unwrap().mods, vec![SHIFT]);
        assert_eq!(stroke_for_char('a').unwrap().evdev, 30);
    }

    #[test]
    fn keys_and_codes_are_unique() {
        for (i, a) in KEYS.iter().enumerate() {
            for b in &KEYS[i + 1..] {
                assert_ne!(a.xkb, b.xkb);
                assert_ne!(a.evdev, b.evdev);
            }
        }
    }

    #[test]
    fn named_keys_and_chords() {
        assert_eq!(
            stroke_for_name("Return").unwrap(),
            Stroke {
                evdev: 28,
                mods: vec![]
            }
        );
        assert_eq!(
            stroke_for_name("ctrl+u").unwrap(),
            Stroke {
                evdev: 22,
                mods: vec![CONTROL]
            }
        );
        assert_eq!(
            stroke_for_name("ctrl+T").unwrap().mods,
            vec![SHIFT, CONTROL]
        );
        assert_eq!(stroke_for_name("+").unwrap().evdev, 13);
        assert_eq!(
            stroke_for_name("ctrl++").unwrap().mods,
            vec![SHIFT, CONTROL]
        );
        assert!(stroke_for_name("hyper+a").is_err());
        assert!(stroke_for_name("Multi_key").is_err());
    }

    #[test]
    fn type_refuses_what_the_layout_lacks_before_sending() {
        let err = parse(&args(&["type", "caf\u{e9}"])).unwrap_err();
        assert!(err.contains("U+00E9"), "{err}");
        match parse(&args(&["type", "echo", "hi"])).unwrap() {
            Action::Type(s) => assert_eq!(s.len(), 7),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_keymap_names_every_key_once() {
        let map = keymap();
        for k in KEYS {
            assert_eq!(map.matches(&format!("key <{}> ", k.xkb)).count(), 1);
            assert!(map.contains(&format!("<{}> = {};", k.xkb, k.evdev + 8)));
        }
        assert!(map.contains("modifier_map Control { <LCTL> };"));
    }
}
