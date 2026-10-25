//! `org.freedesktop.PolicyKit1.AuthenticationAgent` at
//! `/org/freedesktop/PolicyKit1/AuthenticationAgent`: the two methods
//! polkitd calls, each turned into an [`Event`] for the conductor.
//!
//! **Which bus.** polkitd calls an agent back on the connection that
//! registered it, which is a system-bus connection (`RegisterAuthentication
//! Agent` is a method of polkitd's, on the system bus), so in earnest this
//! object is served there and answers only polkitd: a call whose sender is
//! not the current owner of `org.freedesktop.PolicyKit1` is refused. The dry
//! run serves it on the session bus instead and takes any caller, so a test
//! can be polkitd (the crate's note, *Dry run*).

use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::message::Header;
use zbus::zvariant::OwnedValue;

use crate::conversation::{Begin, Event, Outcome};
use crate::witness;

pub const PATH: &str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";
pub const INTERFACE: &str = "org.freedesktop.PolicyKit1.AuthenticationAgent";
/// polkitd's name on the system bus.
pub const AUTHORITY: &str = "org.freedesktop.PolicyKit1";

/// The errors an agent returns, under polkit's own prefix
/// (`org.freedesktop.PolicyKit1.Error.Failed`, `….Cancelled`), as
/// libpolkit-agent's listener and soteria return them.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
pub enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
}

/// A polkit identity as it crosses the bus: `(sa{sv})`, a kind and its
/// details. `unix-user` carries `uid`; `unix-group` carries `gid`.
pub type Identity = (String, HashMap<String, OwnedValue>);

/// The served object.
pub struct Agent {
    /// The conductor's inbox.
    pub events: UnboundedSender<Event>,
    /// `true` in earnest: only polkitd's own connection may call.
    pub check_caller: bool,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    #[allow(clippy::too_many_arguments)]
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<Identity>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PolkitError> {
        if self.check_caller && !from_authority(conn, &header).await {
            witness(&format!(
                "BeginAuthentication from {}, which is not polkitd; refused",
                header
                    .sender()
                    .map_or_else(|| "nobody".to_owned(), |s| s.to_string())
            ));
            return Err(PolkitError::Failed("only polkitd may ask".to_owned()));
        }
        let Some((uid, user)) = choose(&identities, own_uid()) else {
            witness(&format!(
                "{action_id}: none of the {} identities offered is a unix user with a name; refused",
                identities.len()
            ));
            return Err(PolkitError::Failed(
                "no unix-user identity to authenticate as".to_owned(),
            ));
        };
        let (reply, outcome) = oneshot::channel();
        let begin = Begin {
            action_id,
            message,
            user,
            uid,
            cookie,
            reply,
        };
        if self.events.send(Event::Begin(begin)).is_err() {
            return Err(PolkitError::Failed("the agent is leaving".to_owned()));
        }
        match outcome.await {
            Ok(Outcome::Authorized) => Ok(()),
            Ok(Outcome::Cancelled) => Err(PolkitError::Cancelled(
                "the request was cancelled".to_owned(),
            )),
            Ok(Outcome::Dismissed) => Err(PolkitError::Cancelled(
                "the prompt was dismissed".to_owned(),
            )),
            Ok(Outcome::Failed) | Err(_) => {
                Err(PolkitError::Failed("authentication failed".to_owned()))
            }
        }
    }

    async fn cancel_authentication(
        &self,
        cookie: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PolkitError> {
        if self.check_caller && !from_authority(conn, &header).await {
            witness("CancelAuthentication from a connection that is not polkitd; refused");
            return Err(PolkitError::Failed("only polkitd may cancel".to_owned()));
        }
        let _ = self.events.send(Event::Cancel { cookie });
        Ok(())
    }
}

/// The call came from whoever owns polkitd's name now.
async fn from_authority(conn: &Connection, header: &Header<'_>) -> bool {
    let Some(sender) = header.sender() else {
        return false;
    };
    let Ok(dbus) = DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(name) = AUTHORITY.try_into() else {
        return false;
    };
    dbus.get_name_owner(name)
        .await
        .is_ok_and(|owner| owner.as_str() == sender.as_str())
}

/// Whom to authenticate as (docs/canvas.md, *How each job is wired*): the
/// first `unix-user` identity whose uid is `own`, else the first `unix-user`
/// offered; a group or any other kind is passed over, because the helper
/// authenticates a user. `None` when no offered user has a name.
pub fn choose(identities: &[Identity], own: u32) -> Option<(u32, String)> {
    let users: Vec<u32> = identities
        .iter()
        .filter(|(kind, _)| kind == "unix-user")
        .filter_map(|(_, details)| details.get("uid").and_then(|v| u32::try_from(v).ok()))
        .collect();
    let uid = users
        .iter()
        .copied()
        .find(|uid| *uid == own)
        .or_else(|| users.first().copied())?;
    user_name(uid).map(|name| (uid, name))
}

/// This process's real uid, and the name a uid has in the user database:
/// the session lookup's, shared with the lock sink.
pub use caustics_sidecar::session::{own_uid, user_name};

#[cfg(test)]
mod tests;
