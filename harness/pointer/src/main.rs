//! `caustics-pointer`: the nested gate's mouse (docs/testing.md, *The nested gate*).
//!
//! A virtual machine target can drive its guest's pointer with QMP
//! `input-send-event`; the
//! nested target has no hypervisor between it and the compositor, so it asks
//! the compositor itself, through `zwlr_virtual_pointer_manager_v1` — which
//! niri implements with absolute motion on a named output. One invocation is
//! one action and exits once the compositor has seen it (a roundtrip), so a
//! shell step that runs `move` and then `button` gets them in that order.
//!
//! ```text
//! caustics-pointer [--output NAME] move X Y        # physical px of that output
//! caustics-pointer [--output NAME] button left|right|middle press|release
//! caustics-pointer [--output NAME] click [left|right|middle] [HOLD_MS]
//! caustics-pointer [--output NAME] wheel up|down [STEPS]
//! ```
//!
//! `--output` defaults to `winit`, the nested niri's one output. The extents
//! of an absolute move are that output's current mode, read off `wl_output`,
//! so a move is in the output's own physical pixels, whatever size the window came up at; an
//! explicit `--extent WxH` overrides them.
//!
//! Not a sidecar: it is in no `SIDECARS` list, `bar/install.sh` does not ship
//! it, and it speaks no ABI. It is a harness tool that happens to be Rust.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// One wheel notch, in `wl_pointer.axis` units: what libinput reports for a
/// mouse wheel's click, and what every client's scroll code expects.
const NOTCH: f64 = 15.0;

#[derive(Default)]
struct Outputs {
    /// Every bound `wl_output`, with what it said about itself.
    seen: Vec<Seen>,
}

/// One `wl_output`: its name (v4), current mode in physical px, transform.
struct Seen {
    output: wl_output::WlOutput,
    name: Option<String>,
    mode: Option<(i32, i32)>,
    transform: wl_output::Transform,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Outputs {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Outputs {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(entry) = state.seen.iter_mut().find(|s| s.output.id() == output.id()) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => entry.name = Some(name),
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                let current =
                    matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current));
                if current {
                    entry.mode = Some((width, height));
                }
            }
            wl_output::Event::Geometry {
                transform: WEnum::Value(t),
                ..
            } => entry.transform = t,
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Outputs {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for Outputs {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerManagerV1,
        _: <ZwlrVirtualPointerManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for Outputs {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerV1,
        _: <ZwlrVirtualPointerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

enum Action {
    Move { x: u32, y: u32 },
    Button { code: u32, pressed: bool },
    Click { code: u32, hold_ms: u64 },
    Wheel { steps: i32 },
}

const USAGE: &str = "usage: caustics-pointer [--output NAME] [--extent WxH] \
    (move X Y | button left|right|middle press|release | click [BUTTON] [HOLD_MS] | wheel up|down [STEPS])";

fn button_code(name: &str) -> Result<u32, String> {
    match name {
        "left" => Ok(BTN_LEFT),
        "right" => Ok(BTN_RIGHT),
        "middle" => Ok(BTN_MIDDLE),
        other => Err(format!("unknown button '{other}'")),
    }
}

fn number<T: std::str::FromStr>(arg: Option<&String>, what: &str) -> Result<T, String> {
    let arg = arg.ok_or_else(|| format!("missing {what}"))?;
    // A coordinate may come out of the harness's arithmetic as `640.0`.
    let trimmed = arg.strip_suffix(".0").unwrap_or(arg);
    trimmed.parse().map_err(|_| format!("bad {what} '{arg}'"))
}

fn parse(args: &[String]) -> Result<(String, Option<(u32, u32)>, Action), String> {
    let mut output = "winit".to_owned();
    let mut extent = None;
    let mut rest = args;
    loop {
        match rest.first().map(String::as_str) {
            Some("--output") => {
                output = rest.get(1).ok_or("--output needs a name")?.clone();
                rest = &rest[2..];
            }
            Some("--extent") => {
                let value = rest.get(1).ok_or("--extent needs WxH")?;
                let (w, h) = value.split_once('x').ok_or("--extent is WxH")?;
                extent = Some((
                    w.parse().map_err(|_| "bad extent width")?,
                    h.parse().map_err(|_| "bad extent height")?,
                ));
                rest = &rest[2..];
            }
            _ => break,
        }
    }
    let action = match rest.first().map(String::as_str) {
        Some("move") => Action::Move {
            x: number(rest.get(1), "x")?,
            y: number(rest.get(2), "y")?,
        },
        Some("button") => Action::Button {
            code: button_code(rest.get(1).map(String::as_str).unwrap_or(""))?,
            pressed: match rest.get(2).map(String::as_str) {
                Some("press") => true,
                Some("release") => false,
                other => {
                    return Err(format!(
                        "button state must be press or release, not {other:?}"
                    ));
                }
            },
        },
        Some("click") => Action::Click {
            code: button_code(rest.get(1).map(String::as_str).unwrap_or("left"))?,
            hold_ms: if rest.len() > 2 {
                number(rest.get(2), "hold")?
            } else {
                100
            },
        },
        Some("wheel") => {
            let steps: i32 = if rest.len() > 2 {
                number(rest.get(2), "steps")?
            } else {
                1
            };
            Action::Wheel {
                steps: match rest.get(1).map(String::as_str) {
                    Some("down") => steps,
                    Some("up") => -steps,
                    other => {
                        return Err(format!("wheel direction must be up or down, not {other:?}"));
                    }
                },
            }
        }
        _ => return Err(USAGE.to_owned()),
    };
    Ok((output, extent, action))
}

/// Where on the *device* to put the pointer so that it lands on pixel
/// `(x, y)` of a `w`x`h` output with this transform, and the device's extents.
///
/// An absolute pointer is in the output's untransformed frame, and the
/// compositor turns its position by the output's transform, the way it would a
/// tablet mapped to a rotated screen. niri's winit output reports `Flipped180`
/// (its GL frame is upside down), so without this a move to the bottom edge
/// lands at the top. Each arm undoes its transform's map from the device area
/// onto the output; the extents swap for the quarter turns.
fn device_point(t: wl_output::Transform, x: u32, y: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    use wl_output::Transform as T;
    let (dw, dh) = match t {
        T::_90 | T::_270 | T::Flipped90 | T::Flipped270 => (h, w),
        _ => (w, h),
    };
    let (rx, ry) = (dw - 1, dh - 1);
    let (px, py) = match t {
        T::_90 => (y, ry.saturating_sub(x)),
        T::_180 => (rx.saturating_sub(x), ry.saturating_sub(y)),
        T::_270 => (rx.saturating_sub(y), x),
        T::Flipped => (rx.saturating_sub(x), y),
        T::Flipped90 => (y, x),
        T::Flipped180 => (x, ry.saturating_sub(y)),
        T::Flipped270 => (rx.saturating_sub(y), ry.saturating_sub(x)),
        _ => (x, y),
    };
    (px, py, dw, dh)
}

/// Milliseconds for the protocol's `time`: wall-clock, truncated. Only order
/// matters to the compositor, and successive invocations are increasing.
fn now_ms() -> u32 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (ms & 0xffff_ffff) as u32
}

fn run(args: &[String]) -> Result<(), String> {
    let (want_output, extent, action) = parse(args)?;
    let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<Outputs>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let mut state = Outputs::default();

    let manager: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 2..=2, ())
        .map_err(|e| format!("zwlr_virtual_pointer_manager_v1 v2: {e}"))?;
    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("wl_seat: {e}"))?;
    let list = globals.contents().clone_list();
    for global in list
        .iter()
        .filter(|g| g.interface == wl_output::WlOutput::interface().name)
    {
        if global.version < 4 {
            continue;
        }
        let output: wl_output::WlOutput = globals.registry().bind(global.name, 4, &qh, ());
        state.seen.push(Seen {
            output,
            name: None,
            mode: None,
            transform: wl_output::Transform::Normal,
        });
    }
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip: {e}"))?;

    let Some(seen) = state
        .seen
        .iter()
        .find(|s| s.name.as_deref() == Some(want_output.as_str()))
    else {
        let names: Vec<_> = state.seen.iter().filter_map(|s| s.name.clone()).collect();
        return Err(format!(
            "no output named '{want_output}' (have: {})",
            names.join(", ")
        ));
    };
    let mode = &seen.mode;
    let pointer =
        manager.create_virtual_pointer_with_output(Some(&seat), Some(&seen.output), &qh, ());

    match action {
        Action::Move { x, y } => {
            let (w, h) = match (extent, mode) {
                (Some(e), _) => e,
                (None, Some((w, h))) if *w > 0 && *h > 0 => (*w as u32, *h as u32),
                _ => {
                    return Err(format!(
                        "output '{want_output}' has no current mode; pass --extent"
                    ));
                }
            };
            let (dx, dy, dw, dh) = device_point(seen.transform, x.min(w - 1), y.min(h - 1), w, h);
            pointer.motion_absolute(now_ms(), dx, dy, dw, dh);
            pointer.frame();
        }
        Action::Button { code, pressed } => {
            let state = if pressed {
                wl_pointer::ButtonState::Pressed
            } else {
                wl_pointer::ButtonState::Released
            };
            pointer.button(now_ms(), code, state);
            pointer.frame();
        }
        Action::Click { code, hold_ms } => {
            pointer.button(now_ms(), code, wl_pointer::ButtonState::Pressed);
            pointer.frame();
            conn.flush().map_err(|e| format!("flush: {e}"))?;
            std::thread::sleep(std::time::Duration::from_millis(hold_ms));
            pointer.button(now_ms(), code, wl_pointer::ButtonState::Released);
            pointer.frame();
        }
        Action::Wheel { steps } => {
            // The source after the first axis event: niri opens the frame on
            // an axis event and drops a source that arrives before one.
            pointer.axis_discrete(
                now_ms(),
                wl_pointer::Axis::VerticalScroll,
                NOTCH * f64::from(steps),
                steps,
            );
            pointer.axis_source(wl_pointer::AxisSource::Wheel);
            pointer.frame();
        }
    }
    // The compositor has handled every request above once this returns, so
    // the next invocation's events land after these.
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("roundtrip: {e}"))?;
    pointer.destroy();
    manager.destroy();
    conn.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("caustics-pointer: {err}");
            ExitCode::FAILURE
        }
    }
}
