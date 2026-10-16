use super::*;

#[test]
fn proc_numbers_read_as_something() {
    // This process has used some CPU and has some pages resident.
    assert!(cpu_seconds() >= 0.0);
    assert!(rss_bytes() > 0);
}

#[test]
fn another_process_is_read_by_pid_and_a_dead_one_is_none() {
    // Our own pid through the by-pid path agrees with `/proc/self`.
    let (cpu, rss) = proc_cost(std::process::id()).expect("this process exists");
    assert!(cpu >= 0.0);
    assert!(rss > 0, "the sidecar's RSS is what the health line reports");
    assert!(
        (rss as i64 - rss_bytes() as i64).abs() < 64 * 1_048_576,
        "the two readings of our own RSS disagree wildly: {rss} vs {}",
        rss_bytes()
    );
    // pid 0 is never a process; a sidecar between a death and a restart
    // reads the same way.
    assert_eq!(proc_cost(0), None);
}

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
