//! UTC RFC 3339 timestamps without a date-time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn now_rfc3339() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format_millis(i128::try_from(millis).unwrap_or(0))
}

/// UTC calendar day (`YYYY-MM-DD`) of a Unix millisecond instant.
pub(crate) fn utc_day(millis: i64) -> String {
    format_millis(i128::from(millis))[..10].to_string()
}

/// Days-from-civil inverse (Howard Hinnant), proleptic Gregorian, UTC.
fn format_millis(millis: i128) -> String {
    let secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i128::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::format_millis;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_millis(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(format_millis(1_795_305_600_000), "2026-11-22T00:00:00.000Z");
    }
}
