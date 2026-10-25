//! caustics-canvas — the canvas host: the wallpaper on every output, drawn by
//! a Slang caustic and cross-faded on command (docs/canvas.md).
//!
//! The bar process's shape, for a second caustic: a small process that owns
//! the Wayland objects (one background layer surface per output, one overlay
//! per output while a dim is up, one prompt pane per output while a prompt
//! is), the seat's input, the device, its sidecars' pipes and the one buffer
//! a typed answer passes through, and decides nothing. `caustics-wallpaper` sends the
//! pictures and the commands, `caustics-reload` the caustic as it is saved,
//! and the caustic says how far a fade or a dim is and whether it wants
//! another frame. The mechanism is `caustics-host`'s, which the bar runs on
//! too.
//!
//! Exit codes, as the bar's: 0 a clean stop, 1 a protocol error or a fatal
//! dispatch or frame failure, 2 a refusal (a bad argument, a discrete GPU),
//! 3 the device gone (the device's own callbacks in `caustics_host::device`).
//! The canvas's unit restarts on all of them (canvas/README.md).

mod clock;
mod dim;
mod gpu;
mod keys;
mod prompt;
mod role;
mod sidecars;
mod slots;
mod wayland;

use std::time::Duration;

use anyhow::{Context, Result};
use smithay_client_toolkit::reexports::calloop::{
    EventLoop,
    channel::{Event as ChannelEvent, channel},
    timer::{TimeoutAction, Timer},
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use wayland_client::{Connection, globals::registry_queue_init};

use caustics_host::pipe::Event;
use wayland::{Canvas, Exit};

const EXIT_OK: i32 = 0;
const EXIT_PROTOCOL: i32 = 1;
const EXIT_REFUSE: i32 = 2;
// The device's callbacks exit with it; this line keeps the four in one list.
#[allow(dead_code)]
const EXIT_DEVICE: i32 = caustics_host::device::EXIT_DEVICE;

/// How long an output's surface may wait for its first configure.
const FIRST_CONFIGURE_TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    let max_runtime = match parse_args() {
        Ok(max_runtime) => max_runtime,
        Err(err) => {
            tracing::error!("{err:#}");
            std::process::exit(EXIT_REFUSE);
        }
    };
    // Which set this is, and which process: the harness checks a sidecar's
    // parent against this pid before it kills one.
    tracing::info!(
        "set: {} (ABI {:016x}), pid {}",
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.display().to_string()))
            .unwrap_or_else(|| "unknown".to_owned()),
        caustics_bar_abi::ABI_HASH,
        std::process::id()
    );
    let code = match run(max_runtime) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!("exit: {err:#}");
            EXIT_PROTOCOL
        }
    };
    std::process::exit(code);
}

/// `--max-runtime SECS` exits 0 on a deadline, for unattended runs. Nothing
/// else is accepted: the canvas has no configuration but its caustic's.
fn parse_args() -> Result<Option<Duration>> {
    let mut args = std::env::args().skip(1);
    let mut max_runtime = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--max-runtime" => {
                let secs: u64 = args
                    .next()
                    .context("--max-runtime needs a value in seconds")?
                    .parse()
                    .context("--max-runtime takes a whole number of seconds")?;
                max_runtime = Some(Duration::from_secs(secs));
            }
            other => anyhow::bail!(
                "unknown argument {other:?}; caustics-canvas takes --max-runtime SECS or nothing"
            ),
        }
    }
    Ok(max_runtime)
}

fn run(max_runtime: Option<Duration>) -> Result<i32> {
    let conn = Connection::connect_to_env().context("could not connect to a Wayland display")?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).context("the Wayland registry could not be enumerated")?;
    let qh = event_queue.handle();

    // Every sidecar's events, applied on the loop's own thread.
    let (events, incoming) = channel::<Event>();
    let mut canvas = Canvas::new(conn.clone(), &globals, qh, &events)?;

    // The first output's configure brings the device up, on a plain blocking
    // dispatch before calloop owns the queue, as the bar's does (bar/src/main.rs
    // says why). With no output yet this waits for one.
    let mut first_surface: Option<std::time::Instant> = None;
    while canvas.awaiting_device() {
        if canvas.has_screens() {
            let since = *first_surface.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() > FIRST_CONFIGURE_TIMEOUT {
                anyhow::bail!(
                    "no layer-surface configure in {} s; does this compositor implement \
                     zwlr_layer_shell_v1?",
                    FIRST_CONFIGURE_TIMEOUT.as_secs()
                );
            }
        }
        event_queue
            .blocking_dispatch(&mut canvas)
            .context("the Wayland connection failed before the first configure")?;
        canvas.service()?;
    }
    if let Some(exit) = canvas.exit.clone() {
        return Ok(finish(&canvas, exit));
    }

    let mut event_loop: EventLoop<'static, Canvas> =
        EventLoop::try_new().context("could not create the calloop event loop")?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .map_err(|err| anyhow::anyhow!("could not insert the Wayland source: {err}"))?;
    handle
        .insert_source(incoming, |event, _, canvas: &mut Canvas| {
            if let ChannelEvent::Msg(event) = event {
                canvas.apply_event(event);
            }
        })
        .map_err(|err| anyhow::anyhow!("could not insert the sidecar channel: {err}"))?;
    canvas.set_loop_handle(handle.clone());
    if let Some(deadline) = max_runtime {
        handle
            .insert_source(
                Timer::from_duration(deadline),
                |_, _, canvas: &mut Canvas| {
                    canvas.exit = Some(Exit::Deadline);
                    TimeoutAction::Drop
                },
            )
            .map_err(|err| anyhow::anyhow!("could not insert the deadline timer: {err}"))?;
    }

    // The first frame, before anything has happened to ask for one: the dim
    // colour, until a picture arrives.
    if let Err(err) = canvas.after_dispatch() {
        tracing::error!("frame failed: {err:#}");
        return Ok(EXIT_PROTOCOL);
    }
    loop {
        if let Err(err) = event_loop.dispatch(None, &mut canvas) {
            log_protocol_error(&conn);
            tracing::error!("wayland dispatch failed: {err}");
            return Ok(EXIT_PROTOCOL);
        }
        if conn.protocol_error().is_some() {
            log_protocol_error(&conn);
            tracing::error!("wayland dispatch failed: the compositor closed the connection");
            return Ok(EXIT_PROTOCOL);
        }
        if let Err(err) = canvas.after_dispatch() {
            tracing::error!("frame failed: {err:#}");
            return Ok(EXIT_PROTOCOL);
        }
        if let Some(exit) = canvas.exit.clone() {
            return Ok(finish(&canvas, exit));
        }
    }
}

fn log_protocol_error(conn: &Connection) {
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

fn finish(canvas: &Canvas, exit: Exit) -> i32 {
    match exit {
        Exit::Deadline => {
            tracing::info!(frames = canvas.frames(), "exit: deadline reached");
            EXIT_OK
        }
        Exit::Fault(reason) => {
            tracing::error!(frames = canvas.frames(), "exit: {reason}");
            EXIT_PROTOCOL
        }
    }
}
