//! The picture as a value, and the one pure function that makes it out of a
//! `GetManagedObjects` walk and the `GetSettings` replies beside it.
//!
//! Everything that decides what the bar sees lives here, takes [`nm::Managed`]
//! and [`nm::SettingsByPath`] by reference, and touches no bus — so
//! `network/tests/frames.rs` drives all five icon states and the whole row set
//! from fixture bytes plus a field edited by hand.

use std::collections::HashMap;

use crate::nm::{self, Managed, SettingsByPath};

/// What the bar item's icon says about the default route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Primary {
    /// No default route at all.
    #[default]
    None,
    /// A wired device carries it — or a VPN or bridge does, which is a wire as
    /// far as one chip can say.
    Wired,
    /// A Wi-Fi device carries it, at this signal strength.
    WiFi { strength: u8 },
}

/// One known Wi-Fi connection: a stored NM profile. A network in range that no
/// profile claims is a [`Visible`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    /// The row id, derived from the UUID by [`row_id`]: stable across frames
    /// for as long as the profile exists, which is what keeps the bar
    /// uploading rather than rebuilding when only a strength moved.
    pub row: u32,
    pub uuid: String,
    /// `connection.id`, the label.
    pub label: String,
    /// The `Settings.Connection` object path, which is what
    /// `ActivateConnection` is given.
    pub path: String,
    /// The strongest visible access point with this profile's SSID, or `None`
    /// when none is in NM's (aged) scan list — the no-signal icon, and nothing
    /// more: see [`Snapshot::joinable`].
    pub strength: Option<u8>,
    /// This profile is an active connection (activating or activated).
    pub active: bool,
}

/// One access point in range whose SSID matches **no** stored profile: a network
/// the machine has never joined, offered so the menu says what is *there*.
///
/// A click on its row **joins** it (`crate::actions`): NetworkManager is asked
/// to add a profile and activate it, and for a secured network NetworkManager
/// asks its secret agent for the passphrase, which is the canvas's prompt
/// (`caustics-secrets`). The passphrase never passes through the bar or this
/// sidecar. An enterprise (802.1X) or WEP network needs more than one
/// passphrase or a profile made elsewhere, and its row does nothing but say
/// so. It is drawn without a check or a radio, and that missing mark is the
/// whole difference between it and a known network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visible {
    /// [`visible_row_id`] over the SSID bytes: stable while the access point
    /// keeps its name, and unchanged when its strength moves — which is what
    /// keeps a scan result from rebuilding the bar's blocks and atlas.
    pub row: u32,
    /// The SSID as NM gave it. Bytes, not text: an `ay` is not required to be
    /// UTF-8, and the *match* against a profile's SSID is on these bytes.
    pub ssid: Vec<u8>,
    /// The strongest access point with this SSID that the Wi-Fi device can see.
    pub strength: u8,
    /// That access point's object path: the join's `specific_object`.
    pub ap: String,
    /// The Wi-Fi device that sees it: the join's `device`.
    pub device: String,
    /// What that access point asks for.
    pub security: Security,
}

/// What an access point in range asks of a client, read from its `Flags`,
/// `WpaFlags` and `RsnFlags` by [`security`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Security {
    /// Nothing: a profile with no security setting.
    #[default]
    Open,
    /// Enhanced Open (OWE): encrypted, with no secret.
    Owe,
    /// WPA or WPA2 with a passphrase: `key-mgmt` `wpa-psk`.
    Psk,
    /// WPA3 Personal: `key-mgmt` `sae`.
    Sae,
    /// 802.1X: a user, a method, certificates. Not joinable from the bar yet.
    Enterprise,
    /// WEP. Not joinable from the bar.
    Wep,
}

impl Security {
    /// The `802-11-wireless-security.key-mgmt` a new profile gets, or `None`
    /// for one with no security setting at all.
    pub fn key_mgmt(self) -> Option<&'static str> {
        match self {
            Security::Open => None,
            Security::Owe => Some("owe"),
            Security::Psk => Some("wpa-psk"),
            Security::Sae => Some("sae"),
            Security::Enterprise => Some("wpa-eap"),
            Security::Wep => Some("none"),
        }
    }

    /// Why a click cannot join it, or `None` when it can.
    pub fn unjoinable(self) -> Option<&'static str> {
        match self {
            Security::Enterprise => Some("802.1X"),
            Security::Wep => Some("WEP"),
            _ => None,
        }
    }
}

/// An access point's security, as NetworkManager's own clients read it: any
/// 802.1X key management is enterprise; a passphrase over WPA or WPA2 is
/// PSK, which a WPA2/WPA3 transition network also takes; SAE alone is WPA3;
/// OWE alone is Enhanced Open; privacy with no key management is WEP; and
/// nothing is open.
pub fn security(flags: u32, wpa: u32, rsn: u32) -> Security {
    let both = wpa | rsn;
    if both & (nm::SEC_KEY_MGMT_802_1X | nm::SEC_KEY_MGMT_EAP_SUITE_B_192) != 0 {
        Security::Enterprise
    } else if both & nm::SEC_KEY_MGMT_PSK != 0 {
        Security::Psk
    } else if rsn & nm::SEC_KEY_MGMT_SAE != 0 {
        Security::Sae
    } else if rsn & nm::SEC_KEY_MGMT_OWE != 0 {
        Security::Owe
    } else if flags & nm::AP_PRIVACY != 0 {
        Security::Wep
    } else {
        Security::Open
    }
}

impl Visible {
    /// The label the row wears. Lossy, because an SSID is bytes and a label has
    /// to be text; two SSIDs that differ only outside UTF-8 still get different
    /// rows, because the id is over the bytes.
    pub fn label(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}

impl Snapshot {
    /// Whether a click on a network's row can do anything. The one place that
    /// decision is made — `crate::rows` disables the row on it and
    /// `crate::actions` declines the action on it.
    ///
    /// **Not** "an access point with this SSID is in range": NM argues with
    /// that rule. Idle, NM ages its scan list down to the associated access
    /// point (from about twenty access points to one over some twenty minutes, measured on NetworkManager 1.56), so in steady state
    /// every profile but the joined one reads as out of range, and a menu
    /// built on that rule could never switch networks — its one job.
    /// `ActivateConnection` scans for the SSID itself, so the honest rule is
    /// the radio's: a row is joinable while Wi-Fi is on, and an unseen network
    /// keeps the no-signal icon as the hint that it may take a moment.
    pub fn joinable(&self, _network: &Network) -> bool {
        self.wireless_enabled
    }
}

/// The whole picture, and the only thing [`crate::rows::picture`] reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub wireless_enabled: bool,
    /// Every Wi-Fi device NM lists, by object path, in path order.
    ///
    /// Empty means there is no radio at all — a desktop with one ethernet port
    /// gets the plain offline icon rather than a disabled-radio one — and it is
    /// also what `RequestScan` is addressed to, one call per device, which is
    /// why it is the paths and not a bool.
    pub wifi: Vec<String>,
    pub primary: Primary,
    /// Known Wi-Fi profiles, in the order the menu shows them.
    pub networks: Vec<Network>,
    /// The access points in range that no stored profile claims, in the order
    /// the menu shows them: strongest first. Empty until something asks
    /// for a scan, which is when the menu opens and never otherwise.
    pub visible: Vec<Visible>,
}

impl Snapshot {
    /// There is a Wi-Fi device at all.
    pub fn wifi_device(&self) -> bool {
        !self.wifi.is_empty()
    }
}

// ---------------------------------------------------------------------------
// The icons (all themed names; Adwaita carries every one of these)
// ---------------------------------------------------------------------------

pub const WIRED: &str = "network-wired-symbolic";
pub const WIRELESS_OFFLINE: &str = "network-wireless-offline-symbolic";
pub const WIRELESS_DISABLED: &str = "network-wireless-disabled-symbolic";
pub const OFFLINE: &str = "network-offline-symbolic";

pub const EXCELLENT: &str = "network-wireless-signal-excellent-symbolic";
pub const GOOD: &str = "network-wireless-signal-good-symbolic";
pub const OK: &str = "network-wireless-signal-ok-symbolic";
pub const WEAK: &str = "network-wireless-signal-weak-symbolic";
pub const NONE: &str = "network-wireless-signal-none-symbolic";

/// A strength to its bucket. The bucket, not the number, is what the picture
/// is made of — which is why `Strength` moving a point or two on the active AP
/// every six seconds (measured, NM 1.56) publishes
/// nothing at all.
///
/// `None` — no access point with this SSID in range — and a strength under 5
/// are the same bucket: there is nothing there to draw.
pub fn bucket(strength: Option<u8>) -> &'static str {
    match strength {
        Some(s) if s >= 80 => EXCELLENT,
        Some(s) if s >= 55 => GOOD,
        Some(s) if s >= 30 => OK,
        Some(s) if s >= 5 => WEAK,
        _ => NONE,
    }
}

impl Snapshot {
    /// The bar item's icon: five states, in the
    /// order they have to be asked in — "no Wi-Fi device" comes before "the
    /// radio is off", because a machine with no radio has not switched one off.
    pub fn icon(&self) -> &'static str {
        match self.primary {
            Primary::Wired => WIRED,
            Primary::WiFi { strength } => bucket(Some(strength)),
            Primary::None if !self.wifi_device() => OFFLINE,
            Primary::None if !self.wireless_enabled => WIRELESS_DISABLED,
            Primary::None => WIRELESS_OFFLINE,
        }
    }
}

// ---------------------------------------------------------------------------
// Row ids
// ---------------------------------------------------------------------------

/// Or'd into every derived row id so it can never collide with the fixed ones
/// (`crate::rows`'s 1..=6), which is the rule for rows that
/// come and go. Never an index: the bar keys a menu row's identity on the id,
/// and an index moves when a network drops out of range.
pub const DERIVED: u32 = 0x100;

/// Set on a [`Visible`] row's id and **cleared** on a [`Network`]'s, so the two
/// derived spaces cannot collide however the hashes fall. Without the
/// clearing they would only be *mostly* apart: a UUID whose hash happens to
/// have the top bit set would land in the visible half, and the wire says an id
/// is unique in a frame.
pub const VISIBLE: u32 = 1 << 31;

/// FNV-1a, the one hash both derived ids are made with.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// FNV-1a over the UUID. A profile's UUID is the one thing about it that
/// outlives a rename, a reconnect and a reboot, so the row id does too.
pub fn row_id(uuid: &str) -> u32 {
    (fnv1a(uuid.as_bytes()) | DERIVED) & !VISIBLE
}

/// FNV-1a over the SSID **bytes**, in the visible half of the space. A network
/// in range has no UUID — that is what makes it unknown — and its SSID is the
/// only name it has. The id therefore holds still while the strength moves,
/// which is the property the bar's upload-versus-rebuild decision rests on.
pub fn visible_row_id(ssid: &[u8]) -> u32 {
    fnv1a(ssid) | VISIBLE
}

/// At most this many unknown networks. Twenty access points is an ordinary
/// reading in a flat and the menu is not a survey tool: ten strongest is a list
/// somebody can look at, and the rest are weaker than the ten above them.
pub const MAX_VISIBLE: usize = 10;

// ---------------------------------------------------------------------------
// The build
// ---------------------------------------------------------------------------

/// The walk and its settings replies as the picture. Pure, total, and the only
/// place the NM enumerations are interpreted.
pub fn build(managed: &Managed, settings: &SettingsByPath) -> Snapshot {
    let wifi = managed.devices.iter().find(|d| d.kind == nm::DEVICE_WIFI);
    let strengths: HashMap<&str, u8> = managed
        .access_points
        .iter()
        .map(|ap| (ap.path.as_str(), ap.strength))
        .collect();

    let mut networks: Vec<Network> = Vec::new();
    let mut seen: Vec<u32> = Vec::new();
    for path in &managed.connections {
        let Some(conn) = settings.get(path) else {
            continue;
        };
        if conn.kind != nm::TYPE_WIFI {
            continue;
        }
        let row = row_id(&conn.uuid);
        // Two FNV-1a ids colliding among a handful of profiles is a one-in-tens-of-
        // millions event, and the wire says ids are unique in a frame, so the
        // second of a colliding pair is dropped rather than sent.
        if seen.contains(&row) {
            continue;
        }
        seen.push(row);
        networks.push(Network {
            row,
            uuid: conn.uuid.clone(),
            label: conn.id.clone(),
            path: path.clone(),
            strength: conn
                .ssid
                .as_deref()
                .and_then(|ssid| visible(managed, wifi, ssid)),
            active: managed.active.iter().any(|a| {
                a.uuid == conn.uuid
                    && (a.state == nm::ACTIVE_ACTIVATING || a.state == nm::ACTIVE_ACTIVATED)
            }),
        });
    }
    // The active one first, then what is in range, then by label: an order
    // that moves only when connectivity or the scan result moves, never with
    // the signal strength, so a strength drifting past a bucket edge changes
    // one row's icon and nothing else's place.
    networks.sort_by(|a, b| {
        (
            !a.active,
            a.strength.is_none(),
            a.label.to_lowercase(),
            &a.uuid,
        )
            .cmp(&(
                !b.active,
                b.strength.is_none(),
                b.label.to_lowercase(),
                &b.uuid,
            ))
    });

    let known: Vec<&[u8]> = settings
        .values()
        .filter(|c| c.kind == nm::TYPE_WIFI)
        .filter_map(|c| c.ssid.as_deref())
        .collect();

    Snapshot {
        wireless_enabled: managed.wireless_enabled,
        wifi: managed
            .devices
            .iter()
            .filter(|d| d.kind == nm::DEVICE_WIFI)
            .map(|d| d.path.clone())
            .collect(),
        primary: primary(managed, &strengths),
        networks,
        visible: visible_networks(managed, wifi, &known),
    }
}

/// The access points in range that no stored profile claims, as the rows the
/// menu shows: deduplicated by SSID keeping the strongest, hidden ones dropped,
/// strongest first, capped at [`MAX_VISIBLE`].
///
/// Only the Wi-Fi device's own `AccessPoints` list is consulted, for the same
/// reason [`visible`] does: an access point object no device lists is one NM is
/// about to forget.
fn visible_networks(managed: &Managed, wifi: Option<&nm::Device>, known: &[&[u8]]) -> Vec<Visible> {
    let Some(wifi) = wifi else { return Vec::new() };

    let mut out: Vec<Visible> = Vec::new();
    for ap in &managed.access_points {
        // A hidden network broadcasts an empty SSID. There is nothing to label
        // a row with and nothing a click could do, so it is not a row.
        if ap.ssid.is_empty() {
            continue;
        }
        if !wifi.access_points.iter().any(|p| p == &ap.path) {
            continue;
        }
        // A profile the machine already has is a *known* network, and it is
        // already a row above with its radio mark. The match is on bytes.
        if known.iter().any(|ssid| *ssid == ap.ssid.as_slice()) {
            continue;
        }
        // One row per SSID: a mesh or a dual-band router puts the same name on
        // several access points, and the strongest is the one worth showing.
        // The strongest is also the one a join names, and whose security it
        // asks for.
        let security = security(ap.flags, ap.wpa_flags, ap.rsn_flags);
        match out.iter_mut().find(|v| v.ssid == ap.ssid) {
            Some(seen) if ap.strength > seen.strength => {
                seen.strength = ap.strength;
                seen.ap = ap.path.clone();
                seen.security = security;
            }
            Some(_) => {}
            None => out.push(Visible {
                row: visible_row_id(&ap.ssid),
                ssid: ap.ssid.clone(),
                strength: ap.strength,
                ap: ap.path.clone(),
                device: wifi.path.clone(),
                security,
            }),
        }
    }
    // Strongest first, then by label so the order is total and a tie does not
    // shuffle between frames. The *id* is not in the order, which is what lets
    // a strength move a row without moving its identity.
    out.sort_by(|a, b| {
        (b.strength, a.label().to_lowercase(), &a.ssid).cmp(&(
            a.strength,
            b.label().to_lowercase(),
            &b.ssid,
        ))
    });
    // Two ids colliding among a few dozen names is a one-in-ten-million event, and the wire says an id is unique in a frame: the second of a
    // colliding pair is dropped rather than sent, exactly as a profile's is.
    let mut ids: Vec<u32> = Vec::new();
    out.retain(|v| {
        let fresh = !ids.contains(&v.row);
        if fresh {
            ids.push(v.row);
        }
        fresh
    });
    out.truncate(MAX_VISIBLE);
    out
}

/// The strongest access point the Wi-Fi device can see with this SSID, or
/// `None` when none of them has it. Only the Wi-Fi device's own list is
/// consulted: an access point object that no device lists is one NM is about to
/// forget.
fn visible(managed: &Managed, wifi: Option<&nm::Device>, ssid: &[u8]) -> Option<u8> {
    let wifi = wifi?;
    managed
        .access_points
        .iter()
        .filter(|ap| wifi.access_points.iter().any(|p| p == &ap.path))
        .filter(|ap| ap.ssid == ssid)
        .map(|ap| ap.strength)
        .max()
}

/// `PrimaryConnection` as a [`Primary`]. The *device* decides wired or Wi-Fi,
/// not the connection's type string: a profile's type says what it is, and the
/// device says how the packets actually leave.
fn primary(managed: &Managed, strengths: &HashMap<&str, u8>) -> Primary {
    if managed.primary == nm::NO_PATH {
        return Primary::None;
    }
    let Some(active) = managed.active.iter().find(|a| a.path == managed.primary) else {
        return Primary::None;
    };
    let kinds: Vec<u32> = active
        .devices
        .iter()
        .filter_map(|path| managed.devices.iter().find(|d| &d.path == path))
        .map(|d| d.kind)
        .collect();
    if kinds.contains(&nm::DEVICE_WIFI) {
        let ap = managed
            .devices
            .iter()
            .find(|d| d.kind == nm::DEVICE_WIFI)
            .map(|d| d.active_access_point.as_str())
            .unwrap_or(nm::NO_PATH);
        return Primary::WiFi {
            strength: strengths.get(ap).copied().unwrap_or(0),
        };
    }
    // Ethernet, and anything else that is somehow the default route — a tun, a
    // bridge, a modem. One chip cannot say more than "there is a wire".
    Primary::Wired
}
