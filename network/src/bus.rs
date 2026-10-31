//! The only module here that touches the system bus: the two walks that read
//! the picture, the two calls a row can make, and the watch that turns NM's
//! signals into snapshots.
//!
//! **Signal wakes a re-read**, as `caustics-power`'s UPower module does: one match
//! rule for every signal NM sends, and every one of them means "read
//! `GetManagedObjects` again and rebuild". At this scale a full re-read — one
//! round trip, about 20 KB for a machine on Wi-Fi with a tunnel up — is
//! cheaper to get right than tracking thirty-odd properties across seven
//! interfaces, and NM is quiet: idle, it was measured at five signals in
//! forty-five seconds, all of them `Strength` on the associated access point,
//! every one of which the bucket absorbs and publishes nothing for.
//!
//! The re-read is **debounced** all the same, because an activation is not idle
//! traffic: joining a network walks a device through six states and emits
//! dozens of signals in under a second, and one re-read at the end of that
//! burst is the same picture as thirty.
//!
//! `GetSettings` is cached per connection path and dropped whenever a signal
//! arrives from the `Settings` object or a connection under it — a new profile,
//! a removed one, or an edited one (`VersionId` bumps, which is a
//! `PropertiesChanged` on that path). Nothing else invalidates it: a profile's
//! id and SSID cannot change without NM saying so.
//!
//! **No stream is ever held across a bus call.** zbus's socket reader awaits
//! room in every stream's channel before it reads the next message, so a
//! stream left unpolled through `snapshot`'s round trips fills at 64 signals
//! — an activation's burst is enough — and parks the reader with the reply
//! behind it (docs/architecture.md, *Rules*). Both streams here are
//! handed to [`caustics_sidecar::forward`] the moment they exist: a task that
//! awaits nothing but the stream and sends what the loop needs — whether a
//! signal touched `Settings`, whether NM came back — down an unbounded channel.

use std::collections::HashMap;
use std::time::Duration;

use caustics_sidecar::forward;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zbus::zvariant::ObjectPath;
use zbus::{Connection, MatchRule, MessageStream};

use crate::actions::{self, Join};
use crate::model::{self, Snapshot};
use crate::nm::{self, Settings, SettingsByPath};
use crate::witness;

/// How long a burst of signals is allowed to keep arriving before the re-read.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
/// How long to wait before trying NM again when it is not on the bus at all and
/// `NameOwnerChanged` has not said otherwise. Belt to the signal's braces: a
/// bus that dropped our match rule would otherwise leave the chip empty
/// forever.
pub const RETRY: Duration = Duration::from_secs(30);

/// `Device.Wireless.RequestScan(a{sv})`, the one method this sidecar calls on a
/// device rather than on the manager. As NM 1.56 has it:
/// it takes an options dictionary (empty asks NM to scan
/// every channel it may) and returns nothing.
///
/// **A scan is a read of the air**, which is why it is here beside the two walks
/// and not beside `ActivateConnection`: it joins nothing, leaves nothing and
/// changes no setting. It does take the radio off-channel for a moment, which is
/// why it happens only while the menu is open (`crate::serve`).
#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wireless {
    fn request_scan(&self, options: HashMap<&str, &zbus::zvariant::Value<'_>>) -> zbus::Result<()>;
}

/// `RequestScan({})` on one Wi-Fi device.
///
/// NM refuses a scan that follows another too closely — `NotAllowed`,
/// *"Scanning not allowed immediately following previous scan"* — and that is
/// not an error: it means the list this sidecar is about to read is seconds old
/// already. The caller witnesses whatever comes back and carries on.
pub async fn request_scan(conn: &Connection, device: &str) -> Result<(), String> {
    let proxy = WirelessProxy::builder(conn)
        .path(device)
        .map_err(|e| format!("{device}: {e}"))?
        .build()
        .await
        .map_err(|e| format!("{device}: {e}"))?;
    proxy
        .request_scan(HashMap::new())
        .await
        .map_err(|e| e.to_string())
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait Manager {
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_wireless_enabled(&self, value: bool) -> zbus::Result<()>;

    /// `(connection, device, specific_object) -> active`. `"/"` for the last
    /// two lets NM pick both, which is what makes a row one call.
    fn activate_connection(
        &self,
        connection: &ObjectPath<'_>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// `(connection, device, specific_object, options) -> (path, active,
    /// result)`: add a profile and activate it, in one call.
    #[allow(clippy::type_complexity)]
    fn add_and_activate_connection2(
        &self,
        connection: HashMap<&str, HashMap<&str, zbus::zvariant::Value<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
        options: HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<(
        zbus::zvariant::OwnedObjectPath,
        zbus::zvariant::OwnedObjectPath,
        HashMap<String, zbus::zvariant::OwnedValue>,
    )>;
}

// ---------------------------------------------------------------------------
// The calls a row makes
// ---------------------------------------------------------------------------

/// `WirelessEnabled = value`. A polkit denial is an error we witness, not a
/// dialogue a bar has anywhere to show.
pub async fn set_wireless_enabled(conn: &Connection, value: bool) -> Result<(), String> {
    let manager = ManagerProxy::new(conn)
        .await
        .map_err(|e| format!("no NetworkManager ({e})"))?;
    manager
        .set_wireless_enabled(value)
        .await
        .map_err(|e| e.to_string())
}

/// `ActivateConnection(connection, "/", "/")`. The returned active-connection
/// path is witnessed and then forgotten: the picture comes back by signal.
pub async fn activate(conn: &Connection, connection: &str) -> Result<(), String> {
    let manager = ManagerProxy::new(conn)
        .await
        .map_err(|e| format!("no NetworkManager ({e})"))?;
    let path = ObjectPath::try_from(connection).map_err(|e| format!("{connection}: {e}"))?;
    let none = ObjectPath::try_from(nm::NO_PATH).expect("\"/\" is an object path");
    manager
        .activate_connection(&path, &none, &none)
        .await
        .map(|active| witness(&format!("activating {}", active.as_str())))
        .map_err(|e| e.to_string())
}

/// How long a join is followed before it is left to NetworkManager: past
/// NetworkManager's own wait for its secret agent, so a prompt left open
/// is followed to its end.
pub const JOIN_WATCH: Duration = Duration::from_secs(300);
/// How often the activation's `State` is read beside its signal: the object
/// can go before a signal for it is seen, and gone is an answer too.
pub const JOIN_POLL: Duration = Duration::from_secs(2);

/// `AddAndActivateConnection2` for a network in range: a new profile, kept on
/// disk, activated on this device and access point. Then the activation is
/// followed on a task of its own ([`follow_join`]).
pub async fn join(conn: &Connection, join: &Join) -> Result<(), String> {
    let manager = ManagerProxy::new(conn)
        .await
        .map_err(|e| format!("no NetworkManager ({e})"))?;
    let device =
        ObjectPath::try_from(join.device.as_str()).map_err(|e| format!("{}: {e}", join.device))?;
    let ap = ObjectPath::try_from(join.ap.as_str()).map_err(|e| format!("{}: {e}", join.ap))?;
    let options = HashMap::from([("persist", zbus::zvariant::Value::from("disk"))]);
    let (profile, active, _) = manager
        .add_and_activate_connection2(actions::profile(join), &device, &ap, options)
        .await
        .map_err(|e| e.to_string())?;
    witness(&format!(
        "join {:?}: added {}, activating {}",
        join.label,
        profile.as_str(),
        active.as_str()
    ));
    tokio::spawn(follow_join(
        conn.clone(),
        join.label.clone(),
        profile.as_str().to_owned(),
        active.as_str().to_owned(),
    ));
    Ok(())
}

/// Follow a join's activation until it connects or ends. **A join that ends
/// before it ever connected deletes the profile it added**: a wrong password
/// given up on, a dismissed prompt (the agent's `UserCanceled`, which
/// NetworkManager reports as no secrets), a supplicant that failed, so no
/// dead profile is left behind to be offered as a known network. One that
/// connected once is kept, whatever happens to it after. The signal stream is
/// forwarded the moment it exists (docs/architecture.md, *Rules*).
pub async fn follow_join(conn: Connection, label: String, profile: String, active: String) {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(nm::SERVICE)
        .and_then(|b| b.path(active.as_str()))
        .and_then(|b| b.interface(IF_ACTIVE))
        .and_then(|b| b.member("StateChanged"))
        .map(|b| b.build());
    let (tx, mut changes) = unbounded_channel();
    let stream = match rule {
        Ok(rule) => match MessageStream::for_match_rule(rule, &conn, None).await {
            Ok(stream) => Some(forward(stream, tx, |msg: zbus::Result<zbus::Message>| {
                msg.ok()?.body().deserialize::<(u32, u32)>().ok()
            })),
            Err(e) => {
                witness(&format!("join {label:?}: following {active}: {e}"));
                None
            }
        },
        Err(e) => {
            witness(&format!("join {label:?}: following {active}: {e}"));
            None
        }
    };
    // While the stream is open its arm is listened to; once it ends (or never
    // began), the poll is all there is.
    let mut open = stream.is_some();
    let deadline = tokio::time::sleep(JOIN_WATCH);
    tokio::pin!(deadline);
    let reason = loop {
        tokio::select! {
            () = &mut deadline => {
                witness(&format!(
                    "join {label:?}: not connected after {} s; the profile is kept",
                    JOIN_WATCH.as_secs()
                ));
                return;
            }
            change = changes.recv(), if open => match change {
                Some((nm::ACTIVE_ACTIVATED, _)) => break None,
                Some((nm::ACTIVE_DEACTIVATED, reason)) => break Some(Some(reason)),
                Some(_) => {}
                None => open = false,
            },
            () = tokio::time::sleep(JOIN_POLL) => match active_state(&conn, &active).await {
                Some(nm::ACTIVE_ACTIVATED) => break None,
                Some(nm::ACTIVE_DEACTIVATED) | None => break Some(None),
                Some(_) => {}
            },
        }
    };
    let Some(reason) = reason else {
        witness(&format!(
            "join {label:?}: connected; the profile {profile} is kept"
        ));
        return;
    };
    let why = reason.map_or_else(
        || "gone".to_owned(),
        |r| format!("reason {}", state_reason(r)),
    );
    match delete(&conn, &profile).await {
        Ok(()) => witness(&format!(
            "join {label:?}: ended before it connected ({why}); deleted the profile {profile}"
        )),
        Err(e) => witness(&format!(
            "join {label:?}: ended before it connected ({why}); deleting {profile}: {e}"
        )),
    }
}

/// The activation's `State`, or `None` when the object is gone.
async fn active_state(conn: &Connection, active: &str) -> Option<u32> {
    let reply = conn
        .call_method(
            Some(nm::SERVICE),
            active,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(IF_ACTIVE, "State"),
        )
        .await
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    u32::try_from(value).ok()
}

/// `Settings.Connection.Delete()` on a profile this sidecar added.
async fn delete(conn: &Connection, profile: &str) -> Result<(), String> {
    conn.call_method(Some(nm::SERVICE), profile, Some(CONNECTION), "Delete", &())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// `NMActiveConnectionStateReason` as a word, for the witness.
pub fn state_reason(reason: u32) -> String {
    let word = match reason {
        0 => "unknown",
        1 => "none",
        2 => "user-disconnected",
        3 => "device-disconnected",
        4 => "service-stopped",
        5 => "ip-config-invalid",
        6 => "connect-timeout",
        7 => "service-start-timeout",
        8 => "service-start-failed",
        9 => "no-secrets",
        10 => "login-failed",
        11 => "connection-removed",
        12 => "dependency-failed",
        13 => "device-realize-failed",
        14 => "device-removed",
        _ => return reason.to_string(),
    };
    word.to_owned()
}

const IF_ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";

// ---------------------------------------------------------------------------
// Reading the picture
// ---------------------------------------------------------------------------

/// One `GetManagedObjects`, the `GetSettings` replies the cache is missing, and
/// the pure build over the two of them.
pub async fn snapshot(conn: &Connection, cache: &mut SettingsByPath) -> Result<Snapshot, String> {
    let body = call(
        conn,
        nm::OBJECT_MANAGER_PATH,
        OBJECT_MANAGER,
        "GetManagedObjects",
    )
    .await?;
    let managed = nm::parse_managed_objects(&body)?;

    // Only the paths NM still lists, and only the ones not already known: a
    // handful of profiles, and this asks about none of them on a signal
    // that did not touch the Settings object.
    cache.retain(|path, _| managed.connections.contains(path));
    for path in &managed.connections {
        if cache.contains_key(path) {
            continue;
        }
        match call(conn, path, CONNECTION, "GetSettings").await {
            Ok(body) => match nm::parse_settings(&body) {
                Ok(settings) => {
                    cache.insert(path.clone(), guard(path, settings, &body));
                }
                Err(e) => witness(&format!("{path}: GetSettings: {e}")),
            },
            // A profile deleted between the two calls, or one this uid may not
            // read: one witness, and it is simply not a row.
            Err(e) => witness(&format!("{path}: GetSettings: {e}")),
        }
    }
    Ok(model::build(&managed, cache))
}

const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";

/// One method call, returning the **raw reply body** — because
/// `nm::parse_managed_objects` and `nm::parse_settings` are the one door the
/// tests come through, and the live path has to go through it too or the tests
/// prove nothing.
async fn call(
    conn: &Connection,
    path: &str,
    interface: &str,
    member: &str,
) -> Result<Vec<u8>, String> {
    let reply = conn
        .call_method(Some(nm::SERVICE), path, Some(interface), member, &())
        .await
        .map_err(|e| e.to_string())?;
    Ok(reply.body().data().bytes().to_vec())
}

/// NM omits secrets from `GetSettings` by contract (its API docs say so, and
/// a WPA-PSK profile's reply bears it out). This checks rather than
/// trusts: a body that ever carried one gets a witness naming the keys, and the
/// settings are used anyway — the sidecar keeps nothing but the id, the UUID,
/// the type and the SSID, so a secret in the reply is a line in the journal and
/// not a leak.
fn guard(path: &str, settings: Settings, body: &[u8]) -> Settings {
    match nm::secret_keys(body) {
        Ok(keys) if !keys.is_empty() => {
            witness(&format!(
                "{path}: GetSettings carried {}; ignored",
                keys.join(", ")
            ));
        }
        _ => {}
    }
    settings
}

// ---------------------------------------------------------------------------
// The watch
// ---------------------------------------------------------------------------

/// Follow NM for as long as anybody is listening. `None` down the channel is
/// "NetworkManager is not there", which is the empty frame; the watch never
/// exits on NM's absence, because a sidecar that exits only makes the bar
/// restart it into a backoff and fill the journal.
pub async fn watch(conn: Connection, out: UnboundedSender<Option<Snapshot>>) {
    let mut cache: SettingsByPath = HashMap::new();
    loop {
        match follow(&conn, &out, &mut cache).await {
            // The receiver is gone: so is the reason to be here.
            Stop::Done => return,
            Stop::Absent => {
                witness("absent: networkmanager");
                if out.send(None).is_err() {
                    return;
                }
                cache.clear();
                if !appeared(&conn).await {
                    return;
                }
            }
        }
    }
}

enum Stop {
    /// Nobody is listening any more.
    Done,
    /// NM is not on the bus, or stopped answering.
    Absent,
}

/// Read, publish, wait for a signal, repeat. Returns when NM goes away or the
/// receiver does.
async fn follow(
    conn: &Connection,
    out: &UnboundedSender<Option<Snapshot>>,
    cache: &mut SettingsByPath,
) -> Stop {
    // Every signal from NM, on any path: each one means "re-read".
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(nm::SERVICE)
        .map(|b| b.build());
    let rule = match rule {
        Ok(rule) => rule,
        Err(e) => {
            witness(&format!("networkmanager: match rule: {e}"));
            return Stop::Absent;
        }
    };
    // Each signal becomes one word, whether it touched the stored connections:
    // the one thing `settle` reads from the body.
    let (touched, mut signals) = unbounded_channel();
    let _signals = match MessageStream::for_match_rule(rule, conn, None).await {
        Ok(stream) => forward(stream, touched, |msg: zbus::Result<zbus::Message>| {
            Some(touches_settings(msg.as_ref().ok()))
        }),
        Err(e) => {
            witness(&format!("networkmanager: {e}"));
            return Stop::Absent;
        }
    };

    // Announced after the first read, not before: `AddMatch` succeeds for a
    // sender that is not on the bus, so the read is the presence test and
    // "watching" must not be said ahead of it.
    let mut announced = false;
    loop {
        match snapshot(conn, cache).await {
            Ok(state) => {
                if !announced {
                    witness(&format!("networkmanager: watching {}", nm::SERVICE));
                    announced = true;
                }
                if out.send(Some(state)).is_err() {
                    return Stop::Done;
                }
            }
            // NM not running answers ServiceUnknown here rather than at
            // match-rule time, and so does NM stopping under us.
            Err(e) => {
                witness(&format!("networkmanager: {e}"));
                return Stop::Absent;
            }
        }

        // Wait for the first signal of a burst, then let the burst finish.
        let first = tokio::select! {
            signal = signals.recv() => signal,
            // Nothing has happened for a long time. Re-read anyway: one round
            // trip every thirty seconds is the price of never being wrong
            // about a match rule the bus quietly dropped.
            () = tokio::time::sleep(RETRY) => continue,
        };
        let Some(first) = first else {
            witness("networkmanager: the signal stream ended");
            return Stop::Absent;
        };
        settle(&mut signals, cache, first).await;
    }
}

/// Let a burst of signals finish for [`DEBOUNCE`], noting whether any of them
/// touched the stored connections — in which case the [`Settings`] cache is
/// stale and the next read walks it again. `first` is the first signal's word.
async fn settle(signals: &mut UnboundedReceiver<bool>, cache: &mut SettingsByPath, first: bool) {
    let mut stale = first;
    let deadline = tokio::time::sleep(DEBOUNCE);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => break,
            signal = signals.recv() => match signal {
                Some(touched) => stale |= touched,
                None => break,
            },
        }
    }
    if stale {
        cache.clear();
    }
}

/// A signal from the `Settings` object or from a connection under it: a new
/// profile, a removed one, or an edited one.
fn touches_settings(msg: Option<&zbus::Message>) -> bool {
    msg.and_then(|msg| {
        msg.header()
            .path()
            .map(|p| p.as_str().starts_with(nm::SETTINGS_PATH))
    })
    .unwrap_or(false)
}

/// Wait for NM's name to be owned again, or for [`RETRY`] to pass — whichever
/// comes first, because a `NameOwnerChanged` we missed must not be a chip that
/// never comes back. `false` means the bus itself is gone.
async fn appeared(conn: &Connection) -> bool {
    let dbus = match zbus::fdo::DBusProxy::new(conn).await {
        Ok(dbus) => dbus,
        Err(e) => {
            witness(&format!("name watch: {e}"));
            tokio::time::sleep(RETRY).await;
            return true;
        }
    };
    // Forwarded like the NM stream, though nothing here makes a call while it
    // waits: one rule for every stream is easier to keep than an argument
    // about each one.
    let (back, mut appeared) = unbounded_channel();
    let _changes = match dbus.receive_name_owner_changed().await {
        Ok(changes) => forward(changes, back, |signal: zbus::fdo::NameOwnerChanged| {
            let args = signal.args().ok()?;
            (args.name.as_str() == nm::SERVICE && !args.new_owner.is_none()).then_some(())
        }),
        Err(e) => {
            witness(&format!("name watch: {e}"));
            tokio::time::sleep(RETRY).await;
            return true;
        }
    };
    tokio::select! {
        () = tokio::time::sleep(RETRY) => true,
        signal = appeared.recv() => {
            if signal.is_some() {
                witness("networkmanager: back on the bus");
            }
            // `None`: the stream ended, and the bus with it.
            signal.is_some()
        }
    }
}
