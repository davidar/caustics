use super::*;

const LOCKER: &str = "caustics-lock";
const OTHER: &str = "caustics-polkit";

/// The protocol and the host, faked: what `wayland.rs` would do with each
/// effect, recorded. `lock` and `unlock_and_destroy` are calls on a fake
/// compositor that holds the lock or not, and that only says `locked` when
/// the test makes it.
#[derive(Debug, Default)]
struct Fake {
    /// The compositor has `ext_session_lock_manager_v1`.
    no_manager: bool,
    /// A lock object is out, and whether the compositor said locked on it.
    lock_object: bool,
    compositor_locked: bool,
    /// Lock panes up.
    panes: bool,
    /// A sync is out after an unlock.
    sync_out: bool,
    calls: Vec<&'static str>,
    told: Vec<(String, u32)>,
    dim_up: bool,
    reserved: Option<String>,
    refused: Vec<String>,
    fatal: Option<String>,
}

impl Fake {
    fn new() -> Fake {
        Fake::default()
    }

    /// Apply effects as the host would, feeding the lock's answers back in.
    fn apply(&mut self, locks: &mut Locks, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Lock => {
                    assert!(!self.lock_object, "a second lock object while one is out");
                    if self.no_manager {
                        self.calls.push("lock refused");
                        let more = locks.refused();
                        self.apply(locks, more);
                    } else {
                        self.calls.push("lock");
                        self.lock_object = true;
                        self.panes = true;
                    }
                }
                Effect::Unlock => {
                    assert!(
                        self.compositor_locked,
                        "unlock_and_destroy before locked is a protocol error"
                    );
                    self.calls.push("unlock_and_destroy");
                    self.lock_object = false;
                    self.compositor_locked = false;
                    self.panes = false;
                    self.sync_out = true;
                }
                Effect::Tell { sidecar, event } => self.told.push((sidecar, event)),
                Effect::EndDim => self.dim_up = false,
                Effect::Reserve(who) => self.reserved = who,
                Effect::Note(_) => {}
                Effect::Refused(why) => self.refused.push(why),
                Effect::Fatal(why) => self.fatal = Some(why),
            }
        }
    }

    /// The compositor says `locked` on the lock object out.
    fn compositor_locks(&mut self, locks: &mut Locks) {
        assert!(self.lock_object, "locked with no lock object");
        self.compositor_locked = true;
        let effects = locks.locked();
        self.apply(locks, effects);
    }

    /// The sync after the unlock comes back.
    fn sync_answers(&mut self, locks: &mut Locks) {
        assert!(self.sync_out, "no sync out");
        self.sync_out = false;
        let effects = locks.synced();
        self.apply(locks, effects);
    }

    fn ask(&mut self, locks: &mut Locks, sidecar: &str, command: u32) {
        let effects = locks.request(sidecar, command);
        self.apply(locks, effects);
    }
}

/// The whole round: unlocked, locking (the lock object and its panes, the
/// prompt reserved), locked (told), unlocking (the object and the panes
/// gone), unlocked (told after the sync, the reservation over).
#[test]
fn a_lock_goes_round_and_each_step_is_told() {
    let mut locks = Locks::new();
    let mut fake = Fake::new();
    assert_eq!(locks.phase(), Phase::Unlocked);
    assert!(!locks.held());

    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    assert_eq!(locks.phase(), Phase::Locking);
    assert!(locks.held(), "the lock panes are up from the request");
    assert_eq!(fake.calls, ["lock"]);
    assert!(fake.panes);
    assert_eq!(fake.reserved.as_deref(), Some(LOCKER));
    assert!(
        fake.told.is_empty(),
        "nothing is told before the compositor says"
    );

    fake.compositor_locks(&mut locks);
    assert_eq!(locks.phase(), Phase::Locked);
    assert_eq!(fake.told, [(LOCKER.to_owned(), CANVAS_EVENT_LOCKED)]);

    fake.ask(&mut locks, LOCKER, LOCK_UNLOCK);
    assert_eq!(locks.phase(), Phase::Unlocking);
    assert!(!locks.held(), "the lock panes go with the unlock");
    assert_eq!(fake.calls, ["lock", "unlock_and_destroy"]);
    assert!(!fake.panes);
    assert_eq!(fake.told.len(), 1, "unlocked waits for the sync");

    fake.sync_answers(&mut locks);
    assert_eq!(locks.phase(), Phase::Unlocked);
    assert_eq!(
        fake.told,
        [
            (LOCKER.to_owned(), CANVAS_EVENT_LOCKED),
            (LOCKER.to_owned(), CANVAS_EVENT_UNLOCKED)
        ]
    );
    assert_eq!(fake.reserved, None, "the prompt is anyone's again");
    assert!(fake.refused.is_empty() && fake.fatal.is_none());
}

/// A second lock while locking changes nothing and tells nobody; while
/// locked it changes nothing but tells the asker locked again, which is what
/// a restarted lock sidecar waits for before it asks for a secret.
#[test]
fn a_second_lock_is_no_change() {
    let mut locks = Locks::new();
    let mut fake = Fake::new();
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    assert_eq!(fake.calls, ["lock"], "one lock object");
    assert!(fake.told.is_empty());
    fake.compositor_locks(&mut locks);
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    assert_eq!(fake.calls, ["lock"]);
    assert_eq!(locks.phase(), Phase::Locked);
    assert_eq!(
        fake.told,
        [
            (LOCKER.to_owned(), CANVAS_EVENT_LOCKED),
            (LOCKER.to_owned(), CANVAS_EVENT_LOCKED)
        ]
    );
    assert!(fake.refused.is_empty());
}

/// Only the sidecar that locked unlocks: another's unlock is a witness and
/// the session stays locked; the owner's goes through.
#[test]
fn an_unlock_from_another_sidecar_is_refused() {
    let mut locks = Locks::new();
    let mut fake = Fake::new();
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    fake.compositor_locks(&mut locks);
    fake.ask(&mut locks, OTHER, LOCK_UNLOCK);
    assert_eq!(locks.phase(), Phase::Locked);
    assert_eq!(fake.calls, ["lock"]);
    assert_eq!(fake.refused.len(), 1);
    assert!(fake.refused[0].contains(OTHER), "{:?}", fake.refused);
    fake.ask(&mut locks, LOCKER, LOCK_UNLOCK);
    assert_eq!(locks.phase(), Phase::Unlocking);
}

/// An unlock while unlocked is a witness; one before `locked` is refused, since
/// `unlock_and_destroy` then is a protocol error, and the lock goes on.
#[test]
fn an_unlock_with_nothing_to_unlock_is_a_witness() {
    let mut locks = Locks::new();
    let mut fake = Fake::new();
    fake.ask(&mut locks, LOCKER, LOCK_UNLOCK);
    assert_eq!(locks.phase(), Phase::Unlocked);
    assert_eq!(fake.refused.len(), 1);
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    fake.ask(&mut locks, LOCKER, LOCK_UNLOCK);
    assert_eq!(locks.phase(), Phase::Locking);
    assert_eq!(fake.refused.len(), 2);
    fake.compositor_locks(&mut locks);
    assert_eq!(fake.told, [(LOCKER.to_owned(), CANVAS_EVENT_LOCKED)]);
}

/// `finished` is fatal whenever it comes: while locking (another locker
/// holds the lock) and while locked (the compositor ended it).
#[test]
fn finished_is_fatal() {
    for after_locked in [false, true] {
        let mut locks = Locks::new();
        let mut fake = Fake::new();
        fake.ask(&mut locks, LOCKER, LOCK_LOCK);
        if after_locked {
            fake.compositor_locks(&mut locks);
        }
        let effects = locks.finished();
        fake.apply(&mut locks, effects);
        let why = fake.fatal.as_deref().expect("finished is fatal");
        assert!(why.contains(LOCKER), "{why}");
        assert!(
            !fake.calls.contains(&"unlock_and_destroy"),
            "finished never unlocks"
        );
    }
}

/// A dim up when the lock is asked for is ended, and nobody is told.
#[test]
fn a_dim_up_at_lock_time_is_ended() {
    let mut locks = Locks::new();
    let mut fake = Fake {
        dim_up: true,
        ..Fake::new()
    };
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    assert!(!fake.dim_up);
    assert!(fake.told.is_empty(), "the dim's sidecar is told nothing");
}

/// A lock asked for while unlocking waits for the unlock's sync, then locks
/// afresh: the owner hears unlocked, then locked.
#[test]
fn a_lock_while_unlocking_follows_the_unlock() {
    let mut locks = Locks::new();
    let mut fake = Fake::new();
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    fake.compositor_locks(&mut locks);
    fake.ask(&mut locks, LOCKER, LOCK_UNLOCK);
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    assert_eq!(locks.phase(), Phase::Unlocking);
    fake.sync_answers(&mut locks);
    assert_eq!(locks.phase(), Phase::Locking);
    assert_eq!(fake.calls, ["lock", "unlock_and_destroy", "lock"]);
    assert_eq!(fake.reserved.as_deref(), Some(LOCKER));
    fake.compositor_locks(&mut locks);
    assert_eq!(
        fake.told,
        [
            (LOCKER.to_owned(), CANVAS_EVENT_LOCKED),
            (LOCKER.to_owned(), CANVAS_EVENT_UNLOCKED),
            (LOCKER.to_owned(), CANVAS_EVENT_LOCKED)
        ]
    );
}

/// The owner's pipe closing leaves the lock as it is; its restart, under the
/// same name, re-locks (told locked again) and may unlock.
#[test]
fn the_owner_going_leaves_the_lock() {
    let mut locks = Locks::new();
    let mut fake = Fake::new();
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    fake.compositor_locks(&mut locks);
    let effects = locks.gone(LOCKER);
    assert!(
        effects.iter().all(|e| matches!(e, Effect::Note(_))),
        "{effects:?}"
    );
    fake.apply(&mut locks, effects);
    assert_eq!(locks.phase(), Phase::Locked);
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    assert_eq!(fake.told.len(), 2);
    fake.ask(&mut locks, LOCKER, LOCK_UNLOCK);
    assert_eq!(locks.phase(), Phase::Unlocking);
}

/// A compositor without the lock manager: the lock is refused with a
/// witness, the host stays unlocked and the prompt unreserved.
#[test]
fn no_lock_manager_refuses_the_lock() {
    let mut locks = Locks::new();
    let mut fake = Fake {
        no_manager: true,
        ..Fake::new()
    };
    fake.ask(&mut locks, LOCKER, LOCK_LOCK);
    assert_eq!(locks.phase(), Phase::Unlocked);
    assert_eq!(fake.calls, ["lock refused"]);
    assert_eq!(fake.reserved, None);
    assert_eq!(fake.refused.len(), 1);
    assert!(fake.fatal.is_none());
}

/// A `locked` the host did not ask for, or a sync with no unlock, changes
/// nothing.
#[test]
fn stray_events_change_nothing() {
    let mut locks = Locks::new();
    assert!(matches!(locks.locked().as_slice(), [Effect::Refused(_)]));
    assert!(locks.synced().is_empty());
    assert_eq!(locks.phase(), Phase::Unlocked);
}
