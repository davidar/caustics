//! logind on the **system** bus: the three power actions that are not this
//! sidecar's to invent, and the Lock walk that is.
//!
//! Suspend, Restart and Shut Down are one method call each on
//! `org.freedesktop.login1.Manager`. Lock is a walk rather than a call: our
//! own user object, its sessions, and `Lock()` on every one of them that is a
//! **user-class, non-tty** session — a graphical session locks, the tty that
//! started something does not, and a `greeter` session belongs to somebody
//! else.
//!
//! `Lock()` only makes logind emit the session's `Lock` signal; what locks is
//! whichever locker listens for it, and a session may have none. So the walk
//! then reads each session's `LockedHint` for a few seconds
//! ([`LOCKED_HINT_WAIT`]): a hint that turns true is witnessed as locked, and
//! one that stays false is witnessed as what it is, a signal nobody may have
//! answered, rather than as a lock.
//!
//! Nothing here decides and nothing here retries: the caustic decided that a
//! row was clicked, [`crate::actions`] said which action that is, and this
//! makes the call and writes down what came back. A polkit denial, a logind
//! that is not there, a session that will not lock — each is one witness line
//! and the sidecar goes on.

use std::time::Duration;

use zbus::Connection;
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedObjectPath;

use crate::witness;

/// `false`: no polkit agent, no prompt. A desktop session that may suspend is
/// allowed to without asking, and one that may not gets an error we witness
/// rather than a dialogue a bar has nowhere to show.
pub const INTERACTIVE: bool = false;

/// How long the Lock walk reads a session's `LockedHint` after its `Lock()`
/// before saying the hint never turned: long enough for a locker to put its
/// surfaces up and set it, short enough to be one click's answer.
pub const LOCKED_HINT_WAIT: Duration = Duration::from_secs(3);

/// How often the hint is read inside [`LOCKED_HINT_WAIT`].
const LOCKED_HINT_STEP: Duration = Duration::from_millis(100);

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    fn get_user(&self, uid: u32) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
trait User {
    /// `(session id, object path)` for every session of this user.
    #[zbus(property)]
    fn sessions(&self) -> zbus::Result<Vec<(String, OwnedObjectPath)>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    /// `user`, `greeter` or `lock-screen`.
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
    /// `wayland`, `x11` or `tty`.
    #[zbus(property, name = "Type")]
    fn kind(&self) -> zbus::Result<String>;
    /// Whether the session's locker says it is locked. Read uncached
    /// ([`CacheProperties::No`]), so each read is logind's answer now.
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
    fn lock(&self) -> zbus::Result<()>;
}

/// One `login1.Manager` method, by the name [`crate::actions::Call`] gives it.
/// The outcome is the one line the caller witnesses.
pub async fn call(conn: &Connection, member: &str) -> Result<(), String> {
    let manager = ManagerProxy::new(conn)
        .await
        .map_err(|e| format!("no logind ({e})"))?;
    let result = match member {
        "Suspend" => manager.suspend(INTERACTIVE).await,
        "Reboot" => manager.reboot(INTERACTIVE).await,
        "PowerOff" => manager.power_off(INTERACTIVE).await,
        other => return Err(format!("{other} is not a logind method this sidecar makes")),
    };
    result.map_err(|e| e.to_string())
}

/// Lock every user-class, non-tty session of our own uid, then wait up to
/// [`LOCKED_HINT_WAIT`] for each one's `LockedHint` and say what it showed.
pub async fn lock(conn: &Connection) -> Result<(), String> {
    let uid = uid()?;
    let manager = ManagerProxy::new(conn)
        .await
        .map_err(|e| format!("no logind ({e})"))?;
    let user = manager.get_user(uid).await.map_err(|e| e.to_string())?;
    let user = UserProxy::builder(conn)
        .path(user)
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    let sessions = user.sessions().await.map_err(|e| e.to_string())?;

    // The sessions whose `Lock()` logind took, with the words their witness
    // line wears.
    let mut asked = Vec::new();
    for (id, path) in sessions {
        let building = SessionProxy::builder(conn)
            .path(path)
            .map(|b| b.cache_properties(CacheProperties::No).build());
        let session = match building {
            Ok(building) => match building.await {
                Ok(proxy) => proxy,
                Err(e) => {
                    witness(&format!("lock: session {id}: {e}"));
                    continue;
                }
            },
            Err(e) => {
                witness(&format!("lock: session {id}: {e}"));
                continue;
            }
        };
        let class = session.class().await.unwrap_or_default();
        let kind = session.kind().await.unwrap_or_default();
        if class != "user" || kind == "tty" {
            witness(&format!("lock: skipping session {id} ({class}/{kind})"));
            continue;
        }
        match session.lock().await {
            Ok(()) => asked.push((format!("{id} ({class}/{kind})"), session)),
            Err(e) => witness(&format!("lock: session {id}: {e}")),
        }
    }
    if asked.is_empty() {
        return Err(format!("no user session of uid {uid} took it"));
    }

    // Every session's hint is read on the same ticks, so two sessions with
    // no locker cost one wait, not two.
    let deadline = tokio::time::Instant::now() + LOCKED_HINT_WAIT;
    loop {
        let mut waiting = Vec::new();
        for (name, session) in asked {
            match session.locked_hint().await {
                Ok(true) => witness(&format!("lock: locked session {name}")),
                Ok(false) => waiting.push((name, session)),
                Err(e) => witness(&format!(
                    "lock: asked session {name} to lock; its LockedHint could not be read ({e})"
                )),
            }
        }
        asked = waiting;
        if asked.is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            for (name, _) in &asked {
                witness(&unanswered(name));
            }
            break;
        }
        tokio::time::sleep(LOCKED_HINT_STEP).await;
    }
    Ok(())
}

/// The line for a session whose `LockedHint` stayed false through
/// [`LOCKED_HINT_WAIT`]. Not an error: the row did all it can, which is ask
/// logind. Either no locker listens to the session's `Lock` signal, or the
/// one that does never sets the hint (swaylock and hyprlock do not;
/// caustics-lock, GNOME's and KDE's lockers do).
pub fn unanswered(session: &str) -> String {
    format!(
        "lock: asked session {session} to lock, and its LockedHint stayed false for {} s: \
         either no locker listens to logind's Lock on this session, or the locker does not \
         set the hint (swaylock and hyprlock do not); caustics-lock does \
         (canvas/README.md, Enabling the lock)",
        LOCKED_HINT_WAIT.as_secs()
    )
}

/// Log Out where there is no niri: `Session.Terminate()` on this uid's own
/// graphical session, found as the lock and polkit sidecars find theirs
/// (`caustics_sidecar::session`). `Ok` carries which session and how it was
/// found, for the witness.
pub async fn terminate_own_session(conn: &Connection) -> Result<String, String> {
    use caustics_sidecar::session;

    let (id, how) = session::session_id(conn).await?;
    let proxy = session::session(conn, &id)
        .await
        .map_err(|e| format!("session {id} ({how}): {e}"))?;
    proxy
        .terminate()
        .await
        .map_err(|e| format!("session {id} ({how}): {e}"))?;
    Ok(format!("ended session {id}, found by {how}"))
}

/// Our own uid, without a `libc` dependency: `/proc/self`'s owner is the
/// process's **effective** uid, and `std::os::unix` reads it. That is the
/// real one for this sidecar, which is never setuid; a process whose two
/// differed would lock the effective user's sessions.
fn uid() -> Result<u32, String> {
    use std::os::unix::fs::MetadataExt as _;

    std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .map_err(|e| format!("/proc/self: {e}"))
}
