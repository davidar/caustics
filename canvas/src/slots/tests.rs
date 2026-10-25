use super::*;
use caustics_bar_abi::wire::{CANVAS_DIM, CANVAS_SHOW, CANVAS_UNDIM};

fn image(key: &str) -> Image {
    Image {
        key: key.into(),
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 4],
    }
}

fn show(key: &str, start: u64, duration: u32) -> Canvas {
    Canvas::Show {
        key: key.into(),
        fade_start_ms: start,
        fade_duration_ms: duration,
    }
}

/// Before anything: no picture, so the caustic paints the dim colour.
#[test]
fn a_new_canvas_holds_no_picture() {
    let slots = Slots::new();
    assert_eq!(slots.facts().from_slot, NO_INDEX);
    assert_eq!(slots.facts().to_slot, NO_INDEX);
    assert_eq!(slots.facts().shows, 0);
    assert_eq!(slots.showing(), None);
}

/// The first show fades from the dim colour into slot A; the next from A into
/// B; the one after from B into A. The show's head is copied whole.
#[test]
fn shows_alternate_the_slots_and_fade_from_the_picture_on_screen() {
    let mut slots = Slots::new();
    slots.hold(image("one"));
    let applied = slots.apply(&show("one", 1000, 0)).expect("one arrived");
    assert_eq!(
        applied,
        Applied::Upload {
            slot: SLOT_A,
            image: image("one")
        }
    );
    assert_eq!(
        (slots.facts().from_slot, slots.facts().to_slot),
        (NO_INDEX, SLOT_A)
    );

    slots.hold(image("two"));
    let Applied::Upload { slot, .. } = slots.apply(&show("two", 5000, 4000)).expect("arrived")
    else {
        panic!("two was not placed")
    };
    assert_eq!(slot, SLOT_B);
    assert_eq!(
        (slots.facts().from_slot, slots.facts().to_slot),
        (SLOT_A, SLOT_B)
    );
    assert_eq!(slots.facts().show.command, CANVAS_SHOW);
    assert_eq!(slots.facts().show.start_ms(), 5000);
    assert_eq!(slots.facts().show.duration_ms, 4000);
    assert_eq!(slots.facts().shows, 2);

    slots.hold(image("three"));
    slots.apply(&show("three", 9000, 1)).expect("arrived");
    assert_eq!(
        (slots.facts().from_slot, slots.facts().to_slot),
        (SLOT_B, SLOT_A)
    );
    assert_eq!(slots.showing(), Some("three"));
}

/// An image is placed by its show, not when it lands: a second image before
/// the show replaces the first, which is handed back unshown.
#[test]
fn an_image_waits_for_its_show() {
    let mut slots = Slots::new();
    assert_eq!(slots.hold(image("a")), None);
    assert_eq!(slots.hold(image("b")).map(|i| i.key), Some("a".to_owned()));
    assert!(slots.apply(&show("a", 0, 0)).is_err(), "a was replaced");
    assert!(slots.apply(&show("b", 0, 0)).is_ok());
    assert_eq!(slots.facts().to_slot, SLOT_A);
}

/// A show for a key that never arrived moves nothing and says why; a show of
/// the picture on screen is no fade.
#[test]
fn a_show_of_nothing_or_of_the_same_picture_moves_nothing() {
    let mut slots = Slots::new();
    let err = slots
        .apply(&show("ghost", 0, 0))
        .expect_err("never arrived");
    assert!(err.contains("ghost"), "{err}");
    assert_eq!(slots.facts().to_slot, NO_INDEX);

    slots.hold(image("x"));
    slots.apply(&show("x", 0, 100)).expect("arrived");
    let before = *slots.facts();
    assert_eq!(slots.apply(&show("x", 99, 100)), Ok(Applied::Facts));
    assert_eq!(*slots.facts(), before, "a repeat show changed the facts");
}

/// A dim and an undim are copied into their own place and leave the show
/// alone.
#[test]
fn a_dim_is_its_own_fact() {
    let mut slots = Slots::new();
    slots.hold(image("x"));
    slots.apply(&show("x", 7, 0)).expect("arrived");
    let dim = Canvas::Dim {
        id: 3,
        alpha_milli: 400,
        start_ms: 10,
        duration_ms: 3000,
    };
    assert_eq!(slots.apply(&dim), Ok(Applied::Facts));
    assert_eq!(slots.facts().dim.command, CANVAS_DIM);
    assert_eq!(slots.facts().dim.alpha_milli, 400);
    assert_eq!(slots.facts().dim.id, 3, "the dim's id is copied with it");
    assert_eq!(slots.facts().show.start_ms(), 7);
    assert_eq!(slots.apply(&Canvas::Undim), Ok(Applied::Facts));
    assert_eq!(slots.facts().dim.command, CANVAS_UNDIM);
    assert_eq!(slots.facts().to_slot, SLOT_A);
}

/// A sidecar that leaves takes its waiting image with it.
#[test]
fn a_gone_sidecar_takes_its_waiting_image() {
    let mut slots = Slots::new();
    slots.hold(image("w"));
    assert_eq!(slots.forget_waiting().map(|i| i.key), Some("w".to_owned()));
    assert!(slots.apply(&show("w", 0, 0)).is_err());
}

/// A dim that ends without an undim (input, or its sidecar gone) leaves the
/// facts saying what an undim would, and the picture where it was.
#[test]
fn a_dim_cleared_by_the_host_reads_as_an_undim() {
    let mut slots = Slots::new();
    slots.hold(image("x"));
    slots.apply(&show("x", 7, 0)).expect("arrived");
    slots
        .apply(&Canvas::Dim {
            id: 2,
            alpha_milli: 1000,
            start_ms: 0,
            duration_ms: 0,
        })
        .expect("a dim");
    slots.clear_dim();
    assert_eq!(slots.facts().dim, Canvas::Undim.body());
    assert_eq!(slots.facts().to_slot, SLOT_A);
}
