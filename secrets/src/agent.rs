//! `org.freedesktop.NetworkManager.SecretAgent` at
//! `/org/freedesktop/NetworkManager/SecretAgent`: the four methods
//! NetworkManager calls, the two that matter turned into an [`Event`] for
//! the conductor.
//!
//! **Which bus.** NetworkManager calls an agent back on the connection that
//! registered it, a system-bus connection, so in earnest this object is
//! served there and answers only NetworkManager: a call whose sender is not
//! the current owner of `org.freedesktop.NetworkManager` is refused. The dry
//! run serves it on the session bus instead and takes any caller, so a test
//! can be NetworkManager (the crate's note, *Settings*).

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::message::Header;
use zbus::zvariant::OwnedObjectPath;

use crate::ask::{self, ConnectionDict, Refusal};
use crate::conversation::{Event, Outcome, Request};
use crate::reply::Secrets;
use crate::witness;

pub const PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";
pub const INTERFACE: &str = "org.freedesktop.NetworkManager.SecretAgent";
/// NetworkManager's name on the system bus.
pub const SERVICE: &str = "org.freedesktop.NetworkManager";

/// The errors an agent returns, under NetworkManager's own prefix, as
/// libnm's `NMSecretAgentError` names them.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.NetworkManager.SecretAgent.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    PermissionDenied(String),
    UserCanceled(String),
    AgentCanceled(String),
    NoSecrets(String),
    Failed(String),
}

/// The served object.
pub struct Agent {
    /// The conductor's inbox.
    pub events: UnboundedSender<Event>,
    /// `true` in earnest: only NetworkManager's own connection may call.
    pub check_caller: bool,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.SecretAgent")]
impl Agent {
    #[allow(clippy::too_many_arguments)]
    async fn get_secrets(
        &self,
        connection: ConnectionDict,
        connection_path: OwnedObjectPath,
        setting_name: String,
        hints: Vec<String>,
        flags: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<Secrets, AgentError> {
        self.only_networkmanager(conn, &header, "GetSecrets")
            .await?;
        let ask = match ask::ask(&connection, &setting_name, &hints, flags) {
            Ok(ask) => ask,
            Err(Refusal::NoInteraction) => {
                witness(&format!(
                    "{:?} {setting_name}: no interaction allowed (flags {flags:#x}); no secrets",
                    ask::title(&connection)
                ));
                return Err(AgentError::NoSecrets(
                    "this agent stores nothing and may not ask".to_owned(),
                ));
            }
            Err(Refusal::Unknown(what)) => {
                witness(&format!(
                    "{:?}: no question for {what}; no secrets",
                    ask::title(&connection)
                ));
                return Err(AgentError::NoSecrets(format!("nothing to ask for {what}")));
            }
        };
        let (reply, outcome) = oneshot::channel();
        let request = Request {
            path: connection_path.as_str().to_owned(),
            ask,
            reply,
        };
        if self.events.send(Event::Get(request)).is_err() {
            return Err(AgentError::Failed("the agent is leaving".to_owned()));
        }
        match outcome.await {
            Ok(Outcome::Secret(secrets)) => Ok(secrets),
            Ok(Outcome::Dismissed) => Err(AgentError::UserCanceled(
                "the prompt was dismissed".to_owned(),
            )),
            Ok(Outcome::Cancelled) => Err(AgentError::AgentCanceled(
                "the request was cancelled".to_owned(),
            )),
            Ok(Outcome::Failed) | Err(_) => Err(AgentError::Failed(
                "the prompt could not be shown".to_owned(),
            )),
        }
    }

    async fn cancel_get_secrets(
        &self,
        connection_path: OwnedObjectPath,
        setting_name: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_networkmanager(conn, &header, "CancelGetSecrets")
            .await?;
        let _ = self.events.send(Event::Cancel {
            path: connection_path.as_str().to_owned(),
            setting: setting_name,
        });
        Ok(())
    }

    /// This agent stores nothing: a system-owned secret is NetworkManager's
    /// to keep, and an agent-owned one is never made here.
    async fn save_secrets(
        &self,
        _connection: ConnectionDict,
        _connection_path: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_networkmanager(conn, &header, "SaveSecrets").await
    }

    /// Nothing is stored, so nothing is deleted.
    async fn delete_secrets(
        &self,
        _connection: ConnectionDict,
        _connection_path: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_networkmanager(conn, &header, "DeleteSecrets")
            .await
    }
}

impl Agent {
    async fn only_networkmanager(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        method: &str,
    ) -> Result<(), AgentError> {
        if !self.check_caller || from_networkmanager(conn, header).await {
            return Ok(());
        }
        witness(&format!(
            "{method} from {}, which is not NetworkManager; refused",
            header
                .sender()
                .map_or_else(|| "nobody".to_owned(), |s| s.to_string())
        ));
        Err(AgentError::PermissionDenied(
            "only NetworkManager may ask".to_owned(),
        ))
    }
}

/// The call came from whoever owns NetworkManager's name now.
async fn from_networkmanager(conn: &Connection, header: &Header<'_>) -> bool {
    let Some(sender) = header.sender() else {
        return false;
    };
    let Ok(dbus) = DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(name) = SERVICE.try_into() else {
        return false;
    };
    dbus.get_name_owner(name)
        .await
        .is_ok_and(|owner| owner.as_str() == sender.as_str())
}
