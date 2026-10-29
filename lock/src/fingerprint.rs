//! The fingerprint reader, spoken to directly: fprintd on the **system** bus,
//! verifying while the password conversation runs.
//!
//! PAM is serialised: one stack asks one thing at a time, so a stack with
//! `pam_fprintd` in it cannot take a password and a finger at once (its
//! manual, pam_fprintd(8), says so under its limitations). So the lock does
//! not put the reader in its PAM stack at all. While the session is locked
//! this module claims the default reader for the lock's user and keeps a
//! verification going, beside the PAM conversation on screen; a match is a
//! verdict the conductor unlocks on, as it would on PAM's success. GNOME's
//! answer (two PAM stacks, each in a worker process of its own) is not this
//! one: the reader is one D-Bus device, and fprintd is its only owner.
//!
//! [`Reader`] is what a reader is to the conductor, so the conductor's tests
//! run against a fake. [`Fprintd`] is the real one: one **verification run
//! per lock**, a task that
//!
//! - claims the default device for the user and loops `VerifyStart("any")`,
//!   a `VerifyStatus`, and what that status means, sending the conductor a
//!   [`Finger`] for each;
//! - after a no-match stops the verify, pauses ([`Timings::retry_pause`]) and
//!   starts another;
//! - on a reader that goes away (a resume from suspend is the usual cause:
//!   the device re-enumerates under fprintd; or fprintd itself leaves the
//!   bus, taking the claim with it), releases what it held and claims again
//!   with a doubling backoff, for as long as the session stays locked; no
//!   reader, or no fprintd, after one that went is one on its way back. The
//!   backoff starts again from its first pause only after a verify that
//!   lived ([`Timings::settled`]): fprintd reports a sensor that libfprint
//!   has disabled for heat as `verify-disconnected`, the same status as a
//!   reader unplugged, and the lock cannot tell the two apart, so a
//!   disconnect soon after a start is a reader that needs time, and the
//!   pause before the next claim doubles rather than hammering it;
//! - on a reader that cannot ever verify for this lock (no reader, no
//!   enrolled prints, no permission, no system bus) says so once and does
//!   nothing more until the next lock;
//! - when the conductor stops it (the lock is over, or the host went), stops
//!   the verify and releases the device, within a bound, and ends. The
//!   release matters: a polkit prompt's PAM stack, once the session is
//!   unlocked, must find the reader free.
//!
//! The witness says what the reader did and never which finger matched.

use std::time::Duration;

use caustics_sidecar::Drain;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use zbus::Connection;
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedObjectPath;

use crate::conductor::Event;
use crate::witness;

#[zbus::proxy(
    interface = "net.reactivated.Fprint.Manager",
    default_service = "net.reactivated.Fprint",
    default_path = "/net/reactivated/Fprint/Manager"
)]
pub trait Manager {
    /// The reader fprintd would pick; `NoSuchDevice` when there is none.
    fn get_default_device(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "net.reactivated.Fprint.Device",
    default_service = "net.reactivated.Fprint"
)]
pub trait Device {
    /// Take the reader for `username`'s prints; nobody else may verify on it
    /// until it is released or this connection goes.
    fn claim(&self, username: &str) -> zbus::Result<()>;
    fn release(&self) -> zbus::Result<()>;
    /// `"any"`: any of the user's enrolled fingers.
    fn verify_start(&self, finger_name: &str) -> zbus::Result<()>;
    fn verify_stop(&self) -> zbus::Result<()>;

    /// One scan's result; `done` when the verify is over and must be stopped
    /// before another starts.
    #[zbus(signal)]
    fn verify_status(&self, result: &str, done: bool) -> zbus::Result<()>;

    /// fprintd's properties are named in lowercase, hyphenated
    /// (`name`, `scan-type`, `num-enroll-stages`, `finger-present`,
    /// `finger-needed`), not in zbus's default case.
    #[zbus(property, name = "name")]
    fn name(&self) -> zbus::Result<String>;

    /// `press` or `swipe`.
    #[zbus(property, name = "scan-type")]
    fn scan_type(&self) -> zbus::Result<String>;
}

/// What the reader tells the conductor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finger {
    /// The verify is live; `how` is the line that says what to do.
    Ready { how: String },
    /// A finger that is not the user's: an error line, and another verify.
    NoMatch,
    /// A scan to try again (too short, off centre); the line says how.
    Retry(String),
    /// The user's finger: unlock.
    Match,
    /// The reader went; it is claimed again after a pause.
    Gone(String),
    /// No reader will verify for this lock; the password is the only way.
    Unavailable(String),
}

/// The line for a reader of `scan_type`.
pub fn how(scan_type: &str) -> &'static str {
    match scan_type {
        "swipe" => "Swipe a finger across the reader",
        _ => "Touch the fingerprint reader",
    }
}

/// The error line after a finger that did not match.
pub const NOT_RECOGNISED: &str = "Fingerprint not recognised; try again";

/// The line for a `VerifyStatus` that is not done: the scan is to be tried
/// again.
pub fn retry_line(result: &str) -> &'static str {
    match result {
        "verify-swipe-too-short" => "Swipe too short; try again",
        "verify-finger-not-centered" => "Finger not centred; try again",
        "verify-remove-and-retry" => "Lift your finger and try again",
        _ => "Try the scan again",
    }
}

/// A reader, to the conductor: one verification run per lock.
pub trait Reader: Send + 'static {
    /// Begin a run for `user`, its findings sent on `events` as
    /// [`Event::Finger`] under `run`. The run lasts until [`Run::stop`].
    fn start(&mut self, user: &str, run: u32, events: UnboundedSender<Event>) -> Run;
}

/// A run under way: its stop, and its task.
pub struct Run {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Run {
    pub fn new(stop: oneshot::Sender<()>, task: JoinHandle<()>) -> Run {
        Run {
            stop: Some(stop),
            task,
        }
    }

    /// Tell the run the lock is over. The task releases what it holds and
    /// ends; the handle resolves when it has.
    pub fn stop(mut self) -> JoinHandle<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task
    }
}

/// How long the run waits, for what.
#[derive(Clone, Copy, Debug)]
pub struct Timings {
    /// The first pause before a claim after the reader went, doubling to
    /// [`Timings::last_backoff`].
    pub first_backoff: Duration,
    pub last_backoff: Duration,
    /// After a no-match, before the next verify: long enough that a finger
    /// still on the reader is not read again at once.
    pub retry_pause: Duration,
    /// The release at the end of a run, at most.
    pub release_bound: Duration,
    /// How long a claim's verify must have been live for its end to reset
    /// the backoff: a reader that goes sooner (a sensor too hot, which
    /// fprintd says as `verify-disconnected`) waits twice as long each time.
    pub settled: Duration,
}

impl Default for Timings {
    fn default() -> Timings {
        Timings {
            first_backoff: Duration::from_secs(1),
            last_backoff: Duration::from_secs(30),
            retry_pause: Duration::from_millis(500),
            release_bound: Duration::from_secs(2),
            settled: Duration::from_secs(60),
        }
    }
}

/// fprintd: the real reader.
pub struct Fprintd {
    /// The bus to speak on: `None` is the system bus
    /// (`DBUS_SYSTEM_BUS_ADDRESS` when set, as zbus reads it).
    address: Option<String>,
    timings: Timings,
}

impl Fprintd {
    /// fprintd on the system bus.
    pub fn system() -> Fprintd {
        Fprintd {
            address: None,
            timings: Timings::default(),
        }
    }

    /// fprintd on the bus at `address`, with its own timings: the tests'.
    pub fn at(address: &str, timings: Timings) -> Fprintd {
        Fprintd {
            address: Some(address.to_owned()),
            timings,
        }
    }
}

impl Reader for Fprintd {
    fn start(&mut self, user: &str, run: u32, events: UnboundedSender<Event>) -> Run {
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(verify_run(
            self.address.clone(),
            user.to_owned(),
            Report { run, events },
            Stop::new(stopped),
            self.timings,
        ));
        Run::new(stop, task)
    }
}

/// Where a run's findings go.
struct Report {
    run: u32,
    events: UnboundedSender<Event>,
}

impl Report {
    fn send(&self, finger: Finger) {
        let _ = self.events.send(Event::Finger {
            run: self.run,
            finger,
        });
    }
}

/// The conductor's word that the lock is over: a stop sent, or the sender
/// dropped (the conductor is gone).
struct Stop {
    rx: oneshot::Receiver<()>,
    stopped: bool,
}

impl Stop {
    fn new(rx: oneshot::Receiver<()>) -> Stop {
        Stop { rx, stopped: false }
    }

    /// Resolves once the stop has come; at once every time after.
    async fn wait(&mut self) {
        if !self.stopped {
            let _ = (&mut self.rx).await;
            self.stopped = true;
        }
    }

    /// `fut`, unless the stop comes first.
    async fn or<T>(&mut self, fut: impl std::future::Future<Output = T>) -> Option<T> {
        tokio::select! {
            out = fut => Some(out),
            () = self.wait() => None,
        }
    }
}

/// How one claim ended.
enum End {
    /// The conductor stopped the run.
    Stopped,
    /// Never for this lock.
    Unavailable(String),
    /// Gone; claim again after the backoff. `lived` when the claim's verify
    /// had been live for [`Timings::settled`], which resets the backoff.
    Gone { why: String, lived: bool },
}

/// What a claim holds, released at its end.
#[derive(Default)]
struct Held {
    device: Option<DeviceProxy<'static>>,
    claimed: bool,
    verifying: bool,
}

impl Held {
    /// `VerifyStop` if a verify is on, `Release` if claimed, best effort
    /// within `bound`. Whether anything was claimed.
    async fn release(self, bound: Duration) -> bool {
        let Held {
            device,
            claimed,
            verifying,
        } = self;
        let Some(device) = device.filter(|_| claimed) else {
            return false;
        };
        let released = async {
            if verifying {
                let _ = device.verify_stop().await;
            }
            let _ = device.release().await;
        };
        let _ = tokio::time::timeout(bound, released).await;
        true
    }
}

/// One run: claims, verifies and re-claims until stopped.
async fn verify_run(
    address: Option<String>,
    user: String,
    report: Report,
    mut stop: Stop,
    timings: Timings,
) {
    let mut backoff = timings.first_backoff;
    let mut first = true;
    loop {
        let mut held = Held::default();
        let end = claim(
            address.as_deref(),
            &user,
            &report,
            &mut stop,
            &mut held,
            first,
            timings,
        )
        .await;
        first = false;
        let claimed = held.release(timings.release_bound).await;
        match end {
            End::Stopped => {
                if claimed {
                    witness("fingerprint: released");
                }
                return;
            }
            End::Unavailable(why) => {
                witness(&format!("fingerprint: {why}; password only"));
                report.send(Finger::Unavailable(why));
                stop.wait().await;
                return;
            }
            End::Gone { why, lived } => {
                if lived {
                    backoff = timings.first_backoff;
                }
                witness(&format!(
                    "fingerprint: reader gone ({why}); again in {} s",
                    backoff.as_secs()
                ));
                report.send(Finger::Gone(why));
                if stop.or(tokio::time::sleep(backoff)).await.is_none() {
                    return;
                }
                backoff = (backoff * 2).min(timings.last_backoff);
            }
        }
    }
}

/// The error's name after fprintd's prefix, for one of fprintd's own.
fn fprint_error(e: &zbus::Error) -> Option<&str> {
    match e {
        zbus::Error::MethodError(name, _, _) => {
            name.as_str().strip_prefix("net.reactivated.Fprint.Error.")
        }
        _ => None,
    }
}

/// Whether the bus says nothing owns or can start fprintd's name.
fn no_service(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ),
        _ => false,
    }
}

async fn connect(address: Option<&str>) -> zbus::Result<Connection> {
    match address {
        None => Connection::system().await,
        Some(address) => zbus::connection::Builder::address(address)?.build().await,
    }
}

/// One claim: the device, its signals, the claim, and verifies until one of
/// them ends it.
async fn claim(
    address: Option<&str>,
    user: &str,
    report: &Report,
    stop: &mut Stop,
    held: &mut Held,
    first: bool,
    timings: Timings,
) -> End {
    let gone = |why: String| End::Gone { why, lived: false };
    let conn = match stop.or(connect(address)).await {
        None => return End::Stopped,
        Some(Ok(conn)) => conn,
        Some(Err(e)) if first => return End::Unavailable(format!("no system bus ({e})")),
        Some(Err(e)) => return gone(format!("the system bus: {e}")),
    };
    let manager = match stop.or(ManagerProxy::new(&conn)).await {
        None => return End::Stopped,
        Some(Ok(manager)) => manager,
        Some(Err(e)) => return gone(e.to_string()),
    };
    let path = match stop.or(manager.get_default_device()).await {
        None => return End::Stopped,
        Some(Ok(path)) => path,
        // After a reader that went, none for now is a reader on its way back
        // (a resume re-enumerates it, a restarted fprintd is being started):
        // claimed again after the backoff, not given up on.
        Some(Err(e)) if fprint_error(&e) == Some("NoSuchDevice") => {
            return if first {
                End::Unavailable("no reader".to_owned())
            } else {
                gone("no reader yet".to_owned())
            };
        }
        Some(Err(e)) if no_service(&e) => {
            return if first {
                End::Unavailable("no fprintd on the system bus".to_owned())
            } else {
                gone("no fprintd yet".to_owned())
            };
        }
        Some(Err(e)) => return gone(e.to_string()),
    };
    let device = DeviceProxy::builder(&conn)
        .path(path)
        .map(|b| b.cache_properties(CacheProperties::No));
    let device = match device {
        Ok(builder) => match stop.or(builder.build()).await {
            None => return End::Stopped,
            Some(Ok(device)) => device,
            Some(Err(e)) => return gone(e.to_string()),
        },
        Err(e) => return gone(e.to_string()),
    };
    // Subscribed before the first verify, so no status is missed, and
    // drained into a channel so it never stalls the connection
    // (docs/architecture.md, *Rules*).
    let (statuses, mut status) = unbounded_channel();
    let _drain: Drain = match stop.or(device.receive_verify_status()).await {
        None => return End::Stopped,
        Some(Ok(stream)) => {
            caustics_sidecar::forward(stream, statuses.clone(), |signal: VerifyStatus| {
                signal
                    .args()
                    .ok()
                    .map(|args| (args.result().to_string(), *args.done()))
            })
        }
        Some(Err(e)) => return gone(e.to_string()),
    };
    // fprintd leaving the bus (a crash, a restart) takes the claim and the
    // verify with it, and no status will ever come: that is the reader gone.
    let _owner: Drain = match stop.or(device.inner().receive_owner_changed()).await {
        None => return End::Stopped,
        Some(Ok(stream)) => {
            caustics_sidecar::forward(stream, statuses, |_| Some((FPRINTD_LEFT.to_owned(), true)))
        }
        Some(Err(e)) => return gone(e.to_string()),
    };
    held.device = Some(device.clone());
    match stop.or(device.claim(user)).await {
        None => return End::Stopped,
        Some(Ok(())) => held.claimed = true,
        Some(Err(e)) => {
            return match fprint_error(&e) {
                Some("NoEnrolledPrints") => End::Unavailable(no_prints(user)),
                Some("PermissionDenied") => {
                    End::Unavailable("not permitted to claim the reader".to_owned())
                }
                _ => gone(format!("claim: {e}")),
            };
        }
    }
    let name = stop.or(reader_name(&device)).await;
    let scan_type = stop.or(device.scan_type()).await;
    let (Some(name), Some(scan_type)) = (name, scan_type) else {
        return End::Stopped;
    };
    let how = how(&scan_type.unwrap_or_default());
    verify(
        &device,
        &name,
        how,
        user,
        report,
        stop,
        held,
        &mut status,
        timings,
    )
    .await
}

/// The reader's name for the witness; `(unnamed)` when fprintd gives none.
async fn reader_name(device: &DeviceProxy<'_>) -> String {
    device
        .name()
        .await
        .unwrap_or_else(|_| "(unnamed)".to_owned())
}

/// Not a result fprintd sends: fprintd's name changed owner.
const FPRINTD_LEFT: &str = "caustics-lock: fprintd left";

fn no_prints(user: &str) -> String {
    format!("no enrolled prints for {}", shown_user(user))
}

fn shown_user(user: &str) -> &str {
    if user.is_empty() { "this user" } else { user }
}

/// Verifies on a claimed device until a match, a reader gone, or the stop.
#[allow(clippy::too_many_arguments)]
async fn verify(
    device: &DeviceProxy<'static>,
    name: &str,
    how: &str,
    user: &str,
    report: &Report,
    stop: &mut Stop,
    held: &mut Held,
    status: &mut UnboundedReceiver<(String, bool)>,
    timings: Timings,
) -> End {
    // When this claim's first verify went live: the claim has lived once it
    // has been verifying for `settled`, through every no-match since.
    let mut live_since: Option<tokio::time::Instant> = None;
    let gone = |why: String, live_since: Option<tokio::time::Instant>| End::Gone {
        why,
        lived: live_since.is_some_and(|at| at.elapsed() >= timings.settled),
    };
    loop {
        // Statuses of a verify that is over are no business of the next.
        while status.try_recv().is_ok() {}
        match stop.or(device.verify_start("any")).await {
            None => return End::Stopped,
            Some(Ok(())) => held.verifying = true,
            Some(Err(e)) => {
                return match fprint_error(&e) {
                    Some("NoEnrolledPrints") => End::Unavailable(no_prints(user)),
                    Some("PermissionDenied") => {
                        End::Unavailable("not permitted to verify".to_owned())
                    }
                    _ => gone(format!("verify: {e}"), live_since),
                };
            }
        }
        if live_since.is_none() {
            live_since = Some(tokio::time::Instant::now());
            witness(&format!(
                "fingerprint: reader {name} claimed for {}; verifying",
                shown_user(user)
            ));
            report.send(Finger::Ready {
                how: how.to_owned(),
            });
        }
        loop {
            let Some(next) = stop.or(status.recv()).await else {
                return End::Stopped;
            };
            let Some((result, done)) = next else {
                return gone("its signals ended".to_owned(), live_since);
            };
            match result.as_str() {
                FPRINTD_LEFT => {
                    // Nothing is held any more to release.
                    *held = Held::default();
                    return gone("fprintd left the bus".to_owned(), live_since);
                }
                "verify-match" => {
                    report.send(Finger::Match);
                    stop.wait().await;
                    return End::Stopped;
                }
                "verify-no-match" => {
                    witness("fingerprint: not recognised");
                    report.send(Finger::NoMatch);
                    match stop.or(device.verify_stop()).await {
                        None => return End::Stopped,
                        Some(Ok(())) => held.verifying = false,
                        Some(Err(e)) => {
                            return gone(format!("stop: {e}"), live_since);
                        }
                    }
                    if stop
                        .or(tokio::time::sleep(timings.retry_pause))
                        .await
                        .is_none()
                    {
                        return End::Stopped;
                    }
                    break;
                }
                // A reader unplugged or re-enumerated, and a sensor libfprint
                // disabled for heat: fprintd says both this way.
                "verify-disconnected" => {
                    return gone("disconnected".to_owned(), live_since);
                }
                other if !done => {
                    let line = retry_line(other);
                    witness(&format!("fingerprint: {line}"));
                    report.send(Finger::Retry(line.to_owned()));
                }
                other => return gone(other.to_owned(), live_since),
            }
        }
    }
}

#[cfg(test)]
mod tests;
