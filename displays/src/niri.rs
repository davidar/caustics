//! niri's IPC, the part of it this sidecar speaks: the `Outputs` request, and
//! the `Output` request with `On`, `Off`, `Scale`, `Mode` and `Transform`.
//!
//! One request, one reply, one connection, over `$NIRI_SOCKET`, as
//! `caustics-power`'s Log Out does it. The requests are built here as
//! `serde_json::Value`s (an output's name is the one string that comes from
//! outside, and it is escaped by being a JSON string), and the replies are
//! read the same way: niri's IPC types are not copied into this crate. The
//! shapes are the ones `niri msg output` sends, read off its socket.
//!
//! Every function here but [`round_trip`] is pure, so the tests drive the
//! parsing and the requests with no niri at all.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Value, json};

/// The environment variable niri sets to its IPC socket.
pub const SOCKET_ENV: &str = "NIRI_SOCKET";

/// How long either half of a round trip may take. Blocking I/O, on the
/// worker thread ([`crate::serve`]), never the loop's.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// The most a reply may be. An `Outputs` reply is about a kilobyte per
/// output with its mode list; this is far past any real one, and a bound on a
/// peer that never stops writing.
pub const MAX_REPLY: u64 = 1 << 20;

/// The `Outputs` request, verbatim: a unit variant is a bare JSON string.
pub const OUTPUTS: &str = r#""Outputs""#;

/// One mode of an output, as niri lists it. `refresh` is niri's own unit,
/// millihertz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u16,
    pub height: u16,
    pub refresh: u32,
    pub preferred: bool,
}

/// One output, as much of niri's `Output` as the menu shows. An output that
/// is off has no current mode and no logical size, so `on` is false and
/// `current`, `scale` and `transform` are `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    /// The connector: `eDP-1`, `DP-1`.
    pub name: String,
    pub make: String,
    pub model: String,
    pub on: bool,
    pub modes: Vec<Mode>,
    /// The index of the current mode in `modes`.
    pub current: Option<usize>,
    pub scale: Option<f64>,
    /// niri's spelling: `Normal`, `90`, `180`, `270`, `Flipped`, `Flipped90`…
    pub transform: Option<String>,
}

impl Output {
    /// What the menu calls it: the connector, then the make and model niri
    /// reports, without the words a missing EDID leaves behind.
    pub fn label(&self) -> String {
        let words: Vec<&str> = [self.make.as_str(), self.model.as_str()]
            .into_iter()
            .map(str::trim)
            .filter(|w| !w.is_empty() && *w != "Unknown")
            .collect();
        if words.is_empty() {
            self.name.clone()
        } else {
            format!("{} · {}", self.name, words.join(" "))
        }
    }

    /// The current mode, if the output is on and niri named one.
    pub fn current_mode(&self) -> Option<Mode> {
        self.current.and_then(|i| self.modes.get(i)).copied()
    }
}

/// niri's `Outputs` reply, read: `{"Ok":{"Outputs":{name: Output, …}}}`,
/// sorted by connector name so the menu's order does not follow a hash map's.
/// `Err` says what was wrong with the reply, or what niri said instead.
pub fn parse_outputs(reply: &str) -> Result<Vec<Output>, String> {
    let value: Value = serde_json::from_str(reply.trim())
        .map_err(|e| format!("niri's reply is not JSON ({e})"))?;
    if let Some(err) = value.get("Err") {
        return Err(format!("niri answered {}", text_of(err)));
    }
    let Some(map) = value
        .get("Ok")
        .and_then(|ok| ok.get("Outputs"))
        .and_then(Value::as_object)
    else {
        return Err(format!("niri answered {}", short(reply)));
    };
    let mut outputs: Vec<Output> = map.iter().map(|(key, o)| parse_output(key, o)).collect();
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(outputs)
}

fn parse_output(key: &str, o: &Value) -> Output {
    let text = |field: &str| {
        o.get(field)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let modes: Vec<Mode> = o
        .get("modes")
        .and_then(Value::as_array)
        .map(|modes| modes.iter().filter_map(parse_mode).collect())
        .unwrap_or_default();
    let current = o
        .get("current_mode")
        .and_then(Value::as_u64)
        .map(|i| i as usize)
        .filter(|i| *i < modes.len());
    let logical = o.get("logical").filter(|l| !l.is_null());
    let name = match text("name") {
        n if n.is_empty() => key.to_owned(),
        n => n,
    };
    Output {
        name,
        make: text("make"),
        model: text("model"),
        on: logical.is_some() && current.is_some(),
        current,
        scale: logical.and_then(|l| l.get("scale")).and_then(Value::as_f64),
        transform: logical
            .and_then(|l| l.get("transform"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        modes,
    }
}

fn parse_mode(m: &Value) -> Option<Mode> {
    Some(Mode {
        width: u16::try_from(m.get("width")?.as_u64()?).ok()?,
        height: u16::try_from(m.get("height")?.as_u64()?).ok()?,
        refresh: u32::try_from(m.get("refresh_rate")?.as_u64()?).ok()?,
        preferred: m
            .get("is_preferred")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// What an `Output` request asks niri to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    On,
    Off,
    Scale(f64),
    Mode(Mode),
    /// niri's spelling of the transform (`Normal`, `90`, `180`, `270`).
    Transform(&'static str),
}

/// The `Output` request for `output`, as one line of JSON.
pub fn request(output: &str, change: &Change) -> String {
    let action = match change {
        Change::On => json!("On"),
        Change::Off => json!("Off"),
        Change::Scale(scale) => json!({ "Scale": { "scale": { "Specific": scale } } }),
        // niri's `refresh` is in hertz, and it matches it against its own
        // millihertz, so the mode's own number divided by a thousand is the
        // mode and no rounding picks a neighbour.
        Change::Mode(mode) => json!({ "Mode": { "mode": { "Specific": {
            "width": mode.width,
            "height": mode.height,
            "refresh": f64::from(mode.refresh) / 1000.0,
        } } } }),
        Change::Transform(t) => json!({ "Transform": { "transform": t } }),
    };
    json!({ "Output": { "output": output, "action": action } }).to_string()
}

/// niri's reply to an `Output` request, judged. `Applied` is done;
/// `OutputWasMissing` is niri keeping the setting for an output it does not
/// have (it went between the menu and the click), which is not what the
/// click meant, so it is said as a failure; anything else is said as niri
/// said it.
pub fn outcome(reply: &str) -> Result<(), String> {
    let reply = reply.trim();
    if reply.is_empty() {
        return Err("niri closed the socket without a reply".to_owned());
    }
    let Ok(value) = serde_json::from_str::<Value>(reply) else {
        return Err(format!("niri answered {}", short(reply)));
    };
    if let Some(err) = value.get("Err") {
        return Err(format!("niri answered {}", text_of(err)));
    }
    match value
        .get("Ok")
        .and_then(|ok| ok.get("OutputConfigChanged"))
        .and_then(Value::as_str)
    {
        Some("Applied") => Ok(()),
        Some("OutputWasMissing") => Err(
            "niri has no such output now (it keeps the setting for when one appears)".to_owned(),
        ),
        _ => Err(format!("niri answered {}", short(reply))),
    }
}

/// A JSON value as a witness says it: a string bare, anything else as JSON.
fn text_of(v: &Value) -> String {
    match v.as_str() {
        Some(s) => s.to_owned(),
        None => v.to_string(),
    }
}

/// A reply cut to what a witness line can carry.
fn short(reply: &str) -> String {
    let reply = reply.trim();
    match reply.char_indices().nth(160) {
        Some((at, _)) => format!("{}…", &reply[..at]),
        None => reply.to_owned(),
    }
}

/// One request and its whole reply over the socket at `socket`. niri closes
/// its write half after a non-streaming reply, so reading to the end is the
/// same as reading the one line.
pub fn round_trip(socket: &OsStr, request: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    writeln!(stream, "{request}")?;
    stream.flush()?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.take(MAX_REPLY).read_to_string(&mut reply)?;
    Ok(reply.trim().to_owned())
}
