use super::*;

/// A dim reaching its depth is told to its sidecar once, under its id; a
/// readback for another id, or one that says not yet, tells nobody.
#[test]
fn a_dim_at_its_depth_is_told_once() {
    let mut dims = Dims::new();
    assert_eq!(dims.reached(1, 1), None, "no dim, nobody to tell");
    dims.begin("caustics-wallpaper", 4);
    assert_eq!(dims.reached(0, 4), None, "not there yet");
    assert_eq!(dims.reached(1, 3), None, "another dim's depth");
    assert_eq!(
        dims.reached(1, 4),
        Some(("caustics-wallpaper".to_owned(), 4))
    );
    assert_eq!(dims.reached(1, 4), None, "said once");
    assert!(dims.is_up(), "a completed dim stays up");
}

/// A newer dim replaces the one up, which is never told it reached anything;
/// the newer one is told of its own depth.
#[test]
fn a_newer_dim_replaces_the_one_up() {
    let mut dims = Dims::new();
    dims.begin("caustics-wallpaper", 1);
    let replaced = dims.begin("caustics-wallpaper", 2).expect("one was up");
    assert_eq!(replaced.id, 1);
    assert_eq!(dims.reached(1, 1), None, "the replaced dim's late readback");
    assert_eq!(dims.reached(1, 2).map(|(_, id)| id), Some(2));
}

/// Input cancels the dim once, whatever comes after it in the same burst;
/// and after an undim there is nothing to cancel.
#[test]
fn input_cancels_once_and_an_undim_leaves_nothing_to_cancel() {
    let mut dims = Dims::new();
    dims.begin("caustics-wallpaper", 9);
    assert_eq!(dims.cancel().map(|dim| dim.id), Some(9));
    assert_eq!(dims.cancel(), None, "the second event of a burst");
    dims.begin("caustics-wallpaper", 10);
    assert_eq!(dims.end().map(|dim| dim.id), Some(10));
    assert_eq!(dims.cancel(), None);
    assert_eq!(dims.reached(1, 10), None);
}

/// A sidecar that goes takes its dim with it; another sidecar going does not.
#[test]
fn a_gone_sidecar_takes_its_dim() {
    let mut dims = Dims::new();
    dims.begin("caustics-wallpaper", 2);
    assert_eq!(dims.gone("caustics-reload"), None);
    assert!(dims.is_up());
    assert_eq!(dims.gone("caustics-wallpaper").map(|dim| dim.id), Some(2));
    assert!(!dims.is_up());
}
