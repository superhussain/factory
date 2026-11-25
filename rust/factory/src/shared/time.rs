//! UTC stamps in `%Y-%m-%dT%H:%M:%SZ`, with no date crate.

use std::time::{SystemTime, UNIX_EPOCH};

use super::pyre::{Flags, Pattern};

#[cfg(test)]
thread_local! {
    /// A test's pinned clock: while set, `unix_now` on that test's thread returns it.
    pub static PINNED_NOW: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

pub fn unix_now() -> i64 {
    #[cfg(test)]
    if let Some(secs) = PINNED_NOW.with(|p| p.get()) {
        return secs;
    }
    // The parity harness pins the clock, in seconds since the epoch, so a golden never depends on the date.
    if let Some(secs) = std::env::var("FACTORY_NOW")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return secs;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn format_stamp(secs: i64) -> String {
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

/// Python's `now()`.
pub fn now() -> String {
    format_stamp(unix_now())
}

/// The regex `strptime(s, "%Y-%m-%dT%H:%M:%SZ")` builds, which ignores case.
static STAMP: Pattern = Pattern::new(
    r"(\d\d\d\d)-(1[0-2]|0[1-9]|[1-9])-(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])T(2[0-3]|[0-1]\d|\d):([0-5]\d|\d):(6[0-1]|[0-5]\d|\d)Z",
    Flags::I,
);

/// Seconds since the epoch for what `strptime` accepts, else None.
pub fn parse_stamp(s: &str) -> Option<i64> {
    let caps = STAMP.fullmatch(s)?;
    let mut f = [0i64; 6];
    for (i, v) in f.iter_mut().enumerate() {
        let text = caps.get(i + 1)?.as_str().trim_start();
        *v = text
            .chars()
            .try_fold(0i64, |n, c| Some(n * 10 + c.to_digit(10)? as i64))?;
    }
    let [y, m, d, hh, mm, ss] = f;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if y < 1 || d > month_days[(m - 1) as usize] || ss > 59 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Seconds since stamp, never negative.
pub fn elapsed_seconds(stamp: &str) -> Option<i64> {
    parse_stamp(stamp).map(|start| (unix_now() - start).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_round_trip() {
        assert_eq!(format_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_stamp(1_793_404_800), "2026-10-31T00:00:00Z");
        assert_eq!(parse_stamp("2026-10-31T00:00:00Z"), Some(1_793_404_800));
        assert_eq!(
            parse_stamp("2026-11-01T00:00:00Z")
                .map(format_stamp)
                .as_deref(),
            Some("2026-11-01T00:00:00Z")
        );
    }

    #[test]
    fn hold_prints_elapsed_since_started() {
        // A start 340 seconds ago reads as 340 to 400 elapsed seconds, as hold prints it.
        let started = format_stamp(unix_now() - 340);
        let elapsed = elapsed_seconds(&started).unwrap();
        assert!((340..=400).contains(&elapsed), "{elapsed}");
    }

    #[test]
    fn parse_accepts_what_strptime_accepts() {
        assert_eq!(
            parse_stamp("2026-1-5T1:2:3Z"),
            parse_stamp("2026-01-05T01:02:03Z")
        );
        assert!(parse_stamp("2026-01-05T01:02:03Z").is_some());
        assert_eq!(
            parse_stamp("2026-01-05t01:02:03z"),
            parse_stamp("2026-01-05T01:02:03Z")
        );
        assert_eq!(
            parse_stamp("2026-01- 5T00:00:00Z"),
            parse_stamp("2026-01-05T00:00:00Z")
        );
        assert!(parse_stamp("2024-02-29T00:00:00Z").is_some());
        for bad in [
            "2023-02-29T00:00:00Z",
            "2026-01-05T01:02:60Z",
            "0000-01-01T00:00:00Z",
            "2026-01-05T01:02:03Z ",
            "2026-13-01T00:00:00Z",
            "soon",
            "",
        ] {
            assert_eq!(parse_stamp(bad), None, "{bad}");
        }
        assert_eq!(elapsed_seconds("2999-01-01T00:00:00Z"), Some(0));
    }
}
