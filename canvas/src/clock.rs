//! The time a fade is measured in: `CLOCK_BOOTTIME`, in milliseconds.
//!
//! The boot clock and not the monotonic one, because the monotonic clock
//! stops during a suspend: a fade measured on it would resume where it slept,
//! hours late. The sidecar stamps a command's start on this clock, the host
//! writes this clock's now into every frame, and the caustic subtracts
//! (docs/canvas.md, *Time*).

/// `CLOCK_BOOTTIME` now, in ms. Zero if the clock cannot be read, which on
/// Linux it always can.
pub fn boottime_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the call's duration.
    let ok = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } == 0;
    if !ok {
        return 0;
    }
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

/// A time in ms as the two words the caustic reads, low first.
pub fn words(ms: u64) -> (u32, u32) {
    (ms as u32, (ms >> 32) as u32)
}
