//! `org.freedesktop.Notifications` at `/org/freedesktop/Notifications`
//! (Desktop Notifications Specification 1.2).
//!
//! The interface translates and records: a `Notify` becomes an [`Incoming`]
//! in the shared [`History`], and the loop in `lib.rs` is woken to publish.
//! What a click does is the loop's (`actions.rs`), because a click arrives on
//! stdin, not on the bus.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

use crate::history::{Closed, History, Icon, Incoming, URGENCY_NORMAL};
use crate::image::{self, ImageData};
use crate::{markup, witness};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const INTERFACE: &str = "org.freedesktop.Notifications";

/// `GetCapabilities`. Not `body-markup`: tags are stripped (`markup.rs`).
pub const CAPABILITIES: &[&str] = &["body", "actions", "icon-static", "persistence"];

/// The served object.
pub struct Server {
    pub history: Arc<Mutex<History>>,
    /// The loop's wake-up: something changed, publish.
    pub wake: Arc<Notify>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        CAPABILITIES.iter().map(|c| (*c).to_owned()).collect()
    }

    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            crate::NAME.to_owned(),
            "caustics".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            "1.2".to_owned(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> u32 {
        let mut incoming = decode(
            app_name,
            replaces_id,
            &app_icon,
            summary,
            &body,
            actions,
            &hints,
            expire_timeout,
        );
        // The click's signals go back to this connection alone (`lib.rs`
        // says why).
        incoming.sender = header.sender().map(|name| name.to_string());
        let urgency = incoming.urgency;
        let app = incoming.app_name.clone();
        let (id, closed) = {
            let mut history = self.history.lock().expect("the history");
            history.notify(incoming, Instant::now())
        };
        // The app and the id, never the text: the journal is not a place for
        // a chat message's contents.
        witness(&format!(
            "notify id={id}{} app={app:?} urgency={urgency}",
            if replaces_id != 0 && replaces_id == id {
                " (replaced)"
            } else {
                ""
            },
        ));
        emit_closed(&emitter, &closed).await;
        self.wake.notify_one();
        id
    }

    /// `NotificationClosed(id, 3)` for one this daemon holds. One it does not
    /// — never given, or already closed — is the specification's "an empty
    /// D-BUS Error message is sent back": `org.freedesktop.DBus.Error.Failed`
    /// with no text.
    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let removed = self.history.lock().expect("the history").remove(id);
        if !removed {
            return Err(zbus::fdo::Error::Failed(String::new()));
        }
        emit_closed(&emitter, &[(id, crate::history::CLOSED_BY_CALL)]).await;
        self.wake.notify_one();
        Ok(())
    }

    #[zbus(signal)]
    pub async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    /// Desktop Notifications 1.2: the token the app may activate itself
    /// with, emitted before the `ActionInvoked` of the same click
    /// (`lib.rs`, *A click hands the app focus*).
    #[zbus(signal)]
    pub async fn activation_token(
        emitter: &SignalEmitter<'_>,
        id: u32,
        activation_token: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

/// Emit each `NotificationClosed`, with one witness line apiece.
pub async fn emit_closed(emitter: &SignalEmitter<'_>, closed: &[Closed]) {
    for &(id, reason) in closed {
        witness(&format!("closed id={id} reason={reason}"));
        if let Err(e) = Server::notification_closed(emitter, id, reason).await {
            witness(&format!("NotificationClosed({id}, {reason}): {e}"));
        }
    }
}

/// A `Notify` call's arguments → what the history keeps.
#[allow(clippy::too_many_arguments)]
pub fn decode(
    app_name: String,
    replaces_id: u32,
    app_icon: &str,
    summary: String,
    body: &str,
    actions: Vec<String>,
    hints: &HashMap<String, OwnedValue>,
    expire_timeout: i32,
) -> Incoming {
    let summary = markup::strip(&summary);
    let body = markup::body(body);
    let actions = actions
        .chunks_exact(2)
        .map(|pair| (pair[0].clone(), markup::strip(&pair[1])))
        .collect();
    Incoming {
        app_name,
        replaces_id,
        icon: icon(app_icon, hints),
        summary,
        body,
        actions,
        urgency: hint_u8(hints, "urgency").unwrap_or(URGENCY_NORMAL),
        transient: hint_bool(hints, "transient"),
        resident: hint_bool(hints, "resident"),
        expire_timeout,
        sender: None,
    }
}

/// The specification's precedence: `image-data` (and its two older
/// spellings), then `image-path`, then `app_icon` — passed on as a name or a
/// path, for `caustics-glyphs` and `caustics-apps` to resolve; nothing here asks.
///
/// Not the `desktop-entry` hint: it names a desktop
/// file, not an icon, and the bar asks `caustics-apps` about a row's icon as a
/// themed name (`REQ_ICON`), which `caustics-apps` looks up in the icon themes
/// alone. Treating it as an icon name would resolve only when the desktop id happens to equal its `Icon=`, and draw an empty slot otherwise.
pub fn icon(app_icon: &str, hints: &HashMap<String, OwnedValue>) -> Icon {
    for key in ["image-data", "image_data", "icon_data"] {
        if let Some(value) = hints.get(key) {
            match image_data(value).as_ref().and_then(image::fit_hint) {
                Some(fitted) => return Icon::Pixels(fitted),
                None => witness(&format!("{key}: not an image this can read; ignored")),
            }
        }
    }
    let named = ["image-path", "image_path"]
        .iter()
        .filter_map(|key| hint_str(hints, key))
        .chain(std::iter::once(app_icon.to_owned()))
        .find_map(|name| icon_name(&name));
    named.map_or(Icon::None, Icon::Name)
}

/// A name or a path as the bar takes it: `file://` stripped, and `None` for
/// nothing at all or a relative path (a name with a `/` that is not absolute
/// is no name: a themed name has no slash, and a relative path has nothing
/// the bar could resolve it against).
pub fn icon_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    let name = name.strip_prefix("file://").unwrap_or(name);
    if name.is_empty() || (name.contains('/') && !name.starts_with('/')) {
        return None;
    }
    Some(name.to_owned())
}

fn image_data(value: &OwnedValue) -> Option<ImageData> {
    let value: Value<'_> = value.try_clone().ok()?.into();
    ImageData::try_from(value).ok()
}

fn hint_str(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = hints.get(key)?;
    <&str>::try_from(value).ok().map(str::to_owned)
}

/// A byte by the specification; some clients send an int.
fn hint_u8(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    let value = hints.get(key)?;
    if let Ok(b) = u8::try_from(value) {
        return Some(b);
    }
    if let Ok(i) = i32::try_from(value) {
        return u8::try_from(i).ok();
    }
    u32::try_from(value).ok().and_then(|u| u8::try_from(u).ok())
}

/// A boolean by the specification; some clients send an int.
fn hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    let Some(value) = hints.get(key) else {
        return false;
    };
    if let Ok(b) = bool::try_from(value) {
        return b;
    }
    i32::try_from(value)
        .map(|i| i != 0)
        .or_else(|_| u8::try_from(value).map(|b| b != 0))
        .unwrap_or(false)
}
