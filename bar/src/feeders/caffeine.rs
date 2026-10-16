//! `zwp_idle_inhibit_manager_v1` — the one applet that stays in the bar's
//! process.
//!
//! The bar's membership test (docs/architecture.md, *Sidecars*): a
//! feeder lives here **iff** it owns a Wayland object on
//! the bar's connection. Caffeine does — a `zwp_idle_inhibitor_v1` on the
//! bar's own layer surface — so unlike audio, network and bluetooth it is not
//! a sidecar over a pipe.
//!
//! The whole protocol is three requests and **no events at all**: the
//! manager's `destroy` and `create_inhibitor`, and the inhibitor's `destroy`.
//! Both `Dispatch` impls are therefore empty bodies, and both still have to
//! exist — they are what the `Feeders::bind` bounds and `delegate_feeders!`
//! name, and the `wiring` probe in `feeders/tests.rs` is what type-checks them without a
//! compositor.
//!
//! An inhibitor is **never persisted**. The bar starts with the session free
//! to idle every time, because "the screen never sleeps" is not a state to
//! wake up into.
//!
//! # The inhibit is weaker than it looks, and the bar cannot fix that
//!
//! niri counts an inhibitor only while its surface is actually being scanned
//! out (niri's `src/niri.rs`, `refresh_idle_inhibit`, which tests
//! `surface_primary_scanout_output(...).is_some()`), not merely while it is
//! mapped. The bar's layer surface is created once, anchored with an exclusive
//! zone and never unmapped, so in practice it is always scanned out; a bar
//! that were fully occluded would silently stop inhibiting. Nothing here can
//! do better, and a second surface to hold the inhibitor on would be a lie
//! about what the bar is.

use smithay_client_toolkit::reexports::protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle};

use super::{FeederData, Feeders, InhibitQueue};
use crate::model::{Caffeine as State, Update, Updates};

/// The idle-inhibit feeder.
pub struct Caffeine {
    /// `None` when the compositor offered no manager: the chip is not
    /// published and a toggle is a no-op.
    manager: Option<ZwpIdleInhibitManagerV1>,
    /// The live inhibitor, which *is* the "on" state.
    inhibitor: Option<ZwpIdleInhibitorV1>,
    /// The bar's own layer surface, cloned at bind time. Whose surface the
    /// inhibitor hangs on is the protocol's only argument, and the bar has
    /// exactly one.
    surface: WlSurface,
    /// A `QueueHandle<D>` with the embassy's state type erased, because
    /// `create_inhibitor` happens at click time and not at bind time — the
    /// same problem the capture feeder solved (`super::CaptureQueue`).
    queue: Box<dyn InhibitQueue>,
    updates: Updates,
    /// How many times it has been flipped this run, for the health line: an
    /// inhibitor left on for a week is the failure worth seeing.
    toggles: u64,
}

impl std::fmt::Debug for Caffeine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caffeine")
            .field("state", &self.state())
            .field("toggles", &self.toggles)
            .finish()
    }
}

impl Caffeine {
    /// Take the bound manager, if there was one, and tell the model at once:
    /// the chip exists only while the model says the feature does, so the bar
    /// needs the `Off` as much as it needs the `Absent` that `bind_optional`
    /// already sent for a compositor without the global.
    pub fn new(
        manager: Option<ZwpIdleInhibitManagerV1>,
        surface: WlSurface,
        queue: Box<dyn InhibitQueue>,
        updates: Updates,
    ) -> Self {
        let caffeine = Self {
            manager,
            inhibitor: None,
            surface,
            queue,
            updates,
            toggles: 0,
        };
        if caffeine.manager.is_some() {
            let _ = caffeine.updates.send(Update::Caffeine(State::Off));
        }
        caffeine
    }

    /// Whether the global was there at all.
    pub fn is_bound(&self) -> bool {
        self.manager.is_some()
    }

    /// Absent, off or on — derived from what this feeder holds, so there is no
    /// second copy of the state to disagree with the objects.
    pub fn state(&self) -> State {
        match (&self.manager, &self.inhibitor) {
            (None, _) => State::Absent,
            (Some(_), None) => State::Off,
            (Some(_), Some(_)) => State::On,
        }
    }

    /// Flips held↔free and says so in one witness line. `Absent` cannot be left: with
    /// no manager there is no chip to click, and a stale mirror naming one is
    /// answered with the state unchanged rather than with a panic.
    pub fn toggle(&mut self) -> State {
        let Some(manager) = self.manager.as_ref() else {
            return State::Absent;
        };
        match self.inhibitor.take() {
            Some(inhibitor) => {
                inhibitor.destroy();
                tracing::info!("caffeine: inhibitor released");
            }
            None => {
                self.inhibitor = Some(self.queue.create_inhibitor(manager, &self.surface));
                tracing::info!("caffeine: inhibitor held");
            }
        }
        self.toggles += 1;
        self.state()
    }

    /// Toggles since the process started, for the hourly health line.
    pub fn toggles(&self) -> u64 {
        self.toggles
    }

    /// Perform [`crate::model::Action::ToggleCaffeine`]: flip it and publish
    /// the result like any other feeder's update.
    pub fn perform(&mut self) {
        let state = self.toggle();
        let _ = self.updates.send(Update::Caffeine(state));
    }
}

// ---------------------------------------------------------------------------
// Dispatch — two empty bodies, because neither interface has an event
// ---------------------------------------------------------------------------

impl<D> Dispatch<ZwpIdleInhibitManagerV1, FeederData, D> for Feeders
where
    D: Dispatch<ZwpIdleInhibitManagerV1, FeederData> + AsMut<Feeders> + 'static,
{
    fn event(
        _: &mut D,
        _: &ZwpIdleInhibitManagerV1,
        _: <ZwpIdleInhibitManagerV1 as wayland_client::Proxy>::Event,
        _: &FeederData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        // `zwp_idle_inhibit_manager_v1` has no events (idle-inhibit-unstable-v1).
    }
}

impl<D> Dispatch<ZwpIdleInhibitorV1, FeederData, D> for Feeders
where
    D: Dispatch<ZwpIdleInhibitorV1, FeederData> + AsMut<Feeders> + 'static,
{
    fn event(
        _: &mut D,
        _: &ZwpIdleInhibitorV1,
        _: <ZwpIdleInhibitorV1 as wayland_client::Proxy>::Event,
        _: &FeederData,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        // `zwp_idle_inhibitor_v1` has no events either.
    }
}

#[cfg(test)]
mod tests;
