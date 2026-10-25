//! `caustics-canvasctl` — the canvas's client for scripts.
//!
//! ```text
//! caustics-canvasctl show <path> [fade <seconds>]
//! caustics-canvasctl status
//! caustics-canvasctl dim <alpha> [over <seconds>]
//! caustics-canvasctl undim
//! ```
//!
//! One line to `caustics-wallpaper`'s socket and its one reply back. The reply
//! is printed without its `ok`, and the exit is 0 for `ok` and 1 for anything
//! else, an unreachable socket included. 2 is reserved for a dim that input
//! cancelled (docs/canvas.md, *How each job is wired*), which this version
//! never reports.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;

/// How long a reply may take: a show decodes and fits a picture first.
const REPLY_WAIT: Duration = Duration::from_secs(60);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let line = match line_of(&args) {
        Ok(line) => line,
        Err(why) => {
            eprintln!("caustics-canvasctl: {why}");
            eprintln!(
                "usage: caustics-canvasctl show <path> [fade <seconds>] | status | \
                 dim <alpha> [over <seconds>] | undim"
            );
            std::process::exit(EXIT_FAILED);
        }
    };
    std::process::exit(match send(&line) {
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
        "status" | "dim" | "undim" => std::iter::once(verb.as_str())
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

fn send(line: &str) -> Result<String, String> {
    let dir = caustics_wallpaper::runtime_dir().ok_or("no XDG_RUNTIME_DIR")?;
    let socket = dir.join(caustics_wallpaper::SOCKET);
    let mut stream = UnixStream::connect(&socket).map_err(|err| {
        format!(
            "no canvas at {} ({err}); is caustics-canvas running?",
            socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(REPLY_WAIT))
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
