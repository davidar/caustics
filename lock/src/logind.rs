//! logind on the **system** bus: the session's `Lock` and `Unlock` signals in,
//! the Manager's `PrepareForSleep` in on both edges (`true` as the machine
//! going to sleep, when the lock lets the fingerprint reader go and claims
//! nothing; `false` as a resume, when the person is back and the lock opens
//! a fingerprint window), its `LockedHint` read once at start and set on the
//! host's word.
//!
//! The session is the one `caustics_sidecar::session` finds, as the polkit
//! sink finds its own. The signal streams are subscribed **before** the
//! hint is read, so a `Lock` raised between the two is not lost, and each is
//! forwarded the moment it exists (docs/architecture.md, *Rules*).
//! `SetLockedHint` is called on a task of its own, in the order the
//! conductor asked, so the conductor never waits on the bus.
//!
//! Without a system bus, or without a session there, there is **no logind**:
//! one witness, no signals (and so no resume), no hint, and the socket's
//! `lock` is the only trigger. That is the tests' case: they point `DBUS_SYSTEM_BUS_ADDRESS` at
//! a socket that does not exist.

use caustics_sidecar::Drain;
use caustics_sidecar::session::{self, ManagerProxy, PrepareForSleep, SessionProxy};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use zbus::Connection;

use crate::conductor::Event;
use crate::witness;

/// logind, found and subscribed to.
pub struct Logind {
    /// What `SetLockedHint` is called with, in order.
    pub hint: UnboundedSender<bool>,
    /// `LockedHint` when this process started.
    pub locked: bool,
    _streams: [Drain; 3],
    _hint_task: tokio::task::JoinHandle<()>,
}

/// logind for this session, with its signals forwarded into `events`; or
/// `None`, witnessed, when there is none.
pub async fn connect(events: &UnboundedSender<Event>) -> Option<Logind> {
    let conn = match Connection::system().await {
        Ok(conn) => conn,
        Err(e) => {
            witness(&format!(
                "no logind (the system bus: {e}); the socket's lock is the only trigger"
            ));
            return None;
        }
    };
    let (id, how) = match session::session_id(&conn).await {
        Ok(found) => found,
        Err(e) => {
            witness(&format!(
                "no logind session to lock ({e}); the socket's lock is the only trigger"
            ));
            return None;
        }
    };
    let proxy = match session::session(&conn, &id).await {
        Ok(proxy) => proxy,
        Err(e) => {
            witness(&format!(
                "no logind session {id} ({e}); the socket's lock is the only trigger"
            ));
            return None;
        }
    };
    let streams = match subscribe(&conn, &proxy, events).await {
        Ok(streams) => streams,
        Err(e) => {
            witness(&format!(
                "logind's Lock, Unlock and PrepareForSleep for session {id}: {e}; the \
                 socket's lock is the only trigger"
            ));
            return None;
        }
    };
    let locked = match proxy.locked_hint().await {
        Ok(locked) => locked,
        Err(e) => {
            witness(&format!(
                "LockedHint of session {id}: {e}; taken as unlocked"
            ));
            false
        }
    };
    let (hint, mut asked) = unbounded_channel::<bool>();
    let session_id = id.clone();
    let hint_task = tokio::spawn(async move {
        while let Some(locked) = asked.recv().await {
            match proxy.set_locked_hint(locked).await {
                Ok(()) => witness(&format!("LockedHint of session {session_id} is {locked}")),
                Err(e) => witness(&format!(
                    "SetLockedHint({locked}) on session {session_id}: {e}"
                )),
            }
        }
    });
    witness(&format!(
        "the locker of session {id} ({how}); LockedHint is {locked}"
    ));
    Some(Logind {
        hint,
        locked,
        _streams: streams,
        _hint_task: hint_task,
    })
}

impl Logind {
    /// Let the last `SetLockedHint` the conductor asked for reach logind
    /// before the process goes, for at most a second.
    pub async fn finish(self) {
        let Logind {
            hint,
            _streams,
            _hint_task,
            ..
        } = self;
        drop(hint);
        drop(_streams);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), _hint_task).await;
    }
}

async fn subscribe(
    conn: &Connection,
    proxy: &SessionProxy<'static>,
    events: &UnboundedSender<Event>,
) -> zbus::Result<[Drain; 3]> {
    let locks = proxy.receive_lock().await?;
    let unlocks = proxy.receive_unlock().await?;
    let sleeps = ManagerProxy::new(conn)
        .await?
        .receive_prepare_for_sleep()
        .await?;
    Ok([
        caustics_sidecar::forward(locks, events.clone(), |_| Some(Event::LogindLock)),
        caustics_sidecar::forward(unlocks, events.clone(), |_| Some(Event::LogindUnlock)),
        // Both edges: the sleep stops the reader being claimed while the
        // machine suspends (the before-sleep hook's `lock --wait` puts the
        // lock up in that same moment), and the resume opens a window.
        caustics_sidecar::forward(sleeps, events.clone(), |signal: PrepareForSleep| {
            signal.args().ok().map(|args| {
                if *args.start() {
                    Event::Sleep
                } else {
                    Event::Resume
                }
            })
        }),
    ])
}
