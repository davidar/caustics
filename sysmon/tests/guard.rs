//! The lease over the NVML handle, driven by fakes and a fake clock.
//!
//! The property that matters is negative: *NVML is never opened while the
//! kernel does not already say the device is `active`*. So the fake port
//! records every open that happened against a shut gate, and every test
//! asserts that count is zero. The rest is the letting-go half: a device that
//! goes unused for `RELEASE_AFTER` gets its handle back, and the rest that
//! follows keeps the handle closed until the device has either suspended or
//! shown that something else is keeping it awake.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use caustics_sysmon::GAP;
use caustics_sysmon::nvidia::{
    DEFAULT_AUTOSUSPEND, Gate, Lease, Observation, Port, RELEASE_AFTER, REST_CAP, REST_MARGIN,
    Standing, rest_limit,
};

/// Everything the two fakes share, and everything a test asserts on.
#[derive(Default)]
struct Shared {
    /// What the gate answers.
    status: Option<String>,
    /// Calls to `Port::open`.
    opens: u32,
    /// Calls to `Port::close`.
    closes: u32,
    /// Opens that happened while the gate was *not* saying `active`. This must
    /// be zero in every test: it is the never-wake guarantee.
    opens_against_a_shut_gate: u32,
    /// What a look reports.
    clients: Option<u32>,
    busy: f32,
    /// Whether `open` fails, standing in for a missing driver or library.
    init_fails: bool,
}

type State = Arc<Mutex<Shared>>;

struct FakePort {
    state: State,
    open: bool,
}

impl Port for FakePort {
    fn open(&mut self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.opens += 1;
        if state.status.as_deref() != Some("active") {
            state.opens_against_a_shut_gate += 1;
        }
        if state.init_fails {
            return Err("no driver".into());
        }
        self.open = true;
        Ok(())
    }

    fn close(&mut self) {
        self.state.lock().unwrap().closes += 1;
        self.open = false;
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn observe(&mut self) -> Option<Observation> {
        let state = self.state.lock().unwrap();
        Some(Observation {
            busy: Some(state.busy),
            clients: state.clients,
        })
    }
}

struct FakeGate {
    state: State,
}

impl Gate for FakeGate {
    fn status(&mut self) -> Option<String> {
        self.state.lock().unwrap().status.clone()
    }
}

/// A lease over the fakes with an unreadable `autosuspend_delay_ms`, so the
/// default delay applies.
fn lease(state: &State) -> Lease {
    Lease::new(
        Box::new(FakePort {
            state: Arc::clone(state),
            open: false,
        }),
        Box::new(FakeGate {
            state: Arc::clone(state),
        }),
        None,
    )
}

/// An active, unused device: the gate open, nobody on the card.
fn active_and_unused() -> State {
    let state: State = Arc::default();
    {
        let mut s = state.lock().unwrap();
        s.status = Some("active".into());
        s.clients = Some(0);
        s.busy = 0.0;
    }
    state
}

const SECOND: Duration = Duration::from_secs(1);

fn quiet(_: &str) {}

/// The whole point: a gate that does not say `active` is never opened
/// through, and neither is one that cannot be read.
#[test]
fn nvml_is_never_opened_while_the_gate_is_shut() {
    for status in [
        Some("suspended"),
        Some("suspending"),
        Some("resuming"),
        Some("error"),
        None,
    ] {
        let state: State = Arc::default();
        state.lock().unwrap().status = status.map(str::to_owned);
        let mut lease = lease(&state);
        for _ in 0..20 {
            assert_eq!(
                lease.sample(SECOND, &mut quiet),
                GAP,
                "{status:?} must sample as a gap"
            );
        }
        assert_eq!(lease.standing(), Standing::Waiting);
        let state = state.lock().unwrap();
        assert_eq!(state.opens, 0, "{status:?} opened NVML");
        assert_eq!(state.opens_against_a_shut_gate, 0);
    }
}

/// An unused device gets its handle back after `RELEASE_AFTER`, and the rest
/// then keeps the handle closed while the card is still `active`, for as long
/// as the card needs to suspend. Only once that time has passed with the card
/// still active is it taken to be in use and the lease retaken.
#[test]
fn an_unused_device_is_released_and_rested_before_the_lease_is_retaken() {
    let state = active_and_unused();
    let mut lease = lease(&state);
    let limit = rest_limit(None);
    assert_eq!(limit, DEFAULT_AUTOSUSPEND + REST_MARGIN);

    // Taken on the first sample, held while the unused clock runs up.
    assert_eq!(lease.sample(SECOND, &mut quiet), 0.0);
    assert_eq!(
        lease.standing(),
        Standing::Held {
            unused: Duration::ZERO
        }
    );
    assert_eq!(lease.sample(SECOND, &mut quiet), 0.0);
    assert_eq!(lease.sample(SECOND, &mut quiet), 0.0);
    assert_eq!(
        lease.sample(SECOND, &mut quiet),
        0.0,
        "the sample that releases still reads the handle"
    );
    {
        let s = state.lock().unwrap();
        assert_eq!(s.opens, 1, "opened once");
        assert_eq!(
            s.closes, 1,
            "and released once the device had gone RELEASE_AFTER unused"
        );
    }
    assert!(matches!(lease.standing(), Standing::Resting { .. }));

    // The rest: gaps, and no open, for as long as the card is allowed to take.
    let mut rested = Duration::ZERO;
    while rested + SECOND < limit {
        assert_eq!(
            lease.sample(SECOND, &mut quiet),
            GAP,
            "resting at {rested:?}"
        );
        assert_eq!(
            state.lock().unwrap().opens,
            1,
            "the rest reopened NVML at {rested:?}"
        );
        rested += SECOND;
    }

    // Past the limit with the card still active: something else has it, and
    // the lease is taken again.
    assert_eq!(lease.sample(SECOND, &mut quiet), 0.0);
    let s = state.lock().unwrap();
    assert_eq!(s.opens, 2);
    assert_eq!(s.opens_against_a_shut_gate, 0);
}

/// The device suspending during the rest is the ordinary case: the rest ends
/// there and then, and the gate, now shut, keeps the handle closed.
#[test]
fn a_device_that_suspends_during_the_rest_stays_closed() {
    let state = active_and_unused();
    let mut lease = lease(&state);
    for _ in 0..4 {
        lease.sample(SECOND, &mut quiet);
    }
    assert_eq!(state.lock().unwrap().closes, 1);

    // Runtime PM does its job one second into the rest.
    assert_eq!(lease.sample(SECOND, &mut quiet), GAP);
    state.lock().unwrap().status = Some("suspended".into());
    assert_eq!(lease.sample(SECOND, &mut quiet), GAP);
    assert_eq!(
        lease.standing(),
        Standing::Waiting,
        "the rest ended when the card suspended"
    );
    for _ in 0..200 {
        assert_eq!(lease.sample(SECOND, &mut quiet), GAP);
    }
    let s = state.lock().unwrap();
    assert_eq!(s.opens, 1, "the shut gate held");
    assert_eq!(s.opens_against_a_shut_gate, 0);
}

/// A compositor or a browser on the card keeps the client count up, and the
/// handle simply stays held: the card is awake anyway.
#[test]
fn a_device_with_clients_keeps_its_handle() {
    let state = active_and_unused();
    {
        let mut s = state.lock().unwrap();
        s.clients = Some(2);
        s.busy = 37.0;
    }
    let mut lease = lease(&state);
    for _ in 0..50 {
        assert_eq!(lease.sample(SECOND, &mut quiet), 37.0);
    }
    let s = state.lock().unwrap();
    assert_eq!(s.opens, 1, "one open for the whole run");
    assert_eq!(
        s.closes, 0,
        "and never released while something is using it"
    );
}

/// The unused clock restarts whenever a client appears: it is `RELEASE_AFTER`
/// of *continuous* disuse that releases, not that much in total.
#[test]
fn a_client_restarts_the_unused_clock() {
    let state = active_and_unused();
    let mut lease = lease(&state);
    lease.sample(SECOND, &mut quiet);
    lease.sample(SECOND, &mut quiet);
    state.lock().unwrap().clients = Some(1);
    lease.sample(SECOND, &mut quiet);
    assert_eq!(
        lease.standing(),
        Standing::Held {
            unused: Duration::ZERO
        }
    );
    state.lock().unwrap().clients = Some(0);
    lease.sample(SECOND, &mut quiet);
    lease.sample(SECOND, &mut quiet);
    assert_eq!(state.lock().unwrap().closes, 0, "the clock restarted");
    lease.sample(SECOND, &mut quiet);
    assert_eq!(
        state.lock().unwrap().closes,
        1,
        "and then RELEASE_AFTER of disuse released it"
    );
    assert_eq!(RELEASE_AFTER, Duration::from_secs(3));
}

/// When NVML cannot count clients, busyness stands in for them.
#[test]
fn busyness_stands_in_for_an_unknown_client_count() {
    let state = active_and_unused();
    {
        let mut s = state.lock().unwrap();
        s.clients = None;
        s.busy = 12.0;
    }
    let mut lease = lease(&state);
    for _ in 0..20 {
        assert_eq!(lease.sample(SECOND, &mut quiet), 12.0);
    }
    assert_eq!(state.lock().unwrap().closes, 0, "a busy card is in use");
    state.lock().unwrap().busy = 0.0;
    for _ in 0..4 {
        lease.sample(SECOND, &mut quiet);
    }
    assert_eq!(
        state.lock().unwrap().closes,
        1,
        "an idle card with no known clients is unused"
    );
}

/// An init that fails while the gate says `active` is gaps for the rest of
/// the run, and is never retried: no driver does not become a driver.
#[test]
fn a_failed_init_is_permanent_and_said_once() {
    let state = active_and_unused();
    state.lock().unwrap().init_fails = true;
    let lines = std::cell::RefCell::new(Vec::new());
    let mut log = |line: &str| lines.borrow_mut().push(line.to_owned());
    let mut lease = lease(&state);
    for _ in 0..30 {
        assert_eq!(lease.sample(SECOND, &mut log), GAP);
    }
    assert_eq!(lease.standing(), Standing::Refused);
    assert_eq!(state.lock().unwrap().opens, 1, "tried exactly once");
    let lines = lines.into_inner();
    assert_eq!(lines.len(), 1, "and said so exactly once: {lines:?}");
    assert!(lines[0].contains("init failed"), "{lines:?}");
}

/// The rest's bound: the device's own delay plus the margin, the default
/// delay when the file is unreadable, and a cap.
#[test]
fn the_rest_is_bounded_by_the_autosuspend_delay_plus_a_margin() {
    assert_eq!(rest_limit(None), Duration::from_millis(8000));
    assert_eq!(rest_limit(Some(Duration::ZERO)), REST_MARGIN);
    assert_eq!(rest_limit(Some(Duration::from_secs(10_000))), REST_CAP);
}

/// Each change of standing is one line; a settled standing is silence.
#[test]
fn the_witnesses_are_one_per_change() {
    let state = active_and_unused();
    state.lock().unwrap().status = Some("suspended".into());
    let lines = std::cell::RefCell::new(Vec::new());
    let mut log = |line: &str| lines.borrow_mut().push(line.to_owned());
    let mut lease = lease(&state);

    for _ in 0..10 {
        lease.sample(SECOND, &mut log);
    }
    assert_eq!(
        lines.borrow().len(),
        1,
        "ten suspended samples, one line: {lines:?}"
    );

    state.lock().unwrap().status = Some("active".into());
    for _ in 0..4 {
        lease.sample(SECOND, &mut log);
    }
    let lines = lines.into_inner();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[1].contains("opened"), "{lines:?}");
    assert!(lines[2].contains("released after 3 s"), "{lines:?}");
}
