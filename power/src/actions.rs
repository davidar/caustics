//! What an `Action` frame on stdin means, and what it does.
//!
//! The dispatch is a **pure table** — `(row id, ACTION_*, value)` to an [`Op`],
//! and an [`Op`] to the [`Call`] it would make — so the whole mapping is
//! testable without a bus, without a compositor, and without ever performing
//! a real power action. [`perform`] is the only thing here that touches
//! either, and it takes the [`Call`] the table already decided. The brightness and power-mode ops need the loop's state (which backlight, which profile is at which index), so the loop performs those itself
//! ([`crate::serve`]); the table still says which row means what.

use zbus::Connection;

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_SET_VALUE,
};

use crate::rows::{
    MAX_PROFILES, ROW_BRIGHTNESS, ROW_LOCK, ROW_LOG_OUT, ROW_PROFILE_BASE, ROW_RESTART,
    ROW_SETTINGS, ROW_SHUTDOWN, ROW_SUSPEND,
};
use crate::{logind, niri, witness};
use caustics_sidecar::settings::{self, Panel};

/// A row of either menu that performs something.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Lock,
    LogOut,
    Suspend,
    Restart,
    Shutdown,
    /// The brightness slider moved: the row's own raw units, as sent.
    Brightness(f32),
    /// A power mode's radio row: its index in the daemon's `Profiles`.
    Profile(u32),
    /// "Power settings…".
    Settings,
}

/// What an [`Op`] would do, named rather than done: the shape a test can
/// assert against without a system bus or a running compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// `org.freedesktop.login1.Manager.<member>(interactive = false)`.
    Logind(&'static str),
    /// The Lock walk: our user's sessions, `Lock()` on every user-class,
    /// non-tty one. Not a single method call, which is why it is its own
    /// variant.
    LogindLockWalk,
    /// The one line written to `$NIRI_SOCKET`.
    Niri(&'static str),
    /// `login1.Session.SetBrightness("backlight", <name>, v)` on
    /// `session/auto` ([`crate::backlight`]).
    SetBrightness,
    /// `ActiveProfile = <name>` on `org.freedesktop.UPower.PowerProfiles`,
    /// or `net.hadess.PowerProfiles` where only the old name is owned
    /// ([`crate::profiles::ENDPOINTS`]).
    SetProfile,
    /// The settings program, detached, on this panel.
    Settings(Panel),
}

/// The settings page "Power settings…" opens; which program shows it is
/// `caustics_sidecar::settings`' choice.
pub const SETTINGS_PANEL: Panel = Panel::Power;

/// Whether a settings program can be named for [`SETTINGS_PANEL`]; the
/// picture leaves the settings row out when none can.
pub fn settings_available() -> bool {
    settings::settings_command(SETTINGS_PANEL).is_some()
}

impl Op {
    /// The name this op wears in every witness line.
    pub fn name(self) -> &'static str {
        match self {
            Op::Lock => "lock",
            Op::LogOut => "log-out",
            Op::Suspend => "suspend",
            Op::Restart => "restart",
            Op::Shutdown => "shutdown",
            Op::Brightness(_) => "brightness",
            Op::Profile(_) => "power-mode",
            Op::Settings => "settings",
        }
    }

    /// The call this op makes.
    pub fn call(self) -> Call {
        match self {
            Op::Lock => Call::LogindLockWalk,
            Op::LogOut => Call::Niri(niri::QUIT),
            Op::Suspend => Call::Logind("Suspend"),
            Op::Restart => Call::Logind("Reboot"),
            Op::Shutdown => Call::Logind("PowerOff"),
            Op::Brightness(_) => Call::SetBrightness,
            Op::Profile(_) => Call::SetProfile,
            Op::Settings => Call::Settings(SETTINGS_PANEL),
        }
    }
}

/// The dispatch table. `None` is a row that performs nothing — the battery,
/// the separators, the state row (disabled), either chip (its click opens its
/// menu and never comes back), or a kind of action this row has no meaning
/// for. Only the slider takes `ACTION_SET_VALUE`, and only activate performs
/// anything else.
pub fn op_for(row: u32, kind: u32, value: f32) -> Option<Op> {
    if kind == ACTION_SET_VALUE {
        return (row == ROW_BRIGHTNESS).then_some(Op::Brightness(value));
    }
    if kind != ACTION_ACTIVATE {
        return None;
    }
    match row {
        ROW_LOCK => Some(Op::Lock),
        ROW_LOG_OUT => Some(Op::LogOut),
        ROW_SUSPEND => Some(Op::Suspend),
        ROW_RESTART => Some(Op::Restart),
        ROW_SHUTDOWN => Some(Op::Shutdown),
        ROW_SETTINGS => Some(Op::Settings),
        row if (ROW_PROFILE_BASE..ROW_PROFILE_BASE + MAX_PROFILES as u32).contains(&row) => {
            Some(Op::Profile(row - ROW_PROFILE_BASE))
        }
        _ => None,
    }
}

/// `CAUSTICS_POWER_DRY_RUN`: every action becomes one witness line and nothing
/// else. The sidecar's own setting, read once at startup and never the bar's
/// business — it is what lets an unattended test click Shut Down.
pub const DRY_RUN_ENV: &str = "CAUSTICS_POWER_DRY_RUN";

/// Read strictly: exactly `1` arms it, so an empty or leftover value cannot
/// silently disarm the power menu of a real session.
pub fn dry_run(value: Option<&str>) -> bool {
    value == Some("1")
}

/// `ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`: the bar telling the sidecar that
/// owns a menu that it opened or closed (docs/abi.md, *Action*). The power
/// button's menu has no use for either; the battery's starts and stops the
/// once-a-second brightness read ([`crate::serve`]). Neither is witnessed: one
/// line per menu open would be journal noise for nothing.
pub fn is_menu_notice(kind: u32) -> bool {
    kind == ACTION_MENU_OPEN || kind == ACTION_MENU_CLOSE
}

/// [`perform`] on its own task, so the loop goes on while an action
/// waits: a logind that never answers does not freeze the menu, and the
/// brightness and the battery keep publishing beside it. `set_profile` in the
/// loop has the same shape.
pub fn spawn_perform(conn: Option<Connection>, op: Op, dry: bool) {
    tokio::spawn(async move { perform(conn.as_ref(), op, dry).await });
}

/// Do one of the power menu's ops, or open the settings page, and write down
/// what came back. Exactly one line per outcome: `caustics-power: <action>: ok`, or
/// `caustics-power: <action>: <error>`. The brightness and power-mode ops are the
/// loop's, which knows the backlight's name and the profiles' order.
pub async fn perform(conn: Option<&Connection>, op: Op, dry: bool) {
    if dry {
        witness(&format!("{}: dry run", op.name()));
        return;
    }
    let outcome = match op.call() {
        // The compositor's socket, not the bus: Log Out works on a machine
        // with no system bus at all, and fails on one with no niri. Blocking
        // socket I/O, so off the runtime's one thread.
        Call::Niri(_) => match tokio::task::spawn_blocking(niri::quit).await {
            Ok(outcome) => outcome,
            Err(e) => Err(format!("the niri call did not finish ({e})")),
        },
        Call::Settings(panel) => {
            witness(&format!(
                "{}: {}",
                op.name(),
                settings::open_settings(panel)
            ));
            Ok(())
        }
        Call::SetBrightness | Call::SetProfile => {
            Err("the loop performs this one, not the table".to_owned())
        }
        Call::Logind(_) | Call::LogindLockWalk => match conn {
            None => Err("there is no system bus".to_owned()),
            Some(conn) => match op.call() {
                Call::LogindLockWalk => logind::lock(conn).await,
                Call::Logind(member) => logind::call(conn, member).await,
                _ => unreachable!("the other arms are above"),
            },
        },
    };
    match outcome {
        // A spawn's own line already said where it went.
        Ok(()) if matches!(op, Op::Settings) => {}
        Ok(()) => witness(&format!("{}: ok", op.name())),
        Err(why) => witness(&format!("{}: {why}", op.name())),
    }
}
