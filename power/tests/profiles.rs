//! The power modes against a fake
//! power-profiles daemon on a private bus: the set and the active one read, a
//! click's `ActiveProfile` set, the daemon appearing and vanishing under a
//! running watch, and the new name `org.freedesktop.UPower.PowerProfiles`
//! asked before the old `net.hadess.PowerProfiles`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use caustics_power::profiles::{self, Endpoint, HADESS, Profiles, UPOWER};
use caustics_sidecar::test_bus::Bus;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

/// The daemon's state: one active profile, and every set recorded by the name
/// it came in on.
#[derive(Clone)]
struct State {
    active: Arc<Mutex<String>>,
    sets: Arc<Mutex<Vec<(&'static str, String)>>>,
}

fn profile(name: &str) -> HashMap<String, OwnedValue> {
    HashMap::from([
        (
            "Profile".to_owned(),
            OwnedValue::try_from(Value::from(name)).unwrap(),
        ),
        (
            "Driver".to_owned(),
            OwnedValue::try_from(Value::from("tuned")).unwrap(),
        ),
    ])
}

fn three_profiles() -> Vec<HashMap<String, OwnedValue>> {
    vec![
        profile("power-saver"),
        profile("balanced"),
        profile("performance"),
    ]
}

/// The same daemon under each of its two interface names, as tuned-ppd and
/// power-profiles-daemon 0.20+ export it.
struct Hadess(State);
struct Upower(State);

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl Hadess {
    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        three_profiles()
    }
    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.0.active.lock().unwrap().clone()
    }
    #[zbus(property)]
    fn set_active_profile(&mut self, profile: String) {
        self.0
            .sets
            .lock()
            .unwrap()
            .push(("hadess", profile.clone()));
        *self.0.active.lock().unwrap() = profile;
    }
}

#[zbus::interface(name = "org.freedesktop.UPower.PowerProfiles")]
impl Upower {
    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        three_profiles()
    }
    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.0.active.lock().unwrap().clone()
    }
    #[zbus(property)]
    fn set_active_profile(&mut self, profile: String) {
        self.0
            .sets
            .lock()
            .unwrap()
            .push(("upower", profile.clone()));
        *self.0.active.lock().unwrap() = profile;
    }
}

async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .expect("the bus address parses")
        .build()
        .await
        .expect("a connection to the private bus")
}

/// A peer serving the daemon's object under each of `names`, not yet owning
/// any name.
async fn daemon(bus: &Bus, active: &str, names: &[Endpoint]) -> (Connection, State) {
    let peer = connect(bus).await;
    let state = State {
        active: Arc::new(Mutex::new(active.to_owned())),
        sets: Arc::new(Mutex::new(Vec::new())),
    };
    for at in names {
        let served = if *at == UPOWER {
            peer.object_server()
                .at(at.path, Upower(state.clone()))
                .await
        } else {
            peer.object_server()
                .at(at.path, Hadess(state.clone()))
                .await
        };
        served.expect("the daemon's object");
    }
    (peer, state)
}

async fn next(seen: &mut UnboundedReceiver<Option<Profiles>>) -> Option<Profiles> {
    tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the watch said something within 5 s")
        .expect("the watch is still running")
}

/// Read until the watch says `want`, skipping repeats (the loop drops them).
async fn until(seen: &mut UnboundedReceiver<Option<Profiles>>, want: Option<&Profiles>) {
    loop {
        if next(seen).await.as_ref() == want {
            return;
        }
    }
}

fn three(active: &str) -> Profiles {
    Profiles {
        names: vec![
            "power-saver".into(),
            "balanced".into(),
            "performance".into(),
        ],
        active: active.into(),
    }
}

#[tokio::test]
async fn the_set_and_the_active_one_are_read() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the profiles are untested here");
        return;
    };
    let (peer, _) = daemon(&bus, "balanced", &[HADESS]).await;
    peer.request_name(HADESS.service)
        .await
        .expect("the peer owns the name");
    let client = connect(&bus).await;
    assert_eq!(
        profiles::read_from(&client, HADESS).await,
        Some(three("balanced"))
    );
    // Only the old name is owned: the new one is skipped, the old one read.
    assert_eq!(
        profiles::read(&client, false).await,
        Some((three("balanced"), HADESS))
    );
    assert_eq!(profiles::read_from(&client, UPOWER).await, None);
}

#[tokio::test]
async fn the_new_name_is_asked_first_and_the_old_one_is_the_fallback() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the names are untested here");
        return;
    };
    assert_eq!(profiles::ENDPOINTS, [UPOWER, HADESS]);
    assert_eq!(UPOWER.service, "org.freedesktop.UPower.PowerProfiles");
    assert_eq!(UPOWER.path, "/org/freedesktop/UPower/PowerProfiles");
    // Both names, as tuned-ppd owns them.
    let (peer, state) = daemon(&bus, "balanced", &[UPOWER, HADESS]).await;
    peer.request_name(UPOWER.service)
        .await
        .expect("the new name");
    peer.request_name(HADESS.service)
        .await
        .expect("the old name");
    let client = connect(&bus).await;
    assert_eq!(
        profiles::read(&client, false).await,
        Some((three("balanced"), UPOWER))
    );
    profiles::set_active(&client, "power-saver")
        .await
        .expect("the set is answered");
    assert_eq!(
        *state.sets.lock().unwrap(),
        vec![("upower", "power-saver".to_owned())]
    );

    // The new name leaves: the old one is read and set.
    peer.release_name(UPOWER.service)
        .await
        .expect("the peer lets go");
    assert_eq!(
        profiles::read(&client, false).await,
        Some((three("power-saver"), HADESS))
    );
    profiles::set_active(&client, "performance")
        .await
        .expect("the set is answered");
    assert_eq!(
        state.sets.lock().unwrap().last(),
        Some(&("hadess", "performance".to_owned()))
    );

    // Neither: no daemon, and a click says so.
    peer.release_name(HADESS.service)
        .await
        .expect("the peer lets go");
    assert_eq!(profiles::read(&client, false).await, None);
    let refused = profiles::set_active(&client, "balanced").await;
    assert_eq!(
        refused,
        Err("no power-profiles daemon on the bus".to_owned())
    );
}

#[tokio::test]
async fn a_click_sets_active_profile_and_the_rows_follow() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; the set is untested here");
        return;
    };
    let (peer, state) = daemon(&bus, "balanced", &[HADESS]).await;
    peer.request_name(HADESS.service)
        .await
        .expect("the peer owns the name");
    let client = connect(&bus).await;

    let (tx, mut seen) = unbounded_channel();
    let task = tokio::spawn(profiles::watch(client.clone(), tx));
    until(&mut seen, Some(&three("balanced"))).await;

    profiles::set_active(&client, "performance")
        .await
        .expect("the set is answered");
    assert_eq!(
        *state.sets.lock().unwrap(),
        vec![("hadess", "performance".to_owned())]
    );
    // The daemon's PropertiesChanged wakes the watch, which reads again.
    until(&mut seen, Some(&three("performance"))).await;
    task.abort();
}

#[tokio::test]
async fn a_daemon_that_appears_and_vanishes_is_seen_both_ways() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon to start; NameOwnerChanged is untested here");
        return;
    };
    let client = connect(&bus).await;
    let (tx, mut seen) = unbounded_channel();
    let task = tokio::spawn(profiles::watch(client, tx));

    // Nobody owns either name: no rows.
    assert_eq!(next(&mut seen).await, None);

    let (peer, _) = daemon(&bus, "power-saver", &[HADESS, UPOWER]).await;
    peer.request_name(HADESS.service)
        .await
        .expect("the peer owns the old name");
    until(&mut seen, Some(&three("power-saver"))).await;
    // The new name arriving and the old one leaving: still there, on the
    // other name.
    peer.request_name(UPOWER.service)
        .await
        .expect("the peer owns the new name");
    peer.release_name(HADESS.service)
        .await
        .expect("the peer lets the old one go");
    until(&mut seen, Some(&three("power-saver"))).await;

    peer.release_name(UPOWER.service)
        .await
        .expect("the peer lets go");
    until(&mut seen, None).await;
    task.abort();
}
