use super::*;

/// The commands the embassy puts in the ring, each with the payload *Constants* gives
/// its tag. Nothing here is a decision; the caustic answers every one.
#[test]
fn every_command_carries_its_tags_payload() {
    let down = Command::pointer_down(TARGET_POPUP, [3.0, 4.0], BTN_RIGHT);
    assert_eq!((down.tag, down.target), (CMD_POINTER_DOWN, TARGET_POPUP));
    assert_eq!(down.pos, [3.0, 4.0]);
    assert_eq!(down.args[0], BTN_RIGHT, "the button, as an integer");

    let mapped = Command::popup_mapped(300, 200);
    assert_eq!(mapped.tag, CMD_POPUP_MAPPED);
    assert_eq!((mapped.args[0], mapped.args[1]), (300, 200));

    let leave = Command::pointer_leave(TARGET_BAR);
    assert_eq!(leave.pos, [-1.0, -1.0], "a leave has no position to report");

    // The embassy's wall clock noticed the bar animating and asks
    // the caustic to say what is holding it awake (docs/abi.md, *The witness ring*).
    let stall = Command::dump_stall();
    assert_eq!(stall.tag, CMD_DUMP_STALL);
    assert_eq!(
        stall.target, TARGET_BAR,
        "and it is about no pointer at all"
    );

    // The embassy wrote a sidecar's body into its region of the rows
    // arena; whose it was is the whole payload, and whether that is a rebuild
    // is the caustic's (docs/abi.md, *The arenas*).
    let rows = Command::rows(4);
    assert_eq!(rows.tag, CMD_ROWS);
    assert_eq!(rows.args[0], 4);

    // The wheel, as the compositor sent it. Signed, so a scroll up
    // survives the trip as its two's complement and the caustic reads it back
    // with `asint`.
    let down = Command::axis(TARGET_POPUP, [5.0, 6.0], 45, 3);
    assert_eq!((down.tag, down.target), (CMD_AXIS, TARGET_POPUP));
    assert_eq!(down.pos, [5.0, 6.0]);
    assert_eq!((down.args[0], down.args[1]), (45, 3));
    let up = Command::axis(TARGET_POPUP, [5.0, 6.0], -30, -2);
    assert_eq!(up.args[0] as i32, -30);
    assert_eq!(up.args[1] as i32, -2);
}

/// The action ring's records, read by the meanings *The action ring* gives each kind.
///
/// This is the whole of what the embassy knows about an action: there is no
/// table behind any of these words. A window id is the compositor's own, an
/// app id is a range of the strings buffer this side uploaded, a sidecar
/// action is the wire body's words.
#[test]
fn a_notes_operands_are_read_by_meaning() {
    let mut window = Note::ZERO;
    window.head = ACT_ACTIVATE_WINDOW;
    window.arg[0] = 0x89ab_cdef;
    window.arg[1] = 0x0123_4567;
    assert_eq!(window.window_id(), 0x0123_4567_89ab_cdef, "low word first");
    assert_eq!(
        Note::ZERO.window_id(),
        0,
        "and a zeroed record names no window"
    );

    let mut spawn = Note::ZERO;
    spawn.head = ACT_SPAWN;
    spawn.arg[0] = 96;
    spawn.arg[1] = 7;
    assert_eq!(spawn.string_range(), (96, 7));

    let mut anchor = Note::ZERO;
    anchor.arg[0] = EDGE_TOP;
    anchor.arg[1] = 40;
    assert_eq!(anchor.anchor(), (EDGE_TOP, 40));

    let mut sidecar = Note::ZERO;
    sidecar.head = ACT_SIDECAR;
    sidecar.arg = [
        5,
        7,
        ACTION_SET_VALUE,
        BTN_LEFT,
        3.0f32.to_bits(),
        4.0f32.to_bits(),
        0.25f32.to_bits(),
        0,
        0,
        0,
    ];
    let args = sidecar.sidecar();
    assert_eq!((args.slot, args.row), (5, 7));
    assert_eq!(
        args.kind, ACTION_SET_VALUE,
        "the **wire's** kind, forwarded"
    );
    assert_eq!(args.button, BTN_LEFT);
    assert_eq!(
        args.pos,
        [3.0, 4.0],
        "physical px; the embassy divides by the scale"
    );
    assert_eq!(args.value, 0.25);

    let mut popup = Note::ZERO;
    popup.head = ACT_POPUP_OPEN;
    popup.arg = [
        POPUP_MENU,
        2,
        9,
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
        300,
        200,
        0,
    ];
    let open = popup.popup_open();
    assert_eq!((open.kind, open.slot, open.row), (POPUP_MENU, 2, 9));
    assert_eq!(open.anchor, [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(open.want, [300, 200]);

    let mut moved = Note::ZERO;
    moved.arg[0] = 320;
    moved.arg[1] = 240;
    assert_eq!(moved.popup_size(), [320, 240]);

    let mut want = Note::ZERO;
    want.head = ACT_WANT_ICON;
    want.arg[0] = 11;
    want.arg[1] = 4;
    want.arg[2] = crate::wire::REQ_GLYPH;
    // ...and which of the three buffers the bytes are in: a
    // range alone cannot say (docs/abi.md, *The action ring*).
    want.arg[3] = 2;
    let asked = want.want_icon();
    assert_eq!(
        (asked.off, asked.len, asked.kind),
        (11, 4, crate::wire::REQ_GLYPH)
    );
    assert_eq!(asked.source, 2, "slot 2's region of the rows arena");
    assert_eq!(
        SRC_STRINGS, MAX_SLOTS as u32,
        "and the two that are not a slot follow them"
    );
    assert_eq!(SRC_ANSWERS, MAX_SLOTS as u32 + 1);

    // An `ACT_COVER` carries the same word, third (docs/abi.md, *The action ring*): a
    // sidecar's row label is a range of its slot's region, not of `strings`.
    let mut cover = Note::ZERO;
    cover.head = ACT_COVER;
    cover.arg[0] = 40;
    cover.arg[1] = 17;
    cover.arg[2] = 3;
    assert_eq!(
        cover.cover(),
        Cover {
            off: 40,
            len: 17,
            source: 3
        }
    );

    // A float travels as its bit pattern, which is what lets one record serve
    // both rings (docs/abi.md, *Structs*).
    let mut floats = Note::ZERO;
    floats.arg[3] = (-0.5f32).to_bits();
    assert_eq!(floats.arg_f(3), -0.5);
    assert_eq!(floats.arg_f(9), 0.0, "and a word nobody wrote is zero");
}

/// Each ring is read against its own count, and never past its capacity: a
/// count from a caustic that has since been swapped must name a record this
/// build has.
///
/// Which of them the embassy has already acted on is the *caller's*
/// bookkeeping — a ring belongs to the frame stamped on it (*The three rules the buffer adds*, rule 1), and a
/// frame that failed at the swapchain keeps its stamp so the next attempt
/// appends to what it pushed.
#[test]
fn the_rings_are_read_against_their_counts() {
    let mut state = State::ZERO;
    assert!(
        state.live_actions().is_empty(),
        "a caustic that has not run pushed nothing"
    );
    assert!(state.live_witnesses().is_empty());
    assert_eq!(state.surface.frame, 0, "and `frame` is how that is known");

    state.surface.action_count = 3;
    state.actions[2].head = ACT_PREVIEW_BEGIN;
    assert_eq!(state.live_actions().len(), 3);
    assert_eq!(state.live_actions()[2].head, ACT_PREVIEW_BEGIN);

    state.surface.witness_count = 2;
    state.witnesses[1].aux = WITNESS_WARN;
    assert_eq!(state.live_witnesses().len(), 2);
    assert_eq!(state.live_witnesses()[1].aux, WITNESS_WARN);

    // A count past the ring is a corrupt mirror, not a buffer overrun.
    state.surface.action_count = 9999;
    assert_eq!(state.live_actions().len(), MAX_ACTIONS);
    state.surface.witness_count = 9999;
    assert_eq!(state.live_witnesses().len(), MAX_WITNESSES);
}

/// What the embassy writes down: a literal of the pool, a dock entry
/// that is an app id and one flag, a sidecar slot that is a name, and an image
/// slot named by its key (docs/abi.md, *The facts*).
#[test]
fn the_facts_the_embassy_writes_read_back_by_name() {
    assert_eq!(size_of::<Literal>(), 16);
    assert_eq!(
        Literal::ZERO.len,
        0,
        "a pool the embassy has not filled answers for nothing"
    );

    // The slot's name, because the caustic spells `applet-caustics-power` in a
    // witness of its own (docs/abi.md, *The witness ring*). Its *rows* are its region of the arena.
    let slot = SlotFact {
        name_off: 40,
        name_len: 10,
        ..SlotFact::ZERO
    };
    assert_eq!((slot.name_off, slot.name_len), (40, 10));
    assert_eq!(SlotFact::ZERO.name_len, 0, "and an empty slot has no name");

    // A dock entry is its app id and whether it is pinned. What it is called
    // and what its icon is are the apps sidecar's answer, in the arena.
    let app = AppFact {
        id_off: 8,
        id_len: 20,
        flags: AFLAG_FAVOURITE,
        _pad0: 0,
    };
    assert_eq!((app.id_off, app.id_len), (8, 20));
    assert_eq!(size_of::<AppFact>(), 16, "four words");
    assert_eq!(AppFact::ZERO.flags, 0);

    // An image slot is named by the key its pixmap arrived under, and by
    // nothing else: a row's pixmap icon is a key too (docs/abi.md, *Pixmap icons*).
    let keyed = ImageFact {
        name_off: 4,
        name_len: 5,
        slot: 3,
        _pad0: 0,
    };
    assert_eq!(keyed.slot, 3);
    assert_eq!(size_of::<ImageFact>(), 16);

    // And the whole thing is 10528 B, uploaded whole on every change.
    assert_eq!(size_of::<Facts>(), 10528);
    assert_eq!(FACTS_BUFFER_SIZE, 10528);
}

#[test]
fn byte_views_round_trip() {
    let mut state = State::ZERO;
    state.surface.frame = 7;
    state.surface.action_count = 3;
    state.actions[2].head = ACT_PREVIEW_BEGIN;
    state.witnesses[MAX_WITNESSES - 1].arg[9] = 0xffff;
    let bytes = as_bytes(&state).to_vec();
    assert_eq!(bytes.len(), 21536, "32 + 64 x 48 + 384 x 48");
    let mut back = State::ZERO;
    assert!(write_from_bytes(&mut back, &bytes));
    assert_eq!(back, state);
    assert!(!write_from_bytes(&mut back, &bytes[..100]));
}
