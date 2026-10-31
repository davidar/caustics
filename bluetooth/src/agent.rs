//! `org.bluez.Agent1` at [`PATH`]: the methods BlueZ calls while a device
//! pairs or asks for a service, each handed to the sidecar's loop as an
//! [`Event`], and BlueZ's agent manager, which this sidecar registers with.
//!
//! **One agent.** BlueZ sends every question of one pairing to one agent:
//! the one registered by the connection that started the pairing, else the
//! default. This sidecar starts the pairings of its menu and asks to be the
//! default, so it is the agent for both, and it routes: a yes or a no is its
//! own chip's rows ([`crate::pairing`]), a code to type is the canvas's
//! pairing sink's ([`crate::sink`]).
//!
//! **Which bus.** BlueZ calls an agent on the connection that registered it,
//! the system-bus connection, so in earnest this object is served there and
//! answers only BlueZ: a call whose sender is not the current owner of
//! `org.bluez` is refused. The dry run serves it on the session bus instead
//! and takes any caller, so a test can be BlueZ (the crate's note,
//! *Settings*).

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::message::Header;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::ask;
use crate::bluez::{DEVICE, SERVICE};
use crate::reply::Pin;
use crate::witness;

/// Where the agent is served: this project's own namespace, as BlueZ asks.
pub const PATH: &str = "/caustics/bluetooth/agent";
pub const INTERFACE: &str = "org.bluez.Agent1";

/// The capability registered while the pairing sink answers: this agent can
/// show a code and have one typed, so BlueZ picks the strongest pairing both
/// devices allow.
pub const KEYBOARD_DISPLAY: &str = "KeyboardDisplay";
/// The capability registered while it does not: a code is shown or
/// compared, never typed here.
pub const DISPLAY_YES_NO: &str = "DisplayYesNo";

/// The capability that follows whether the sink answers.
pub fn capability(sink_answers: bool) -> &'static str {
    if sink_answers {
        KEYBOARD_DISPLAY
    } else {
        DISPLAY_YES_NO
    }
}

/// The two errors an agent returns, under BlueZ's own prefix.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

/// What a method asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `RequestConfirmation`: the passkey both devices show.
    Confirm { passkey: u32 },
    /// `RequestAuthorization`: allow a pairing nothing is compared for.
    Authorize,
    /// `AuthorizeService`: allow a profile.
    Service { uuid: String },
    /// `RequestPinCode`: a legacy PIN, typed.
    Pin,
    /// `RequestPasskey`: a six-digit passkey, typed.
    Passkey,
}

impl Request {
    /// What this request is called in a witness: never a passkey.
    pub fn name(&self) -> &'static str {
        match self {
            Request::Confirm { .. } => "a passkey confirmation",
            Request::Authorize => "an authorization",
            Request::Service { .. } => "a service authorization",
            Request::Pin => "a PIN",
            Request::Passkey => "a passkey",
        }
    }
}

/// How a request ended, as the method returns it.
#[derive(Debug)]
pub enum Outcome {
    /// Confirmed, or allowed: the method returns.
    Confirmed,
    /// The PIN typed, as the reply.
    Pin(Pin),
    /// The passkey typed, as the reply.
    Passkey(u32),
    /// `org.bluez.Error.Rejected`, with words that say why.
    Rejected(&'static str),
    /// BlueZ withdrew the request (`Cancel`, `Release`):
    /// `org.bluez.Error.Canceled`.
    Canceled,
}

/// Everything the agent object hands the loop.
#[derive(Debug)]
pub enum Event {
    /// A request BlueZ waits on.
    Ask {
        device: String,
        title: String,
        request: Request,
        reply: oneshot::Sender<Outcome>,
    },
    /// `DisplayPinCode` or `DisplayPasskey`: a code to type on the device,
    /// owed no reply.
    Display {
        device: String,
        title: String,
        code: String,
        entered: Option<u16>,
    },
    /// `Cancel()`: the request asked is withdrawn.
    Cancel,
    /// `Release()`: BlueZ has unregistered this agent.
    Release,
}

/// The served object.
pub struct Agent {
    /// The loop's inbox.
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
        witness("agent: released by BlueZ; registering again when it comes back");
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
        match self.put(conn, &device, Request::Pin).await? {
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
        self.show(conn, &device, pincode, None).await;
        Ok(())
    }

    async fn request_passkey(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<u32, AgentError> {
        self.only_bluez(conn, &header, "RequestPasskey").await?;
        match self.put(conn, &device, Request::Passkey).await? {
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
        self.show(conn, &device, ask::spaced(passkey), Some(entered))
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
        self.yes_or_no(conn, &device, Request::Confirm { passkey })
            .await
    }

    async fn request_authorization(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "RequestAuthorization")
            .await?;
        self.yes_or_no(conn, &device, Request::Authorize).await
    }

    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), AgentError> {
        self.only_bluez(conn, &header, "AuthorizeService").await?;
        self.yes_or_no(conn, &device, Request::Service { uuid })
            .await
    }

    /// The request asked is withdrawn: the pairing failed, or the other
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
    /// Hand `request` about `device` to the loop and wait for the outcome.
    async fn put(
        &self,
        conn: &Connection,
        device: &OwnedObjectPath,
        request: Request,
    ) -> Result<Outcome, AgentError> {
        let (reply, outcome) = oneshot::channel();
        let event = Event::Ask {
            device: device.as_str().to_owned(),
            title: title(conn, device.as_str()).await,
            request,
            reply,
        };
        if self.events.send(event).is_err() {
            return Err(AgentError::Rejected("the agent is leaving".to_owned()));
        }
        outcome
            .await
            .map_err(|_| AgentError::Rejected("the agent is leaving".to_owned()))
    }

    async fn yes_or_no(
        &self,
        conn: &Connection,
        device: &OwnedObjectPath,
        request: Request,
    ) -> Result<(), AgentError> {
        match self.put(conn, device, request).await? {
            Outcome::Confirmed => Ok(()),
            other => Err(refusal(other)),
        }
    }

    async fn show(
        &self,
        conn: &Connection,
        device: &OwnedObjectPath,
        code: String,
        entered: Option<u16>,
    ) {
        let _ = self.events.send(Event::Display {
            device: device.as_str().to_owned(),
            title: title(conn, device.as_str()).await,
            code,
            entered,
        });
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
            "agent: {method} from {}, which is not BlueZ; refused",
            header
                .sender()
                .map_or_else(|| "nobody".to_owned(), |s| s.to_string())
        ));
        Err(AgentError::Rejected("only BlueZ may ask".to_owned()))
    }
}

/// The error an outcome that is not an answer returns.
pub fn refusal(outcome: Outcome) -> AgentError {
    match outcome {
        Outcome::Canceled => AgentError::Canceled("the request was cancelled".to_owned()),
        Outcome::Rejected(why) => AgentError::Rejected(why.to_owned()),
        _ => AgentError::Rejected("not the answer this request asked for".to_owned()),
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

/// `org.bluez.AgentManager1` at `/org/bluez`, where an agent registers.
#[zbus::proxy(
    interface = "org.bluez.AgentManager1",
    default_service = "org.bluez",
    default_path = "/org/bluez"
)]
trait AgentManager {
    fn register_agent(&self, agent: &ObjectPath<'_>, capability: &str) -> zbus::Result<()>;
    fn request_default_agent(&self, agent: &ObjectPath<'_>) -> zbus::Result<()>;
    fn unregister_agent(&self, agent: &ObjectPath<'_>) -> zbus::Result<()>;
}

fn path() -> ObjectPath<'static> {
    ObjectPath::from_static_str_unchecked(PATH)
}

/// Serve the agent object on `conn`.
pub async fn serve(
    conn: &Connection,
    check_caller: bool,
    events: UnboundedSender<Event>,
) -> zbus::Result<bool> {
    conn.object_server()
        .at(
            PATH,
            Agent {
                events,
                check_caller,
            },
        )
        .await
}

/// `RegisterAgent(PATH, capability)`, then `RequestDefaultAgent(PATH)` so
/// the pairings another device starts while the adapter is discoverable ask
/// this one too. The second refused is one witness and not a failure: the
/// agent still answers the pairings this sidecar starts. `why` is the
/// witness's reason for the capability.
pub async fn register(conn: &Connection, capability: &str, why: &str) -> zbus::Result<()> {
    let manager = AgentManagerProxy::new(conn).await?;
    manager.register_agent(&path(), capability).await?;
    match manager.request_default_agent(&path()).await {
        Ok(()) => witness(&format!(
            "agent: {PATH} ({capability}: {why}), registered with {SERVICE} as the default"
        )),
        Err(e) => witness(&format!(
            "agent: {PATH} ({capability}: {why}), registered with {SERVICE}; \
             not the default: {}",
            error_name(&e)
        )),
    }
    Ok(())
}

pub async fn unregister(conn: &Connection) -> zbus::Result<()> {
    AgentManagerProxy::new(conn)
        .await?
        .unregister_agent(&path())
        .await
}

/// The error a call to a name nobody owns gets.
pub fn absent(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ),
        _ => false,
    }
}

/// An error as BlueZ named it (`org.bluez.Error.AlreadyExists`), with its
/// words; anything else as zbus says it.
pub fn error_name(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, Some(words), _) => format!("{} ({words})", name.as_str()),
        zbus::Error::MethodError(name, None, _) => name.as_str().to_owned(),
        other => other.to_string(),
    }
}

/// BlueZ's name getting a new owner, as a stream of the owners' names: a
/// restarted bluetoothd has forgotten every agent (and released this one on
/// its way down), so the loop registers again. The stream is forwarded the
/// moment it exists (docs/architecture.md, *Rules*).
pub fn owners(conn: &Connection, into: UnboundedSender<String>) -> tokio::task::JoinHandle<()> {
    let conn = conn.clone();
    tokio::spawn(async move {
        let Ok(dbus) = DBusProxy::new(&conn).await else {
            witness("agent: org.freedesktop.DBus: no proxy; BlueZ restarting will not be noticed");
            return;
        };
        let stream = match dbus
            .receive_name_owner_changed_with_args(&[(0, SERVICE)])
            .await
        {
            Ok(stream) => stream,
            Err(e) => {
                witness(&format!("agent: watching {SERVICE}: {e}"));
                return;
            }
        };
        let (tx, mut owners) = tokio::sync::mpsc::unbounded_channel();
        let _drain = caustics_sidecar::forward(stream, tx, |signal| {
            signal
                .args()
                .ok()
                .and_then(|args| args.new_owner().as_ref().map(|o| o.to_string()))
        });
        while let Some(owner) = owners.recv().await {
            if into.send(owner).is_err() {
                return;
            }
        }
    })
}
