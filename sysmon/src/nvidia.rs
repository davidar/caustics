//! The NVIDIA half of the `gpu` chart, read through NVML without ever being
//! the reason the card is awake.
//!
//! `Nvml::init` loads `libnvidia-ml.so.1`, which opens `/dev/nvidia*`. Opening
//! those nodes resumes a runtime-suspended device, and holding them open
//! keeps it from suspending again. On a hybrid laptop that is the difference
//! between a discrete GPU that sleeps and one that burns watts for a chart.
//! The kernel exposes what this file needs in order to stay out of the way
//! (`Documentation/ABI/testing/sysfs-devices-power`):
//!
//! - `power/runtime_status` reads `active`, `suspended`, `suspending`,
//!   `resuming`, `error` or `unsupported`. Only `active` means the card is
//!   already awake for somebody else's reasons.
//! - `power/autosuspend_delay_ms` is how long the device must go unused
//!   before runtime PM suspends it. Some drivers answer EIO, and then
//!   [`DEFAULT_AUTOSUSPEND`] stands in.
//!
//! The handle is held under a **lease**. It is taken only while the kernel
//! says the device is already active; it is kept while NVML reports other
//! clients on the device; and once the device has gone [`RELEASE_AFTER`]
//! without one it is given back. Giving it back is what lets the card
//! suspend, and the card takes its autosuspend delay to get there, so a
//! handle retaken inside that delay would hold it awake indefinitely. After a
//! release the lease therefore **rests**: it reads the gate without touching
//! NVML until the card reads something other than `active` (the rest did its
//! job, and the gate now rules), or until the delay plus [`REST_MARGIN`] has
//! passed with the card still `active`, which means something else is using
//! it and the lease may be taken again.
//!
//! The NVML calls and the sysfs reads sit behind two traits so the policy is
//! driven by fakes and a fake clock in `tests/guard.rs`, which counts every
//! open and asserts that none happened against a shut gate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::GAP;

/// PCI vendor id of NVIDIA.
pub const VENDOR: &str = "0x10de";
/// How long the device may go without another client before the handle is
/// given back.
pub const RELEASE_AFTER: Duration = Duration::from_secs(3);
/// The autosuspend delay assumed when `power/autosuspend_delay_ms` cannot be
/// read.
pub const DEFAULT_AUTOSUSPEND: Duration = Duration::from_millis(5000);
/// Added to the autosuspend delay to bound a rest: the card has this much
/// longer than its own delay to suspend before an `active` reading is taken
/// to mean another user.
pub const REST_MARGIN: Duration = Duration::from_secs(3);
/// The longest a rest lasts, whatever the delay says.
pub const REST_CAP: Duration = Duration::from_secs(120);

/// What one look at an open handle yields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation {
    /// Busy percent, 0..100, or `None` when the query failed.
    pub busy: Option<f32>,
    /// Graphics and compute clients on the device. This process holds no
    /// context and is not one of them. `None` when NVML cannot say.
    pub clients: Option<u32>,
}

impl Observation {
    /// Whether anything else is using the device: a client when the count is
    /// known, busyness when it is not.
    fn in_use(&self) -> bool {
        match self.clients {
            Some(n) => n > 0,
            None => self.busy.is_some_and(|b| b > 0.0),
        }
    }
}

/// The NVML handle, behind a trait so the lease is tested without a driver,
/// a device or the library.
pub trait Port {
    /// Open the handle. This is the call that can wake the GPU.
    fn open(&mut self) -> Result<(), String>;
    /// Drop the handle, closing `/dev/nvidia*`.
    fn close(&mut self);
    /// Whether the handle is held.
    fn is_open(&self) -> bool;
    /// One look at the device, or `None` when the query failed.
    fn observe(&mut self) -> Option<Observation>;
}

/// `power/runtime_status`, behind a trait for the same reason.
pub trait Gate {
    /// The trimmed contents, or `None` when the file cannot be read.
    fn status(&mut self) -> Option<String>;
}

/// Where the lease stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// No handle. The gate decides whether one may be taken.
    Waiting,
    /// A handle is held; `unused` is how long the device has gone without
    /// another client.
    Held { unused: Duration },
    /// The handle was given back; `elapsed` is how long the rest has lasted.
    Resting { elapsed: Duration },
    /// NVML could not be opened on an active device. Nothing is tried again
    /// this run: a missing driver does not appear later.
    Refused,
}

/// The lease over the NVML handle.
pub struct Lease {
    port: Box<dyn Port>,
    gate: Box<dyn Gate>,
    standing: Standing,
    /// How long a rest may last while the card stays `active`.
    rest_limit: Duration,
    /// The last line said, so a standing that does not change says nothing.
    said: Option<String>,
}

/// How long a rest may last while the card stays `active`: the device's own
/// delay plus [`REST_MARGIN`], capped at [`REST_CAP`].
pub fn rest_limit(autosuspend: Option<Duration>) -> Duration {
    (autosuspend.unwrap_or(DEFAULT_AUTOSUSPEND) + REST_MARGIN).min(REST_CAP)
}

impl Lease {
    pub fn new(port: Box<dyn Port>, gate: Box<dyn Gate>, autosuspend: Option<Duration>) -> Self {
        Self {
            port,
            gate,
            standing: Standing::Waiting,
            rest_limit: rest_limit(autosuspend),
            said: None,
        }
    }

    /// How long a rest may last while the card stays `active`. For the start
    /// witness.
    pub fn rest_limit(&self) -> Duration {
        self.rest_limit
    }

    pub fn standing(&self) -> Standing {
        self.standing
    }

    /// One sample, `elapsed` after the previous one: the busy percent, or
    /// [`GAP`] whenever the handle is not held. `log` takes one line per
    /// change of standing, never one per sample.
    pub fn sample(&mut self, elapsed: Duration, log: &mut dyn FnMut(&str)) -> f32 {
        match self.standing {
            Standing::Refused => GAP,
            Standing::Held { unused } => self.look(unused, elapsed, log),
            Standing::Resting { elapsed: so_far } => {
                let so_far = so_far + elapsed;
                match self.gate.status().as_deref() {
                    Some("active") if so_far < self.rest_limit => {
                        self.standing = Standing::Resting { elapsed: so_far };
                        GAP
                    }
                    Some("active") => {
                        self.say(log, "nvml: the device stayed active through the rest; something else is using it");
                        self.take(log)
                    }
                    other => {
                        self.standing = Standing::Waiting;
                        self.say(
                            log,
                            &format!(
                                "nvml: rest over; the device reads {}",
                                other.unwrap_or("unreadable")
                            ),
                        );
                        GAP
                    }
                }
            }
            Standing::Waiting => match self.gate.status().as_deref() {
                Some("active") => self.take(log),
                Some(other) => {
                    self.say(log, &format!("nvml: the device reads {other}; not opening"));
                    GAP
                }
                None => {
                    self.say(log, "nvml: runtime_status is unreadable; not opening");
                    GAP
                }
            },
        }
    }

    /// The only path that opens the port, reached only through an `active`
    /// gate.
    fn take(&mut self, log: &mut dyn FnMut(&str)) -> f32 {
        if let Err(why) = self.port.open() {
            self.standing = Standing::Refused;
            self.say(
                log,
                &format!("nvml: init failed ({why}); gpu is gaps for this run"),
            );
            return GAP;
        }
        self.say(log, "nvml: opened; the device was already active");
        self.standing = Standing::Held {
            unused: Duration::ZERO,
        };
        match self.port.observe() {
            Some(seen) => seen.busy.unwrap_or(GAP),
            None => GAP,
        }
    }

    /// One look at a held handle, and the clock on how long nobody else has
    /// used the device.
    fn look(&mut self, unused: Duration, elapsed: Duration, log: &mut dyn FnMut(&str)) -> f32 {
        let Some(seen) = self.port.observe() else {
            // A query failing on a held handle is the device going away
            // underneath it. Let go and come back through the gate.
            self.give_back(
                log,
                "nvml: a query failed on the open handle; released".to_owned(),
            );
            return GAP;
        };
        let unused = if seen.in_use() {
            Duration::ZERO
        } else {
            unused + elapsed
        };
        if unused >= RELEASE_AFTER {
            self.give_back(
                log,
                format!(
                    "nvml: released after {} s without another client; resting up to {} s for the device to suspend",
                    RELEASE_AFTER.as_secs(),
                    self.rest_limit.as_secs()
                ),
            );
        } else {
            self.standing = Standing::Held { unused };
        }
        seen.busy.unwrap_or(GAP)
    }

    fn give_back(&mut self, log: &mut dyn FnMut(&str), line: String) {
        self.port.close();
        self.standing = Standing::Resting {
            elapsed: Duration::ZERO,
        };
        self.say(log, &line);
    }

    /// One line per change. Saying the same thing again is silence.
    fn say(&mut self, log: &mut dyn FnMut(&str), line: &str) {
        if self.said.as_deref() == Some(line) {
            return;
        }
        self.said = Some(line.to_owned());
        log(line);
    }
}

// ---------------------------------------------------------------------------
// The device, the gate and the port on a real machine
// ---------------------------------------------------------------------------

/// A discovered NVIDIA device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The PCI device directory (`…/card1/device`, canonicalised).
    pub dev: PathBuf,
    /// `0000:01:00.0`.
    pub slot: String,
    /// `nvidia:card1 = 0000:01:00.0, runtime PM auto`: the discovery witness.
    pub witness: String,
}

/// The first NVIDIA card under `root`.
pub fn discover(root: &Path) -> Option<Device> {
    for card in crate::amd::cards(root) {
        if crate::amd::vendor(&card).as_deref() != Some(VENDOR) {
            continue;
        }
        let dev =
            std::fs::canonicalize(card.join("device")).unwrap_or_else(|_| card.join("device"));
        let slot = crate::amd::slot(&card);
        let name = card
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("card")
            .to_owned();
        let control = std::fs::read_to_string(dev.join("power/control"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| "?".into());
        let witness = format!("nvidia:{name} = {slot}, runtime PM {control}");
        return Some(Device { dev, slot, witness });
    }
    None
}

/// `power/autosuspend_delay_ms`, or `None` when it cannot be read, which is a
/// real case: some drivers answer EIO.
pub fn autosuspend(dev: &Path) -> Option<Duration> {
    std::fs::read_to_string(dev.join("power/autosuspend_delay_ms"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// The live gate: one small `read` per consultation.
pub struct SysfsGate {
    path: PathBuf,
}

impl SysfsGate {
    pub fn new(dev: &Path) -> Self {
        Self {
            path: dev.join("power/runtime_status"),
        }
    }
}

impl Gate for SysfsGate {
    fn status(&mut self) -> Option<String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .map(|s| s.trim().to_owned())
    }
}

/// The live port: an `Option<Nvml>` that is `None` most of the time.
pub struct NvmlPort {
    nvml: Option<nvml_wrapper::Nvml>,
    /// The bus id NVML wants, `00000000:01:00.0`: the sysfs slot with the
    /// domain padded to eight digits.
    bus_id: String,
}

impl NvmlPort {
    pub fn new(slot: &str) -> Self {
        let bus_id = match slot.split_once(':') {
            Some((domain, rest)) => format!("{domain:0>8}:{rest}"),
            None => slot.to_owned(),
        };
        Self { nvml: None, bus_id }
    }
}

impl Port for NvmlPort {
    fn open(&mut self) -> Result<(), String> {
        match nvml_wrapper::Nvml::init() {
            Ok(nvml) => {
                self.nvml = Some(nvml);
                Ok(())
            }
            Err(err) => Err(err.to_string()),
        }
    }

    fn close(&mut self) {
        // Dropping the `Nvml` is what closes `/dev/nvidia*`; there is no other
        // way to let the device runtime-suspend.
        self.nvml = None;
    }

    fn is_open(&self) -> bool {
        self.nvml.is_some()
    }

    fn observe(&mut self) -> Option<Observation> {
        let nvml = self.nvml.as_ref()?;
        let device = nvml
            .device_by_pci_bus_id(self.bus_id.as_str())
            .or_else(|_| nvml.device_by_index(0))
            .ok()?;
        let busy = device
            .utilization_rates()
            .ok()
            .map(|u| (u.gpu as f32).clamp(0.0, 100.0));
        // The counts rather than the lists: the same NVML query underneath,
        // without allocating a list of process records once a second.
        let clients = match (
            device.running_graphics_processes_count(),
            device.running_compute_processes_count(),
        ) {
            (Ok(graphics), Ok(compute)) => Some(graphics + compute),
            _ => None,
        };
        Some(Observation { busy, clients })
    }
}
