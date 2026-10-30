//! A fingerprint reader for a machine without one: fprintd's Manager and one
//! Device on the bus `DBUS_SYSTEM_BUS_ADDRESS` names, for the lock's tests
//! and `bar/bar-test.sh`.
//!
//! Not a test of fprintd: a fixture with the calls `caustics-lock` makes
//! (`GetDefaultDevice`, `Claim`, `VerifyStart`, `VerifyStop`, `Release`, the
//! `name` and `scan-type` properties) and a `VerifyStatus` for each verify,
//! read from a script. Scripted by its environment:
//!
//! - `FAKE_FPRINTD_SCRIPT`: a comma list consumed one item per
//!   `VerifyStart`, each item one or more results joined by `+`, every one
//!   but the last sent with `done` false (`no-match,retry-scan+match,match`).
//!   A result is fprintd's name without its `verify-`. A verify started
//!   once the script is spent never ends, as a real one waits for a finger.
//! - `FAKE_FPRINTD_DELAY_MS`: the pause after each `VerifyStart` and between
//!   the results of one item (default 300).
//! - `FAKE_FPRINTD_SCAN_TYPE`: the device's `scan-type` (default `press`).
//! - `FAKE_FPRINTD_MODE`: `normal`; `no-device` (`GetDefaultDevice` is
//!   `NoSuchDevice`); `no-enrolled` (`VerifyStart` is `NoEnrolledPrints`);
//!   `in-use` (`Claim` is `AlreadyInUse`); `refused` (`Claim` is
//!   `PermissionDenied`, as when polkit refuses it).
//!
//! It prints one line per call on stderr (`fake-fprintd: Claim <user>`,
//! `VerifyStart any`, `VerifyStatus <result> <done>`, `VerifyStop`,
//! `Release`), so a test can say the reader was let go. It serves until it
//! is stopped.
//!
//! ```text
//! cargo build -p caustics-lock --example fake_fprintd
//! DBUS_SYSTEM_BUS_ADDRESS=unix:path=… target/debug/examples/fake_fprintd
//! ```
//!
//! The lock's own tests include this file as a module and serve it
//! in-process on a private bus ([`serve`]).

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

pub const SERVICE: &str = "net.reactivated.Fprint";
pub const MANAGER: &str = "/net/reactivated/Fprint/Manager";
pub const DEVICE: &str = "/net/reactivated/Fprint/Device/0";
const DEVICE_INTERFACE: &str = "net.reactivated.Fprint.Device";

/// What the device does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    NoDevice,
    NoEnrolled,
    InUse,
    Refused,
}

impl Mode {
    pub fn parse(mode: &str) -> Option<Mode> {
        match mode {
            "" | "normal" => Some(Mode::Normal),
            "no-device" => Some(Mode::NoDevice),
            "no-enrolled" => Some(Mode::NoEnrolled),
            "in-use" => Some(Mode::InUse),
            "refused" => Some(Mode::Refused),
            _ => None,
        }
    }
}

/// The fake's script.
#[derive(Clone, Debug)]
pub struct Script {
    /// One entry per `VerifyStart`: the results it sends, in order.
    pub verifies: VecDeque<Vec<String>>,
    pub delay: Duration,
    pub scan_type: String,
    pub mode: Mode,
}

impl Script {
    /// `no-match,retry-scan+match` and the rest.
    pub fn parse(script: &str, delay: Duration, scan_type: &str, mode: Mode) -> Script {
        Script {
            verifies: script
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| {
                    item.split('+')
                        .map(|result| format!("verify-{}", result.trim()))
                        .collect()
                })
                .collect(),
            delay,
            scan_type: scan_type.to_owned(),
            mode,
        }
    }
}

/// Every call the fake was made, in order, as its stderr lines say them.
#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn say(&self, line: String) {
        let _ = std::io::stderr().write_all(format!("fake-fprintd: {line}\n").as_bytes());
        self.0.lock().unwrap().push(line);
    }

    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    pub fn count(&self, line: &str) -> usize {
        self.lines().iter().filter(|l| *l == line).count()
    }
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "net.reactivated.Fprint.Error")]
pub enum FprintError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSuchDevice(String),
    NoEnrolledPrints(String),
    AlreadyInUse(String),
    PermissionDenied(String),
    ClaimDevice(String),
}

struct State {
    script: Script,
    claimed: bool,
    /// Bumped by every `VerifyStop` and `Release`: a pending verify's
    /// results are for the generation it started in, and no other.
    generation: u64,
}

struct Manager {
    mode: Mode,
    log: Log,
}

#[zbus::interface(name = "net.reactivated.Fprint.Manager")]
impl Manager {
    fn get_default_device(&self) -> Result<OwnedObjectPath, FprintError> {
        self.log.say("GetDefaultDevice".to_owned());
        if self.mode == Mode::NoDevice {
            return Err(FprintError::NoSuchDevice("No devices available".to_owned()));
        }
        Ok(OwnedObjectPath::try_from(DEVICE).expect("a valid path"))
    }
}

struct Device {
    state: Arc<Mutex<State>>,
    log: Log,
}

#[zbus::interface(name = "net.reactivated.Fprint.Device")]
impl Device {
    fn claim(&self, username: &str) -> Result<(), FprintError> {
        self.log.say(format!("Claim {username}"));
        let mut state = self.state.lock().unwrap();
        if state.script.mode == Mode::InUse {
            return Err(FprintError::AlreadyInUse(
                "Device was already claimed".to_owned(),
            ));
        }
        if state.script.mode == Mode::Refused {
            return Err(FprintError::PermissionDenied(
                "Not Authorized: net.reactivated.fprint.device.verify".to_owned(),
            ));
        }
        state.claimed = true;
        Ok(())
    }

    fn release(&self) -> Result<(), FprintError> {
        self.log.say("Release".to_owned());
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        if !std::mem::replace(&mut state.claimed, false) {
            return Err(FprintError::ClaimDevice(
                "Device was not claimed".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_start(
        &self,
        finger_name: &str,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), FprintError> {
        self.log.say(format!("VerifyStart {finger_name}"));
        let mut state = self.state.lock().unwrap();
        if !state.claimed {
            return Err(FprintError::ClaimDevice(
                "Device was not claimed".to_owned(),
            ));
        }
        if state.script.mode == Mode::NoEnrolled {
            return Err(FprintError::NoEnrolledPrints(
                "No fingerprints enrolled".to_owned(),
            ));
        }
        let Some(results) = state.script.verifies.pop_front() else {
            // Spent: a verify that waits for a finger that never comes.
            return Ok(());
        };
        let generation = state.generation;
        let delay = state.script.delay;
        let shared = self.state.clone();
        let conn = conn.clone();
        let log = self.log.clone();
        tokio::spawn(async move {
            let last = results.len().saturating_sub(1);
            for (i, result) in results.into_iter().enumerate() {
                tokio::time::sleep(delay).await;
                if shared.lock().unwrap().generation != generation {
                    return;
                }
                let done = i == last;
                log.say(format!("VerifyStatus {result} {done}"));
                let _ = conn
                    .emit_signal(
                        None::<&str>,
                        DEVICE,
                        DEVICE_INTERFACE,
                        "VerifyStatus",
                        &(result.as_str(), done),
                    )
                    .await;
            }
        });
        Ok(())
    }

    fn verify_stop(&self) -> Result<(), FprintError> {
        self.log.say("VerifyStop".to_owned());
        self.state.lock().unwrap().generation += 1;
        Ok(())
    }

    /// fprintd's own name for it, lowercase as its introspection has it.
    #[zbus(property, name = "name")]
    fn name(&self) -> String {
        "Fake Fingerprint Reader".to_owned()
    }

    #[zbus(property, name = "scan-type")]
    fn scan_type(&self) -> String {
        self.state.lock().unwrap().script.scan_type.clone()
    }
}

/// Serve the fake on `conn` and take fprintd's name there. The log fills as
/// the calls come.
pub async fn serve(conn: &Connection, script: Script) -> zbus::Result<Log> {
    let log = Log::default();
    conn.object_server()
        .at(
            MANAGER,
            Manager {
                mode: script.mode,
                log: log.clone(),
            },
        )
        .await?;
    conn.object_server()
        .at(
            DEVICE,
            Device {
                state: Arc::new(Mutex::new(State {
                    script,
                    claimed: false,
                    generation: 0,
                })),
                log: log.clone(),
            },
        )
        .await?;
    conn.request_name(SERVICE).await?;
    Ok(log)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let delay = var("FAKE_FPRINTD_DELAY_MS").parse().unwrap_or(300);
    let scan_type = Some(var("FAKE_FPRINTD_SCAN_TYPE"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "press".to_owned());
    let Some(mode) = Mode::parse(&var("FAKE_FPRINTD_MODE")) else {
        eprintln!(
            "fake-fprintd: FAKE_FPRINTD_MODE is not one of normal, no-device, no-enrolled, in-use, refused"
        );
        std::process::exit(2);
    };
    let script = Script::parse(
        &var("FAKE_FPRINTD_SCRIPT"),
        Duration::from_millis(delay),
        &scan_type,
        mode,
    );
    let conn = Connection::system().await?;
    serve(&conn, script).await?;
    eprintln!("fake-fprintd: ready, {SERVICE} on the system bus");
    std::future::pending::<()>().await;
    Ok(())
}
