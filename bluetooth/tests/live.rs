//! The test that needs a machine, and the one that writes
//! `bluetooth/tests/data/`.
//!
//! The binary test spawns the built sidecar against the real system bus and
//! reads its frames back with the ABI's own reader. It needs `org.bluez` with
//! an adapter; without one the frame is the empty one and the witnesses are
//! the `absent:` pair. It **reads** and nothing else: no action is ever
//! written to the child's stdin, so nothing powers an adapter, connects a
//! device or disconnects one.
//!
//! The committed fixtures are **synthesised** from the invented data in
//! `synth/` by `synthesise_fixtures`, so nothing in the repository names a
//! real machine or device. A recording of a live bus (the crate's own
//! recorder in `src/bluez.rs`) is for diagnosis on the developer's machine and
//! is never committed: it carries the host name and the addresses and names
//! of paired devices.
//!
//! Both are `#[ignore]`:
//!
//! ```text
//! cargo test -p caustics-bluetooth --test live -- --ignored --nocapture
//! cargo test -p caustics-bluetooth --test live synthesise_fixtures -- --ignored
//! ```

use std::process::{Command, Stdio};

use caustics_bar_abi::buffer::APPLET_BLUETOOTH;
use caustics_bar_abi::wire::{
    self, Frame, RFLAG_CHECKABLE, RFLAG_HAS_MENU, ROW_ITEM, ROW_MENU, ROW_SEPARATOR,
};
use caustics_bluetooth::rows::{DEVICE_ID_BIT, POWER_LABEL, ROW_CHIP, ROW_POWER};

mod synth;

#[test]
#[ignore = "needs org.bluez with an adapter on the system bus"]
fn the_binary_publishes_a_chip_a_toggle_and_the_paired_devices() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-bluetooth"))
        // The bar spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*).
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Inherited, so the witnesses land in this test's own output.
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawning caustics-bluetooth");

    let mut out = child.stdout.take().expect("piped stdout");
    // The first frame is published before the bus round trip has answered, so it
    // is the empty one; read until a frame has rows, or run out.
    let mut rows = None;
    for _ in 0..3 {
        let next = match wire::read(&mut out) {
            Ok(Some(Frame::Rows(next))) => next,
            other => panic!("expected a rows frame, got {other:?}"),
        };
        let empty = next.rows.is_empty();
        rows = Some(next);
        if !empty {
            break;
        }
    }
    let rows = rows.expect("at least one frame");

    // The child's stdin closing is its notice to leave (docs/abi.md, *Lifecycle*).
    drop(child.stdin.take());
    let status = child.wait().expect("waiting for caustics-bluetooth");

    println!(
        "applet={} revision={} rows={}",
        rows.applet,
        rows.revision,
        rows.rows.len()
    );
    for (i, row) in rows.rows.iter().enumerate() {
        println!(
            "  [{i}] id={} parent={} role={} flags={} label={:?} icon={:?} value={}",
            row.id,
            row.parent,
            row.role,
            row.flags,
            rows.label(i).as_deref(),
            rows.icon(i),
            row.value
        );
    }
    println!("exit: {status}");

    assert_eq!(rows.applet, APPLET_BLUETOOTH);
    let chip = &rows.rows[0];
    assert_eq!(chip.id, ROW_CHIP);
    assert_eq!(chip.role, ROW_ITEM);
    assert_eq!(
        chip.flags & RFLAG_HAS_MENU,
        RFLAG_HAS_MENU,
        "the chip opens its menu"
    );
    assert_eq!(
        rows.rows.iter().filter(|row| row.role == ROW_ITEM).count(),
        1,
        "one chip"
    );

    // The adapter's own row: `Powered` is whatever the machine's radio is, so
    // what is asserted is that it is checkable and says so.
    let toggle = rows
        .rows
        .iter()
        .position(|row| row.id == ROW_POWER)
        .expect("the toggle row");
    assert_eq!(rows.rows[toggle].role, ROW_MENU);
    assert_eq!(rows.label(toggle).as_deref(), Some(POWER_LABEL));
    assert_eq!(rows.rows[toggle].flags & RFLAG_CHECKABLE, RFLAG_CHECKABLE);

    // At least one paired device (the test needs one), each with an icon the
    // bar can resolve.
    let devices: Vec<usize> = (0..rows.rows.len())
        .filter(|i| rows.rows[*i].role == ROW_MENU && rows.rows[*i].id >= DEVICE_ID_BIT)
        .collect();
    assert!(!devices.is_empty(), "a paired device should show");
    for i in devices {
        assert_eq!(rows.rows[i].flags & RFLAG_CHECKABLE, RFLAG_CHECKABLE);
        assert!(
            rows.label(i).is_some_and(|label| !label.is_empty()),
            "row {i} has a label"
        );
        assert!(rows.icon(i).is_some(), "row {i} has an icon");
    }

    let last = rows.rows.len() - 1;
    assert!(
        rows.rows[last].role == ROW_MENU && rows.rows[last].id >= DEVICE_ID_BIT,
        "a device is last"
    );
    assert_eq!(
        rows.rows
            .iter()
            .filter(|row| row.role == ROW_SEPARATOR)
            .count(),
        1,
        "one separator, above the devices"
    );
    assert!(status.success(), "a sidecar exits 0 when its stdin closes");
}

// ---------------------------------------------------------------------------
// The synthesiser
// ---------------------------------------------------------------------------

/// Rewrite `bluetooth/tests/data/` from `synth/`. Run it after editing the
/// invented data; `frames.rs` fails until the files and the module agree.
#[test]
#[ignore = "rewrites bluetooth/tests/data from synth/"]
fn synthesise_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    for (name, body) in synth::fixtures() {
        std::fs::write(format!("{dir}/{name}"), &body).expect("the fixture writes");
        println!("synthesised {name} ({} bytes)", body.len());
    }
}
