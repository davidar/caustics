use super::*;
use core::mem::offset_of;

// Every assertion below is a cell of docs/abi.md, *The canvas's buffer*'s
// table; the Slang half is checked against the same tables by the canvas's
// own layout test (`canvas/src/gpu/tests.rs`).

#[test]
fn canvas_params_layout() {
    assert_eq!(size_of::<CanvasParams>(), 32);
    assert_eq!(offset_of!(CanvasParams, width), 0);
    assert_eq!(offset_of!(CanvasParams, height), 4);
    assert_eq!(offset_of!(CanvasParams, scale), 8);
    assert_eq!(offset_of!(CanvasParams, frame), 12);
    assert_eq!(offset_of!(CanvasParams, now_lo), 16);
    assert_eq!(offset_of!(CanvasParams, now_hi), 20);
}

#[test]
fn canvas_facts_layout() {
    assert_eq!(size_of::<CanvasFacts>(), 80);
    assert_eq!(offset_of!(CanvasFacts, show), 0);
    assert_eq!(offset_of!(CanvasFacts, dim), 32);
    assert_eq!(offset_of!(CanvasFacts, from_slot), 64);
    assert_eq!(offset_of!(CanvasFacts, to_slot), 68);
    assert_eq!(offset_of!(CanvasFacts, shows), 72);
}

#[test]
fn canvas_state_layout() {
    assert_eq!(size_of::<Note>(), 48);
    assert_eq!(size_of::<CanvasState>(), 16 + 48 * MAX_CANVAS_WITNESSES);
    assert_eq!(size_of::<CanvasState>(), 1552);
    assert_eq!(offset_of!(CanvasState, wants_redraw), 0);
    assert_eq!(offset_of!(CanvasState, witness_frame), 4);
    assert_eq!(offset_of!(CanvasState, witness_count), 8);
    assert_eq!(offset_of!(CanvasState, witness_lost), 12);
    assert_eq!(offset_of!(CanvasState, witnesses), 16);
}

/// The ring is read against the frame it was stamped with, and never past
/// its own length (rule 1).
#[test]
fn the_canvas_ring_is_read_against_its_frame_and_its_count() {
    let mut state = CanvasState::ZERO;
    state.witness_frame = 7;
    state.witness_count = 3;
    assert_eq!(state.witnesses_of(7).len(), 3);
    assert!(state.witnesses_of(8).is_empty());
    state.witness_count = 10_000;
    assert_eq!(state.witnesses_of(7).len(), MAX_CANVAS_WITNESSES);
}
