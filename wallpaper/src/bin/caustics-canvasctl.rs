//! `caustics-canvasctl` — the canvas's client for scripts.
//!
//! ```text
//! caustics-canvasctl show <path> [fade <seconds>]
//! caustics-canvasctl status
//! caustics-canvasctl dim <alpha> [over <seconds>] [--wait]
//! caustics-canvasctl undim
//! ```
//!
//! One line to `caustics-wallpaper`'s socket and its one reply back. The reply
//! is printed without its `ok`, and the exit is 0 for `ok` and 1 for anything
//! else, an unreachable socket included.
//!
//! `dim … --wait` is chayang's contract (docs/canvas.md, *How each job is
//! wired*): it returns when the dim has reached its depth, **0**; when input
//! cancelled it, or an undim or a newer dim ended it first, **2**; and **1**
//! when it could not run or the canvas went away mid-dim, so that a stage
//! that locks on anything but 2 locks on a broken fade too.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
/// A waited dim that input (or an undim, or a newer dim) ended.
const EXIT_CANCELLED: i32 = 2;

/// How long a reply may take: a show decodes and fits a picture first. A
/// waited dim may take this much longer than its own duration.
const REPLY_WAIT: Duration = Duration::from_secs(60);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wait = waits_for(&args);
    let line = match line_of(&args) {
        Ok(line) => line,
        Err(why) => {
            eprintln!("caustics-canvasctl: {why}");
            eprintln!(
                "usage: caustics-canvasctl show <path> [fade <seconds>] | status | \
                 dim <alpha> [over <seconds>] [--wait] | undim"
            );
            std::process::exit(EXIT_FAILED);
        }
    };
    std::process::exit(match send(&line, wait) {
        Ok(reply) if reply == caustics_wallpaper::dim::DONE => EXIT_OK,
        Ok(reply) if reply == caustics_wallpaper::dim::CANCELLED => {
            eprintln!("caustics-canvasctl: the dim was cancelled");
            EXIT_CANCELLED
        }
        Ok(reply) => match reply.strip_prefix("ok") {
            Some(rest) => {
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    println!("{rest}");
                }
                EXIT_OK
            }
            None => {
                eprintln!(
                    "caustics-canvasctl: {}",
                    reply.strip_prefix("error ").unwrap_or(&reply)
                );
                EXIT_FAILED
            }
        },
        Err(why) => {
            eprintln!("caustics-canvasctl: {why}");
            EXIT_FAILED
        }
    });
}

/// The command line as one line of the socket's language, with a relative
/// path made absolute here, where it means something.
fn line_of(args: &[String]) -> Result<String, String> {
    let Some((verb, rest)) = args.split_first() else {
        return Err("no command".to_owned());
    };
    let line = match verb.as_str() {
        "show" => {
            let (path, tail) = rest.split_first().ok_or("show needs a path")?;
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .map_err(|err| format!("no working directory to resolve {path:?}: {err}"))?
                    .join(path)
            };
            let mut line = format!("show {}", path.display());
            for word in tail {
                line.push(' ');
                line.push_str(word);
            }
            line
        }
        "dim" => std::iter::once("dim")
            .chain(rest.iter().map(|word| match word.as_str() {
                // The flag is this client's spelling of the socket's word.
                "--wait" => "wait",
                word => word,
            }))
            .collect::<Vec<_>>()
            .join(" "),
        "status" | "undim" => std::iter::once(verb.as_str())
            .chain(rest.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        other => return Err(format!("{other:?} is not a command")),
    };
    // The sidecar parses it; this side only refuses what cannot be one line.
    if line.contains('\n') {
        return Err("a command is one line".to_owned());
    }
    Ok(line)
}

/// For a `dim … --wait`, how long its dim may take: the `over` it names (0
/// without one). `None` for anything else.
fn waits_for(args: &[String]) -> Option<Duration> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if words.first() != Some(&"dim") || !words.contains(&"--wait") {
        return None;
    }
    let over = words
        .windows(2)
        .find(|pair| pair[0] == "over")
        .and_then(|pair| pair[1].parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map_or(Duration::ZERO, |seconds| {
            Duration::from_secs_f64(seconds.min(caustics_wallpaper::command::MAX_SECONDS))
        });
    Some(over)
}

fn send(line: &str, wait: Option<Duration>) -> Result<String, String> {
    let dir = caustics_wallpaper::runtime_dir().ok_or("no XDG_RUNTIME_DIR")?;
    let socket = dir.join(caustics_wallpaper::SOCKET);
    let mut stream = UnixStream::connect(&socket).map_err(|err| {
        format!(
            "no canvas at {} ({err}); is caustics-canvas running?",
            socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(REPLY_WAIT + wait.unwrap_or(Duration::ZERO)))
        .map_err(|err| format!("{err}"))?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|err| format!("could not send: {err}"))?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|err| format!("no reply: {err}"))?;
    if reply.is_empty() {
        return Err("the canvas closed the socket without a reply".to_owned());
    }
    Ok(reply.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    /// `--wait` is the socket's `wait`, last; and only a waited dim waits
    /// past the reply bound, by its own duration.
    #[test]
    fn a_waited_dim_is_the_socket_s_wait_and_waits_its_duration() {
        let waited = args(&["dim", "1", "over", "10", "--wait"]);
        assert_eq!(line_of(&waited).as_deref(), Ok("dim 1 over 10 wait"));
        assert_eq!(waits_for(&waited), Some(Duration::from_secs(10)));
        let plain = args(&["dim", "0.5"]);
        assert_eq!(line_of(&plain).as_deref(), Ok("dim 0.5"));
        assert_eq!(waits_for(&plain), None);
        assert_eq!(
            waits_for(&args(&["dim", "1", "--wait"])),
            Some(Duration::ZERO)
        );
        assert_eq!(waits_for(&args(&["status"])), None);
    }
}
