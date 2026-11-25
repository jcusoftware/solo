//! Google Calendar timestamp normalization (issue #43 port; pure logic kept
//! host-testable per AGENTS.md §3).

use sws_security::validate::{is_valid_date, iso8601_utc};

/// Google Calendar timed events use RFC 3339. Accept explicit offsets and
/// fractional seconds, then truncate to seconds for the D1 UTC representation.
/// This upcoming-events importer rejects pre-2000 dates and leap seconds.
pub fn google_datetime_utc(value: &str) -> Option<String> {
    let utc = google_datetime_seconds(value)?;
    let formatted = iso8601_utc(utc as u64 * 1_000);
    (formatted.len() == 24).then(|| format!("{}Z", &formatted[..19]))
}

pub fn google_datetime_seconds(value: &str) -> Option<i64> {
    let b = value.as_bytes();
    let date = value.get(..10)?;
    if !is_valid_date(date) || !matches!(b.get(10), Some(b'T' | b't')) {
        return None;
    }
    let number = |digits: &[u8]| {
        digits.iter().try_fold(0_i64, |n, &d| {
            d.is_ascii_digit().then(|| n * 10 + i64::from(d - b'0'))
        })
    };
    let clock = |bytes: &[u8]| match bytes {
        [h1, h2, b':', m1, m2] => {
            let h = number(&[*h1, *h2])?;
            let m = number(&[*m1, *m2])?;
            (h < 24 && m < 60).then_some(h * 3600 + m * 60)
        }
        _ => None,
    };
    let daytime = clock(b.get(11..16)?)?;
    if b.get(16) != Some(&b':') {
        return None;
    }
    let seconds = number(b.get(17..19)?)?;
    if seconds >= 60 {
        return None;
    }
    let mut zone = b.get(19..)?;
    if let Some(fraction) = zone.strip_prefix(b".") {
        let digits = fraction.iter().take_while(|d| d.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        zone = &fraction[digits..];
    }
    let offset = match zone {
        [b'Z' | b'z'] => 0,
        [b'+', rest @ ..] => clock(rest)?,
        [b'-', rest @ ..] => -clock(rest)?,
        _ => return None,
    };

    // Inverse of civil-from-days in iso8601_utc (proleptic Gregorian calendar).
    let month = number(&b[5..7])?;
    let year = number(&b[..4])? - i64::from(month <= 2);
    let day = number(&b[8..10])?;
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let march_month = if month <= 2 { month + 9 } else { month - 3 };
    let doy = (153 * march_month + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + daytime + seconds - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_offsets_across_dates() {
        for (input, output) in [
            ("2028-03-01T00:15:25.1234+05:30", "2028-02-29T18:45:25Z"),
            ("2026-12-31T23:30:00-05:00", "2027-01-01T04:30:00Z"),
            ("2026-01-01T12:34:56Z", "2026-01-01T12:34:56Z"),
        ] {
            assert_eq!(google_datetime_utc(input).as_deref(), Some(output));
        }
    }

    #[test]
    fn rejects_invalid_dates_clocks_and_offsets() {
        for invalid in [
            "2027-02-29T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:60Z",
            "2026-01-01T00:00:00",
            "2026-01-01T00:00:00.Z",
            "2026-01-01T00:00:00+05:60",
            "2026-01-01T00:00:00+24:00",
            "2026-01-01T00:00:00Zextra",
        ] {
            assert_eq!(google_datetime_utc(invalid), None, "{invalid}");
        }
    }
}
