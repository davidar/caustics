//! What time it is, as two numbers.
//!
//! The bar does not shape, measure, rasterise or even *spell* the clock: the
//! caustic does all four. What stays here is the one thing that is genuinely
//! I/O: asking the C library, which is the only thing
//! on this host that already knows about `/etc/localtime`, `TZ` and DST, what
//! the local broken-down time is.
//!
//! So there is no `strftime` and no format string here. The digits are
//! `bar_compose`'s, and the weekday and month *words* are
//! `caustics/bar_tunables.slang`'s `WEEKDAYS` and `MONTHS` — so an edit of
//! the tunables can reword the date, or reorder it, without a line of Rust
//! knowing what a weekday is.

use std::time::{SystemTime, UNIX_EPOCH};

/// The clock as the buffer carries it: `(clock_time, clock_date,
/// clock_month)` for [`crate::abi::FrameInputs`], in **local** time.
///
/// - `clock_time` is `hour * 60 + minute`, 0..1439.
/// - `clock_date` is `weekday << 16 | month << 8 | day` — weekday 0 = Sunday
///   (which is `struct tm`'s own `tm_wday`), month 1..12, day 1..31.
/// - `clock_month` is `year << 16 | first << 8 | days`: the year, the weekday
///   of the month's 1st (0 = Sunday) and how many days the month has — what
///   the calendar lays its grid out from. See
///   [`month_word`].
///
/// Numbers and not a string: the caustic writes `HH:MM` from the
/// first and the date from the second, in the tunables' own words and order.
/// `(0, 0, 0)` if the C library will not give local time at all — a day 0
/// composes no day, a weekday 0 composes "Sun" and a month of 0 days lays out
/// no grid, which is the honest shape of "the clock is broken" and still
/// leaves the bar drawing.
pub fn clock_numbers(now: SystemTime) -> (u32, u32, u32) {
    let unix = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match localtime(unix) {
        Some(tm) => pack(&tm),
        None => (0, 0, 0),
    }
}

/// Broken-down local time as the buffer's three words.
///
/// Split out from [`clock_numbers`] because it is the whole of the arithmetic
/// and none of the I/O: `localtime_r` reads the host's zone once per process
/// and there is no putting it back, so a test that pinned a known instant
/// through it would depend on which test ran first. This one takes the `tm`.
fn pack(tm: &libc::tm) -> (u32, u32, u32) {
    let time = (tm.tm_hour.clamp(0, 23) as u32) * 60 + tm.tm_min.clamp(0, 59) as u32;
    let wday = tm.tm_wday.clamp(0, 6) as u32;
    let mon = tm.tm_mon.clamp(0, 11) as u32;
    let mday = tm.tm_mday.clamp(0, 31) as u32;
    let date = wday << 16 | (mon + 1) << 8 | mday;
    let year = (i64::from(tm.tm_year) + 1900).clamp(0, 0xffff) as u32;
    (time, date, month_word(year, mon, wday, mday))
}

/// `year << 16 | first << 8 | days` for the month `mon` (0..11) of `year`, of
/// which day `mday` is a weekday `wday` (0 = Sunday).
///
/// **The calendar's facts**: the weekday the 1st falls on
/// and the month's length are what the caustic lays the grid out from, and the
/// year is what its title says. The year is the fact the caustic has no other
/// way to know; the other two are arithmetic on it — the leap rule and a day of
/// the week — and that arithmetic is here, beside the one call that knows the
/// date, because a calendar algorithm is not a tunable.
fn month_word(year: u32, mon: u32, wday: u32, mday: u32) -> u32 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let feb = if leap { 29 } else { 28 };
    let days = [31, feb, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][mon.min(11) as usize];
    let first = (wday + 35 - mday.clamp(1, 31) + 1) % 7;
    year << 16 | first << 8 | days
}

/// `unix` as local broken-down time, or `None` if the C library refused.
///
/// The timezone lookup is the reason `libc` is the dependency rather than
/// `time` or `chrono`: it is already in `Cargo.lock` transitively.
///
/// `tzset` first, every call: `localtime_r` initialises the zone on its first
/// call and never looks again, so without `tzset` a bar keeps the zone it
/// started in for life — and when the host's zone changes to one that differs
/// only in DST, the bar is right until the next DST change and an hour wrong
/// after it. With `TZ` unset glibc's `tzset` stats `/etc/localtime` and re-reads
/// it only if the file changed, which once a minute is nothing.
fn localtime(unix: i64) -> Option<libc::tm> {
    // SAFETY: `tm` is a plain-old-data struct for which all-zero is a valid
    // value (its only pointer field, glibc's `tm_zone`, is then null, which
    // `localtime_r` overwrites). `localtime_r` writes through the `&mut` we
    // hand it and returns either that same pointer or null; we read the struct
    // only after checking for null. It is the thread-safe, reentrant form.
    // `tzset` takes glibc's own zone lock, as `localtime_r` does.
    unsafe {
        tzset();
        let t = unix as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        Some(tm)
    }
}

unsafe extern "C" {
    /// POSIX `tzset`, which the `libc` crate does not declare on Linux.
    fn tzset();
}

#[cfg(test)]
mod tests;
