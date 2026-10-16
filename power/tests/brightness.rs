//! The brightness slider: its position from a temp
//! dir standing in for `/sys/class/backlight`, and the logind call a tick
//! makes, recorded by a fake `login1.Session` on a private bus. Nothing here
//! reads or writes this machine's backlight.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use caustics_power::backlight::{
    self, Backlight, Brightness, Kind, MIN_SET, SESSION_AUTO, SUBSYSTEM,
};
use caustics_sidecar::test_bus::Bus;
use zbus::Connection;

/// A fresh directory standing in for `/sys/class/backlight`, removed on drop.
struct Sysfs(PathBuf);

impl Sysfs {
    fn new(tag: &str) -> Sysfs {
        let dir = std::env::temp_dir().join(format!(
            "caustics-power-backlight-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Sysfs(dir)
    }

    fn device(&self, name: &str, brightness: Option<&str>, max: Option<&str>) -> &Self {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).expect("a device dir");
        if let Some(value) = brightness {
            std::fs::write(dir.join("brightness"), value).unwrap();
        }
        if let Some(value) = max {
            std::fs::write(dir.join("max_brightness"), value).unwrap();
        }
        self
    }

    /// The kernel's `type` file.
    fn kind(&self, name: &str, kind: &str) -> &Self {
        std::fs::write(self.0.join(name).join("type"), format!("{kind}\n")).unwrap();
        self
    }

    /// The `device` link, to a directory named like its real target's last
    /// component (`card0-eDP-1`, an i2c address), made beside the class dir
    /// rather than in it, where it would be one more backlight.
    fn device_link(&self, name: &str, target: &str) -> &Self {
        let target_dir = self.devices().join(target);
        std::fs::create_dir_all(&target_dir).unwrap();
        std::os::unix::fs::symlink(&target_dir, self.0.join(name).join("device")).unwrap();
        self
    }

    fn devices(&self) -> PathBuf {
        self.0.with_extension("devices")
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Sysfs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir_all(self.devices());
    }
}

#[test]
fn the_position_is_brightness_over_max() {
    let sysfs = Sysfs::new("read");
    // sysfs files end in a newline.
    sysfs.device("amdgpu_bl0", Some("120\n"), Some("255\n"));
    let (device, reading) = backlight::sample(sysfs.root()).expect("one backlight");
    assert_eq!(device.name, "amdgpu_bl0");
    assert_eq!(device.dir, sysfs.root().join("amdgpu_bl0"));
    assert_eq!(
        reading,
        Brightness {
            value: 120,
            max: 255
        }
    );
}

#[test]
fn the_backlight_is_chosen_by_type_then_by_panel_then_by_name() {
    // Not the first by name: a `ddcci` monitor can sort before the panel.
    //
    // The type first: `firmware` (ACPI) is the kernel's word for "the
    // platform's own control, quirks known", ahead of `platform` and `raw` —
    // the order GNOME's and systemd's have. Here it also sorts first by name,
    // so the next case is the one a first-by-name rule gets wrong.
    let sysfs = Sysfs::new("type");
    sysfs
        .device("intel_backlight", Some("9000"), Some("96000"))
        .kind("intel_backlight", "raw");
    sysfs
        .device("acpi_video0", Some("5"), Some("10"))
        .kind("acpi_video0", "firmware");
    let found = Backlight::find(sysfs.root()).unwrap();
    assert_eq!(
        (found.name.as_str(), found.kind),
        ("acpi_video0", Kind::Firmware)
    );

    // Two raw devices: the internal panel's, whose `device` link is the eDP
    // connector, beats a DDC/CI monitor's — which sorts first by name, so a first-by-name rule would dim the external monitor instead.
    let sysfs = Sysfs::new("panel");
    sysfs
        .device("intel_backlight", Some("9000"), Some("96000"))
        .kind("intel_backlight", "raw")
        .device_link("intel_backlight", "card0-eDP-1");
    sysfs
        .device("ddcci6", Some("50"), Some("100"))
        .kind("ddcci6", "raw")
        .device_link("ddcci6", "6-0037");
    let found = Backlight::find(sysfs.root()).unwrap();
    assert_eq!(found.name, "intel_backlight");
    assert!(found.internal);
    // LVDS and DSI panels are internal too.
    for connector in ["card1-LVDS-1", "card0-DSI-1"] {
        let sysfs = Sysfs::new(connector);
        sysfs
            .device("zz_panel", Some("1"), Some("2"))
            .kind("zz_panel", "raw")
            .device_link("zz_panel", connector);
        sysfs
            .device("aa_other", Some("1"), Some("2"))
            .kind("aa_other", "raw");
        assert_eq!(
            Backlight::find(sysfs.root()).unwrap().name,
            "zz_panel",
            "{connector}"
        );
    }

    // A `platform` device beats a raw panel; a device with no `type` comes
    // last; among equals the name decides, so it is the same one every time.
    let sysfs = Sysfs::new("rest");
    sysfs.device("a_untyped", Some("1"), Some("2"));
    sysfs
        .device("m_raw", Some("1"), Some("2"))
        .kind("m_raw", "raw")
        .device_link("m_raw", "card0-eDP-1");
    sysfs
        .device("z_platform", Some("1"), Some("2"))
        .kind("z_platform", "platform");
    assert_eq!(Backlight::find(sysfs.root()).unwrap().name, "z_platform");
    let sysfs = Sysfs::new("tie");
    sysfs.device("b", Some("1"), Some("2")).kind("b", "raw");
    sysfs.device("a", Some("1"), Some("2")).kind("a", "raw");
    sysfs.device("0", Some("1"), Some("2"));
    assert_eq!(Backlight::find(sysfs.root()).unwrap().name, "a");
}

#[test]
fn an_amdgpu_panel_is_a_raw_internal_backlight() {
    // `amdgpu_bl0`, `type` raw, `device` → `…/drm/card0/card0-eDP-1`, as an
    // AMD APU's sysfs has it.
    let sysfs = Sysfs::new("amdgpu-panel");
    sysfs
        .device("amdgpu_bl0", Some("120"), Some("255"))
        .kind("amdgpu_bl0", "raw")
        .device_link("amdgpu_bl0", "card0-eDP-1");
    let found = Backlight::find(sysfs.root()).unwrap();
    assert_eq!((found.kind, found.internal), (Kind::Raw, true));
    // 1 % of 255, rounded up.
    assert_eq!(found.floor(255), 3);
}

#[test]
fn no_backlight_is_no_slider() {
    // An empty class directory (a virtual machine), and none at all.
    let sysfs = Sysfs::new("empty");
    assert_eq!(backlight::sample(sysfs.root()), None);
    assert_eq!(backlight::sample(&sysfs.root().join("not-there")), None);
}

#[test]
fn a_device_that_cannot_be_read_is_no_slider() {
    let sysfs = Sysfs::new("broken");
    sysfs.device("a", Some("12"), None);
    assert_eq!(backlight::sample(sysfs.root()), None, "no max_brightness");

    let sysfs = Sysfs::new("zero");
    sysfs.device("a", Some("0"), Some("0"));
    assert_eq!(backlight::sample(sysfs.root()), None, "nothing to slide");

    let sysfs = Sysfs::new("garbage");
    sysfs.device("a", Some("bright"), Some("255"));
    assert_eq!(backlight::sample(sysfs.root()), None);
}

#[test]
fn a_value_over_max_reads_as_max() {
    let sysfs = Sysfs::new("over");
    sysfs.device("a", Some("300"), Some("255"));
    assert_eq!(
        backlight::sample(sysfs.root()).unwrap().1,
        Brightness {
            value: 255,
            max: 255
        }
    );
}

#[test]
fn a_backlight_appearing_is_seen_the_next_time_anyone_looks() {
    let sysfs = Sysfs::new("appear");
    assert_eq!(backlight::sample(sysfs.root()), None);
    sysfs.device("amdgpu_bl0", Some("40"), Some("255"));
    assert_eq!(backlight::sample(sysfs.root()).unwrap().1.value, 40);
    // …and the keys changing it behind the menu is the next read's.
    std::fs::write(sysfs.root().join("amdgpu_bl0/brightness"), "200\n").unwrap();
    assert_eq!(backlight::sample(sysfs.root()).unwrap().1.value, 200);
}

#[test]
fn a_tick_is_rounded_and_never_turns_the_panel_off() {
    assert_eq!(backlight::to_raw(127.6, 255, 3), 128);
    assert_eq!(backlight::to_raw(0.0, 255, 3), 3);
    assert_eq!(backlight::to_raw(-3.0, 255, 3), 3);
    assert_eq!(backlight::to_raw(300.0, 255, 3), 255);
    assert_eq!(backlight::to_raw(f32::NAN, 255, 3), 3);
    // A floor above the maximum is the maximum.
    assert_eq!(backlight::to_raw(0.0, 2, 3), 2);
    assert_eq!(MIN_SET, 1);
}

#[test]
fn a_raw_device_is_floored_at_one_percent_and_a_firmware_one_at_one_step() {
    // One raw unit of 400000 is off in all but name.
    let raw = |kind: &str| {
        let sysfs = Sysfs::new(&format!("floor-{kind}"));
        sysfs.device("a", Some("1"), Some("2"));
        if !kind.is_empty() {
            sysfs.kind("a", kind);
        }
        Backlight::find(sysfs.root()).unwrap()
    };
    let panel = raw("raw");
    assert_eq!(panel.floor(400_000), 4_000);
    assert_eq!(panel.floor(255), 3);
    assert_eq!(panel.floor(100), 1);
    assert_eq!(panel.floor(10), 1, "never below one unit");
    assert_eq!(panel.floor(0), 0, "never above the maximum");
    // No `type` file is floored like raw, the cautious reading.
    assert_eq!(raw("").floor(400_000), 4_000);
    // ACPI levels: 1 of 10 is already the dimmest lit step.
    assert_eq!(raw("firmware").floor(10), MIN_SET);
    assert_eq!(raw("platform").floor(400_000), MIN_SET);
}

/// logind's session object, recording every `SetBrightness`.
struct Session {
    calls: Arc<Mutex<Vec<(String, String, u32)>>>,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl Session {
    async fn set_brightness(&self, subsystem: String, name: String, brightness: u32) {
        // A little latency, so a burst of ticks finds the setter busy.
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.calls
            .lock()
            .unwrap()
            .push((subsystem, name, brightness));
    }
}

async fn logind(
    bus: &Bus,
) -> (
    Connection,
    Connection,
    Arc<Mutex<Vec<(String, String, u32)>>>,
) {
    let connect = || async {
        zbus::connection::Builder::address(bus.address.as_str())
            .expect("the bus address parses")
            .build()
            .await
            .expect("a connection to the private bus")
    };
    let peer = connect().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    peer.object_server()
        .at(
            SESSION_AUTO,
            Session {
                calls: calls.clone(),
            },
        )
        .await
        .expect("the session object");
    peer.request_name("org.freedesktop.login1")
        .await
        .expect("the peer owns logind's name");
    let client = connect().await;
    (peer, client, calls)
}

#[tokio::test]
async fn a_tick_is_set_brightness_on_session_auto() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the logind call is untested here");
        return;
    };
    let (_peer, client, calls) = logind(&bus).await;

    backlight::set(&client, "amdgpu_bl0", 128)
        .await
        .expect("the call is answered");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(SUBSYSTEM.to_owned(), "amdgpu_bl0".to_owned(), 128)],
        "SetBrightness(\"backlight\", <name>, v)"
    );
    assert_eq!(SUBSYSTEM, "backlight");
    assert_eq!(SESSION_AUTO, "/org/freedesktop/login1/session/auto");
}

#[tokio::test]
async fn a_drag_is_coalesced_to_the_latest_value() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the setter is untested here");
        return;
    };
    let (_peer, client, calls) = logind(&bus).await;

    let setter = backlight::spawn_setter(client);
    // A hundred ticks in a burst, as a drag sends them.
    for v in 1..=100u32 {
        setter.send(Some(("amdgpu_bl0".to_owned(), v))).unwrap();
        tokio::task::yield_now().await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let last = calls.lock().unwrap().last().map(|c| c.2);
        if last == Some(100) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the last value never arrived: {last:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let made = calls.lock().unwrap().len();
    assert!(
        made < 100,
        "{made} calls for 100 ticks: the lane did not coalesce"
    );
}
