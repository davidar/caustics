//! Who this process is and which login session it speaks for: the user's uid
//! and name, and logind's session on the **system** bus (feature `session`).
//!
//! The sinks of the canvas are each a session's: the polkit agent registers
//! for one, and the lock sidecar watches one's `Lock` signal and keeps its
//! `LockedHint`. Both find it the same way, so the lookup is written once.
//!
//! The session is `$XDG_SESSION_ID` when the session set it, else logind's
//! session for this process (`GetSessionByPID`), else the user's display
//! session (`User.Display`): a process under the user's service manager is
//! in no session of its own, and the display session is the one a graphical
//! prompt or a lock screen is for.

use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
pub trait Manager {
    #[zbus(name = "GetSessionByPID")]
    fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
    fn get_session(&self, session_id: &str) -> zbus::Result<OwnedObjectPath>;
    fn get_user(&self, uid: u32) -> zbus::Result<OwnedObjectPath>;

    /// `start` true before the machine sleeps, false once it has resumed.
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
pub trait Session {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;

    /// Whether the session's locker says it is locked: logind's record of
    /// it, which the locker sets and clears.
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;

    fn set_locked_hint(&self, locked: bool) -> zbus::Result<()>;

    /// `loginctl lock-session`, the idle timer, the lid: lock now.
    #[zbus(signal)]
    fn lock(&self) -> zbus::Result<()>;

    /// `loginctl unlock-session`: a request a locker may refuse.
    #[zbus(signal)]
    fn unlock(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
pub trait User {
    /// `(session id, object path)` of the user's graphical session.
    #[zbus(property)]
    fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

/// This process's uid.
pub fn own_uid() -> u32 {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// The name `uid` has in the user database (`getpwuid_r`), or `None`.
pub fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    loop {
        // SAFETY: zeroed is a valid `passwd` (pointers null, numbers 0); it is
        // only read after `getpwuid_r` reports it filled.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is to a live local of the stated size, and
        // `buf` outlives every read of `pwd`'s strings below.
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || pwd.pw_name.is_null() {
            return None;
        }
        // SAFETY: `pw_name` points into `buf`, NUL-terminated, set by the call.
        let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
        return name.to_str().ok().map(str::to_owned);
    }
}

/// The session this process speaks for, and how it was found (for the
/// witness).
pub async fn session_id(conn: &Connection) -> Result<(String, &'static str), String> {
    if let Some(id) = std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty())
    {
        return Ok((id, "XDG_SESSION_ID"));
    }
    let manager = ManagerProxy::new(conn)
        .await
        .map_err(|e| format!("no logind ({e})"))?;
    if let Ok(path) = manager.get_session_by_pid(std::process::id()).await {
        let session = SessionProxy::builder(conn)
            .path(path)
            .map_err(|e| e.to_string())?
            .build()
            .await
            .map_err(|e| e.to_string())?;
        if let Ok(id) = session.id().await {
            return Ok((id, "logind's session for this process"));
        }
    }
    let user = manager
        .get_user(own_uid())
        .await
        .map_err(|e| format!("logind has no user for this uid ({e})"))?;
    let user = UserProxy::builder(conn)
        .path(user)
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    match user.display().await {
        Ok((id, _)) if !id.is_empty() => Ok((id, "the user's display session")),
        Ok(_) => Err("no session for this process and no display session".to_owned()),
        Err(e) => Err(format!("no display session ({e})")),
    }
}

/// The session `id` names, as a proxy on its object.
pub async fn session(conn: &Connection, id: &str) -> zbus::Result<SessionProxy<'static>> {
    let path = ManagerProxy::new(conn).await?.get_session(id).await?;
    SessionProxy::builder(conn).path(path)?.build().await
}
