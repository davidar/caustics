//! The screen's brightness: read from sysfs, set through logind.
//!
//! **Which device**: the best by the kernel's own `type` file —
//! `firmware` (ACPI, which knows the platform's quirks), then `platform`, then
//! `raw` — and among equals the one whose `device` link is an internal panel's
//! connector (`card0-eDP-1`, LVDS, DSI), so a `ddcci6` beside
//! `intel_backlight` never dims the external monitor instead; the name breaks
//! what is left, so it is the same one every time.
//!
//! **Reading** is two files under `/sys/class/backlight/<name>/`:
//! `brightness` over `max_brightness`. sysfs does not inotify,
//! and the brightness keys change it behind the menu, so the loop reads once
//! when the battery's menu opens and once a second while it stays open, and
//! otherwise only when the kernel's uevent says the device changed
//! (`crate::uevent`), which is also what raises the on-screen display.
//!
//! **Setting** is `org.freedesktop.login1.Session.SetBrightness("backlight",
//! <name>, value)` on `/org/freedesktop/login1/session/auto` over the system
//! bus: logind writes the file for the session's owner, so there is no root, no
//! udev rule and no group. `session/auto` is the caller's session, or — for a
//! process that has none, like a `systemd --user` unit — the user's display
//! session.
//!
//! The root of the class directory is a parameter so `power/tests/` can stand
//! a temp dir in for sysfs; the program passes [`SYSFS_BACKLIGHT`].

use std::path::{Path, PathBuf};

use tokio::sync::watch;
use zbus::Connection;

use crate::witness;

/// Where the kernel lists backlights.
pub const SYSFS_BACKLIGHT: &str = "/sys/class/backlight";

/// The subsystem argument of `SetBrightness`: the sysfs class name.
pub const SUBSYSTEM: &str = "backlight";

/// The session object the call goes to.
pub const SESSION_AUTO: &str = "/org/freedesktop/login1/session/auto";

/// The lowest value a drag may set on a `firmware` or `platform` device: a
/// slider pulled to its left end dims the panel rather than turning it off,
/// because a black screen is a bad place to find a slider again.
/// cosmic-settings-daemon keeps a floor for the same reason. Those devices
/// count in a handful of ACPI levels, and 1 is the dimmest lit one.
pub const MIN_SET: u32 = 1;

/// The same floor on a `raw` device, as a fraction of its maximum: raw units
/// are the panel's PWM duty, `amdgpu_bl0` counts to 255 and some panels to
/// 400000, and one unit of those is off in all but name.
pub const RAW_FLOOR_PERCENT: u32 = 1;

/// The kernel's `type` file: which interface drives the light. The order is
/// the preference, best first, as GNOME's and systemd's have it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Firmware,
    Platform,
    Raw,
    /// No `type` file, or a word the kernel may add later: last, and floored
    /// like a raw device, which is the cautious reading.
    Unknown,
}

impl Kind {
    fn read(dir: &Path) -> Kind {
        match std::fs::read_to_string(dir.join("type"))
            .as_deref()
            .map(str::trim)
        {
            Ok("firmware") => Kind::Firmware,
            Ok("platform") => Kind::Platform,
            Ok("raw") => Kind::Raw,
            _ => Kind::Unknown,
        }
    }
}

/// One backlight device, by its directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlight {
    /// The directory's name — what `SetBrightness` takes.
    pub name: String,
    pub dir: PathBuf,
    pub kind: Kind,
    /// Its `device` link names an internal panel's connector.
    pub internal: bool,
}

/// Is this `device` link's target an internal panel's DRM connector? The
/// link ends in `card0-eDP-1`, `card1-LVDS-1`, `card0-DSI-1`; a `ddcci`
/// device's ends in an i2c address, and an ACPI one's in a PCI address.
fn is_internal_connector(target: &Path) -> bool {
    let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    ["-eDP-", "-LVDS-", "-DSI-"]
        .iter()
        .any(|kind| name.contains(kind))
}

/// One reading: the slider's position is `value` over `max`, in the device's
/// own raw units, which is also what `SetBrightness` takes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brightness {
    pub value: u32,
    pub max: u32,
}

impl Backlight {
    /// The best backlight under `root` (the module's *Which device*), or
    /// `None` when there is none (a virtual machine, a desktop).
    pub fn find(root: &Path) -> Option<Backlight> {
        std::fs::read_dir(root)
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .map(|name| Backlight::at(root, name))
            .min_by(|a, b| a.rank().cmp(&b.rank()))
    }

    /// The device called `name` under `root`, its two facts read.
    pub fn at(root: &Path, name: String) -> Backlight {
        let dir = root.join(&name);
        let kind = Kind::read(&dir);
        let internal =
            std::fs::read_link(dir.join("device")).is_ok_and(|t| is_internal_connector(&t));
        Backlight {
            name,
            dir,
            kind,
            internal,
        }
    }

    /// Smaller is better: the type, then an internal panel, then the name.
    fn rank(&self) -> (Kind, bool, &str) {
        (self.kind, !self.internal, &self.name)
    }

    /// The lowest value a drag may set on this device, given its maximum:
    /// [`MIN_SET`] on a firmware or platform device, [`RAW_FLOOR_PERCENT`] of
    /// `max` (rounded up, at least 1) on any other.
    pub fn floor(&self, max: u32) -> u32 {
        let floor = match self.kind {
            Kind::Firmware | Kind::Platform => MIN_SET,
            Kind::Raw | Kind::Unknown => (u64::from(max) * u64::from(RAW_FLOOR_PERCENT))
                .div_ceil(100)
                .max(1) as u32,
        };
        floor.min(max)
    }

    /// Read both files. `None` when either is missing, unreadable, or the
    /// maximum is zero (a device with nothing to slide).
    pub fn read(&self) -> Option<Brightness> {
        let max = read_u32(&self.dir.join("max_brightness"))?;
        let value = read_u32(&self.dir.join("brightness"))?;
        (max > 0).then_some(Brightness {
            value: value.min(max),
            max,
        })
    }
}

/// Find the best backlight under `root` and read it: what the loop does when the battery's menu opens and on every poll while it is open, so a backlight that appears or vanishes while the
/// sidecar runs is seen the next time anyone looks.
pub fn sample(root: &Path) -> Option<(Backlight, Brightness)> {
    let backlight = Backlight::find(root)?;
    let brightness = backlight.read()?;
    Some((backlight, brightness))
}

fn read_u32(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// A slider value — the row's own raw units, as the bar sends it back — as the
/// value to set: rounded, at most `max`, at least `floor`
/// ([`Backlight::floor`]).
pub fn to_raw(value: f32, max: u32, floor: u32) -> u32 {
    let floor = floor.min(max);
    if !value.is_finite() {
        return floor;
    }
    (value.round().max(0.0) as u32).clamp(floor, max)
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1/session/auto"
)]
trait Session {
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

/// One `SetBrightness` call. The outcome is the caller's witness.
pub async fn set(conn: &Connection, name: &str, value: u32) -> Result<(), String> {
    let session = SessionProxy::new(conn)
        .await
        .map_err(|e| format!("no logind ({e})"))?;
    session
        .set_brightness(SUBSYSTEM, name, value)
        .await
        .map_err(|e| e.to_string())
}

/// The slider's own lane, the shape `caustics-audio`'s volume setter has: a drag
/// is one `ACTION_SET_VALUE` per pointer motion, a hundred and more a second,
/// and only the latest can matter. The task makes one call at a time and, when
/// it comes back, takes whatever the newest value is by then.
///
/// A failure is witnessed once per run of failures, not once per tick.
pub fn spawn_setter(conn: Connection) -> watch::Sender<Option<(String, u32)>> {
    let (tx, mut rx) = watch::channel(None::<(String, u32)>);
    tokio::spawn(async move {
        let mut failing = false;
        while rx.changed().await.is_ok() {
            let Some((name, value)) = rx.borrow_and_update().clone() else {
                continue;
            };
            match set(&conn, &name, value).await {
                Ok(()) => {
                    if failing {
                        witness("brightness: ok again");
                    }
                    failing = false;
                }
                Err(why) => {
                    if !failing {
                        witness(&format!("brightness: {why}"));
                    }
                    failing = true;
                }
            }
        }
    });
    tx
}
