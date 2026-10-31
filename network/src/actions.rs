//! What an `Action` frame on stdin means, and what it does.
//!
//! The dispatch is a **pure table** — the picture plus `(row id, ACTION_*)` to an
//! [`Op`], and an [`Op`] to the [`Call`] it would make — so the whole mapping
//! is testable without a bus and without ever joining, leaving or switching off
//! a network. [`perform`] is the only thing here that touches NM, and it takes
//! the [`Call`] the table already decided.

use std::collections::HashMap;

use zbus::Connection;
use zbus::zvariant::Value;

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE,
};

use crate::model::{Security, Snapshot};
use crate::{bus, rows, witness};

/// Something a row of this applet does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Set `WirelessEnabled` to this.
    Wireless(bool),
    /// Join this known network.
    Activate {
        /// The `Settings.Connection` object path.
        path: String,
        /// Only for the witness line.
        label: String,
    },
    /// Join a network in range that no profile claims: NetworkManager adds
    /// a profile and activates it, and asks its secret agent for anything
    /// secret.
    Join(Join),
    /// A network in range this row cannot join (802.1X, WEP): one witness,
    /// nothing else.
    Unjoinable {
        /// Only for the witness line.
        label: String,
        /// What it needs, for the witness line.
        needs: &'static str,
    },
    /// Ask every Wi-Fi device what is in range. The menu just opened,
    /// or fifteen seconds have passed with it still open.
    ///
    /// The paths come from the picture, so a machine with no radio produces this
    /// op with an empty list and one witness line saying so.
    Scan { devices: Vec<String> },
    /// The menu closed: stop asking. Makes no call at all — the timer it turns
    /// off lives in the sidecar's own loop.
    ScanStop,
}

/// A network to join, as [`Op::Join`] carries it: everything the call names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Join {
    /// The SSID's bytes, the new profile's `802-11-wireless.ssid`.
    pub ssid: Vec<u8>,
    /// The SSID as text: the new profile's `connection.id`, the title the
    /// secret agent's prompt wears, and the witness's.
    pub label: String,
    /// The Wi-Fi device's object path.
    pub device: String,
    /// The access point's object path.
    pub ap: String,
    pub security: Security,
}

/// The new profile [`Join`] asks for (`AddAndActivateConnection2`'s
/// `connection`): a name, an SSID, infrastructure mode, and for a secured
/// network the key management and **no secret**, so that NetworkManager
/// asks its secret agent for it.
pub fn profile(join: &Join) -> HashMap<&'static str, HashMap<&'static str, Value<'static>>> {
    let mut profile = HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from(join.label.clone())),
                ("type", Value::from(crate::nm::TYPE_WIFI)),
            ]),
        ),
        (
            crate::nm::TYPE_WIFI,
            HashMap::from([
                ("ssid", Value::from(join.ssid.clone())),
                ("mode", Value::from("infrastructure")),
            ]),
        ),
    ]);
    if let Some(key_mgmt) = join.security.key_mgmt() {
        profile.insert(
            "802-11-wireless-security",
            HashMap::from([("key-mgmt", Value::from(key_mgmt))]),
        );
    }
    profile
}

/// What an [`Op`] would do, named rather than done: the shape a test asserts
/// against without a system bus and without touching the machine's connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// `org.freedesktop.NetworkManager.WirelessEnabled = value`.
    SetWirelessEnabled(bool),
    /// `ActivateConnection(connection, "/", "/")` — NM picks the device and
    /// the access point, which is the whole reason this sidecar does not.
    ActivateConnection(String),
    /// `AddAndActivateConnection2(profile(join), device, ap, {persist:
    /// "disk"})`, then a watch on the activation that deletes the profile if
    /// it never connected.
    AddAndActivate(Join),
    /// Nothing on the bus: a witness saying what the network needs.
    Decline(&'static str),
    /// `Device.Wireless.RequestScan({})`, once per Wi-Fi device. A read of the
    /// air: it joins nothing and changes no setting, which is why the live test
    /// is allowed to drive it on a machine somebody is using.
    RequestScan(Vec<String>),
    /// Nothing on the bus. [`Op::ScanStop`]'s whole effect is in the loop.
    StopScanning,
}

impl Op {
    /// The name this op wears in its witness line.
    pub fn name(&self) -> String {
        match self {
            Op::Wireless(true) => "wi-fi on".to_owned(),
            Op::Wireless(false) => "wi-fi off".to_owned(),
            Op::Activate { label, .. } => format!("activate {label:?}"),
            Op::Join(join) => format!("join {:?}", join.label),
            Op::Unjoinable { label, .. } => format!("join {label:?}"),
            Op::Scan { .. } => "scan".to_owned(),
            Op::ScanStop => "scan stop".to_owned(),
        }
    }

    /// The call this op makes.
    pub fn call(&self) -> Call {
        match self {
            Op::Wireless(to) => Call::SetWirelessEnabled(*to),
            Op::Activate { path, .. } => Call::ActivateConnection(path.clone()),
            Op::Join(join) => Call::AddAndActivate(join.clone()),
            Op::Unjoinable { needs, .. } => Call::Decline(needs),
            Op::Scan { devices } => Call::RequestScan(devices.clone()),
            Op::ScanStop => Call::StopScanning,
        }
    }
}

/// The dispatch table: pure, and a function of the picture because every op
/// here is relative to it — the Wi-Fi switch toggles whatever it currently is,
/// and a network row is only a row while that profile exists.
///
/// `None` is a row that performs nothing: the bar item itself on a plain click
/// (`RFLAG_HAS_MENU`, so the click opens the menu and never comes back), a
/// separator, a network that is already active (radio semantics), any network
/// while Wi-Fi is off (the rows are `RFLAG_DISABLED`; see `Snapshot::joinable`
/// for why "not in range" is not a reason), or a kind of action this applet
/// has no meaning for.
pub fn op_for(state: &Snapshot, row: u32, kind: u32) -> Option<Op> {
    match (row, kind) {
        // The two notices the bar sends about the menu itself (docs/abi.md
        // *Action*). This is the one sidecar that wants them: NM ages its idle scan
        // list down to the associated access point, so a list of what is in
        // range is only true if something asked while the menu was up. The row
        // is the chip's, but it is the menu that is being talked about, so the
        // row is not matched on.
        (_, ACTION_MENU_OPEN) => Some(Op::Scan {
            devices: state.wifi.clone(),
        }),
        (_, ACTION_MENU_CLOSE) => Some(Op::ScanStop),
        // Middle-clicking the chip is the switch, without opening the menu.
        (rows::ITEM, ACTION_MIDDLE) | (rows::WIFI, ACTION_ACTIVATE) => {
            Some(Op::Wireless(!state.wireless_enabled))
        }
        (row, ACTION_ACTIVATE) => state
            .networks
            .iter()
            .find(|n| n.row == row)
            .filter(|n| !n.active && state.joinable(n))
            .map(|n| Op::Activate {
                path: n.path.clone(),
                label: n.label.clone(),
            })
            // A network in range that no profile claims: joined, while the
            // radio is on. NetworkManager adds the profile and asks its secret
            // agent (the canvas's prompt) for the passphrase, so the bar never
            // holds it. One that needs more than a passphrase says so and
            // does nothing.
            .or_else(|| {
                let visible = state.visible.iter().find(|v| v.row == row)?;
                if let Some(needs) = visible.security.unjoinable() {
                    return Some(Op::Unjoinable {
                        label: visible.label(),
                        needs,
                    });
                }
                state.wireless_enabled.then(|| {
                    Op::Join(Join {
                        ssid: visible.ssid.clone(),
                        label: visible.label(),
                        device: visible.device.clone(),
                        ap: visible.ap.clone(),
                        security: visible.security,
                    })
                })
            }),
        _ => None,
    }
}

/// `CAUSTICS_NETWORK_DRY_RUN`: every action becomes one witness line and nothing
/// else. The sidecar's own setting, read once at startup and never the bar's
/// business — it is what lets an unattended test click a row
/// on a machine somebody is using.
pub const DRY_RUN_ENV: &str = "CAUSTICS_NETWORK_DRY_RUN";

/// Read strictly: exactly `1` arms it, so an empty or leftover value cannot
/// silently disarm the menu of a real session.
pub fn dry_run(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Do it, and write down what came back. Exactly one line per outcome:
/// `caustics-network: <action>: ok`, or `caustics-network: <action>: <error>`.
///
/// An activation takes seconds and the reply is not the interesting part — the
/// next signal republishes the picture — so it is spawned rather than awaited,
/// and the loop stays free to read the next action frame.
pub async fn perform(conn: Option<&Connection>, op: Op, dry: bool) {
    if dry {
        witness(&format!("{}: dry run", op.name()));
        return;
    }
    match op.call() {
        // Nothing on the bus: the loop has already dropped its timer.
        Call::StopScanning => {}
        // One `RequestScan` per Wi-Fi device, each witnessed. NM's `NotAllowed`
        // — "Scanning not allowed immediately following previous scan" — is one
        // of the expected outcomes and not an error: it means the list is
        // already seconds old.
        Call::RequestScan(devices) => match conn {
            None => witness(&format!("{}: there is no system bus", op.name())),
            Some(_) if devices.is_empty() => {
                witness(&format!("{}: no wi-fi device", op.name()));
            }
            Some(conn) => {
                for device in &devices {
                    match bus::request_scan(conn, device).await {
                        Ok(()) => witness(&format!("{}: requested on {device}", op.name())),
                        Err(why) => witness(&format!("{}: {device}: {why}", op.name())),
                    }
                }
            }
        },
        Call::SetWirelessEnabled(to) => match conn {
            None => witness(&format!("{}: there is no system bus", op.name())),
            Some(conn) => {
                let outcome = bus::set_wireless_enabled(conn, to).await;
                report(&op, outcome);
            }
        },
        Call::Decline(needs) => witness(&format!(
            "{}: needs a profile ({needs}); not joinable from the bar yet",
            op.name()
        )),
        // Spawned like an activation: the call returns at once, and the watch
        // after it lasts as long as the activation does.
        Call::AddAndActivate(join) => match conn {
            None => witness(&format!("{}: there is no system bus", op.name())),
            Some(conn) => {
                let conn = conn.clone();
                tokio::spawn(async move {
                    report(&op, bus::join(&conn, &join).await);
                });
            }
        },
        Call::ActivateConnection(path) => match conn {
            None => witness(&format!("{}: there is no system bus", op.name())),
            Some(conn) => {
                let conn = conn.clone();
                tokio::spawn(async move {
                    report(&op, bus::activate(&conn, &path).await);
                });
            }
        },
    }
}

fn report(op: &Op, outcome: Result<(), String>) {
    match outcome {
        Ok(()) => witness(&format!("{}: ok", op.name())),
        Err(why) => witness(&format!("{}: {why}", op.name())),
    }
}
