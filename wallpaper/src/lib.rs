//! `caustics-wallpaper` — the canvas's wallpaper sidecar (docs/canvas.md,
//! *One host, sink sidecars*).
//!
//! It owns the control socket, `$XDG_RUNTIME_DIR/caustics-canvas.sock`, whose
//! language is [`command`]'s; it decodes and fits the pictures ([`picture`]);
//! and it writes them to the canvas host as `Image` frames, each followed by
//! the `Canvas` frame that shows it (docs/abi.md, *Image*, *Canvas*). The host
//! copies the bytes and the caustic fades; how far a fade is, this process
//! knows from its own clock, which is the clock it stamped the fade with.
//!
//! The patterns are every sidecar's (docs/abi.md, *Writing a sidecar*): no
//! arguments, settings from its own environment, one witness a line on
//! stderr, and an exit when the host closes stdin. One thing is its own:
//! the path it last showed is kept in `$XDG_RUNTIME_DIR/caustics-canvas.last`,
//! so a restarted sidecar, or a restarted host with its sidecars, shows the
//! session's picture again rather than leaving the canvas at its dim colour
//! (docs/canvas.md, *Failure model*). The runtime directory is the session's
//! and goes with it.

pub mod command;
pub mod frames;
pub mod picture;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{self, Frame, REQ_OUTPUT_SIZE};

use crate::command::{Command, MAX_LINE};

/// This sidecar's witness line: `caustics-wallpaper: …` on stderr, one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-wallpaper");

pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The socket's file name in `$XDG_RUNTIME_DIR`.
pub const SOCKET: &str = "caustics-canvas.sock";

/// The last shown path's file name in `$XDG_RUNTIME_DIR`.
pub const LAST: &str = "caustics-canvas.last";

/// How long a start waits for the host's output size before it shows the
/// last picture anyway, fitted to the wire's cap.
pub const RESTORE_WAIT: Duration = Duration::from_secs(2);

/// The session's runtime directory, or `None` without one.
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// `CLOCK_BOOTTIME` now, in ms: the clock a fade is stamped with, the one
/// that counts a suspend (docs/canvas.md, *Time*).
pub fn boottime_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the call's duration.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

/// What the main loop is woken by.
enum Input {
    /// The host's largest output, px.
    Size(u32, u32),
    /// The host closed stdin.
    Eof,
    /// A command from a client, and where its reply goes.
    Command(Command, Sender<String>),
}

/// The picture on screen, as this process sent it.
#[derive(Clone, Debug)]
struct Shown {
    path: PathBuf,
    start_ms: u64,
    duration_ms: u32,
}

/// `fade=… image=…`: how far the shown picture's fade is at `now`, and what
/// it is. The path last, so a path with spaces reads whole.
fn describe(shown: Option<&Shown>, now_ms: u64) -> String {
    let Some(shown) = shown else {
        return "fade=none image=none".to_owned();
    };
    let elapsed = now_ms.saturating_sub(shown.start_ms);
    if shown.duration_ms == 0 || elapsed >= u64::from(shown.duration_ms) {
        return format!("fade=done image={}", shown.path.display());
    }
    format!(
        "fade=fading progress={:.2} duration={:.3} image={}",
        elapsed as f64 / f64::from(shown.duration_ms),
        f64::from(shown.duration_ms) / 1000.0,
        shown.path.display()
    )
}

/// The whole program. Returns the exit status.
pub fn run() -> i32 {
    let (sender, inbox) = mpsc::channel();
    let stdin_sender = sender.clone();
    if std::thread::Builder::new()
        .name("stdin".to_owned())
        .spawn(move || listen_stdin(stdin_sender))
        .is_err()
    {
        witness("could not read stdin");
        return 1;
    }
    let Some(dir) = runtime_dir() else {
        witness("no XDG_RUNTIME_DIR; there is nowhere to put the socket");
        idle(&inbox);
        return 0;
    };
    let socket = dir.join(SOCKET);
    let listener = match bind(&socket) {
        Ok(listener) => listener,
        Err(err) => {
            // Another canvas's wallpaper holds it, or the directory is not
            // ours: say so once and stay, so the host does not restart this
            // process into the same answer every second.
            witness(&format!("{err}; no socket, nothing to show"));
            idle(&inbox);
            return 0;
        }
    };
    witness(&format!("listening on {}", socket.display()));
    let accept_sender = sender.clone();
    if std::thread::Builder::new()
        .name("socket".to_owned())
        .spawn(move || accept(listener, accept_sender))
        .is_err()
    {
        witness("could not listen on the socket");
        let _ = std::fs::remove_file(&socket);
        return 1;
    }
    drop(sender);

    let last = dir.join(LAST);
    let code = serve(&inbox, &last);
    let _ = std::fs::remove_file(&socket);
    code
}

/// Wait for the host to close stdin, ignoring everything else.
fn idle(inbox: &Receiver<Input>) {
    while let Ok(input) = inbox.recv() {
        if let Input::Eof = input {
            return;
        }
    }
}

/// The socket, bound: a stale one (nothing answers on it) is removed first;
/// a live one is another process's, and this one does not take it.
fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} is another wallpaper's", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    UnixListener::bind(path)
}

/// The host's requests: the output size is the only one this sidecar reads.
fn listen_stdin(into: Sender<Input>) {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        let sent = match wire::read(&mut input) {
            Ok(None) | Err(_) => {
                let _ = into.send(Input::Eof);
                return;
            }
            Ok(Some(Frame::Request(request))) if request.kind == REQ_OUTPUT_SIZE => {
                match parse_size(&request.name) {
                    Some((w, h)) => into.send(Input::Size(w, h)),
                    None => {
                        witness(&format!("an output size of {:?}; ignored", request.name));
                        Ok(())
                    }
                }
            }
            Ok(Some(other)) => {
                witness(&format!(
                    "stdin: frame kind {} has no meaning here",
                    other.kind()
                ));
                Ok(())
            }
        };
        if sent.is_err() {
            return;
        }
    }
}

/// `WIDTHxHEIGHT`, both positive.
pub fn parse_size(name: &str) -> Option<(u32, u32)> {
    let (w, h) = name.split_once('x')?;
    let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// One thread a connection: lines in, a reply each, through the main loop.
fn accept(listener: UnixListener, into: Sender<Input>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let into = into.clone();
        let _ = std::thread::Builder::new()
            .name("client".to_owned())
            .spawn(move || client(stream, into));
    }
}

fn client(stream: UnixStream, into: Sender<Input>) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut lines = BufReader::new(stream.take(1 << 20));
    let mut line = String::new();
    loop {
        line.clear();
        match lines.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.len() > MAX_LINE {
            let _ = out.write_all(b"error a line longer than the socket reads\n");
            return;
        }
        let reply = match command::parse(&line) {
            Ok(command) => {
                let (tx, rx) = mpsc::channel();
                if into.send(Input::Command(command, tx)).is_err() {
                    return;
                }
                rx.recv()
                    .unwrap_or_else(|_| "error the sidecar is leaving".to_owned())
            }
            Err(why) => format!("error {why}"),
        };
        if out.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return;
        }
    }
}

/// The main loop: one thread owns stdout and the state.
fn serve(inbox: &Receiver<Input>, last: &Path) -> i32 {
    let mut size: Option<(u32, u32)> = None;
    let mut shown: Option<Shown> = None;
    let mut serial = 0u32;
    // The picture the session last showed, to be shown again once the host
    // has said how big its outputs are (or after RESTORE_WAIT).
    let mut restore: Option<(PathBuf, Instant)> = std::fs::read_to_string(last)
        .ok()
        .map(|text| PathBuf::from(text.trim_end()))
        .filter(|path| path.is_absolute())
        .map(|path| (path, Instant::now() + RESTORE_WAIT));
    if let Some((path, _)) = &restore {
        witness(&format!(
            "the session last showed {}; showing it again",
            path.display()
        ));
    }
    let stdout = io::stdout();
    loop {
        let wait = restore
            .as_ref()
            .map_or(Duration::from_secs(3600), |(_, at)| {
                at.saturating_duration_since(Instant::now())
            });
        let input = match inbox.recv_timeout(wait) {
            Ok(input) => Some(input),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return 0,
        };
        let mut out = stdout.lock();
        match input {
            Some(Input::Eof) => return 0,
            Some(Input::Size(w, h)) => {
                let changed = size != Some((w, h));
                size = Some((w, h));
                if changed {
                    witness(&format!("the canvas covers {w}x{h} px"));
                    // The picture on screen was fitted to the old size; send
                    // it again, at once, for the new one.
                    if restore.is_none()
                        && let Some(now) = shown.clone()
                    {
                        let _ = show(&mut out, &now.path, 0, size, &mut serial, last)
                            .map(|again| shown = Some(again));
                    }
                }
                if let Some((path, _)) = restore.take() {
                    if let Ok(again) = show(&mut out, &path, 0, size, &mut serial, last) {
                        shown = Some(again);
                    }
                }
            }
            Some(Input::Command(command, reply)) => {
                let answer = match command {
                    Command::Show { path, fade_ms } => {
                        restore = None;
                        match show(&mut out, &path, fade_ms, size, &mut serial, last) {
                            Ok(now) => {
                                shown = Some(now);
                                "ok".to_owned()
                            }
                            Err(why) => format!("error {why}"),
                        }
                    }
                    Command::Status => format!("ok {}", describe(shown.as_ref(), boottime_ms())),
                    Command::Dim {
                        alpha_milli,
                        over_ms,
                    } => {
                        let frame = frames::dim(alpha_milli, boottime_ms(), over_ms);
                        match wire::write(&mut out, &frame) {
                            Ok(()) => {
                                witness(&format!("dim to {alpha_milli}/1000 over {over_ms} ms"));
                                "ok".to_owned()
                            }
                            Err(err) => return gone(err),
                        }
                    }
                    Command::Undim => match wire::write(&mut out, &frames::undim()) {
                        Ok(()) => {
                            witness("undim");
                            "ok".to_owned()
                        }
                        Err(err) => return gone(err),
                    },
                };
                let _ = reply.send(answer);
            }
            None => {
                if let Some((path, _)) = restore.take() {
                    witness("no output size from the host yet; fitting to the wire's cap");
                    if let Ok(again) = show(&mut out, &path, 0, size, &mut serial, last) {
                        shown = Some(again);
                    }
                }
            }
        }
    }
}

/// stdout is gone: the host left.
fn gone(err: io::Error) -> i32 {
    witness(&format!("stdout: {err}; exiting"));
    0
}

/// Decode, fit, send the picture and the show that fades to it; remember the
/// path for the session. A file that will not decode is the client's error
/// and sends nothing.
fn show(
    out: &mut impl Write,
    path: &Path,
    fade_ms: u32,
    size: Option<(u32, u32)>,
    serial: &mut u32,
    last: &Path,
) -> Result<Shown, String> {
    let started = Instant::now();
    let pixels = picture::load(path)?;
    let (width, height) = picture::fitted_size(pixels.width, pixels.height, size);
    let fitted = picture::shrink(&pixels, width, height);
    *serial = serial.wrapping_add(1);
    let key = frames::key(std::process::id(), *serial);
    let (width, height) = (fitted.width, fitted.height);
    let (sent, start_ms) = frames::write_show(out, &key, fitted, fade_ms, boottime_ms)
        .map_err(|err| format!("the host's pipe: {err}"))?;
    witness(&format!(
        "showing {} as {key}: {width}x{height} from {}x{}, {sent} frames, fading over {fade_ms} ms \
         ({} ms to decode and fit)",
        path.display(),
        pixels.width,
        pixels.height,
        started.elapsed().as_millis()
    ));
    if let Err(err) = std::fs::write(last, path.as_os_str().as_encoded_bytes()) {
        witness(&format!(
            "could not keep the path in {}: {err}",
            last.display()
        ));
    }
    Ok(Shown {
        path: path.to_owned(),
        start_ms,
        duration_ms: fade_ms,
    })
}

#[cfg(test)]
mod tests;
