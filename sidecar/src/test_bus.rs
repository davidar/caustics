//! A private `dbus-daemon` for one test.
//!
//! Not a peer-to-peer pair: zbus's `p2p` feature is off in this workspace and
//! turning it on brings `uuid` in, and a p2p message carries no sender, so a
//! match rule on a service's name would never match a signal. The deadlock
//! tests of the D-Bus sidecars (tray, network, bluetooth, power, notify) start one of these, put a peer on it that plays the service, and point the
//! sidecar's own loop at it.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// A private session bus, killed when it drops.
pub struct Bus {
    child: Child,
    /// The address to hand `zbus::connection::Builder::address`.
    pub address: String,
}

impl Bus {
    /// Start one, or `None` when there is no `dbus-daemon` on this machine —
    /// in which case the test says so and passes.
    pub fn start() -> Option<Bus> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
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
        Some(Bus { child, address })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
