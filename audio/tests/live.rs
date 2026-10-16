//! The one test that needs a machine: spawn the built binary against the real
//! PipeWire session and read its frames back with the ABI's own reader.
//!
//! `#[ignore]` because it needs `pactl`, `wpctl` and a default sink — a bare
//! container has none of them, and the frame there is the empty one.
//!
//! It **reads** and nothing else: no action is ever written to the child's
//! stdin, so no volume moves, nothing is muted, and no settings window opens.
//! Then it closes stdin and asserts the child left with 0, which is the lifecycle docs/abi.md, *Lifecycle*, describes.
//!
//! ```text
//! cargo test -p caustics-audio --test live -- --ignored --nocapture
//! ```

use std::process::{Command, Stdio};

use caustics_bar_abi::buffer::{APPLET_AUDIO, NO_INDEX};
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_HAS_MENU, RFLAG_RADIO, RFLAG_SUBMENU,
    ROW_ITEM, ROW_MENU, ROW_SEPARATOR, ROW_SLIDER,
};

#[test]
#[ignore = "needs pactl, wpctl and a default sink"]
fn the_binary_publishes_a_chip_a_slider_two_mutes_and_the_devices() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-audio"))
        // The bar spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*).
        // A named settings program, so the settings row is drawn whatever is
        // installed; no click is sent, so it never runs.
        .env("CAUSTICS_SETTINGS", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Inherited, so the witnesses land in this test's own output.
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawning caustics-audio");

    let mut out = child.stdout.take().expect("piped stdout");
    // The first rows frame is the whole picture, since the three `wpctl` reads
    // happen before it, but a `pactl subscribe` that was already running can
    // land an empty one first; read a few frames and stop at the first with rows.
    let mut rows = None;
    for _ in 0..4 {
        let next = match wire::read(&mut out) {
            Ok(Some(Frame::Rows(next))) => next,
            // The greeting comes first (docs/abi.md, *Hello*); it is not a picture.
            Ok(Some(Frame::Hello(_))) => continue,
            other => panic!("expected a rows frame, got {other:?}"),
        };
        let full = !next.rows.is_empty();
        rows = Some(next);
        if full {
            break;
        }
    }
    let rows = rows.expect("at least one frame");

    // The child's stdin closing is its notice to leave (docs/abi.md, *Lifecycle*).
    drop(child.stdin.take());
    let status = child.wait().expect("waiting for caustics-audio");

    println!(
        "applet={} revision={} rows={}",
        rows.applet,
        rows.revision,
        rows.rows.len()
    );
    for (i, row) in rows.rows.iter().enumerate() {
        println!(
            "  [{i}] id={} parent={} role={} flags={} label={:?} icon={:?} value={} min={} max={}",
            row.id,
            row.parent,
            row.role,
            row.flags,
            rows.label(i).as_deref(),
            rows.icon(i),
            row.value,
            row.min,
            row.max
        );
    }
    println!("exit: {status}");

    assert_eq!(rows.applet, APPLET_AUDIO);
    let chip = &rows.rows[0];
    assert_eq!(chip.role, ROW_ITEM);
    assert_eq!(chip.parent, NO_INDEX);
    assert_eq!(
        chip.flags & RFLAG_HAS_MENU,
        RFLAG_HAS_MENU,
        "the chip opens its menu"
    );
    assert!(
        (0.0..=1.0).contains(&chip.value),
        "the level is a fraction: {}",
        chip.value
    );
    assert!(rows.icon(0).is_some(), "the chip is an icon chip");

    let slider = rows
        .rows
        .iter()
        .find(|row| row.role == ROW_SLIDER)
        .expect("the volume slider");
    assert!(
        (0.0..=150.0).contains(&slider.value),
        "the slider sits somewhere sane: {}",
        slider.value
    );
    assert_eq!((slider.min, slider.max), (0.0, 100.0));

    let mutes = rows
        .rows
        .iter()
        .filter(|row| {
            row.role == ROW_MENU && row.flags & RFLAG_CHECKABLE != 0 && row.flags & RFLAG_RADIO == 0
        })
        .count();
    assert_eq!(mutes, 2, "Mute and Mute microphone");
    assert_eq!(
        rows.rows
            .iter()
            .filter(|row| row.role == ROW_SEPARATOR)
            .count(),
        2
    );
    // The Output and Input submenus, each with at least one child — a
    // device, or the disabled "No device" row — and at most one default each.
    let submenus: Vec<usize> = (0..rows.rows.len())
        .filter(|&i| rows.rows[i].flags & RFLAG_SUBMENU != 0)
        .collect();
    assert_eq!(submenus.len(), 2, "Output and Input");
    for &i in &submenus {
        let children: Vec<_> = rows
            .rows
            .iter()
            .filter(|row| row.parent == i as u32)
            .collect();
        assert!(!children.is_empty(), "submenu {:?} is empty", rows.label(i));
        let checked = children
            .iter()
            .filter(|row| row.flags & RFLAG_CHECKED != 0)
            .count();
        assert!(
            checked <= 1,
            "submenu {:?} has {checked} defaults",
            rows.label(i)
        );
    }
    assert!(status.success(), "a sidecar exits 0 when its stdin closes");
}
