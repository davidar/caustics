//! What an `Action` frame on stdin means, and what it does.
//!
//! The dispatch is a **pure table** — `(row id, ACTION_*, value)` to an [`Op`],
//! and an [`Op`] to the [`Call`] it would make — so the whole mapping is
//! testable without PipeWire, without `wpctl`, and without ever moving the
//! volume on the machine running the test. [`perform`] is the only thing here
//! that runs anything, and it runs the [`Call`] the table already decided.
//!
//! The slider's action *carries* a number, so `op_for` takes it as well as
//! the row and the kind: `Op::SetVolume` then describes the whole command
//! rather than half of it, and the function stays pure.

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SET_VALUE,
};

use crate::rows::{KIND_OUTPUT, ROW_AUDIO, ROW_MIC, ROW_MUTE, ROW_SETTINGS, ROW_VOLUME, device_of};
use crate::witness;
use crate::wpctl::{self, Devices, SINK, SOURCE};
use caustics_sidecar::settings::{self, Panel};

/// The settings page the last row opens; which program shows it is
/// `caustics_sidecar::settings`' choice.
pub const SETTINGS_PANEL: Panel = Panel::Sound;

/// Whether a settings program can be named for [`SETTINGS_PANEL`]; the
/// picture leaves the settings row out when none can.
pub fn settings_available() -> bool {
    settings::settings_command(SETTINGS_PANEL).is_some()
}

/// A row of this menu that performs something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// The slider, in whole percent.
    SetVolume(u32),
    MuteSink,
    MuteSource,
    Settings,
    /// A row of the Output submenu: make this sink the default. The PipeWire
    /// node id, out of the row id.
    SetOutput(u32),
    /// A row of the Input submenu: the same for a source.
    SetInput(u32),
}

/// What an [`Op`] would do, named rather than done: the shape a test can
/// assert against without a sound server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// `wpctl` with these words after it.
    Wpctl(Vec<String>),
    /// The settings program, detached, on this panel.
    Settings(Panel),
}

impl Op {
    /// The name this op wears in every witness line.
    pub fn name(self) -> &'static str {
        match self {
            Op::SetVolume(_) => "set-volume",
            Op::MuteSink => "mute",
            Op::MuteSource => "mute-mic",
            Op::Settings => "settings",
            Op::SetOutput(_) => "set-output",
            Op::SetInput(_) => "set-input",
        }
    }

    /// The call this op makes.
    pub fn call(self) -> Call {
        match self {
            Op::SetVolume(percent) => {
                Call::Wpctl(words(&["set-volume", SINK, &format!("{percent}%")]))
            }
            Op::MuteSink => Call::Wpctl(words(&["set-mute", SINK, TOGGLE])),
            Op::MuteSource => Call::Wpctl(words(&["set-mute", SOURCE, TOGGLE])),
            Op::Settings => Call::Settings(SETTINGS_PANEL),
            // One command for both: `set-default` takes any node, and
            // WirePlumber knows a sink from a source by the node itself.
            Op::SetOutput(id) | Op::SetInput(id) => {
                Call::Wpctl(words(&["set-default", &id.to_string()]))
            }
        }
    }
}

/// `wpctl set-mute`'s third word: let it read the state, so two clicks in a
/// row can never both mean "mute".
pub const TOGGLE: &str = "toggle";

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The dispatch table. `None` is a row that performs nothing — the separator,
/// the bar item's own click (it opens the menu and never comes back), or a
/// kind of action this applet has no meaning for.
pub fn op_for(row: u32, kind: u32, value: f32) -> Option<Op> {
    match (row, kind) {
        (ROW_VOLUME, ACTION_SET_VALUE) => Some(Op::SetVolume(percent(value))),
        (ROW_MUTE, ACTION_ACTIVATE) => Some(Op::MuteSink),
        (ROW_MIC, ACTION_ACTIVATE) => Some(Op::MuteSource),
        (ROW_SETTINGS, ACTION_ACTIVATE) => Some(Op::Settings),
        // A middle click on the chip is the mute, which is the one thing worth
        // doing without opening the menu at all.
        (ROW_AUDIO, ACTION_MIDDLE) => Some(Op::MuteSink),
        // A device row: its id carries the node (`rows::device_row`). Clicking
        // the default again is harmless — `set-default` of what is already
        // the default changes nothing anyone can see.
        (row, ACTION_ACTIVATE) => match device_of(row)? {
            (KIND_OUTPUT, id) => Some(Op::SetOutput(id)),
            (_, id) => Some(Op::SetInput(id)),
        },
        _ => None,
    }
}

/// Whether `op` writes something the on-screen display would show (the
/// volume, a mute, a default device), so the change it brings round is this
/// sidecar's own and not a toast (`crate::osd`). Opening the settings program
/// writes nothing.
pub fn writes(op: Op) -> bool {
    !matches!(op, Op::Settings)
}

/// `ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`: the bar telling the sidecar that owns
/// a menu that it opened or closed (docs/abi.md, *Action*). The applet is told,
/// not asked: the loop keeps whether the menu is open for the on-screen
/// display (`crate::osd`) and says nothing, since one line per menu open, on
/// every applet, would be journal noise for nothing.
pub fn is_menu_notice(kind: u32) -> bool {
    kind == ACTION_MENU_OPEN || kind == ACTION_MENU_CLOSE
}

/// A device click, checked against the list its row came from before it
/// becomes a `set-default`. `set-default` takes any node and WirePlumber
/// moves whichever default the node's own `media.class` says, so the kind in
/// the row id is the only thing that says "this was an Output row": a headset
/// switching A2DP → HFP recreates its nodes, PipeWire recycles their ids, and
/// a click on the still-drawn Output row could otherwise move the
/// **microphone** and witness success. `Err` is the witness's text. Every
/// other op passes through.
pub fn admit(op: Op, devices: Option<&Devices>) -> Result<Op, String> {
    let (id, listed, list) = match op {
        Op::SetOutput(id) => (id, devices.is_some_and(|d| d.is_output(id)), "Output"),
        Op::SetInput(id) => (id, devices.is_some_and(|d| d.is_input(id)), "Input"),
        _ => return Ok(op),
    };
    if listed {
        Ok(op)
    } else {
        Err(format!(
            "{}: node {id} is not in the {list} list now; nothing done",
            op.name()
        ))
    }
}

/// The value the bar hands back, as the whole percent `wpctl` takes: the bar
/// de-normalised it into the row's own `0..100` already (docs/abi.md
/// *Action*), so this only has to round and refuse nonsense. Clamped to 100
/// because a *drag* never asks to overamplify, whatever arrives.
pub fn percent(value: f32) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(0.0, 100.0) as u32
}

/// The slider's own lane. A drag is one `ACTION_SET_VALUE` per pointer motion —
/// a hundred and more a second — and a `wpctl` child for each one, run
/// to completion on the select loop would queue seconds of processes that then
/// play back one by one, the volume trailing the pointer. Only the latest value
/// can matter, which is exactly a `watch` channel: the setter task runs one
/// `wpctl` at a time and, when it comes back, takes whatever the newest value
/// is by then, skipping everything in between. Mutes and the settings row
/// still go through [`perform`]; they are one click each.
pub fn spawn_volume_setter() -> tokio::sync::watch::Sender<Option<u32>> {
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let Some(percent) = *rx.borrow_and_update() else {
                continue;
            };
            perform(Op::SetVolume(percent)).await;
        }
    });
    tx
}

/// Do it, and write down what came back: nothing on success for a volume or
/// mute `wpctl` call — the next frame is the reply — one line for a device
/// switch, and one line on any failure. Opening the settings program is
/// always one line, because nothing else will show that it happened.
pub async fn perform(op: Op) {
    match op.call() {
        Call::Wpctl(args) => match wpctl::call(&args).await {
            Err(why) => witness(&format!("{}: {why}", op.name())),
            // A device switch is one click and rare, and the line is what a
            // reader of the journal (and a test) sees it by; a volume
            // drag or a mute is the next frame and nothing else.
            Ok(()) if matches!(op, Op::SetOutput(_) | Op::SetInput(_)) => witness(&format!(
                "{}: {} {}",
                op.name(),
                wpctl::WPCTL,
                args.join(" ")
            )),
            Ok(()) => {}
        },
        Call::Settings(panel) => {
            witness(&format!(
                "{}: {}",
                op.name(),
                settings::open_settings(panel)
            ));
        }
    }
}
