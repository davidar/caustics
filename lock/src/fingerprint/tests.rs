//! The reader against the fake fprintd (`examples/fake_fprintd.rs`), served
//! in-process on a private `dbus-daemon`: what each script and each mode
//! makes the run say, and what it lets go of when it stops.

use std::time::Duration;

use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::unbounded_channel;

use super::*;

#[path = "../../examples/fake_fprintd.rs"]
mod fake;

const WAIT: Duration = Duration::from_secs(10);

/// Short enough for a test, long enough to tell apart.
const TIMINGS: Timings = Timings {
    first_backoff: Duration::from_millis(100),
    last_backoff: Duration::from_millis(400),
    retry_pause: Duration::from_millis(50),
    release_bound: Duration::from_secs(2),
    settled: Duration::from_millis(300),
};

/// A bus with the fake on it, and a run of the reader against it.
struct Rig {
    _bus: Bus,
    _fake: Option<Connection>,
    log: fake::Log,
    events: UnboundedReceiver<Event>,
    run: Option<Run>,
}

impl Rig {
    /// `None` when there is no `dbus-daemon` here; the test says so and
    /// passes.
    async fn start(script: &str, scan_type: &str, mode: fake::Mode) -> Option<Rig> {
        Rig::with(Some(fake::Script::parse(
            script,
            Duration::from_millis(50),
            scan_type,
            mode,
        )))
        .await
    }

    async fn with(script: Option<fake::Script>) -> Option<Rig> {
        let Some(bus) = Bus::start() else {
            eprintln!("no dbus-daemon to start; the reader is untested here");
            return None;
        };
        let (fake, log) = match script {
            Some(script) => {
                let conn = zbus::connection::Builder::address(bus.address.as_str())
                    .expect("the bus address parses")
                    .build()
                    .await
                    .expect("the fake's connection");
                let log = fake::serve(&conn, script).await.expect("the fake serves");
                (Some(conn), log)
            }
            None => (None, fake::Log::default()),
        };
        let (events, inbox) = unbounded_channel();
        let run = Fprintd::at(&bus.address, TIMINGS).start("tester", 7, events);
        Some(Rig {
            _bus: bus,
            _fake: fake,
            log,
            events: inbox,
            run: Some(run),
        })
    }

    async fn finger(&mut self) -> Finger {
        match tokio::time::timeout(WAIT, self.events.recv()).await {
            Ok(Some(Event::Finger { run, finger })) => {
                assert_eq!(run, 7, "the run's own id");
                finger
            }
            Ok(Some(_)) => panic!("an event that is not the reader's"),
            other => panic!("no finding in time: {:?}", other.map(|e| e.is_some())),
        }
    }

    async fn stop(&mut self) {
        let task = self.run.take().expect("a run").stop();
        tokio::time::timeout(WAIT, task)
            .await
            .expect("the run ends when stopped")
            .expect("the run's task");
    }

    /// Wait until the fake's log holds `line` at least `n` times.
    async fn logged(&self, line: &str, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.log.count(line) < n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{line} {n} times: {:?}",
                self.log.lines()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn ready_press() -> Finger {
    Finger::Ready {
        how: "Touch the fingerprint reader".to_owned(),
    }
}

/// A no-match is an error line and a fresh verify after a stop; the match
/// that follows is the verdict; the stop lets the reader go.
#[tokio::test]
async fn a_no_match_restarts_the_verify_and_a_match_is_the_verdict() {
    let Some(mut rig) = Rig::start("no-match,match", "press", fake::Mode::Normal).await else {
        return;
    };
    assert_eq!(rig.finger().await, ready_press());
    assert_eq!(rig.finger().await, Finger::NoMatch);
    assert_eq!(rig.finger().await, Finger::Match);
    rig.stop().await;
    assert_eq!(
        rig.log.lines(),
        [
            "GetDefaultDevice",
            "Claim tester",
            "VerifyStart any",
            "VerifyStatus verify-no-match true",
            "VerifyStop",
            "VerifyStart any",
            "VerifyStatus verify-match true",
            "VerifyStop",
            "Release",
        ]
    );
}

/// A scan to retry keeps the verify going, with its line, and the ready
/// line says how this reader is used.
#[tokio::test]
async fn a_retry_keeps_the_verify_and_a_swipe_reader_says_swipe() {
    let Some(mut rig) = Rig::start(
        "swipe-too-short+finger-not-centered+remove-and-retry+retry-scan+match",
        "swipe",
        fake::Mode::Normal,
    )
    .await
    else {
        return;
    };
    assert_eq!(
        rig.finger().await,
        Finger::Ready {
            how: "Swipe a finger across the reader".to_owned()
        }
    );
    for line in [
        "Swipe too short; try again",
        "Finger not centred; try again",
        "Lift your finger and try again",
        "Try the scan again",
    ] {
        assert_eq!(rig.finger().await, Finger::Retry(line.to_owned()));
    }
    assert_eq!(rig.finger().await, Finger::Match);
    assert_eq!(rig.log.count("VerifyStart any"), 1, "{:?}", rig.log.lines());
    rig.stop().await;
}

#[tokio::test]
async fn no_reader_is_password_only() {
    let Some(mut rig) = Rig::start("", "press", fake::Mode::NoDevice).await else {
        return;
    };
    assert_eq!(
        rig.finger().await,
        Finger::Unavailable("no reader".to_owned())
    );
    rig.stop().await;
    assert_eq!(rig.log.lines(), ["GetDefaultDevice"]);
}

/// No enrolled prints is found at the first verify, after the claim; the
/// claim is released at once, and nothing is tried again.
#[tokio::test]
async fn no_enrolled_prints_is_password_only_and_releases_the_claim() {
    let Some(mut rig) = Rig::start("match", "press", fake::Mode::NoEnrolled).await else {
        return;
    };
    assert_eq!(
        rig.finger().await,
        Finger::Unavailable("no enrolled prints for tester".to_owned())
    );
    rig.logged("Release", 1).await;
    tokio::time::sleep(TIMINGS.first_backoff * 3).await;
    rig.stop().await;
    assert_eq!(rig.log.count("Claim tester"), 1, "{:?}", rig.log.lines());
}

#[tokio::test]
async fn no_fprintd_on_the_bus_is_password_only() {
    let Some(mut rig) = Rig::with(None).await else {
        return;
    };
    assert_eq!(
        rig.finger().await,
        Finger::Unavailable("no fprintd on the system bus".to_owned())
    );
    rig.stop().await;
}

/// A reader someone else holds is gone, and claimed again after the
/// backoff, for as long as the run lasts.
#[tokio::test]
async fn a_reader_in_use_is_claimed_again_after_the_backoff() {
    let Some(mut rig) = Rig::start("match", "press", fake::Mode::InUse).await else {
        return;
    };
    let started = tokio::time::Instant::now();
    assert!(matches!(rig.finger().await, Finger::Gone(_)));
    assert!(matches!(rig.finger().await, Finger::Gone(_)));
    assert!(
        started.elapsed() >= TIMINGS.first_backoff,
        "claimed again before the backoff"
    );
    rig.stop().await;
    assert!(rig.log.count("Claim tester") >= 2, "{:?}", rig.log.lines());
    assert_eq!(
        rig.log.count("Release"),
        0,
        "nothing was claimed to release"
    );
}

/// A run stopped while its verify waits for a finger stops the verify and
/// releases the reader: the password won.
#[tokio::test]
async fn a_stop_mid_verify_releases_the_reader() {
    let Some(mut rig) = Rig::start("", "press", fake::Mode::Normal).await else {
        return;
    };
    assert_eq!(rig.finger().await, ready_press());
    rig.logged("VerifyStart any", 1).await;
    rig.stop().await;
    let lines = rig.log.lines();
    assert_eq!(lines[lines.len() - 2..], ["VerifyStop", "Release"]);
}

/// fprintd leaving the bus mid-verify is the reader gone, and an fprintd
/// that is not back yet is claimed again rather than given up on; once it
/// is back, the run verifies again.
#[tokio::test]
async fn fprintd_leaving_mid_verify_is_gone_and_its_return_is_claimed() {
    let Some(mut rig) = Rig::start("", "press", fake::Mode::Normal).await else {
        return;
    };
    assert_eq!(rig.finger().await, ready_press());
    rig.logged("VerifyStart any", 1).await;
    drop(rig._fake.take());
    assert_eq!(
        rig.finger().await,
        Finger::Gone("fprintd left the bus".to_owned())
    );
    assert_eq!(
        rig.finger().await,
        Finger::Gone("no fprintd yet".to_owned())
    );
    let conn = zbus::connection::Builder::address(rig._bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("the fake's second connection");
    let log = fake::serve(
        &conn,
        fake::Script::parse(
            "match",
            Duration::from_millis(50),
            "press",
            fake::Mode::Normal,
        ),
    )
    .await
    .expect("the fake serves again");
    rig._fake = Some(conn);
    loop {
        match rig.finger().await {
            Finger::Gone(_) => continue,
            other => {
                assert_eq!(other, ready_press());
                break;
            }
        }
    }
    assert_eq!(rig.finger().await, Finger::Match);
    rig.stop().await;
    assert_eq!(log.lines().last().map(String::as_str), Some("Release"));
}

/// The proxy reads the properties under fprintd's own names, so the witness
/// names the reader rather than saying `(unnamed)`.
#[tokio::test]
async fn the_reader_is_named_from_fprintd_s_lowercase_properties() {
    let Some(rig) = Rig::start("", "swipe", fake::Mode::Normal).await else {
        return;
    };
    let conn = zbus::connection::Builder::address(rig._bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a client connection");
    let device = DeviceProxy::builder(&conn)
        .path(fake::DEVICE)
        .expect("the device path")
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .expect("the device proxy");
    assert_eq!(reader_name(&device).await, "Fake Fingerprint Reader");
    assert_eq!(device.scan_type().await.expect("scan-type"), "swipe");
    let mut rig = rig;
    rig.stop().await;
}

/// The next finding that is not a scan to try again, and when it came.
async fn next_gone(rig: &mut Rig) -> tokio::time::Instant {
    loop {
        match rig.finger().await {
            Finger::Retry(_) | Finger::Ready { .. } => continue,
            Finger::Gone(why) => {
                assert_eq!(why, "disconnected");
                return tokio::time::Instant::now();
            }
            other => panic!("not the reader gone: {other:?}"),
        }
    }
}

/// A verify that is disconnected moments after it started (fprintd's word
/// for a sensor too hot, as for one unplugged) did not live: the next claim
/// waits the doubled backoff, never the first pause again.
#[tokio::test]
async fn a_disconnect_right_after_the_start_doubles_the_backoff() {
    let Some(mut rig) = Rig::start(
        "disconnected,disconnected,disconnected",
        "press",
        fake::Mode::Normal,
    )
    .await
    else {
        return;
    };
    let first = next_gone(&mut rig).await;
    let second = next_gone(&mut rig).await;
    let third = next_gone(&mut rig).await;
    // Each gap is the backoff, a claim and the fake's 50 ms to the status.
    assert!(
        second - first >= TIMINGS.first_backoff,
        "the first pause: {:?}",
        second - first
    );
    assert!(
        third - second >= TIMINGS.first_backoff * 2,
        "the second claim came after {:?}, not the doubled backoff",
        third - second
    );
    rig.stop().await;
    assert_eq!(rig.log.count("Claim tester"), 3, "{:?}", rig.log.lines());
}

/// A verify that was live past `settled` before it was disconnected resets
/// the backoff: the reader really went, and is claimed again at the first
/// pause.
#[tokio::test]
async fn a_verify_that_lived_resets_the_backoff() {
    // Two quick disconnects take the backoff to its longest; then a verify
    // that lives eight retries (400 ms, past the 300 ms of `settled`)
    // before it goes; then a quick one again.
    let lived = ["retry-scan"; 8].join("+") + "+disconnected";
    let script = format!("disconnected,disconnected,{lived},disconnected");
    let Some(mut rig) = Rig::start(&script, "press", fake::Mode::Normal).await else {
        return;
    };
    next_gone(&mut rig).await;
    let second = next_gone(&mut rig).await;
    let third = next_gone(&mut rig).await;
    let fourth = next_gone(&mut rig).await;
    assert!(
        third - second >= TIMINGS.first_backoff * 2 + Duration::from_millis(400),
        "the verify that lived: {:?}",
        third - second
    );
    assert!(
        fourth - third < TIMINGS.last_backoff,
        "the claim after a verify that lived came after {:?}, not the first pause",
        fourth - third
    );
    rig.stop().await;
}
