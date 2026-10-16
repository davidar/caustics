//! The Output and Input submenus, from `wpctl status`
//! read through [`parse_status`] to the rows frame the bar would decode, and a
//! click on a device to the argv it would run — never run here, so no test
//! moves this machine's default device.
//!
//! **The fixtures.** `wpctl-status.txt` is *recorded* from a PipeWire session and
//! scrubbed (the client list cut to three, `user@host`, pids and the cookie
//! replaced, the audio hardware, the codec and a virtual camera given generic
//! names, column widths kept): one sink, one source, and a `Video` block whose
//! `Sources:` holds a camera that must not become a microphone. The rest are
//! **derived** from it by hand, because recording them would have meant
//! plugging in a headset or moving the default under the session the test runs
//! in: `-several-` (three sinks, one muted, two sources), `-default-moved-`
//! (the same with the `*` on the headset), `-monitor-` (two monitor sources
//! that must be skipped, and an echo-cancel filter source that must not),
//! `-filters-` (an EasyEffects sink as the default under `Filters:`,
//! its source, a monitor, and two entries of other classes), `-no-input-` (no
//! source at all). Every `Filters:` line is in `wpctl.c`'s exact format,
//! `"%c %4u. %-60s [%s]"`.

use caustics_audio::actions::{Call, Op, admit, op_for};
use caustics_audio::rows::{
    DEVICE_ID_MASK, ICON_INPUT, ICON_OUTPUT, KIND_INPUT, KIND_OUTPUT, LABEL_INPUT, LABEL_NO_DEVICE,
    LABEL_OUTPUT, ROW_INPUT, ROW_NO_INPUT, ROW_NO_OUTPUT, ROW_OUTPUT, ROW_SETTINGS, device_of,
    device_row, picture,
};
use caustics_audio::wpctl::{Audio, Device, Devices, is_monitor, parse_status};
use caustics_bar_abi::buffer::{
    ACTION_ACTIVATE, ACTION_MIDDLE, ACTION_SECONDARY, ACTION_SET_VALUE,
};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_RADIO, RFLAG_SUBMENU,
    ROW_MENU, Rows,
};

const RECORDED: &str = include_str!("data/wpctl-status.txt");
const SEVERAL: &str = include_str!("data/wpctl-status-several-derived.txt");
const MOVED: &str = include_str!("data/wpctl-status-default-moved-derived.txt");
const MONITOR: &str = include_str!("data/wpctl-status-monitor-derived.txt");
const NO_INPUT: &str = include_str!("data/wpctl-status-no-input-derived.txt");
const FILTERS: &str = include_str!("data/wpctl-status-filters-derived.txt");

const AUDIO: Audio = Audio {
    volume: 0.72,
    muted: false,
    mic_muted: false,
};

fn device(id: u32, label: &str, default: bool) -> Device {
    Device {
        id,
        label: label.to_owned(),
        default,
    }
}

/// Through `wire::write` and `wire::read`, so every assertion is about what
/// the bar would decode — the reader's own checks (parents before children, a
/// parent that is an item or a submenu) included.
fn decoded(devices: &Devices) -> Rows {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &picture(1, Some(&AUDIO), devices, true)).expect("writing to a Vec");
    match wire::read(&mut bytes.as_slice()) {
        Ok(Some(Frame::Rows(rows))) => rows,
        other => panic!("expected a rows frame, got {other:?}"),
    }
}

fn index_of(rows: &Rows, id: u32) -> usize {
    rows.rows
        .iter()
        .position(|row| row.id == id)
        .unwrap_or_else(|| panic!("no row {id:#x}"))
}

/// A submenu's children, as `(id, label, flags)`, in frame order.
fn children(rows: &Rows, submenu: u32) -> Vec<(u32, String, u32)> {
    let parent = index_of(rows, submenu) as u32;
    (0..rows.rows.len())
        .filter(|&i| rows.rows[i].parent == parent)
        .map(|i| {
            let row = &rows.rows[i];
            (
                row.id,
                rows.label(i).map(|l| l.into_owned()).unwrap_or_default(),
                row.flags,
            )
        })
        .collect()
}

const RADIO: u32 = RFLAG_RADIO | RFLAG_CHECKABLE;
const DEFAULT: u32 = RADIO | RFLAG_CHECKED;

// -- parsing ----------------------------------------------------------------

#[test]
fn the_recorded_status_is_one_sink_and_one_source() {
    let devices = parse_status(RECORDED);
    let builtin = "Built-in Audio Controller Analog Stereo";
    assert_eq!(devices.outputs, vec![device(60, builtin, true)]);
    // The camera under `Video` → `Sources:` (`*  68. Integrated Camera`) is
    // not a microphone, and the `Devices:` cards are not sinks.
    assert_eq!(devices.inputs, vec![device(61, builtin, true)]);
    assert_eq!(devices.default_output(), Some(60));
    assert_eq!(devices.default_input(), Some(61));
}

#[test]
fn several_devices_keep_their_order_and_one_default() {
    let devices = parse_status(SEVERAL);
    assert_eq!(
        devices.outputs,
        vec![
            device(
                57,
                "Integrated GPU Audio Controller Digital Stereo (HDMI)",
                false
            ),
            device(60, "Built-in Audio Controller Analog Stereo", true),
            // `[vol: 0.40 MUTED]` is cut with the rest of the bracket.
            device(95, "Headphones", false),
        ]
    );
    assert_eq!(
        devices.inputs,
        vec![
            device(61, "Built-in Audio Controller Analog Stereo", true),
            device(96, "Headphones", false),
        ]
    );
}

#[test]
fn a_default_change_moves_the_star_and_nothing_else() {
    let before = parse_status(SEVERAL);
    let after = parse_status(MOVED);
    assert_eq!(
        (after.default_output(), after.default_input()),
        (Some(95), Some(96))
    );
    let ids = |list: &[Device]| {
        list.iter()
            .map(|d| (d.id, d.label.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&before.outputs), ids(&after.outputs));
    assert_eq!(ids(&before.inputs), ids(&after.inputs));
}

#[test]
fn monitor_sources_are_not_inputs_and_a_filter_source_is() {
    let devices = parse_status(MONITOR);
    // `Monitor of …` (a description) and `….monitor` (a node name) both go;
    // the echo-cancel node under `Filters:` is `[Audio/Source]`, a microphone
    // anyone may pick.
    assert_eq!(
        devices.inputs,
        vec![
            device(61, "Built-in Audio Controller Analog Stereo", true),
            device(84, "Echo-Cancel Source", false),
        ]
    );
    assert_eq!(devices.outputs.len(), 1);
    assert!(is_monitor("Monitor of Built-in Audio"));
    assert!(is_monitor(
        "alsa_output.pci-0000_c4_00.6.analog-stereo.monitor"
    ));
    assert!(!is_monitor("Monitors Audio"), "a brand is not a monitor");
}

#[test]
fn nothing_that_is_not_a_status_is_a_device() {
    for text in [
        "",
        "\n",
        "Audio\n",
        "Audio\n ├─ Sinks:\n │  \n",
        "Node '1' not found\n",
    ] {
        assert_eq!(parse_status(text), Devices::default(), "{text:?}");
    }
    // An id that is not a number is skipped rather than fatal; the good line
    // after it still counts.
    let text = "Audio\n ├─ Sinks:\n │      x3. Bad\n │      14. Good [vol: 1.00]\n";
    assert_eq!(parse_status(text).outputs, vec![device(14, "Good", false)]);
}

#[test]
fn a_device_with_no_description_is_listed_by_its_node() {
    // An empty description must not drop the device, or a lone sink reads
    // "No device" while audio plays. wpctl pads the empty `%-35s`, then the
    // volume; a line trimmed right after the id is the same device.
    let pad = " ".repeat(35);
    let text = format!("Audio\n ├─ Sinks:\n │  *   60. {pad} [vol: 0.50]\n │      12. \n");
    assert_eq!(
        parse_status(&text).outputs,
        vec![device(60, "Node 60", true), device(12, "Node 12", false)]
    );
    // And a filter's empty `%-60s`.
    let pad = " ".repeat(60);
    let text = format!("Audio\n ├─ Filters:\n │      90. {pad} [Audio/Sink]\n");
    assert_eq!(
        parse_status(&text).outputs,
        vec![device(90, "Node 90", false)]
    );
}

#[test]
fn a_filter_of_an_audio_class_is_a_device_and_its_default_is_checked() {
    let devices = parse_status(FILTERS);
    // EasyEffects is the default output: its row comes after the hardware
    // sink and carries the check, so the menu can go back to it.
    assert_eq!(
        devices.outputs,
        vec![
            device(60, "Built-in Audio Controller Analog Stereo", false),
            device(90, "Easy Effects Sink", true),
        ]
    );
    assert_eq!(devices.default_output(), Some(90));
    // Its source is an input; a monitor under `Filters:` is still a monitor;
    // `[Stream/Input/Audio]` and `[Audio/Sink/Internal]` are not exactly a
    // sink or a source, and are skipped.
    assert_eq!(
        devices.inputs,
        vec![
            device(61, "Built-in Audio Controller Analog Stereo", true),
            device(91, "Easy Effects Source", false),
        ]
    );
    let rows = decoded(&devices);
    let checked: Vec<u32> = children(&rows, ROW_OUTPUT)
        .iter()
        .filter(|c| c.2 == DEFAULT)
        .map(|c| c.0)
        .collect();
    assert_eq!(checked, vec![(KIND_OUTPUT << 16) | 90]);
}

#[test]
fn a_filter_label_is_cut_at_its_last_bracket() {
    // A description with a bracket of its own keeps it.
    let label = format!("{:<60}", "Loopback [left]");
    let text = format!("Audio\n ├─ Filters:\n │      95. {label} [Audio/Source]\n");
    assert_eq!(
        parse_status(&text).inputs,
        vec![device(95, "Loopback [left]", false)]
    );
    // No bracket at all is not a filter entry.
    let text = "Audio\n ├─ Filters:\n │      96. Loopback\n";
    assert_eq!(parse_status(text), Devices::default());
}

// -- the rows ---------------------------------------------------------------

#[test]
fn one_device_is_still_a_submenu_with_its_one_row() {
    let rows = decoded(&parse_status(RECORDED));

    for (submenu, label, icon) in [
        (ROW_OUTPUT, LABEL_OUTPUT, ICON_OUTPUT),
        (ROW_INPUT, LABEL_INPUT, ICON_INPUT),
    ] {
        let i = index_of(&rows, submenu);
        assert_eq!(rows.rows[i].role, ROW_MENU);
        assert_eq!(rows.rows[i].parent, 0, "the submenu hangs off the chip");
        assert_eq!(rows.rows[i].flags, RFLAG_SUBMENU);
        assert_eq!(rows.label(i).as_deref(), Some(label));
        assert_eq!(rows.icon(i), Some(icon));
    }
    let builtin = "Built-in Audio Controller Analog Stereo".to_owned();
    assert_eq!(
        children(&rows, ROW_OUTPUT),
        vec![((1 << 16) | 60, builtin.clone(), DEFAULT)]
    );
    assert_eq!(
        children(&rows, ROW_INPUT),
        vec![((2 << 16) | 61, builtin, DEFAULT)]
    );
    // The device rows carry no icon: the radio mark is their state.
    assert_eq!(rows.icon(index_of(&rows, (1 << 16) | 60)), None);
    // Output and Input come after the volume rows and before the settings row.
    assert!(index_of(&rows, ROW_OUTPUT) < index_of(&rows, ROW_INPUT));
    assert!(index_of(&rows, ROW_INPUT) < index_of(&rows, ROW_SETTINGS));
}

#[test]
fn several_devices_are_radio_rows_with_the_default_checked() {
    let rows = decoded(&parse_status(SEVERAL));
    let outputs = children(&rows, ROW_OUTPUT);
    assert_eq!(
        outputs
            .iter()
            .map(|(id, _, flags)| (*id, *flags))
            .collect::<Vec<_>>(),
        vec![
            ((1 << 16) | 57, RADIO),
            ((1 << 16) | 60, DEFAULT),
            ((1 << 16) | 95, RADIO)
        ]
    );
    assert_eq!(outputs[2].1, "Headphones");
    let inputs = children(&rows, ROW_INPUT);
    assert_eq!(
        inputs
            .iter()
            .map(|(id, _, flags)| (*id, *flags))
            .collect::<Vec<_>>(),
        vec![((2 << 16) | 61, DEFAULT), ((2 << 16) | 96, RADIO)]
    );
}

#[test]
fn a_default_change_keeps_every_id_and_moves_the_check() {
    let before = decoded(&parse_status(SEVERAL));
    let after = decoded(&parse_status(MOVED));
    // The caustic holds an open menu by id (docs/abi.md, *Rows*): a default
    // moving under the open Output submenu must not change one.
    let ids = |rows: &Rows| {
        rows.rows
            .iter()
            .map(|r| (r.id, r.parent, r.role))
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&before), ids(&after));
    let checked = |rows: &Rows| {
        rows.rows
            .iter()
            .filter(|r| r.flags & RFLAG_RADIO != 0 && r.flags & RFLAG_CHECKED != 0)
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(checked(&before), vec![(1 << 16) | 60, (2 << 16) | 61]);
    assert_eq!(checked(&after), vec![(1 << 16) | 95, (2 << 16) | 96]);
}

#[test]
fn an_empty_section_carries_one_disabled_row() {
    let rows = decoded(&parse_status(NO_INPUT));
    assert_eq!(children(&rows, ROW_OUTPUT).len(), 1);
    assert_eq!(
        children(&rows, ROW_INPUT),
        vec![(ROW_NO_INPUT, LABEL_NO_DEVICE.to_owned(), RFLAG_DISABLED)]
    );

    // And both, when `wpctl status` gave nothing at all: the volume rows stay.
    let rows = decoded(&Devices::default());
    assert_eq!(
        children(&rows, ROW_OUTPUT),
        vec![(ROW_NO_OUTPUT, LABEL_NO_DEVICE.to_owned(), RFLAG_DISABLED)]
    );
    assert_eq!(
        children(&rows, ROW_INPUT),
        vec![(ROW_NO_INPUT, LABEL_NO_DEVICE.to_owned(), RFLAG_DISABLED)]
    );
}

#[test]
fn a_node_id_that_does_not_fit_is_not_listed() {
    assert_eq!(device_row(KIND_OUTPUT, 60), Some(0x1_003C));
    assert_eq!(device_row(KIND_INPUT, DEVICE_ID_MASK), Some(0x2_FFFF));
    assert_eq!(device_row(KIND_OUTPUT, DEVICE_ID_MASK + 1), None);
    // Its only device does not fit, so the submenu says "No device" rather
    // than aliasing it onto another id.
    let devices = Devices {
        outputs: vec![device(70_000, "Far", true)],
        inputs: vec![],
    };
    let rows = decoded(&devices);
    assert_eq!(children(&rows, ROW_OUTPUT)[0].0, ROW_NO_OUTPUT);
}

#[test]
fn the_cap_keeps_a_pathological_graph_inside_the_frame() {
    let mut text = String::from("Audio\n ├─ Sinks:\n");
    for id in 0..500 {
        text.push_str(&format!(" │      {id}. Sink {id} [vol: 1.00]\n"));
    }
    let devices = parse_status(&text);
    assert_eq!(devices.outputs.len(), caustics_audio::wpctl::MAX_DEVICES);
    // …and the frame still passes the reader.
    let rows = decoded(&devices);
    assert_eq!(
        children(&rows, ROW_OUTPUT).len(),
        caustics_audio::wpctl::MAX_DEVICES
    );
}

// -- the click ----------------------------------------------------------------

fn wpctl(words: &[&str]) -> Call {
    Call::Wpctl(words.iter().map(|w| (*w).to_owned()).collect())
}

#[test]
fn a_click_on_a_device_is_set_default_with_its_node_id() {
    assert_eq!(
        op_for((1 << 16) | 95, ACTION_ACTIVATE, 0.0),
        Some(Op::SetOutput(95))
    );
    assert_eq!(
        op_for((2 << 16) | 96, ACTION_ACTIVATE, 0.0),
        Some(Op::SetInput(96))
    );
    // The argv, recorded rather than run.
    assert_eq!(Op::SetOutput(95).call(), wpctl(&["set-default", "95"]));
    assert_eq!(Op::SetInput(96).call(), wpctl(&["set-default", "96"]));
    assert_eq!(
        (Op::SetOutput(1).name(), Op::SetInput(1).name()),
        ("set-output", "set-input")
    );
    // Round trip through the row id the frame carries.
    for devices in [parse_status(SEVERAL), parse_status(MOVED)] {
        let rows = decoded(&devices);
        for row in &rows.rows {
            if let Some((kind, node)) = device_of(row.id) {
                let op = op_for(row.id, ACTION_ACTIVATE, 0.0).expect("a device row performs");
                let want = if kind == KIND_OUTPUT {
                    Op::SetOutput(node)
                } else {
                    Op::SetInput(node)
                };
                assert_eq!(op, want);
                assert_eq!(op.call(), wpctl(&["set-default", &node.to_string()]));
            }
        }
    }
}

#[test]
fn the_submenus_and_the_no_device_rows_perform_nothing() {
    for row in [ROW_OUTPUT, ROW_INPUT, ROW_NO_OUTPUT, ROW_NO_INPUT] {
        assert_eq!(op_for(row, ACTION_ACTIVATE, 0.0), None, "row {row}");
    }
    // Only a plain activate switches: a drag, a right or middle click do not.
    for kind in [ACTION_SET_VALUE, ACTION_SECONDARY, ACTION_MIDDLE] {
        assert_eq!(op_for((1 << 16) | 60, kind, 50.0), None, "kind {kind}");
    }
    // A high half that is neither kind is not a device.
    assert_eq!(op_for((3 << 16) | 60, ACTION_ACTIVATE, 0.0), None);
    assert_eq!(device_of(60), None);
    assert_eq!(device_of((KIND_INPUT << 16) | 7), Some((KIND_INPUT, 7)));
}

#[test]
fn a_device_click_is_checked_against_the_list_it_came_from() {
    // `set-default` moves whichever default the node's own class says,
    // so a recycled id clicked on a stale Output row could move the
    // microphone. Only an id the current list has goes through.
    let devices = parse_status(SEVERAL);
    assert_eq!(
        admit(Op::SetOutput(95), Some(&devices)),
        Ok(Op::SetOutput(95))
    );
    assert_eq!(
        admit(Op::SetInput(96), Some(&devices)),
        Ok(Op::SetInput(96))
    );
    // 96 is a source: an Output row naming it is refused, and witnessed.
    let refused = admit(Op::SetOutput(96), Some(&devices)).expect_err("96 is not an output");
    assert_eq!(
        refused,
        "set-output: node 96 is not in the Output list now; nothing done"
    );
    assert!(
        admit(Op::SetInput(95), Some(&devices)).is_err(),
        "95 is not an input"
    );
    // Gone altogether, or no lists at all (no sink, no wpctl).
    assert!(admit(Op::SetOutput(4242), Some(&devices)).is_err());
    assert!(admit(Op::SetOutput(95), None).is_err());
    // Every other op passes through, lists or none.
    for op in [
        Op::SetVolume(40),
        Op::MuteSink,
        Op::MuteSource,
        Op::Settings,
    ] {
        assert_eq!(admit(op, None), Ok(op));
    }
}
