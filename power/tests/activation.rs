//! A daemon that left is not started again by the sidecar's next read.
//!
//! UPower, `net.hadess.PowerProfiles` and `org.freedesktop.UPower.PowerProfiles`
//! are commonly D-Bus-activatable, and zbus sets no `NO_AUTO_START`: the read
//! that a `NameOwnerChanged` wake makes would start tuned-ppd again within a
//! second, so `systemctl stop tuned-ppd` would never stick.
//! `caustics_sidecar::test_bus` has no activation files, so a test there passes
//! without describing a real system bus. This bus has them: each service file's `Exec` makes a
//! marker named after the service and fails without taking the name, so the
//! bus answers the activating call with an error and the marker says that
//! activation was asked for.
//!
//! The first read may activate — a sidecar started at login before tuned-ppd
//! is how the daemon gets started — and the test says so; every read after it
//! must not.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use caustics_power::profiles::{self, HADESS, Profiles, UPOWER};
use caustics_power::upower;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

/// A private session bus whose service directory activates the three names.
struct ActivatingBus {
    child: Child,
    address: String,
    dir: PathBuf,
}

impl ActivatingBus {
    fn start(tag: &str) -> Option<ActivatingBus> {
        let dir = std::env::temp_dir().join(format!(
            "caustics-power-activation-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let services = dir.join("services");
        std::fs::create_dir_all(&services).ok()?;
        for name in [upower::SERVICE, UPOWER.service, HADESS.service] {
            // `mkdir m m`: the first makes the marker, the second fails, and a
            // child that exits non-zero is the bus's `Spawn.ChildExited` at
            // once — one that exits 0 without the name is a 25 s timeout.
            let marker = dir.join(format!("started-{name}"));
            let file = format!(
                "[D-BUS Service]\nName={name}\nExec=/usr/bin/mkdir {0} {0}\n",
                marker.display()
            );
            std::fs::write(services.join(format!("{name}.service")), file).ok()?;
        }
        let config = dir.join("bus.conf");
        let xml = format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <servicedir>{}</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
            services.display()
        );
        std::fs::write(&config, xml).ok()?;
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut line)
            .ok()?;
        let address = line.trim().to_owned();
        if address.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(ActivatingBus {
            child,
            address,
            dir,
        })
    }

    /// Whether anything asked the bus to start `name`; forgets it.
    fn started(&self, name: &str) -> bool {
        std::fs::remove_dir(self.marker(name)).is_ok()
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.dir.join(format!("started-{name}"))
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .expect("the bus address parses")
            .build()
            .await
            .expect("a connection to the private bus")
    }
}

impl Drop for ActivatingBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn no_bus(what: &str) {
    eprintln!("no dbus-daemon to start; {what} is untested here");
}

async fn next<T>(seen: &mut UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the watch said something within 5 s")
        .expect("the watch is still running")
}

async fn until<T: PartialEq + std::fmt::Debug>(seen: &mut UnboundedReceiver<T>, want: T) {
    loop {
        if next(seen).await == want {
            return;
        }
    }
}

fn exists(path: &Path) -> bool {
    path.exists()
}

// -- power profiles ---------------------------------------------------------

struct Daemon;

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl Daemon {
    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        let name = OwnedValue::try_from(Value::from("balanced")).unwrap();
        vec![HashMap::from([("Profile".to_owned(), name)])]
    }
    #[zbus(property)]
    fn active_profile(&self) -> String {
        "balanced".to_owned()
    }
}

#[tokio::test]
async fn a_power_profiles_daemon_that_left_is_not_started_again() {
    let Some(bus) = ActivatingBus::start("profiles") else {
        return no_bus("activation");
    };
    let client = bus.connect().await;
    let (tx, mut seen) = unbounded_channel();
    let task = tokio::spawn(profiles::watch(client, tx));

    // The first read may start the daemon, and here it is asked to: the new
    // name first, and the old one when that fails.
    assert_eq!(next(&mut seen).await, None);
    assert!(
        bus.started(UPOWER.service),
        "the first read asks for the new name"
    );
    assert!(bus.started(HADESS.service), "and falls back to the old one");

    // The daemon arrives on the old name, and is read.
    let peer = bus.connect().await;
    peer.object_server()
        .at(HADESS.path, Daemon)
        .await
        .expect("the daemon's object");
    peer.request_name(HADESS.service)
        .await
        .expect("the peer owns the name");
    let balanced = Profiles {
        names: vec!["balanced".into()],
        active: "balanced".into(),
    };
    until(&mut seen, Some(balanced)).await;

    // It leaves: the wake reads nothing, and starts nothing.
    peer.release_name(HADESS.service)
        .await
        .expect("the peer lets go");
    until(&mut seen, None).await;
    assert!(
        !exists(&bus.marker(HADESS.service)),
        "a read after the first started the old name"
    );
    assert!(
        !exists(&bus.marker(UPOWER.service)),
        "a read after the first started the new name"
    );
    task.abort();
}

// -- UPower -----------------------------------------------------------------

struct Device;

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl Device {
    #[zbus(property)]
    fn is_present(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        64.0
    }
}

#[tokio::test]
async fn a_upower_that_left_is_not_started_again() {
    let Some(bus) = ActivatingBus::start("upower") else {
        return no_bus("activation");
    };
    let client = bus.connect().await;
    let (tx, mut seen) = unbounded_channel();
    let task = tokio::spawn(upower::watch(client, tx));

    assert_eq!(next(&mut seen).await, None);
    assert!(
        bus.started(upower::SERVICE),
        "the first read may start UPower"
    );

    let peer = bus.connect().await;
    peer.object_server()
        .at(upower::DISPLAY_DEVICE, Device)
        .await
        .expect("the device");
    peer.request_name(upower::SERVICE)
        .await
        .expect("the peer owns UPower's name");
    loop {
        if let Some(battery) = next(&mut seen).await {
            assert_eq!(battery.percent, 64.0);
            break;
        }
    }

    peer.release_name(upower::SERVICE)
        .await
        .expect("the peer lets go");
    until(&mut seen, None).await;
    assert!(
        !exists(&bus.marker(upower::SERVICE)),
        "a read after the first started UPower"
    );
    task.abort();
}
