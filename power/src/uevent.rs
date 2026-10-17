//! The kernel's word that the backlight changed: a `NETLINK_KOBJECT_UEVENT`
//! socket bound to multicast group 1, the kernel's own feed.
//!
//! sysfs does not inotify, but every write to
//! `/sys/class/backlight/<name>/brightness` makes the kernel emit a `change`
//! uevent for that device with `SOURCE=sysfs`, whoever wrote it (a brightness
//! key's handler, logind's `SetBrightness`, a shell) and even when the value
//! written is the one already there. An unprivileged process may listen.
//! Group 2 is udev's re-broadcast in libudev's own binary framing, and is not
//! what this reads.
//!
//! A message is `"<action>@<devpath>\0KEY=VALUE\0…"`. [`is_backlight_change`]
//! is the whole parser, a pure function over those bytes, so
//! `power/tests/uevent.rs` feeds it captured messages. The socket is opened by
//! hand over `libc` rather than through libudev: the sidecar links no C
//! library it does not need, and the host this is built on lacks the linker's
//! unversioned `.so` names.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc::UnboundedSender;

use crate::witness;

/// The kernel's multicast group on `NETLINK_KOBJECT_UEVENT`.
pub const GROUP_KERNEL: u32 = 1;

/// The subsystem the slider's device is in.
pub const SUBSYSTEM: &str = "backlight";

/// The largest message read. A uevent's environment is capped by the kernel
/// at 2 KiB (`UEVENT_BUFFER_SIZE`), so this is twice what can arrive.
const BUFFER: usize = 4096;

/// The fields of one uevent this sidecar looks at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uevent<'a> {
    pub action: &'a str,
    pub devpath: &'a str,
    pub subsystem: Option<&'a str>,
}

/// One message, read: the `action@devpath` header and the `ACTION` and
/// `SUBSYSTEM` keys. `None` for anything that is not a kernel uevent: no
/// header, a header without its `@`, a libudev-framed message, bytes that are
/// not UTF-8, or keys that contradict the header (a truncated or garbled
/// message is not guessed at).
pub fn parse(bytes: &[u8]) -> Option<Uevent<'_>> {
    let mut fields = bytes.split(|b| *b == 0).filter(|f| !f.is_empty());
    let header = std::str::from_utf8(fields.next()?).ok()?;
    let (action, devpath) = header.split_once('@')?;
    if action.is_empty() || !devpath.starts_with('/') {
        return None;
    }
    let mut keyed_action = None;
    let mut subsystem = None;
    for field in fields {
        let Ok(field) = std::str::from_utf8(field) else {
            continue;
        };
        match field.split_once('=') {
            Some(("ACTION", value)) => keyed_action = Some(value),
            Some(("SUBSYSTEM", value)) => subsystem = Some(value),
            _ => {}
        }
    }
    if keyed_action.is_some_and(|keyed| keyed != action) {
        return None;
    }
    Some(Uevent {
        action,
        devpath,
        subsystem,
    })
}

/// Whether a message is a backlight's `change`: the one this sidecar wakes for.
/// Every other subsystem and action is ignored, and so is a message cut off
/// before its `SUBSYSTEM` key.
pub fn is_backlight_change(bytes: &[u8]) -> bool {
    parse(bytes).is_some_and(|event| event.action == "change" && event.subsystem == Some(SUBSYSTEM))
}

/// Open the socket: `AF_NETLINK`, `NETLINK_KOBJECT_UEVENT`, bound to the
/// kernel's group, non-blocking and close-on-exec.
pub fn open() -> io::Result<OwnedFd> {
    // SAFETY: a plain socket(2) call; the result is checked before use and
    // owned from here on.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            libc::NETLINK_KOBJECT_UEVENT,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor this function owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: `sockaddr_nl` is plain old data; all-zero is a valid value.
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // Port 0: the kernel assigns one.
    addr.nl_pid = 0;
    addr.nl_groups = GROUP_KERNEL;
    // SAFETY: `addr` is a valid `sockaddr_nl` and the length is its size.
    let bound = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const addr).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if bound < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// One datagram, and whether the kernel sent it (`nl_pid` 0): only the
/// kernel's messages are uevents.
fn receive(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<(usize, bool)> {
    // SAFETY: as in `open`.
    let mut from: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
    // SAFETY: `buf` is valid for `buf.len()` bytes and `from`/`len` describe a
    // writable `sockaddr_nl`.
    let n = unsafe {
        libc::recvfrom(
            fd.as_raw_fd(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            (&raw mut from).cast::<libc::sockaddr>(),
            &mut len,
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((n as usize, from.nl_pid == 0))
}

/// Listen on `fd` for the rest of the process's life and send `()` for every
/// backlight change. The loop coalesces a burst; this only filters.
///
/// `ENOBUFS` is the kernel saying messages were dropped because the socket
/// fell behind: one of them may have been the backlight's, so it is a wake as
/// well. Any other error ends the watch with one line, and the slider keeps
/// what the menu's own reads give it.
pub async fn watch(fd: OwnedFd, wake: UnboundedSender<()>) {
    let fd = match AsyncFd::new(fd) {
        Ok(fd) => fd,
        Err(e) => {
            witness(&format!("uevent: {e}; no brightness on-screen display"));
            return;
        }
    };
    let mut buf = vec![0u8; BUFFER];
    loop {
        let mut ready = match fd.readable().await {
            Ok(ready) => ready,
            Err(e) => {
                witness(&format!("uevent: {e}; no brightness on-screen display"));
                return;
            }
        };
        match ready.try_io(|fd| receive(fd.get_ref(), &mut buf)) {
            Err(_would_block) => continue,
            Ok(Ok((n, true))) if is_backlight_change(&buf[..n]) => {
                if wake.send(()).is_err() {
                    return;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) if e.raw_os_error() == Some(libc::ENOBUFS) => {
                if wake.send(()).is_err() {
                    return;
                }
            }
            Ok(Err(e)) => {
                witness(&format!("uevent: {e}; no brightness on-screen display"));
                return;
            }
        }
    }
}
