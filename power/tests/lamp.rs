//! The loop's own logic for the brightness slider:
//! sysfs is read when the battery's menu opens and on each poll
//! while it stays open, never while it is closed, and a poll within
//! `TICK_HOLD` of a slider tick does not pull the slider back. The lamp takes
//! its clock as a parameter, so nothing here sleeps; a temp dir stands in for
//! `/sys/class/backlight`, and nothing reads or writes this machine's.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::{ACTION_MENU_CLOSE, ACTION_MENU_OPEN};
use caustics_power::TICK_HOLD;
use caustics_power::backlight::Brightness;
use caustics_power::lamp::Lamp;
use caustics_power::rows::{ROW_BATTERY, ROW_POWER};

/// A class directory with one raw panel, `amdgpu_bl0`, out of 255.
struct Sysfs(PathBuf);

impl Sysfs {
    fn new(tag: &str, value: u32) -> Sysfs {
        let dir =
            std::env::temp_dir().join(format!("caustics-power-lamp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("amdgpu_bl0")).expect("a temp dir");
        std::fs::write(dir.join("amdgpu_bl0/max_brightness"), "255\n").unwrap();
        std::fs::write(dir.join("amdgpu_bl0/type"), "raw\n").unwrap();
        let sysfs = Sysfs(dir);
        sysfs.set(value);
        sysfs
    }

    /// The brightness keys, behind the menu.
    fn set(&self, value: u32) {
        std::fs::write(self.0.join("amdgpu_bl0/brightness"), format!("{value}\n")).unwrap();
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Sysfs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn at(value: u32) -> Option<Brightness> {
    Some(Brightness { value, max: 255 })
}

#[test]
fn the_backlight_is_read_while_the_battery_menu_is_open_and_never_while_it_is_closed() {
    let sysfs = Sysfs::new("gate", 100);
    let mut lamp = Lamp::new(sysfs.root());
    let t0 = Instant::now();
    // The first frame's look.
    assert_eq!(lamp.brightness(), at(100));
    assert!(!lamp.polling());

    // Closed: the keys move it and nobody looks.
    sysfs.set(140);
    assert!(!lamp.poll(t0), "a poll while closed reads nothing");
    assert_eq!(lamp.brightness(), at(100));

    // Another menu's notices are not the battery's.
    assert!(!lamp.notice(ROW_POWER, ACTION_MENU_OPEN));
    assert!(!lamp.polling());
    assert_eq!(lamp.brightness(), at(100));

    // The battery's menu opens: read now, and poll from here on.
    assert!(
        lamp.notice(ROW_BATTERY, ACTION_MENU_OPEN),
        "the loop resets its interval"
    );
    assert!(lamp.polling());
    assert_eq!(lamp.brightness(), at(140));
    sysfs.set(180);
    assert!(lamp.poll(t0 + Duration::from_secs(1)));
    assert_eq!(lamp.brightness(), at(180));

    // It closes: no more reads.
    assert!(!lamp.notice(ROW_BATTERY, ACTION_MENU_CLOSE));
    assert!(!lamp.polling());
    sysfs.set(60);
    assert!(!lamp.poll(t0 + Duration::from_secs(2)));
    assert_eq!(lamp.brightness(), at(180));
}

#[test]
fn a_poll_inside_the_hold_does_not_pull_the_slider_back() {
    let sysfs = Sysfs::new("hold", 100);
    let mut lamp = Lamp::new(sysfs.root());
    lamp.notice(ROW_BATTERY, ACTION_MENU_OPEN);
    let t0 = Instant::now();

    // A drag to 200: the slider is there at once; logind has not written the
    // file yet, so sysfs still says 100.
    assert_eq!(lamp.tick(200.0, t0), Some(200));
    assert_eq!(lamp.brightness(), at(200));
    assert_eq!(lamp.tick(200.0, t0), None, "the same value is no call");

    // A poll inside the hold leaves the slider where the pointer put it.
    assert!(lamp.poll(t0 + TICK_HOLD / 2));
    assert_eq!(lamp.brightness(), at(200));

    // After it, the file is the truth again — whatever the keys did.
    sysfs.set(90);
    assert!(lamp.poll(t0 + TICK_HOLD + Duration::from_millis(1)));
    assert_eq!(lamp.brightness(), at(90));

    // An open reads whatever the hold says: a menu must not open stale.
    assert_eq!(lamp.tick(30.0, t0 + TICK_HOLD * 2), Some(30));
    sysfs.set(250);
    lamp.notice(ROW_BATTERY, ACTION_MENU_CLOSE);
    lamp.notice(ROW_BATTERY, ACTION_MENU_OPEN);
    assert_eq!(lamp.brightness(), at(250));
}

#[test]
fn a_drag_to_the_left_end_stops_at_the_raw_floor() {
    let sysfs = Sysfs::new("floor", 100);
    let mut lamp = Lamp::new(sysfs.root());
    // 1 % of 255, rounded up.
    assert_eq!(lamp.tick(0.0, Instant::now()), Some(3));
    assert_eq!(
        lamp.backlight().map(|b| b.name.as_str()),
        Some("amdgpu_bl0")
    );
}

#[test]
fn no_backlight_is_no_slider_and_no_tick() {
    let dir = std::env::temp_dir().join(format!("caustics-power-lamp-none-{}", std::process::id()));
    let mut lamp = Lamp::new(&dir);
    assert_eq!(lamp.brightness(), None);
    assert_eq!(lamp.tick(50.0, Instant::now()), None);
    lamp.notice(ROW_BATTERY, ACTION_MENU_OPEN);
    assert!(lamp.poll(Instant::now()));
    assert_eq!(lamp.brightness(), None);
}
