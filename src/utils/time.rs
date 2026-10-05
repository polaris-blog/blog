//! Zero-dependency date/time helpers. Timestamps are Unix epoch seconds (UTC).
//! This avoids pulling in chrono/time while covering everything Polaris needs.

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Howard Hinnant's `days_from_civil` algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: u32, // 0 = Sunday
}

pub fn breakdown(ts: i64) -> DateTime {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 was a Thursday (weekday 4).
    let weekday = (days + 4).rem_euclid(7) as u32;
    DateTime {
        year,
        month,
        day,
        hour: (secs / 3600) as u32,
        minute: ((secs % 3600) / 60) as u32,
        second: (secs % 60) as u32,
        weekday,
    }
}

/// Format an epoch timestamp with a named preset.
pub fn format(ts: i64, fmt: &str) -> String {
    let d = breakdown(ts);
    match fmt {
        "date" => format!(
            "{} {:02}, {}",
            MONTHS[(d.month - 1) as usize],
            d.day,
            d.year
        ),
        "datetime" => format!(
            "{} {:02}, {} {:02}:{:02}",
            MONTHS[(d.month - 1) as usize],
            d.day,
            d.year,
            d.hour,
            d.minute
        ),
        "year" => format!("{}", d.year),
        "rfc3339" => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            d.year, d.month, d.day, d.hour, d.minute, d.second
        ),
        "rfc822" => format!(
            "{}, {:02} {} {} {:02}:{:02}:{:02} +0000",
            DAYS[d.weekday as usize],
            d.day,
            MONTHS[(d.month - 1) as usize],
            d.year,
            d.hour,
            d.minute,
            d.second
        ),
        "http" => format!(
            "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
            DAYS[d.weekday as usize],
            d.day,
            MONTHS[(d.month - 1) as usize],
            d.year,
            d.hour,
            d.minute,
            d.second
        ),
        // Value for <input type="datetime-local"> (seconds omitted).
        "local" => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            d.year, d.month, d.day, d.hour, d.minute
        ),
        _ => format(ts, "date"),
    }
}

/// Widest year the calendar maths can round-trip without overflowing `i64`
/// days (`era * 146_097` must stay in range). Roughly ±5.8 million years;
/// anything outside is rejected rather than silently wrapped.
const MAX_YEAR: i64 = 5_800_000;

/// Parse `YYYY-MM-DDTHH:MM[:SS]` (a `datetime-local` form value) as UTC.
///
/// Rejects out-of-range years instead of overflowing: a hostile
/// `99999999999999-01-01` would otherwise panic in debug builds (a trivial
/// request-level DoS) or wrap to a bogus timestamp in release builds.
pub fn parse_datetime_local(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    if !(-MAX_YEAR..=MAX_YEAR).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return None;
    }
    let mut secs = 0i64;
    if !time.is_empty() {
        let mut parts = time.split(':');
        let h: i64 = parts.next().unwrap_or("0").parse().ok()?;
        let m: i64 = parts.next().unwrap_or("0").parse().ok()?;
        let sec: i64 = parts.next().unwrap_or("0").parse().ok()?;
        if h > 23 || m > 59 || sec > 59 {
            return None;
        }
        secs = h * 3600 + m * 60 + sec;
    }
    Some(days_from_civil(year, month, day) * 86_400 + secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero() {
        assert_eq!(breakdown(0).year, 1970);
        assert_eq!(breakdown(0).month, 1);
        assert_eq!(breakdown(0).day, 1);
        assert_eq!(breakdown(0).weekday, 4); // Thursday
        assert_eq!(format(0, "rfc3339"), "1970-01-01T00:00:00Z");
        assert_eq!(format(0, "rfc822"), "Thu, 01 Jan 1970 00:00:00 +0000");
    }

    #[test]
    fn known_dates() {
        // 2026-08-23 12:34:56 UTC
        let ts = days_from_civil(2026, 8, 23) * 86_400 + 12 * 3600 + 34 * 60 + 56;
        assert_eq!(format(ts, "rfc3339"), "2026-08-23T12:34:56Z");
        assert_eq!(format(ts, "date"), "Aug 23, 2026");
        assert_eq!(format(ts, "datetime"), "Aug 23, 2026 12:34");
        let d = breakdown(ts);
        assert_eq!(d.weekday, 0); // Sunday
    }

    #[test]
    fn roundtrip() {
        for &(y, m, d) in &[(1970, 1, 1), (2000, 2, 29), (2026, 8, 23), (2100, 3, 1)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
    }

    #[test]
    fn parse_datetime() {
        assert_eq!(parse_datetime_local("1970-01-01T00:00"), Some(0));
        assert_eq!(parse_datetime_local("1970-01-02T00:00"), Some(86_400));
        assert_eq!(
            parse_datetime_local("2026-08-23T12:34:56"),
            Some(days_from_civil(2026, 8, 23) * 86_400 + 45_296)
        );
        assert_eq!(parse_datetime_local("nonsense"), None);
        assert_eq!(parse_datetime_local("2026-13-01T00:00"), None);
    }
}
