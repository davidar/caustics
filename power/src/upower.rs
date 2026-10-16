//! UPower's `DisplayDevice` on the **system** bus.
//!
//! `DisplayDevice` is UPower's own composite of whatever batteries the machine
//! has, so nothing here enumerates devices and nothing here picks one. A
//! machine without a battery (a desktop, a virtual machine) has the object
//! but reports `IsPresent = false`, which is `None` and a frame with no
//! battery row; a machine without UPower at all gets the same plus one
//! `absent: upower` witness, and a UPower that leaves or arrives while the
//! sidecar runs is seen both ways.
//!
//! The icon is UPower's own `IconName`, passed through as a themed name for
//! the bar to resolve. It already encodes charging, so the frame carries no
//! charging flag of its own.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use zbus::Connection;
use zbus::names::InterfaceName;
use zbus::zvariant::OwnedValue;

use crate::witness;

pub const SERVICE: &str = "org.freedesktop.UPower";
pub const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

/// What the icon name falls back to when UPower gives an empty one — a
/// battery that is there but nameless still gets a chip.
pub const FALLBACK_ICON: &str = "battery-symbolic";

/// What the battery row is made of. Everything the frame needs and nothing
/// else: the percentage becomes the label and the level, the icon name goes
/// through to the bar's icon theme, and the state with its two times becomes
/// the menu's first row ([`crate::rows::state_line`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    pub percent: f32,
    pub icon: String,
    /// UPower's `State`: one of the `STATE_*` below.
    pub state: u32,
    /// Seconds, as UPower gives them; 0 is "UPower does not know yet".
    pub time_to_empty: i64,
    pub time_to_full: i64,
}

/// UPower's `Device.State` values (its own enum, `up-types.h`).
pub const STATE_UNKNOWN: u32 = 0;
pub const STATE_CHARGING: u32 = 1;
pub const STATE_DISCHARGING: u32 = 2;
pub const STATE_EMPTY: u32 = 3;
pub const STATE_FULLY_CHARGED: u32 = 4;
pub const STATE_PENDING_CHARGE: u32 = 5;
pub const STATE_PENDING_DISCHARGE: u32 = 6;

/// The interface the six properties are on: `IsPresent`, `Percentage`,
/// `IconName`, `State`, `TimeToEmpty`, `TimeToFull`.
pub const INTERFACE: &str = "org.freedesktop.UPower.Device";

/// Watch the display device, sending the battery on every change, and UPower
/// itself coming and going. Two signal streams wake the reader, each
/// handed to a [`caustics_sidecar::drain`] the moment it exists (docs/architecture.md,
/// *Rules*): the device's `PropertiesChanged`, and the bus's
/// `NameOwnerChanged` for UPower — zbus follows a restarted daemon's new owner
/// but never yields the change itself, so without it an upowerd restart would leave
/// the chip on the dead instance's reading, and a UPower absent at start would mean
/// no chip until the sidecar restarts. The profiles watcher's shape.
///
/// UPower is D-Bus-activatable, and a read of a name nobody owns starts it:
/// every read after the first asks `NameHasOwner` first, so a UPower someone
/// stopped stays stopped and is witnessed as absent ([`crate::has_owner`]).
///
/// The task ends only when both streams have, or when the receiver has gone;
/// the sidecar keeps running either way, because the power menu is not the
/// battery's.
pub async fn watch(conn: Connection, batteries: UnboundedSender<Option<Battery>>) {
    let changed = Arc::new(Notify::new());

    // UPower coming and going.
    let owners = match zbus::fdo::DBusProxy::new(&conn).await {
        Ok(bus) => match bus
            .receive_name_owner_changed_with_args(&[(0, SERVICE)])
            .await
        {
            Ok(stream) => Some(caustics_sidecar::drain(stream, changed.clone())),
            Err(e) => {
                witness(&format!("upower: no NameOwnerChanged ({e})"));
                None
            }
        },
        Err(e) => {
            witness(&format!("upower: no bus proxy ({e})"));
            None
        }
    };

    // The display device's properties: one proxy both reads them (`GetAll`,
    // explicit after every wake, so nothing can go stale) and is subscribed
    // to their changes. The stream goes to a drain the moment it exists: a
    // stream held unpolled across a read is one UPower burst away from
    // parking zbus's socket reader — and with it every call on this
    // connection, logind's included. Every
    // change means "read again", so a burst is one re-read.
    let device = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination(SERVICE)
        .and_then(|b| b.path(DISPLAY_DEVICE))
        .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No).build());
    let device = match device {
        Ok(building) => building.await,
        Err(e) => Err(e),
    };
    let device = match device {
        Ok(device) => device,
        Err(e) => {
            // Not a bus round trip: a builder that cannot be built never will.
            witness("absent: upower");
            witness(&format!("upower: {e}"));
            let _ = batteries.send(None);
            return;
        }
    };
    let props = match device.receive_properties_changed().await {
        Ok(stream) => Some(caustics_sidecar::drain(stream, changed.clone())),
        Err(e) => {
            witness(&format!("upower: no PropertiesChanged ({e})"));
            None
        }
    };

    let mut first = true;
    let mut was: Option<bool> = None;
    loop {
        // The first read may start an activatable UPower, as any client's
        // would at login; a later one only reads a UPower that is there.
        let may_start = std::mem::replace(&mut first, false);
        let now = if may_start || crate::has_owner(&conn, SERVICE).await {
            read(&device).await
        } else {
            Err(format!("{SERVICE} has no owner"))
        };
        let present = now.is_ok();
        if was != Some(present) {
            match &now {
                Ok(_) => witness(&format!("upower: watching {SERVICE}{DISPLAY_DEVICE}")),
                Err(why) => {
                    witness("absent: upower");
                    witness(&format!("upower: {why}"));
                }
            }
            was = Some(present);
        }
        if batteries.send(now.unwrap_or(None)).is_err() {
            return;
        }
        if owners.is_none() && props.is_none() {
            // Nothing will ever wake this loop again; what was read stands.
            return;
        }
        changed.notified().await;
    }
}

/// One `GetAll`, not six `Get`s: one round trip a wake. `Err` is
/// UPower not answering; `Ok(None)` is a UPower with no battery to show.
async fn read(device: &zbus::fdo::PropertiesProxy<'_>) -> Result<Option<Battery>, String> {
    let interface = InterfaceName::from_static_str_unchecked(INTERFACE);
    let all = device.get_all(interface).await.map_err(|e| e.to_string())?;
    Ok(battery(&all))
}

/// The battery out of the display device's properties, as `GetAll` gives
/// them. A property this UPower does not answer, or answers with another
/// type, reads as "unknown" and "no time", which is the percentage alone; a
/// percentage that is not finite reads as 0 (a NaN survives a clamp).
pub fn battery(all: &HashMap<String, OwnedValue>) -> Option<Battery> {
    fn get<T: TryFrom<OwnedValue>>(all: &HashMap<String, OwnedValue>, name: &str) -> Option<T> {
        all.get(name)
            .and_then(|value| value.try_clone().ok())
            .and_then(|v| T::try_from(v).ok())
    }
    if !get::<bool>(all, "IsPresent").unwrap_or(false) {
        return None;
    }
    let percent = get::<f64>(all, "Percentage").unwrap_or(0.0) as f32;
    let percent = if percent.is_finite() {
        percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let icon = get::<String>(all, "IconName").unwrap_or_default();
    Some(Battery {
        percent,
        icon: if icon.is_empty() {
            FALLBACK_ICON.to_owned()
        } else {
            icon
        },
        state: get::<u32>(all, "State").unwrap_or(STATE_UNKNOWN),
        time_to_empty: get::<i64>(all, "TimeToEmpty").unwrap_or(0),
        time_to_full: get::<i64>(all, "TimeToFull").unwrap_or(0),
    })
}
