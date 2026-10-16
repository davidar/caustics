//! What an `Action` frame on stdin means, and what it does.
//!
//! The dispatch is a **pure table** — `(state, row id, ACTION_*)` to an [`Op`],
//! and an [`Op`] to the [`Call`] it would make — so the whole mapping is
//! testable without a bus and without ever connecting or disconnecting
//! anything. [`perform`] is the only thing here that touches the bus, and it
//! takes the [`Call`] the table already decided.
//!
//! The table takes the state as well as the row, which is the one place this
//! differs from `caustics-power`'s `op_for(row, kind, value)`: a device row's id is
//! derived from its address and what a click means depends on whether that
//! device is connected, so the state is part of the lookup. It is still pure —
//! a [`crate::bluez::State`] is data — and the tests hand it one by hand.

use zbus::Connection;

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE,
};

use crate::bluez::{self, ADAPTER, CONNECT, DISCONNECT, State};
use crate::rows::{ROW_CHIP, ROW_POWER, ROW_SETTINGS, device_row};
use crate::witness;
use caustics_sidecar::settings::{self, Panel};

/// The settings page the last row opens, which is where pairing and
/// forgetting live; which program shows it is `caustics_sidecar::settings`'
/// choice.
pub const SETTINGS_PANEL: Panel = Panel::Bluetooth;

/// Whether a settings program can be named for [`SETTINGS_PANEL`]; the
/// picture leaves the settings row out when none can.
pub fn settings_available() -> bool {
    settings::settings_command(SETTINGS_PANEL).is_some()
}

/// Something a row of this applet does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// The adapter's radio, to this value.
    Power { path: String, on: bool },
    /// `Connect()` on a device that is not connected.
    Connect { path: String, alias: String },
    /// `Disconnect()` on one that is.
    Disconnect { path: String, alias: String },
    /// The settings program's bluetooth page.
    Settings,
}

/// What an [`Op`] would do, named rather than done: the shape a test can assert
/// against without a system bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// `org.freedesktop.DBus.Properties.Set(interface, "Powered", on)` on a
    /// path — the one property this sidecar writes.
    SetPowered {
        interface: &'static str,
        path: String,
        on: bool,
    },
    /// `org.bluez.Device1.<member>()` on a path.
    Device { member: &'static str, path: String },
    /// The settings program, detached, on this panel.
    Settings(Panel),
}

impl Op {
    /// The name this op wears in every witness line.
    pub fn name(&self) -> &'static str {
        match self {
            Op::Power { on: true, .. } => "power on",
            Op::Power { on: false, .. } => "power off",
            Op::Connect { .. } => "connect",
            Op::Disconnect { .. } => "disconnect",
            Op::Settings => "settings",
        }
    }

    /// The name with what it is about: `connect Pixel Buds`.
    pub fn describe(&self) -> String {
        match self {
            Op::Connect { alias, .. } | Op::Disconnect { alias, .. } => {
                format!("{} {alias}", self.name())
            }
            _ => self.name().to_owned(),
        }
    }

    /// The call this op makes.
    pub fn call(&self) -> Call {
        match self {
            Op::Power { path, on } => Call::SetPowered {
                interface: ADAPTER,
                path: path.clone(),
                on: *on,
            },
            Op::Connect { path, .. } => Call::Device {
                member: CONNECT,
                path: path.clone(),
            },
            Op::Disconnect { path, .. } => Call::Device {
                member: DISCONNECT,
                path: path.clone(),
            },
            Op::Settings => Call::Settings(SETTINGS_PANEL),
        }
    }
}

/// The dispatch table. `None` is a row that performs nothing — a separator, the
/// chip's own left click (which opens the menu and never comes back), a device
/// row while the radio is off (the row is `DISABLED`, so the bar should not
/// send it at all), a row id this frame never wrote, or a kind of action this
/// applet has no meaning for.
pub fn op_for(state: &State, row: u32, kind: u32) -> Option<Op> {
    let adapter = state.adapter.as_ref()?;
    let toggle = || Op::Power {
        path: adapter.path.clone(),
        on: !adapter.powered,
    };
    match (row, kind) {
        // A middle click on the chip is the radio, the same as the row.
        (ROW_CHIP, ACTION_MIDDLE) => Some(toggle()),
        (ROW_POWER, ACTION_ACTIVATE) => Some(toggle()),
        (ROW_SETTINGS, ACTION_ACTIVATE) => Some(Op::Settings),
        (_, ACTION_ACTIVATE) => {
            if !adapter.powered {
                return None;
            }
            let device = state
                .devices
                .iter()
                .find(|device| device_row(&device.address) == row)?;
            let (path, alias) = (device.path.clone(), device.alias.clone());
            Some(if device.connected {
                Op::Disconnect { path, alias }
            } else {
                Op::Connect { path, alias }
            })
        }
        _ => None,
    }
}

/// `ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`: the bar telling the sidecar that owns
/// a menu that it opened or closed (docs/abi.md, *Action*). This applet has
/// nothing to do with either — it is told, it is not asked — so the loop drops
/// them before the `nothing to do` witness: one line per menu open, on every
/// applet, would be journal noise for nothing.
pub fn is_menu_notice(kind: u32) -> bool {
    kind == ACTION_MENU_OPEN || kind == ACTION_MENU_CLOSE
}

/// Do it, and write down what came back. Exactly one line per outcome.
///
/// `Connect` and `Disconnect` are **spawned**: either can take several seconds
/// on a real device and this is called from the sidecar's one select loop, so
/// the outcome is witnessed by the spawned task and the republish comes from the
/// next `PropertiesChanged` like any other change.
pub async fn perform(conn: Option<&Connection>, op: Op) {
    match op.call() {
        Call::Settings(panel) => witness(&format!("settings: {}", settings::open_settings(panel))),
        Call::SetPowered { path, on, .. } => {
            let Some(conn) = conn else {
                return witness(&format!("{}: there is no system bus", op.describe()));
            };
            match bluez::set_powered(conn, &path, on).await {
                Ok(()) => witness(&format!("{}: ok", op.describe())),
                Err(why) => witness(&format!("{}: {why}", op.describe())),
            }
        }
        Call::Device { member, path } => {
            let Some(conn) = conn else {
                return witness(&format!("{}: there is no system bus", op.describe()));
            };
            let conn = conn.clone();
            let said = op.describe();
            tokio::spawn(async move {
                match bluez::device_method(&conn, &path, member).await {
                    Ok(()) => witness(&format!("{said}: ok")),
                    Err(why) => witness(&format!("{said}: {why}")),
                }
            });
        }
    }
}
