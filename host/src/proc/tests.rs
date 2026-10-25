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
