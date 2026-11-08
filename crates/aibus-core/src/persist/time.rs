//! Timestamps as text. UTC only, no calendar crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYY-MM-DD`, UTC (days since 1970-01-01 on the proleptic Gregorian calendar).
pub fn date(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// RFC 3339 UTC with milliseconds, as 06 section 4 wants timestamps.
pub fn rfc3339(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let rem = secs % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}.{:03}Z",
        date(at),
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60,
        since.subsec_millis()
    )
}
