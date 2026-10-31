//! `org.bluez.Agent1` at [`PATH`]: the methods BlueZ calls while a device
//! pairs or asks for a service, each turned into an [`Event`] for the
//! conductor.
//!
//! **Which bus.** BlueZ calls an agent on the connection that registered it,
//! a system-bus connection, so in earnest this object is served there and
//! answers only BlueZ: a call whose sender is not the current owner of
//! `org.bluez` is refused. The dry run serves it on the session bus instead
//! and takes any caller, so a test can be BlueZ (the crate's note,
//! *Settings*).

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::ask;
use crate::conversation::{Display, Event, Outcome, Question, Request};
use crate::reply::Pin;
use crate::witness;

/// Where the agent is served: this project's own namespace, as BlueZ asks.
pub const PATH: &str = "/caustics/pairing";
pub const INTERFACE: &str = "org.bluez.Agent1";
/// BlueZ's name on the system bus.
pub const SERVICE: &str = "org.bluez";
/// The device interface, whose `Alias` is a conversation's title.
pub const DEVICE: &str = "org.bluez.Device1";

/// The two errors an agent returns, under BlueZ's own prefix.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

/// The served object.
pub struct Agent {
    /// The conductor's inbox.
    pub events: UnboundedSender<Event>,
    /// `true` in earnest: only BlueZ's own connection may call.
    pub check_caller: bool,
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    /// BlueZ has unregistered this agent (it is stopping, most often).
    async fn release(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "Release").await?;
        witness("released by BlueZ; registering again when it comes back");
        let _ = self.events.send(Event::Release);
        Ok(())
    }

    async fn request_pin_code(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<Pin, AgentError> {
        self.only_bluez(conn, &header, "RequestPinCode").await?;
        match self.put(conn, &device, Question::Pin).await? {
            Outcome::Pin(pin) => Ok(pin),
            other => Err(refusal(other)),
        }
    }

    async fn display_pin_code(
        &self,
        device: OwnedObjectPath,
        pincode: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "DisplayPinCode").await?;
        self.show(conn, &device, ask::type_on_device(&pincode, None))
            .await;
        Ok(())
    }

    async fn request_passkey(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<u32, AgentError> {
        self.only_bluez(conn, &header, "RequestPasskey").await?;
        match self.put(conn, &device, Question::Passkey).await? {
            Outcome::Passkey(passkey) => Ok(passkey),
            other => Err(refusal(other)),
        }
    }

    async fn display_passkey(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        entered: u16,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "DisplayPasskey").await?;
        self.show(
            conn,
            &device,
            ask::type_on_device(&ask::six(passkey), Some(entered)),
        )
        .await;
        Ok(())
    }

    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "RequestConfirmation")
            .await?;
        let line = ask::confirm_passkey(passkey);
        self.confirm(conn, &device, line).await
    }

    async fn request_authorization(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "RequestAuthorization")
            .await?;
        self.confirm(conn, &device, ask::ALLOW_PAIRING.to_owned())
            .await
    }

    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "AuthorizeService").await?;
        self.confirm(conn, &device, ask::allow_service(&uuid)).await
    }

    /// The request on screen is withdrawn: the pairing failed, or the other
    /// device gave up.
    async fn cancel(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "Cancel").await?;
        let _ = self.events.send(Event::Cancel);
        Ok(())
    }
}

impl Agent {
    /// Put `question` about `device` to the user and wait for the outcome.
    async fn put(
        &self,
        conn: &Connection,
        device: &OwnedObjectPath,
        question: Question,
    ) -> Result<Outcome, AgentError> {
        let (reply, outcome) = oneshot::channel();
        let request = Request {
            device: device.as_str().to_owned(),
            title: title(conn, device.as_str()).await,
            question,
            reply,
        };
        if self.events.send(Event::Ask(request)).is_err() {
            return Err(AgentError::Rejected("the agent is leaving".to_owned()));
        }
        outcome.await.map_err(|_| refusal(Outcome::Failed))
    }

    async fn confirm(
        &self,
        conn: &Connection,
        device: &OwnedObjectPath,
        line: String,
    ) -> Result<(), AgentError> {
        match self.put(conn, device, Question::Confirm { line }).await? {
            Outcome::Confirmed => Ok(()),
            other => Err(refusal(other)),
        }
    }

    async fn show(&self, conn: &Connection, device: &OwnedObjectPath, line: String) {
        let display = Display {
            device: device.as_str().to_owned(),
            title: title(conn, device.as_str()).await,
            line,
        };
        let _ = self.events.send(Event::Display(display));
    }

    async fn only_bluez(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        method: &str,
    ) -> Result<(), AgentError> {
        if !self.check_caller || from_bluez(conn, header).await {
            return Ok(());
        }
        witness(&format!(
            "{method} from {}, which is not BlueZ; refused",
            header
                .sender()
                .map_or_else(|| "nobody".to_owned(), |s| s.to_string())
        ));
        Err(AgentError::Rejected("only BlueZ may ask".to_owned()))
    }
}

/// The error an outcome that is not an answer returns.
fn refusal(outcome: Outcome) -> AgentError {
    match outcome {
        Outcome::Canceled => AgentError::Canceled("the request was cancelled".to_owned()),
        Outcome::Failed => AgentError::Rejected("the prompt could not be shown".to_owned()),
        _ => AgentError::Rejected("the prompt was dismissed".to_owned()),
    }
}

/// The device's `Alias`, which BlueZ keeps as its name or, for a device
/// that never gave one, its address; the address the path spells when the
/// property cannot be read (the dry run, where no BlueZ answers).
pub async fn title(conn: &Connection, device: &str) -> String {
    let alias = conn
        .call_method(
            Some(SERVICE),
            device,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(DEVICE, "Alias"),
        )
        .await
        .ok()
        .and_then(|reply| reply.body().deserialize::<OwnedValue>().ok())
        .and_then(|value| text(&value));
    alias.unwrap_or_else(|| ask::address_of(device))
}

/// A string, or a variant holding one, that is not empty.
fn text(value: &Value<'_>) -> Option<String> {
    match value {
        Value::Str(text) if !text.is_empty() => Some(text.as_str().to_owned()),
        Value::Value(inner) => text(inner),
        _ => None,
    }
}

/// The call came from whoever owns BlueZ's name now.
async fn from_bluez(conn: &Connection, header: &Header<'_>) -> bool {
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
