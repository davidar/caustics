//! The one frame this sidecar publishes: `APPLET_NETWORK`, one bar item, the
//! Wi-Fi switch, a radio row per known Wi-Fi network, the networks in range
//! that no profile claims, and the settings row when there is a settings
//! program to open.
//!
//! Whole-state, like every rows frame: there is never a delta to track, and a
//! restart of either end needs no replay.

use caustics_bar_abi::buffer::APPLET_NETWORK;
use caustics_bar_abi::wire::{
    RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_RADIO, Rows, RowsBuilder,
};

use crate::model::{self, Snapshot};

/// The fixed row ids. Small integers, stable across every frame this process
/// writes — which is the contract: an `Action` comes back naming one of these,
/// and the bar's registry keys a menu row's identity on it (docs/abi.md
/// *Rows*). A known network's id is [`model::row_id`] instead and one in
/// range is [`model::visible_row_id`]; each carries a bit that keeps it out of
/// this range and out of the other's.
pub const ITEM: u32 = 1;
pub const WIFI: u32 = 2;
pub const SEP_TOP: u32 = 3;
pub const SEP_BOTTOM: u32 = 4;
pub const SETTINGS: u32 = 5;
/// Between the networks in range and the settings row. Absent with that
/// section, so a menu with nothing unknown in range has no empty gap. Its id is 6, after `SETTINGS`, because ids are never
/// renumbered.
pub const SEP_VISIBLE: u32 = 6;

pub const WIFI_LABEL: &str = "Wi-Fi";
pub const SETTINGS_LABEL: &str = "Network Settings";
pub const SETTINGS_ICON: &str = "preferences-system-network-symbolic";

/// No rows at all: the frame that says "there is no NetworkManager". The chip
/// disappears and the bar's health line still says `running`; a missing
/// service costs its chip and nothing else. It is also the first frame this sidecar
/// writes, before the bus has answered — an absent chip for a few milliseconds
/// rather than a wrong one.
pub fn empty(revision: u32) -> Rows {
    RowsBuilder::new(APPLET_NETWORK, revision).build()
}

/// The whole picture. The returned [`Rows`] is compared with the last one
/// published — everything but its `revision` — which is how a `Strength`
/// signal every six seconds turns into no frame at all.
///
/// `settings` says whether a settings program can be named
/// ([`crate::actions::settings_available`]). Without one the settings row is
/// left out, and so is any separator that would then end the menu: a row
/// that opens nothing is worse than no row.
pub fn picture(revision: u32, state: &Snapshot, settings: bool) -> Rows {
    let mut rows = RowsBuilder::new(APPLET_NETWORK, revision);
    let item = rows.item(ITEM, None, Some(state.icon()), -1.0, RFLAG_HAS_MENU);

    let switch = RFLAG_CHECKABLE
        | if state.wireless_enabled {
            RFLAG_CHECKED
        } else {
            0
        };
    rows.menu(item, WIFI, Some(WIFI_LABEL), None, switch);
    if settings || !state.networks.is_empty() || !state.visible.is_empty() {
        rows.separator(item, SEP_TOP);
    }

    for network in &state.networks {
        // Radio, because exactly one Wi-Fi network is joined at a time and
        // clicking the joined one is not a request to do anything.
        let mut flags = RFLAG_RADIO | RFLAG_CHECKABLE;
        if network.active {
            flags |= RFLAG_CHECKED;
        }
        // Greyed out only while the radio is off; an unseen network keeps its
        // no-signal icon and stays clickable (`Snapshot::joinable` says why).
        if !state.joinable(network) {
            flags |= RFLAG_DISABLED;
        }
        rows.menu(
            item,
            network.row,
            Some(&network.label),
            Some(model::bucket(network.strength)),
            flags,
        );
    }
    if !state.networks.is_empty() && (settings || !state.visible.is_empty()) {
        rows.separator(item, SEP_BOTTOM);
    }

    // The networks in range that no profile claims. **No `CHECKABLE` and no
    // `RADIO`**: the missing mark is what tells them from the known ones
    // above, and it is honest — none of them is a thing this applet can join.
    // A click opens the settings program's network page, because there is no
    // password entry in the bar, ever.
    for network in &state.visible {
        rows.menu(
            item,
            network.row,
            Some(&network.label()),
            Some(model::bucket(Some(network.strength))),
            0,
        );
    }
    if settings {
        if !state.visible.is_empty() {
            rows.separator(item, SEP_VISIBLE);
        }
        rows.menu(item, SETTINGS, Some(SETTINGS_LABEL), Some(SETTINGS_ICON), 0);
    }
    rows.build()
}
