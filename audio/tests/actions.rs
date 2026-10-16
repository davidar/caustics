//! The dispatch: each row id, with each action kind, to the command it would
//! run — asserted against the *description* of the call, so no test ever moves
//! the volume, mutes a microphone or opens a settings window.

use caustics_audio::actions::{Call, Op, SETTINGS_PANEL, TOGGLE, op_for, percent};
use caustics_audio::rows::{ROW_AUDIO, ROW_MIC, ROW_MUTE, ROW_SEP, ROW_SETTINGS, ROW_VOLUME};
use caustics_audio::wpctl::{SINK, SOURCE};
use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MIDDLE, ACTION_SECONDARY, ACTION_SET_VALUE,
};

/// `wpctl <words>`, the way [`Call::Wpctl`] holds it.
fn wpctl(words: &[&str]) -> Call {
    Call::Wpctl(words.iter().map(|w| (*w).to_owned()).collect())
}

#[test]
fn the_slider_sets_the_sink_volume_in_whole_percent() {
    // The bar de-normalised the drag into the row's own 0..100 already
    // (docs/abi.md, *Action*), so what arrives is a percentage.
    assert_eq!(
        op_for(ROW_VOLUME, ACTION_SET_VALUE, 56.0),
        Some(Op::SetVolume(56))
    );
    assert_eq!(
        Op::SetVolume(56).call(),
        wpctl(&["set-volume", SINK, "56%"]),
        "the one command `wpctl` takes for this"
    );
    assert_eq!(Op::SetVolume(0).call(), wpctl(&["set-volume", SINK, "0%"]));
    assert_eq!(
        Op::SetVolume(100).call(),
        wpctl(&["set-volume", SINK, "100%"])
    );
}

#[test]
fn a_drag_is_rounded_and_never_overamplifies() {
    assert_eq!(percent(55.4), 55);
    assert_eq!(percent(55.5), 56);
    assert_eq!(percent(0.0), 0);
    assert_eq!(percent(100.0), 100);
    // Above the row's own max, or below its min, or not a number at all:
    // dragging a slider is not a way to overamplify a sink.
    assert_eq!(percent(100.4), 100);
    assert_eq!(percent(153.0), 100);
    assert_eq!(percent(-1.0), 0);
    // A value that is not a number is not a volume: silence is the safe
    // reading of nonsense, not full volume.
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(percent(bad), 0, "{bad}");
    }
    // Which the table carries through.
    assert_eq!(
        op_for(ROW_VOLUME, ACTION_SET_VALUE, 240.0),
        Some(Op::SetVolume(100))
    );
    assert_eq!(
        op_for(ROW_VOLUME, ACTION_SET_VALUE, f32::NAN),
        Some(Op::SetVolume(0))
    );
}

#[test]
fn each_mute_row_toggles_its_own_node() {
    assert_eq!(op_for(ROW_MUTE, ACTION_ACTIVATE, 0.0), Some(Op::MuteSink));
    assert_eq!(op_for(ROW_MIC, ACTION_ACTIVATE, 0.0), Some(Op::MuteSource));
    // `toggle` rather than `1`: two clicks in a row can never both mean mute,
    // whatever this sidecar last believed.
    assert_eq!(Op::MuteSink.call(), wpctl(&["set-mute", SINK, TOGGLE]));
    assert_eq!(Op::MuteSource.call(), wpctl(&["set-mute", SOURCE, TOGGLE]));
    assert_eq!(TOGGLE, "toggle");
    assert_eq!(SINK, "@DEFAULT_AUDIO_SINK@");
    assert_eq!(SOURCE, "@DEFAULT_AUDIO_SOURCE@");
}

#[test]
fn a_middle_click_on_the_chip_is_the_mute() {
    assert_eq!(op_for(ROW_AUDIO, ACTION_MIDDLE, 0.0), Some(Op::MuteSink));
    // …and a left click on it opens the menu (RFLAG_HAS_MENU), so it never
    // comes back at all.
    assert_eq!(op_for(ROW_AUDIO, ACTION_ACTIVATE, 0.0), None);
}

#[test]
fn the_last_row_spawns_the_sound_page() {
    assert_eq!(
        op_for(ROW_SETTINGS, ACTION_ACTIVATE, 0.0),
        Some(Op::Settings)
    );
    assert_eq!(Op::Settings.call(), Call::Settings(SETTINGS_PANEL));
    assert_eq!(SETTINGS_PANEL, caustics_sidecar::settings::Panel::Sound);
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
    assert_eq!(chosen.describe(), "gnome-control-center sound");
    let niri = Some(OsStr::new("niri"));
    let chosen = resolve(SETTINGS_PANEL, None, niri, found(&["cosmic-settings"]))
        .expect("cosmic-settings is on PATH");
    assert_eq!(chosen.describe(), "cosmic-settings sound");
    assert_eq!(resolve(SETTINGS_PANEL, None, niri, found(&[])), None);
}

#[test]
fn every_op_has_its_own_witness_name() {
    let names: Vec<&str> = [Op::SetVolume(1), Op::MuteSink, Op::MuteSource, Op::Settings]
        .iter()
        .map(|op| op.name())
        .collect();
    assert_eq!(names, vec!["set-volume", "mute", "mute-mic", "settings"]);
}

#[test]
fn the_rows_that_perform_nothing_perform_nothing() {
    assert_eq!(op_for(ROW_SEP, ACTION_ACTIVATE, 0.0), None);
    // An id this applet never wrote.
    assert_eq!(op_for(9999, ACTION_ACTIVATE, 0.0), None);
    // A kind on a row that has no meaning for it: a slider that is activated,
    // a checkbox that is dragged, a right click on anything.
    assert_eq!(op_for(ROW_VOLUME, ACTION_ACTIVATE, 0.0), None);
    for row in [ROW_AUDIO, ROW_MUTE, ROW_MIC, ROW_SEP, ROW_SETTINGS] {
        assert_eq!(
            op_for(row, ACTION_SET_VALUE, 50.0),
            None,
            "row {row} set-value"
        );
        assert_eq!(
            op_for(row, ACTION_SECONDARY, 0.0),
            None,
            "row {row} secondary"
        );
    }
    for row in [ROW_VOLUME, ROW_MUTE, ROW_MIC, ROW_SEP, ROW_SETTINGS] {
        assert_eq!(op_for(row, ACTION_MIDDLE, 0.0), None, "row {row} middle");
    }
}

/// A `wpctl` that never answers — a PipeWire stopped or in a debugger —
/// is given up on at the limit and killed, rather than holding the loop.
/// `sleep` stands in for it; nothing here runs `wpctl`.
#[tokio::test]
async fn a_child_that_never_answers_is_given_up_on() {
    use std::time::{Duration, Instant};

    use caustics_audio::wpctl::{TIMEOUT, output};

    let started = Instant::now();
    let why = output("sleep", &["30"], Duration::from_millis(200))
        .await
        .expect_err("a child that does not answer is an error");
    assert!(why.contains("no answer in 200 ms"), "{why}");
    assert!(started.elapsed() < Duration::from_secs(5), "the limit held");
    // One that does answer is its output, exit status and all.
    let done = output("false", &[], Duration::from_secs(5))
        .await
        .expect("false runs");
    assert!(!done.status.success());
    assert_eq!(TIMEOUT, Duration::from_secs(2));
}
