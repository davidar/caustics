//! The control socket's language: one command a line, one reply a line.
//!
//! ```text
//! show <path> [fade <seconds>]          cross-fade to the picture at <path>
//! status                                where the dim is, what is shown, and
//!                                       how far its fade is
//! dim <alpha> [over <seconds>] [wait]   dim towards alpha (0..1) of the dim
//!                                       colour, over every window, until an
//!                                       undim or any input
//! undim                                 no dim, now
//! ```
//!
//! A reply starts `ok` or `error`, but for a `dim … wait`'s: the connection is
//! held until the dim reached its depth, answered `done`, or ended before
//! that or by input after it, answered `cancelled` (*Canvas event* in
//! docs/abi.md says where the two come from). A path is everything between `show ` and a
//! trailing `fade <seconds>`, so it may hold spaces; it must be absolute,
//! because the sidecar's working directory is not the caller's
//! (`caustics-canvasctl` makes it so).

use std::path::PathBuf;

/// The longest line the socket reads; a longer one is an error and the
/// connection is closed.
pub const MAX_LINE: usize = 4096;

/// The longest fade or dim a command may ask for: a day.
pub const MAX_SECONDS: f64 = 86_400.0;

/// One command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Show {
        path: PathBuf,
        fade_ms: u32,
    },
    Status,
    Dim {
        alpha_milli: u32,
        over_ms: u32,
        /// Hold the reply until the dim is `done` or `cancelled`.
        wait: bool,
    },
    Undim,
}

/// Seconds as the wire's milliseconds: finite, not negative, at most
/// [`MAX_SECONDS`].
fn millis(word: &str) -> Result<u32, String> {
    let seconds: f64 = word
        .parse()
        .map_err(|_| format!("{word:?} is not a number of seconds"))?;
    if !seconds.is_finite() || seconds < 0.0 || seconds > MAX_SECONDS {
        return Err(format!(
            "{word} seconds is outside 0..={MAX_SECONDS}; a fade is at most a day"
        ));
    }
    Ok((seconds * 1000.0).round() as u32)
}

/// One line, without its newline.
pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim_end_matches(['\r', '\n']).trim();
    let (verb, rest) = match line.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (line, ""),
    };
    match verb {
        "show" => {
            if rest.is_empty() {
                return Err("show needs a path".to_owned());
            }
            let words: Vec<&str> = rest.rsplitn(3, char::is_whitespace).collect();
            let (path, fade_ms) = match words.as_slice() {
                [seconds, "fade", path] => (path.trim_end(), millis(seconds)?),
                _ => (rest, 0),
            };
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(format!(
                    "{} is not an absolute path; the sidecar's directory is not yours",
                    path.display()
                ));
            }
            Ok(Command::Show { path, fade_ms })
        }
        "status" if rest.is_empty() => Ok(Command::Status),
        "undim" if rest.is_empty() => Ok(Command::Undim),
        "dim" => {
            let mut words: Vec<&str> = rest.split_whitespace().collect();
            let wait = words.last() == Some(&"wait");
            if wait {
                words.pop();
            }
            let (alpha, over_ms) = match words.as_slice() {
                [alpha] => (*alpha, 0),
                [alpha, "over", seconds] => (*alpha, millis(seconds)?),
                _ => return Err("dim takes <alpha> [over <seconds>] [wait]".to_owned()),
            };
            let alpha: f64 = alpha
                .parse()
                .map_err(|_| format!("{alpha:?} is not an alpha"))?;
            if !(0.0..=1.0).contains(&alpha) {
                return Err(format!("{alpha} is outside 0..=1"));
            }
            Ok(Command::Dim {
                alpha_milli: (alpha * 1000.0).round() as u32,
                over_ms,
                wait,
            })
        }
        "" => Err("an empty line".to_owned()),
        other => Err(format!(
            "{other:?} is not a command (show, status, dim, undim), or takes no words"
        )),
    }
}

#[cfg(test)]
mod tests;
