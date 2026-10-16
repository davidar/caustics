//! The battery menu's state row: its words from each UPower state and time
//! pair.

use caustics_power::rows::{duration, state_line};
use caustics_power::upower::{
    Battery, STATE_CHARGING, STATE_DISCHARGING, STATE_EMPTY, STATE_FULLY_CHARGED,
    STATE_PENDING_CHARGE, STATE_PENDING_DISCHARGE, STATE_UNKNOWN,
};

fn at(percent: f32, state: u32, time_to_empty: i64, time_to_full: i64) -> String {
    state_line(&Battery {
        percent,
        icon: "battery-symbolic".to_owned(),
        state,
        time_to_empty,
        time_to_full,
    })
}

#[test]
fn discharging_says_how_long_until_empty() {
    // 2 h 13 m = 7980 s.
    assert_eq!(
        at(87.0, STATE_DISCHARGING, 7980, 0),
        "87% · 2 h 13 m until empty"
    );
    // A pending discharge and an empty battery word the same way.
    assert_eq!(
        at(87.0, STATE_PENDING_DISCHARGE, 7980, 0),
        "87% · 2 h 13 m until empty"
    );
    assert_eq!(at(1.0, STATE_EMPTY, 45, 0), "1% · 1 m until empty");
    // UPower's "no estimate yet" is zero; the row still says where it is.
    assert_eq!(at(64.0, STATE_DISCHARGING, 0, 0), "64% · On battery");
}

#[test]
fn charging_says_how_long_until_full() {
    assert_eq!(
        at(41.0, STATE_CHARGING, 0, 3900),
        "41% · 1 h 5 m until full"
    );
    assert_eq!(at(98.0, STATE_CHARGING, 0, 240), "98% · 4 m until full");
    assert_eq!(at(41.0, STATE_CHARGING, 0, 0), "41% · Charging");
    // The other time is ignored: a charging battery is never "until empty".
    assert_eq!(at(41.0, STATE_CHARGING, 9000, 3600), "41% · 1 h until full");
}

#[test]
fn full_is_words_alone_and_on_ac_says_where_it_holds() {
    assert_eq!(at(100.0, STATE_FULLY_CHARGED, 0, 0), "Fully charged");
    // Plugged in and holding — a charge threshold (`ChargeThresholdEnabled`,
    // at 77 %, say), or a battery UPower says is waiting to charge.
    // the percentage is the point of the row, so it stays.
    assert_eq!(at(77.0, STATE_PENDING_CHARGE, 0, 0), "77% · On AC");
}

#[test]
fn an_unknown_state_is_the_percentage() {
    assert_eq!(at(55.4, STATE_UNKNOWN, 1200, 1200), "55%");
    assert_eq!(at(55.0, 42, 0, 0), "55%", "a state UPower may add later");
}

#[test]
fn durations_round_to_the_minute() {
    assert_eq!(duration(0), None);
    assert_eq!(duration(-5), None);
    assert_eq!(
        duration(1).as_deref(),
        Some("1 m"),
        "never less than a minute"
    );
    assert_eq!(duration(89).as_deref(), Some("1 m"));
    assert_eq!(duration(90).as_deref(), Some("2 m"));
    assert_eq!(duration(3570).as_deref(), Some("1 h"));
    assert_eq!(duration(3600).as_deref(), Some("1 h"));
    assert_eq!(duration(7980).as_deref(), Some("2 h 13 m"));
    assert_eq!(duration(36_000 + 59).as_deref(), Some("10 h 1 m"));
    // The rounding saturates rather than overflowing at the top of `x`.
    assert_eq!(
        duration(i64::MAX).as_deref(),
        Some("2562047788015215 h 30 m")
    );
}
