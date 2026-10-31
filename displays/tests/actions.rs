//! The dispatch table: which row asks niri for what, and which asks for
//! nothing. No niri, no socket.

mod common;

use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_SECONDARY,
};
use caustics_displays::actions::{self, Decision};
use caustics_displays::niri::{Change, Output};
use caustics_displays::rows::{self, Slots};

fn slotted(outputs: &[Output]) -> Slots {
    let mut slots = Slots::default();
    slots.assign(outputs);
    slots
}

fn click(row: u32, outputs: &[Output]) -> Decision {
    actions::decide(row, ACTION_ACTIVATE, outputs, &slotted(outputs))
}

fn change(output: &str, change: Change) -> Decision {
    Decision::Change {
        output: output.to_owned(),
        change,
    }
}

// The fixture's slots: DP-1 (off) is 0, eDP-1 (on) is 1.
const DP: u32 = rows::SLOT_STRIDE;
const EDP: u32 = 2 * rows::SLOT_STRIDE;

#[test]
fn on_turns_an_output_that_is_off_on() {
    let outputs = common::outputs();
    assert_eq!(
        click(DP + rows::OFF_ON, &outputs),
        change("DP-1", Change::On)
    );
}

#[test]
fn the_only_output_on_is_never_turned_off() {
    let outputs = common::outputs();
    assert_eq!(
        click(EDP + rows::OFF_ON, &outputs),
        Decision::LastOn {
            output: "eDP-1".to_owned()
        }
    );
}

#[test]
fn with_two_on_either_turns_off() {
    let outputs = common::both_on();
    assert_eq!(
        click(EDP + rows::OFF_ON, &outputs),
        change("eDP-1", Change::Off)
    );
    assert_eq!(
        click(DP + rows::OFF_ON, &outputs),
        change("DP-1", Change::Off)
    );
}

#[test]
fn a_scale_row_is_its_scale() {
    let outputs = common::outputs();
    for (k, s) in rows::SCALES.iter().enumerate() {
        assert_eq!(
            click(EDP + rows::OFF_SCALES + k as u32, &outputs),
            change("eDP-1", Change::Scale(*s))
        );
    }
    // No eighth row while the current scale is one of the seven.
    assert_eq!(
        click(EDP + rows::OFF_SCALES + 7, &outputs),
        Decision::Nothing
    );
    let mut odd = outputs.clone();
    odd[1].scale = Some(1.6);
    assert_eq!(
        click(EDP + rows::OFF_SCALES + 7, &odd),
        change("eDP-1", Change::Scale(1.6))
    );
}

#[test]
fn a_mode_row_is_niris_mode() {
    let outputs = common::outputs();
    let third = outputs[1].modes[2];
    assert_eq!(
        click(EDP + rows::OFF_MODES + 2, &outputs),
        change("eDP-1", Change::Mode(third))
    );
    assert_eq!(
        click(EDP + rows::OFF_MODES + 3, &outputs),
        Decision::Nothing
    );
}

#[test]
fn a_rotation_row_is_its_transform() {
    let outputs = common::outputs();
    for (k, (t, _)) in rows::ROTATIONS.iter().enumerate() {
        assert_eq!(
            click(EDP + rows::OFF_ROTATIONS + k as u32, &outputs),
            change("eDP-1", Change::Transform(t))
        );
    }
    assert_eq!(
        click(EDP + rows::OFF_ROTATIONS + 4, &outputs),
        Decision::Nothing
    );
}

#[test]
fn rows_that_perform_nothing() {
    let outputs = common::outputs();
    for row in [
        rows::ROW_CHIP,
        rows::ROW_NOTE,
        rows::ROW_NOTE_SEP,
        EDP + rows::OFF_OUTPUT,
        EDP + rows::OFF_SCALE,
        EDP + rows::OFF_MODE,
        EDP + rows::OFF_ROTATION,
        EDP + rows::OFF_SEP,
        // An output that is off has no radio rows.
        DP + rows::OFF_SCALES,
        DP + rows::OFF_MODES,
        // A slot nobody has.
        3 * rows::SLOT_STRIDE + rows::OFF_ON,
        // Past the slots.
        0xFFFF_FFFF,
    ] {
        assert_eq!(click(row, &outputs), Decision::Nothing, "row {row}");
    }
    // Only the primary click performs.
    assert_eq!(
        actions::decide(
            EDP + rows::OFF_SCALES,
            ACTION_SECONDARY,
            &outputs,
            &slotted(&outputs)
        ),
        Decision::Nothing
    );
}

#[test]
fn the_menu_notices_are_notices() {
    assert!(actions::is_menu_notice(ACTION_MENU_OPEN));
    assert!(actions::is_menu_notice(ACTION_MENU_CLOSE));
    assert!(!actions::is_menu_notice(ACTION_ACTIVATE));
}

#[test]
fn the_witness_names() {
    let outputs = common::outputs();
    assert_eq!(actions::change_name(&Change::Scale(1.5)), "scale 1.5");
    assert_eq!(
        actions::change_name(&Change::Mode(outputs[1].modes[0])),
        "mode 2880x1800@120.001"
    );
    assert_eq!(
        actions::change_name(&Change::Transform("90")),
        "rotation 90"
    );
    assert_eq!(actions::change_name(&Change::Off), "off");
}
