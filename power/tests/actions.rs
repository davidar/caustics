//! The dispatch: each row id, with each action kind, to the call it would
//! make — asserted against the *description* of the call, so no test ever
//! locks a session, suspends, reboots, shuts down or quits the compositor
//! (no test performs a real power action, ever).

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MIDDLE, ACTION_SECONDARY, ACTION_SET_VALUE,
};
use caustics_power::actions::{Call, Op, SETTINGS_PANEL, dry_run, op_for};
use caustics_power::niri;
use caustics_power::rows::{
    MAX_PROFILES, ROW_BATTERY, ROW_BRIGHTNESS, ROW_LOCK, ROW_LOG_OUT, ROW_MODES_SEP, ROW_POWER,
    ROW_PROFILE_BASE, ROW_RESTART, ROW_SEP, ROW_SETTINGS, ROW_SETTINGS_SEP, ROW_SHUTDOWN,
    ROW_STATE, ROW_SUSPEND,
};

#[test]
fn each_menu_row_maps_to_its_own_op() {
    assert_eq!(op_for(ROW_LOCK, ACTION_ACTIVATE, 0.0), Some(Op::Lock));
    assert_eq!(op_for(ROW_LOG_OUT, ACTION_ACTIVATE, 0.0), Some(Op::LogOut));
    assert_eq!(op_for(ROW_SUSPEND, ACTION_ACTIVATE, 0.0), Some(Op::Suspend));
    assert_eq!(op_for(ROW_RESTART, ACTION_ACTIVATE, 0.0), Some(Op::Restart));
    assert_eq!(
        op_for(ROW_SHUTDOWN, ACTION_ACTIVATE, 0.0),
        Some(Op::Shutdown)
    );
}

#[test]
fn each_op_names_the_logind_call_or_the_niri_line() {
    assert_eq!(Op::Suspend.call(), Call::Logind("Suspend"));
    assert_eq!(Op::Restart.call(), Call::Logind("Reboot"));
    assert_eq!(Op::Shutdown.call(), Call::Logind("PowerOff"));
    // Lock is the session walk, not one method — its own variant says so.
    assert_eq!(Op::Lock.call(), Call::LogindLockWalk);
    assert_eq!(Op::LogOut.call(), Call::Niri(niri::QUIT));
    assert_eq!(
        niri::QUIT,
        r#"{"Action":{"Quit":{"skip_confirmation":false}}}"#
    );
}

#[test]
fn every_op_has_its_own_witness_name() {
    let names: Vec<&str> = [Op::Lock, Op::LogOut, Op::Suspend, Op::Restart, Op::Shutdown]
        .iter()
        .map(|op| op.name())
        .collect();
    assert_eq!(
        names,
        vec!["lock", "log-out", "suspend", "restart", "shutdown"]
    );
}

#[test]
fn the_rows_that_perform_nothing_perform_nothing() {
    // The button opens its menu (RFLAG_HAS_MENU), so a click never comes
    // back; the separator and the battery have nothing to do either.
    assert_eq!(op_for(ROW_POWER, ACTION_ACTIVATE, 0.0), None);
    assert_eq!(op_for(ROW_SEP, ACTION_ACTIVATE, 0.0), None);
    assert_eq!(op_for(ROW_BATTERY, ACTION_ACTIVATE, 0.0), None);
    // An id this applet never wrote.
    assert_eq!(op_for(9999, ACTION_ACTIVATE, 0.0), None);
}

#[test]
fn the_dry_run_is_armed_by_exactly_one() {
    assert!(dry_run(Some("1")));
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("yes"),
        Some(" 1"),
    ] {
        assert!(!dry_run(value), "{value:?} armed the dry run");
    }
}

#[test]
fn only_activate_performs() {
    // A middle click, a right click or a slider drag on a power row is not a
    // reason to shut the machine down.
    for kind in [ACTION_SECONDARY, ACTION_MIDDLE, ACTION_SET_VALUE] {
        for row in [
            ROW_LOCK,
            ROW_LOG_OUT,
            ROW_SUSPEND,
            ROW_RESTART,
            ROW_SHUTDOWN,
        ] {
            assert_eq!(op_for(row, kind, 0.5), None, "row {row} kind {kind}");
        }
    }
}

#[test]
fn the_battery_menu_rows_map_to_their_ops() {
    // The slider takes only a value; its units are the row's own, passed on.
    assert_eq!(
        op_for(ROW_BRIGHTNESS, ACTION_SET_VALUE, 180.0),
        Some(Op::Brightness(180.0))
    );
    assert_eq!(op_for(ROW_BRIGHTNESS, ACTION_ACTIVATE, 180.0), None);
    // A power mode's row is its index in the daemon's list.
    assert_eq!(
        op_for(ROW_PROFILE_BASE, ACTION_ACTIVATE, 0.0),
        Some(Op::Profile(0))
    );
    assert_eq!(
        op_for(ROW_PROFILE_BASE + 2, ACTION_ACTIVATE, 0.0),
        Some(Op::Profile(2))
    );
    assert_eq!(
        op_for(ROW_PROFILE_BASE + MAX_PROFILES as u32, ACTION_ACTIVATE, 0.0),
        None
    );
    assert_eq!(
        op_for(ROW_SETTINGS, ACTION_ACTIVATE, 0.0),
        Some(Op::Settings)
    );
    // The state row is disabled and the separators are separators.
    for row in [ROW_STATE, ROW_MODES_SEP, ROW_SETTINGS_SEP] {
        assert_eq!(op_for(row, ACTION_ACTIVATE, 0.0), None, "row {row}");
    }
    // A value on any other row is nothing.
    assert_eq!(op_for(ROW_SHUTDOWN, ACTION_SET_VALUE, 1.0), None);
}

#[test]
fn the_battery_menu_ops_name_their_calls() {
    assert_eq!(Op::Brightness(3.0).call(), Call::SetBrightness);
    assert_eq!(Op::Profile(1).call(), Call::SetProfile);
    assert_eq!(Op::Settings.call(), Call::Settings(SETTINGS_PANEL));
    assert_eq!(SETTINGS_PANEL, caustics_sidecar::settings::Panel::Power);
}

/// The settings row names a panel, never a program: `$CAUSTICS_SETTINGS` wins,
/// the first known program on `PATH` follows, and with neither there is
/// nothing to run (the click is witnessed and nothing starts).
#[test]
fn the_settings_program_is_the_override_or_the_first_found_or_none() {
    use caustics_sidecar::settings::resolve;
    use std::ffi::OsStr;

    let found = |names: &'static [&'static str]| move |name: &str| names.contains(&name);
    let gnome = Some(OsStr::new("gnome-control-center"));
    let chosen = resolve(SETTINGS_PANEL, gnome, None, found(&[])).expect("an override is taken");
    assert_eq!(chosen.describe(), "gnome-control-center power");
    let niri = Some(OsStr::new("niri"));
    let chosen = resolve(SETTINGS_PANEL, None, niri, found(&["cosmic-settings"]))
        .expect("cosmic-settings is on PATH");
    assert_eq!(chosen.describe(), "cosmic-settings power");
    assert_eq!(resolve(SETTINGS_PANEL, None, niri, found(&[])), None);
}

/// Log Out reads niri's reply, so an `Err` is witnessed as one rather
/// than as "ok". Only the judgement is tested — never the socket, because the
/// session running the tests may well be a real niri.
#[test]
fn log_out_is_ok_only_when_niri_says_so() {
    assert_eq!(niri::outcome(r#"{"Ok":"Handled"}"#), Ok(()));
    assert_eq!(
        niri::outcome("{\"Ok\":\"Handled\"}\n"),
        Ok(()),
        "the line's newline"
    );
    assert_eq!(
        niri::outcome(r#"{"Err":"error quitting"}"#),
        Err(r#"niri answered {"Err":"error quitting"}"#.to_owned())
    );
    assert_eq!(
        niri::outcome(""),
        Err("niri closed the socket without a reply".to_owned())
    );
    assert!(
        niri::outcome("Handled").is_err(),
        "a reply this sidecar does not know"
    );
}

/// A broken pipe is the bar gone and exits 0; any other write error
/// exits 1, not 0 as though the bar had left.
#[test]
fn only_a_broken_pipe_is_an_ordinary_exit() {
    use std::io::{Error, ErrorKind};

    assert_eq!(
        caustics_power::write_failed(&Error::from(ErrorKind::BrokenPipe)),
        0
    );
    assert_eq!(
        caustics_power::write_failed(&Error::from(ErrorKind::StorageFull)),
        1
    );
    assert_eq!(caustics_power::write_failed(&Error::other("EIO")), 1);
}
