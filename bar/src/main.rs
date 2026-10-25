//! caustics-bar — a status bar drawn by a Slang caustic, running as a layer-shell
//! client (docs/architecture.md).
//!
//! The process follows the split of a GPU-resident compositor (docs/architecture.md, *Why it is shaped this way*): a small CPU embassy that
//! owns the Wayland objects and the GPU resources, and a Slang caustic that
//! owns every decision about what the bar looks like and how it behaves. The
//! embassy translates pointer events into commands, hands the caustic a frame,
//! and reads back what the caustic decided — where the elements are, whether a
//! popup should exist and of which kind, and which actions to carry out.
//!
//! Frame discipline (docs/architecture.md, *The frame*): a frame happens when a
//! command was queued, the content changed, a surface was configured, or the
//! previous mirror said `wants_redraw`. Otherwise the process sleeps in
//! calloop. There is no vblank loop.
//!
//! Exit discipline: a Wayland dispatch failure — a protocol error
//! included — is logged verbatim and the process exits 1, so the user unit
//! restarts it rather than leaving a half-dead bar on screen. Exit 2 is the
//! *refusal* code — bad arguments, a dGPU-only adapter, or another caustics-bar
//! already holding this edge — and the unit's `RestartPreventExitStatus=2`
//! is what stops a refusal from becoming a restart loop.

mod abi;
mod clock;
mod facts;
mod favourites;
mod feeders;
mod gpu;
mod health;
mod model;
/// The oracle: the caustic's model passes run headless on the machine's own GPU. Test-only, and
/// gated on `CAUSTICS_BAR_ORACLE=1` inside.
#[cfg(test)]
mod oracle;
mod strings;
mod wayland;

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use smithay_client_toolkit::reexports::calloop::{
    EventLoop,
    channel::{Event as ChannelEvent, channel},
    timer::{TimeoutAction, Timer},
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use wayland_client::{Connection, globals::registry_queue_init};

use model::Update;
use wayland::{Bar, Edge, Exit};

/// Exit codes. 0 is a clean stop, 1 is a protocol error or a fatal dispatch
/// failure (the unit restarts), 2 is a refusal to run at all — the dGPU guard
/// in `caustics_host::device`, bad arguments, or an edge another caustics-bar
/// already holds — and 3 is the device gone, which the device's own callbacks
/// in `caustics_host::device` exit with.
///
/// The unit's `RestartPreventExitStatus=2 3` makes both of the last two final.
/// A lost device is not a transient fault: whatever produced it is in the
/// buffers a sidecar will replay to the next bar the moment it starts, so a
/// restart is a loop and one journal line is the honest answer.
const EXIT_OK: i32 = 0;
const EXIT_PROTOCOL: i32 = 1;
const EXIT_REFUSE: i32 = 2;
// Not referenced here: the callbacks that exit with it are the crate's, and
// this line keeps all four codes in one list.
#[allow(dead_code)]
const EXIT_DEVICE: i32 = caustics_host::device::EXIT_DEVICE;

/// How long the bar waits for its first configure before giving up. Generous:
/// a compositor that has not answered in ten seconds is not going to.
const FIRST_CONFIGURE_TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        // Under the unit this is the journal, and `journalctl` keeps escape
        // sequences verbatim — which turns every `field=value` the test
        // harness greps for into `field<ESC>[0m=<ESC>[0m value`. Colour only when
        // stderr is a terminal.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    let max_runtime = match parse_args() {
        Ok(Args::Run { max_runtime }) => max_runtime,
        Ok(Args::AbiHash) => {
            // Read by `bar/install.sh`, which names the set's directory after
            // it. Stdout, one line, no logging: it is a
            // number a shell reads, not a witness.
            println!("{:016x}", abi::ABI_HASH);
            std::process::exit(EXIT_OK);
        }
        Err(err) => {
            tracing::error!("{err:#}");
            std::process::exit(EXIT_REFUSE);
        }
    };

    // **Which set this is** (docs/architecture.md, *The ABI*). `current_exe` is the resolved
    // path, so a bar started through `~/.local/bin/caustics-bar` says the
    // `~/.local/lib/caustics-bar/<hash>/` it really came from — and that directory
    // is where `feeders::pipe` will look for its sidecars, which is what makes
    // a set a set rather than binaries that happen to be beside each other. The hash is the same number the directory is named after.
    tracing::info!(
        "set: {} (ABI {:016x})",
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.display().to_string()))
            .unwrap_or_else(|| "unknown".to_owned()),
        abi::ABI_HASH,
    );

    // The default edge's lock, taken before the Wayland connection because
    // the connection needs an anchor before any frame exists and refusing to
    // be the second bar on that edge is cheaper than connecting first. `Bar` owns it
    // from here on — the `EDGE` tunable (caustics/bar_tunables.slang), which can move the bar to the other edge, has
    // to move the lock with it.
    let Some(lock) = take_edge_lock(wayland::BAR_EDGE) else {
        std::process::exit(EXIT_REFUSE);
    };

    let code = match run(max_runtime, lock) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!("exit: {err:#}");
            EXIT_PROTOCOL
        }
    };
    std::process::exit(code);
}

/// Refuse to be the second bar on an edge.
///
/// Two bars on the same edge is two exclusive zones and two identical
/// strips, and under `Restart=always` the loser would respawn forever. An
/// advisory `flock` on a file in `$XDG_RUNTIME_DIR` is the cheapest check that
/// is also correct across a crash: the kernel drops the lock when the process
/// dies, however it dies, so a killed bar leaves nothing to clean up.
pub fn take_edge_lock(edge: Edge) -> Option<File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("caustics-bar-{}.lock", edge.name()));
    let file = match File::create(&path) {
        Ok(file) => file,
        Err(err) => {
            // An unwritable runtime dir is not a reason to refuse to run; it
            // is a reason to say so and carry on unguarded.
            tracing::warn!(
                "could not open {}: {err}; running without the edge lock",
                path.display()
            );
            return Some(File::open("/dev/null").ok()?);
        }
    };
    // SAFETY: `file` owns the descriptor for the whole call.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if locked {
        return Some(file);
    }
    tracing::error!(
        "another caustics-bar holds the {} edge; exiting ({})",
        edge.name(),
        path.display()
    );
    None
}

/// What the command line asked for. Two things, and the bar still has no
/// configuration: one is a deadline for unattended runs, the other prints a
/// number and leaves.
enum Args {
    Run { max_runtime: Option<Duration> },
    AbiHash,
}

/// `--max-runtime SECS` exits 0 on a deadline, for unattended runs;
/// `--abi-hash` prints this build's `ABI_HASH` and exits, which is how
/// `bar/install.sh` learns the set's directory name.
/// Nothing else is accepted.
fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut max_runtime = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--abi-hash" => return Ok(Args::AbiHash),
            "--max-runtime" => {
                let secs: u64 = args
                    .next()
                    .context("--max-runtime needs a value in seconds")?
                    .parse()
                    .context("--max-runtime takes a whole number of seconds")?;
                max_runtime = Some(Duration::from_secs(secs));
            }
            other => anyhow::bail!(
                "unknown argument {other:?}; caustics-bar takes --max-runtime SECS or --abi-hash"
            ),
        }
    }
    Ok(Args::Run { max_runtime })
}

fn run(max_runtime: Option<Duration>, edge_lock: File) -> Result<i32> {
    let conn = Connection::connect_to_env().context("could not connect to a Wayland display")?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).context("the Wayland registry could not be enumerated")?;
    let qh = event_queue.handle();

    // One channel, every feeder. The sending end goes to the Wayland feeders,
    // the compositor seam and the sidecars inside `Bar::new`; the receiving
    // end becomes a calloop source below, and `Bar::apply_update` is the only
    // thing that touches the model (model.rs, "the two typed streams").
    let (updates, incoming) = channel::<Update>();

    // The lock moves into `Bar`, which holds it for the life of the process
    // and swaps it if the caustic asks for the other edge. Dropping the file
    // closes the fd, which releases the flock, so it must outlive the bar.
    let mut bar = Bar::new(conn.clone(), &globals, qh, Some(edge_lock), updates.clone())?;
    // The clock is an update like any other, on the same path as everything
    // else the bar knows.
    bar.tick_clock();

    // Commit, wait for the first configure, *then* create the wgpu surface —
    // and do it here, on a plain blocking dispatch, before calloop owns the
    // queue.
    //
    // Not merely tidiness: Mesa's Wayland WSI runs a
    // `wl_display_roundtrip` of its own inside
    // `vkGetPhysicalDeviceSurfaceSupportKHR`, and that roundtrip cannot make
    // progress while `calloop-wayland-source` is holding the connection's
    // read guard. Bringing the swapchain up while calloop drives the queue
    // deadlocks the process in `poll()` with one thread and no diagnostic.
    // Everything after this point only *resizes* that swapchain, which does not roundtrip.
    let start = std::time::Instant::now();
    while bar.awaiting_first_surface() {
        if start.elapsed() > FIRST_CONFIGURE_TIMEOUT {
            anyhow::bail!(
                "no layer-surface configure in {} s; does this compositor implement \
                 zwlr_layer_shell_v1?",
                FIRST_CONFIGURE_TIMEOUT.as_secs()
            );
        }
        event_queue
            .blocking_dispatch(&mut bar)
            .context("the Wayland connection failed before the first configure")?;
        bar.service_surfaces()?;
    }

    let mut event_loop: EventLoop<'static, Bar> =
        EventLoop::try_new().context("could not create the calloop event loop")?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .map_err(|err| anyhow::anyhow!("could not insert the Wayland source: {err}"))?;

    // Everything the feeders send, applied on the loop's own thread.
    handle
        .insert_source(incoming, |event, _, bar: &mut Bar| {
            if let ChannelEvent::Msg(update) = event {
                bar.apply_update(update);
            }
        })
        .map_err(|err| anyhow::anyhow!("could not insert the update channel: {err}"))?;

    // The capture feeder's timer is the bar's own, and it needs the loop to
    // arm itself on (`Bar::arm_capture`).
    bar.set_loop_handle(handle.clone());

    // The clock: one timer, rescheduled to the next minute boundary, rather
    // than a poll. The bar sleeps between minutes.
    handle
        .insert_source(
            Timer::from_duration(until_next_minute()),
            move |_, _, bar: &mut Bar| {
                bar.tick_clock();
                TimeoutAction::ToDuration(until_next_minute())
            },
        )
        .map_err(|err| anyhow::anyhow!("could not insert the clock timer: {err}"))?;

    // The health line (health.rs): frames, CPU, memory, the atlas, the capture
    // session, the popup, and every sidecar's state. What a slow leak or a busy
    // loop leaves in the journal for the morning after.
    //
    // The first one comes early, because it is also the
    // line that says what the install set that just started actually did —
    // ten seconds in, every sidecar has had its first second. The same timer
    // does both: no second source, one `why` that differs on the first fire.
    let mut first_report = true;
    handle
        .insert_source(
            Timer::from_duration(health::STARTUP_REPORT),
            move |_, _, bar: &mut Bar| {
                bar.health_report(if first_report { "startup" } else { "hourly" });
                first_report = false;
                TimeoutAction::ToDuration(health::REPORT_INTERVAL)
            },
        )
        .map_err(|err| anyhow::anyhow!("could not insert the health timer: {err}"))?;

    if let Some(deadline) = max_runtime {
        tracing::info!(seconds = deadline.as_secs(), "running with a deadline");
        handle
            .insert_source(Timer::from_duration(deadline), |_, _, bar: &mut Bar| {
                bar.exit = Some(Exit::Deadline);
                TimeoutAction::Drop
            })
            .map_err(|err| anyhow::anyhow!("could not insert the deadline timer: {err}"))?;
    }

    tracing::info!(
        namespace = "caustics-bar",
        height = wayland::BAR_HEIGHT,
        "starting; waiting for the first layer-surface configure"
    );

    loop {
        // No timeout: every wakeup is an event, a timer or a feeder's update.
        // Nothing in this process polls — the mtime watch under live reload
        // belongs to `caustics-reload`, which runs the compiler, and
        // a swapped caustic arrives on the update channel like a font does.
        if let Err(err) = event_loop.dispatch(None, &mut bar) {
            // Verbatim, at error level, and then out: the unit restarts the bar.
            log_protocol_error(&conn);
            tracing::error!("wayland dispatch failed: {err}");
            return Ok(EXIT_PROTOCOL);
        }

        // A protocol error does not reliably surface as a dispatch error:
        // once the compositor has closed the connection, the read guard's
        // `read()` fails with a *protocol* error, which calloop-wayland-source
        // 0.3 only stores when it is an I/O error, and the loop then polls an
        // EOF socket forever, printing the error each pass (37k journal lines
        // in six seconds, measured). Asking the
        // connection directly after every dispatch is the reliable path.
        if conn.protocol_error().is_some() {
            log_protocol_error(&conn);
            tracing::error!("wayland dispatch failed: the compositor closed the connection");
            return Ok(EXIT_PROTOCOL);
        }

        if let Some(exit) = bar.exit.clone() {
            return Ok(finish(&bar, exit));
        }

        if let Err(err) = bar.after_dispatch() {
            tracing::error!("frame failed: {err:#}");
            return Ok(EXIT_PROTOCOL);
        }

        if let Some(exit) = bar.exit.clone() {
            return Ok(finish(&bar, exit));
        }
    }
}

/// A protocol error carries its object and its message on the connection,
/// which is the detail worth having in the journal. `bar-test.sh` greps for
/// the phrase "wayland protocol error".
fn log_protocol_error(conn: &Connection) {
    // A compositor answering a fault sends wl_display.error and closes the
    // socket at once. A flush can then fail with EPIPE before that event has
    // been read, and the dispatch error is an I/O one with the real cause
    // still in the socket's buffer: on a slow adapter the order flips every
    // time. One read picks the event up, and the connection records it.
    if conn.protocol_error().is_none() {
        if let Some(guard) = conn.prepare_read() {
            let _ = guard.read();
        }
    }
    if let Some(protocol) = conn.protocol_error() {
        tracing::error!(
            object_id = protocol.object_id,
            object_interface = %protocol.object_interface,
            code = protocol.code,
            "wayland protocol error: {}",
            protocol.message
        );
    }
}

fn finish(bar: &Bar, exit: Exit) -> i32 {
    match exit {
        Exit::Deadline => {
            tracing::info!(frames = bar.frames(), "exit: deadline reached");
            EXIT_OK
        }
        Exit::Closed => {
            tracing::info!(frames = bar.frames(), "exit: the compositor closed the bar");
            EXIT_OK
        }
        Exit::Protocol(reason) => {
            tracing::error!(frames = bar.frames(), "exit: {reason}");
            EXIT_PROTOCOL
        }
    }
}

/// How long until the wall clock rolls over to the next minute.
fn until_next_minute() -> Duration {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Duration::from_secs(60 - (secs % 60))
}
