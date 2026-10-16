//! The one test that needs a machine: spawn the built binary against the real
//! system bus and read its first frame back with the ABI's own reader.
//!
//! `#[ignore]` because it needs UPower and logind on the system bus — a bare
//! container has neither, and the frame is the same shape there minus the
//! battery row. It **reads** and nothing else: no action is ever written to
//! the child's stdin, so nothing locks, suspends, reboots, shuts down or
//! quits.
//!
//! ```text
//! cargo test -p caustics-power --test live -- --ignored --nocapture
//! ```

use std::process::{Command, Stdio};

use caustics_bar_abi::buffer::APPLET_POWER;
use caustics_bar_abi::wire::{self, Frame, RFLAG_HAS_MENU, ROW_ITEM, ROW_MENU};

#[test]
#[ignore = "needs UPower and logind on the system bus"]
fn the_binary_publishes_a_button_with_five_rows_and_a_battery() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_caustics-power"))
        // The bar spawns a sidecar with no arguments (docs/abi.md, *Lifecycle*).
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Inherited, so the witnesses land in this test's own output.
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawning caustics-power");

    let mut out = child.stdout.take().expect("piped stdout");
    // The battery arrives after the bus round trip, so the first frame may be
    // the batteryless one; read until the battery row shows up, or run out.
    let mut rows = None;
    for _ in 0..3 {
        let next = match wire::read(&mut out) {
            Ok(Some(Frame::Rows(next))) => next,
            other => panic!("expected a rows frame, got {other:?}"),
        };
        let items = next.rows.iter().filter(|row| row.role == ROW_ITEM).count();
        rows = Some(next);
        if items == 2 {
            break;
        }
    }
    let rows = rows.expect("at least one frame");

    // The child's stdin closing is its notice to leave (docs/abi.md, *Lifecycle*).
    drop(child.stdin.take());
    let status = child.wait().expect("waiting for caustics-power");

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

    assert_eq!(rows.applet, APPLET_POWER);
    let button = &rows.rows[0];
    assert_eq!(button.role, ROW_ITEM);
    assert_eq!(
        button.flags & RFLAG_HAS_MENU,
        RFLAG_HAS_MENU,
        "the button opens its menu"
    );
    assert_eq!(
        rows.rows
            .iter()
            .filter(|row| row.role == ROW_MENU && row.parent == 0)
            .count(),
        5,
        "the power button's five rows: Lock, Log Out, Suspend, Restart, Shut Down"
    );
    let items: Vec<u32> = rows
        .rows
        .iter()
        .filter(|row| row.role == ROW_ITEM)
        .map(|r| r.id)
        .collect();
    assert_eq!(
        items.len(),
        2,
        "the power button and the battery: {items:?}"
    );
    let battery = rows.rows.iter().position(|row| row.id == items[1]).unwrap();
    assert!(
        rows.label(battery).is_some_and(|l| l.ends_with('%')),
        "the battery's label is a percentage"
    );
    assert!(status.success(), "a sidecar exits 0 when its stdin closes");
}
