//! polkitd on the **system** bus: registering this process as the session's
//! authentication agent, and taking it back.
//!
//! The subject is a `unix-session` with the session's id, as every agent
//! registers. The id is found through logind by `caustics_sidecar::session`
//! (`$XDG_SESSION_ID`, else logind's session for this process, else the
//! user's display session), the lookup the lock sink shares.

use std::collections::HashMap;

use zbus::Connection;
use zbus::zvariant::Value;

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

/// The session this agent speaks for, and how it was found (for the
/// witness): `caustics_sidecar::session`'s lookup, which the lock sink
/// shares.
pub use caustics_sidecar::session::session_id;

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
