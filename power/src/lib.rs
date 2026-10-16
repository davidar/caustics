//! `caustics-power` — the bar's power sidecar.
//!
//! UPower's `DisplayDevice` and logind's power actions, outside the bar
//! process: **one `WIRE_ROWS` frame** with `applet = APPLET_POWER`, the power
//! button and the battery as two `ROW_ITEM`s, the five power rows under the
//! button, the battery's state, brightness and power modes under the battery,
//! and actions back by row id on stdin (docs/abi.md, *Rows*, *Action*). Nothing
//! here knows about Wayland, the GPU, or the bar's internals.
//!
//! Why a separate process is the bar's membership test (docs/architecture.md,
//! *Sidecars*): this owns no Wayland object and none of the bar's fds — it
//! owns system-bus proxies, two sysfs files and a menu — so the bar process knows only Wayland, the compositor's IPC socket, and pipes.
//!
//! The split into a library and a two-line `main.rs` is so `power/tests/` can
//! drive the frame and the dispatch with fakes; no real power action is ever
//! performed by a test, and no test reads or writes this machine's backlight.

pub mod actions;
pub mod backlight;
pub mod lamp;
pub mod logind;
pub mod niri;
pub mod profiles;
pub mod rows;
pub mod upower;

use std::time::{Duration, Instant};

use caustics_bar_abi::wire::{self, ActionBody, Frame};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use zbus::Connection;

use actions::Op;
use lamp::Lamp;
use rows::Picture;

/// The usage text, on stderr — stdout carries frames and nothing else. The
/// bar spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*), so this is
/// for a human running it by hand.
pub const USAGE: &str = "\
usage: caustics-power

  No options. Writes one WIRE_ROWS frame (APPLET_POWER) to stdout whenever the
  picture changes, reads WIRE_ACTION frames from stdin, and exits when stdin
  closes. Every witness is one line on stderr.";

/// This sidecar's witness line: `caustics-power: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-power");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

/// How often the backlight is re-read while the battery's menu is open.
pub const BRIGHTNESS_POLL: Duration = Duration::from_secs(1);

/// How long after a slider tick a sysfs read may not move the slider: logind
/// writes the file a round trip after the tick, and a read in between would
/// pull the slider back under the pointer for a frame.
pub const TICK_HOLD: Duration = Duration::from_millis(1500);

/// The whole program. Returns the process's exit status.
pub fn run<I: IntoIterator<Item = String>>(argv: I) -> i32 {
    for arg in argv {
        match arg.as_str() {
            "-h" | "--help" => {
                witness(USAGE);
                return 0;
            }
            other => witness(&format!("ignoring the argument {other:?}")),
        }
    }

    // Current-thread: one battery and two menus' worth of D-Bus traffic does
    // not want a thread pool, and a single thread keeps the ordering obvious.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build();
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(e) => {
            witness(&format!("no tokio runtime ({e})"));
            return 1;
        }
    };
    runtime.block_on(serve())
}

async fn serve() -> i32 {
    // The bar holds the write end of stdin, so its EOF is this process's
    // notice to leave — the one thing every sidecar must read.
    let (act_tx, mut incoming) = unbounded_channel();
    watch_stdin(act_tx);

    // Read once at startup: an environment that changes
    // under a running process must not change whether Shut Down shuts down.
    let dry = actions::dry_run(std::env::var(actions::DRY_RUN_ENV).ok().as_deref());
    if dry {
        witness(&format!(
            "{}=1: every action is a witness and nothing else",
            actions::DRY_RUN_ENV
        ));
    }

    let conn = match Connection::system().await {
        Ok(conn) => Some(conn),
        Err(e) => {
            witness("absent: system-bus");
            witness(&format!("system-bus: {e}"));
            None
        }
    };

    let (bat_tx, mut batteries) = unbounded_channel();
    let (prof_tx, mut profiles) = unbounded_channel();
    match conn.as_ref() {
        Some(conn) => {
            tokio::spawn(upower::watch(conn.clone(), bat_tx));
            tokio::spawn(profiles::watch(conn.clone(), prof_tx));
        }
        // No bus at all is also no UPower, and `bar/bar-test.sh` reads this line.
        None => {
            witness("absent: upower");
            witness("absent: power-profiles");
        }
    }
    // The slider's lane; without a bus a tick moves the slider and nothing
    // else, and says so once.
    let setter = conn.clone().map(backlight::spawn_setter);

    // The backlight is looked for once now, so the first frame has its slider
    // (or does not), and then only while the battery's menu is open.
    let mut lamp = Lamp::new(backlight::SYSFS_BACKLIGHT);

    let mut state = Picture {
        brightness: lamp.brightness(),
        ..Picture::default()
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut revision = 1u32;
    if let Err(status) = publish(&mut out, revision, &state, true) {
        return status;
    }

    let mut poll = tokio::time::interval(BRIGHTNESS_POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // What this turn changed, and whether to say so on stderr: a slider
        // drag is a hundred frames a second and is not worth a line each.
        let mut next = state.clone();
        let mut loud = true;
        tokio::select! {
            // `Some(..)`: a watcher's end closing simply disables its arm.
            Some(battery) = batteries.recv() => next.battery = battery,
            Some(modes) = profiles.recv() => next.profiles = modes,
            _ = poll.tick(), if lamp.polling() => {
                lamp.poll(Instant::now());
                next.brightness = lamp.brightness();
                loud = false;
            }
            action = incoming.recv() => {
                let Some(action) = action else {
                    witness("stdin closed; exiting");
                    return 0;
                };
                if actions::is_menu_notice(action.kind) {
                    if lamp.notice(action.row, action.kind) {
                        poll.reset();
                    }
                    next.brightness = lamp.brightness();
                } else {
                    match actions::op_for(action.row, action.kind, action.value) {
                        Some(Op::Brightness(value)) => {
                            if let Some(raw) = lamp.tick(value, Instant::now()) {
                                next.brightness = lamp.brightness();
                                loud = false;
                                match (&setter, lamp.backlight(), dry) {
                                    (_, _, true) => {}
                                    (Some(setter), Some(backlight), false) => {
                                        let _ = setter.send(Some((backlight.name.clone(), raw)));
                                    }
                                    (None, _, false) => {
                                        if lamp.say_no_bus() {
                                            witness("brightness: there is no system bus");
                                        }
                                    }
                                    (Some(_), None, false) => {}
                                }
                            }
                        }
                        Some(Op::Profile(index)) => {
                            set_profile(conn.as_ref(), state.profiles.as_ref(), index, dry);
                        }
                        // On its own task: Lock is a walk of several
                        // logind round trips with no method timeout, and Log
                        // Out is blocking socket I/O.
                        Some(op) => actions::spawn_perform(conn.clone(), op, dry),
                        None => witness(&format!(
                            "action row={} kind={}: nothing to do",
                            action.row, action.kind
                        )),
                    }
                }
            }
        }
        if next == state {
            continue;
        }
        state = next;
        revision += 1;
        if let Err(status) = publish(&mut out, revision, &state, loud) {
            return status;
        }
    }
}

/// Whether `name` has an owner on the bus now — asked of the bus itself, so
/// asking never starts a D-Bus-activatable service. UPower and both
/// power-profiles names are commonly activatable, and zbus sets no
/// `NO_AUTO_START`: a read after the daemon left would start it again within a
/// second, so `systemctl stop tuned-ppd` would never stick and "absent:
/// power-profiles" would never be witnessed. `false` on any error: a bus that
/// cannot say is no daemon.
pub async fn has_owner(conn: &Connection, name: &str) -> bool {
    let Ok(name) = zbus::names::BusName::try_from(name) else {
        return false;
    };
    match zbus::fdo::DBusProxy::new(conn).await {
        Ok(bus) => bus.name_has_owner(name).await.unwrap_or(false),
        Err(_) => false,
    }
}

/// A power mode's radio row was clicked: set it on its own task, so a slow
/// daemon never holds the loop, and witness what came back.
fn set_profile(
    conn: Option<&Connection>,
    profiles: Option<&profiles::Profiles>,
    index: u32,
    dry: bool,
) {
    let Some(name) = profiles.and_then(|p| p.names.get(index as usize)).cloned() else {
        witness(&format!("power-mode: no profile at index {index}"));
        return;
    };
    if dry {
        witness(&format!("power-mode {name}: dry run"));
        return;
    }
    let Some(conn) = conn.cloned() else {
        witness(&format!("power-mode {name}: there is no system bus"));
        return;
    };
    tokio::spawn(async move {
        match profiles::set_active(&conn, &name).await {
            Ok(()) => witness(&format!("power-mode {name}: ok")),
            Err(why) => witness(&format!("power-mode {name}: {why}")),
        }
    });
}

/// Write the whole picture. `Err` is the process's exit status: stdout is
/// gone or broken, and so are we.
fn publish(
    out: &mut impl std::io::Write,
    revision: u32,
    state: &Picture,
    loud: bool,
) -> Result<(), i32> {
    let frame = rows::picture(revision, state, actions::settings_available());
    if let Err(err) = wire::write(out, &frame) {
        return Err(write_failed(&err));
    }
    if !loud {
        return Ok(());
    }
    let brightness = match state.brightness {
        Some(b) => format!("{}/{}", b.value, b.max),
        None => "none".to_owned(),
    };
    let modes = match &state.profiles {
        Some(p) => format!("{} active={}", p.names.join(","), p.active),
        None => "none".to_owned(),
    };
    match &state.battery {
        Some(battery) => witness(&format!(
            "published rev={revision} battery={:.0}% icon={} state={:?} brightness={brightness} modes={modes}",
            battery.percent,
            battery.icon,
            rows::state_line(battery),
        )),
        None => witness(&format!(
            "published rev={revision} battery=none brightness={brightness} modes={modes}"
        )),
    }
    Ok(())
}

/// A failed write to stdout, witnessed, as the exit status. A broken
/// pipe is the bar gone — the ordinary end, 0, like stdin's EOF; anything else
/// (a full disk behind a redirect, `EIO`) is a failure and says so with 1,
/// rather than 0 as though the bar had simply left.
pub fn write_failed(err: &std::io::Error) -> i32 {
    if err.kind() == std::io::ErrorKind::BrokenPipe {
        witness("stdout closed; exiting");
        0
    } else {
        witness(&format!("stdout write failed: {err}; exiting"));
        1
    }
}

/// Read `Action` frames on a thread and hand them to the loop. Dropping the
/// sender at EOF is what tells the loop the bar is gone.
fn watch_stdin(actions: UnboundedSender<ActionBody>) {
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read(&mut input) {
                    Ok(None) => return,
                    Ok(Some(Frame::Action(action))) => {
                        if actions.send(action).is_err() {
                            return;
                        }
                    }
                    Ok(Some(other)) => {
                        witness(&format!("ignoring a frame of kind {}", other.kind()));
                    }
                    Err(err) => {
                        witness(&format!("stdin: {err}"));
                        return;
                    }
                }
            }
        })
        .expect("spawning the stdin watcher");
}
