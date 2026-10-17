//! **`printf` for a caustic** (docs/abi.md, *The witness ring*): the witness ring, printed.
//!
//! A witness is a *format literal* in the caustic (`caustics/`) and up to ten words.
//! Slang records the literal's text in the reflection's string table, the
//! caustic pushes its `getStringHash` as the record's head, and this file
//! resolves the head through that table and substitutes. There is **no
//! per-witness Rust**: a new line in the journal is one Slang line, and adding
//! one needs nothing here.
//!
//! ```text
//! witness(WITNESS_INFO, getStringHash("dock: {0} entries ({1} running)"), n, r);
//! ```
//!
//! | spec | reads | prints |
//! |---|---|---|
//! | `{n}` | `arg[n]` | unsigned decimal |
//! | `{n:x}` | `arg[n]` | `0x` and eight hex digits |
//! | `{n:f}` | `arg[n]` as an f32 | Rust's `{}` for `f32` (`724`, `0.5`) |
//! | `{n:s}` | `arg[n]` as a hash | that literal's own text |
//! | `{n:str}` | `arg[n]`, `arg[n+1]` | UTF-8 at that range of `strings` |
//! | `{n:u64}` | `arg[n]`, `arg[n+1]` | `lo + (hi << 32)`, decimal |
//!
//! `{{` and `}}` are braces. A spec past `arg[9]`, a hash the table lacks or a
//! range past the strings buffer prints `?` — the honest answer, since a
//! caustic that has since been swapped may name a literal this table no longer
//! has — and the embassy warns once per literal per generation.
//!
//! # The target is a fixed match, not the word itself
//!
//! `tracing`'s `target:` must be a literal at the macro's call site, so the
//! "word before the first colon" rule (docs/abi.md, *The witness ring*) is three arms rather than an
//! open-ended lookup: `layout:` → `caustics_bar::layout`, `shape:` →
//! `caustics_bar::shape`, everything else → `caustics_bar`. That is exactly what `RUST_LOG=info,caustics_bar::layout=debug,caustics_bar::shape=debug` turns on (the test harness sets it; bar/systemd/README.md shows how to set it on the unit), and a fourth target is a line here as well as a line in Slang.
//!
//! # Once per change is the caustic's duty
//!
//! This file prints every record it is handed, in order, and dedups nothing
//! (once-per-change is the caustic's, against what it persisted in its
//! private buffer). That is deliberate: the pass that decided a thing
//! changed is the pass that knows.

use std::collections::HashSet;

use crate::abi::{self, Note};
use crate::strings::Strings;

/// What a `tracing` event needs that a format literal cannot carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Layout,
    Shape,
    Bar,
}

impl Target {
    /// docs/abi.md, *The witness ring*'s rule: the word before the line's first colon, when it is one of
    /// the ones a `RUST_LOG` filter knows.
    fn of(line: &str) -> Target {
        match line.split(':').next() {
            Some("layout") => Target::Layout,
            Some("shape") => Target::Shape,
            _ => Target::Bar,
        }
    }
}

/// The printer's own memory: which literals it has already complained about.
///
/// One warning per literal per generation, because a caustic that names a
/// format this build cannot resolve names it on every frame it draws.
#[derive(Debug, Default)]
pub struct Printer {
    warned: HashSet<u32>,
}

impl Printer {
    pub fn new() -> Printer {
        Printer::default()
    }

    /// A fresh caustic: whatever this printer could not resolve before is
    /// about a program that is no longer running.
    pub fn reset(&mut self) {
        self.warned.clear();
    }

    /// Print one record of the witness ring.
    pub fn print(&mut self, note: &Note, strings: &Strings, bytes: &[u8]) {
        let Some(format) = strings.get(note.head) else {
            if self.warned.insert(note.head) {
                tracing::warn!(
                    "witness: the caustic named format {:#010x}, which this generation's \
                     string table has no text for",
                    note.head
                );
            }
            return;
        };
        let (line, complete) = render(format, note, strings, bytes);
        if !complete && self.warned.insert(note.head) {
            tracing::warn!("witness: {line:?} has a spec this bar could not fill");
        }
        emit(Target::of(&line), note.aux, &line);
    }
}

/// One record, rendered. The `bool` is false when any spec printed `?`.
pub fn render(format: &str, note: &Note, strings: &Strings, bytes: &[u8]) -> (String, bool) {
    let mut out = String::with_capacity(format.len() + 16);
    let mut complete = true;
    let mut rest = format;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        let brace = &rest[at..at + 1];
        let after = &rest[at + 1..];
        // `{{` and `}}` are braces.
        if after.starts_with(brace) {
            out.push_str(brace);
            rest = &after[1..];
            continue;
        }
        if brace == "}" {
            // A stray `}`: print it and carry on. A format literal is the
            // caustic's own text and a typo in one is not worth a hole.
            out.push('}');
            rest = after;
            continue;
        }
        let Some(end) = after.find('}') else {
            out.push('{');
            rest = after;
            continue;
        };
        match spec(&after[..end], note, strings, bytes) {
            Some(text) => out.push_str(&text),
            None => {
                out.push('?');
                complete = false;
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    (out, complete)
}

/// One `{…}`, filled. `None` is the `?` case: an index past the record, a hash
/// the table lacks, a range past the bytes, or a spec nobody writes.
fn spec(body: &str, note: &Note, strings: &Strings, bytes: &[u8]) -> Option<String> {
    let (index, kind) = match body.split_once(':') {
        Some((index, kind)) => (index, kind),
        None => (body, ""),
    };
    let index: usize = index.parse().ok()?;
    let word = |at: usize| -> Option<u32> { note.arg.get(at).copied() };
    let first = word(index)?;
    Some(match kind {
        "" => first.to_string(),
        "x" => format!("{first:#010x}"),
        "f" => f32::from_bits(first).to_string(),
        "s" => strings.get(first)?.to_owned(),
        "u64" => (u64::from(first) | (u64::from(word(index + 1)?) << 32)).to_string(),
        "str" => {
            let len = word(index + 1)? as usize;
            let start = first as usize;
            let end = start.checked_add(len)?;
            String::from_utf8_lossy(bytes.get(start..end)?).into_owned()
        }
        _ => return None,
    })
}

/// The nine arms the two fixed facts — three targets, three levels — make.
/// Both have to be literals at the call site, which is why this is a table and
/// not a pair of variables.
fn emit(target: Target, level: u32, line: &str) {
    match (target, level) {
        (Target::Layout, abi::WITNESS_WARN) => {
            tracing::warn!(target: "caustics_bar::layout", "{line}")
        }
        (Target::Layout, abi::WITNESS_INFO) => {
            tracing::info!(target: "caustics_bar::layout", "{line}")
        }
        (Target::Layout, _) => tracing::debug!(target: "caustics_bar::layout", "{line}"),
        (Target::Shape, abi::WITNESS_WARN) => {
            tracing::warn!(target: "caustics_bar::shape", "{line}")
        }
        (Target::Shape, abi::WITNESS_INFO) => {
            tracing::info!(target: "caustics_bar::shape", "{line}")
        }
        (Target::Shape, _) => tracing::debug!(target: "caustics_bar::shape", "{line}"),
        (Target::Bar, abi::WITNESS_WARN) => tracing::warn!(target: "caustics_bar", "{line}"),
        (Target::Bar, abi::WITNESS_INFO) => tracing::info!(target: "caustics_bar", "{line}"),
        (Target::Bar, _) => tracing::debug!(target: "caustics_bar", "{line}"),
    }
}

#[cfg(test)]
mod tests;
