//! The on-screen display's rule and the bits it puts in the frame: which
//! change raises a toast and with which rows, what the own-write hold and an
//! open menu suppress, when the burst ends, and that the frame carries
//! `RFLAG_SHOW` and `RFLAG_ATTENTION` exactly while it is up. The clock is a
//! parameter, so nothing here sleeps, and no sound server is involved.

use std::time::{Duration, Instant};

use caustics_audio::osd::{Decision, Flagged, Osd, decide};
use caustics_audio::rows::{ROW_AUDIO, ROW_MIC, ROW_MUTE, ROW_VOLUME, toasting};
use caustics_audio::wpctl::{Audio, Device, Devices, Sound};
use caustics_audio::{OSD_HOLD, OSD_OWN_HOLD};
use caustics_bar_abi::wire::{self, Frame, RFLAG_ATTENTION, RFLAG_SHOW, Rows};

/// One sink and one source, both default.
fn devices(sink: u32, source: u32) -> Devices {
    let device = |id| Device {
        id,
        label: format!("node {id}"),
        default: true,
    };
    Devices {
        outputs: vec![device(sink)],
        inputs: vec![device(source)],
    }
}

fn on(volume: f32, muted: bool, mic_muted: bool) -> Sound {
    on_at(volume, muted, mic_muted, 50, 60)
}

fn on_at(volume: f32, muted: bool, mic_muted: bool, sink: u32, source: u32) -> Sound {
    Sound::On(
        Audio {
            volume,
            muted,
            mic_muted,
        },
        devices(sink, source),
    )
}

const VOLUME: Flagged = Flagged {
    volume: true,
    mute: false,
    mic: false,
};
const MUTE: Flagged = Flagged {
    volume: true,
    mute: true,
    mic: false,
};
const MIC: Flagged = Flagged {
    volume: false,
    mute: false,
    mic: true,
};

#[test]
fn the_first_reading_is_never_a_toast() {
    let now = Instant::now();
    assert_eq!(
        decide(None, &on(0.4, false, false), None, now),
        Decision::Nothing
    );
}

#[test]
fn a_sink_appearing_is_not_a_toast() {
    let now = Instant::now();
    let after = on(0.4, false, false);
    assert_eq!(
        decide(Some(&Sound::NoSink), &after, None, now),
        Decision::Nothing
    );
    let absent = Sound::Absent("no wpctl".to_owned());
    assert_eq!(decide(Some(&absent), &after, None, now), Decision::Nothing);
    // And leaving is not one either: there is nothing to show.
    assert_eq!(
        decide(Some(&after), &Sound::NoSink, None, now),
        Decision::Nothing
    );
}

#[test]
fn a_volume_change_flags_the_slider() {
    let now = Instant::now();
    let before = on(0.4, false, false);
    let after = on(0.45, false, false);
    assert_eq!(
        decide(Some(&before), &after, None, now),
        Decision::Show(VOLUME)
    );
}

#[test]
fn the_sinks_mute_flags_the_slider_and_its_checkbox() {
    let now = Instant::now();
    let before = on(0.4, false, false);
    let after = on(0.4, true, false);
    assert_eq!(
        decide(Some(&before), &after, None, now),
        Decision::Show(MUTE)
    );
}

#[test]
fn the_microphones_mute_flags_its_checkbox_alone() {
    let now = Instant::now();
    let before = on(0.4, false, false);
    let after = on(0.4, false, true);
    assert_eq!(
        decide(Some(&before), &after, None, now),
        Decision::Show(MIC)
    );
}

#[test]
fn a_default_device_moving_is_not_a_toast() {
    let now = Instant::now();
    // A new default sink brings its own volume and mute; a new default
    // source its own mute. Neither is a change anyone made to a level.
    let before = on_at(0.4, false, false, 50, 60);
    let sink_moved = on_at(0.7, true, false, 51, 60);
    assert_eq!(
        decide(Some(&before), &sink_moved, None, now),
        Decision::Nothing
    );
    let source_moved = on_at(0.4, false, true, 50, 61);
    assert_eq!(
        decide(Some(&before), &source_moved, None, now),
        Decision::Nothing
    );
}

#[test]
fn a_device_list_change_alone_is_not_a_toast() {
    let now = Instant::now();
    let before = on(0.4, false, false);
    let Sound::On(audio, mut more) = before.clone() else {
        unreachable!()
    };
    more.outputs.push(Device {
        id: 70,
        label: "headset".to_owned(),
        default: false,
    });
    let after = Sound::On(audio, more);
    assert_eq!(decide(Some(&before), &after, None, now), Decision::Nothing);
}

#[test]
fn an_own_write_within_the_hold_is_not_shown() {
    let now = Instant::now();
    let before = on(0.4, false, false);
    let after = on(0.45, false, false);
    let own = now - OSD_OWN_HOLD + Duration::from_millis(1);
    assert_eq!(decide(Some(&before), &after, Some(own), now), Decision::Own);
}

#[test]
fn an_own_write_outside_the_hold_is_old_news() {
    let now = Instant::now();
    let before = on(0.4, false, false);
    let after = on(0.45, false, false);
    let own = now - OSD_OWN_HOLD;
    assert_eq!(
        decide(Some(&before), &after, Some(own), now),
        Decision::Show(VOLUME)
    );
}

#[test]
fn a_burst_shows_the_union_and_extends_its_deadline() {
    let t0 = Instant::now();
    let mut osd = Osd::default();
    let a = on(0.4, false, false);
    let b = on(0.45, false, false);
    let c = on(0.45, false, true);
    let line = osd.change(Some(&a), &b, t0).expect("a witness");
    assert_eq!(line, "osd: volume 45% muted=no (hold 1500ms)");
    assert_eq!(osd.flagged(), VOLUME);
    assert_eq!(osd.deadline(), Some(t0 + OSD_HOLD));

    let t1 = t0 + Duration::from_millis(500);
    let line = osd.change(Some(&b), &c, t1).expect("a witness");
    assert_eq!(line, "osd: mic muted=yes (hold 1500ms)");
    assert_eq!(
        osd.flagged(),
        Flagged {
            volume: true,
            mute: false,
            mic: true,
        }
    );
    assert_eq!(osd.deadline(), Some(t1 + OSD_HOLD));

    // Not before its time, and then once.
    assert!(!osd.expire(t0 + OSD_HOLD));
    assert!(osd.expire(t1 + OSD_HOLD));
    assert_eq!(osd.flagged(), Flagged::NONE);
    assert_eq!(osd.deadline(), None);
    assert!(!osd.expire(t1 + OSD_HOLD * 2));
}

#[test]
fn a_shown_toast_ends_the_burst_at_once_and_once() {
    let t0 = Instant::now();
    let mut osd = Osd::default();
    osd.change(Some(&on(0.4, false, false)), &on(0.4, true, false), t0);
    assert_eq!(osd.flagged(), MUTE);
    // One `ACTION_TOAST_SHOWN` per row the toast held: the first ends it, the
    // second finds nothing to end.
    assert!(osd.end());
    assert!(!osd.end());
    assert_eq!(osd.flagged(), Flagged::NONE);
    assert_eq!(osd.deadline(), None);
}

#[test]
fn own_changes_say_so_once_per_burst() {
    let t0 = Instant::now();
    let mut osd = Osd::default();
    osd.own_write(t0);
    let mut before = on(0.40, false, false);
    let mut lines = Vec::new();
    // A drag: a frame every 20 ms, each one its own write and its own change.
    for step in 1..=20u32 {
        let now = t0 + Duration::from_millis(20 * u64::from(step));
        osd.own_write(now);
        let after = on(0.40 + 0.01 * step as f32, false, false);
        lines.extend(osd.change(Some(&before), &after, now));
        before = after;
    }
    assert_eq!(lines, ["osd: own change, not shown"]);
    assert_eq!(osd.flagged(), Flagged::NONE);
    assert_eq!(osd.deadline(), None);

    // A second drag well after the first is a second burst, and said again.
    let later = t0 + Duration::from_secs(10);
    osd.own_write(later);
    let after = on(0.2, false, false);
    assert_eq!(
        osd.change(Some(&before), &after, later).as_deref(),
        Some("osd: own change, not shown")
    );
}

#[test]
fn a_change_under_the_open_menu_is_not_flagged() {
    let t0 = Instant::now();
    let mut osd = Osd::default();
    assert!(!osd.menu(true), "nothing to end");
    let line = osd.change(Some(&on(0.4, false, false)), &on(0.5, false, false), t0);
    assert_eq!(line.as_deref(), Some("osd: menu open, not shown"));
    assert_eq!(osd.flagged(), Flagged::NONE);
    osd.menu(false);
    let t1 = t0 + Duration::from_millis(100);
    assert!(
        osd.change(Some(&on(0.5, false, false)), &on(0.6, false, false), t1)
            .is_some()
    );
    assert_eq!(osd.flagged(), VOLUME);
    // The menu opening over a burst ends it.
    assert!(osd.menu(true));
    assert_eq!(osd.flagged(), Flagged::NONE);
}

/// Round-trip through the ABI's own writer and reader.
fn decoded(osd: Flagged) -> Rows {
    let audio = Audio {
        volume: 0.4,
        muted: false,
        mic_muted: false,
    };
    let mut bytes = Vec::new();
    wire::write(
        &mut bytes,
        &toasting(3, Some(&audio), &devices(50, 60), true, osd),
    )
    .expect("writing to a Vec");
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

#[test]
fn a_toasting_picture_carries_the_bits_and_a_plain_one_none() {
    let plain = decoded(Flagged::NONE);
    assert!(
        plain
            .rows
            .iter()
            .all(|row| row.flags & (RFLAG_SHOW | RFLAG_ATTENTION) == 0)
    );

    let all = Flagged {
        volume: true,
        mute: true,
        mic: true,
    };
    let toast = decoded(all);
    assert_ne!(flags(&toast, ROW_AUDIO) & RFLAG_SHOW, 0);
    for id in [ROW_VOLUME, ROW_MUTE, ROW_MIC] {
        assert_ne!(flags(&toast, id) & RFLAG_ATTENTION, 0, "row {id}");
    }
    // Nothing else is flagged, and every other bit is the plain picture's.
    for (a, b) in plain.rows.iter().zip(&toast.rows) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.flags, b.flags & !(RFLAG_SHOW | RFLAG_ATTENTION));
        if ![ROW_AUDIO, ROW_VOLUME, ROW_MUTE, ROW_MIC].contains(&b.id) {
            assert_eq!(a.flags, b.flags, "row {}", b.id);
        }
    }

    // The microphone alone: the chip asks, only its row is picked.
    let mic = decoded(MIC);
    assert_ne!(flags(&mic, ROW_AUDIO) & RFLAG_SHOW, 0);
    assert_ne!(flags(&mic, ROW_MIC) & RFLAG_ATTENTION, 0);
    assert_eq!(flags(&mic, ROW_VOLUME) & RFLAG_ATTENTION, 0);
    assert_eq!(flags(&mic, ROW_MUTE) & RFLAG_ATTENTION, 0);
}
