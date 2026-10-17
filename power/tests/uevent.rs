//! The kernel uevent parser, on whole messages as the socket delivers them:
//! `"<action>@<devpath>\0KEY=VALUE\0…"`. Only a backlight's `change` wakes
//! the loop; another subsystem, another action, a libudev-framed message and
//! a message cut short do not.
//!
//! **The fixtures** are written out in the kernel's own format
//! (`lib/kobject_uevent.c`: the header, then `ACTION`, `DEVPATH`,
//! `SUBSYSTEM`, the device's own keys and `SEQNUM`, each NUL-terminated),
//! with the keys and order a backlight write and a battery update carry.
//! They are not recorded from this machine: recording a backlight change
//! means writing the backlight, which no test here does.

use caustics_power::uevent::{self, Uevent, is_backlight_change, parse};

/// A write to `/sys/class/backlight/amdgpu_bl1/brightness`.
const BACKLIGHT: &[u8] =
    b"change@/devices/pci0000:00/0000:00:08.1/0000:c4:00.0/drm/card1/card1-eDP-1/amdgpu_bl1\0\
ACTION=change\0\
DEVPATH=/devices/pci0000:00/0000:00:08.1/0000:c4:00.0/drm/card1/card1-eDP-1/amdgpu_bl1\0\
SUBSYSTEM=backlight\0\
SOURCE=sysfs\0\
SEQNUM=5121\0";

/// A battery's periodic update: the same action on another subsystem.
const BATTERY: &[u8] = b"change@/devices/LNXSYSTM:00/LNXSYBUS:00/PNP0C0A:00/power_supply/BAT0\0\
ACTION=change\0\
DEVPATH=/devices/LNXSYSTM:00/LNXSYBUS:00/PNP0C0A:00/power_supply/BAT0\0\
SUBSYSTEM=power_supply\0\
POWER_SUPPLY_NAME=BAT0\0\
POWER_SUPPLY_STATUS=Discharging\0\
POWER_SUPPLY_CAPACITY=71\0\
SEQNUM=5122\0";

/// A backlight device being added (a driver loading): not a change.
const ADDED: &[u8] = b"add@/devices/platform/acpi_video0/backlight/acpi_video0\0\
ACTION=add\0\
DEVPATH=/devices/platform/acpi_video0/backlight/acpi_video0\0\
SUBSYSTEM=backlight\0\
SEQNUM=40\0";

#[test]
fn a_backlight_change_is_one() {
    assert!(is_backlight_change(BACKLIGHT));
    assert_eq!(
        parse(BACKLIGHT),
        Some(Uevent {
            action: "change",
            devpath: "/devices/pci0000:00/0000:00:08.1/0000:c4:00.0/drm/card1/card1-eDP-1/amdgpu_bl1",
            subsystem: Some(uevent::SUBSYSTEM),
        })
    );
}

#[test]
fn another_subsystem_or_action_is_not() {
    assert!(!is_backlight_change(BATTERY));
    assert_eq!(
        parse(BATTERY).and_then(|e| e.subsystem),
        Some("power_supply")
    );
    assert!(!is_backlight_change(ADDED));
    assert_eq!(parse(ADDED).map(|e| e.action), Some("add"));
}

#[test]
fn a_truncated_message_is_not() {
    // Cut before the `SUBSYSTEM` key: which device it is cannot be told.
    let cut = BACKLIGHT
        .windows(b"SUBSYSTEM".len())
        .position(|w| w == b"SUBSYSTEM")
        .unwrap();
    assert!(!is_backlight_change(&BACKLIGHT[..cut]));
    // Cut inside the key's value.
    assert!(!is_backlight_change(
        &BACKLIGHT[..cut + "SUBSYSTEM=back".len()]
    ));
    // Cut inside the header: no `@`, no message.
    assert_eq!(parse(&BACKLIGHT[..4]), None);
    assert_eq!(parse(b""), None);
    // Without the trailing NUL the last key still reads: the kernel ends
    // every key with one, but a datagram's length is its end.
    let unterminated = &BACKLIGHT[..BACKLIGHT.len() - 1];
    assert!(is_backlight_change(unterminated));
}

#[test]
fn what_is_not_a_kernel_uevent_is_not() {
    // udev's re-broadcast (group 2) opens with its own binary header.
    let mut libudev = b"libudev\0\xfe\xed\xca\xfe".to_vec();
    libudev.extend_from_slice(&[0u8; 32]);
    assert_eq!(parse(&libudev), None);
    // A header whose devpath is not a path, or whose keys contradict it.
    assert_eq!(parse(b"change@nowhere\0SUBSYSTEM=backlight\0"), None);
    assert!(!is_backlight_change(
        b"change@/devices/x\0ACTION=add\0SUBSYSTEM=backlight\0"
    ));
    // Bytes that are not UTF-8 in the header.
    assert_eq!(parse(b"\xffchange@/devices/x\0"), None);
}

#[test]
fn the_socket_opens_unprivileged() {
    // Listening is all it does; nothing is read or written on the device.
    // The kernel lets any process bind the uevent group.
    let fd = uevent::open().expect("a NETLINK_KOBJECT_UEVENT socket");
    drop(fd);
}
