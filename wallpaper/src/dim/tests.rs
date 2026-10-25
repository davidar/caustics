use super::*;

/// A waited dim is answered `done` when the host says it reached its depth,
/// and stays dimmed after; ids count from 1.
#[test]
fn a_waiter_is_done_when_its_dim_reaches_its_depth() {
    let mut dims: Dimmer<&str> = Dimmer::new();
    let (id, replaced) = dims.begin(1000, 3000, Some("client"));
    assert_eq!((id, replaced), (1, None));
    assert_eq!(dims.describe(2500), "dim=dimming progress=0.50");
    assert_eq!(
        dims.event(CANVAS_EVENT_DIMMED, 1),
        Outcome::Done(Some("client"))
    );
    assert_eq!(dims.describe(9000), "dim=dimmed");
    // The dim is still up; input after it cancels it, with nobody waiting.
    assert_eq!(dims.event(CANVAS_EVENT_INPUT, 1), Outcome::Cancelled(None));
    assert_eq!(dims.describe(9000), "dim=none");
}

/// Input cancels the dim and its waiter is answered `cancelled`; the dim is
/// over, so a late event for it is stale.
#[test]
fn input_cancels_the_dim_and_its_waiter() {
    let mut dims: Dimmer<&str> = Dimmer::new();
    let (id, _) = dims.begin(0, 6000, Some("client"));
    assert_eq!(
        dims.event(CANVAS_EVENT_INPUT, id),
        Outcome::Cancelled(Some("client"))
    );
    assert_eq!(dims.event(CANVAS_EVENT_DIMMED, id), Outcome::Stale);
    assert_eq!(dims.describe(0), "dim=none");
}

/// A second dim replaces the first: the first's waiter is handed back to be
/// answered `cancelled`, and an event under the first's id changes nothing.
#[test]
fn a_second_dim_replaces_the_first_and_cancels_its_waiter() {
    let mut dims: Dimmer<&str> = Dimmer::new();
    let (first, _) = dims.begin(0, 6000, Some("first"));
    let (second, replaced) = dims.begin(10, 1000, Some("second"));
    assert_eq!(replaced, Some("first"));
    assert_ne!(first, second);
    assert_eq!(dims.event(CANVAS_EVENT_DIMMED, first), Outcome::Stale);
    assert_eq!(dims.event(CANVAS_EVENT_INPUT, first), Outcome::Stale);
    assert_eq!(
        dims.event(CANVAS_EVENT_DIMMED, second),
        Outcome::Done(Some("second"))
    );
}

/// An undim ends the dim and hands back its waiter; with no dim up, every
/// event is stale.
#[test]
fn an_undim_cancels_the_waiter() {
    let mut dims: Dimmer<&str> = Dimmer::new();
    let (id, _) = dims.begin(0, 0, Some("client"));
    assert_eq!(dims.describe(0), "dim=dimming progress=1.00");
    assert_eq!(dims.undim(), Some("client"));
    assert_eq!(dims.undim(), None);
    assert_eq!(dims.event(CANVAS_EVENT_DIMMED, id), Outcome::Stale);
    // A dim nobody waits on has no waiter to answer either way.
    let (id, replaced) = dims.begin(0, 0, None);
    assert_eq!(replaced, None);
    assert_eq!(dims.event(CANVAS_EVENT_DIMMED, id), Outcome::Done(None));
}
