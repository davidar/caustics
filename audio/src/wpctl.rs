//! What PipeWire's default sink and source are doing, read through `wpctl`.
//!
//! Two reads make the volume picture:`wpctl get-volume @DEFAULT_AUDIO_SINK@`
//! and the same for `@DEFAULT_AUDIO_SOURCE@`; a third, `wpctl status`, lists
//! the sinks and sources the Output and Input submenus offer.
//! [`parse_volume`] and [`parse_status`] are pure functions over what they
//! print, so the recorded strings in `audio/tests/data/` are the tests' whole
//! input.
//!
//! Why a subprocess rather than libpulse or libpipewire: `lib.rs` says (no
//! build-time headers needed), and what a sidecar does to get its facts is the
//! sidecar's own affair. `power/src/niri.rs` is the precedent for a sidecar
//! speaking something that is not D-Bus by itself.
//!
//! **`wpctl get-volume` exits 0 when it fails.** `wpctl get-volume 999999`
//! prints `Node '999999' not found` and exits **0**, so its exit status says
//! nothing and the *text* is the only signal. That is why a read is
//! `Reading::NoNode` rather than an error. `wpctl status` and the write
//! commands do say so in their status, and are held to it.
//!
//! **Every child has [`TIMEOUT`]**: a PipeWire that is stopped, wedged
//! or in a debugger never answers `wpctl`'s core sync, and a bare
//! `.output().await` would hold the loop — its actions and its stdin EOF with
//! it. `kill_on_drop` is what makes the timeout also the kill.

use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::process::Command;

use crate::witness;

/// The binary. On `PATH`, installed with pipewire's own tooling (wireplumber).
pub const WPCTL: &str = "wpctl";

/// How long any one `wpctl` child may take. The slowest, `status`, measured
/// 0.03–0.04 s; two seconds is a server that is not answering.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// The two node names `wpctl` resolves itself, so this sidecar never has to
/// track which card is default — a default change is a `server` event and the
/// next read follows it.
pub const SINK: &str = "@DEFAULT_AUDIO_SINK@";
pub const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";

/// The picture, as the rows frame needs it. `volume` is `wpctl`'s own
/// fraction: 1.0 is 100% and anything above it is overamplified, which is a
/// state the icon set has a name for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Audio {
    pub volume: f32,
    pub muted: bool,
    /// The default source's mute. Its volume is not in the picture: "is the
    /// microphone live" is the only thing the menu offers about it.
    pub mic_muted: bool,
}

impl Audio {
    /// The volume as whole percent, which is both the slider's unit and
    /// `wpctl set-volume`'s.
    pub fn percent(&self) -> f32 {
        self.volume * 100.0
    }
}

/// One `wpctl get-volume`, read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reading {
    /// The fraction and the mute.
    Volume(f32, bool),
    /// `wpctl` ran and said there is no such node: a machine with no card,
    /// a virtual machine. Not an error — see the module's note on the exit status.
    NoNode,
}

/// Why there is no picture, or the picture.
#[derive(Clone, Debug, PartialEq)]
pub enum Sound {
    /// Both nodes read, and the devices the Output and Input submenus list.
    On(Audio, Devices),
    /// `wpctl` ran and there is no default sink.
    NoSink,
    /// `wpctl` could not be run at all; the string is the witness's reason.
    Absent(String),
}

impl Sound {
    /// What [`crate::rows::picture`] draws: `None` is the empty frame.
    pub fn audio(&self) -> Option<&Audio> {
        match self {
            Sound::On(audio, _) => Some(audio),
            _ => None,
        }
    }

    /// The device lists; empty when there is no picture.
    pub fn devices(&self) -> Option<&Devices> {
        match self {
            Sound::On(_, devices) => Some(devices),
            _ => None,
        }
    }
}

/// `Volume: 0.56` or `Volume: 0.56 [MUTED]` — the one line either read prints.
///
/// Takes the whole output rather than a line: `wpctl` is free to print
/// something else first, and the caller should not have to split. Anything
/// that is not a volume line at all (`Node '…' not found`) is `None`, which is
/// [`Reading::NoNode`] one level up.
pub fn parse_volume(text: &str) -> Option<(f32, bool)> {
    let rest = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Volume:"))?;
    let mut words = rest.split_whitespace();
    let volume: f32 = words.next()?.parse().ok()?;
    // A negative or non-finite volume is not a volume; refusing it here keeps
    // every `f32` downstream safe to clamp and to format.
    if !volume.is_finite() || volume < 0.0 {
        return None;
    }
    Some((volume, words.any(|word| word == MUTED)))
}

/// The marker `wpctl` appends to a muted node's line.
pub const MUTED: &str = "[MUTED]";

/// Read both nodes and the device lists. Three short-lived children, about
/// 10 ms each.
///
/// Each of them is a new PipeWire client, and a new client is two `pactl
/// subscribe` lines (`new` then `remove`) — which is exactly why
/// [`crate::subscribe::is_wakeup`] ignores client events. Waking on them would
/// be this function calling itself forever.
///
/// `previous` is the lists the last read gave, which a failed `status` keeps.
pub async fn read(previous: Option<&Devices>) -> Sound {
    let sink = match get_volume(SINK).await {
        Ok(reading) => reading,
        Err(err) => return Sound::Absent(err),
    };
    let Reading::Volume(volume, muted) = sink else {
        return Sound::NoSink;
    };
    // A source that is missing or unreadable is a microphone that is not
    // muted: the row is still there and still toggles, and a second failure
    // mode on the way to a mute checkbox is not worth a second absent branch.
    let mic_muted = matches!(get_volume(SOURCE).await, Ok(Reading::Volume(_, true)));
    // The third child: the lists for the Output and Input submenus. It
    // runs on every re-read, and the re-reads are exactly the `sink`,
    // `source` and `server` events — a device arriving or leaving, and the
    // default moving — so no event needs a reading of its own. A `status`
    // that would not run, ran out of time, or exited non-zero (a server
    // restarting between the reads) keeps the lists the last read gave — or
    // none at the first, and the menu says "No device" — beside the volume
    // rows, which are fresh; the next wake reads again.
    let devices = match status().await {
        Ok(devices) => devices,
        Err(why) => {
            witness(&format!("{WPCTL} status: {why}; keeping the last lists"));
            previous.cloned().unwrap_or_default()
        }
    };
    Sound::On(
        Audio {
            volume,
            muted,
            mic_muted,
        },
        devices,
    )
}

/// One audio device as `wpctl status` lists it: the node's id, which is what
/// `wpctl set-default` takes, and its description, which is what the menu row
/// says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: u32,
    pub label: String,
    /// The `*` in front of the id: this is the default.
    pub default: bool,
}

/// The two lists the Output and Input submenus are built from. Empty is a
/// section with nothing in it, and also what a `wpctl status` that would not
/// run reads as — the menu then says "No device" rather than nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Devices {
    /// `Audio` → `Sinks:`, then the `Filters:` entries of class `Audio/Sink`.
    pub outputs: Vec<Device>,
    /// `Audio` → `Sources:`, then the `Filters:` entries of class
    /// `Audio/Source`; monitors left out.
    pub inputs: Vec<Device>,
}

impl Devices {
    /// The default output's id, if one is marked.
    pub fn default_output(&self) -> Option<u32> {
        self.outputs.iter().find(|d| d.default).map(|d| d.id)
    }

    /// The default input's id, if one is marked.
    pub fn default_input(&self) -> Option<u32> {
        self.inputs.iter().find(|d| d.default).map(|d| d.id)
    }

    /// Is `id` in the Output list? A click on an Output row is checked against
    /// this before it becomes a `set-default` ([`crate::actions::admit`]).
    pub fn is_output(&self, id: u32) -> bool {
        self.outputs.iter().any(|d| d.id == id)
    }

    /// Is `id` in the Input list?
    pub fn is_input(&self, id: u32) -> bool {
        self.inputs.iter().any(|d| d.id == id)
    }
}

/// The most devices one submenu lists. A real machine has a handful; the cap
/// is what keeps a pathological graph inside the frame's `MAX_ROWS`.
pub const MAX_DEVICES: usize = 32;

/// The characters `wpctl status` draws its tree with, plus the indent.
fn is_tree(c: char) -> bool {
    matches!(c, ' ' | '\t' | '│' | '├' | '└' | '─')
}

/// A source that is a sink's monitor rather than a microphone. `wpctl status`
/// does not list PipeWire's own monitor ports as nodes, but pulse modules and
/// loopbacks can make monitor *nodes*, and they say so in the description
/// (`Monitor of …`) or, under `--name`, in the name (`….monitor`).
pub fn is_monitor(label: &str) -> bool {
    label.starts_with("Monitor of ") || label.ends_with(".monitor")
}

/// `wpctl status`, read for the two lists: the `Sinks:`, `Sources:` and
/// `Filters:` sections of the top-level `Audio` block — never `Video`'s, which
/// has the same headings for cameras. A pure function, so the recorded outputs
/// in `audio/tests/data/` are the tests' whole input.
///
/// An entry is `│  *   60. Built-in Audio Controller Analog Stereo [vol: 0.72]`:
/// the tree, an optional `*`, the id and a dot, the description, and the
/// volume in brackets, which is cut (`wpctl.c`: `"%c %4u. %-35s"`, then
/// `" [vol: %.2f%s"`). Anything else in a section is skipped.
///
/// **Filters**: wpctl's `Sinks:` is `*/Sink*` *without* a
/// `node.link-group`; a node with one — EasyEffects, echo-cancel,
/// `pw-loopback`, filter-chain — is listed under `Filters:` instead, as
/// `"%c %4u. %-60s [%s]"`, its media class in the bracket and the `*` when it
/// is the default. An entry whose bracket is exactly `[Audio/Sink]` is an
/// output and exactly `[Audio/Source]` an input; any other class is skipped.
/// Without them, an EasyEffects default would leave the Output submenu with nothing
/// checked and no row to go back to it by.
pub fn parse_status(text: &str) -> Devices {
    #[derive(Clone, Copy, PartialEq)]
    enum In {
        Elsewhere,
        Sinks,
        Sources,
        Filters,
    }
    let mut devices = Devices::default();
    let mut audio = false;
    let mut section = In::Elsewhere;
    for line in text.lines() {
        // A top-level heading starts in column 0: `Audio`, `Video`,
        // `Settings`, and the `PipeWire '…'` line.
        if line.chars().next().is_some_and(|c| !is_tree(c)) {
            audio = line.trim_end() == "Audio";
            section = In::Elsewhere;
            continue;
        }
        if !audio {
            continue;
        }
        let rest = line.trim_start_matches(is_tree).trim_end();
        let entry = rest.starts_with('*') || rest.starts_with(|c: char| c.is_ascii_digit());
        if !entry {
            // A sub-heading — `Sinks:`, `Sources:`, `Devices:`, `Filters:`,
            // `Streams:` — or the tree's blank spacer line.
            if let Some(heading) = rest.strip_suffix(':') {
                section = match heading {
                    "Sinks" => In::Sinks,
                    "Sources" => In::Sources,
                    "Filters" => In::Filters,
                    _ => In::Elsewhere,
                };
            }
            continue;
        }
        let (output, cut) = match section {
            In::Sinks => (true, VOL),
            In::Sources => (false, VOL),
            // The class is the last bracket, and the label is what is before it.
            In::Filters => match rest.rfind(" [").map(|at| &rest[at..]) {
                Some(" [Audio/Sink]") => (true, " ["),
                Some(" [Audio/Source]") => (false, " ["),
                _ => continue,
            },
            In::Elsewhere => continue,
        };
        let Some(device) = parse_entry(rest, cut) else {
            continue;
        };
        if !output && is_monitor(&device.label) {
            continue;
        }
        let list = if output {
            &mut devices.outputs
        } else {
            &mut devices.inputs
        };
        if list.len() < MAX_DEVICES {
            list.push(device);
        }
    }
    devices
}

/// Where a sink's or a source's label ends: wpctl's volume bracket.
const VOL: &str = " [vol:";

/// The label a device with an empty description wears: a lone sink must
/// still be a row, or the menu reads "No device" while audio plays.
pub fn fallback_label(id: u32) -> String {
    format!("Node {id}")
}

/// `*   60. Built-in Audio Controller Analog Stereo [vol: 0.72]`, or the same
/// without the `*`, with the tree already trimmed off; the label ends at the
/// last `cut`.
fn parse_entry(rest: &str, cut: &str) -> Option<Device> {
    let (default, rest) = match rest.strip_prefix('*') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };
    // The id ends at its dot; a line trimmed right after it has no space.
    let (id, rest) = rest.split_once('.')?;
    let id: u32 = id.parse().ok()?;
    let label = match rest.rfind(cut) {
        Some(at) => &rest[..at],
        None => rest,
    };
    let label = label.trim();
    let label = if label.is_empty() {
        fallback_label(id)
    } else {
        label.to_owned()
    };
    Some(Device { id, label, default })
}

/// Run one `wpctl`-shaped child to completion, or give up on it after
/// `limit`. `Err` is the binary not running, or not answering in time — which
/// drops the child, and `kill_on_drop` kills it. The program is a parameter
/// so a test can stand a `sleep` in for a wedged server.
pub async fn output(program: &str, args: &[&str], limit: Duration) -> Result<Output, String> {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(limit, child).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => Err(format!("{program}: {err}")),
        Err(_) => Err(format!(
            "{program} {}: no answer in {} ms",
            args.join(" "),
            limit.as_millis()
        )),
    }
}

/// `wpctl status`, run and read. `Err` is the binary not running, not
/// answering, or exiting non-zero; a server with nothing in it is `Ok` with
/// two empty lists.
pub async fn status() -> Result<Devices, String> {
    let output = output(WPCTL, &["status"], TIMEOUT).await?;
    if !output.status.success() {
        return Err(complaint(&output));
    }
    Ok(parse_status(&String::from_utf8_lossy(&output.stdout)))
}

/// One read. `Err` is "the binary would not run, or did not answer", the
/// only thing that is really an error here.
pub async fn get_volume(node: &str) -> Result<Reading, String> {
    let output = output(WPCTL, &["get-volume", node], TIMEOUT).await?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(match parse_volume(&text) {
        Some((volume, muted)) => Reading::Volume(volume, muted),
        None => Reading::NoNode,
    })
}

/// `wpctl <args…>`, for the write commands the action table names. The exit
/// status is the witness on failure only, so this returns `Ok(())` for a
/// command that ran and said nothing.
pub async fn call(args: &[String]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = output(WPCTL, &args, TIMEOUT).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(complaint(&output))
}

/// `wpctl`'s own complaint, first line, so the witness stays one line; the
/// exit status when it said nothing.
fn complaint(output: &Output) -> String {
    let why = String::from_utf8_lossy(&output.stderr);
    let why = why.lines().next().unwrap_or("").trim();
    if why.is_empty() {
        format!("{}", output.status)
    } else {
        why.to_owned()
    }
}
