//! polkitd and logind on the **system** bus: registering this process as
//! the session's authentication agent, and taking it back.
//!
//! The subject is a `unix-session` with the session's id, as every agent
//! registers. The id is `$XDG_SESSION_ID` when the session set it, else
//! logind's session for this process (`GetSessionByPID`), else the user's
//! display session (`User.Display`): a process under the user's service
//! manager is in no session of its own, and the display session is the one
//! a graphical prompt is for.

use std::collections::HashMap;

use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::agent::PATH;

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn register_authentication_agent(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        locale: &str,
        object_path: &str,
    ) -> zbus::Result<()>;

    fn unregister_authentication_agent(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        object_path: &str,
    ) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    #[zbus(name = "GetSessionByPID")]
    fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
    fn get_user(&self, uid: u32) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
trait User {
    /// `(session id, object path)` of the user's graphical session.
    #[zbus(property)]
    fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

/// The session this agent speaks for, and how it was found (for the
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
        .get_user(crate::agent::own_uid())
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

/// The locale polkitd translates its messages into: `LC_ALL`, then
/// `LC_MESSAGES`, then `LANG`, as the C library would choose, else `C`.
pub fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "C".to_owned())
}

fn subject(session: &str) -> (&'static str, HashMap<&'static str, Value<'_>>) {
    (
        "unix-session",
        HashMap::from([("session-id", Value::from(session))]),
    )
}

/// `RegisterAuthenticationAgent(subject, locale, PATH)`.
pub async fn register(conn: &Connection, session: &str, locale: &str) -> zbus::Result<()> {
    AuthorityProxy::new(conn)
        .await?
        .register_authentication_agent(&subject(session), locale, PATH)
        .await
}

/// `UnregisterAuthenticationAgent(subject, PATH)`.
pub async fn unregister(conn: &Connection, session: &str) -> zbus::Result<()> {
    AuthorityProxy::new(conn)
        .await?
        .unregister_authentication_agent(&subject(session), PATH)
        .await
}
