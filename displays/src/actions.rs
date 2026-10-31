//! What an `Action` frame on stdin means.
//!
//! A **pure table**: a row id, the action's kind and the outputs as last read
//! to a [`Decision`], which names the output and the [`Change`] the click
//! asks for, or why it asks for nothing. The loop ([`crate::serve`]) sends
//! the request; the tests assert the table with no niri at all.

use caustics_bar_abi::buffer::{ACTION_ACTIVATE, ACTION_MENU_CLOSE, ACTION_MENU_OPEN};

use crate::niri::{Change, Output};
use crate::rows::{
    OFF_MODES, OFF_ON, OFF_ROTATIONS, OFF_SCALES, ROTATIONS, SLOT_STRIDE, Slots, modes_budget,
    scales_for, split,
};

/// What a click decides.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Ask niri for this change to this output.
    Change { output: String, change: Change },
    /// The On row of the only output that is on: turning it off would leave
    /// the session with no screen, so the click is refused and says so.
    LastOn { output: String },
    /// A row that performs nothing (the chip, a submenu, a separator, the
    /// note, a kind with no meaning here, an output that has gone).
    Nothing,
}

/// The table. Only `ACTION_ACTIVATE` performs anything; the menu notices are
/// the loop's ([`is_menu_notice`]).
pub fn decide(row: u32, kind: u32, outputs: &[Output], slots: &Slots) -> Decision {
    if kind != ACTION_ACTIVATE {
        return Decision::Nothing;
    }
    let Some((slot, off)) = split(row) else {
        return Decision::Nothing;
    };
    let Some(output) = slots
        .name_of(slot)
        .and_then(|name| outputs.iter().find(|o| o.name == name))
    else {
        return Decision::Nothing;
    };
    let change = |change| Decision::Change {
        output: output.name.clone(),
        change,
    };
    if off == OFF_ON {
        if !output.on {
            return change(Change::On);
        }
        if outputs.iter().filter(|o| o.on).count() <= 1 {
            return Decision::LastOn {
                output: output.name.clone(),
            };
        }
        return change(Change::Off);
    }
    // The radio rows exist only for an output that is on.
    if !output.on {
        return Decision::Nothing;
    }
    let listed = slots_listed(outputs, slots);
    match off {
        off if (OFF_SCALES..OFF_ROTATIONS).contains(&off) => {
            match scales_for(output).get((off - OFF_SCALES) as usize) {
                Some(scale) => change(Change::Scale(*scale)),
                None => Decision::Nothing,
            }
        }
        off if (OFF_ROTATIONS..OFF_MODES).contains(&off) => {
            match ROTATIONS.get((off - OFF_ROTATIONS) as usize) {
                Some((transform, _)) => change(Change::Transform(transform)),
                None => Decision::Nothing,
            }
        }
        off if (OFF_MODES..SLOT_STRIDE).contains(&off) => {
            let k = (off - OFF_MODES) as usize;
            match output.modes.get(k) {
                Some(mode) if k < modes_budget(listed) => change(Change::Mode(*mode)),
                _ => Decision::Nothing,
            }
        }
        _ => Decision::Nothing,
    }
}

/// How many of these outputs have a slot: the count the frame's row budget
/// was shared by, so a mode row past the budget (never sent) means nothing.
fn slots_listed(outputs: &[Output], slots: &Slots) -> usize {
    outputs
        .iter()
        .filter(|o| slots.slot_of(&o.name).is_some())
        .count()
}

/// `ACTION_MENU_OPEN` and `ACTION_MENU_CLOSE`: the outputs are read when the
/// menu opens and every [`crate::POLL`] while it stays up, and never on a
/// clock while it is closed.
pub fn is_menu_notice(kind: u32) -> bool {
    kind == ACTION_MENU_OPEN || kind == ACTION_MENU_CLOSE
}

/// The witness's name for a change: `on`, `off`, `scale 1.5`,
/// `mode 1920x1080@60`, `rotation 90`.
pub fn change_name(change: &Change) -> String {
    match change {
        Change::On => "on".to_owned(),
        Change::Off => "off".to_owned(),
        Change::Scale(s) => format!("scale {}", crate::rows::scale_label(*s)),
        Change::Mode(m) => format!(
            "mode {}x{}@{}",
            m.width,
            m.height,
            crate::rows::refresh_label(m.refresh)
        ),
        Change::Transform(t) => format!("rotation {t}"),
    }
}
