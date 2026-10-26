//! `caustics-canvasctl` — the canvas's client for scripts.
//!
//! ```text
//! caustics-canvasctl show <path> [fade <seconds>]
//! caustics-canvasctl status
//! caustics-canvasctl dim <alpha> [over <seconds>] [--wait]
//! caustics-canvasctl undim
//! caustics-canvasctl lock [--wait]
//! ```
//!
//! One line to `caustics-wallpaper`'s socket and its one reply back. The reply
//! is printed without its `ok`, and the exit is 0 for `ok` and 1 for anything
//! else, an unreachable socket included.
//!
//! `lock` goes to `caustics-lock`'s socket instead (lock/README.md): it asks
//! for the lock and exits 0 on `ok`; with `--wait` it returns only once the
//! canvas has said the session is locked, **0** with `locked`, which is the
//! before-sleep hold (docs/canvas.md, *How each job is wired*), and **1** for
//! anything else, the canvas going first included. `status` asks the lock's
//! socket too, and prints its line after the wallpaper's when it answers.
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

/// `caustics-lock`'s socket in `$XDG_RUNTIME_DIR` (`caustics_lock::socket::
/// SOCKET`): named here rather than taken from that crate, which would bring
/// logind's bus and libpam into a client that needs neither.
const LOCK_SOCKET: &str = "caustics-lock.sock";

/// The lock's reply to a `lock wait` once the session is locked.
const LOCKED: &str = "locked";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("lock") {
        std::process::exit(lock(&args[1..]));
    }
    let wait = waits_for(&args);
    let line = match line_of(&args) {
        Ok(line) => line,
        Err(why) => {
            eprintln!("caustics-canvasctl: {why}");
            eprintln!(
                "usage: caustics-canvasctl show <path> [fade <seconds>] | status | \
                 dim <alpha> [over <seconds>] [--wait] | undim | lock [--wait]"
            );
            std::process::exit(EXIT_FAILED);
        }
    };
    let reply = send(caustics_wallpaper::SOCKET, &line, wait);
    // The lock's line, printed after the wallpaper's, when its socket
    // answers; the exit is still the wallpaper's. No lock sidecar is no line
    // (the wallpaper runs without one); a lock that is there and refuses
    // says why on stderr, so the two never look the same.
    let lock_line = (line == "status")
        .then(|| send(LOCK_SOCKET, "status", None).ok())
        .flatten()
        .and_then(|reply| match reply.strip_prefix("ok ") {
            Some(rest) => Some(rest.to_owned()),
            None => {
                eprintln!(
                    "caustics-canvasctl: the lock: {}",
                    reply.strip_prefix("error ").unwrap_or(&reply)
                );
                None
            }
        });
    let code = match reply {
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
    };
    if let Some(lock_line) = lock_line {
        println!("{lock_line}");
    }
    std::process::exit(code);
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

/// `caustics-canvasctl lock [--wait]`: one line to the lock's socket.
fn lock(args: &[String]) -> i32 {
    let Some(line) = lock_line(args) else {
        eprintln!("caustics-canvasctl: usage: caustics-canvasctl lock [--wait]");
        return EXIT_FAILED;
    };
    let wait = line == "lock wait";
    // A waited lock waits for the canvas's word, which a lock that is not
    // coming never sends: the same bound as any other reply.
    match send(LOCK_SOCKET, line, None) {
        Ok(reply) if wait && reply == LOCKED => {
            println!("{LOCKED}");
            EXIT_OK
        }
        Ok(reply) if !wait && reply == "ok" => EXIT_OK,
        Ok(reply) => {
            eprintln!(
                "caustics-canvasctl: {}",
                reply.strip_prefix("error ").unwrap_or(&reply)
            );
            EXIT_FAILED
        }
        Err(why) => {
            eprintln!("caustics-canvasctl: {why}");
            EXIT_FAILED
        }
    }
}

/// `lock`'s arguments as the lock socket's line.
fn lock_line(args: &[String]) -> Option<&'static str> {
    match args {
        [] => Some("lock"),
        [flag] if flag == "--wait" => Some("lock wait"),
        _ => None,
    }
}

fn send(name: &str, line: &str, wait: Option<Duration>) -> Result<String, String> {
    let dir = caustics_wallpaper::runtime_dir().ok_or("no XDG_RUNTIME_DIR")?;
    let socket = dir.join(name);
    let mut stream = UnixStream::connect(&socket).map_err(|err| {
        let whose = if name == LOCK_SOCKET {
            "caustics-lock"
        } else {
            "caustics-canvas"
        };
        format!("no {whose} at {} ({err}); is it running?", socket.display())
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

    /// `lock` speaks the lock's socket: `--wait` is its `wait`, and nothing
    /// else is an argument.
    #[test]
    fn a_lock_is_the_lock_socket_s_line() {
        assert_eq!(lock_line(&args(&[])), Some("lock"));
        assert_eq!(lock_line(&args(&["--wait"])), Some("lock wait"));
        assert_eq!(lock_line(&args(&["now"])), None);
        assert_eq!(lock_line(&args(&["--wait", "--wait"])), None);
    }
}
