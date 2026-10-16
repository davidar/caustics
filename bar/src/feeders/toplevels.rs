//! `ext_foreign_toplevel_list_v1` — the set of windows, and the handles the
//! capture feeder needs.
//!
//! The whole v1 surface is seven events: the list's `toplevel` and `finished`,
//! and the handle's `title`, `app_id`, `identifier`, `done` and `closed`.
//! There is **no state event** — no `activated`, no `minimized`, nothing — so
//! this feeder cannot say which window is focused and does not try. Focus
//! comes from the niri IPC event stream (`super::niri_ipc`), and the two
//! halves meet in [`WindowSet`] before either sends an `Update::Window`.
//!
//! The identifier is niri's window id printed as a decimal
//! (`MappedId::to_protocol_identifier` is `format!("{}", self.0)`), which is
//! the same `u64` the IPC's `Window.id` and `Action::FocusWindow { id }` use.
//! A handle whose identifier does not parse is witnessed once and dropped: it
//! is not a niri window and nothing in the bar could address it.
//!
//! The bar never binds `zwlr_foreign_toplevel_manager_v1`. A client that binds
//! both has to join the two handle sets on `(app_id, title)`, which is
//! ambiguous the moment two windows share both; being ext-only avoids the join.

use std::collections::HashMap;

use smithay_client_toolkit::reexports::protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_client::backend::ObjectId;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use super::{FeederData, Feeders, ToplevelData, WindowSet};
use crate::model::{Update, Updates};

/// The handle type, named once so the other feeders do not repeat the path.
pub type Handle = ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
/// The list type.
pub type List = ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1;

/// What a handle has told us since its last `done`. The protocol is
/// double-buffered: properties accumulate and `done` applies them.
#[derive(Clone, Debug, Default)]
struct Pending {
    app_id: Option<String>,
    title: Option<String>,
    identifier: Option<String>,
    /// Set once `done` resolved the identifier; `None` while unparsed.
    window: Option<u64>,
}

/// The toplevel feeder's state.
pub struct Toplevels {
    list: Option<List>,
    /// Per live handle, keyed by proxy id because that is what an event
    /// carries before we know the window id.
    pending: HashMap<ObjectId, Pending>,
    /// Window id → its handle, which is what `capture` asks for.
    handles: HashMap<u64, Handle>,
    windows: WindowSet,
    updates: Updates,
}

impl std::fmt::Debug for Toplevels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Toplevels")
            .field("bound", &self.list.is_some())
            .field("handles", &self.handles.len())
            .finish()
    }
}

impl Toplevels {
    pub fn new(list: Option<List>, windows: WindowSet, updates: Updates) -> Self {
        Self {
            list,
            pending: HashMap::new(),
            handles: HashMap::new(),
            windows,
            updates,
        }
    }

    /// Window id → handle, for the capture feeder's `create_source`.
    pub fn handles(&self) -> &HashMap<u64, Handle> {
        &self.handles
    }

    /// Whether the global was there at all.
    pub fn is_bound(&self) -> bool {
        self.list.is_some()
    }

    /// The handle map, drained and every handle destroyed. Used on shutdown.
    pub fn destroy(&mut self) {
        for (_, handle) in self.handles.drain() {
            handle.destroy();
        }
        self.pending.clear();
        if let Some(list) = self.list.take() {
            list.destroy();
        }
    }

    fn apply_done(&mut self, handle: &Handle) {
        let key = handle.id();
        let Some(pending) = self.pending.get_mut(&key) else {
            return;
        };

        // The identifier is immutable, so this resolves exactly once per
        // handle; later `done`s are title and app-id changes.
        if pending.window.is_none() {
            let Some(raw) = pending.identifier.clone() else {
                // A `done` before any `identifier` is legal but useless to us.
                return;
            };
            match raw.parse::<u64>() {
                Ok(id) => {
                    pending.window = Some(id);
                    self.handles.insert(id, handle.clone());
                    tracing::info!("toplevel {id} registered (identifier {raw:?})");
                }
                Err(err) => {
                    tracing::info!(
                        "toplevel identifier {raw:?} is not a decimal window id ({err}); dropping the handle"
                    );
                    self.pending.remove(&key);
                    handle.destroy();
                    return;
                }
            }
        }

        let pending = &self.pending[&key];
        let id = pending.window.expect("resolved just above");
        let app_id = pending.app_id.clone().unwrap_or_default();
        let title = pending.title.clone().unwrap_or_default();
        if let Some(window) = self.windows.merge_ext(id, app_id, title) {
            let _ = self.updates.send(Update::Window(window));
        }
    }

    fn closed(&mut self, handle: &Handle) {
        let key = handle.id();
        let Some(pending) = self.pending.remove(&key) else {
            handle.destroy();
            return;
        };
        if let Some(id) = pending.window {
            self.handles.remove(&id);
            self.windows.remove(id);
            tracing::info!("toplevel {id} vanished");
            let _ = self.updates.send(Update::WindowClosed(id));
        }
        handle.destroy();
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

impl<D> Dispatch<List, FeederData, D> for Feeders
where
    D: Dispatch<List, FeederData> + Dispatch<Handle, ToplevelData> + AsMut<Feeders> + 'static,
{
    /// The list hands out handles. `toplevel` is the only event with a
    /// `new_id`, and wayland-rs has already constructed the handle for us
    /// with the user data the `event_created_child!` table below names.
    fn event(
        state: &mut D,
        _list: &List,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &FeederData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        let feeders = state.as_mut();
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                feeders
                    .toplevels
                    .pending
                    .insert(toplevel.id(), Pending::default());
            }
            ext_foreign_toplevel_list_v1::Event::Finished => {
                tracing::info!("ext_foreign_toplevel_list_v1 finished; no more toplevels");
                let _ = feeders
                    .updates
                    .send(Update::Absent("ext_foreign_toplevel_list_v1".to_owned()));
                feeders.toplevels.destroy();
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(D, List, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (Handle, ToplevelData),
    ]);
}

impl<D> Dispatch<Handle, ToplevelData, D> for Feeders
where
    D: Dispatch<Handle, ToplevelData> + AsMut<Feeders> + 'static,
{
    fn event(
        state: &mut D,
        handle: &Handle,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &ToplevelData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        use ext_foreign_toplevel_handle_v1::Event;
        let feeders = state.as_mut();
        match event {
            Event::Title { title } => {
                if let Some(p) = feeders.toplevels.pending.get_mut(&handle.id()) {
                    p.title = Some(title);
                }
            }
            Event::AppId { app_id } => {
                if let Some(p) = feeders.toplevels.pending.get_mut(&handle.id()) {
                    p.app_id = Some(app_id);
                }
            }
            Event::Identifier { identifier } => {
                if let Some(p) = feeders.toplevels.pending.get_mut(&handle.id()) {
                    p.identifier = Some(identifier);
                }
            }
            Event::Done => feeders.toplevels.apply_done(handle),
            Event::Closed => feeders.toplevels.closed(handle),
            _ => {}
        }
    }
}
