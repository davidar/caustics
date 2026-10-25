use super::*;

#[test]
fn a_stall_warns_once_then_every_repeat() {
    let t0 = Instant::now();
    let mut h = Health::new();
    assert_eq!(h.observe_redraw(true, t0), None, "just started animating");
    assert_eq!(h.observe_redraw(true, t0 + Duration::from_secs(5)), None);
    let first = h.observe_redraw(true, t0 + STALL_WARN);
    assert_eq!(first, Some(STALL_WARN), "the first warning at STALL_WARN");
    assert_eq!(
        h.observe_redraw(true, t0 + STALL_WARN + Duration::from_secs(1)),
        None
    );
    assert!(
        h.observe_redraw(true, t0 + STALL_WARN + STALL_REPEAT)
            .is_some()
    );
    assert_eq!(
        h.observe_redraw(false, t0 + STALL_WARN + STALL_REPEAT),
        None
    );
    // Settling resets the stretch: a new one starts from zero.
    assert_eq!(h.observe_redraw(true, t0 + Duration::from_secs(1000)), None);
}

#[test]
fn a_settled_bar_never_warns() {
    let t0 = Instant::now();
    let mut h = Health::new();
    for i in 0..100 {
        assert_eq!(h.observe_redraw(false, t0 + Duration::from_secs(i)), None);
    }
}

#[test]
fn the_interval_reports_deltas_and_resets() {
    let t0 = Instant::now();
    let mut h = Health::new();
    h.frames_at_report = 10;
    let (_, frames, cpu) = h.take_interval(25, t0 + Duration::from_secs(60));
    assert_eq!(frames, 15);
    assert!(cpu >= 0.0);
    let (_, frames, _) = h.take_interval(25, t0 + Duration::from_secs(120));
    assert_eq!(frames, 0, "nothing drawn since the last report");
}
