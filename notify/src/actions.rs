//! Actions back: what a click on a row
//! does, decided by a pure table so `notify/tests/` can ask it without a bus.

use caustics_bar_abi::wire::{
    ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN, ACTION_MIDDLE, ACTION_SECONDARY,
    ACTION_TOAST_SHOWN,
};

use crate::history::{DEFAULT_ACTION, History};
use crate::rows::{INDEX_DISMISS, ROW_CHIP, ROW_CLEAR, ROW_DND, ROW_SETTINGS, split_action_row};
use crate::witness;
use caustics_sidecar::settings::{self, Panel};

/// One thing to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// The bar opened this applet's menu: everything is read, and the minute
    /// re-send starts.
    MenuOpen,
    /// It closed: the re-send stops.
    MenuClose,
    ToggleDnd,
    ClearAll,
    Settings,
    /// `ActionInvoked(id, key)` when `key` is some, then
    /// `NotificationClosed(id, 2)` unless `keep` (a resident notification
    /// whose action was invoked).
    Invoke {
        id: u32,
        key: Option<String>,
        keep: bool,
    },
    /// A toast that showed notification `id` has ended: it is shown, and
    /// toasts no more.
    ToastShown {
        id: u32,
    },
}

/// The settings page the last row opens; which program shows it is
/// `caustics_sidecar::settings`' choice.
pub const SETTINGS_PANEL: Panel = Panel::Notifications;

/// Whether a settings program can be named for [`SETTINGS_PANEL`]; the
/// picture leaves the settings row out when none can.
pub fn settings_available() -> bool {
    settings::settings_command(SETTINGS_PANEL).is_some()
}

/// The table. `None` is a row that does nothing for that kind of click.
pub fn op_for(history: &History, row: u32, kind: u32) -> Option<Op> {
    match (row, kind) {
        (_, ACTION_MENU_OPEN) => Some(Op::MenuOpen),
        (_, ACTION_MENU_CLOSE) => Some(Op::MenuClose),
        // The bar says which rows a toast showed, one action per row; a
        // toast's rows are notifications' own rows, so the row is the id.
        (row, ACTION_TOAST_SHOWN) => history.get(row).map(|_| Op::ToastShown { id: row }),
        // The chip opens its menu on a click; a middle click on the bell is
        // the do-not-disturb switch.
        (ROW_CHIP, ACTION_MIDDLE) | (ROW_DND, ACTION_ACTIVATE) => Some(Op::ToggleDnd),
        // A right click on a notification's row — in a toast or the menu — is
        // a dismissal: closed, reason 2, no action invoked.
        (row, ACTION_SECONDARY) => {
            let id = match split_action_row(row) {
                Some((id, _)) => id,
                None => row,
            };
            history.get(id).map(|_| Op::Invoke {
                id,
                key: None,
                keep: false,
            })
        }
        (ROW_CLEAR, ACTION_ACTIVATE) => Some(Op::ClearAll),
        (ROW_SETTINGS, ACTION_ACTIVATE) => Some(Op::Settings),
        (row, ACTION_ACTIVATE) => match split_action_row(row) {
            Some((id, INDEX_DISMISS)) => history.get(id).map(|_| Op::Invoke {
                id,
                key: None,
                keep: false,
            }),
            Some((id, index)) => {
                let entry = history.get(id)?;
                let (key, _) = entry.actions.get(index as usize)?;
                Some(Op::Invoke {
                    id,
                    key: Some(key.clone()),
                    keep: entry.resident,
                })
            }
            // The row itself: its default action if it has one; without one
            // the click is a dismissal, resident or not.
            None => {
                let entry = history.get(row)?;
                if entry.has_default() {
                    Some(Op::Invoke {
                        id: row,
                        key: Some(DEFAULT_ACTION.to_owned()),
                        keep: entry.resident,
                    })
                } else {
                    Some(Op::Invoke {
                        id: row,
                        key: None,
                        keep: false,
                    })
                }
            }
        },
        _ => None,
    }
}

/// The settings page, detached (`caustics_sidecar::settings::open_settings`),
/// and one witness line saying where it went.
pub fn spawn_settings() {
    witness(&format!(
        "settings: {}",
        settings::open_settings(SETTINGS_PANEL)
    ));
}
