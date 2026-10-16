//! A body is plain text by the time it is a label.
//!
//! The server does not advertise `body-markup`, so by the specification a
//! client sends plain text — and many send markup anyway (`<b>`, a link from a
//! chat client, an `<img>` from a mail client). The engine draws text, not
//! HTML, so the five tags the specification names are dropped, `<br>` becomes
//! a line break, and the five XML entities and numeric references are
//! unescaped. Anything else that starts with `<` is text: `a <3 b` stays as it
//! was sent.

/// The tags removed, open or close, with any attributes.
const TAGS: &[&str] = &["b", "i", "u", "a", "img", "br"];

/// `text` with markup removed and entities unescaped.
pub fn strip(text: &str) -> String {
    unescape(&strip_tags(text))
}

/// A body as the frame carries it: markup stripped, its
/// own line breaks kept as `\n` (a `\r\n` or a lone `\r` is one), trailing
/// spaces off each line, blank lines at either end off, and at most
/// [`BODY_MAX`] bytes — cut on a char boundary, an ellipsis standing in for
/// the rest. The caustic wraps it; nothing here measures a line.
pub fn body(text: &str) -> String {
    let text = strip(text).replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    let joined = match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    };
    cut_bytes(&joined, BODY_MAX)
}

/// The most of a body a notification keeps, in bytes. The bar shapes a few
/// hundred of them (`BODY_SHAPE_BYTES` in the tunables); the rest is for a
/// later reader, and a cap is what keeps a client's megabyte out of the
/// history.
pub const BODY_MAX: usize = 2048;

/// `text` cut to at most `max` bytes on a char boundary, `…` (three bytes)
/// ending it when anything was cut.
pub fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut keep = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}…", text[..keep].trim_end())
}

/// The first non-empty line of `text`, trimmed: what a menu row shows of a
/// summary. Empty when there is none.
pub fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match tag_len(tail) {
            Some((len, name)) => {
                if name == "br" {
                    out.push('\n');
                }
                rest = &tail[len..];
            }
            None => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// When `tail` (which starts with `<`) opens one of [`TAGS`], its length up to
/// and including the `>`, and the tag's name.
fn tag_len(tail: &str) -> Option<(usize, &'static str)> {
    let end = tail.find('>')?;
    let inner = &tail[1..end];
    let inner = inner.strip_prefix('/').unwrap_or(inner);
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    let name_end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let name = &inner[..name_end];
    TAGS.iter()
        .find(|tag| tag.eq_ignore_ascii_case(name))
        .map(|tag| (end + 1, *tag))
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match entity(tail) {
            Some((len, c)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// When `tail` (which starts with `&`) is an entity this knows, its length
/// including the `;` and the character it stands for.
fn entity(tail: &str) -> Option<(usize, char)> {
    // The longest reference worth reading: `&#x10FFFF;` is ten bytes.
    let end = tail.bytes().take(12).position(|b| b == b';')?;
    let name = &tail[1..end];
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((end + 1, c))
}
