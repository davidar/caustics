//! The one frame this sidecar publishes: `APPLET_AUDIO`, one bar item and
//! its menu: the slider, the two mutes, the Output and Input submenus with
//! one radio row per device, and the settings row when there is a settings
//! program to open.
//!
//! Whole-state, like every rows frame: there is never a delta to track, and a
//! restart of either end needs no replay. The ids are constants, so a volume
//! moving changes a value and nothing else — which is what keeps the bar on
//! its upload path instead of a rebuild, and keeps the menu from closing under
//! the drag that is changing it (docs/abi.md, *Rows*).

use caustics_bar_abi::buffer::APPLET_AUDIO;
use caustics_bar_abi::wire::{
    Frame, RFLAG_ATTENTION, RFLAG_CHECKABLE, RFLAG_CHECKED, RFLAG_DISABLED, RFLAG_HAS_MENU,
    RFLAG_RADIO, RFLAG_SHOW, RFLAG_SUBMENU, RowRef, RowsBuilder,
};

use crate::osd::Flagged;
use crate::wpctl::{Audio, Device, Devices};

/// The row ids. Small integers, stable across every frame this process writes
/// — which is the contract: an `Action` comes back naming one of these, and
/// the bar's registry keys a row's identity on it (docs/abi.md, *Rows*).
pub const ROW_AUDIO: u32 = 1;
pub const ROW_VOLUME: u32 = 2;
pub const ROW_MUTE: u32 = 3;
pub const ROW_MIC: u32 = 4;
pub const ROW_SEP: u32 = 5;
pub const ROW_SETTINGS: u32 = 6;
/// The two device submenus, the separator between them
/// and the settings row, and the disabled row a submenu with nothing in it
/// carries. Ids are never renumbered, so these take the numbers after the six above: a menu open
/// across the sidecar's upgrade holds its level by id.
pub const ROW_OUTPUT: u32 = 7;
pub const ROW_INPUT: u32 = 8;
pub const ROW_SEP_DEVICES: u32 = 9;
pub const ROW_NO_OUTPUT: u32 = 10;
pub const ROW_NO_INPUT: u32 = 11;

/// A device row's id is its kind in the high half and PipeWire's node id in
/// the low: `(1 << 16) | id` for a sink, `(2 << 16) | id` for a source. The
/// node id is what `wpctl set-default` takes, so the action needs no table,
/// and it is stable for as long as the node exists — which is exactly as long
/// as the row should be. A node id that does not fit the low half (PipeWire's
/// ids are small and recycled; none has been seen above a few hundred) is not
/// listed rather than aliased.
pub const KIND_OUTPUT: u32 = 1;
pub const KIND_INPUT: u32 = 2;
pub const DEVICE_ID_MASK: u32 = 0xFFFF;

/// The row id for a device of `kind` (`KIND_OUTPUT`/`KIND_INPUT`), or `None`
/// when the node id does not fit.
pub fn device_row(kind: u32, node: u32) -> Option<u32> {
    (node <= DEVICE_ID_MASK).then_some((kind << 16) | node)
}

/// The other way: a row id to `(kind, node id)`, when it is a device row.
pub fn device_of(row: u32) -> Option<(u32, u32)> {
    let kind = row >> 16;
    matches!(kind, KIND_OUTPUT | KIND_INPUT).then_some((kind, row & DEVICE_ID_MASK))
}

/// The slider's own units: whole percent, the same thing `wpctl set-volume`
/// takes. The bar normalises on the way in and de-normalises the action on the
/// way out (docs/abi.md, *Action*), so these two numbers are the whole contract
/// for a drag.
pub const VOLUME_MIN: f32 = 0.0;
pub const VOLUME_MAX: f32 = 100.0;

/// The labels, in one place because the tests read them from here.
pub const LABEL_VOLUME: &str = "Volume";
pub const LABEL_MUTE: &str = "Mute";
pub const LABEL_MIC: &str = "Mute microphone";
pub const LABEL_SETTINGS: &str = "Sound Settings";
pub const LABEL_OUTPUT: &str = "Output";
pub const LABEL_INPUT: &str = "Input";
pub const LABEL_NO_DEVICE: &str = "No device";

/// The icon on the bar, by bucket. All five are in Adwaita;
/// `-overamplified-` is the one that only appears above 100%,
/// which PipeWire allows and the slider does not offer.
pub const ICON_MUTED: &str = "audio-volume-muted-symbolic";
pub const ICON_LOW: &str = "audio-volume-low-symbolic";
pub const ICON_MEDIUM: &str = "audio-volume-medium-symbolic";
pub const ICON_HIGH: &str = "audio-volume-high-symbolic";
pub const ICON_OVERAMPLIFIED: &str = "audio-volume-overamplified-symbolic";

/// The menu rows' own icons. A checkbox already says whether it is on, so
/// each icon only has to say what the row is *about*: the speaker for the
/// sink's mute, the microphone for the source's.
pub const ICON_MIC: &str = "audio-input-microphone-symbolic";
pub const ICON_SETTINGS: &str = "preferences-system-symbolic";
/// The two submenus' icons, both in Adwaita's `devices/`. The device rows under them carry none: the radio mark is the
/// row's whole state.
pub const ICON_OUTPUT: &str = "audio-speakers-symbolic";
pub const ICON_INPUT: &str = ICON_MIC;

/// Which bucket a volume falls in. Muted wins over every percentage, because
/// a muted 80% is silence and the bar should say so.
pub fn icon_for(percent: f32, muted: bool) -> &'static str {
    if muted {
        return ICON_MUTED;
    }
    match percent {
        p if p < 34.0 => ICON_LOW,
        p if p < 67.0 => ICON_MEDIUM,
        p if p <= 100.0 => ICON_HIGH,
        _ => ICON_OVERAMPLIFIED,
    }
}

/// The whole picture. `None` is the empty frame: no `wpctl`, or no default
/// sink. The chip disappears and the bar's health line still says `running`;
/// a missing sound server costs this chip and nothing else.
///
/// `settings` says whether a settings program can be named
/// ([`crate::actions::settings_available`]). Without one the settings row and
/// the separator above it are left out: a row that opens nothing is worse
/// than no row.
pub fn picture(revision: u32, audio: Option<&Audio>, devices: &Devices, settings: bool) -> Frame {
    toasting(revision, audio, devices, settings, Flagged::NONE)
}

/// The same picture, with the on-screen display's bits (`crate::osd`): the
/// chip carries `RFLAG_SHOW` while any row is flagged, and each flagged row
/// `RFLAG_ATTENTION`, which is what the caustic opens a toast from
/// (docs/abi.md, **Toasts**). Ids, order and every other flag are the plain
/// picture's, so a toast's slider follows the volume in place.
pub fn toasting(
    revision: u32,
    audio: Option<&Audio>,
    devices: &Devices,
    settings: bool,
    osd: Flagged,
) -> Frame {
    let mut rows = RowsBuilder::new(APPLET_AUDIO, revision);
    let Some(audio) = audio else {
        return rows.frame();
    };
    let percent = audio.percent();
    let item = rows.item(
        ROW_AUDIO,
        None,
        Some(icon_for(percent, audio.muted)),
        // The level the caustic draws on the icon is a fraction, so an
        // overamplified sink draws a full bar rather than an overflowing one.
        audio.volume.clamp(0.0, 1.0),
        RFLAG_HAS_MENU | if osd.any() { RFLAG_SHOW } else { 0 },
    );
    // A volume above 100 shows as a full slider: the track is the range a drag
    // may ask for, and a drag never asks to overamplify.
    rows.slider(
        item,
        ROW_VOLUME,
        Some(LABEL_VOLUME),
        percent.clamp(VOLUME_MIN, VOLUME_MAX),
        VOLUME_MIN,
        VOLUME_MAX,
        attention(osd.volume),
    );
    rows.menu(
        item,
        ROW_MUTE,
        Some(LABEL_MUTE),
        Some(ICON_MUTED),
        checkbox(audio.muted) | attention(osd.mute),
    );
    rows.menu(
        item,
        ROW_MIC,
        Some(LABEL_MIC),
        Some(ICON_MIC),
        checkbox(audio.mic_muted) | attention(osd.mic),
    );
    rows.separator(item, ROW_SEP);
    let output = rows.menu(
        item,
        ROW_OUTPUT,
        Some(LABEL_OUTPUT),
        Some(ICON_OUTPUT),
        RFLAG_SUBMENU,
    );
    choices(
        &mut rows,
        output,
        KIND_OUTPUT,
        &devices.outputs,
        ROW_NO_OUTPUT,
    );
    let input = rows.menu(
        item,
        ROW_INPUT,
        Some(LABEL_INPUT),
        Some(ICON_INPUT),
        RFLAG_SUBMENU,
    );
    choices(&mut rows, input, KIND_INPUT, &devices.inputs, ROW_NO_INPUT);
    if settings {
        rows.separator(item, ROW_SEP_DEVICES);
        rows.menu(
            item,
            ROW_SETTINGS,
            Some(LABEL_SETTINGS),
            Some(ICON_SETTINGS),
            0,
        );
    }
    rows.frame()
}

/// One submenu's children: a radio row per device, checked on the default —
/// one device is still listed, because the submenu is also where the menu
/// says *which* device is playing — or the one disabled "No device" row, so a
/// submenu is never empty.
fn choices(rows: &mut RowsBuilder, parent: RowRef, kind: u32, devices: &[Device], none: u32) {
    let mut listed = 0;
    for device in devices {
        let Some(id) = device_row(kind, device.id) else {
            continue;
        };
        let mut flags = RFLAG_RADIO | RFLAG_CHECKABLE;
        if device.default {
            flags |= RFLAG_CHECKED;
        }
        rows.menu(parent, id, Some(&device.label), None, flags);
        listed += 1;
    }
    if listed == 0 {
        rows.menu(parent, none, Some(LABEL_NO_DEVICE), None, RFLAG_DISABLED);
    }
}

/// A checkable row's flags, checked or not.
fn checkbox(checked: bool) -> u32 {
    RFLAG_CHECKABLE | if checked { RFLAG_CHECKED } else { 0 }
}

/// `RFLAG_ATTENTION` on a row the on-screen display shows.
fn attention(flagged: bool) -> u32 {
    if flagged { RFLAG_ATTENTION } else { 0 }
}
