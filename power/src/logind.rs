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
//! Nothing here decides and nothing here retries: the caustic decided that a
//! row was clicked, [`crate::actions`] said which action that is, and this
//! makes the call and writes down what came back. A polkit denial, a logind
//! that is not there, a session that will not lock — each is one witness line
//! and the sidecar goes on.

use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

use crate::witness;

/// `false`: no polkit agent, no prompt. A desktop session that may suspend is
/// allowed to without asking, and one that may not gets an error we witness
/// rather than a dialogue a bar has nowhere to show.
pub const INTERACTIVE: bool = false;

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

/// Lock every user-class, non-tty session of our own uid.
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

    let mut locked = 0;
    for (id, path) in sessions {
        let session = match SessionProxy::builder(conn).path(path).map(|b| b.build()) {
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
            Ok(()) => {
                locked += 1;
                witness(&format!("lock: locked session {id} ({class}/{kind})"));
            }
            Err(e) => witness(&format!("lock: session {id}: {e}")),
        }
    }
    if locked == 0 {
        return Err(format!("no user session of uid {uid} took it"));
    }
    Ok(())
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
