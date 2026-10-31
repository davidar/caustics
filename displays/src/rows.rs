//! The one frame this sidecar publishes: `APPLET_DISPLAYS`, one `ROW_ITEM`
//! (the chip), one submenu row per output under it, and a last disabled row
//! that says how long a change lasts.
//!
//! An output's submenu, top to bottom: **On** (a check row), a separator, then
//! three submenus of radio rows: **Scale** (1, 1.25, 1.5, 1.75, 2, 2.5, 3, and
//! the current scale when it is none of those), **Mode** (niri's list, in its
//! order, the preferred one named) and **Rotation** (Normal, 90°, 180°, 270°;
//! the flipped transforms are niri's config's). An output that is off has the
//! On row and nothing else: it has no current scale, mode or rotation to mark.
//!
//! Whole-state, like every rows frame. Every id is a constant, or a constant
//! plus an output's **slot** ([`Slots`]) and a row's index in its list, so an
//! open submenu is held by the same id while the outputs are re-read under it
//! (docs/abi.md, *Rows*).

use caustics_bar_abi::buffer::APPLET_DISPLAYS;
use caustics_bar_abi::wire::{
    Frame, MAX_ROWS, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU, RFLAG_RADIO,
    RFLAG_SUBMENU, RowsBuilder,
};

use crate::niri::{Mode, Output};

/// The chip.
pub const ROW_CHIP: u32 = 1;
/// The separator above the last row, and the last row.
pub const ROW_NOTE_SEP: u32 = 2;
pub const ROW_NOTE: u32 = 3;

/// An output's rows are its slot's base plus an offset: slot 0's base is
/// `0x100`, slot 1's `0x200`, and so on.
pub const SLOT_STRIDE: u32 = 0x100;
/// The most outputs the menu lists. More than any desk has; the rest are not
/// rows, and the publish witness says how many there were.
pub const MAX_OUTPUTS: usize = 8;

/// Offsets within a slot.
pub const OFF_OUTPUT: u32 = 0;
pub const OFF_ON: u32 = 1;
pub const OFF_SCALE: u32 = 2;
pub const OFF_MODE: u32 = 3;
pub const OFF_ROTATION: u32 = 4;
pub const OFF_SEP: u32 = 5;
/// A scale's row is this plus its index in [`SCALES`]; the current scale,
/// when it is none of them, is this plus `SCALES.len()`.
pub const OFF_SCALES: u32 = 0x10;
/// A rotation's row is this plus its index in [`ROTATIONS`].
pub const OFF_ROTATIONS: u32 = 0x20;
/// A mode's row is this plus its index in niri's list.
pub const OFF_MODES: u32 = 0x40;
/// The most modes a slot can name: the rest of the stride.
pub const MAX_MODES: usize = (SLOT_STRIDE - OFF_MODES) as usize;

/// The scales offered, the ones a desktop's display settings offer.
pub const SCALES: [f64; 7] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
/// The rotations offered: niri's spelling and the menu's.
pub const ROTATIONS: [(&str, &str); 4] = [
    ("Normal", "Normal"),
    ("90", "90°"),
    ("180", "180°"),
    ("270", "270°"),
];

pub const ICON: &str = "video-display-symbolic";
pub const LABEL_ON: &str = "On";
pub const LABEL_SCALE: &str = "Scale";
pub const LABEL_MODE: &str = "Mode";
pub const LABEL_ROTATION: &str = "Rotation";
/// The last row: a change through this menu is niri's until it restarts, and
/// the layout (and anything that should last) is niri's config file's.
pub const LABEL_NOTE: &str = "Until niri restarts";
pub const BODY_NOTE: &str = "The layout lives in niri's config.";

/// Two scales are the same scale within this: niri reports what it was set
/// to, but a fractional scale is rounded to the nearest 1/120 on the way.
pub const SCALE_EPSILON: f64 = 0.005;

/// Which slot each output name has. An output keeps its slot for as long as
/// niri lists it, so its row ids do not move when another output arrives or
/// leaves; a new output takes the lowest free slot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slots {
    names: [Option<String>; MAX_OUTPUTS],
}

impl Slots {
    /// Assign slots for this list: a name already slotted keeps its slot, a
    /// name gone frees its own, and a new name takes the lowest free one.
    /// Outputs past [`MAX_OUTPUTS`] get none.
    pub fn assign(&mut self, outputs: &[Output]) {
        for slot in self.names.iter_mut() {
            if slot
                .as_ref()
                .is_some_and(|n| !outputs.iter().any(|o| &o.name == n))
            {
                *slot = None;
            }
        }
        for o in outputs {
            if self.slot_of(&o.name).is_some() {
                continue;
            }
            if let Some(free) = self.names.iter_mut().find(|s| s.is_none()) {
                *free = Some(o.name.clone());
            }
        }
    }

    pub fn slot_of(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|s| s.as_deref() == Some(name))
    }

    pub fn name_of(&self, slot: usize) -> Option<&str> {
        self.names.get(slot)?.as_deref()
    }
}

/// The base id of a slot's rows.
pub fn base(slot: usize) -> u32 {
    SLOT_STRIDE * (slot as u32 + 1)
}

/// A row id taken apart: its slot and its offset within it, or `None` for
/// the chip, the note and anything outside the slots.
pub fn split(id: u32) -> Option<(usize, u32)> {
    let slot = (id / SLOT_STRIDE).checked_sub(1)? as usize;
    (slot < MAX_OUTPUTS).then_some((slot, id % SLOT_STRIDE))
}

/// A scale as the menu says it: `1`, `1.25`, `1.5`, at most two decimals.
pub fn scale_label(scale: f64) -> String {
    let s = format!("{scale:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A refresh rate in niri's millihertz as hertz: `60`, `59.94`, `120.001`.
pub fn refresh_label(mhz: u32) -> String {
    let s = format!("{}.{:03}", mhz / 1000, mhz % 1000);
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A mode as the menu says it: `2880×1800 @ 120.001 Hz`.
pub fn mode_label(mode: &Mode) -> String {
    format!(
        "{}×{} @ {} Hz",
        mode.width,
        mode.height,
        refresh_label(mode.refresh)
    )
}

/// A transform as the menu says it: the rotations by their degrees, and a
/// flipped one (which this menu does not set) by niri's own word.
pub fn rotation_label(transform: &str) -> String {
    if let Some((_, label)) = ROTATIONS.iter().find(|(t, _)| *t == transform) {
        return (*label).to_owned();
    }
    match transform.strip_prefix("Flipped") {
        Some("") => "Flipped".to_owned(),
        Some(degrees) => format!("Flipped {degrees}°"),
        None => transform.to_owned(),
    }
}

/// Whether `a` and `b` are the same scale (see [`SCALE_EPSILON`]).
pub fn same_scale(a: f64, b: f64) -> bool {
    (a - b).abs() < SCALE_EPSILON
}

/// The scale rows' values for an output now: [`SCALES`], then its current
/// scale when that is none of them.
pub fn scales_for(output: &Output) -> Vec<f64> {
    let mut scales = SCALES.to_vec();
    if let Some(current) = output.scale
        && !SCALES.iter().any(|s| same_scale(*s, current))
    {
        scales.push(current);
    }
    scales
}

/// How many of each output's modes fit: the frame's row budget, shared
/// evenly, never more than [`MAX_MODES`].
pub fn modes_budget(outputs: usize) -> usize {
    if outputs == 0 {
        return 0;
    }
    // The chip and the note with its separator, then per output its own row,
    // On, the separator, three submenus, every scale and every rotation.
    let fixed = 3 + outputs * (6 + SCALES.len() + 1 + ROTATIONS.len());
    (MAX_ROWS.saturating_sub(fixed) / outputs).min(MAX_MODES)
}

/// The whole frame for these outputs, slotted by `slots` (which
/// [`Slots::assign`] has already seen this list). No outputs is no rows at
/// all: no chip, as when niri is not there.
pub fn picture(revision: u32, outputs: &[Output], slots: &Slots) -> Frame {
    let mut rows = RowsBuilder::new(APPLET_DISPLAYS, revision);
    let listed: Vec<(usize, &Output)> = outputs
        .iter()
        .filter_map(|o| slots.slot_of(&o.name).map(|s| (s, o)))
        .collect();
    if listed.is_empty() {
        return rows.frame();
    }
    let budget = modes_budget(listed.len());
    let chip = rows.item(ROW_CHIP, None, Some(ICON), -1.0, RFLAG_HAS_MENU);
    for (slot, output) in &listed {
        let b = base(*slot);
        let on = if output.on { RFLAG_CHECKED } else { 0 };
        let row = rows.menu(
            chip,
            b + OFF_OUTPUT,
            Some(&output.label()),
            Some(ICON),
            RFLAG_SUBMENU | RFLAG_CHECKABLE | on,
        );
        rows.menu(row, b + OFF_ON, Some(LABEL_ON), None, RFLAG_CHECKABLE | on);
        if !output.on {
            continue;
        }
        rows.separator(row, b + OFF_SEP);

        let scale = output.scale.map(scale_label).unwrap_or_default();
        let scales = rows.menu(
            row,
            b + OFF_SCALE,
            Some(&format!("{LABEL_SCALE} · {scale}")),
            None,
            RFLAG_SUBMENU,
        );
        for (k, value) in scales_for(output).into_iter().enumerate() {
            let checked = output.scale.is_some_and(|s| same_scale(s, value));
            rows.menu(
                scales,
                b + OFF_SCALES + k as u32,
                Some(&scale_label(value)),
                None,
                RFLAG_RADIO | if checked { RFLAG_CHECKED } else { 0 },
            );
        }

        let current = output.current_mode();
        let mode = current.as_ref().map(mode_label).unwrap_or_default();
        let modes = rows.menu(
            row,
            b + OFF_MODE,
            Some(&format!("{LABEL_MODE} · {mode}")),
            None,
            RFLAG_SUBMENU,
        );
        for (k, m) in output.modes.iter().enumerate().take(budget) {
            let label = if m.preferred {
                format!("{} · preferred", mode_label(m))
            } else {
                mode_label(m)
            };
            let checked = output.current == Some(k);
            rows.menu(
                modes,
                b + OFF_MODES + k as u32,
                Some(&label),
                None,
                RFLAG_RADIO | if checked { RFLAG_CHECKED } else { 0 },
            );
        }

        let transform = output.transform.as_deref().unwrap_or("");
        let rotations = rows.menu(
            row,
            b + OFF_ROTATION,
            Some(&format!("{LABEL_ROTATION} · {}", rotation_label(transform))),
            None,
            RFLAG_SUBMENU,
        );
        for (k, (t, label)) in ROTATIONS.iter().enumerate() {
            let checked = *t == transform;
            rows.menu(
                rotations,
                b + OFF_ROTATIONS + k as u32,
                Some(label),
                None,
                RFLAG_RADIO | if checked { RFLAG_CHECKED } else { 0 },
            );
        }
    }
    rows.separator(chip, ROW_NOTE_SEP);
    let note = rows.menu(chip, ROW_NOTE, Some(LABEL_NOTE), None, RFLAG_DISABLED);
    rows.body(note, BODY_NOTE);
    rows.frame()
}

/// The publish witness's account of the outputs: `eDP-1 on
/// 2880x1800@120.001 scale=1.75 Normal`, `DP-1 off`, joined by `; `.
pub fn summary(outputs: &[Output]) -> String {
    if outputs.is_empty() {
        return "none".to_owned();
    }
    outputs
        .iter()
        .map(|o| match (o.on, o.current_mode()) {
            (true, Some(m)) => format!(
                "{} on {}x{}@{} scale={} {}",
                o.name,
                m.width,
                m.height,
                refresh_label(m.refresh),
                o.scale.map(scale_label).unwrap_or_default(),
                o.transform.as_deref().unwrap_or("?"),
            ),
            (true, None) => format!("{} on", o.name),
            (false, _) => format!("{} off", o.name),
        })
        .collect::<Vec<_>>()
        .join("; ")
}
