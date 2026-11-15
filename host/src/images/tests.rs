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

/// A build that could have drawn a slot and did not frees it: the key no
/// longer answers, the embassy hears the name, and the slot is the next
/// insert's.
#[test]
fn a_slot_the_build_stops_drawing_is_freed() {
    let mut images = Images::new();
    images.begin_generation();
    images.insert("kept", &pixels(1)).expect("room");
    images.insert("gone", &pixels(2)).expect("room");
    // The compose that names both, and a build that draws only the first.
    images.begin_generation();
    assert!(images.touch_drawn(&mask([0])), "a slot was freed");
    assert_eq!(images.slot_of("kept"), Some(0));
    assert_eq!(images.slot_of("gone"), None);
    assert_eq!(images.len(), 1);
    assert_eq!(images.drawn(), 1);
    assert_eq!(images.frees(), 1);
    assert!(images.has_freed());
    assert_eq!(images.take_freed(), vec!["gone".to_owned()]);
    assert!(!images.has_freed(), "and only once");
    assert_eq!(images.insert("next", &pixels(3)), Some(1));
    // The same mask again frees nothing more.
    images.begin_generation();
    assert!(!images.touch_drawn(&mask([0, 1])));
}

/// A picture that landed after the last compose is one no build has been
/// told of, so a mask without its bit leaves it alone — until a compose has
/// named it.
#[test]
fn a_slot_no_build_has_seen_is_not_freed() {
    let mut images = Images::new();
    images.begin_generation();
    images.insert("new", &pixels(1)).expect("room");
    // A build made from the facts before this slot was named.
    assert!(!images.touch_drawn(&mask([])));
    assert_eq!(images.slot_of("new"), Some(0));
    // The compose names it; a build that still does not draw it lets it go.
    images.begin_generation();
    assert!(images.touch_drawn(&mask([])));
    assert_eq!(images.slot_of("new"), None);
}

/// More images at once than there are slots is a witness and a missing
/// icon — never a slot taken from another key.
#[test]
fn a_frame_that_wants_more_than_it_has_is_refused() {
    let mut images = Images::new();
    images.begin_generation();
    for i in 0..Images::capacity() {
        assert!(images.insert(&format!("k{i}"), &pixels(i as u8)).is_some());
    }
    assert_eq!(images.insert("one too many", &pixels(9)), None);
    assert_eq!(images.refusals(), 1);
    for i in 0..Images::capacity() {
        assert!(images.slot_of(&format!("k{i}")).is_some());
    }
    // Every slot drawn is every slot kept, and the refusal stands.
    images.begin_generation();
    assert!(!images.touch_drawn(&mask(0..Images::capacity() as u32)));
    assert_eq!(images.insert("still too many", &pixels(9)), None);
    assert_eq!(images.refusals(), 2);
    assert_eq!(images.frees(), 0);
}

/// A full array is retried by every build's wants, so a refusal warns once
/// an episode: the first after a successful claim. Retries are debug lines,
/// still counted; a claim that succeeds ends the episode, and the next
/// refusal warns again.
#[test]
fn a_full_array_warns_once_an_episode() {
    let mut images = Images::new();
    images.begin_generation();
    for i in 0..Images::capacity() {
        assert!(images.insert(&format!("k{i}"), &pixels(i as u8)).is_some());
    }
    for n in 0..10 {
        assert_eq!(images.insert(&format!("extra{n}"), &pixels(9)), None);
    }
    assert_eq!(images.refusals(), 10, "every refusal counted");
    assert_eq!(images.refusal_episodes(), 1, "one warning for the ten");

    // A slot freed, a claim that lands: the episode is over.
    images.begin_generation();
    assert!(images.touch_drawn(&mask(1..Images::capacity() as u32)));
    assert!(images.insert("lands", &pixels(3)).is_some());
    assert_eq!(images.insert("refused again", &pixels(4)), None);
    assert_eq!(images.insert("and again", &pixels(4)), None);
    assert_eq!(
        images.refusal_episodes(),
        2,
        "a new episode, one more warning"
    );
    assert_eq!(images.refusals(), 12);
}

/// A lookup is a listing, not a draw: a slot only ever looked up — the image
/// directory names every key asked, on every compose — is freed like any
/// other the build does not draw.
#[test]
fn a_lookup_does_not_keep_a_slot() {
    let mut images = Images::new();
    images.begin_generation();
    images.insert("listed", &pixels(1)).expect("room");
    images.begin_generation();
    assert!(images.slot_of("listed").is_some());
    assert!(images.touch_drawn(&mask([])));
    assert_eq!(images.slot_of("listed"), None);
}

/// A mask replaces the previous mask: a slot drawn last build and not this
/// one goes.
#[test]
fn a_mask_replaces_the_previous_mask() {
    let mut images = Images::new();
    images.begin_generation();
    for i in 0..Images::capacity() {
        images.insert(&format!("k{i}"), &pixels(1)).expect("room");
    }
    images.begin_generation();
    assert!(!images.touch_drawn(&mask(0..Images::capacity() as u32)));
    assert_eq!(images.drawn(), Images::capacity());
    // The next build draws everything but slot 5.
    assert!(images.touch_drawn(&mask(
        (0..Images::capacity() as u32).filter(|slot| *slot != 5),
    )));
    assert_eq!(images.drawn(), Images::capacity() - 1);
    assert_eq!(images.slot_of("k5"), None);
    assert_eq!(images.insert("next", &pixels(3)), Some(5));
}

/// The slot size is the one the ABI declares, and one slot is square RGBA.
#[test]
fn the_slot_size_is_the_abis() {
    assert_eq!(Images::slot_bytes(), 64 * 64 * 4);
    assert_eq!(Images::capacity(), abi::MAX_IMAGE_SLOTS);
    // 1 MiB, fixed: the whole point of the slot array.
    assert_eq!(Images::new().pixels.len(), 64 * 64 * 4 * 64);
}
