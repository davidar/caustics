//! `caustics-wallpaper` — the canvas's wallpaper sidecar (docs/canvas.md,
//! *One host, sink sidecars*).
//!
//! It owns the control socket, `$XDG_RUNTIME_DIR/caustics-canvas.sock`, whose
//! language is [`command`]'s; it decodes and fits the pictures, PNG, JPEG or
//! JPEG XL ([`picture`]); and it writes them to the canvas host as `Image`
//! frames, each followed by the `Canvas` frame that shows it (docs/abi.md,
//! *Image*, *Canvas*). The host copies the bytes and the caustic fades; how
//! far a fade is, this process knows from its own clock, which is the clock
//! it stamped the fade with.
//!
//! It owns the dim too ([`dim`]): a `dim` on the socket is a `CANVAS_DIM`
//! under the next id, and the host's `CanvasEvent`s on stdin say when that
//! dim reached its depth or was cancelled by input, which is what a client
//! that asked to `wait` is answered (docs/abi.md, *Canvas event*).
//!
//! The patterns are every sidecar's (docs/abi.md, *Writing a sidecar*): no
//! arguments, settings from its own environment, one witness a line on
//! stderr, and an exit when the host closes stdin. One thing is its own:
//! the path it last showed is kept in `$XDG_RUNTIME_DIR/caustics-canvas.last`,
//! so a restarted sidecar, or a restarted host with its sidecars, shows the
//! session's picture again rather than leaving the canvas at its dim colour
//! (docs/canvas.md, *Failure model*). The runtime directory is the session's
//! and goes with it, so the path is also kept in
//! `$XDG_STATE_HOME/caustics/canvas/last` ([`Last`]), and the first start of a
//! fresh login shows the picture the last session ended on.

pub mod command;
pub mod dim;
pub mod frames;
pub mod picture;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{self, CanvasEventBody, Frame, REQ_OUTPUT_SIZE};

use crate::command::{Command, MAX_LINE};
use crate::dim::{CANCELLED, DONE, Dimmer, Outcome};

/// This sidecar's witness line: `caustics-wallpaper: …` on stderr, one write.
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-wallpaper");

pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// The socket's file name in `$XDG_RUNTIME_DIR`.
pub const SOCKET: &str = "caustics-canvas.sock";

/// The last shown path's file name in `$XDG_RUNTIME_DIR`.
pub const LAST: &str = "caustics-canvas.last";

/// Where the last shown path outlives the session: this, under
/// `$XDG_STATE_HOME` (`~/.local/state` when it is unset), beside the bar's
/// `caustics/bar/favourites`.
pub const LAST_STATE: &str = "caustics/canvas/last";

/// The state file's path, from `$XDG_STATE_HOME` and `$HOME` as given: the
/// XDG base-directory rule, where a relative `$XDG_STATE_HOME` is ignored.
/// `None` with neither.
pub fn state_last(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    xdg_state_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".local/state"))
        })
        .map(|state| state.join(LAST_STATE))
}

/// The files the last shown path is kept in, in the order a start reads
/// them: the session's runtime file first (it is never older than the
/// state file), then the state file, which is what survives a logout.
#[derive(Clone, Debug)]
pub struct Last {
    pub files: Vec<PathBuf>,
}

impl Last {
    /// The runtime file in `runtime`, then the state file if there is one.
    pub fn new(runtime: &Path, state: Option<PathBuf>) -> Last {
        let mut files = vec![runtime.join(LAST)];
        files.extend(state);
        Last { files }
    }

    /// The first file that names an absolute path, and that file.
    pub fn read(&self) -> Option<(PathBuf, &Path)> {
        self.files.iter().find_map(|file| {
            let text = std::fs::read_to_string(file).ok()?;
            let path = PathBuf::from(text.trim_end());
            path.is_absolute().then_some((path, file.as_path()))
        })
    }

    /// Keep `path` in every file, making the state file's directory; one
    /// witness line for each file that could not be written.
    pub fn keep(&self, path: &Path) {
        for file in &self.files {
            if let Some(dir) = file.parent()
                && let Err(err) = std::fs::create_dir_all(dir)
            {
                witness(&format!(
                    "could not keep the path in {}: {err}",
                    file.display()
                ));
                continue;
            }
            if let Err(err) = std::fs::write(file, path.as_os_str().as_encoded_bytes()) {
                witness(&format!(
                    "could not keep the path in {}: {err}",
                    file.display()
                ));
            }
        }
    }
}

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
    /// What became of a dim (docs/abi.md, *Canvas event*).
    Event(CanvasEventBody),
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
    let listener = match bind(&socket, TAKEOVER_WINDOW) {
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

    let last = Last::new(
        &dir,
        state_last(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME")),
    );
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

/// How long a socket that answers is given to stop answering before it is
/// another process's. The previous sidecar of a restarted set is still being
/// torn down when its successor starts, and its listener can answer a probe
/// for some milliseconds after the unit has reported it gone; a probe read
/// as "another wallpaper's" at that instant would leave the canvas with no
/// socket and the dim colour until the next restart.
pub const TAKEOVER_WINDOW: Duration = Duration::from_secs(2);

/// The socket, bound: a stale one (nothing answers on it) is removed first.
/// One that answers is probed again across `window`, and one still answering
/// at its end is another process's, which this one does not take.
fn bind(path: &Path, window: Duration) -> io::Result<UnixListener> {
    let started = Instant::now();
    loop {
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                if started.elapsed() >= window {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "{} is another wallpaper's (it answered for {} ms)",
                            path.display(),
                            window.as_millis()
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            match std::fs::remove_file(path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        return UnixListener::bind(path);
    }
}

/// What the host writes: the output size, and the events of a dim.
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
            Ok(Some(Frame::CanvasEvent(event))) => into.send(Input::Event(event)),
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
fn serve(inbox: &Receiver<Input>, last: &Last) -> i32 {
    let mut size: Option<(u32, u32)> = None;
    let mut shown: Option<Shown> = None;
    let mut serial = 0u32;
    // The dim, and the one client that may be waiting on it.
    let mut dims: Dimmer<Sender<String>> = Dimmer::new();
    // The picture the session (or the last session) showed, to be shown
    // again once the host has said how big its outputs are (or after
    // RESTORE_WAIT). One that is gone is one line and no picture.
    let mut restore: Option<(PathBuf, Instant)> = match last.read() {
        None => None,
        Some((path, from)) if !path.exists() => {
            witness(&format!(
                "the session last showed {} (kept in {}), which is gone; nothing to show",
                path.display(),
                from.display()
            ));
            None
        }
        Some((path, from)) => {
            witness(&format!(
                "the session last showed {} (kept in {}); showing it again",
                path.display(),
                from.display()
            ));
            Some((path, Instant::now() + RESTORE_WAIT))
        }
    };
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
                    match show(&mut out, &path, 0, size, &mut serial, last) {
                        Ok(again) => shown = Some(again),
                        Err(err) => witness(&format!("{}: {err}; nothing to show", path.display())),
                    }
                }
            }
            Some(Input::Event(event)) => match dims.event(event.event, event.id) {
                Outcome::Stale => witness(&format!(
                    "a late event {} for dim {}, which is not the dim now; ignored",
                    event.event, event.id
                )),
                Outcome::Done(waiter) => {
                    witness(&format!("dim {} reached its depth", event.id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(DONE.to_owned());
                    }
                }
                Outcome::Cancelled(waiter) => {
                    // The host has taken the overlay down already; the
                    // undim makes the two ends agree the dim is over.
                    if let Err(err) = wire::write(&mut out, &frames::undim()) {
                        return gone(err);
                    }
                    witness(&format!("dim {} cancelled by input; undim", event.id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(CANCELLED.to_owned());
                    }
                }
            },
            Some(Input::Command(command, reply)) => {
                // Taken by a dim that is waited on, which answers later.
                let mut reply = Some(reply);
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
                    Command::Status => {
                        let now = boottime_ms();
                        format!(
                            "ok {} {}",
                            dims.describe(now),
                            describe(shown.as_ref(), now)
                        )
                    }
                    Command::Dim {
                        alpha_milli,
                        over_ms,
                        wait,
                    } => {
                        let start = boottime_ms();
                        let waiter = if wait { reply.take() } else { None };
                        let (id, replaced) = dims.begin(start, over_ms, waiter);
                        if let Some(replaced) = replaced {
                            let _ = replaced.send(CANCELLED.to_owned());
                        }
                        let frame = frames::dim(id, alpha_milli, start, over_ms);
                        match wire::write(&mut out, &frame) {
                            Ok(()) => {
                                witness(&format!(
                                    "dim {id} to {alpha_milli}/1000 over {over_ms} ms{}",
                                    if wait { ", waited on" } else { "" }
                                ));
                                "ok".to_owned()
                            }
                            Err(err) => return gone(err),
                        }
                    }
                    Command::Undim => {
                        if let Some(waiter) = dims.undim() {
                            let _ = waiter.send(CANCELLED.to_owned());
                        }
                        match wire::write(&mut out, &frames::undim()) {
                            Ok(()) => {
                                witness("undim");
                                "ok".to_owned()
                            }
                            Err(err) => return gone(err),
                        }
                    }
                };
                if let Some(reply) = reply {
                    let _ = reply.send(answer);
                }
            }
            None => {
                if let Some((path, _)) = restore.take() {
                    witness("no output size from the host yet; fitting to the wire's cap");
                    match show(&mut out, &path, 0, size, &mut serial, last) {
                        Ok(again) => shown = Some(again),
                        Err(err) => witness(&format!("{}: {err}; nothing to show", path.display())),
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
    last: &Last,
) -> Result<Shown, String> {
    let started = Instant::now();
    let pixels = picture::load(path)?;
    let (from_width, from_height) = (pixels.width, pixels.height);
    let fitted = picture::fit(pixels, size);
    *serial = serial.wrapping_add(1);
    let key = frames::key(std::process::id(), *serial);
    let (width, height) = (fitted.width, fitted.height);
    let (sent, start_ms) = frames::write_show(out, &key, fitted, fade_ms, boottime_ms)
        .map_err(|err| format!("the host's pipe: {err}"))?;
    witness(&format!(
        "showing {} as {key}: {width}x{height} from {}x{}, {sent} frames, fading over {fade_ms} ms \
         ({} ms to decode and fit)",
        path.display(),
        from_width,
        from_height,
        started.elapsed().as_millis()
    ));
    last.keep(path);
    Ok(Shown {
        path: path.to_owned(),
        start_ms,
        duration_ms: fade_ms,
    })
}

#[cfg(test)]
mod tests;
