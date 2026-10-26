use super::*;

fn facts(pointer: &Pointer) -> CanvasFacts {
    let mut facts = CanvasFacts::default();
    pointer.write(&mut facts);
    facts
}

/// A position is written in the pane's buffer px: logical px times the
/// surface's scale, a fractional one included.
#[test]
fn a_position_is_written_in_buffer_px() {
    let mut pointer = Pointer::new();
    assert!(pointer.at(3, (100.0, 40.5), 240));
    let f = facts(&pointer);
    assert_eq!(f.pointer_pane, 3);
    assert_eq!((f.pointer_x, f.pointer_y), (200.0, 81.0));
    assert!(pointer.at(3, (100.0, 40.0), 180));
    let f = facts(&pointer);
    assert_eq!((f.pointer_x, f.pointer_y), (150.0, 60.0));
}

/// Nothing is news twice: the same pane and the same place is no change, so
/// a compositor repeating a motion costs no frame.
#[test]
fn the_same_place_again_is_not_news() {
    let mut pointer = Pointer::new();
    assert!(pointer.at(1, (10.0, 10.0), 120));
    assert!(!pointer.at(1, (10.0, 10.0), 120));
    assert!(
        pointer.at(2, (10.0, 10.0), 120),
        "the same place on another pane is"
    );
}

/// A leave of the pane the pointer has already left for another (the order
/// a compositor may send them in) leaves it where it is; a leave of its own
/// pane puts it nowhere, which is pane 0.
#[test]
fn only_a_leave_of_its_own_pane_puts_it_nowhere() {
    let mut pointer = Pointer::new();
    pointer.at(1, (5.0, 5.0), 120);
    pointer.at(2, (6.0, 6.0), 120);
    assert!(!pointer.left(1));
    assert_eq!(facts(&pointer).pointer_pane, 2);
    assert!(pointer.left(2));
    assert_eq!(facts(&pointer).pointer_pane, 0);
    assert!(!pointer.left(0), "nowhere is not a pane to leave");
}

/// Presses are counted where the pointer is, and only there: one on a pane
/// it is not on (or with it nowhere) is not the wake's. The count is what
/// the caustic compares, so two presses before a frame are still news.
#[test]
fn presses_are_counted_where_the_pointer_is() {
    let mut pointer = Pointer::new();
    assert!(!pointer.pressed(1), "nowhere: no press");
    pointer.at(1, (5.0, 5.0), 120);
    assert!(!pointer.pressed(2));
    assert!(pointer.pressed(1));
    assert!(pointer.pressed(1));
    assert_eq!(facts(&pointer).presses, 2);
    pointer.left(1);
    assert!(!pointer.pressed(1));
    assert_eq!(
        facts(&pointer).presses,
        2,
        "the count is kept across a leave"
    );
}

/// The count wraps rather than overflows: the caustic only asks whether it
/// changed.
#[test]
fn the_press_count_wraps() {
    let mut pointer = Pointer {
        pane: 1,
        at: (0.0, 0.0),
        presses: u32::MAX,
    };
    assert!(pointer.pressed(1));
    assert_eq!(facts(&pointer).presses, 0);
}
