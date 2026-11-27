//! Canonical UTC timestamps (`YYYY-MM-DDTHH:MM:SSZ`) from unix seconds, so SQL
//! compares them as text exactly like the registry migration's `*_at` columns.

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn utc_text(secs: u64) -> String {
    let secs = secs as i64;
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        t / 3600,
        (t % 3600) / 60,
        t % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(utc_text(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_text(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_text(1_795_737_600), "2026-11-27T00:00:00Z");
        assert_eq!(utc_text(1_795_737_600 + 3661), "2026-11-27T01:01:01Z");
    }

    #[test]
    fn text_order_is_time_order() {
        assert!(utc_text(1_000_000_000) < utc_text(1_000_000_001));
        assert!(utc_text(999_999_999) < utc_text(1_000_000_000));
    }
}
