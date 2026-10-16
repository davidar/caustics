//! The fixtures under `network/tests/data/`, written from invented data.
//!
//! Each body is what NetworkManager 1.56 answers on the system bus, byte for
//! byte in layout: the same objects, interfaces, property names and variant
//! types as a real `GetManagedObjects` walk of a machine on Wi-Fi with a
//! Tailscale tunnel up, serialised through zvariant exactly as zbus hands a
//! reply body to the parser. Only the values are made up, and made up so that
//! nothing in them can point at a real person, machine or network:
//!
//! - SSIDs and connection ids that are plainly examples (`ExampleNet`,
//!   `hotspot-example`);
//! - MAC addresses and BSSIDs that are locally administered (`02:…`);
//! - IPv4 addresses in the documentation ranges (192.0.2.0/24 for the LAN,
//!   198.51.100.0/24 for DNS and NTP), the tunnel and its three peers in the
//!   shared range 100.64.0.0/10;
//! - IPv6 addresses in the documentation prefix 2001:db8::/32 and invented
//!   unique-local prefixes;
//! - a hostname under `example.invalid`, an invented DUID and UUIDs.
//!
//! The shapes are the point: object paths inside variants, byte-array SSIDs,
//! `aa{sv}` and `a(ayuay)` values the parser must step over without reading.
//! The bodies are deterministic (every map here is ordered), so a test can
//! check that the committed files are this module's output, and
//! `live.rs`'s `synthesise_fixtures` rewrites them after an edit here:
//!
//! ```text
//! cargo test -p caustics-network --test live synthesise_fixtures -- --ignored
//! ```

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, Ipv6Addr};

use zbus::zvariant::serialized::Context;
use zbus::zvariant::{LE, ObjectPath, Value, to_bytes};

/// The joined network: its profile's `connection.id`, which is also its SSID.
pub const ACTIVE_ID: &str = "ExampleNet";
pub const ACTIVE_UUID: &str = "5e7e0000-0000-4000-8000-000000000002";
/// A known network that is out of range, a WPA-PSK phone hotspot whose
/// profile name and SSID differ, as they do when a person renames one.
pub const OTHER_ID: &str = "hotspot-example";
pub const OTHER_SSID: &str = "Example Phone";
pub const OTHER_UUID: &str = "5e7e0000-0000-4000-8000-000000000004";
pub const HOSTNAME: &str = "workstation.example.invalid";

const NM: &str = "/org/freedesktop/NetworkManager";
const AP: &str = "/org/freedesktop/NetworkManager/AccessPoint/101";
const ROOT: &str = "/";

type Props = BTreeMap<&'static str, Value<'static>>;

fn props<const N: usize>(entries: [(&'static str, Value<'static>); N]) -> Props {
    entries.into_iter().collect()
}

fn s(text: &str) -> Value<'static> {
    Value::from(text.to_owned())
}

fn o(path: &str) -> Value<'static> {
    Value::from(ObjectPath::try_from(path.to_owned()).expect("an object path"))
}

fn ao(paths: &[&str]) -> Value<'static> {
    Value::from(
        paths
            .iter()
            .map(|p| ObjectPath::try_from(p.to_string()).expect("an object path"))
            .collect::<Vec<_>>(),
    )
}

fn strings(items: &[&str]) -> Value<'static> {
    Value::from(items.iter().map(|i| i.to_string()).collect::<Vec<_>>())
}

fn nm(path: &str) -> String {
    format!("{NM}/{path}")
}

/// A `HashMap<String, Value>` as an `a{sv}` value.
fn dict<const N: usize>(entries: [(&str, Value<'static>); N]) -> Value<'static> {
    Value::from(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect::<HashMap<_, _>>(),
    )
}

/// An `aa{sv}` value.
fn dicts(entries: Vec<HashMap<String, Value<'static>>>) -> Value<'static> {
    Value::from(entries)
}

fn entry<const N: usize>(entries: [(&str, Value<'static>); N]) -> HashMap<String, Value<'static>> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
}

fn no_dicts() -> Value<'static> {
    dicts(Vec::new())
}

/// An IPv4 address the way NM's deprecated `au` properties carry it: the
/// network-order bytes read as a little-endian `u32`.
fn ip4(text: &str) -> u32 {
    let addr: Ipv4Addr = text.parse().expect("an IPv4 address");
    u32::from_le_bytes(addr.octets())
}

fn ip6(text: &str) -> Vec<u8> {
    let addr: Ipv6Addr = text.parse().expect("an IPv6 address");
    addr.octets().to_vec()
}

fn serialise<T: zbus::export::serde::Serialize + zbus::zvariant::Type>(value: &T) -> Vec<u8> {
    to_bytes(Context::new_dbus(LE, 0), value)
        .expect("the body serialises")
        .bytes()
        .to_vec()
}

// ---------------------------------------------------------------------------
// GetManagedObjects
// ---------------------------------------------------------------------------

/// `a{oa{sa{sv}}}`: the whole walk.
pub fn managed_objects() -> Vec<u8> {
    let mut objects: BTreeMap<ObjectPath<'static>, BTreeMap<&'static str, Props>> = BTreeMap::new();
    let mut put = |path: String, interfaces: Vec<(&'static str, Props)>| {
        objects.insert(
            ObjectPath::try_from(path).expect("an object path"),
            interfaces.into_iter().collect(),
        );
    };

    put(
        NM.to_owned(),
        vec![("org.freedesktop.NetworkManager", manager())],
    );
    put(
        AP.to_owned(),
        vec![("org.freedesktop.NetworkManager.AccessPoint", access_point())],
    );
    for (n, active) in active_connections() {
        put(
            nm(&format!("ActiveConnection/{n}")),
            vec![("org.freedesktop.NetworkManager.Connection.Active", active)],
        );
    }
    put(
        nm("AgentManager"),
        vec![("org.freedesktop.NetworkManager.AgentManager", Props::new())],
    );
    put(
        nm("DHCP4Config/14"),
        vec![("org.freedesktop.NetworkManager.DHCP4Config", dhcp4())],
    );
    put(
        nm("DHCP6Config/14"),
        vec![("org.freedesktop.NetworkManager.DHCP6Config", dhcp6())],
    );
    for (n, interfaces) in devices() {
        put(nm(&format!("Devices/{n}")), interfaces);
    }
    put(
        nm("DnsManager"),
        vec![("org.freedesktop.NetworkManager.DnsManager", dns_manager())],
    );
    for (n, config) in ip4_configs() {
        put(
            nm(&format!("IP4Config/{n}")),
            vec![("org.freedesktop.NetworkManager.IP4Config", config)],
        );
    }
    for (n, config) in ip6_configs() {
        put(
            nm(&format!("IP6Config/{n}")),
            vec![("org.freedesktop.NetworkManager.IP6Config", config)],
        );
    }
    put(
        nm("Settings"),
        vec![(
            "org.freedesktop.NetworkManager.Settings",
            props([
                ("CanModify", Value::from(true)),
                (
                    "Connections",
                    ao(&[
                        &nm("Settings/1"),
                        &nm("Settings/2"),
                        &nm("Settings/3"),
                        &nm("Settings/4"),
                        &nm("Settings/5"),
                    ]),
                ),
                ("Hostname", s(HOSTNAME)),
            ]),
        )],
    );
    for (n, file, flags, unsaved, version) in [
        (
            1,
            format!("/etc/NetworkManager/system-connections/{ACTIVE_ID}.nmconnection"),
            0u32,
            false,
            14u64,
        ),
        (
            2,
            "/run/NetworkManager/system-connections/lo.nmconnection".to_owned(),
            15,
            true,
            2,
        ),
        (
            3,
            "/run/NetworkManager/system-connections/tailscale0.nmconnection".to_owned(),
            15,
            true,
            3,
        ),
        (
            4,
            format!("/etc/NetworkManager/system-connections/{OTHER_ID}.nmconnection"),
            0,
            false,
            3,
        ),
        (
            5,
            "/etc/NetworkManager/system-connections/usb-tether.nmconnection".to_owned(),
            0,
            false,
            3,
        ),
    ] {
        put(
            nm(&format!("Settings/{n}")),
            vec![(
                "org.freedesktop.NetworkManager.Settings.Connection",
                props([
                    ("Filename", s(&file)),
                    ("Flags", Value::from(flags)),
                    ("Unsaved", Value::from(unsaved)),
                    ("VersionId", Value::from(version)),
                ]),
            )],
        );
    }

    serialise(&objects)
}

fn manager() -> Props {
    let devices = ao(&[
        &nm("Devices/1"),
        &nm("Devices/2"),
        &nm("Devices/4"),
        &nm("Devices/15"),
    ]);
    props([
        ("ActivatingConnection", o(ROOT)),
        (
            "ActiveConnections",
            ao(&[
                &nm("ActiveConnection/16"),
                &nm("ActiveConnection/3"),
                &nm("ActiveConnection/1"),
            ]),
        ),
        ("AllDevices", devices.try_clone().expect("an array clones")),
        ("Capabilities", Value::from(vec![1u32])),
        ("Checkpoints", ao(&[])),
        ("Connectivity", Value::from(4u32)),
        ("ConnectivityCheckAvailable", Value::from(true)),
        ("ConnectivityCheckEnabled", Value::from(true)),
        (
            "ConnectivityCheckUri",
            s("http://connectivity.example.invalid/check"),
        ),
        ("Devices", devices),
        ("GlobalDnsConfiguration", dict([])),
        ("Metered", Value::from(4u32)),
        ("NetworkingEnabled", Value::from(true)),
        ("PrimaryConnection", o(&nm("ActiveConnection/16"))),
        ("PrimaryConnectionType", s("802-11-wireless")),
        ("RadioFlags", Value::from(1u32)),
        ("Startup", Value::from(false)),
        ("State", Value::from(70u32)),
        ("Version", s("1.56.1")),
        ("VersionInfo", Value::from(vec![79873u32, 7])),
        ("WimaxEnabled", Value::from(false)),
        ("WimaxHardwareEnabled", Value::from(false)),
        ("WirelessEnabled", Value::from(true)),
        ("WirelessHardwareEnabled", Value::from(true)),
        ("WwanEnabled", Value::from(true)),
        ("WwanHardwareEnabled", Value::from(true)),
    ])
}

/// The one access point in NM's list: the joined one. NM ages an idle scan
/// list down to the associated access point, so one is the steady state.
fn access_point() -> Props {
    props([
        ("Bandwidth", Value::from(160u32)),
        ("Flags", Value::from(1u32)),
        ("Frequency", Value::from(5200u32)),
        ("HwAddress", s("02:5E:00:00:01:01")),
        ("LastSeen", Value::from(960510i32)),
        ("MaxBitrate", Value::from(270000u32)),
        ("Mode", Value::from(2u32)),
        ("RsnFlags", Value::from(1160u32)),
        ("Ssid", Value::from(ACTIVE_ID.as_bytes().to_vec())),
        // The `good` bucket, one point under `excellent`.
        ("Strength", Value::from(79u8)),
        ("WpaFlags", Value::from(0u32)),
    ])
}

fn active_connections() -> Vec<(u32, Props)> {
    let active = |settings: u32,
                  default: bool,
                  device: u32,
                  dhcp: Option<u32>,
                  id: &str,
                  config: u32,
                  specific: &str,
                  flags: u32,
                  kind: &str,
                  uuid: &str| {
        let dhcp4 = dhcp
            .map(|n| nm(&format!("DHCP4Config/{n}")))
            .unwrap_or_else(|| ROOT.to_owned());
        let dhcp6 = dhcp
            .map(|n| nm(&format!("DHCP6Config/{n}")))
            .unwrap_or_else(|| ROOT.to_owned());
        props([
            ("Connection", o(&nm(&format!("Settings/{settings}")))),
            ("Controller", o(ROOT)),
            ("Default", Value::from(default)),
            ("Default6", Value::from(default)),
            ("Devices", ao(&[&nm(&format!("Devices/{device}"))])),
            ("Dhcp4Config", o(&dhcp4)),
            ("Dhcp6Config", o(&dhcp6)),
            ("Id", s(id)),
            ("Ip4Config", o(&nm(&format!("IP4Config/{config}")))),
            ("Ip6Config", o(&nm(&format!("IP6Config/{config}")))),
            ("Master", o(ROOT)),
            ("SpecificObject", o(specific)),
            ("State", Value::from(2u32)),
            ("StateFlags", Value::from(flags)),
            ("Type", s(kind)),
            ("Uuid", s(uuid)),
            ("Vpn", Value::from(false)),
        ])
    };
    vec![
        (
            1,
            active(
                2,
                false,
                1,
                None,
                "lo",
                1,
                ROOT,
                220,
                "loopback",
                "5e7e0000-0000-4000-8000-000000000001",
            ),
        ),
        (
            16,
            active(
                1,
                true,
                2,
                Some(14),
                ACTIVE_ID,
                2,
                AP,
                92,
                "802-11-wireless",
                ACTIVE_UUID,
            ),
        ),
        (
            3,
            active(
                3,
                false,
                4,
                None,
                "tailscale0",
                3,
                ROOT,
                220,
                "tun",
                "5e7e0000-0000-4000-8000-000000000003",
            ),
        ),
    ]
}

fn dhcp4() -> Props {
    props([(
        "Options",
        dict([
            ("broadcast_address", s("192.0.2.255")),
            ("dhcp_client_identifier", s("01:02:5e:00:00:00:01")),
            ("dhcp_lease_time", s("7200")),
            ("dhcp_server_identifier", s("192.0.2.1")),
            ("domain_name_servers", s("198.51.100.53 198.51.100.54")),
            ("expiry", s("1700007200")),
            ("host_name", s("workstation")),
            ("ip_address", s("192.0.2.118")),
            ("next_server", s("0.0.0.0")),
            ("ntp_servers", s("198.51.100.123")),
            ("requested_broadcast_address", s("1")),
            ("requested_domain_name", s("1")),
            ("requested_domain_name_servers", s("1")),
            ("requested_host_name", s("1")),
            ("requested_routers", s("1")),
            ("requested_subnet_mask", s("1")),
            ("routers", s("192.0.2.1")),
            ("subnet_mask", s("255.255.255.0")),
        ]),
    )])
}

fn dhcp6() -> Props {
    props([(
        "Options",
        dict([
            (
                "dhcp6_client_id",
                s("00:04:5e:5e:5e:5e:00:00:40:00:80:00:00:00:00:00:00:01"),
            ),
            ("dhcp6_name_servers", s("2001:db8:53::1 2001:db8:53::2")),
            ("iaid", s("5e:00:00:01")),
            ("ip6_address", s("2001:db8:5e:ab00::1000")),
        ]),
    )])
}

fn devices() -> Vec<(u32, Vec<(&'static str, Props)>)> {
    let device = |active: &str,
                  available: &[&str],
                  caps: u32,
                  kind: u32,
                  dhcp: Option<u32>,
                  driver: &str,
                  driver_version: &str,
                  firmware: &str,
                  hw: &str,
                  iface: &str,
                  flags: u32,
                  config: Option<u32>,
                  connectivity: u32,
                  metered: u32,
                  mtu: u32,
                  path: &str,
                  state: u32,
                  udi: &str| {
        let dhcp4 = dhcp
            .map(|n| nm(&format!("DHCP4Config/{n}")))
            .unwrap_or_else(|| ROOT.to_owned());
        let dhcp6 = dhcp
            .map(|n| nm(&format!("DHCP6Config/{n}")))
            .unwrap_or_else(|| ROOT.to_owned());
        let ip4 = config
            .map(|n| nm(&format!("IP4Config/{n}")))
            .unwrap_or_else(|| ROOT.to_owned());
        let ip6 = config
            .map(|n| nm(&format!("IP6Config/{n}")))
            .unwrap_or_else(|| ROOT.to_owned());
        props([
            ("ActiveConnection", o(active)),
            ("Autoconnect", Value::from(true)),
            ("AvailableConnections", ao(available)),
            ("Capabilities", Value::from(caps)),
            ("DeviceType", Value::from(kind)),
            ("Dhcp4Config", o(&dhcp4)),
            ("Dhcp6Config", o(&dhcp6)),
            ("Driver", s(driver)),
            ("DriverVersion", s(driver_version)),
            ("FirmwareMissing", Value::from(false)),
            ("FirmwareVersion", s(firmware)),
            ("HwAddress", s(hw)),
            ("Interface", s(iface)),
            ("InterfaceFlags", Value::from(flags)),
            ("Ip4Address", Value::from(0u32)),
            ("Ip4Config", o(&ip4)),
            ("Ip4Connectivity", Value::from(connectivity)),
            ("Ip6Config", o(&ip6)),
            ("Ip6Connectivity", Value::from(connectivity)),
            ("IpInterface", s(if config.is_some() { iface } else { "" })),
            ("LldpNeighbors", no_dicts()),
            ("Managed", Value::from(true)),
            ("Metered", Value::from(metered)),
            ("Mtu", Value::from(mtu)),
            ("NmPluginMissing", Value::from(false)),
            ("Path", s(path)),
            ("PhysicalPortId", s("")),
            ("Ports", ao(&[])),
            ("Real", Value::from(true)),
            ("State", Value::from(state)),
            ("StateReason", Value::from((state, 0u32))),
            ("Udi", s(udi)),
        ])
    };
    let statistics = |rx: u64, tx: u64| {
        props([
            ("RefreshRateMs", Value::from(0u32)),
            ("RxBytes", Value::from(rx)),
            ("TxBytes", Value::from(tx)),
        ])
    };
    let wifi_hw = "02:5E:00:00:00:01";

    vec![
        (
            1,
            vec![
                (
                    "org.freedesktop.NetworkManager.Device",
                    device(
                        &nm("ActiveConnection/1"),
                        &[&nm("Settings/2")],
                        5,
                        32,
                        None,
                        "unknown",
                        "",
                        "",
                        "00:00:00:00:00:00",
                        "lo",
                        65539,
                        Some(1),
                        0,
                        4,
                        65536,
                        "",
                        100,
                        "/sys/devices/virtual/net/lo",
                    ),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.Loopback",
                    Props::new(),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.Statistics",
                    statistics(4096, 4096),
                ),
            ],
        ),
        (
            2,
            vec![
                (
                    "org.freedesktop.NetworkManager.Device",
                    device(
                        &nm("ActiveConnection/16"),
                        &[&nm("Settings/1")],
                        1,
                        2,
                        Some(14),
                        "iwlwifi",
                        "6.12.0",
                        "1.0.0",
                        wifi_hw,
                        "wlo1",
                        65539,
                        Some(2),
                        4,
                        4,
                        1500,
                        "pci-0000:00:14.3",
                        100,
                        "/sys/devices/pci0000:00/0000:00:14.3/net/wlo1",
                    ),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.Statistics",
                    statistics(123456789, 12345678),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.Wireless",
                    props([
                        ("AccessPoints", ao(&[AP])),
                        ("ActiveAccessPoint", o(AP)),
                        ("Bitrate", Value::from(2401900u32)),
                        ("HwAddress", s(wifi_hw)),
                        ("LastScan", Value::from(960510254i64)),
                        ("Mode", Value::from(2u32)),
                        // A randomised MAC is locally administered by definition; the
                        // permanent one is invented in the same range.
                        ("PermHwAddress", s("02:5E:00:00:00:02")),
                        ("WirelessCapabilities", Value::from(3967u32)),
                    ]),
                ),
            ],
        ),
        (
            4,
            vec![
                (
                    "org.freedesktop.NetworkManager.Device",
                    device(
                        &nm("ActiveConnection/3"),
                        &[&nm("Settings/3")],
                        5,
                        16,
                        None,
                        "tun",
                        "1.6",
                        "",
                        "",
                        "tailscale0",
                        65539,
                        Some(3),
                        3,
                        4,
                        1280,
                        "",
                        100,
                        "/sys/devices/virtual/net/tailscale0",
                    ),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.Statistics",
                    statistics(0, 0),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.Tun",
                    props([
                        ("Group", Value::from(-1i64)),
                        ("HwAddress", s("")),
                        ("Mode", s("tun")),
                        ("MultiQueue", Value::from(false)),
                        ("NoPi", Value::from(true)),
                        ("Owner", Value::from(-1i64)),
                        ("VnetHdr", Value::from(true)),
                    ]),
                ),
            ],
        ),
        (
            15,
            vec![
                (
                    "org.freedesktop.NetworkManager.Device",
                    device(
                        ROOT,
                        &[],
                        1,
                        30,
                        None,
                        "",
                        "",
                        "",
                        "",
                        "p2p-dev-wlo1",
                        0,
                        None,
                        1,
                        0,
                        0,
                        "",
                        30,
                        "/virtual/device/placeholder/13",
                    ),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.Statistics",
                    statistics(0, 0),
                ),
                (
                    "org.freedesktop.NetworkManager.Device.WifiP2P",
                    props([("HwAddress", s("")), ("Peers", ao(&[]))]),
                ),
            ],
        ),
    ]
}

fn dns_manager() -> Props {
    props([
        (
            "Configuration",
            dicts(vec![
                entry([
                    ("interface", s("wlo1")),
                    ("nameservers", strings(&["198.51.100.53", "198.51.100.54"])),
                    ("priority", Value::from(100i32)),
                    ("vpn", Value::from(false)),
                ]),
                entry([
                    ("interface", s("wlo1")),
                    (
                        "nameservers",
                        strings(&["2001:db8:53::1", "2001:db8:53::2"]),
                    ),
                    ("priority", Value::from(100i32)),
                    ("vpn", Value::from(false)),
                ]),
            ]),
        ),
        ("Mode", s("systemd-resolved")),
        ("RcManager", s("unmanaged")),
    ])
}

/// The tailnet: this machine and three peers, in the shared range a tailnet
/// draws from.
const TAILNET_SELF: &str = "100.64.0.10";
const TAILNET_PEERS: [&str; 3] = ["100.64.0.11", "100.64.0.12", "100.64.0.13"];
const TAILNET6_SELF: &str = "fd7a:115c:a1e0::a";
const TAILNET6_PEERS: [&str; 3] = [
    "fd7a:115c:a1e0::b",
    "fd7a:115c:a1e0::c",
    "fd7a:115c:a1e0::d",
];

fn ip4_config(
    address: &str,
    prefix: u32,
    gateway: &str,
    priority: i32,
    nameservers: &[&str],
    route_data: Vec<HashMap<String, Value<'static>>>,
    routes: Vec<Vec<u32>>,
) -> Props {
    let gateway_u32 = if gateway.is_empty() { 0 } else { ip4(gateway) };
    props([
        (
            "AddressData",
            dicts(vec![entry([
                ("address", s(address)),
                ("prefix", Value::from(prefix)),
            ])]),
        ),
        (
            "Addresses",
            Value::from(vec![vec![ip4(address), prefix, gateway_u32]]),
        ),
        ("DnsOptions", strings(&[])),
        ("DnsPriority", Value::from(priority)),
        ("Domains", strings(&[])),
        ("Gateway", s(gateway)),
        (
            "NameserverData",
            dicts(
                nameservers
                    .iter()
                    .map(|n| entry([("address", s(n)), ("uri", s(n))]))
                    .collect(),
            ),
        ),
        (
            "Nameservers",
            Value::from(nameservers.iter().map(|n| ip4(n)).collect::<Vec<u32>>()),
        ),
        ("RouteData", dicts(route_data)),
        ("Routes", Value::from(routes)),
        ("Searches", strings(&[])),
        ("WinsServerData", strings(&[])),
        ("WinsServers", Value::from(Vec::<u32>::new())),
    ])
}

fn route(
    dest: &str,
    prefix: u32,
    metric: u32,
    next_hop: Option<&str>,
    table: Option<u32>,
) -> HashMap<String, Value<'static>> {
    let mut route = entry([
        ("dest", s(dest)),
        ("metric", Value::from(metric)),
        ("prefix", Value::from(prefix)),
    ]);
    if let Some(hop) = next_hop {
        route.insert("next-hop".to_owned(), s(hop));
    }
    if let Some(table) = table {
        route.insert("table".to_owned(), Value::from(table));
    }
    route
}

fn ip4_configs() -> Vec<(u32, Props)> {
    let mut tailnet_routes: Vec<_> = TAILNET_PEERS
        .iter()
        .map(|peer| route(peer, 32, 0, None, Some(52)))
        .collect();
    tailnet_routes.push(route("100.100.100.100", 32, 0, None, Some(52)));
    vec![
        (
            1,
            ip4_config("127.0.0.1", 8, "", 0, &[], Vec::new(), Vec::new()),
        ),
        (
            2,
            ip4_config(
                "192.0.2.118",
                24,
                "192.0.2.1",
                100,
                &["198.51.100.53", "198.51.100.54"],
                vec![
                    route("192.0.2.0", 24, 600, None, None),
                    route("0.0.0.0", 0, 600, Some("192.0.2.1"), None),
                ],
                vec![vec![ip4("192.0.2.0"), 24, 0, 600]],
            ),
        ),
        (
            3,
            ip4_config(TAILNET_SELF, 32, "", 0, &[], tailnet_routes, Vec::new()),
        ),
    ]
}

fn ip6_config(
    addresses: &[(&str, u32)],
    gateway: &str,
    priority: i32,
    nameservers: &[&str],
    route_data: Vec<HashMap<String, Value<'static>>>,
    routes: Vec<(&str, u32, &str, u32)>,
) -> Props {
    let gateway_bytes = if gateway.is_empty() {
        ip6("::")
    } else {
        ip6(gateway)
    };
    props([
        (
            "AddressData",
            dicts(
                addresses
                    .iter()
                    .map(|(a, p)| entry([("address", s(a)), ("prefix", Value::from(*p))]))
                    .collect(),
            ),
        ),
        (
            "Addresses",
            Value::from(
                addresses
                    .iter()
                    .map(|(a, p)| (ip6(a), *p, gateway_bytes.clone()))
                    .collect::<Vec<_>>(),
            ),
        ),
        ("DnsOptions", strings(&[])),
        ("DnsPriority", Value::from(priority)),
        ("Domains", strings(&[])),
        ("Gateway", s(gateway)),
        (
            "Nameservers",
            Value::from(nameservers.iter().map(|n| ip6(n)).collect::<Vec<_>>()),
        ),
        ("RouteData", dicts(route_data)),
        (
            "Routes",
            Value::from(
                routes
                    .iter()
                    .map(|(d, p, h, m)| (ip6(d), *p, ip6(h), *m))
                    .collect::<Vec<_>>(),
            ),
        ),
        ("Searches", strings(&[])),
    ])
}

fn ip6_configs() -> Vec<(u32, Props)> {
    let gateway = "fe80::5e:ff:fe00:101";
    let mut tailnet_routes = vec![
        route("fe80::", 64, 256, None, None),
        route("fd7a:115c:a1e0::", 48, 0, None, Some(52)),
    ];
    tailnet_routes.extend(
        TAILNET6_PEERS
            .iter()
            .map(|peer| route(peer, 128, 0, None, Some(52))),
    );
    vec![
        (
            1,
            ip6_config(&[("::1", 128)], "", 0, &[], Vec::new(), Vec::new()),
        ),
        (
            2,
            ip6_config(
                &[
                    ("2001:db8:5e:ab00::1000", 128),
                    ("fd00:5e:1::1000", 64),
                    ("fd00:5e:2::1000", 64),
                    ("fe80::5e:ff:fe00:1", 64),
                ],
                gateway,
                100,
                &["2001:db8:53::1", "2001:db8:53::2"],
                vec![
                    route("fe80::", 64, 1024, None, None),
                    route("fd00:5e:1::", 64, 600, None, None),
                    route("fd00:5e:2::", 64, 600, None, None),
                    route("2001:db8:5e:ab00::", 64, 600, None, None),
                    route("::", 0, 600, Some(gateway), None),
                ],
                vec![
                    ("fe80::", 64, "::", 1024),
                    ("fd00:5e:1::", 64, "::", 600),
                    ("2001:db8:5e:ab00::", 64, "::", 600),
                ],
            ),
        ),
        (
            3,
            ip6_config(
                &[(TAILNET6_SELF, 128), ("fe80::5e:ff:fe00:a", 64)],
                "",
                0,
                &[],
                tailnet_routes,
                vec![("fe80::", 64, "::", 256)],
            ),
        ),
    ]
}

// ---------------------------------------------------------------------------
// GetSettings
// ---------------------------------------------------------------------------

type Groups = BTreeMap<&'static str, Props>;

fn wifi_settings(
    id: &str,
    ssid: &str,
    uuid: &str,
    bssids: &[&str],
    key_mgmt: &str,
    timestamp: u64,
    hotspot: bool,
) -> Vec<u8> {
    let mut connection = props([
        ("id", s(id)),
        ("interface-name", s("wlo1")),
        ("permissions", strings(&[])),
        ("timestamp", Value::from(timestamp)),
        ("type", s("802-11-wireless")),
        ("uuid", s(uuid)),
    ]);
    if hotspot {
        // A phone's hotspot: joined by hand and metered.
        connection.insert("autoconnect", Value::from(false));
        connection.insert("metered", Value::from(1i32));
    }
    let ip = |addresses: Value<'static>, routes: Value<'static>| {
        props([
            ("address-data", no_dicts()),
            ("addresses", addresses),
            ("method", s("auto")),
            ("route-data", no_dicts()),
            ("routes", routes),
        ])
    };
    let groups: Groups = [
        (
            "802-11-wireless",
            props([
                ("mode", s("infrastructure")),
                ("security", s("802-11-wireless-security")),
                ("seen-bssids", strings(bssids)),
                ("ssid", Value::from(ssid.as_bytes().to_vec())),
            ]),
        ),
        // `wpa-psk` here is a *value*; the key itself never comes back from
        // `GetSettings`, which is what `secret_keys` checks.
        (
            "802-11-wireless-security",
            props([("key-mgmt", s(key_mgmt))]),
        ),
        ("connection", connection),
        (
            "ipv4",
            ip(
                Value::from(Vec::<Vec<u32>>::new()),
                Value::from(Vec::<Vec<u32>>::new()),
            ),
        ),
        (
            "ipv6",
            ip(
                Value::from(Vec::<(Vec<u8>, u32, Vec<u8>)>::new()),
                Value::from(Vec::<(Vec<u8>, u32, Vec<u8>, u32)>::new()),
            ),
        ),
        ("proxy", Props::new()),
    ]
    .into_iter()
    .collect();
    serialise(&groups)
}

/// `a{sa{sv}}` for the joined profile.
pub fn settings_wifi_active() -> Vec<u8> {
    wifi_settings(
        ACTIVE_ID,
        ACTIVE_ID,
        ACTIVE_UUID,
        &["02:5E:00:00:01:01", "02:5E:00:00:01:02"],
        "sae",
        1700000000,
        false,
    )
}

/// `a{sa{sv}}` for the known profile that is out of range.
pub fn settings_wifi_other() -> Vec<u8> {
    wifi_settings(
        OTHER_ID,
        OTHER_SSID,
        OTHER_UUID,
        &["02:5E:00:00:02:01"],
        "wpa-psk",
        1699900000,
        true,
    )
}

/// Every fixture this module writes, by file name under `tests/data/`.
pub fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("managed-objects.bin", managed_objects()),
        ("settings-wifi-active.bin", settings_wifi_active()),
        ("settings-wifi-other.bin", settings_wifi_other()),
    ]
}
