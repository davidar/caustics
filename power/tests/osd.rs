//! The brightness on-screen display: the rule ([`decide`]), the read a uevent
//! causes ([`Lamp::uevent`]), the burst's deadline and the witness's memory
//! ([`Osd`]), and the bits in the frame. The clock is a parameter and a temp
//! dir stands in for `/sys/class/backlight`; nothing reads or writes this
//! machine's backlight.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use caustics_bar_abi::buffer::{ACTION_MENU_CLOSE, ACTION_MENU_OPEN};
use caustics_bar_abi::wire::{self, Frame, RFLAG_ATTENTION, RFLAG_SHOW, Rows};
use caustics_power::backlight::Brightness;
use caustics_power::lamp::Lamp;
use caustics_power::osd::{Decision, Osd, decide};
use caustics_power::rows::{Picture, ROW_BATTERY, ROW_BRIGHTNESS, picture};
use caustics_power::upower::{Battery, STATE_DISCHARGING};
use caustics_power::{OSD_HOLD, TICK_HOLD};

/// A class directory with one raw panel out of 255.
struct Sysfs(PathBuf);

impl Sysfs {
    fn new(tag: &str, value: u32) -> Sysfs {
        let dir =
            std::env::temp_dir().join(format!("caustics-power-osd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("amdgpu_bl0")).expect("a temp dir");
        std::fs::write(dir.join("amdgpu_bl0/max_brightness"), "255\n").unwrap();
        std::fs::write(dir.join("amdgpu_bl0/type"), "raw\n").unwrap();
        let sysfs = Sysfs(dir);
        sysfs.set(value);
        sysfs
    }

    /// A brightness key, or anyone else, writing the file.
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
fn the_rule() {
    let now = Instant::now();
    // A changed value with no tick about is shown.
    assert_eq!(
        decide(at(100), at(120), None, now),
        Decision::Show(at(120).unwrap())
    );
    // The same value written again still emits the uevent, and is nothing.
    assert_eq!(decide(at(100), at(100), None, now), Decision::Nothing);
    // Within the hold of the slider's own tick it is the drag, landing.
    let tick = now - TICK_HOLD + Duration::from_millis(1);
    assert_eq!(decide(at(100), at(120), Some(tick), now), Decision::Own);
    // After it, it is news again.
    assert_eq!(
        decide(at(100), at(120), Some(now - TICK_HOLD), now),
        Decision::Show(at(120).unwrap())
    );
    // No backlight before or after: nothing to show it on.
    assert_eq!(decide(None, None, None, now), Decision::Nothing);
    assert_eq!(decide(None, at(120), None, now), Decision::Nothing);
    assert_eq!(decide(at(100), None, None, now), Decision::Nothing);
}

#[test]
fn a_uevent_reads_with_the_menu_closed() {
    let sysfs = Sysfs::new("closed", 100);
    let mut lamp = Lamp::new(sysfs.root());
    assert!(!lamp.polling());
    let t0 = Instant::now();
    sysfs.set(140);
    assert_eq!(lamp.uevent(t0), Decision::Show(at(140).unwrap()));
    assert_eq!(lamp.brightness(), at(140));
    // The same value again: the kernel says so, and it is nothing.
    assert_eq!(lamp.uevent(t0), Decision::Nothing);
    assert_eq!(lamp.brightness(), at(140));
}

#[test]
fn a_uevent_inside_the_tick_hold_is_the_drag_and_keeps_the_slider() {
    let sysfs = Sysfs::new("tick", 100);
    let mut lamp = Lamp::new(sysfs.root());
    lamp.notice(ROW_BATTERY, ACTION_MENU_OPEN);
    let t0 = Instant::now();
    assert_eq!(lamp.tick(200.0, t0), Some(200));
    lamp.notice(ROW_BATTERY, ACTION_MENU_CLOSE);
    // logind writes an intermediate value of the drag; the slider holds where
    // the pointer put it, and the change is ours.
    sysfs.set(180);
    assert_eq!(lamp.uevent(t0 + TICK_HOLD / 2), Decision::Own);
    assert_eq!(lamp.brightness(), at(200));
    // Past the hold the file is the truth again, and a key's change is shown.
    sysfs.set(90);
    let later = t0 + TICK_HOLD + Duration::from_millis(1);
    assert_eq!(lamp.uevent(later), Decision::Show(at(90).unwrap()));
    assert_eq!(lamp.brightness(), at(90));
}

#[test]
fn a_uevent_without_a_backlight_is_nothing() {
    let dir = std::env::temp_dir().join(format!("caustics-power-osd-none-{}", std::process::id()));
    let mut lamp = Lamp::new(&dir);
    assert_eq!(lamp.uevent(Instant::now()), Decision::Nothing);
    assert_eq!(lamp.brightness(), None);
}

#[test]
fn a_burst_extends_and_ends_by_its_clock_or_the_toast() {
    let t0 = Instant::now();
    let mut osd = Osd::default();
    let show = |value| Decision::Show(at(value).unwrap());
    assert_eq!(
        osd.change(show(128), false, t0).as_deref(),
        Some("osd: brightness 50% (hold 1500ms)")
    );
    assert!(osd.on());
    let t1 = t0 + Duration::from_millis(400);
    assert!(osd.change(show(153), false, t1).is_some());
    assert_eq!(osd.deadline(), Some(t1 + OSD_HOLD));
    assert!(!osd.expire(t0 + OSD_HOLD));
    assert!(osd.expire(t1 + OSD_HOLD));
    assert!(!osd.on());
    assert!(!osd.expire(t1 + OSD_HOLD * 2));

    // `ACTION_TOAST_SHOWN` ends it at once, and only once.
    osd.change(show(100), false, t1 + OSD_HOLD * 3);
    assert!(osd.end());
    assert!(!osd.end());
    assert!(!osd.on());
}

#[test]
fn what_is_not_shown_is_said_once_per_burst() {
    let t0 = Instant::now();
    let mut osd = Osd::default();
    let mut lines = Vec::new();
    for step in 0..30u64 {
        lines.extend(osd.change(Decision::Own, false, t0 + Duration::from_millis(20 * step)));
    }
    assert_eq!(lines, ["osd: own change, not shown"]);
    assert!(!osd.on());

    // The battery's menu open: the slider is on screen in it already.
    let later = t0 + Duration::from_secs(10);
    let shown = Decision::Show(at(30).unwrap());
    assert_eq!(
        osd.change(shown, true, later).as_deref(),
        Some("osd: menu open, not shown")
    );
    assert!(!osd.on());
    assert_eq!(osd.change(Decision::Nothing, false, later), None);
}

fn decoded(state: &Picture) -> Rows {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &picture(4, state, true)).expect("writing to a Vec");
    match wire::read(&mut bytes.as_slice()) {
        Ok(Some(Frame::Rows(rows))) => rows,
        other => panic!("expected a rows frame, got {other:?}"),
    }
}

fn flags(rows: &Rows, id: u32) -> u32 {
    rows.rows
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("row {id}"))
        .flags
}

fn on_battery(osd: bool) -> Picture {
    Picture {
        battery: Some(Battery {
            percent: 60.0,
            icon: "battery-level-60-symbolic".to_owned(),
            state: STATE_DISCHARGING,
            time_to_empty: 3600,
            time_to_full: 0,
        }),
        brightness: at(120),
        osd,
        ..Picture::default()
    }
}

#[test]
fn a_toasting_picture_carries_the_bits_and_a_plain_one_none() {
    let plain = decoded(&on_battery(false));
    assert!(
        plain
            .rows
            .iter()
            .all(|row| row.flags & (RFLAG_SHOW | RFLAG_ATTENTION) == 0)
    );
    let toast = decoded(&on_battery(true));
    assert_ne!(flags(&toast, ROW_BATTERY) & RFLAG_SHOW, 0);
    assert_ne!(flags(&toast, ROW_BRIGHTNESS) & RFLAG_ATTENTION, 0);
    for (a, b) in plain.rows.iter().zip(&toast.rows) {
        assert_eq!(a.id, b.id);
        if b.id != ROW_BATTERY && b.id != ROW_BRIGHTNESS {
            assert_eq!(a.flags, b.flags, "row {}", b.id);
        }
    }
    // The flag alone is a different picture, so the loop publishes it.
    assert_ne!(on_battery(false), on_battery(true));
}

#[test]
fn no_slider_is_no_toast() {
    // Without a backlight the chip does not ask: there is no row to show.
    let state = Picture {
        brightness: None,
        ..on_battery(true)
    };
    let rows = decoded(&state);
    assert_eq!(flags(&rows, ROW_BATTERY) & RFLAG_SHOW, 0);
    // Without a battery there is no chip, so no slider and no toast.
    let state = Picture {
        battery: None,
        ..on_battery(true)
    };
    let rows = decoded(&state);
    assert!(rows.rows.iter().all(|row| row.flags & RFLAG_SHOW == 0));
}
