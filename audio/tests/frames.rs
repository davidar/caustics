//! The one frame `caustics-audio` publishes, built from recorded `wpctl` strings
//! and read back through the ABI's own reader — no PipeWire, no bar.
//!
//! **The fixtures.** `audio/tests/data/wpctl-sink.txt`,
//! `wpctl-source.txt` and `wpctl-no-node.txt` are *recorded* from a session
//! running PipeWire 1.6 (`wpctl get-volume @DEFAULT_AUDIO_SINK@`, the same for
//! the source, and `wpctl get-volume 999999`), and `pactl-subscribe.txt` is a
//! recorded five minutes of `pactl subscribe`. `wpctl-muted-derived.txt` and
//! `wpctl-overamplified-derived.txt` are **derived** — hand-built from the
//! recorded form, because recording them would have meant running `set-mute`
//! and `set-volume` against the session this test runs in.

use caustics_audio::rows::{
    ICON_HIGH, ICON_LOW, ICON_MEDIUM, ICON_MUTED, ICON_OVERAMPLIFIED, LABEL_MIC, LABEL_MUTE,
    LABEL_SETTINGS, LABEL_VOLUME, ROW_AUDIO, ROW_INPUT, ROW_MIC, ROW_MUTE, ROW_OUTPUT, ROW_SEP,
    ROW_SEP_DEVICES, ROW_SETTINGS, ROW_VOLUME, VOLUME_MAX, VOLUME_MIN, icon_for, picture,
};
use caustics_audio::subscribe::is_wakeup;
use caustics_audio::wpctl::{Audio, Devices, parse_status, parse_volume};
use caustics_bar_abi::buffer::{APPLET_AUDIO, NO_INDEX};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_HAS_MENU, ROW_ITEM, ROW_MENU, ROW_SEPARATOR,
    ROW_SLIDER, Rows,
};

const SINK: &str = include_str!("data/wpctl-sink.txt");
const SOURCE: &str = include_str!("data/wpctl-source.txt");
const MUTED: &str = include_str!("data/wpctl-muted-derived.txt");
const OVERAMPLIFIED: &str = include_str!("data/wpctl-overamplified-derived.txt");
const NO_NODE: &str = include_str!("data/wpctl-no-node.txt");
const SUBSCRIBED: &str = include_str!("data/pactl-subscribe.txt");
/// Recorded from a PipeWire session and scrubbed (the client list cut to three,
/// `user@host`, pids and the cookie replaced, the hardware given generic
/// names): one sink, one source.
const STATUS: &str = include_str!("data/wpctl-status.txt");

/// Round-trip through `wire::write`/`wire::read`, so every assertion below is
/// about what the *bar* would decode rather than about `RowsBuilder`'s memory.
fn decoded(revision: u32, audio: Option<&Audio>) -> Rows {
    decoded_with(revision, audio, &parse_status(STATUS))
}

/// The same, with the device lists given.
fn decoded_with(revision: u32, audio: Option<&Audio>, devices: &Devices) -> Rows {
    decoded_full(revision, audio, devices, true)
}

/// The same, saying whether a settings program can be named.
fn decoded_full(revision: u32, audio: Option<&Audio>, devices: &Devices, settings: bool) -> Rows {
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &picture(revision, audio, devices, settings))
        .expect("writing to a Vec");
    match wire::read(&mut bytes.as_slice()) {
        Ok(Some(Frame::Rows(rows))) => rows,
        other => panic!("expected a rows frame, got {other:?}"),
    }
}

/// The two recorded readings, as the sidecar would put them together.
fn recorded() -> Audio {
    let (volume, muted) = parse_volume(SINK).expect("the recorded sink line parses");
    let (_, mic_muted) = parse_volume(SOURCE).expect("the recorded source line parses");
    Audio {
        volume,
        muted,
        mic_muted,
    }
}

fn at(percent: f32) -> Audio {
    Audio {
        volume: percent / 100.0,
        muted: false,
        mic_muted: false,
    }
}

fn index_of(rows: &Rows, id: u32) -> usize {
    rows.rows
        .iter()
        .position(|row| row.id == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

// -- the recorded strings ---------------------------------------------------

#[test]
fn the_recorded_readings_parse() {
    // As recorded: the sink at 56%, the microphone at 27%, neither muted.
    assert_eq!(parse_volume(SINK), Some((0.56, false)));
    assert_eq!(parse_volume(SOURCE), Some((0.27, false)));
    // Derived from that form, by hand: `[MUTED]` is what `wpctl` appends.
    assert_eq!(parse_volume(MUTED), Some((0.56, true)));
    // Also derived: PipeWire allows above 1.0, and the icon set has a name for
    // it.
    assert_eq!(parse_volume(OVERAMPLIFIED), Some((1.32, false)));
}

#[test]
fn a_reading_that_is_not_a_volume_is_no_reading() {
    // Recorded: `wpctl get-volume 999999` prints this and **exits 0**, so the
    // text is the only signal there is.
    assert_eq!(parse_volume(NO_NODE), None);
    for text in [
        "",
        "\n",
        "Volume:",
        "Volume: what",
        "Volume: -0.5",
        "Volume: NaN",
        "volume: 1",
    ] {
        assert_eq!(parse_volume(text), None, "{text:?} parsed as a volume");
    }
    // Leading noise is skipped rather than fatal: the volume line is found.
    assert_eq!(
        parse_volume("Node warming up\nVolume: 1.00 [MUTED]\n"),
        Some((1.0, true))
    );
}

#[test]
fn only_sink_source_and_server_events_wake_the_read() {
    // Recorded: five minutes of `pactl subscribe` during normal use was 15
    // events — 14 `client` pairs (every `wpctl` call this sidecar makes is one
    // of them, which is why they must not wake it) and one `sink-input`.
    let woken: Vec<&str> = SUBSCRIBED.lines().filter(|line| is_wakeup(line)).collect();
    assert!(
        woken.is_empty(),
        "the recorded five minutes woke nothing: {woken:?}"
    );
    assert!(
        SUBSCRIBED
            .lines()
            .filter(|l| l.contains("on client #"))
            .count()
            >= 14
    );

    // Derived, in the form `pactl subscribe` prints.
    assert!(is_wakeup("Event 'change' on sink #52"));
    assert!(is_wakeup("Event 'change' on source #73"));
    assert!(is_wakeup("Event 'change' on server"));
    assert!(is_wakeup("Event 'new' on sink #52"));
    // A stream on a sink is not the sink: it cannot move a volume or a mute.
    assert!(!is_wakeup("Event 'change' on sink-input #2232"));
    assert!(!is_wakeup("Event 'remove' on source-output #99"));
    // What the child prints on its way out, and anything else.
    assert!(!is_wakeup("Got SIGINT, exiting."));
    assert!(!is_wakeup(""));
}

// -- the picture ------------------------------------------------------------

#[test]
fn the_recorded_reading_is_the_whole_menu() {
    let rows = decoded(7, Some(&recorded()));

    assert_eq!(rows.applet, APPLET_AUDIO);
    assert_eq!(rows.revision, 7);
    // The chip, the slider, two mutes, a separator, Output and its one sink,
    // Input and its one source, a separator, the settings row.
    assert_eq!(rows.rows.len(), 11);

    let chip = &rows.rows[0];
    assert_eq!(chip.id, ROW_AUDIO);
    assert_eq!(chip.role, ROW_ITEM);
    assert_eq!(chip.parent, NO_INDEX);
    assert_eq!(chip.flags, RFLAG_HAS_MENU);
    assert_eq!(rows.label(0), None, "the chip is an icon chip");
    // 56% is the medium bucket.
    assert_eq!(rows.icon(0), Some(ICON_MEDIUM));
    assert!(
        (chip.value - 0.56).abs() < 1e-6,
        "the level is the fraction: {}",
        chip.value
    );

    let ids: Vec<u32> = rows.rows[1..].iter().map(|row| row.id).collect();
    assert_eq!(
        ids,
        vec![
            ROW_VOLUME,
            ROW_MUTE,
            ROW_MIC,
            ROW_SEP,
            ROW_OUTPUT,
            (1 << 16) | 60,
            ROW_INPUT,
            (2 << 16) | 61,
            ROW_SEP_DEVICES,
            ROW_SETTINGS,
        ]
    );
    for i in 1..rows.rows.len() {
        let parent = match rows.rows[i].id >> 16 {
            1 => index_of(&rows, ROW_OUTPUT) as u32,
            2 => index_of(&rows, ROW_INPUT) as u32,
            _ => 0,
        };
        assert_eq!(
            rows.rows[i].parent, parent,
            "row {}'s parent",
            rows.rows[i].id
        );
    }

    let slider = &rows.rows[1];
    assert_eq!(slider.role, ROW_SLIDER);
    assert_eq!(rows.label(1).as_deref(), Some(LABEL_VOLUME));
    assert_eq!((slider.min, slider.max), (VOLUME_MIN, VOLUME_MAX));
    assert!(
        (slider.value - 56.0).abs() < 1e-4,
        "percent, not a fraction: {}",
        slider.value
    );

    // Neither mute is on in the recorded pair, so both are checkable and
    // unchecked.
    let mute = &rows.rows[2];
    assert_eq!(mute.role, ROW_MENU);
    assert_eq!(rows.label(2).as_deref(), Some(LABEL_MUTE));
    assert_eq!(mute.flags, RFLAG_CHECKABLE);
    let mic = &rows.rows[3];
    assert_eq!(rows.label(3).as_deref(), Some(LABEL_MIC));
    assert_eq!(mic.flags, RFLAG_CHECKABLE);

    assert_eq!(rows.rows[4].role, ROW_SEPARATOR);
    assert_eq!(rows.label(4), None);
    let settings = index_of(&rows, ROW_SETTINGS);
    assert_eq!(settings, rows.rows.len() - 1, "the settings row is last");
    assert_eq!(rows.rows[settings].role, ROW_MENU);
    assert_eq!(rows.label(settings).as_deref(), Some(LABEL_SETTINGS));
    assert_eq!(
        rows.rows[settings].flags, 0,
        "the settings row performs on a plain activate"
    );
}

/// With no settings program to name, the settings row and the separator above
/// it are not written, and the menu ends at the Input submenu.
#[test]
fn no_settings_program_is_no_settings_row() {
    let rows = decoded_full(1, Some(&recorded()), &parse_status(STATUS), false);
    let ids: Vec<u32> = rows.rows.iter().map(|row| row.id).collect();
    assert!(!ids.contains(&ROW_SETTINGS), "{ids:?}");
    assert!(!ids.contains(&ROW_SEP_DEVICES), "{ids:?}");
    let last_top_level = rows
        .rows
        .iter()
        .rposition(|row| row.parent == 0)
        .expect("menu rows");
    assert_eq!(rows.rows[last_top_level].id, ROW_INPUT);
}

#[test]
fn a_mute_checks_its_own_row_and_nothing_else() {
    let (volume, muted) = parse_volume(MUTED).expect("the derived muted line parses");
    let rows = decoded(
        2,
        Some(&Audio {
            volume,
            muted,
            mic_muted: true,
        }),
    );

    assert_eq!(rows.icon(0), Some(ICON_MUTED), "muted beats the percentage");
    // A muted sink still knows where its slider is: the volume is not lost.
    assert!((rows.rows[index_of(&rows, ROW_VOLUME)].value - 56.0).abs() < 1e-4);
    assert_eq!(
        rows.rows[index_of(&rows, ROW_MUTE)].flags,
        RFLAG_CHECKABLE | RFLAG_CHECKED
    );
    assert_eq!(
        rows.rows[index_of(&rows, ROW_MIC)].flags,
        RFLAG_CHECKABLE | RFLAG_CHECKED
    );

    // One muted and one not, the other way round.
    let half = decoded(
        3,
        Some(&Audio {
            volume,
            muted: false,
            mic_muted: true,
        }),
    );
    assert_eq!(half.rows[index_of(&half, ROW_MUTE)].flags, RFLAG_CHECKABLE);
    assert_eq!(
        half.rows[index_of(&half, ROW_MIC)].flags,
        RFLAG_CHECKABLE | RFLAG_CHECKED
    );
    assert_eq!(
        half.icon(0),
        Some(ICON_MEDIUM),
        "the mic does not mute the chip"
    );
}

#[test]
fn every_bucket_has_its_icon() {
    // The thresholds, in the percent `icon_for` takes: under 34, under 67, up
    // to 100, above it.
    let thresholds = [
        (0.0, ICON_LOW),
        (33.9, ICON_LOW),
        (34.0, ICON_MEDIUM),
        (66.9, ICON_MEDIUM),
        (67.0, ICON_HIGH),
        (100.0, ICON_HIGH),
        (100.1, ICON_OVERAMPLIFIED),
        (153.0, ICON_OVERAMPLIFIED),
    ];
    for (percent, icon) in thresholds {
        assert_eq!(icon_for(percent, false), icon, "{percent}%");
        assert_eq!(icon_for(percent, true), ICON_MUTED, "{percent}% muted");
    }
    // …and through the wire, at volumes far enough from a threshold that
    // `percent / 100.0` cannot round across one.
    for (percent, icon) in [
        (8.0, ICON_LOW),
        (50.0, ICON_MEDIUM),
        (80.0, ICON_HIGH),
        (130.0, ICON_OVERAMPLIFIED),
    ] {
        let rows = decoded(1, Some(&at(percent)));
        assert_eq!(rows.icon(0), Some(icon), "{percent}% through the wire");
    }
}

#[test]
fn an_overamplified_sink_is_a_full_slider_and_a_full_level() {
    let (volume, muted) = parse_volume(OVERAMPLIFIED).expect("the derived line parses");
    let rows = decoded(
        4,
        Some(&Audio {
            volume,
            muted,
            mic_muted: false,
        }),
    );

    assert_eq!(rows.icon(0), Some(ICON_OVERAMPLIFIED));
    // The level on the icon is a fraction, so 132% draws a full bar, not an
    // overflowing one…
    assert!(
        (rows.rows[0].value - 1.0).abs() < 1e-6,
        "level {}",
        rows.rows[0].value
    );
    // …and the slider sits at the top of its own range, which is also the most
    // a drag may ask for.
    let slider = &rows.rows[index_of(&rows, ROW_VOLUME)];
    assert!(
        (slider.value - VOLUME_MAX).abs() < 1e-6,
        "slider {}",
        slider.value
    );
    assert!(slider.min < slider.max, "the reader rejects min >= max");
}

#[test]
fn no_sink_is_no_rows_at_all() {
    // `wpctl` missing and "no default sink" are the same `None` here, and the
    // same frame: the chip disappears and the bar's health line still says
    // `running`.
    let rows = decoded(9, None);
    assert_eq!(rows.applet, APPLET_AUDIO);
    assert_eq!(rows.revision, 9);
    assert!(rows.rows.is_empty(), "{:?}", rows.rows);
}

#[test]
fn ids_roles_and_parents_are_stable_across_frames() {
    let a = decoded(1, Some(&at(56.0)));
    let b = decoded(2, Some(&at(57.0)));

    let shape = |rows: &Rows| -> Vec<(u32, u32, u32, u32)> {
        rows.rows
            .iter()
            .map(|r| (r.id, r.parent, r.role, r.flags))
            .collect()
    };
    // The bar's rebuild-vs-upload distinction depends on this: a volume moving
    // is a value moving and nothing else, or the menu closes under the drag
    // that is changing it (docs/abi.md, *Rows*).
    assert_eq!(shape(&a), shape(&b), "only the volume moved");
    let labels = |rows: &Rows| -> Vec<Option<String>> {
        (0..rows.rows.len())
            .map(|i| rows.label(i).map(|l| l.into_owned()))
            .collect()
    };
    assert_eq!(labels(&a), labels(&b), "no label carries the number either");

    // …and the volume did move, in the slider and in the level.
    let i = index_of(&b, ROW_VOLUME);
    assert!((b.rows[i].value - 57.0).abs() < 1e-4);
    assert!((b.rows[0].value - 0.57).abs() < 1e-6);

    // Crossing a bucket changes the chip's icon, which *is* a rebuild — and
    // the ids, roles and parents still do not move.
    let loud = decoded(3, Some(&at(90.0)));
    assert_eq!(shape(&loud), shape(&a));
    assert_eq!(loud.icon(0), Some(ICON_HIGH));

    // Losing the sink empties the frame rather than reshuffling it.
    let gone = decoded(4, None);
    assert!(gone.rows.is_empty());
}
