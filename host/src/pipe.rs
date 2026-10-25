//! Sidecars over pipes: child processes whose stdout is the **wire**
//! (docs/abi.md, *The wire*), frame by frame.
//!
//! This is a host's side of the membership test (docs/architecture.md, *Sidecars*): a
//! feeder that does not own a Wayland object and does not write the host's shm
//! or GPU memory is a **child process**. So there is no vocabulary here and no
//! parser — [`wire::read`] checks the magic, the kind, the length and the
//! bounds, and this file forwards what came out. The one thing this file does
//! decide is whether a child is **ours**: its first frame says which contract
//! it was built against (docs/abi.md, *Hello*), and a child from another tree is killed and
//! not started again. Everything after the greeting goes on as an
//! [`Event::Frame`], whole, through the sink the host handed [`Pipe::start`];
//! the bar's sink turns a samples body into the update that reaches binding 10
//! without the bar looking inside it.
//!
//! **Which sidecars.** A host has no config file (docs/architecture.md, *Configuration
//! is the caustic*), so its set is compiled in and handed to [`commands`]: the
//! bar's is `SIDECARS` in `bar/src/feeders/pipe.rs`. `$CAUSTICS_SIDECARS`
//! *replaces* that set with a whitespace-separated list of commands, which is
//! how a test harness runs a subset and how a new sidecar is tried without
//! installing it (the name is the bar's, kept for the drop-ins that set it).
//! Each name then resolves to the binary beside the host's own, and failing
//! that to the bare name on `PATH`. A sidecar's **name** — for its witnesses
//! and for [`Event::Absent`] — is the file stem of what was spawned.
//!
//! **And back the other way.** A sidecar's stdin is the bar's writer half: one
//! `WIRE_ACTION` frame per decision the caustic made about one of that
//! sidecar's rows (docs/abi.md, *Action*). [`Pipe::send`] is called from the
//! render thread, so it can never wait — the fd is put in non-blocking mode
//! when the child is spawned and a 44-byte frame is written in one syscall,
//! which a pipe delivers whole or not at all. A child that is not reading its
//! stdin costs one witness line and one dropped action, never a stalled bar.
//!
//! Everything else is process hygiene. A child is spawned with **no
//! arguments** (a sidecar's own settings are its own affair), with stdin and
//! stdout piped and **stderr inherited**, so its witnesses land in the bar's
//! journal beside the bar's own. It is restarted with a doubling backoff when
//! it exits. A binary that is not on `PATH` is one [`Event::Absent`] and no
//! further attempts: a missing binary does not appear without the unit being
//! restarted, and a restart loop on `NotFound` is a log flood, not a recovery.

use std::collections::HashSet;
use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use caustics_bar_abi::wire::{self, ActionBody, Frame};

/// The environment variable that *replaces* a host's compiled-in set: a
/// whitespace-separated list of commands, each resolved as below. For test
/// harnesses, and for trying a sidecar that is not installed. The name is the
/// bar's and predates this crate; the installed bar's drop-ins set it.
pub const SIDECARS_ENV: &str = "CAUSTICS_SIDECARS";

/// First wait after a child exits.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// The cap the doubling stops at.
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Most requests one pipe remembers for its children (each is a name of at
/// most `MAX_NAME_LEN` bytes); past this the oldest is forgotten, with one
/// witness, rather than the list growing with every window title ever seen.
const MAX_REQUESTS: usize = 4096;

/// How long `Drop` gives a child to notice its stdin closed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
/// A gap between two samples bodies' `t` larger than this is the sidecar
/// stalling. The bar's stall witness (`health.rs`) watches its own frames and
/// would never see a sidecar that stopped talking, so this is the one that
/// would.
const STALL: f32 = 5.0;

/// The commands to run: `$CAUSTICS_SIDECARS` split on whitespace if it names any,
/// else `defaults` (the host's compiled-in set), each resolved to the binary
/// **beside the host's own** and failing that to the bare name for `PATH` to
/// find.
///
/// The "beside the bar" step is what makes an installed pair work: a user
/// manager's default `PATH` does not include `~/.local/bin`, so a bar
/// installed there and started by systemd would not find a sidecar installed
/// beside it. The bar finding its own sidecars is the bar's business, not a
/// config's.
pub fn commands(defaults: &[&str]) -> Vec<String> {
    resolve(
        std::env::var_os(SIDECARS_ENV),
        std::env::current_exe().ok().as_deref(),
        defaults,
    )
}

/// The resolution, with both facts passed in — which is the whole of it, and
/// the only way to test the order without setting a process-wide variable
/// under every other test in the binary.
fn resolve(
    env: Option<std::ffi::OsString>,
    exe: Option<&std::path::Path>,
    defaults: &[&str],
) -> Vec<String> {
    let names: Vec<String> = match env.as_ref().map(|value| value.to_string_lossy()) {
        Some(value) if !value.split_whitespace().collect::<Vec<_>>().is_empty() => {
            value.split_whitespace().map(str::to_owned).collect()
        }
        _ => defaults.iter().map(|name| (*name).to_owned()).collect(),
    };
    let dir = exe.and_then(|exe| exe.parent());
    names
        .into_iter()
        .map(|name| {
            // A command with a path in it is already resolved; only a bare
            // name gets the "beside the bar" step.
            if name.contains('/') {
                return name;
            }
            match dir.map(|dir| dir.join(&name)).filter(|path| path.is_file()) {
                Some(path) => path.to_string_lossy().into_owned(),
                None => name,
            }
        })
        .collect()
}

/// A sidecar's name for witnesses, [`Event::Absent`] and the health line: the
/// file stem of what was spawned, so `target/debug/caustics-sysmon` and
/// `caustics-sysmon` are the same sidecar under the same name.
pub fn name_of(command: &str) -> String {
    std::path::Path::new(command)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| command.to_owned())
}

/// What a pipe tells its host, through the sink handed to [`Pipe::start`].
///
/// Three kinds and no vocabulary: the pipe knows a sidecar is missing, that
/// what it put on the host is no longer true, and that a frame arrived after
/// the greeting. What a frame *is* to the host (the bar's samples, rows and
/// answers) is the sink's to say.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The sidecar is not there, or not ours: not installed, an empty
    /// command line, or refused at its greeting (docs/abi.md, *Hello*). Sent
    /// once per name, and always followed by [`Event::Gone`].
    Absent(String),
    /// Whatever this sidecar had put on the host is not true any more: the
    /// one absence with state behind it.
    Gone(String),
    /// The child exited, or was killed, and the supervisor will start it
    /// again with backoff: sent on **every** exit, a refusal's included (where
    /// [`Event::Gone`] follows). What the sidecar *published* stays, as a
    /// restart's predecessor's rows do; what it was *in the middle of asking
    /// for* and only it could finish is over, which is how the canvas undoes
    /// a dim whose owner died (docs/canvas.md, *Failure model*).
    Exited(String),
    /// One frame from a greeted child, whole and unread: everything but a
    /// `Hello`, an `Action`, a `Request` and a skipped kind.
    Frame { sidecar: String, frame: Frame },
}

/// What a reader thread knows and the health line reports. Atomics, not a
/// lock: the render path reads these once an hour and the reader thread writes
/// them once a tick, and neither may ever wait on the other.
#[derive(Debug, Default)]
pub struct Stats {
    /// The live child's pid, or zero between one and the next.
    pid: AtomicU32,
    /// Frames read since the bar started, across every child.
    ticks: AtomicU64,
    /// Children after the first.
    restarts: AtomicU32,
    /// Frames `wire::read` consumed and did not understand — an unknown kind,
    /// a version skew, a body that failed its own validation. Each is one
    /// witness per kind per child, and this is the running total.
    skips: AtomicU64,
    /// The command could not be spawned at all; nothing will be retried.
    absent: AtomicBool,
    /// The child said it was built against another contract, or would not say
    /// at all (docs/abi.md, *Hello*). Nothing will be retried either — but
    /// this is a *wrong* sidecar rather than a missing one, and the health
    /// line says which.
    refused: AtomicBool,
}

impl Stats {
    /// The live child's pid, or `None` while there is none.
    pub fn pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::Relaxed) {
            0 => None,
            pid => Some(pid),
        }
    }

    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    pub fn restarts(&self) -> u32 {
        self.restarts.load(Ordering::Relaxed)
    }

    pub fn skips(&self) -> u64 {
        self.skips.load(Ordering::Relaxed)
    }

    /// The sidecar is not installed (or the command line was empty): one
    /// `Absent` was sent and nothing is trying any more.
    pub fn absent(&self) -> bool {
        self.absent.load(Ordering::Relaxed)
    }

    /// The sidecar is installed and is not ours (docs/abi.md, *Hello*). Nothing is trying any
    /// more either: a binary from another tree does not become this tree's
    /// while the bar runs, and a backoff would only replay the same refusal.
    pub fn refused(&self) -> bool {
        self.refused.load(Ordering::Relaxed)
    }
}

/// A running sidecar, or the thread that keeps trying to make one.
pub struct Pipe {
    /// Set by `Drop`; the reader thread checks it between frames and between
    /// backoff naps.
    stop: Arc<AtomicBool>,
    /// The live child, shared so `Drop` can kill it while the reader thread is
    /// parked in a read.
    child: Arc<Mutex<Option<Child>>>,
    /// The live child's stdin, taken out of the `Child` so that the render
    /// thread can write an action to it without touching the child at all.
    /// Dropping what is in here is how a sidecar is asked to leave: its own
    /// `wire::read` returns `Ok(None)`.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// Every `Request` frame this pipe has been asked to carry, serialised,
    /// deduplicated — written to the live child, and **to every child after
    /// it**. A request is a question about a name, idempotent by contract
    /// (docs/abi.md, *Request*), so a child that starts a millisecond after
    /// the bar's first build still gets the build's questions, and a child
    /// that restarts answers them again for the bar's tables. Bounded by
    /// [`MAX_REQUESTS`].
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    command: String,
    name: String,
    stats: Arc<Stats>,
}

impl std::fmt::Debug for Pipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipe")
            .field("name", &self.name)
            .field("command", &self.command)
            .finish()
    }
}

impl Pipe {
    /// Spawn `command` and start reading frames off its stdout.
    ///
    /// `abi_hash` is the contract a child must greet with (docs/abi.md,
    /// *Hello*): the host's own build's `ABI_HASH`. `sink` is called from the
    /// pipe's reader thread with every [`Event`], and returns `false` once the
    /// host has gone, which ends the thread.
    ///
    /// Never fails: an empty command line, a binary that is not there, a child
    /// that dies immediately — each is a witness line and, for the missing
    /// binary, one `Absent`. The host draws without that sidecar.
    pub fn start(
        command: &str,
        abi_hash: u64,
        sink: impl Fn(Event) -> bool + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let stdin: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));
        let requests: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(Stats::default());
        let name = name_of(command);
        let owned = command.to_owned();

        if owned.is_empty() {
            tracing::info!("absent: {name} (no command)");
            stats.absent.store(true, Ordering::Relaxed);
            sink(Event::Absent(name.clone()));
            sink(Event::Gone(name.clone()));
            return Self {
                stop,
                child,
                stdin,
                requests,
                command: owned,
                name,
                stats,
            };
        }

        let thread_command = owned.clone();
        let thread_name = name.clone();
        let thread_stop = Arc::clone(&stop);
        let thread_child = Arc::clone(&child);
        let thread_stdin = Arc::clone(&stdin);
        let thread_requests = Arc::clone(&requests);
        let thread_stats = Arc::clone(&stats);
        thread::Builder::new()
            .name(format!("sidecar-{name}"))
            .spawn(move || {
                supervise(
                    thread_command,
                    thread_name,
                    abi_hash,
                    sink,
                    thread_stop,
                    thread_child,
                    thread_stdin,
                    thread_requests,
                    thread_stats,
                )
            })
            .expect("spawning a sidecar's pipe thread");

        Self {
            stop,
            child,
            stdin,
            requests,
            command: owned,
            name,
            stats,
        }
    }

    /// The sidecar's name — the file stem of what was spawned.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the health line reports: the pid, the ticks, the restarts and the
    /// skips.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Write one `WIRE_ACTION` frame to the child's stdin (docs/abi.md
    /// *Action*). Returns whether the frame went.
    ///
    /// **This is called from the render thread**, so it may never wait. The
    /// fd is non-blocking (`spawn_child` sets it) and the frame is 44 bytes,
    /// which POSIX says a pipe writes whole or not at all — so the call is one
    /// syscall that either delivers the action or fails. Each way of failing
    /// is one witness and a dropped action: a child that is not reading its
    /// stdin (`WouldBlock`), a child that has gone (`BrokenPipe`), a sidecar
    /// between one child and the next (no stdin at all).
    ///
    /// The bar never waits for a reply. The sidecar's reply is its next `Rows`
    /// frame, which arrives on the reader thread like every other.
    pub fn send(&self, body: &ActionBody) -> bool {
        self.send_frame(&Frame::Action(*body), &format!("row {}", body.row))
    }

    /// Ask the sidecar a question it will be asked again (docs/abi.md
    /// *Request*): remembered for every child this pipe ever has, and written to
    /// the live one now if there is one. Unlike an action, a request to a
    /// child that is not there yet is **not dropped** — the bar's first build
    /// runs before its sidecars have a stdin (by a millisecond, which is
    /// enough to leave every name of the first build unanswered), and the answer is worth exactly as
    /// much a second later. Returns whether it was written or queued; `false` only for a
    /// frame that would not serialise.
    pub fn request(&self, frame_to_send: &Frame, what: &str) -> bool {
        let mut frame = Vec::with_capacity(wire::WireHeader::SIZE + 64);
        if wire::write(&mut frame, frame_to_send).is_err() {
            tracing::warn!("sidecar {}: a request frame would not serialise", self.name);
            return false;
        }
        let Ok(mut remembered) = self.requests.lock() else {
            tracing::warn!("sidecar {}: the requests lock is poisoned", self.name);
            return false;
        };
        if remembered.iter().any(|known| *known == frame) {
            // Asked before: the child that is live has it, or will get it
            // when the next one starts. Nothing to write twice.
            return true;
        }
        if remembered.len() >= MAX_REQUESTS {
            tracing::warn!(
                "sidecar {}: {MAX_REQUESTS} requests remembered; forgetting the oldest",
                self.name
            );
            remembered.remove(0);
        }
        remembered.push(frame.clone());
        drop(remembered);
        let Ok(mut slot) = self.stdin.lock() else {
            tracing::warn!("sidecar {}: the stdin lock is poisoned", self.name);
            return false;
        };
        match slot.as_mut() {
            Some(stdin) => write_whole(stdin, &frame, &self.name, what),
            None => {
                tracing::debug!(
                    "sidecar {}: no live child yet; {what} waits for the next one",
                    self.name
                );
                true
            }
        }
    }

    /// Write one frame of any kind a host sends — an `Action`, a `Request` (docs/abi.md, *Request*) or a `CanvasEvent` (*Canvas event*) — under exactly the rules `send`
    /// states: one non-blocking write, never a wait, one witness per way of
    /// failing. `what` names the frame in those witnesses.
    ///
    /// An `Answer` (docs/abi.md, *Answer*) goes through here too, and the
    /// copy this serialises it into is the one copy of a typed secret this
    /// side makes: it is reserved whole before the write, so a growing
    /// buffer never leaves a moved copy behind, and zeroed when it drops.
    pub fn send_frame(&self, frame_to_send: &Frame, what: &str) -> bool {
        let room = match frame_to_send {
            Frame::Answer(_) => wire::WireHeader::SIZE + 16 + wire::MAX_ANSWER_LEN,
            _ => wire::WireHeader::SIZE + 64,
        };
        let mut frame = zeroize::Zeroizing::new(Vec::with_capacity(room));
        if wire::write(&mut *frame, frame_to_send).is_err() {
            // A `Vec` cannot fail to be written to; this is here so that the
            // one path that could ever change never panics on the render
            // thread.
            tracing::warn!("sidecar {}: an action frame would not serialise", self.name);
            return false;
        }
        let Ok(mut slot) = self.stdin.lock() else {
            tracing::warn!("sidecar {}: the stdin lock is poisoned", self.name);
            return false;
        };
        let Some(stdin) = slot.as_mut() else {
            tracing::info!(
                "action dropped: sidecar {} has no live child to take {what}",
                self.name
            );
            return false;
        };
        write_whole(stdin, &frame, &self.name, what)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Closing the write end of stdin is how a well-behaved sidecar is
        // asked to leave: `wire::read` on its side returns `Ok(None)`. Kill
        // only what will not. The stdin handle lives in its own slot, apart from the child, so it is dropped here on its own line.
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
        let Ok(mut slot) = self.child.lock() else {
            return;
        };
        let Some(mut child) = slot.take() else { return };
        let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(_) => break,
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// One non-blocking write of one frame to a child's stdin: whole, or one
/// witness and `false`. Under `PIPE_BUF` a pipe writes whole or not at all;
/// if it ever does not, the child's stream is desynchronised and the honest
/// thing is to say so rather than to write the rest and hope.
fn write_whole(stdin: &mut ChildStdin, frame: &[u8], name: &str, what: &str) -> bool {
    match stdin.write(frame) {
        Ok(n) if n == frame.len() => true,
        Ok(n) => {
            tracing::warn!(
                "sidecar {name}: wrote {n} of {} bytes of {what}; the child will restart",
                frame.len()
            );
            false
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            tracing::info!("dropped: sidecar {name} is not reading its stdin ({what})");
            false
        }
        Err(err) => {
            tracing::info!("dropped: sidecar {name}: {err} ({what})");
            false
        }
    }
}

/// Spawn, read, mourn, wait, repeat.
#[allow(clippy::too_many_arguments)]
fn supervise(
    command: String,
    name: String,
    abi_hash: u64,
    sink: impl Fn(Event) -> bool,
    stop: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Child>>>,
    stdin_slot: Arc<Mutex<Option<ChildStdin>>>,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    stats: Arc<Stats>,
) {
    let mut backoff = BACKOFF_START;
    let mut started = 0u32;

    while !stop.load(Ordering::Relaxed) {
        let spawned = Command::new(&command)
            // No arguments, ever: which charts a sampler samples, which sink
            // an audio applet watches, are the sidecar's own affair
            // (docs/abi.md, *Lifecycle*).
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The journal: the sidecar's witnesses are the bar's witnesses.
            .stderr(Stdio::inherit())
            .spawn();

        let mut child = match spawned {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // A binary that is not installed does not become installed
                // while the bar runs: say so once and stop.
                tracing::info!("absent: {name} ({command} not found)");
                stats.absent.store(true, Ordering::Relaxed);
                sink(Event::Absent(name.clone()));
                // The one absence with state behind it: whatever this sidecar
                // had put on the host is not true any more.
                sink(Event::Gone(name.clone()));
                return;
            }
            Err(err) => {
                tracing::warn!(
                    "sidecar {name}: spawning {command} failed: {err}; retry in {} s",
                    backoff.as_secs()
                );
                nap(&stop, backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };

        let pid = child.id();
        tracing::info!("sidecar {name}: started pid={pid}");
        stats.pid.store(pid, Ordering::Relaxed);
        if started > 0 {
            stats.restarts.fetch_add(1, Ordering::Relaxed);
        }
        started += 1;
        let Some(stdout) = child.stdout.take() else {
            stats.pid.store(0, Ordering::Relaxed);
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!("sidecar {name}: no stdout on pid={pid}");
            nap(&stop, backoff);
            backoff = (backoff * 2).min(BACKOFF_MAX);
            continue;
        };

        // Stdin comes out of the child and into its own slot, non-blocking:
        // the render thread writes actions there (`Pipe::send`) and must never
        // wait on a sidecar that is not reading them.
        if let Some(mut stdin) = child.stdin.take() {
            match set_nonblocking(&stdin) {
                Ok(()) => {
                    // Every question the bar has asked so far goes to this
                    // child before anything else does: the ones asked before
                    // it existed, and after a restart the ones its predecessor
                    // answered — whose answers named glyph ids and slots this
                    // child knows nothing about.
                    if let Ok(remembered) = requests.lock() {
                        let mut written = 0usize;
                        for frame in remembered.iter() {
                            if write_whole(&mut stdin, frame, &name, "a remembered request") {
                                written += 1;
                            }
                        }
                        if written > 0 {
                            tracing::info!(
                                "sidecar {name}: {written} remembered request(s) sent to pid={pid}"
                            );
                        }
                    }
                    if let Ok(mut guard) = stdin_slot.lock() {
                        *guard = Some(stdin);
                    }
                }
                // Without O_NONBLOCK a full pipe would park the render thread,
                // so the handle is dropped instead: the sidecar sees EOF on
                // its stdin and leaves, and the backoff restarts it.
                Err(err) => tracing::warn!(
                    "sidecar {name}: stdin would not go non-blocking ({err}); \
                     no actions can be sent to pid={pid}"
                ),
            }
        }

        // The child lives in the shared slot from here, so `Drop` can reach it
        // while this thread is parked in a read.
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(child);
        }

        let ended = pump(stdout, &name, abi_hash, &sink, &stop, &stats);
        let read_any = ended.read_any;
        stats.pid.store(0, Ordering::Relaxed);
        // The child is going: its stdin closes here, which is also what tells
        // `Pipe::send` there is nobody to write to.
        if let Ok(mut guard) = stdin_slot.lock() {
            guard.take();
        }
        sink(Event::Exited(name.clone()));

        // A sidecar from another tree (docs/abi.md, *Hello*). **Killed, not asked**: it has
        // already written a frame this bar will not read, and whether it
        // notices its stdin closing is its own tree's business. Not a crash
        // either — the binary is installed, it is simply not this bar's, and a
        // backoff would replay the same refusal every second for the life of
        // the session. Its rows leave the bar the way an absent sidecar's do,
        // and this thread ends.
        if ended.refused {
            if let Some(mut child) = slot.lock().ok().and_then(|mut guard| guard.take()) {
                let _ = child.kill();
                let _ = child.wait();
            }
            stats.refused.store(true, Ordering::Relaxed);
            sink(Event::Absent(name.clone()));
            sink(Event::Gone(name.clone()));
            tracing::warn!("sidecar {name}: killed, and not started again");
            return;
        }

        // Reap, and say how it went. `Drop` may have taken the child already.
        let status = match slot.lock().ok().and_then(|mut g| g.take()) {
            Some(mut child) => child.wait().ok(),
            None => None,
        };
        if stop.load(Ordering::Relaxed) {
            return;
        }

        // A child that actually delivered frames earned a fresh first wait:
        // this is a restart, not a crash loop.
        if read_any {
            backoff = BACKOFF_START;
        }
        let status = match status {
            Some(status) => status.to_string(),
            None => "unknown".to_owned(),
        };
        tracing::warn!(
            "sidecar {name}: exited {status}; restart in {} s",
            backoff.as_secs()
        );
        nap(&stop, backoff);
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// How a child's stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ended {
    /// A valid frame arrived at some point, which is what tells a restart from
    /// a crash loop.
    read_any: bool,
    /// The child is not this bar's (docs/abi.md, *Hello*): it greeted with another contract's
    /// hash, or wrote a frame before it greeted at all. **No restart.**
    refused: bool,
}

/// Read frames until the child stops producing them.
fn pump(
    stdout: std::process::ChildStdout,
    name: &str,
    abi_hash: u64,
    sink: &impl Fn(Event) -> bool,
    stop: &AtomicBool,
    stats: &Stats,
) -> Ended {
    // Buffered: a wire frame is a header read and a body read, and an unbuffered
    // pipe would make that two syscalls per frame per sidecar. A large body
    // (a picture is tens of mebibytes) is read in `wire::WIRE_READ_STEP`
    // steps larger than this buffer, so once the bytes it already holds are
    // spent `BufReader` passes each step to the pipe directly; the read blocks
    // on this sidecar's own thread and nothing else waits for it.
    let mut input = BufReader::new(stdout);
    let mut shape: Option<u32> = None;
    let mut rows_shape: Option<(u32, u32, u32)> = None;
    // docs/abi.md, *Framing*: one witness per skip *kind* per child, so a sidecar a version
    // ahead costs one journal line and not one a tick.
    let mut witnessed: HashSet<(u32, u32)> = HashSet::new();
    let mut previous_t: Option<f32> = None;
    let mut any = false;
    // **Nothing is read from this child until it says what it is** (docs/abi.md, *Hello*).
    // The one decision here is not this file's: the
    // hashes are the contract's, and all this does is compare two numbers it
    // was handed and translate the answer into "this child is not ours".
    let mut greeted = false;
    let refused = |read_any: bool| Ended {
        read_any,
        refused: true,
    };

    while !stop.load(Ordering::Relaxed) {
        let frame = match wire::read(&mut input) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                // EOF on a frame boundary: the child is gone. The caller reaps
                // it and says why.
                return Ended {
                    read_any: any,
                    refused: false,
                };
            }
            Err(err) => {
                // A bad magic, an impossible length, an EOF mid-frame. The
                // stream is not one we can resynchronise, so the child goes.
                tracing::warn!("sidecar {name}: {err}; restarting");
                return Ended {
                    read_any: any,
                    refused: false,
                };
            }
        };

        // *Hello*'s gate, before anything is looked at. A `Hello` is the only
        // frame a child may write first; anything else — including a `Hello`
        // this bar cannot match — ends the child for good.
        if !greeted {
            let Frame::Hello(hello) = &frame else {
                tracing::error!(
                    "sidecar {name}: a kind {} frame before it said what it is; \
                     refusing it (docs/abi.md, *Hello*)",
                    frame.kind()
                );
                return refused(any);
            };
            if hello.abi_hash != abi_hash {
                tracing::error!(
                    "sidecar {name}: refused — it calls itself {:?} and was built against ABI \
                     {:016x}; this bar is {:016x}. Install the set, do not mix trees \
                     (docs/architecture.md, *The ABI*).",
                    hello.name,
                    hello.abi_hash,
                    abi_hash
                );
                return refused(any);
            }
            tracing::info!(
                "sidecar {name}: hello from {:?}, ABI {:016x}",
                hello.name,
                hello.abi_hash
            );
            greeted = true;
            continue;
        }

        match frame {
            Frame::Hello(hello) => {
                // A second one. Harmless, and worth exactly one line: a
                // sidecar that re-greets has not restarted, it has repeated
                // itself.
                if witnessed.insert((wire::WIRE_HELLO, 0)) {
                    tracing::warn!("sidecar {name}: greeted twice ({:?}); ignoring", hello.name);
                }
                stats.skips.fetch_add(1, Ordering::Relaxed);
            }
            Frame::Samples(samples) => {
                // One line on the first body and whenever the shape changes —
                // the sidecar's chart list is its own, and this is how the
                // journal shows what it settled on.
                let now_shape = samples.chart_count;
                if shape != Some(now_shape) {
                    tracing::info!("sidecar {name}: samples, {now_shape} charts");
                    shape = Some(now_shape);
                }

                if let Some(previous) = previous_t {
                    let gap = samples.t - previous;
                    if gap > STALL {
                        tracing::warn!("sidecar {name}: stalled {gap:.1} s");
                    }
                }
                previous_t = Some(samples.t);

                any = true;
                stats.ticks.fetch_add(1, Ordering::Relaxed);
                if !sink(Event::Frame {
                    sidecar: name.to_owned(),
                    frame: Frame::Samples(samples),
                }) {
                    // The bar is gone; so is the reason to keep reading.
                    return Ended {
                        read_any: any,
                        refused: false,
                    };
                }
            }
            Frame::Rows(rows) => {
                // Published whole; `bar_build` makes them one chip, its
                // menu rows and its sliders. The witness proves the wire end
                // to end without looking at the caustic.
                let now = (rows.applet, rows.rows.len() as u32, rows.revision);
                if rows_shape.map(|(a, n, _)| (a, n)) != Some((now.0, now.1)) {
                    tracing::info!(
                        "sidecar {name}: rows, applet={} rows={} rev={}",
                        now.0,
                        now.1,
                        now.2
                    );
                }
                rows_shape = Some(now);
                any = true;
                stats.ticks.fetch_add(1, Ordering::Relaxed);
                if !sink(Event::Frame {
                    sidecar: name.to_owned(),
                    frame: Frame::Rows(rows),
                }) {
                    return Ended {
                        read_any: any,
                        refused: false,
                    };
                }
            }
            Frame::Action(_) | Frame::Request(_) | Frame::CanvasEvent(_) | Frame::Answer(_) => {
                // A host writes actions, requests, canvas events and answers;
                // a sidecar writing one back is a confused sidecar, and the
                // frame is dropped like any other kind it has no use for.
                if witnessed.insert((frame.kind(), 1)) {
                    tracing::warn!("sidecar {name}: a bar-to-sidecar frame on stdout; ignoring");
                }
                stats.skips.fetch_add(1, Ordering::Relaxed);
            }
            Frame::Bytes(_)
            | Frame::Font(_)
            | Frame::Glyph(_)
            | Frame::App(_)
            | Frame::Pixmap(_)
            | Frame::Module(_)
            | Frame::Image(_)
            | Frame::Canvas(_)
            | Frame::Prompt(_) => {
                // The answer kinds (docs/abi.md, *Bytes*–*Module*): a sidecar
                // that turns a name into something drawable, or a `.slang`
                // file into a module; and the canvas's three (*Image*,
                // *Canvas*, *Prompt*). This feeder does not look inside: the event
                // carries the frame whole, and the host copies its bytes
                // where they go, which for SPIR-V is the device.
                any = true;
                stats.ticks.fetch_add(1, Ordering::Relaxed);
                if !sink(Event::Frame {
                    sidecar: name.to_owned(),
                    frame,
                }) {
                    return Ended {
                        read_any: any,
                        refused: false,
                    };
                }
            }
            Frame::Skipped(skipped) => {
                stats.skips.fetch_add(1, Ordering::Relaxed);
                if witnessed.insert((skipped.kind, 0)) {
                    tracing::warn!("sidecar {name}: skipped a frame — {skipped}");
                }
            }
        }
    }
    Ended {
        read_any: any,
        refused: false,
    }
}

/// Put a child's stdin in non-blocking mode, so a write from the render thread
/// is one syscall that either delivers the whole frame or says `WouldBlock`.
fn set_nonblocking(stdin: &ChildStdin) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let fd = stdin.as_raw_fd();
    // SAFETY: `fd` is owned by `stdin`, which outlives both calls, and
    // `fcntl` with these two commands touches nothing else.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Sleep, but wake for `Drop`.
fn nap(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(50);
    let mut left = total;
    while left > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let slice = step.min(left);
        thread::sleep(slice);
        left -= slice;
    }
}

#[cfg(test)]
mod tests;
