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
use crate::rows::{ROW_CHIP, ROW_POWER, device_row, nearby_row};
use crate::witness;

/// Something a row of this applet does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// The adapter's radio, to this value.
    Power { path: String, on: bool },
    /// `Connect()` on a device that is not connected.
    Connect { path: String, alias: String },
    /// `Disconnect()` on one that is.
    Disconnect { path: String, alias: String },
    /// `Pair()` on a device in range, then `Trusted = true` and `Connect()`.
    /// Whatever the pairing asks, the session's pairing agent asks.
    Pair { path: String, alias: String },
    /// `Adapter1.RemoveDevice` for a paired device: the pairing goes.
    Forget {
        adapter: String,
        path: String,
        alias: String,
    },
    /// Look for devices in range on one adapter: the menu opened, or an
    /// adapter powered while it is open.
    Discover { adapter: String },
    /// Stop looking on one adapter: the menu closed, or the sidecar is
    /// leaving.
    StopDiscovery { adapter: String },
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
    /// `Device1.Pair()`, then `Trusted = true`, then `Device1.Connect()`,
    /// each step only after the one before it succeeded.
    PairTrustConnect { path: String },
    /// `org.bluez.Adapter1.RemoveDevice(device)` on an adapter.
    RemoveDevice { adapter: String, device: String },
    /// `SetDiscoveryFilter` and `StartDiscovery` on an adapter.
    StartDiscovery { adapter: String },
    /// `StopDiscovery` on an adapter.
    StopDiscovery { adapter: String },
}

impl Op {
    /// The name this op wears in every witness line.
    pub fn name(&self) -> &'static str {
        match self {
            Op::Power { on: true, .. } => "power on",
            Op::Power { on: false, .. } => "power off",
            Op::Connect { .. } => "connect",
            Op::Disconnect { .. } => "disconnect",
            Op::Pair { .. } => "pair",
            Op::Forget { .. } => "forget",
            Op::Discover { .. } => "discover",
            Op::StopDiscovery { .. } => "stop discovery",
        }
    }

    /// The name with what it is about: `connect Pixel Buds`, `discover on
    /// /org/bluez/hci0`.
    pub fn describe(&self) -> String {
        match self {
            Op::Connect { alias, .. }
            | Op::Disconnect { alias, .. }
            | Op::Pair { alias, .. }
            | Op::Forget { alias, .. } => {
                format!("{} {alias}", self.name())
            }
            Op::Discover { adapter } | Op::StopDiscovery { adapter } => {
                format!("{} on {adapter}", self.name())
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
            Op::Pair { path, .. } => Call::PairTrustConnect { path: path.clone() },
            Op::Forget { adapter, path, .. } => Call::RemoveDevice {
                adapter: adapter.clone(),
                device: path.clone(),
            },
            Op::Discover { adapter } => Call::StartDiscovery {
                adapter: adapter.clone(),
            },
            Op::StopDiscovery { adapter } => Call::StopDiscovery {
                adapter: adapter.clone(),
            },
        }
    }
}

/// The dispatch table. `None` is a row that performs nothing — a separator, the
/// chip's own left click (which opens the menu and never comes back), a device
/// row while the radio is off (the row is `DISABLED`, so the bar should not
/// send it at all), a row id this frame never wrote, or a kind of action this
/// applet has no meaning for. The menu's own notices are [`Discovery`]'s, not
/// this table's.
///
/// A middle click on a paired device's row forgets it at once: the bar has no
/// confirmation, and a forgotten device can be paired again from the list of
/// devices in range.
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
        (_, ACTION_ACTIVATE) => {
            if !adapter.powered {
                return None;
            }
            if let Some(device) = state
                .nearby
                .iter()
                .find(|device| nearby_row(&device.address) == row)
            {
                return Some(Op::Pair {
                    path: device.path.clone(),
                    alias: device.alias.clone(),
                });
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
        (_, ACTION_MIDDLE) => {
            let device = state
                .devices
                .iter()
                .find(|device| device_row(&device.address) == row)?;
            Some(Op::Forget {
                adapter: adapter.path.clone(),
                path: device.path.clone(),
                alias: device.alias.clone(),
            })
        }
        _ => None,
    }
}

/// `ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`: the bar telling the sidecar that owns
/// a menu that it opened or closed (docs/abi.md, *Action*). They go to
/// [`Discovery`], not to [`op_for`].
pub fn is_menu_notice(kind: u32) -> bool {
    kind == ACTION_MENU_OPEN || kind == ACTION_MENU_CLOSE
}

/// **The radio looks for devices, and is seen by them, only while the menu
/// is open.** Discovery takes the radio's time from the devices already
/// connected (audio stutters on some adapters), and being discoverable is an
/// invitation that should not stand all day, so both start on
/// `ACTION_MENU_OPEN`, on every powered adapter, and stop on
/// `ACTION_MENU_CLOSE` and when the sidecar leaves. An adapter powered while
/// the menu is open is started too; one powered off has stopped by itself.
/// A pairing begun from the other device while the menu is open reaches the
/// session's pairing agent like one begun here. Pure: the ops it returns are
/// what [`perform`] does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    open: bool,
    /// The adapters this sidecar asked to discover, by path.
    started: Vec<String>,
}

impl Discovery {
    /// Whether the menu is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// A menu notice: what to start or stop. `None` for any other kind.
    pub fn notice(&mut self, kind: u32, state: &State) -> Option<Vec<Op>> {
        match kind {
            ACTION_MENU_OPEN => {
                self.open = true;
                Some(self.changed(state))
            }
            ACTION_MENU_CLOSE => Some(self.close()),
            _ => None,
        }
    }

    /// The picture moved: while the menu is open, start every powered
    /// adapter not yet started, and forget the ones that are no longer
    /// powered (BlueZ stops a discovery on an adapter that powers off).
    pub fn changed(&mut self, state: &State) -> Vec<Op> {
        if !self.open {
            return Vec::new();
        }
        self.started
            .retain(|adapter| state.radios.contains(adapter));
        let mut ops = Vec::new();
        for adapter in &state.radios {
            if !self.started.contains(adapter) {
                self.started.push(adapter.clone());
                ops.push(Op::Discover {
                    adapter: adapter.clone(),
                });
            }
        }
        ops
    }

    /// The menu closed, or the sidecar is leaving: stop what was started.
    pub fn close(&mut self) -> Vec<Op> {
        self.open = false;
        self.started
            .drain(..)
            .map(|adapter| Op::StopDiscovery { adapter })
            .collect()
    }
}

/// Do it, and write down what came back. Exactly one line per outcome, and
/// one per step of a pairing.
///
/// `Connect`, `Disconnect` and a pairing are **spawned**: each can take
/// seconds on a real device (a pairing as long as the person takes to answer
/// the canvas's prompt) and this is called from the sidecar's one select
/// loop, so the outcome is witnessed by the spawned task and the republish
/// comes from the next signal like any other change. A failed step is one
/// witness with BlueZ's error name, and the steps after it are not taken.
pub async fn perform(conn: Option<&Connection>, op: Op) {
    match op.call() {
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
        Call::PairTrustConnect { path } => {
            let Some(conn) = conn else {
                return witness(&format!("{}: there is no system bus", op.describe()));
            };
            let conn = conn.clone();
            let said = op.describe();
            witness(&format!("{said}: pairing"));
            tokio::spawn(async move {
                if let Err(why) = bluez::pair(&conn, &path).await {
                    return witness(&format!("{said}: {why}"));
                }
                witness(&format!("{said}: paired"));
                if let Err(why) = bluez::trust(&conn, &path).await {
                    return witness(&format!("{said}: trusting: {why}"));
                }
                witness(&format!("{said}: trusted"));
                match bluez::connect(&conn, &path).await {
                    Ok(()) => witness(&format!("{said}: connected")),
                    Err(why) => witness(&format!("{said}: connecting: {why}")),
                }
            });
        }
        Call::RemoveDevice { adapter, device } => {
            let Some(conn) = conn else {
                return witness(&format!("{}: there is no system bus", op.describe()));
            };
            witness(&format!("{}: removing {device}", op.describe()));
            match bluez::remove_device(conn, &adapter, &device).await {
                Ok(()) => witness(&format!("{}: removed", op.describe())),
                Err(why) => witness(&format!("{}: {why}", op.describe())),
            }
        }
        Call::StartDiscovery { adapter } => {
            let Some(conn) = conn else {
                return witness(&format!("{}: there is no system bus", op.describe()));
            };
            match bluez::start_discovery(conn, &adapter).await {
                Ok(partly) => {
                    let mut line = format!("{}: ok, discoverable", op.describe());
                    if let Some(filter) = partly.filter {
                        line.push_str(&format!(", with no filter ({filter})"));
                    }
                    if let Some(why) = partly.discoverable {
                        line = format!("{}: ok, not discoverable ({why})", op.describe());
                    }
                    witness(&line);
                }
                Err(why) => witness(&format!("{}: {why}", op.describe())),
            }
        }
        Call::StopDiscovery { adapter } => {
            let Some(conn) = conn else {
                return witness(&format!("{}: there is no system bus", op.describe()));
            };
            match bluez::stop_discovery(conn, &adapter).await {
                Ok(()) => witness(&format!("{}: ok", op.describe())),
                Err(why) => witness(&format!("{}: {why}", op.describe())),
            }
        }
    }
}
