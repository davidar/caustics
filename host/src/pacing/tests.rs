use super::*;

/// With no request out, a dirty host draws; with one out, it waits for the
/// answer, and the answer re-arms it.
#[test]
fn a_request_in_flight_holds_the_next_frame_until_it_is_answered() {
    let t0 = Instant::now();
    let mut pacing = Pacing::new(t0);
    assert!(pacing.allowed_at(t0), "nothing requested yet");
    pacing.requested(t0);
    assert!(!pacing.allowed_at(t0 + Duration::from_millis(16)));
    assert!(
        !pacing.answered(),
        "a settled caustic: the answer is no frame"
    );
    assert!(pacing.allowed_at(t0 + Duration::from_millis(16)));
}

/// A compositor that never answers costs half a second of input, never the
/// rest of the session.
#[test]
fn an_unanswered_request_stops_holding_after_the_stall() {
    let t0 = Instant::now();
    let mut pacing = Pacing::new(t0);
    pacing.requested(t0);
    assert!(!pacing.allowed_at(t0 + FRAME_STALL - Duration::from_millis(1)));
    assert!(pacing.allowed_at(t0 + FRAME_STALL));
}

/// While the caustic animates, the answer to a request is itself a frame.
#[test]
fn an_animating_caustic_draws_on_every_answer() {
    let t0 = Instant::now();
    let mut pacing = Pacing::new(t0);
    pacing.set_animating(true);
    assert!(pacing.animating());
    pacing.requested(t0);
    assert!(pacing.answered());
    pacing.set_animating(false);
    pacing.requested(t0);
    assert!(!pacing.answered());
}
