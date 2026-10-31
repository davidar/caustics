//! The one frame `caustics-power` publishes, built with fakes and read back
//! through the ABI's own reader — no bus, no bar: the power button's menu and
//! the battery's.

use caustics_bar_abi::buffer::{APPLET_POWER, NO_INDEX};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_RADIO, ROW_ITEM, ROW_MENU,
    ROW_SEPARATOR, ROW_SLIDER, Rows,
};
use caustics_power::backlight::Brightness;
use caustics_power::profiles::Profiles;
use caustics_power::rows::{
    LABEL_BRIGHTNESS, POWER_ICON, Picture, ROW_BATTERY, ROW_BRIGHTNESS, ROW_LOCK, ROW_LOG_OUT,
    ROW_MODES_SEP, ROW_POWER, ROW_PROFILE_BASE, ROW_RESTART, ROW_SEP, ROW_SHUTDOWN, ROW_STATE,
    ROW_SUSPEND, picture,
};
use caustics_power::upower::{Battery, STATE_CHARGING, STATE_DISCHARGING};

/// The frame's bytes, as `wire::write` puts them on the pipe.
fn bytes(revision: u32, state: &Picture) -> Vec<u8> {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &picture(revision, state)).expect("writing to a Vec");
    bytes
}

/// Round-trip through `wire::write`/`wire::read`, so every assertion below is
/// about what the *bar* would decode rather than about `RowsBuilder`'s memory.
fn decoded(revision: u32, state: &Picture) -> Rows {
    match wire::read(&mut bytes(revision, state).as_slice()) {
        Ok(Some(Frame::Rows(rows))) => rows,
        other => panic!("expected a rows frame, got {other:?}"),
    }
}

fn charging_at(percent: f32) -> Battery {
    Battery {
        percent,
        icon: "battery-level-80-charging-symbolic".to_owned(),
        state: STATE_CHARGING,
        time_to_empty: 0,
        time_to_full: 3900,
    }
}

fn with_battery(battery: Battery) -> Picture {
    Picture {
        battery: Some(battery),
        ..Picture::default()
    }
}

fn everything() -> Picture {
    Picture {
        battery: Some(Battery {
            percent: 87.0,
            icon: "battery-level-90-symbolic".to_owned(),
            state: STATE_DISCHARGING,
            time_to_empty: 7980,
            time_to_full: 0,
        }),
        brightness: Some(Brightness {
            value: 120,
            max: 255,
        }),
        profiles: Some(Profiles {
            names: vec![
                "power-saver".into(),
                "balanced".into(),
                "performance".into(),
            ],
            active: "balanced".into(),
        }),
        osd: false,
    }
}

fn index_of(rows: &Rows, id: u32) -> usize {
    rows.rows
        .iter()
        .position(|row| row.id == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

/// The battery menu's rows, in frame order, as ids.
fn battery_menu(rows: &Rows) -> Vec<u32> {
    let chip = index_of(rows, ROW_BATTERY) as u32;
    rows.rows
        .iter()
        .filter(|row| row.parent == chip)
        .map(|row| row.id)
        .collect()
}

#[test]
fn a_present_battery_is_a_chip_with_a_menu_after_the_button() {
    let rows = decoded(7, &with_battery(charging_at(76.0)));

    assert_eq!(rows.applet, APPLET_POWER);
    assert_eq!(rows.revision, 7);

    let button = &rows.rows[0];
    assert_eq!(button.id, ROW_POWER);
    assert_eq!(button.role, ROW_ITEM);
    assert_eq!(button.parent, NO_INDEX);
    assert_eq!(button.flags, RFLAG_HAS_MENU);
    assert_eq!(rows.icon(0), Some(POWER_ICON));
    assert_eq!(rows.label(0), None, "the button is an icon chip");

    // The battery follows the power menu, is a bar item of its own, and opens
    // its own menu.
    let i = index_of(&rows, ROW_BATTERY);
    assert_eq!(i, 7, "after the button and its six rows");
    let battery = &rows.rows[i];
    assert_eq!(battery.role, ROW_ITEM);
    assert_eq!(battery.parent, NO_INDEX);
    assert_eq!(battery.flags, RFLAG_HAS_MENU);
    assert_eq!(rows.label(i).as_deref(), Some("76%"));
    assert_eq!(rows.icon(i), Some("battery-level-80-charging-symbolic"));
    assert!(
        (battery.value - 0.76).abs() < 1e-6,
        "the level is the fraction, not the percentage: {}",
        battery.value
    );
}

#[test]
fn the_power_menu_is_session_rows_a_separator_then_machine_rows() {
    let rows = decoded(1, &Picture::default());

    let ids: Vec<u32> = rows.rows[1..].iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![
            ROW_LOCK,
            ROW_LOG_OUT,
            ROW_SEP,
            ROW_SUSPEND,
            ROW_RESTART,
            ROW_SHUTDOWN
        ]
    );

    let labels: Vec<Option<String>> = (1..rows.rows.len())
        .map(|i| rows.label(i).map(|l| l.into_owned()))
        .collect();
    assert_eq!(
        labels,
        vec![
            Some("Lock".to_owned()),
            Some("Log Out".to_owned()),
            None,
            Some("Suspend".to_owned()),
            Some("Restart".to_owned()),
            Some("Shut Down".to_owned()),
        ]
    );

    for i in 1..rows.rows.len() {
        let row = &rows.rows[i];
        assert_eq!(row.parent, 0, "row {} hangs off the power button", row.id);
        let expected = if row.id == ROW_SEP {
            ROW_SEPARATOR
        } else {
            ROW_MENU
        };
        assert_eq!(row.role, expected, "row {}", row.id);
        assert_eq!(row.flags, 0, "row {} performs on a plain activate", row.id);
    }

    assert_eq!(rows.icon(1), Some("system-lock-screen-symbolic"));
    assert_eq!(rows.icon(2), Some("system-log-out-symbolic"));
    assert_eq!(rows.icon(3), None, "a separator has no icon");
    assert_eq!(rows.icon(4), Some("system-suspend-symbolic"));
    assert_eq!(rows.icon(5), Some("system-reboot-symbolic"));
    assert_eq!(rows.icon(6), Some("system-shutdown-symbolic"));
}

#[test]
fn an_absent_battery_is_no_chip_and_no_menu() {
    // `IsPresent = false` (a desktop) and "no UPower on the bus" are the same
    // `None`, and the same frame: seven rows, no battery — and so no state,
    // slider or modes either, whatever the backlight and the daemon say.
    let state = Picture {
        battery: None,
        ..everything()
    };
    let rows = decoded(1, &state);
    assert_eq!(rows.rows.len(), 7);
    assert!(
        rows.rows
            .iter()
            .all(|row| row.id != ROW_BATTERY && row.id != ROW_STATE)
    );
    // The button and its menu are still there: the power menu does not need a
    // battery, or a bus.
    assert_eq!(rows.rows[0].id, ROW_POWER);
    assert_eq!(
        rows.rows.iter().filter(|row| row.role == ROW_ITEM).count(),
        1
    );
}

#[test]
fn the_battery_menu_with_everything_is_state_slider_modes() {
    let rows = decoded(1, &everything());
    assert_eq!(
        battery_menu(&rows),
        vec![
            ROW_STATE,
            ROW_BRIGHTNESS,
            ROW_MODES_SEP,
            ROW_PROFILE_BASE,
            ROW_PROFILE_BASE + 1,
            ROW_PROFILE_BASE + 2,
        ]
    );

    let state = index_of(&rows, ROW_STATE);
    assert_eq!(rows.rows[state].role, ROW_MENU);
    assert_eq!(
        rows.rows[state].flags, RFLAG_DISABLED,
        "the state row is read, not clicked"
    );
    assert_eq!(
        rows.label(state).as_deref(),
        Some("87% · 2 h 13 m until empty")
    );

    let slider = index_of(&rows, ROW_BRIGHTNESS);
    let row = &rows.rows[slider];
    assert_eq!(row.role, ROW_SLIDER);
    assert_eq!(rows.label(slider).as_deref(), Some(LABEL_BRIGHTNESS));
    assert_eq!(
        (row.value, row.min, row.max),
        (120.0, 0.0, 255.0),
        "raw units both ways"
    );

    let modes: Vec<(String, u32, Option<&str>)> = (0..3)
        .map(|k| {
            let i = index_of(&rows, ROW_PROFILE_BASE + k);
            (
                rows.label(i).unwrap().into_owned(),
                rows.rows[i].flags,
                rows.icon(i),
            )
        })
        .collect();
    assert_eq!(
        modes,
        vec![
            (
                "Battery".to_owned(),
                RFLAG_RADIO,
                Some("power-profile-power-saver-symbolic")
            ),
            (
                "Balanced".to_owned(),
                RFLAG_RADIO | RFLAG_CHECKED,
                Some("power-profile-balanced-symbolic")
            ),
            (
                "Performance".to_owned(),
                RFLAG_RADIO,
                Some("power-profile-performance-symbolic")
            ),
        ]
    );
}

#[test]
fn without_a_backlight_or_a_daemon_the_menu_is_the_state_row() {
    // A virtual machine's menu: a battery UPower reports, no backlight, no
    // power-profiles daemon.
    let rows = decoded(1, &with_battery(charging_at(76.0)));
    assert_eq!(battery_menu(&rows), vec![ROW_STATE]);
    assert_eq!(
        rows.label(index_of(&rows, ROW_STATE)).as_deref(),
        Some("76% · 1 h 5 m until full")
    );
}

#[test]
fn an_unknown_profile_is_a_row_by_its_own_name() {
    let mut state = everything();
    state.profiles = Some(Profiles {
        names: vec!["balanced".into(), "quiet".into()],
        active: "quiet".into(),
    });
    let rows = decoded(1, &state);
    let i = index_of(&rows, ROW_PROFILE_BASE + 1);
    assert_eq!(rows.label(i).as_deref(), Some("quiet"));
    assert_eq!(rows.icon(i), None);
    assert_eq!(rows.rows[i].flags, RFLAG_RADIO | RFLAG_CHECKED);
}

#[test]
fn a_slider_tick_changes_the_slider_value_and_nothing_else() {
    // The bar's test for "one write_buffer" is that ids, roles, parents, flags
    // and the label and icon ranges are unchanged (docs/abi.md, *Rows*). The
    // strictest form of that: the two frames differ in exactly the four bytes of the slider row's `value`.
    let before = everything();
    let mut after = before.clone();
    after.brightness = Some(Brightness {
        value: 200,
        max: 255,
    });

    let a = bytes(5, &before);
    let b = bytes(5, &after);
    assert_eq!(a.len(), b.len());
    let differ: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();

    let rows = decoded(5, &after);
    let slider = index_of(&rows, ROW_BRIGHTNESS);
    // Header (12) + RowsHeader (16) + 48 per row; `value` is at 32 in the row.
    let value_at = 12 + 16 + 48 * slider + 32;
    assert!(!differ.is_empty());
    assert!(
        differ
            .iter()
            .all(|&i| (value_at..value_at + 4).contains(&i)),
        "bytes {differ:?} changed; only {value_at}..{} may",
        value_at + 4
    );
    assert_eq!(rows.rows[slider].value, 200.0);
}

#[test]
fn ids_roles_and_parents_are_stable_across_frames() {
    let a = decoded(1, &with_battery(charging_at(76.0)));
    let b = decoded(2, &with_battery(charging_at(41.0)));

    let shape = |rows: &Rows| -> Vec<(u32, u32, u32, u32)> {
        rows.rows
            .iter()
            .map(|r| (r.id, r.parent, r.role, r.flags))
            .collect()
    };
    assert_eq!(shape(&a), shape(&b), "only the percentage moved");

    // …and the percentage did move, in both the label and the level.
    let i = index_of(&b, ROW_BATTERY);
    assert_eq!(b.label(i).as_deref(), Some("41%"));
    assert!((b.rows[i].value - 0.41).abs() < 1e-6);

    // Losing the battery drops its chip and its menu, and leaves every other
    // id where it was.
    let gone = decoded(3, &Picture::default());
    assert_eq!(shape(&gone), shape(&a)[..7]);

    // A daemon appearing adds its rows and moves no id that was there.
    let mut with_modes = everything();
    with_modes.brightness = None;
    let c = decoded(4, &with_modes);
    for id in battery_menu(&a) {
        assert!(battery_menu(&c).contains(&id), "row {id} kept its id");
    }
}
