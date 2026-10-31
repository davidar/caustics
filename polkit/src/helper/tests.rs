use super::*;

/// The four PAM lines and the two ends, as `send_to_helper` and `main`
/// write them: one space after the prefix, even before an empty text.
#[test]
fn every_line_the_helper_writes_is_read_as_its_kind() {
    assert_eq!(
        parse(b"PAM_PROMPT_ECHO_OFF Password: "),
        Line::EchoOff("Password: ".into())
    );
    assert_eq!(
        parse(b"PAM_PROMPT_ECHO_ON login:"),
        Line::EchoOn("login:".into())
    );
    assert_eq!(
        parse(b"PAM_ERROR_MSG Sorry, try again."),
        Line::Error("Sorry, try again.".into())
    );
    assert_eq!(
        parse(b"PAM_TEXT_INFO Touch the key"),
        Line::Info("Touch the key".into())
    );
    assert_eq!(parse(b"PAM_PROMPT_ECHO_OFF "), Line::EchoOff(String::new()));
    assert_eq!(parse(b"SUCCESS"), Line::Success);
    assert_eq!(parse(b"FAILURE"), Line::Failure);
}

/// Anything else is unknown, and says only how long it was.
#[test]
fn an_unknown_line_carries_its_length_and_nothing_else() {
    assert_eq!(parse(b"PAM_PROMPT_ECHO_OFF"), Line::Unknown(19));
    assert_eq!(parse(b"hunter2"), Line::Unknown(7));
    assert_eq!(parse(b""), Line::Unknown(0));
    assert!(!format!("{:?}", parse(b"hunter2")).contains("hunter2"));
}

/// `g_strescape`'s output, undone: the text a PAM module wrote with a
/// newline, a tab, a quote and a non-ASCII byte in it.
#[test]
fn the_text_is_unescaped_as_g_strcompress_does() {
    assert_eq!(
        unescape(br#"a\nb\tc\"d\\e\001\303\251"#),
        b"a\nb\tc\"d\\e\x01\xc3\xa9"
    );
    assert_eq!(unescape(br"\b\f\r\v"), b"\x08\x0c\r\x0b");
    // A backslash before an ordinary character is that character, and a
    // trailing one is dropped.
    assert_eq!(unescape(br"\q\"), b"q");
    // Octal stops after three digits, or at the first that is not one.
    assert_eq!(unescape(br"\1234\08"), b"S4\x008");
    assert_eq!(
        parse(br"PAM_TEXT_INFO caf\303\251\nline"),
        Line::Info("café\nline".into())
    );
}

/// An answer is its bytes and a newline, and one that the helper's `fgets`
/// would read as two is refused.
#[test]
fn an_answer_line_fits_the_helper_s_buffer() {
    assert_eq!(answer_line(b"pw").unwrap().as_slice(), b"pw\n");
    assert_eq!(answer_line(b"").unwrap().as_slice(), b"\n");
    assert_eq!(
        answer_line(&[b'a'; MAX_ANSWER]).unwrap().len(),
        MAX_ANSWER + 1
    );
    assert_eq!(
        answer_line(&[b'a'; MAX_ANSWER + 1]).unwrap_err(),
        Unfit::TooLong
    );
    assert_eq!(answer_line(b"a\nb").unwrap_err(), Unfit::Breaks);
    assert_eq!(answer_line(b"a\0b").unwrap_err(), Unfit::Breaks);
    // The helper reads at most PAM_MAX_RESP_SIZE - 1 bytes a line, newline
    // included.
    assert!(MAX_ANSWER + 1 < PAM_MAX_RESP_SIZE);
}

/// A directory of this test's own, removed at the end.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!(
            "caustics-polkit-helper-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("writable");
        Temp(dir)
    }

    /// `name` as a directory under this one, with a helper in it or not.
    fn dir(&self, name: &str, with_helper: bool) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).expect("writable");
        if with_helper {
            std::fs::write(dir.join(NAME), b"").expect("writable");
        }
        dir
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The first directory that has the helper wins, in the order given; one
/// that does not have it, or has a directory by its name, is passed over.
#[test]
fn the_helper_is_the_first_one_found() {
    let temp = Temp::new("found");
    let empty = temp.dir("lib", false);
    let decoy = temp.dir("decoy", false);
    std::fs::create_dir_all(decoy.join(NAME)).unwrap();
    let libexec = temp.dir("libexec", true);
    let wrappers = temp.dir("wrappers", true);
    let dirs = [empty.clone(), decoy, libexec.clone(), wrappers];
    let (path, source) = choose(None, &dirs);
    assert_eq!(path, libexec.join(NAME));
    assert_eq!(source, Source::Found);
    let line = describe(&path, &source, &dirs);
    assert!(line.starts_with(&format!("the helper is {}", libexec.join(NAME).display())));
    assert!(line.contains(&empty.display().to_string()), "{line}");
}

/// `CAUSTICS_POLKIT_HELPER` names one outright, found or not; set but empty
/// is unset.
#[test]
fn the_override_names_the_helper_outright() {
    let temp = Temp::new("override");
    let dirs = [temp.dir("lib", true)];
    let (path, source) = choose(Some("/opt/helper".into()), &dirs);
    assert_eq!(path, PathBuf::from("/opt/helper"));
    assert_eq!(source, Source::Override);
    assert_eq!(
        describe(&path, &source, &dirs),
        "the helper is /opt/helper (CAUSTICS_POLKIT_HELPER)"
    );
    assert_eq!(choose(Some("".into()), &dirs).1, Source::Found);
}

/// None found: said so, naming where it looked and the override.
#[test]
fn no_helper_found_says_where_it_looked_and_names_the_override() {
    let temp = Temp::new("missing");
    let dirs = [temp.dir("a", false), temp.dir("b", false)];
    let (path, source) = choose(None, &dirs);
    assert_eq!(source, Source::Missing);
    assert_eq!(path, Path::new(DIRS[0]).join(NAME));
    let line = describe(&path, &source, &dirs);
    assert!(
        line.starts_with("no polkit-agent-helper-1 in any of "),
        "{line}"
    );
    assert!(line.contains(&dirs[1].display().to_string()), "{line}");
    assert!(line.contains("CAUSTICS_POLKIT_HELPER"), "{line}");
}
