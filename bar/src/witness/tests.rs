use super::*;

use crate::abi::Note;

/// A record with `words` in its operand slots.
fn note(words: &[u32]) -> Note {
    let mut note = Note::ZERO;
    for (slot, word) in note.arg.iter_mut().zip(words) {
        *slot = *word;
    }
    note
}

/// A table with the literals a test names, keyed as slangc would key them.
fn table(entries: &[(u32, &str)]) -> Strings {
    let mut strings = Strings::new();
    strings.replace(
        entries
            .iter()
            .map(|(hash, text)| (*hash, (*text).to_owned()))
            .collect(),
    );
    strings
}

fn rendered(format: &str, words: &[u32]) -> String {
    render(format, &note(words), &Strings::new(), &[]).0
}

#[test]
fn a_plain_index_prints_its_word_in_decimal() {
    assert_eq!(
        rendered("dock: {0} entries ({1} running)", &[7, 3]),
        "dock: 7 entries (3 running)"
    );
}

#[test]
fn hex_is_eight_digits_with_a_prefix() {
    assert_eq!(rendered("key {0:x}", &[0x0012_abcd]), "key 0x0012abcd");
}

#[test]
fn a_float_prints_as_rust_prints_one() {
    // The `layout:` family is parsed by `bar/bar-test.sh`, and what it reads is
    // `724`, not `724.00` — Rust's own `{}` for an f32.
    let words = [724.0f32.to_bits(), 0.5f32.to_bits(), (-1.0f32).to_bits()];
    assert_eq!(
        rendered("rect={0:f},{1:f},{2:f}", &words),
        "rect=724,0.5,-1"
    );
}

#[test]
fn a_hash_prints_the_literal_behind_it() {
    let strings = table(&[(0xaa, "bar")]);
    let (line, complete) = render("target={0:s}", &note(&[0xaa]), &strings, &[]);
    assert_eq!(line, "target=bar");
    assert!(complete);
}

#[test]
fn a_range_prints_the_bytes_at_it() {
    let bytes = b"..firefox..";
    let (line, complete) = render("{0:str}!", &note(&[2, 7]), &Strings::new(), bytes);
    assert_eq!(line, "firefox!");
    assert!(complete);
}

#[test]
fn a_range_is_lossy_rather_than_a_hole() {
    let bytes = &[b'a', 0xff, b'b'];
    let (line, _) = render("{0:str}", &note(&[0, 3]), &Strings::new(), bytes);
    assert_eq!(line, "a\u{fffd}b");
}

#[test]
fn two_words_make_a_u64() {
    let words = [0x89ab_cdef, 0x0123_4567];
    assert_eq!(
        rendered("window-{0:u64}", &words),
        "window-81985529216486895"
    );
}

#[test]
fn doubled_braces_are_braces() {
    assert_eq!(rendered("{{{0}}}", &[5]), "{5}");
}

#[test]
fn an_index_past_the_record_prints_a_question_mark() {
    // Ten words is the whole record; `{10}` names nothing.
    let (line, complete) = render("a={10}", &note(&[1]), &Strings::new(), &[]);
    assert_eq!(line, "a=?");
    assert!(!complete, "the caller warns once per literal about this");
}

#[test]
fn a_u64s_second_word_may_also_be_past_the_record() {
    let (line, complete) = render("{9:u64}", &note(&[1]), &Strings::new(), &[]);
    assert_eq!(line, "?");
    assert!(!complete);
}

#[test]
fn a_hash_the_table_lacks_prints_a_question_mark() {
    let (line, complete) = render("target={0:s}", &note(&[0xdead]), &Strings::new(), &[]);
    assert_eq!(line, "target=?");
    assert!(!complete);
}

#[test]
fn a_range_past_the_bytes_prints_a_question_mark() {
    let (line, complete) = render("{0:str}", &note(&[4, 9]), &Strings::new(), b"short");
    assert_eq!(line, "?");
    assert!(!complete);
}

#[test]
fn a_spec_nobody_writes_prints_a_question_mark() {
    let (line, complete) = render("{0:wat}", &note(&[1]), &Strings::new(), &[]);
    assert_eq!(line, "?");
    assert!(!complete);
}

#[test]
fn the_target_is_the_word_before_the_first_colon() {
    // The three targets a `RUST_LOG` filter can name, and the fallback. The rule
    // is a fixed match because `tracing`'s `target:` must be a literal
    // (docs/abi.md, *The witness ring*).
    assert_eq!(
        Target::of("layout: clock target=bar rect=1,2,3,4"),
        Target::Layout
    );
    assert_eq!(
        Target::of("shape: built, composed and shaped in 1.00 ms"),
        Target::Shape
    );
    assert_eq!(Target::of("hover: none at=-1,-1"), Target::Bar);
    assert_eq!(Target::of("dock: 3 entries (1 running)"), Target::Bar);
    assert_eq!(Target::of("no colon at all"), Target::Bar);
}

#[test]
fn the_printer_warns_once_per_literal() {
    let mut printer = Printer::new();
    assert!(printer.warned.is_empty());
    printer.print(&note(&[]), &Strings::new(), &[]);
    assert_eq!(
        printer.warned.len(),
        1,
        "an unresolvable head is warned about"
    );
    printer.print(&note(&[]), &Strings::new(), &[]);
    assert_eq!(printer.warned.len(), 1, "and only once");
    printer.reset();
    assert!(
        printer.warned.is_empty(),
        "a fresh caustic starts the count again"
    );
}
