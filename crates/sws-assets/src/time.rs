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

/// (year, month, day) to days since 1970-01-01 (inverse of `civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse a canonical `YYYY-MM-DDTHH:MM:SSZ` timestamp back to unix seconds. Anything
/// that [`utc_text`] would not produce (another layout, an impossible date such as
/// `2026-02-30`, a pre-1970 instant) is `None`.
pub fn parse_utc_text(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = &s[r];
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let days = days_from_civil(i64::from(y), mo, d);
    let secs = days * 86_400 + i64::from(h * 3600 + mi * 60 + se);
    let secs = u64::try_from(secs).ok()?;
    // Rejects impossible days (Feb 30 normalises to another date).
    (utc_text(secs) == s).then_some(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_the_exact_inverse() {
        for secs in [
            0,
            951_782_400,
            1_795_737_600,
            1_795_737_600 + 3661,
            4_102_444_799,
        ] {
            assert_eq!(parse_utc_text(&utc_text(secs)), Some(secs), "{secs}");
        }
        for bad in [
            "",
            "2026-02-30T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-11-27 00:00:00Z",
            "2026-11-27T00:00:00",
            "2026-11-27T00:00:00+00:00",
            "2026-11-27T24:00:00Z",
            "2026-11-27T00:60:00Z",
            "1969-12-31T23:59:59Z",
            "+026-11-27T00:00:00Z",
            "2026-1a-27T00:00:00Z",
            "0000-00-00T00:00:00Z",
        ] {
            assert_eq!(parse_utc_text(bad), None, "{bad:?}");
        }
    }

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
