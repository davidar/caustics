//! What `caustics-displays` publishes for a list of outputs, read back through
//! the wire's own reader: every frame passes *Rows*'s validation, and its rows
//! are the menu `src/rows.rs` describes.

mod common;

use caustics_bar_abi::buffer::APPLET_DISPLAYS;
use caustics_bar_abi::wire::{
    self, Frame, MAX_ROWS, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_RADIO,
    RFLAG_SUBMENU, ROW_ITEM, ROW_MENU, ROW_SEPARATOR, Rows,
};
use caustics_displays::niri::{Mode, Output};
use caustics_displays::rows::{self, Slots};

/// The frame through the wire and back, as the bar would read it.
fn round_trip(outputs: &[Output], slots: &Slots) -> Rows {
    let frame = rows::picture(7, outputs, slots);
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &frame).unwrap();
    match wire::read(&mut bytes.as_slice()).unwrap() {
        Some(Frame::Rows(rows)) => rows,
        other => panic!("not a rows frame: {other:?}"),
    }
}

fn slotted(outputs: &[Output]) -> Slots {
    let mut slots = Slots::default();
    slots.assign(outputs);
    slots
}

/// The index of the row with this id.
fn at(rows: &Rows, id: u32) -> usize {
    rows.rows
        .iter()
        .position(|r| r.id == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

/// The ids under the row with this id, in order.
fn children(rows: &Rows, id: u32) -> Vec<u32> {
    let parent = at(rows, id) as u32;
    rows.rows
        .iter()
        .filter(|r| r.parent == parent)
        .map(|r| r.id)
        .collect()
}

fn label(rows: &Rows, id: u32) -> String {
    rows.label(at(rows, id))
        .map(|l| l.into_owned())
        .unwrap_or_default()
}

#[test]
fn no_outputs_is_no_chip() {
    let rows = round_trip(&[], &Slots::default());
    assert_eq!(rows.applet, APPLET_DISPLAYS);
    assert!(rows.rows.is_empty());
}

#[test]
fn the_chip_lists_each_output_then_the_note() {
    let outputs = common::outputs();
    let slots = slotted(&outputs);
    let rows = round_trip(&outputs, &slots);
    assert_eq!(rows.applet, APPLET_DISPLAYS);
    let chip = &rows.rows[0];
    assert_eq!((chip.id, chip.role), (rows::ROW_CHIP, ROW_ITEM));
    assert_ne!(chip.flags & RFLAG_HAS_MENU, 0);
    assert_eq!(rows.icon(0), Some(rows::ICON));

    // DP-1 sorts first and takes slot 0; eDP-1 slot 1.
    let (dp, edp) = (rows::base(0), rows::base(1));
    assert_eq!(
        children(&rows, rows::ROW_CHIP),
        [dp, edp, rows::ROW_NOTE_SEP, rows::ROW_NOTE]
    );
    assert_eq!(label(&rows, dp), "DP-1 · Example Inc. EX2720");
    assert_eq!(label(&rows, edp), "eDP-1 · Example Panel Co. 0x1234");
    let note = &rows.rows[at(&rows, rows::ROW_NOTE)];
    assert_ne!(note.flags & RFLAG_DISABLED, 0);
    assert_eq!(label(&rows, rows::ROW_NOTE), rows::LABEL_NOTE);
    assert_eq!(
        rows.body(at(&rows, rows::ROW_NOTE)).as_deref(),
        Some(rows::BODY_NOTE)
    );
    assert_eq!(rows.rows[at(&rows, rows::ROW_NOTE_SEP)].role, ROW_SEPARATOR);
}

#[test]
fn an_output_that_is_off_is_drawn_off_and_offers_only_on() {
    let outputs = common::outputs();
    let rows = round_trip(&outputs, &slotted(&outputs));
    let dp = rows::base(0);
    let row = &rows.rows[at(&rows, dp)];
    assert_ne!(row.flags & RFLAG_SUBMENU, 0);
    assert_eq!(row.flags & RFLAG_CHECKED, 0, "off is unchecked");
    assert_eq!(children(&rows, dp), [dp + rows::OFF_ON]);
    let on = &rows.rows[at(&rows, dp + rows::OFF_ON)];
    assert_eq!(on.flags & RFLAG_CHECKED, 0);
    assert_eq!(label(&rows, dp + rows::OFF_ON), rows::LABEL_ON);
}

#[test]
fn an_output_that_is_on_has_on_scale_mode_and_rotation() {
    let outputs = common::outputs();
    let rows = round_trip(&outputs, &slotted(&outputs));
    let b = rows::base(1);
    assert_ne!(rows.rows[at(&rows, b)].flags & RFLAG_CHECKED, 0);
    assert_eq!(
        children(&rows, b),
        [
            b + rows::OFF_ON,
            b + rows::OFF_SEP,
            b + rows::OFF_SCALE,
            b + rows::OFF_MODE,
            b + rows::OFF_ROTATION
        ]
    );
    assert_ne!(
        rows.rows[at(&rows, b + rows::OFF_ON)].flags & RFLAG_CHECKED,
        0
    );
    assert_eq!(label(&rows, b + rows::OFF_SCALE), "Scale · 1.75");
    assert_eq!(
        label(&rows, b + rows::OFF_MODE),
        "Mode · 2880×1800 @ 120.001 Hz"
    );
    assert_eq!(label(&rows, b + rows::OFF_ROTATION), "Rotation · Normal");

    // Scale: the seven, 1.75 checked, and nothing extra.
    let scales = children(&rows, b + rows::OFF_SCALE);
    assert_eq!(scales.len(), 7);
    let labels: Vec<String> = scales.iter().map(|id| label(&rows, *id)).collect();
    assert_eq!(labels, ["1", "1.25", "1.5", "1.75", "2", "2.5", "3"]);
    let checked: Vec<u32> = scales
        .iter()
        .copied()
        .filter(|id| rows.rows[at(&rows, *id)].flags & RFLAG_CHECKED != 0)
        .collect();
    assert_eq!(checked, [b + rows::OFF_SCALES + 3]);
    assert!(
        scales
            .iter()
            .all(|id| rows.rows[at(&rows, *id)].flags & RFLAG_RADIO != 0)
    );

    // Mode: niri's list in its order, the preferred one named, the current one checked.
    let modes = children(&rows, b + rows::OFF_MODE);
    let labels: Vec<String> = modes.iter().map(|id| label(&rows, *id)).collect();
    assert_eq!(
        labels,
        [
            "2880×1800 @ 120.001 Hz · preferred",
            "2880×1800 @ 60.001 Hz",
            "1920×1200 @ 59.94 Hz"
        ]
    );
    assert_ne!(rows.rows[at(&rows, modes[0])].flags & RFLAG_CHECKED, 0);
    assert_eq!(rows.rows[at(&rows, modes[1])].flags & RFLAG_CHECKED, 0);

    // Rotation: the four, Normal checked.
    let rotations = children(&rows, b + rows::OFF_ROTATION);
    let labels: Vec<String> = rotations.iter().map(|id| label(&rows, *id)).collect();
    assert_eq!(labels, ["Normal", "90°", "180°", "270°"]);
    assert_ne!(rows.rows[at(&rows, rotations[0])].flags & RFLAG_CHECKED, 0);
}

#[test]
fn a_scale_that_is_none_of_the_seven_is_an_eighth_row() {
    let mut outputs = common::outputs();
    outputs[1].scale = Some(1.6);
    let rows = round_trip(&outputs, &slotted(&outputs));
    let b = rows::base(1);
    let scales = children(&rows, b + rows::OFF_SCALE);
    assert_eq!(scales.len(), 8);
    assert_eq!(label(&rows, scales[7]), "1.6");
    assert_ne!(rows.rows[at(&rows, scales[7])].flags & RFLAG_CHECKED, 0);
    assert_eq!(label(&rows, b + rows::OFF_SCALE), "Scale · 1.6");
}

#[test]
fn a_flipped_transform_is_named_and_no_rotation_is_checked() {
    let mut outputs = common::outputs();
    outputs[1].transform = Some("Flipped90".to_owned());
    let rows = round_trip(&outputs, &slotted(&outputs));
    let b = rows::base(1);
    assert_eq!(
        label(&rows, b + rows::OFF_ROTATION),
        "Rotation · Flipped 90°"
    );
    assert!(
        children(&rows, b + rows::OFF_ROTATION)
            .iter()
            .all(|id| rows.rows[at(&rows, *id)].flags & RFLAG_CHECKED == 0)
    );
}

#[test]
fn an_output_keeps_its_slot_while_another_comes_and_goes() {
    let outputs = common::outputs();
    let mut slots = slotted(&outputs);
    assert_eq!(slots.slot_of("eDP-1"), Some(1));
    // DP-1 unplugged: eDP-1 keeps slot 1, so its row ids do not move.
    let edp_only: Vec<Output> = outputs
        .iter()
        .filter(|o| o.name == "eDP-1")
        .cloned()
        .collect();
    slots.assign(&edp_only);
    assert_eq!(slots.slot_of("eDP-1"), Some(1));
    assert_eq!(slots.slot_of("DP-1"), None);
    // A new monitor takes the lowest free slot.
    let mut hdmi = outputs[0].clone();
    hdmi.name = "HDMI-A-1".to_owned();
    let mut next = edp_only.clone();
    next.push(hdmi);
    slots.assign(&next);
    assert_eq!(slots.slot_of("HDMI-A-1"), Some(0));
    assert_eq!(slots.slot_of("eDP-1"), Some(1));
}

#[test]
fn many_modes_fit_the_frame() {
    // Eight outputs of 200 modes each: the budget shares the rows, and the
    // frame still passes the reader (which rejects more than MAX_ROWS).
    let mode = |k: u32| Mode {
        width: 640 + k as u16,
        height: 480,
        refresh: 60000,
        preferred: k == 0,
    };
    let outputs: Vec<Output> = (0..8)
        .map(|i| Output {
            name: format!("DP-{i}"),
            make: String::new(),
            model: String::new(),
            on: true,
            modes: (0..200).map(mode).collect(),
            current: Some(0),
            scale: Some(1.0),
            transform: Some("Normal".to_owned()),
        })
        .collect();
    let rows = round_trip(&outputs, &slotted(&outputs));
    assert!(rows.rows.len() <= MAX_ROWS);
    let budget = rows::modes_budget(8);
    assert!(budget > 0);
    assert_eq!(
        children(&rows, rows::base(7) + rows::OFF_MODE).len(),
        budget
    );
    // One output gets as many as its slot can name.
    let one = &outputs[..1];
    let rows = round_trip(one, &slotted(one));
    assert_eq!(
        children(&rows, rows::base(0) + rows::OFF_MODE).len(),
        rows::MAX_MODES
    );
    assert!(
        rows.rows
            .iter()
            .all(|r| r.role == ROW_ITEM || r.role == ROW_MENU || r.role == ROW_SEPARATOR)
    );
}

#[test]
fn the_labels_and_the_summary() {
    assert_eq!(rows::scale_label(1.0), "1");
    assert_eq!(rows::scale_label(1.25), "1.25");
    assert_eq!(rows::scale_label(1.5), "1.5");
    assert_eq!(rows::refresh_label(60000), "60");
    assert_eq!(rows::refresh_label(59940), "59.94");
    assert_eq!(rows::refresh_label(120001), "120.001");
    assert_eq!(
        rows::summary(&common::outputs()),
        "DP-1 off; eDP-1 on 2880x1800@120.001 scale=1.75 Normal"
    );
    assert_eq!(rows::summary(&[]), "none");
}
