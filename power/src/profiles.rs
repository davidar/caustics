//! Power modes: the power-profiles interface on the system bus
//! (power-profiles-daemon, or `tuned-ppd`, which implements it); a machine
//! with neither has no rows.
//!
//! **Two names, one shape**: power-profiles-daemon 0.20 moved to
//! `org.freedesktop.UPower.PowerProfiles` and keeps `net.hadess.PowerProfiles`
//! for old clients; tuned-ppd owns both names, with `Profiles` and
//! `ActiveProfile` readable on each. The new name is asked first and the
//! old one is the fallback ([`ENDPOINTS`]); the properties are the same on
//! both.
//!
//! `Profiles` gives the set, `ActiveProfile` the checked one, and a click sets
//! `ActiveProfile`. Four signal streams wake the reader, each handed to a
//! [`caustics_sidecar::drain`] the moment it exists (docs/architecture.md, *Rules*): each name's `PropertiesChanged`, and the bus's `NameOwnerChanged` for each
//! name — so a daemon that appears or vanishes while the sidecar runs
//! re-reads, and the frame is sent again with or without the rows.
//!
//! Both names are commonly D-Bus-activatable, so every read after the first asks
//! `NameHasOwner` before it reads ([`crate::has_owner`]): a read of an unowned
//! name would start the daemon again, and a stopped tuned-ppd would never be
//! seen as absent.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use zbus::Connection;
use zbus::names::InterfaceName;
use zbus::zvariant::{OwnedValue, Value};

use crate::witness;

/// One name the daemon answers on: the service, its object, its interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub service: &'static str,
    pub path: &'static str,
    pub interface: &'static str,
}

/// power-profiles-daemon's name since 0.20, and tuned-ppd's too.
pub const UPOWER: Endpoint = Endpoint {
    service: "org.freedesktop.UPower.PowerProfiles",
    path: "/org/freedesktop/UPower/PowerProfiles",
    interface: "org.freedesktop.UPower.PowerProfiles",
};

/// The name before it, still exported by both daemons.
pub const HADESS: Endpoint = Endpoint {
    service: "net.hadess.PowerProfiles",
    path: "/net/hadess/PowerProfiles",
    interface: "net.hadess.PowerProfiles",
};

/// The names in the order they are asked.
pub const ENDPOINTS: [Endpoint; 2] = [UPOWER, HADESS];

/// The three profiles every daemon offers, by the names the bus uses.
pub const POWER_SAVER: &str = "power-saver";
pub const BALANCED: &str = "balanced";
pub const PERFORMANCE: &str = "performance";

/// What the radio rows are made of: the set in the daemon's order, and the
/// checked one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profiles {
    pub names: Vec<String>,
    pub active: String,
}

/// A profile's row label: a plain word for each of the three standard
/// names, and any other profile by its own name.
pub fn label(name: &str) -> &str {
    match name {
        POWER_SAVER => "Battery",
        BALANCED => "Balanced",
        PERFORMANCE => "Performance",
        other => other,
    }
}

/// A profile's row icon, when Adwaita has one (all three are
/// under `/usr/share/icons/Adwaita/symbolic/status/`).
pub fn icon(name: &str) -> Option<&'static str> {
    match name {
        POWER_SAVER => Some("power-profile-power-saver-symbolic"),
        BALANCED => Some("power-profile-balanced-symbolic"),
        PERFORMANCE => Some("power-profile-performance-symbolic"),
        _ => None,
    }
}

/// `org.freedesktop.DBus.Properties` on one endpoint's object. Explicit reads
/// after every wake, like the battery's: the signal says "read again", the
/// read cannot go stale.
async fn properties(
    conn: &Connection,
    at: Endpoint,
) -> zbus::Result<zbus::fdo::PropertiesProxy<'static>> {
    zbus::fdo::PropertiesProxy::builder(conn)
        .destination(at.service)?
        .path(at.path)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

/// Read the set and the active one from one endpoint, in one `GetAll`. `None`
/// is "not here": the name is not on the bus, or it answered something that
/// is not a profile list.
pub async fn read_from(conn: &Connection, at: Endpoint) -> Option<Profiles> {
    let proxy = properties(conn, at).await.ok()?;
    let all = proxy
        .get_all(InterfaceName::from_static_str_unchecked(at.interface))
        .await
        .ok()?;
    parse(&all)
}

/// The profiles out of `GetAll`'s answer. `Profiles` is `aa{sv}`: each dict
/// carries `Profile` (and `Driver`, `PlatformDriver`, `CpuDriver`, which a
/// menu has no use for).
pub fn parse(all: &HashMap<String, OwnedValue>) -> Option<Profiles> {
    let list = all.get("Profiles")?.try_clone().ok()?;
    let list = <Vec<HashMap<String, OwnedValue>>>::try_from(list).ok()?;
    let names: Vec<String> = list
        .iter()
        .filter_map(|dict| dict.get("Profile"))
        .filter_map(|value| value.try_clone().ok())
        .filter_map(|value| String::try_from(value).ok())
        .collect();
    if names.is_empty() {
        return None;
    }
    let active = all
        .get("ActiveProfile")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok())
        .unwrap_or_default();
    Some(Profiles { names, active })
}

/// Read from the first endpoint that answers, in [`ENDPOINTS`]' order, and
/// say which. With `may_start` false an endpoint is asked only when its name
/// has an owner, so the read never starts a daemon that was stopped.
pub async fn read(conn: &Connection, may_start: bool) -> Option<(Profiles, Endpoint)> {
    for at in ENDPOINTS {
        if !may_start && !crate::has_owner(conn, at.service).await {
            continue;
        }
        if let Some(profiles) = read_from(conn, at).await {
            return Some((profiles, at));
        }
    }
    None
}

/// Set the active profile on the first endpoint whose name has an owner. The
/// outcome is the caller's witness; the rows change when the daemon's
/// `PropertiesChanged` says it took.
pub async fn set_active(conn: &Connection, name: &str) -> Result<(), String> {
    let mut at = None;
    for endpoint in ENDPOINTS {
        if crate::has_owner(conn, endpoint.service).await {
            at = Some(endpoint);
            break;
        }
    }
    let at = at.ok_or_else(|| "no power-profiles daemon on the bus".to_owned())?;
    let proxy = properties(conn, at)
        .await
        .map_err(|e| format!("no {} ({e})", at.service))?;
    let interface = InterfaceName::from_static_str_unchecked(at.interface);
    proxy
        .set(interface, "ActiveProfile", Value::from(name))
        .await
        .map_err(|e| e.to_string())
}

/// Watch the daemon, sending what it says on every wake (the loop drops a
/// repeat). Every stream is drained before the first read; the task ends
/// only when every stream has, or when the receiver has gone.
pub async fn watch(conn: Connection, profiles: UnboundedSender<Option<Profiles>>) {
    let changed = Arc::new(Notify::new());
    let mut streams = Vec::new();

    for at in ENDPOINTS {
        // The daemon coming and going under this name.
        match zbus::fdo::DBusProxy::new(&conn).await {
            Ok(bus) => match bus
                .receive_name_owner_changed_with_args(&[(0, at.service)])
                .await
            {
                Ok(stream) => streams.push(caustics_sidecar::drain(stream, changed.clone())),
                Err(e) => witness(&format!("power-profiles: no NameOwnerChanged ({e})")),
            },
            Err(e) => witness(&format!("power-profiles: no bus proxy ({e})")),
        }
        // Its properties changing, whoever owns the name at the time.
        match properties(&conn, at).await {
            Ok(proxy) => match proxy.receive_properties_changed().await {
                Ok(stream) => streams.push(caustics_sidecar::drain(stream, changed.clone())),
                Err(e) => witness(&format!("power-profiles: no PropertiesChanged ({e})")),
            },
            Err(e) => witness(&format!("power-profiles: {e}")),
        }
    }

    let mut first = true;
    let mut was: Option<Option<Endpoint>> = None;
    loop {
        // The first read may start an activatable daemon, as any client's
        // would at login; a later one reads only a daemon that is there.
        let now = read(&conn, std::mem::replace(&mut first, false)).await;
        let from = now.as_ref().map(|(_, at)| *at);
        if was != Some(from) {
            match from {
                Some(at) => witness(&format!("power-profiles: watching {}", at.service)),
                None => witness("absent: power-profiles"),
            }
            was = Some(from);
        }
        if profiles.send(now.map(|(p, _)| p)).is_err() {
            return;
        }
        if streams.is_empty() {
            // Nothing will ever wake this loop again; what was read stands.
            return;
        }
        changed.notified().await;
    }
}
