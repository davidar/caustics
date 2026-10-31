//! What a `GetSecrets` call asks for, decided from its arguments alone: the
//! panel's title, the one key the reply carries, the words the ask wears,
//! and whether the last answer was refused. Pure, so the whole table is
//! tested without a bus.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

/// The connection as NetworkManager sends it: `a{sa{sv}}`, setting name to
/// its properties.
pub type ConnectionDict = HashMap<String, HashMap<String, OwnedValue>>;

/// `NM_SECRET_AGENT_GET_SECRETS_FLAG_ALLOW_INTERACTION`: the agent may ask
/// the user. Without it nothing is shown.
pub const ALLOW_INTERACTION: u32 = 0x1;
/// `…_REQUEST_NEW`: the secrets NetworkManager has (the last answer, or the
/// stored one) were refused, and new ones are wanted.
pub const REQUEST_NEW: u32 = 0x2;
/// `…_USER_REQUESTED`: the activation was started by the user, not by
/// NetworkManager on its own. Read for the witness only.
pub const USER_REQUESTED: u32 = 0x4;

/// The Wi-Fi security setting, whose secret for WPA-PSK and WPA3 (SAE)
/// alike is `psk`.
pub const WIRELESS_SECURITY: &str = "802-11-wireless-security";
/// The 802.1X setting, whose key the hints name.
pub const IEEE_8021X: &str = "802-1x";

/// The line shown before the ask when [`REQUEST_NEW`] is set.
pub const WRONG: &str = "Wrong password";

/// One question, as the conductor puts it to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// The panel's title: the connection's `connection.id`, which for a
    /// network the bar joined is its SSID.
    pub title: String,
    /// The setting the reply goes under.
    pub setting: String,
    /// The one key the reply carries.
    pub key: String,
    /// What the ask says is wanted.
    pub words: String,
    /// The last answer was refused: the conversation leads with [`WRONG`].
    pub request_new: bool,
    /// The answer is a WPA-PSK passphrase, which is 8 to 63 characters or 64
    /// hexadecimal digits; anything else is refused on the panel before
    /// NetworkManager sees it.
    pub psk_rule: bool,
}

/// Why no question is put.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The flags do not allow interaction.
    NoInteraction,
    /// A setting, or a key management, this agent has no question for; the
    /// words name it for the witness.
    Unknown(String),
}

/// The question a call puts, or why it puts none.
pub fn ask(
    connection: &ConnectionDict,
    setting: &str,
    hints: &[String],
    flags: u32,
) -> Result<Ask, Refusal> {
    if flags & ALLOW_INTERACTION == 0 {
        return Err(Refusal::NoInteraction);
    }
    // A hint that names a key; `x-`-prefixed hints are VPN plugins' messages,
    // not keys.
    let hinted = hints.iter().find(|h| !h.is_empty() && !h.starts_with("x-"));
    let (key, psk_rule) = match setting {
        WIRELESS_SECURITY => match string(connection, WIRELESS_SECURITY, "key-mgmt").as_deref() {
            Some("wpa-psk") => ("psk".to_owned(), true),
            None | Some("sae") => ("psk".to_owned(), false),
            Some(other) => match hinted {
                Some(key) => (key.clone(), false),
                None => return Err(Refusal::Unknown(format!("{setting} key-mgmt {other}"))),
            },
        },
        IEEE_8021X => (
            hinted.cloned().unwrap_or_else(|| "password".to_owned()),
            false,
        ),
        _ => match hinted {
            Some(key) => (key.clone(), false),
            None => return Err(Refusal::Unknown(setting.to_owned())),
        },
    };
    Ok(Ask {
        title: title(connection),
        setting: setting.to_owned(),
        words: words(&key),
        key,
        request_new: flags & REQUEST_NEW != 0,
        psk_rule,
    })
}

/// The panel's title: `connection.id`, else the SSID as text, else what the
/// connection is.
pub fn title(connection: &ConnectionDict) -> String {
    if let Some(id) = string(connection, "connection", "id").filter(|id| !id.is_empty()) {
        return id;
    }
    match connection
        .get("802-11-wireless")
        .and_then(|w| w.get("ssid"))
        .map(|v| &**v)
    {
        Some(Value::Array(array)) if !array.is_empty() => {
            let bytes: Vec<u8> = array
                .iter()
                .filter_map(|v| match v {
                    Value::U8(b) => Some(*b),
                    _ => None,
                })
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => "A network".to_owned(),
    }
}

/// The words an ask for `key` wears.
pub fn words(key: &str) -> String {
    match key {
        "psk" | "password" => "Password".to_owned(),
        "private-key-password" => "Private key password".to_owned(),
        "phase2-private-key-password" => "Inner private key password".to_owned(),
        "pin" => "PIN".to_owned(),
        other => other.to_owned(),
    }
}

/// A WPA-PSK passphrase NetworkManager would take: 8 to 63 characters, or 64
/// hexadecimal digits (a raw key).
pub fn psk_valid(text: &str) -> bool {
    let chars = text.chars().count();
    (8..=63).contains(&chars) || (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn string(connection: &ConnectionDict, setting: &str, key: &str) -> Option<String> {
    match connection
        .get(setting)
        .and_then(|s| s.get(key))
        .map(|v| &**v)
    {
        Some(Value::Str(s)) => Some(s.as_str().to_owned()),
        _ => None,
    }
}
