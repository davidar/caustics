//! The bar's feeders: one owner per connection, `Update`s out, `Action`s in.
//!
//! A feeder is an event-rate translator and nothing else (`model.rs` is the
//! contract). It owns a connection —
//! a Wayland global here, the niri IPC socket there — turns what arrives on
//! it into [`Update`]s on a calloop channel, and performs the [`Action`]s the
//! caustic decided. It never decides anything itself, and it never fails
//! fatally: a global that is not there becomes one `Update::Absent` and the
//! bar goes on without that feature.
//!
//! Three Wayland feeders live here, two for the windows, and the compositor seam over its IPC socket:
//!
//! - [`toplevels`] — `ext_foreign_toplevel_list_v1`, the set of windows.
//! - [`compositor`] — the seam: focus and urgency in, actions out, over
//!   whatever socket the compositor offers. [`niri_ipc`] is the one
//!   implementation, over `$NIRI_SOCKET`.
//! - [`capture`] — `ext_image_copy_capture_v1` over a foreign-toplevel
//!   source, one hover preview at a time.
//!
//! and one applet:
//!
//! - [`caffeine`] — `zwp_idle_inhibit_manager_v1`, the idle-inhibit toggle. The
//!   one applet that is not a sidecar, because it owns a Wayland object on
//!   the bar's connection (docs/architecture.md, *Sidecars*).
//!
//! and one that is not Wayland's at all:
//!
//! - [`pipe`] — the **sidecars over pipes** (docs/architecture.md, *Sidecars*;
//!   docs/abi.md, *The wire*): child processes whose stdout is the wire. They are
//!   here because they are feeders — a connection in, `Update`s out — and
//!   nowhere near the rest of the bar, because the bar parses none of what
//!   crosses them.
//!
//! # How the embassy plugs this in
//!
//! [`Feeders`] is deliberately not generic over the state type: the one place
//! that needs a `QueueHandle<D>` after bind time (the capture feeder, which
//! creates objects when a hover asks it to) keeps it behind [`CaptureQueue`],
//! so the embassy's state type stays out of every other signature. Wiring is
//! three lines in the embassy's `Bar`:
//!
//! ```ignore
//! impl AsMut<Feeders> for Bar { fn as_mut(&mut self) -> &mut Feeders { &mut self.feeders } }
//! crate::delegate_feeders!(Bar);
//! let feeders = Feeders::bind(&globals, &qh, &surface, updates_tx);
//! ```
//!
//! # Timers
//!
//! The feeders own no timers. The capture feeder wants a new frame every
//! [`capture::MIN_INTERVAL`] while a session lives, and says so through
//! [`Feeders::next_deadline`]; the embassy — which already owns the loop —
//! arms one calloop timer on that instant and calls [`Feeders::tick`]. This
//! keeps the loop in one place, the bar process's own rule, one size down (docs/architecture.md, *The bar process*).

pub mod caffeine;
pub mod capture;
pub mod compositor;
pub mod niri_ipc;
pub mod pipe;
pub mod toplevels;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use smithay_client_toolkit::reexports::protocols::ext::{
    foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
    image_capture_source::v1::client::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
        ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
        ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
    },
};
use smithay_client_toolkit::reexports::protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, Proxy, QueueHandle};

use crate::abi::wire::{ActionBody, Frame, Request};
use crate::model::{Action, Update, Updates, Window};

use compositor::{Compositor, NoCompositor};

/// The user data every global this module binds carries. Deliberately its own
/// type rather than sctk's `GlobalData`: `wl_shm` is bound here *and* may be
/// bound by sctk's `Shm` in the same embassy, and two `Dispatch<WlShm, _>`
/// impls on one state type may only coexist if their user data differ.
#[derive(Debug)]
pub struct FeederData;

/// User data on an `ext_foreign_toplevel_handle_v1`.
#[derive(Debug)]
pub struct ToplevelData;

/// User data on the capture source, session and frame objects.
#[derive(Debug)]
pub struct CaptureData;

// ---------------------------------------------------------------------------
// The shared window set
// ---------------------------------------------------------------------------

/// The half-and-half view of a window that the two Wayland-side feeders build
/// between them.
///
/// `ext_foreign_toplevel_list_v1` knows an app id, a title and an identifier;
/// it has **no state event at all** in v1, so it cannot say which window is
/// focused. The compositor's own IPC ([`compositor::Compositor`], niri's
/// today) knows focus and urgency, and the title and app id as well. Both name
/// the window by the same `u64` (for niri,
/// `MappedId::to_protocol_identifier` is the IPC's `Window.id`), so the two
/// halves join cleanly — but they must join *before* the update is sent,
/// because [`crate::model::Model::apply`] replaces a `Window` wholesale and a
/// title change from the ext side would otherwise stamp `focused: false` over
/// what the compositor just said.
///
/// So this is the join: each feeder writes its own fields and reads back the
/// whole merged window to send. It is shared with the niri event-stream
/// thread, hence the mutex; contention is one lock per window event.
#[derive(Clone, Debug, Default)]
pub struct WindowSet(Arc<Mutex<Inner>>);

#[derive(Debug, Default)]
struct Inner {
    windows: BTreeMap<u64, Window>,
    /// Bumped every time a window becomes focused, so "most recently focused"
    /// is a max and not a guess. `model::Window::focus_stamp`'s contract.
    stamp: u64,
}

fn blank(id: u64) -> Window {
    Window {
        id,
        app_id: String::new(),
        title: String::new(),
        focused: false,
        urgent: false,
        focus_stamp: 0,
    }
}

impl WindowSet {
    /// The ext side's half: app id and title. Returns the merged window when
    /// something actually changed, so the caller can skip a no-op update.
    pub fn merge_ext(&self, id: u64, app_id: String, title: String) -> Option<Window> {
        let mut inner = self.0.lock().unwrap();
        // A window we have never seen always yields an update, even when every
        // field it carries is empty: "the model does not know this window yet"
        // is itself the change.
        let fresh = !inner.windows.contains_key(&id);
        let w = inner.windows.entry(id).or_insert_with(|| blank(id));
        if !fresh && w.app_id == app_id && w.title == title {
            return None;
        }
        w.app_id = app_id;
        w.title = title;
        Some(w.clone())
    }

    /// The compositor's half, whoever the compositor is
    /// ([`compositor::Compositor`]). `focused` and `urgent` come only from
    /// here; app id and title are refreshed too, since the IPC carries them
    /// and agrees.
    pub fn merge_state(
        &self,
        id: u64,
        app_id: String,
        title: String,
        focused: bool,
        urgent: bool,
    ) -> Option<Window> {
        let mut inner = self.0.lock().unwrap();
        let stamp = inner.stamp;
        let fresh = !inner.windows.contains_key(&id);
        let w = inner.windows.entry(id).or_insert_with(|| blank(id));
        let gained_focus = focused && !w.focused;
        let next = Window {
            id,
            app_id,
            title,
            focused,
            urgent,
            focus_stamp: if gained_focus {
                stamp + 1
            } else {
                w.focus_stamp
            },
        };
        if !fresh && *w == next {
            return None;
        }
        *w = next.clone();
        if gained_focus {
            inner.stamp += 1;
        }
        Some(next)
    }

    /// `WindowFocusChanged`: exactly one window is focused, or none. Returns
    /// every window whose focus flag moved, so each can be sent on.
    pub fn set_focus(&self, id: Option<u64>) -> Vec<Window> {
        let mut inner = self.0.lock().unwrap();
        let mut changed = Vec::new();
        let stamp = inner.stamp + 1;
        let mut bumped = false;
        for (wid, w) in inner.windows.iter_mut() {
            let want = Some(*wid) == id;
            if w.focused == want {
                continue;
            }
            w.focused = want;
            if want {
                w.focus_stamp = stamp;
                bumped = true;
            }
            changed.push(w.clone());
        }
        if bumped {
            inner.stamp = stamp;
        }
        changed
    }

    /// `WindowUrgencyChanged`.
    pub fn set_urgent(&self, id: u64, urgent: bool) -> Option<Window> {
        let mut inner = self.0.lock().unwrap();
        let w = inner.windows.get_mut(&id)?;
        if w.urgent == urgent {
            return None;
        }
        w.urgent = urgent;
        Some(w.clone())
    }

    /// Forget a window. Returns whether it was known.
    pub fn remove(&self, id: u64) -> bool {
        self.0.lock().unwrap().windows.remove(&id).is_some()
    }

    /// The ids the set holds, ascending. Used by `WindowsChanged`, which is a
    /// full replacement and so implies a close for everything missing.
    pub fn ids(&self) -> Vec<u64> {
        self.0.lock().unwrap().windows.keys().copied().collect()
    }

    /// Test helper: the merged view of one window, without sending it.
    #[cfg(test)]
    pub fn get(&self, id: u64) -> Option<Window> {
        self.0.lock().unwrap().windows.get(&id).cloned()
    }
}

// ---------------------------------------------------------------------------
// The erased queue handle
// ---------------------------------------------------------------------------

/// The three constructor requests the capture feeder must send long after
/// `bind` returned, with the embassy's state type erased.
///
/// `QueueHandle<D>` is the only thing in a feeder that names `D`. Boxing it
/// behind this trait is what keeps [`Feeders`] itself concrete, so `Bar`
/// meets it through one `AsMut<Feeders>` and nothing else. The blanket impl
/// below carries the real `Dispatch` bounds; the trait carries none.
pub trait CaptureQueue: Send + Sync {
    fn create_source(
        &self,
        manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
        handle: &toplevels::Handle,
    ) -> ExtImageCaptureSourceV1;

    fn create_session(
        &self,
        manager: &ExtImageCopyCaptureManagerV1,
        source: &ExtImageCaptureSourceV1,
    ) -> ExtImageCopyCaptureSessionV1;

    fn create_frame(&self, session: &ExtImageCopyCaptureSessionV1) -> ExtImageCopyCaptureFrameV1;
}

impl<D> CaptureQueue for QueueHandle<D>
where
    D: Dispatch<ExtImageCaptureSourceV1, CaptureData>
        + Dispatch<ExtImageCopyCaptureSessionV1, CaptureData>
        + Dispatch<ExtImageCopyCaptureFrameV1, CaptureData>
        + 'static,
{
    fn create_source(
        &self,
        manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
        handle: &toplevels::Handle,
    ) -> ExtImageCaptureSourceV1 {
        manager.create_source(handle, self, CaptureData)
    }

    fn create_session(
        &self,
        manager: &ExtImageCopyCaptureManagerV1,
        source: &ExtImageCaptureSourceV1,
    ) -> ExtImageCopyCaptureSessionV1 {
        // Options is a bitfield and 0 means "no cursors", which is what a
        // dock preview wants: the pointer is over the dock, not the window.
        manager.create_session(
            source,
            ext_image_copy_capture_manager_v1::Options::empty(),
            self,
            CaptureData,
        )
    }

    fn create_frame(&self, session: &ExtImageCopyCaptureSessionV1) -> ExtImageCopyCaptureFrameV1 {
        session.create_frame(self, CaptureData)
    }
}

/// The one constructor request the caffeine feeder must send long after `bind`
/// returned, with the embassy's state type erased.
///
/// A second trait beside [`CaptureQueue`] rather than one more method on it: one erased queue handle per protocol is readable, and a trait that knew
/// both capture and idle-inhibit would have to be implemented by anything that
/// wanted either.
pub trait InhibitQueue: Send + Sync {
    fn create_inhibitor(
        &self,
        manager: &ZwpIdleInhibitManagerV1,
        surface: &WlSurface,
    ) -> ZwpIdleInhibitorV1;
}

impl<D> InhibitQueue for QueueHandle<D>
where
    D: Dispatch<ZwpIdleInhibitorV1, FeederData> + 'static,
{
    fn create_inhibitor(
        &self,
        manager: &ZwpIdleInhibitManagerV1,
        surface: &WlSurface,
    ) -> ZwpIdleInhibitorV1 {
        manager.create_inhibitor(surface, self, FeederData)
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// Every Wayland-side feeder, bound once at startup.
pub struct Feeders {
    pub toplevels: toplevels::Toplevels,
    pub capture: capture::Capture,
    /// The idle-inhibit toggle, which owns a Wayland object on the bar's own
    /// layer surface and so lives here rather than in a sidecar.
    pub caffeine: caffeine::Caffeine,
    /// Whichever [`Compositor`] this bar is hosted on, chosen once in
    /// [`Feeders::bind`] and never named again (`feeders/compositor.rs`).
    pub compositor: Box<dyn Compositor>,
    /// Every sidecar this bar started, in `pipe::commands()`'s order.
    /// Nothing else in the bar knows which is which: they send `Update`s and
    /// the model keeps what arrives.
    pub sidecars: Vec<pipe::Pipe>,
    updates: Updates,
}

impl std::fmt::Debug for Feeders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feeders")
            .field("toplevels", &self.toplevels)
            .field("capture", &self.capture)
            .field("caffeine", &self.caffeine)
            .field("compositor", &self.compositor.name())
            .field("sidecars", &self.sidecars)
            .finish()
    }
}

/// Bind one optional global, logging one witness line either way (`bound` or
/// `absent`, so a journal always says which features this compositor offers)
/// and sending `Absent` when it is not there. Never fails.
fn bind_optional<I, D>(
    globals: &GlobalList,
    qh: &QueueHandle<D>,
    updates: &Updates,
    name: &'static str,
) -> Option<I>
where
    I: Proxy + 'static,
    D: Dispatch<I, FeederData> + 'static,
{
    match globals.bind::<I, D, FeederData>(qh, 1..=1, FeederData) {
        Ok(proxy) => {
            tracing::info!("bound {name} v{}", proxy.version());
            Some(proxy)
        }
        Err(err) => {
            tracing::info!("{name} absent: {err}");
            let _ = updates.send(Update::Absent(name.to_owned()));
            None
        }
    }
}

/// Which [`Compositor`] implementation this session gets: niri when
/// `$NIRI_SOCKET` names one, and otherwise none at all.
///
/// The whole choice, in one place and made once — which is the point of the
/// seam. A second implementation is one more arm here and one more module
/// beside `niri_ipc.rs`; nothing else in the bar learns of it.
fn pick_compositor(windows: WindowSet, updates: Updates) -> Box<dyn Compositor> {
    if std::env::var_os("NIRI_SOCKET").is_some() {
        Box::new(niri_ipc::NiriIpc::start(windows, updates))
    } else {
        Box::new(NoCompositor::start(windows, updates))
    }
}

impl Feeders {
    /// Bind what the compositor offers and start its event stream.
    ///
    /// Nothing here can fail: every global is optional, every absence is one
    /// `Update::Absent` and one witness line, and the bar renders without the
    /// feature. The `D` bounds are the whole protocol surface the feeders
    /// own; `Bar` satisfies them with one `delegate_feeders!(Bar)`.
    ///
    /// `surface` is the bar's own layer surface, which the caffeine feeder
    /// hangs its inhibitor on. It is a parameter and not something found here,
    /// because the embassy creates the surface — `Bar::new` makes the layer
    /// surface and *then* calls this, an ordering the signature states rather
    /// than one a reader has to notice.
    pub fn bind<D>(
        globals: &GlobalList,
        qh: &QueueHandle<D>,
        surface: &WlSurface,
        updates: Updates,
    ) -> Self
    where
        D: Dispatch<ExtForeignToplevelListV1, FeederData>
            + Dispatch<toplevels::Handle, ToplevelData>
            + Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, FeederData>
            + Dispatch<ExtImageCopyCaptureManagerV1, FeederData>
            + Dispatch<ExtImageCaptureSourceV1, CaptureData>
            + Dispatch<ExtImageCopyCaptureSessionV1, CaptureData>
            + Dispatch<ExtImageCopyCaptureFrameV1, CaptureData>
            + Dispatch<ZwpIdleInhibitManagerV1, FeederData>
            + Dispatch<ZwpIdleInhibitorV1, FeederData>
            + Dispatch<WlShm, FeederData>
            + 'static,
    {
        let windows = WindowSet::default();

        let list = bind_optional::<ExtForeignToplevelListV1, D>(
            globals,
            qh,
            &updates,
            "ext_foreign_toplevel_list_v1",
        );
        let source_manager = bind_optional::<ExtForeignToplevelImageCaptureSourceManagerV1, D>(
            globals,
            qh,
            &updates,
            "ext_foreign_toplevel_image_capture_source_manager_v1",
        );
        let copy_manager = bind_optional::<ExtImageCopyCaptureManagerV1, D>(
            globals,
            qh,
            &updates,
            "ext_image_copy_capture_manager_v1",
        );
        let shm = bind_optional::<WlShm, D>(globals, qh, &updates, "wl_shm");
        let inhibit = bind_optional::<ZwpIdleInhibitManagerV1, D>(
            globals,
            qh,
            &updates,
            "zwp_idle_inhibit_manager_v1",
        );

        let toplevels = toplevels::Toplevels::new(list, windows.clone(), updates.clone());
        let capture = capture::Capture::new(
            source_manager,
            copy_manager,
            shm,
            Box::new(qh.clone()),
            updates.clone(),
        );
        let caffeine = caffeine::Caffeine::new(
            inhibit,
            surface.clone(),
            Box::new(qh.clone()),
            updates.clone(),
        );
        let compositor = pick_compositor(windows, updates.clone());
        // Not globals and not bound: child processes, started here because
        // this is where feeders start. Each absence is one `Absent(<name>)`,
        // exactly like a global that is not offered.
        let sidecars: Vec<pipe::Pipe> = pipe::commands()
            .iter()
            .map(|command| pipe::start(command, updates.clone()))
            .collect();

        Self {
            toplevels,
            capture,
            caffeine,
            compositor,
            sidecars,
            updates,
        }
    }

    /// Perform one action, if it belongs to a feeder here.
    ///
    /// Returns whether it was taken. `false` means the action is somebody
    /// else's — the embassy keeps the dock's favourites — and the caller must
    /// keep looking.
    pub fn perform(&mut self, action: &Action) -> bool {
        match action {
            // `Launch` is the compositor's too: starting a client is what a
            // compositor is for, and the bar holds its socket already.
            Action::Activate(_)
            | Action::Close(_)
            | Action::ToggleOverview
            | Action::Launch { .. } => self.compositor.perform(action),
            Action::PreviewWanted(want) => {
                self.capture.want(*want, self.toplevels.handles());
                true
            }
            Action::ToggleCaffeine => {
                self.caffeine.perform();
                true
            }
            // The embassy's: keeping the dock's favourites belongs to no
            // connection, and `Spawn` is a request for a desktop-entry
            // lookup — the embassy answers it with the `Launch` above.
            Action::Spawn { .. } | Action::Pin { .. } | Action::Unpin { .. } => false,
            Action::Sidecar { name, body } => self.to_sidecar(name, body),
        }
    }

    /// One action to the sidecar whose row it names (docs/abi.md, *Action*).
    ///
    /// Always `true`: the action *is* a sidecar's, whether or not the child is
    /// there to take it. Answering `false` would send the embassy hunting for
    /// another owner and end in a second, vaguer witness line — while the one
    /// that matters, naming the sidecar and why the frame did not go, has
    /// already been logged here or in [`pipe::Pipe::send`].
    fn to_sidecar(&self, name: &str, body: &ActionBody) -> bool {
        match self.sidecars.iter().find(|pipe| pipe.name() == name) {
            Some(pipe) => {
                pipe.send(body);
            }
            None => tracing::info!(
                "action dropped: no sidecar named {name} (row {}, kind {})",
                body.row,
                body.kind
            ),
        }
        true
    }

    /// Ask a sidecar to resolve a name (docs/abi.md, *Request*): a `Request`
    /// frame on its stdin. `false` when no sidecar of that name is running —
    /// one line, and the caller draws without it.
    // Called from `Bar::send_requests` once per build, with every name the caustic wanted and `Facts` had not yet asked about.
    pub fn request(&self, sidecar: &str, kind: u32, name: &str) -> bool {
        match self.sidecars.iter().find(|pipe| pipe.name() == sidecar) {
            Some(pipe) => pipe.request(
                &Frame::Request(Request {
                    kind,
                    arg: 0,
                    name: name.to_owned(),
                }),
                &format!("request kind {kind} for {name:?}"),
            ),
            None => {
                tracing::info!("request dropped: no sidecar named {sidecar} for {name:?}");
                false
            }
        }
    }

    /// The compositor's token for a click on `row`, down `sidecar`'s pipe as a
    /// `REQ_ACTIVATION` (docs/abi.md, *Request*). `send_frame`, not `request`:
    /// a token is the click's, so it is never remembered or replayed to a
    /// child that starts later.
    pub fn activation_token(&self, sidecar: &str, row: u32, token: &str) -> bool {
        let Some(pipe) = self.sidecars.iter().find(|pipe| pipe.name() == sidecar) else {
            tracing::info!("activation: no sidecar named {sidecar} for row={row}");
            return false;
        };
        pipe.send_frame(
            &Frame::Request(Request {
                kind: crate::abi::wire::REQ_ACTIVATION,
                arg: row,
                name: token.to_owned(),
            }),
            &format!("activation token for row {row}"),
        )
    }

    /// When the capture feeder next wants a frame, if a session is live. The
    /// embassy arms one calloop timer on this.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.capture.next_deadline()
    }

    /// Take the frame the deadline was for, and give the next deadline.
    pub fn tick(&mut self) -> Option<Instant> {
        self.capture.tick();
        self.capture.next_deadline()
    }

    /// The channel the feeders send on, for the embassy's own timers. The
    /// embassy keeps its own clone of the sender instead (`main.rs` makes the
    /// channel and hands both ends out), so nothing calls this yet.
    #[allow(dead_code)]
    pub fn updates(&self) -> &Updates {
        &self.updates
    }
}

/// Delegate every feeder protocol to [`Feeders`].
///
/// The impls are written the way sctk writes its own — generic over the
/// dispatching state, reaching the feeder through `AsMut<Feeders>` — so this
/// is all `Bar` needs beside that one `AsMut` impl.
#[macro_export]
macro_rules! delegate_feeders {
    ($ty:ty) => {
        // `delegate_dispatch!` takes one interface per call, so this is one
        // line per protocol object the feeders own.
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1: $crate::feeders::FeederData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1: $crate::feeders::ToplevelData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::ext::image_capture_source::v1::client::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1: $crate::feeders::FeederData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1: $crate::feeders::CaptureData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1: $crate::feeders::FeederData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1: $crate::feeders::CaptureData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1: $crate::feeders::CaptureData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::wayland_client::protocol::wl_shm::WlShm: $crate::feeders::FeederData] => $crate::feeders::Feeders);
        // The two idle-inhibit objects. Neither has an event, so both
        // `Dispatch` impls are empty bodies — and both still have to be here,
        // because the bounds on `Feeders::bind` name them.
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1: $crate::feeders::FeederData] => $crate::feeders::Feeders);
        ::wayland_client::delegate_dispatch!($ty: [::smithay_client_toolkit::reexports::protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1: $crate::feeders::FeederData] => $crate::feeders::Feeders);
    };
}

#[cfg(test)]
mod tests;
