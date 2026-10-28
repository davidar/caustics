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
    assert_eq!(offset_of!(CanvasParams, target), 24);
    assert_eq!(offset_of!(CanvasParams, pane), 28);
}

#[test]
fn canvas_facts_layout() {
    assert_eq!(size_of::<CanvasFacts>(), 176);
    assert_eq!(offset_of!(CanvasFacts, show), 0);
    assert_eq!(offset_of!(CanvasFacts, dim), 32);
    assert_eq!(offset_of!(CanvasFacts, from_slot), 64);
    assert_eq!(offset_of!(CanvasFacts, to_slot), 68);
    assert_eq!(offset_of!(CanvasFacts, shows), 72);
    assert_eq!(offset_of!(CanvasFacts, literal_count), 76);
    assert_eq!(offset_of!(CanvasFacts, prompt), 80);
    assert_eq!(offset_of!(CanvasFacts, pointer_pane), 144);
    assert_eq!(offset_of!(CanvasFacts, pointer_x), 148);
    assert_eq!(offset_of!(CanvasFacts, pointer_y), 152);
    assert_eq!(offset_of!(CanvasFacts, presses), 156);
    assert_eq!(offset_of!(CanvasFacts, lock_start_lo), 160);
    assert_eq!(offset_of!(CanvasFacts, lock_start_hi), 164);
    assert_eq!(offset_of!(CanvasFacts, _pad0), 168);
}

#[test]
fn canvas_prompt_layout() {
    assert_eq!(size_of::<CanvasPrompt>(), 64);
    assert_eq!(offset_of!(CanvasPrompt, id), 0);
    assert_eq!(offset_of!(CanvasPrompt, state), 4);
    assert_eq!(offset_of!(CanvasPrompt, echo), 8);
    assert_eq!(offset_of!(CanvasPrompt, typed), 12);
    assert_eq!(offset_of!(CanvasPrompt, attempts), 16);
    assert_eq!(offset_of!(CanvasPrompt, generation), 20);
    assert_eq!(offset_of!(CanvasPrompt, title_off), 24);
    assert_eq!(offset_of!(CanvasPrompt, line_off), 32);
    assert_eq!(offset_of!(CanvasPrompt, line_error), 40);
    assert_eq!(offset_of!(CanvasPrompt, ask_off), 44);
    assert_eq!(offset_of!(CanvasPrompt, echo_off), 52);
    assert_eq!(size_of::<CanvasCover>(), 16);
}

#[test]
fn canvas_state_layout() {
    assert_eq!(size_of::<Note>(), 48);
    assert_eq!(
        size_of::<CanvasState>(),
        48 + 16 * CANVAS_TEXT_BLOCKS + 48 * MAX_CANVAS_WITNESSES
    );
    assert_eq!(size_of::<CanvasState>(), 1664);
    assert_eq!(offset_of!(CanvasState, wants_redraw), 0);
    assert_eq!(offset_of!(CanvasState, witness_frame), 4);
    assert_eq!(offset_of!(CanvasState, witness_count), 8);
    assert_eq!(offset_of!(CanvasState, witness_lost), 12);
    assert_eq!(offset_of!(CanvasState, dim_reached), 16);
    assert_eq!(offset_of!(CanvasState, dim_id), 20);
    assert_eq!(offset_of!(CanvasState, shape_watchdog_ms), 24);
    assert_eq!(offset_of!(CanvasState, cover_count), 28);
    assert_eq!(offset_of!(CanvasState, wants_pointer), 32);
    assert_eq!(offset_of!(CanvasState, covers), 48);
    assert_eq!(offset_of!(CanvasState, witnesses), 128);
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
