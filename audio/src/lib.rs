//! `caustics-audio` — the bar's audio sidecar.
//!
//! PipeWire's default sink and source as **one `WIRE_ROWS` frame** with
//! `applet = APPLET_AUDIO`: a chip whose icon is the volume bucket, a slider,
//! two mute checkboxes, the Output and Input submenus, and the settings row
//! when there is a settings program, with actions back by row id on stdin
//! (docs/abi.md, *Rows*, *Action*). Nothing here knows about Wayland, the GPU,
//! or the bar's internals.
//!
//! Why a separate process is the bar's membership test (docs/architecture.md,
//! *Sidecars*): this owns no Wayland object and none of the bar's fds — it owns a `pactl subscribe` child and short-lived `wpctl` children — and the bar
//! copies its bytes without parsing any of them.
//!
//! **The shape**, and why it is subprocesses: linking libpulse or libpipewire
//! needs their headers and pkg-config files at build time, which an immutable
//! desktop host does not ship, while `pactl` and `wpctl` come with every
//! PipeWire install; and what a sidecar does to get its facts is its own
//! affair. So `pactl subscribe` is the wake-up, `wpctl get-volume` the read,
//! `wpctl set-volume`/`set-mute` the write. Swapping all of that for libpulse
//! would change nothing above [`wpctl::read`].
//!
//! The split into a library and a two-line `main.rs` is so `audio/tests/` can
//! drive the frame and the dispatch with fakes and recorded strings; no test
//! ever moves the volume on the machine it runs on.

pub mod actions;
pub mod osd;
pub mod rows;
pub mod subscribe;
pub mod wpctl;

use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::ACTION_MENU_OPEN;
use caustics_bar_abi::wire::ACTION_TOAST_SHOWN;
use caustics_bar_abi::wire::{self, ActionBody, Frame};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use osd::{Flagged, Osd};
use wpctl::Sound;

/// The usage text, on stderr — stdout carries frames and nothing else. The bar
/// spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*), so this is for a
/// human running it by hand.
pub const USAGE: &str = "\
usage: caustics-audio

  No options. Writes one WIRE_ROWS frame (APPLET_AUDIO) to stdout whenever the
  picture changes, reads WIRE_ACTION frames from stdin, and exits when stdin
  closes. Needs pactl and wpctl on PATH. Every witness is one line on stderr.";

/// How long to let a burst of `pactl subscribe` lines finish before reading.
/// Events arrive in bursts — a volume change is several — and the read is three child processes, so coalescing is most of the cost saved.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// How often to re-read regardless. It is what makes every absence
/// self-healing — no `wpctl`, no default sink, no `pactl` and so no wake-ups at
/// all — for the price of six short-lived children a minute.
const RETRY: Duration = Duration::from_secs(30);

/// How long the on-screen display stays up after the last change that raised
/// it (`osd`): long enough to read a volume off the slider, short enough that
/// a key press is not a popup to wait out. Each further change extends it.
pub const OSD_HOLD: Duration = Duration::from_millis(1500);

/// How long after this sidecar's own write a change is its own and not shown:
/// the user is dragging the slider or clicking a mute in the menu, or has
/// just left it, and the `pactl` event a write brings round arrives a round
/// trip later. Without it a menu that closes with the flags still set would
/// pop a toast of what the user just did.
pub const OSD_OWN_HOLD: Duration = Duration::from_millis(1500);

/// This sidecar's witness line: `caustics-audio: …` on stderr, in **one write**
/// (docs/abi.md, *Writing a sidecar*; the one copy is `caustics_sidecar::Witness`).
pub const WITNESS: caustics_sidecar::Witness = caustics_sidecar::Witness::new("caustics-audio");

/// One witness line; see [`WITNESS`].
pub fn witness(line: &str) {
    WITNESS.line(line)
}

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

    // Current-thread: one `pactl` child's lines and a pair of `wpctl` reads do
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

    let (wake_tx, mut wakeups) = unbounded_channel();
    tokio::spawn(subscribe::watch(wake_tx));
    let volume = actions::spawn_volume_setter();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut revision = 1u32;
    let mut sound = wpctl::read(None).await;
    say_absence(&sound, None);
    // The first reading is never a toast: nothing changed, it was only read.
    let mut osd = Osd::default();
    if !publish(&mut out, revision, &sound, osd.flagged()) {
        return 0;
    }

    let mut retry = tokio::time::interval(RETRY);
    // The first tick of an interval is immediate, and the read above just
    // happened.
    retry.tick().await;
    loop {
        let deadline = osd.deadline();
        let ends = tokio::time::sleep_until(tokio::time::Instant::from_std(
            deadline.unwrap_or_else(Instant::now),
        ));
        tokio::select! {
            // `Some(..)`: the watcher's end closing simply disables this arm.
            Some(()) = wakeups.recv() => {
                // A volume change is several events; let the burst land before paying for the three `wpctl` children of a read.
                tokio::time::sleep(DEBOUNCE).await;
                while wakeups.try_recv().is_ok() {}
                if !refresh(&mut out, &mut revision, &mut sound, &mut osd).await {
                    return 0;
                }
            }
            _ = retry.tick() => {
                if !refresh(&mut out, &mut revision, &mut sound, &mut osd).await {
                    return 0;
                }
            }
            // The on-screen display's time is up: the same picture without
            // its bits, which is what ends the toast.
            _ = ends, if deadline.is_some() => {
                if osd.expire(Instant::now()) {
                    witness("osd: ended (timeout)");
                    revision += 1;
                    if !publish(&mut out, revision, &sound, osd.flagged()) {
                        return 0;
                    }
                }
            }
            action = incoming.recv() => match action {
                None => {
                    witness("stdin closed; exiting");
                    return 0;
                }
                // The toast that showed our rows ended (one of these per row
                // it showed): the burst is over, whatever its clock says.
                Some(action) if action.kind == ACTION_TOAST_SHOWN => {
                    if osd.end() {
                        witness("osd: ended (shown)");
                        revision += 1;
                        if !publish(&mut out, revision, &sound, osd.flagged()) {
                            return 0;
                        }
                    }
                }
                // The bar's two menu notices (`actions::is_menu_notice`): a
                // change while the menu is up is on screen already, and the
                // menu opening ends a burst.
                Some(action) if actions::is_menu_notice(action.kind) => {
                    if osd.menu(action.kind == ACTION_MENU_OPEN) {
                        witness("osd: ended (menu)");
                        revision += 1;
                        if !publish(&mut out, revision, &sound, osd.flagged()) {
                            return 0;
                        }
                    }
                }
                Some(action) => match actions::op_for(action.row, action.kind, action.value) {
                    // A drag's values go to the setter, newest wins
                    // (`actions::spawn_volume_setter`); the loop never waits
                    // on a `wpctl` for them.
                    Some(actions::Op::SetVolume(percent)) => {
                        osd.own_write(Instant::now());
                        let _ = volume.send(Some(percent));
                    }
                    // The reply to an action is the next frame, which the
                    // resulting `pactl` event brings round by itself. A device
                    // click is first checked against the lists it was drawn
                    // from (`actions::admit`).
                    Some(op) => match actions::admit(op, sound.devices()) {
                        Ok(op) => {
                            if actions::writes(op) {
                                osd.own_write(Instant::now());
                            }
                            actions::perform(op).await;
                        }
                        Err(why) => witness(&why),
                    },
                    None => witness(&format!(
                        "action row={} kind={}: nothing to do",
                        action.row, action.kind
                    )),
                },
            },
        }
    }
}

/// Read both nodes and publish if the picture moved, raising the on-screen
/// display when the change is one to show (`osd::decide`). `false` means
/// stdout is gone and so are we.
async fn refresh(
    out: &mut impl std::io::Write,
    revision: &mut u32,
    sound: &mut Sound,
    osd: &mut Osd,
) -> bool {
    let next = wpctl::read(sound.devices()).await;
    if next == *sound {
        return true;
    }
    say_absence(&next, Some(sound));
    if let Some(line) = osd.change(Some(sound), &next, Instant::now()) {
        witness(&line);
    }
    *sound = next;
    *revision += 1;
    publish(out, *revision, sound, osd.flagged())
}

/// Write the whole picture, with the on-screen display's bits. `false` means
/// stdout is gone and so are we.
fn publish(out: &mut impl std::io::Write, revision: u32, sound: &Sound, osd: Flagged) -> bool {
    let audio = sound.audio();
    let none = wpctl::Devices::default();
    let devices = sound.devices().unwrap_or(&none);
    let frame = rows::toasting(revision, audio, devices, actions::settings_available(), osd);
    if let Err(err) = wire::write(out, &frame) {
        if err.kind() == std::io::ErrorKind::BrokenPipe {
            witness("stdout closed; exiting");
        } else {
            witness(&format!("stdout write failed: {err}"));
        }
        return false;
    }
    match audio {
        Some(audio) => witness(&format!(
            "published rev={revision} volume={:.0}% icon={} mic={} outputs={} output={} inputs={} input={}",
            audio.percent(),
            rows::icon_for(audio.percent(), audio.muted),
            if audio.mic_muted { "muted" } else { "live" },
            devices.outputs.len(),
            node(devices.default_output()),
            devices.inputs.len(),
            node(devices.default_input()),
        )),
        None => witness(&format!("published rev={revision} rows=none")),
    }
    true
}

/// A default's node id in the published witness, or `none`.
fn node(id: Option<u32>) -> String {
    id.map_or_else(|| "none".to_owned(), |id| id.to_string())
}

/// The `absent:` line, once per arrival at an absence rather than once per
/// read: the 30 s re-read would otherwise write the bar's journal for it.
fn say_absence(now: &Sound, before: Option<&Sound>) {
    if before.is_some_and(|before| std::mem::discriminant(before) == std::mem::discriminant(now)) {
        return;
    }
    match now {
        Sound::On(..) => {
            if before.is_some() {
                witness("present: the default sink is back");
            }
        }
        Sound::NoSink => witness("absent: sink"),
        Sound::Absent(why) => {
            witness(&format!("absent: {}", wpctl::WPCTL));
            witness(&format!("{}: {why}", wpctl::WPCTL));
        }
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
