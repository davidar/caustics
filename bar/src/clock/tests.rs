use super::*;

/// The three numbers for *now*, against what the epoch arithmetic says they have
/// to be whatever zone the host is in.
#[test]
fn the_clock_numbers_are_a_local_time_and_a_local_date() {
    let now = SystemTime::now();
    let (time, date, calendar) = clock_numbers(now);
    let (hour, minute) = (time / 60, time % 60);
    assert!(hour < 24 && minute < 60, "{time}");

    // Minute-of-hour is timezone-independent (every civil offset in use is a
    // whole number of minutes), so this pins the arithmetic without depending
    // on the host's zone.
    let unix = now.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    assert_eq!(minute, ((unix / 60) % 60) as u32, "{time}");

    let (weekday, month, day) = unpack(date);
    assert!(weekday < 7, "{weekday}");
    assert!((1..=12).contains(&month), "{month}");
    assert!((1..=31).contains(&day), "{day}");
    let (year, first, days) = unpack_month(calendar);
    assert!(year >= 2026, "{year}");
    assert!(first < 7 && (28..=31).contains(&days), "{first} {days}");
    assert!(day <= days, "day {day} of a {days}-day month");
    // The 1st and today are `day - 1` days apart, whatever the zone.
    assert_eq!(
        (first + day - 1) % 7,
        weekday,
        "the 1st's weekday and today's agree"
    );
    println!(
        "clock_numbers(now) = {hour:02}:{minute:02} / w{weekday} m{month} d{day} / \
         {year}, the 1st a w{first}, {days} days"
    );
}

/// The calendar's word for dates whose answers are known
/// from outside this file: the 1st's weekday from a calendar, the month's
/// length from the leap rule — the century years that are and are not leap
/// among them.
#[test]
fn the_month_word_is_the_year_the_first_and_the_length() {
    // Saturday 10 October 2026: the 1st was a Thursday, and October has 31.
    assert_eq!(unpack_month(month_word(2026, 9, 6, 10)), (2026, 4, 31));
    // Through `pack`, the same date: `tm_year` counts from 1900.
    let mut today = tm(9, 30, 6, 9, 10);
    today.tm_year = 126;
    assert_eq!(unpack_month(pack(&today).2), (2026, 4, 31));
    // Wednesday 1 February 2023: the 1st is the day itself.
    assert_eq!(unpack_month(month_word(2023, 1, 3, 1)), (2023, 3, 28));
    // February of a leap year: Thursday 29 February 2024, the 1st a Thursday.
    assert_eq!(unpack_month(month_word(2024, 1, 4, 29)), (2024, 4, 29));
    // 2000 is a leap year (divisible by 400); 1900 and 2100 are not.
    // Tuesday 15 February 2000: the 1st a Tuesday.
    assert_eq!(unpack_month(month_word(2000, 1, 2, 15)), (2000, 2, 29));
    // Thursday 1 February 1900.
    assert_eq!(unpack_month(month_word(1900, 1, 4, 1)), (1900, 4, 28));
    // Monday 1 February 2100.
    assert_eq!(unpack_month(month_word(2100, 1, 1, 1)), (2100, 1, 28));
    // A month whose 1st is a Sunday: 1 March 2026, read on Tuesday the 31st —
    // the last day, the furthest the subtraction goes.
    assert_eq!(unpack_month(month_word(2026, 2, 2, 31)), (2026, 0, 31));
    // The thirty-day months.
    for mon in [3, 5, 8, 10] {
        assert_eq!(month_word(2026, mon, 0, 1) & 0xff, 30, "month {}", mon + 1);
    }
}

/// The 1st's weekday against an independent day-of-the-week rule (Sakamoto's),
/// for every day of every month of 1999–2101: whichever day of the month the
/// bar reads the clock on, it lays out the same grid.
#[test]
fn the_first_agrees_with_an_independent_rule_on_every_day() {
    fn weekday(y: u32, m: u32, d: u32) -> u32 {
        const T: [u32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let y = if m < 3 { y - 1 } else { y };
        (y + y / 4 - y / 100 + y / 400 + T[m as usize - 1] + d) % 7
    }
    for year in 1999..=2101 {
        for mon in 0..12 {
            let first = weekday(year, mon + 1, 1);
            let days = month_word(year, mon, first, 1) & 0xff;
            // The day after the last is the next month's 1st.
            let next = if mon == 11 {
                weekday(year + 1, 1, 1)
            } else {
                weekday(year, mon + 2, 1)
            };
            assert_eq!((first + days) % 7, next, "{year}-{}: {days} days", mon + 1);
            for mday in 1..=days {
                let word = month_word(year, mon, weekday(year, mon + 1, mday), mday);
                assert_eq!(
                    unpack_month(word),
                    (year, first, days),
                    "{year}-{}-{mday}",
                    mon + 1
                );
            }
        }
    }
}

/// The packing, on a `tm` this test builds: the fields the caustic spells, and
/// the two places an off-by-one would hide.
///
/// Not through `localtime_r`: it reads the host's zone once per process and
/// keeps it, so an instant pinned through it would depend on `$TZ` and on which
/// test in this binary happened to ask the time first.
#[test]
fn the_packing_is_the_fields_the_caustic_spells() {
    // Sunday 13 September 2026, 14:03 — Sunday because it is weekday 0, the
    // value a sign or a one-based weekday would hide.
    let (time, date, _) = pack(&tm(14, 3, 0, 8, 13));
    assert_eq!((time / 60, time % 60), (14, 3));
    assert_eq!(unpack(date), (0, 9, 13), "Sunday, September, the 13th");

    // Midnight on the first of January, which is where a zero-based day or a
    // zero-based month would show. Thursday, as 1 January 2026 was.
    let (time, date, _) = pack(&tm(0, 0, 4, 0, 1));
    assert_eq!(time, 0);
    assert_eq!(unpack(date), (4, 1, 1));

    // The last minute of the last day of the year: every field at its top.
    let (time, date, _) = pack(&tm(23, 59, 6, 11, 31));
    assert_eq!(time, 23 * 60 + 59);
    assert_eq!(unpack(date), (6, 12, 31));

    // A `tm` out of range — which the C library does not produce, but the
    // clamps are what keep a garbage one out of the caustic's own tables, where
    // a weekday of 99 would index past `WEEKDAYS` — packs into range or not at
    // all, never into a neighbour's byte.
    let (time, date, _) = pack(&tm(99, 99, 99, 99, 99));
    assert!(time < 24 * 60, "{time}");
    let (weekday, month, day) = unpack(date);
    assert!(weekday < 7 && (1..=12).contains(&month) && day <= 31);
}

/// A broken-down time with the five fields the packing reads.
fn tm(hour: i32, min: i32, wday: i32, mon: i32, mday: i32) -> libc::tm {
    // SAFETY: `libc::tm` is plain old data for which all-zero is a valid value
    // (its one pointer field, glibc's `tm_zone`, is then null, which nothing
    // here reads).
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_hour = hour;
    tm.tm_min = min;
    tm.tm_wday = wday;
    tm.tm_mon = mon;
    tm.tm_mday = mday;
    tm
}

/// `weekday << 16 | month << 8 | day`, the other way round — the same split
/// `compose_clock_date` does in `caustics/bar.slang`.
fn unpack(date: u32) -> (u32, u32, u32) {
    ((date >> 16) & 0xff, (date >> 8) & 0xff, date & 0xff)
}

/// `year << 16 | first << 8 | days`, the other way round — the split the
/// calendar does in `caustics/bar.slang`.
fn unpack_month(month: u32) -> (u32, u32, u32) {
    (month >> 16, (month >> 8) & 0xff, month & 0xff)
}
