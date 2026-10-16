use super::*;

fn pixels(value: u8) -> Vec<u8> {
    vec![value; Images::slot_bytes()]
}

#[test]
fn an_insert_takes_a_slot_and_a_lookup_finds_it_again() {
    let mut images = Images::new();
    assert_eq!(images.len(), 0);
    assert_eq!(images.generation(), 0);
    images.begin_generation();
    assert_eq!(images.generation(), 1);
    let slot = images.insert("a", &pixels(1)).expect("an empty array");
    assert_eq!(slot, 0);
    assert_eq!(images.len(), 1);
    assert_eq!(images.slot_of("a"), Some(0));
    assert_eq!(images.slot_of("b"), None);
    let dirty = images.take_dirty();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].0, 0);
    assert_eq!(dirty[0].1, pixels(1));
    assert!(images.take_dirty().is_empty(), "and only once");
}

#[test]
fn the_same_key_is_overwritten_in_place() {
    let mut images = Images::new();
    images.begin_generation();
    let first = images.insert("a", &pixels(1)).expect("empty");
    images.take_dirty();
    let second = images.insert("a", &pixels(2)).expect("the same key");
    assert_eq!(first, second, "a changed pixmap keeps its slot");
    assert_eq!(images.len(), 1);
    let dirty = images.take_dirty();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].1, pixels(2));
}

#[test]
fn wrong_sized_pixels_are_refused_not_truncated() {
    let mut images = Images::new();
    images.begin_generation();
    assert_eq!(images.insert("a", &[0, 0, 0, 0]), None);
    assert_eq!(images.len(), 0);
    assert!(images.take_dirty().is_empty());
}

/// The caustic's mask with every slot in `drawn` set, as an
/// `ACT_IMAGES_DRAWN` carries it.
fn mask(drawn: impl IntoIterator<Item = u32>) -> Vec<u32> {
    let mut words = vec![0u32; abi::IMAGE_DRAWN_WORDS];
    for slot in drawn {
        words[slot as usize / 32] |= 1 << (slot % 32);
    }
    words
}

/// The eviction: a full array gives up the slot nothing has drawn for the
/// longest, and the key that lost it no longer answers.
#[test]
fn a_full_array_evicts_the_least_recently_drawn() {
    let mut images = Images::new();
    // Fill it, one key per generation, so every stamp is distinct.
    for i in 0..Images::capacity() {
        images.begin_generation();
        assert!(images.insert(&format!("k{i}"), &pixels(i as u8)).is_some());
    }
    assert_eq!(images.len(), Images::capacity());

    // A new generation whose build draws everything but the first key, then
    // asks for one more.
    images.begin_generation();
    images.touch_drawn(&mask(1..Images::capacity() as u32));
    assert_eq!(images.drawn(), Images::capacity() - 1);
    let slot = images.insert("new", &pixels(200)).expect("k0 is not drawn");
    assert_eq!(slot, 0, "the slot k0 was in");
    assert_eq!(images.slot_of("k0"), None, "k0 no longer answers");
    assert_eq!(images.slot_of("new"), Some(0));
    assert_eq!(images.evictions(), 1);
    assert_eq!(images.take_evicted(), vec!["k0".to_owned()]);
    assert_eq!(images.len(), Images::capacity());
}

/// More images in one frame than there are slots is a witness and a missing
/// icon — never a slot stolen from something the same frame filled.
#[test]
fn a_frame_that_wants_more_than_it_has_is_refused() {
    let mut images = Images::new();
    images.begin_generation();
    for i in 0..Images::capacity() {
        assert!(images.insert(&format!("k{i}"), &pixels(i as u8)).is_some());
    }
    assert_eq!(images.insert("one too many", &pixels(9)), None);
    assert_eq!(images.refusals(), 1);
    // And nothing this frame filled was disturbed.
    for i in 0..Images::capacity() {
        assert!(images.slot_of(&format!("k{i}")).is_some());
    }
    assert_eq!(images.evictions(), 0);
}

/// A lookup is a listing, not a draw. A slot only ever looked up —
/// the image directory names every key ever asked, on every compose — ages
/// like any other undrawn slot.
#[test]
fn a_lookup_does_not_keep_a_slot_young() {
    let mut images = Images::new();
    for i in 0..Images::capacity() {
        images.begin_generation();
        images.insert(&format!("k{i}"), &pixels(1)).expect("room");
    }
    // Every compose lists every key; no build draws any of them.
    images.begin_generation();
    for i in 0..Images::capacity() {
        assert!(images.slot_of(&format!("k{i}")).is_some());
    }
    assert_eq!(
        images.insert("new", &pixels(2)),
        Some(0),
        "k0, the oldest, goes"
    );
    assert_eq!(images.refusals(), 0);
}

/// A drawn slot is never taken while an undrawn one exists — however old the
/// drawn one's last fill and however new the undrawn one's.
#[test]
fn a_drawn_slot_is_never_evicted_while_an_undrawn_one_exists() {
    let mut images = Images::new();
    for i in 0..Images::capacity() {
        images.begin_generation();
        images.insert(&format!("k{i}"), &pixels(1)).expect("room");
    }
    // The build draws all but the newest key.
    images.begin_generation();
    let last = Images::capacity() as u32 - 1;
    images.touch_drawn(&mask(0..last));
    images.begin_generation();
    assert_eq!(
        images.insert("new", &pixels(2)),
        Some(last),
        "the one slot not drawn"
    );
    // The next generation: "new" is undrawn but filled a generation ago, the
    // drawn slots are still drawn — "new" is the only candidate.
    images.begin_generation();
    assert_eq!(images.insert("newer", &pixels(3)), Some(last));
    assert_eq!(images.slot_of("new"), None);
    for i in 0..last {
        assert!(
            images.slot_of(&format!("k{i}")).is_some(),
            "k{i} is drawn and stays"
        );
    }
}

/// The refusal fires only when every slot is drawn or was filled this
/// generation — and then it does.
#[test]
fn the_refusal_fires_only_when_every_slot_is_drawn_or_new() {
    let mut images = Images::new();
    images.begin_generation();
    for i in 0..Images::capacity() / 2 {
        images.insert(&format!("old{i}"), &pixels(1)).expect("room");
    }
    images.begin_generation();
    images.touch_drawn(&mask(0..Images::capacity() as u32 / 2));
    for i in 0..Images::capacity() / 2 {
        images.insert(&format!("new{i}"), &pixels(1)).expect("room");
    }
    // Half drawn, half filled this generation: nothing to take.
    assert_eq!(images.insert("refused", &pixels(9)), None);
    assert_eq!(images.refusals(), 1);
    assert_eq!(images.evictions(), 0);
    // A generation later the new half is fair game.
    images.begin_generation();
    let slot = images
        .insert("taken", &pixels(9))
        .expect("an undrawn slot from last generation");
    assert!(slot as usize >= Images::capacity() / 2);
    assert_eq!(images.refusals(), 1);
}

/// A mask replaces the previous mask: a slot drawn last build and not this
/// one is a candidate again.
#[test]
fn a_mask_replaces_the_previous_mask() {
    let mut images = Images::new();
    images.begin_generation();
    for i in 0..Images::capacity() {
        images.insert(&format!("k{i}"), &pixels(1)).expect("room");
    }
    images.begin_generation();
    images.touch_drawn(&mask(0..Images::capacity() as u32));
    assert_eq!(images.drawn(), Images::capacity());
    images.begin_generation();
    assert_eq!(
        images.insert("refused", &pixels(2)),
        None,
        "everything is drawn"
    );
    // The next build draws everything but slot 5.
    images.touch_drawn(&mask(
        (0..Images::capacity() as u32).filter(|slot| *slot != 5),
    ));
    assert_eq!(images.drawn(), Images::capacity() - 1);
    images.begin_generation();
    assert_eq!(images.insert("next", &pixels(3)), Some(5));
    assert_eq!(images.slot_of("k5"), None);
}

/// The slot size is the one the ABI declares, and one slot is square RGBA.
#[test]
fn the_slot_size_is_the_abis() {
    assert_eq!(Images::slot_bytes(), 64 * 64 * 4);
    assert_eq!(Images::capacity(), abi::MAX_IMAGE_SLOTS);
    // 1 MiB, fixed: the whole point of the slot array.
    assert_eq!(Images::new().pixels.len(), 64 * 64 * 4 * 64);
}
