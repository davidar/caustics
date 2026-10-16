use super::*;
use smithay_client_toolkit::reexports::calloop::channel::channel;

/// An inert proxy: a real type, a null object id, and a backend that is
/// already gone — so every request on it returns without touching a
/// socket. What lets the state machine be tested with no compositor.
///
/// `Proxy::inert` wants a `WeakBackend`, and the only way to make one is
/// from a `Backend`; a socketpair gives one that is never written to, and
/// dropping it at the end of this function is what makes the proxy inert.
fn inert<T: wayland_client::Proxy>() -> T {
    let (stream, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let backend = wayland_client::backend::Backend::connect(stream).expect("the rust backend");
    T::inert(backend.downgrade())
}

/// Stands in for the queue handle: the inhibitor a click would create.
struct InertQueue;

impl InhibitQueue for InertQueue {
    fn create_inhibitor(&self, _: &ZwpIdleInhibitManagerV1, _: &WlSurface) -> ZwpIdleInhibitorV1 {
        inert()
    }
}

fn drain(rx: &smithay_client_toolkit::reexports::calloop::channel::Channel<Update>) -> Vec<Update> {
    let mut seen = Vec::new();
    while let Ok(update) = rx.try_recv() {
        seen.push(update);
    }
    seen
}

/// Absent is absorbing: a toggle on a compositor without the global
/// changes nothing, sends the same state back, and counts no toggle.
#[test]
fn absent_stays_absent_and_counts_nothing() {
    let (tx, rx) = channel::<Update>();
    let mut caffeine = Caffeine::new(None, inert(), Box::new(InertQueue), tx);
    assert!(!caffeine.is_bound());
    assert_eq!(caffeine.state(), State::Absent);
    assert_eq!(caffeine.toggle(), State::Absent);
    assert_eq!(caffeine.toggles(), 0);
    caffeine.perform();
    // The one update is the `perform`'s own: `new` sends nothing when
    // there is no manager, because the model's default already says so.
    assert_eq!(drain(&rx), vec![Update::Caffeine(State::Absent)]);
}

/// Bound: off at startup, always, and every flip is one update and one
/// more toggle on the health line.
#[test]
fn bound_starts_off_and_flips() {
    let (tx, rx) = channel::<Update>();
    let mut caffeine = Caffeine::new(Some(inert()), inert(), Box::new(InertQueue), tx);
    assert!(caffeine.is_bound());
    assert_eq!(
        caffeine.state(),
        State::Off,
        "an inhibitor is never persisted"
    );
    assert_eq!(caffeine.toggle(), State::On);
    assert_eq!(
        caffeine.state(),
        State::On,
        "the live object *is* the state"
    );
    assert_eq!(caffeine.toggle(), State::Off);
    assert_eq!(caffeine.toggles(), 2);
    caffeine.perform();
    assert_eq!(caffeine.state(), State::On);
    assert_eq!(
        drain(&rx),
        vec![Update::Caffeine(State::Off), Update::Caffeine(State::On)],
        "bind says Off once, and `perform` says what it left behind"
    );
}
