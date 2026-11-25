//! Pure, host-testable request parsing and generic validation primitives.
//! No Workers types here so `cargo test` runs on the host target.

use std::collections::BTreeMap;

/// Maximum accepted request body size in bytes.
pub const MAX_BODY_BYTES: usize = 16 * 1024;
/// Honeypot field rendered visually hidden in forms; humans leave it empty.
pub const HONEYPOT_FIELD: &str = "website";
/// Turnstile token field name used by the widget.
pub const TURNSTILE_FIELD: &str = "cf-turnstile-response";
/// Cloudflare documents a 2048-character maximum for Turnstile tokens.
pub const MAX_TURNSTILE_TOKEN: usize = 2048;

/// Returns `true` if `secret` is one of Cloudflare's documented Turnstile dummy test keys.
pub fn is_turnstile_test_secret(secret: &str) -> bool {
    matches!(
        secret.trim(),
        "1x0000000000000000000000000000000AA"
            | "2x0000000000000000000000000000000AA"
            | "3x0000000000000000000000000000000AA"
    )
}

pub type Fields = BTreeMap<String, String>;

/// Field-level validation errors: (field, reason).
pub type FieldErrors = Vec<(String, &'static str)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyError {
    TooLarge,
    UnsupportedMediaType,
    Malformed,
}

impl BodyError {
    pub fn status(&self) -> u16 {
        match self {
            BodyError::TooLarge => 413,
            BodyError::UnsupportedMediaType => 415,
            BodyError::Malformed => 400,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            BodyError::TooLarge => "payload_too_large",
            BodyError::UnsupportedMediaType => "unsupported_media_type",
            BodyError::Malformed => "malformed_body",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Json,
    FormUrlEncoded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Honeypot field was populated; reject generically without naming the field.
    Honeypot,
    /// One or more fields failed validation.
    Fields(FieldErrors),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsletterSignup {
    pub email: String,
    pub source_route: String,
    pub consent_text: Option<String>,
    pub turnstile_token: Option<String>,
}

/// Parse a `Content-Type` header into a supported [`MediaType`], handling
/// parameters such as `; charset=utf-8` and rejecting explicit non-UTF-8 charsets.
pub fn parse_content_type(content_type: Option<&str>) -> Option<MediaType> {
    let mut parts = content_type?.split(';');
    let mime = parts.next()?.trim().to_ascii_lowercase();
    for param in parts {
        if let Some((k, v)) = param.split_once('=')
            && k.trim().eq_ignore_ascii_case("charset")
        {
            let cs = v.trim().trim_matches('"').trim_matches('\'');
            if !cs.eq_ignore_ascii_case("utf-8") && !cs.eq_ignore_ascii_case("utf8") {
                return None;
            }
        }
    }
    match mime.as_str() {
        "application/json" => Some(MediaType::Json),
        "application/x-www-form-urlencoded" => Some(MediaType::FormUrlEncoded),
        _ => None,
    }
}

/// Parse a request body as JSON object or `application/x-www-form-urlencoded`
/// according to its `Content-Type`.
pub fn parse_body(content_type: Option<&str>, body: &[u8]) -> Result<Fields, BodyError> {
    let media_type = parse_content_type(content_type).ok_or(BodyError::UnsupportedMediaType)?;
    if body.len() > MAX_BODY_BYTES {
        return Err(BodyError::TooLarge);
    }
    let text = std::str::from_utf8(body).map_err(|_| BodyError::Malformed)?;
    match media_type {
        MediaType::Json => fields_from_json(text),
        MediaType::FormUrlEncoded => parse_form_urlencoded(text),
    }
}

/// Flatten a JSON object of scalars into string fields. Nested values are rejected.
pub fn fields_from_json(text: &str) -> Result<Fields, BodyError> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| BodyError::Malformed)?;
    let obj = value.as_object().ok_or(BodyError::Malformed)?;
    let mut out = Fields::new();
    for (k, v) in obj {
        let s = match v {
            serde_json::Value::Null => continue,
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => return Err(BodyError::Malformed),
        };
        out.insert(k.clone(), s);
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(s: &str) -> Result<String, BodyError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hi = bytes.get(i + 1).copied().and_then(hex_val);
                let lo = bytes.get(i + 2).copied().and_then(hex_val);
                match (hi, lo) {
                    (Some(h), Some(l)) => out.push(h << 4 | l),
                    _ => return Err(BodyError::Malformed),
                }
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| BodyError::Malformed)
}

/// Minimal `application/x-www-form-urlencoded` parser (last value wins).
pub fn parse_form_urlencoded(text: &str) -> Result<Fields, BodyError> {
    let mut out = Fields::new();
    for pair in text.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k)?, percent_decode(v)?);
    }
    Ok(out)
}

/// Percent-encode for form bodies (used for the Turnstile siteverify request).
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        for (j, part) in [k, v].iter().enumerate() {
            if j == 1 {
                out.push('=');
            }
            for b in part.bytes() {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        out.push(b as char)
                    }
                    _ => out.push_str(&format!("%{b:02X}")),
                }
            }
        }
    }
    out
}

pub fn get_trimmed<'a>(f: &'a Fields, key: &str) -> Option<&'a str> {
    f.get(key).map(|s| s.trim()).filter(|s| !s.is_empty())
}

pub fn has_disallowed_control(s: &str, allow_newlines: bool) -> bool {
    s.chars()
        .any(|c| c.is_control() && !(allow_newlines && matches!(c, '\n' | '\r' | '\t')))
}

pub fn validate_text_field(
    f: &Fields,
    key: &str,
    max_chars: usize,
    required: bool,
    multiline: bool,
    errs: &mut FieldErrors,
) -> Option<String> {
    match get_trimmed(f, key) {
        None => {
            if required {
                errs.push((key.to_string(), "required"));
            }
            None
        }
        Some(v) if v.chars().count() > max_chars => {
            errs.push((key.to_string(), "too_long"));
            None
        }
        Some(v) if has_disallowed_control(v, multiline) => {
            errs.push((key.to_string(), "invalid_characters"));
            None
        }
        Some(v) => Some(v.to_string()),
    }
}

/// Pragmatic email shape check (not full RFC 5322).
pub fn is_valid_email(email: &str) -> bool {
    if email.len() < 3 || email.len() > 254 {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || local.len() > 64 || domain.contains('@') {
        return false;
    }
    let local_ok = local.bytes().all(|b| {
        b.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{|}~.-".contains(&b)
    }) && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..");
    let labels: Vec<&str> = domain.split('.').collect();
    let tld_ok = labels
        .last()
        .is_some_and(|tld| tld.len() >= 2 && tld.bytes().any(|b| b.is_ascii_alphabetic()));
    let domain_ok = labels.len() >= 2
        && tld_ok
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    local_ok && domain_ok
}

pub fn validate_email_field(f: &Fields, errs: &mut FieldErrors) -> Option<String> {
    match get_trimmed(f, "email") {
        None => {
            errs.push(("email".to_string(), "required"));
            None
        }
        Some(e) if !is_valid_email(e) => {
            errs.push(("email".to_string(), "invalid"));
            None
        }
        Some(e) => Some(e.to_ascii_lowercase()),
    }
}

/// Pragmatic phone shape check: allows digits, spaces, `+`, `-`, `(`, `)`, `.`, `x`, `X`
/// and requires at least 7 ASCII digits.
pub fn is_valid_phone(phone: &str) -> bool {
    let chars_ok = phone
        .bytes()
        .all(|b| b.is_ascii_digit() || b" +-().xX".contains(&b));
    let digit_count = phone.bytes().filter(u8::is_ascii_digit).count();
    chars_ok && digit_count >= 7
}

/// A site route path such as `/` or `/about`.
pub fn is_valid_route(route: &str) -> bool {
    route.starts_with('/')
        && route.len() <= 200
        && !route.contains("//")
        && !route.split('/').any(|seg| seg == "..")
        && route
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
}

/// Strict `YYYY-MM-DD` calendar date.
pub fn is_valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<u32>().ok());
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    if !s
        .bytes()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    y >= 2000 && (1..=dim).contains(&d)
}

pub fn validate_token_field(f: &Fields, errs: &mut FieldErrors) -> Option<String> {
    match get_trimmed(f, TURNSTILE_FIELD) {
        Some(t) if t.len() > MAX_TURNSTILE_TOKEN => {
            errs.push((TURNSTILE_FIELD.to_string(), "too_long"));
            None
        }
        t => t.map(str::to_string),
    }
}

/// Consent must be an explicit affirmative value (checkbox `yes`/`on` or JSON `true`).
pub fn is_explicit_consent(v: Option<&str>) -> bool {
    v.map(|s| s.trim().to_ascii_lowercase())
        .is_some_and(|s| matches!(s.as_str(), "true" | "yes" | "on" | "1"))
}

pub fn validate_newsletter(f: &Fields) -> Result<NewsletterSignup, ValidationError> {
    if get_trimmed(f, HONEYPOT_FIELD).is_some() {
        return Err(ValidationError::Honeypot);
    }
    let mut errs = FieldErrors::new();
    let email = validate_email_field(f, &mut errs);
    if !is_explicit_consent(get_trimmed(f, "consent")) {
        errs.push(("consent".to_string(), "required"));
    }
    let source_route = match get_trimmed(f, "source_route") {
        Some(r) if is_valid_route(r) => Some(r.to_string()),
        Some(_) => {
            errs.push(("source_route".to_string(), "invalid"));
            None
        }
        None => {
            errs.push(("source_route".to_string(), "required"));
            None
        }
    };
    let consent_text = validate_text_field(f, "consent_text", 500, false, false, &mut errs);
    let turnstile_token = validate_token_field(f, &mut errs);
    match (email, source_route) {
        (Some(email), Some(source_route)) if errs.is_empty() => Ok(NewsletterSignup {
            email,
            source_route,
            consent_text,
            turnstile_token,
        }),
        _ => Err(ValidationError::Fields(errs)),
    }
}

/// Format Unix epoch milliseconds as ISO-8601 UTC (`YYYY-MM-DDTHH:MM:SS.sssZ`).
pub fn iso8601_utc(millis: u64) -> String {
    let secs = millis / 1000;
    let ms = millis % 1000;
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    // Civil-from-days (H. Hinnant), valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Truncate `s` to at most `max_bytes` along a valid UTF-8 char boundary.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Fields {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn form_urlencoded_decoding() {
        let f = parse_form_urlencoded("a=1+2&b=%C3%A9%26&c&=x").unwrap();
        assert_eq!(f["a"], "1 2");
        assert_eq!(f["b"], "é&");
        assert_eq!(f["c"], "");
        assert!(parse_form_urlencoded("a=%ZZ").is_err());
        assert!(parse_form_urlencoded("a=%C3").is_err());
        assert!(parse_form_urlencoded("a=%").is_err());
    }

    #[test]
    fn form_encode_roundtrip() {
        let s = form_encode(&[("secret", "a b&c"), ("response", "x.y_z~")]);
        assert_eq!(s, "secret=a%20b%26c&response=x.y_z~");
        let f = parse_form_urlencoded(&s).unwrap();
        assert_eq!(f["secret"], "a b&c");
    }

    #[test]
    fn body_parsing_by_content_type() {
        let j = parse_body(
            Some("application/json; charset=utf-8"),
            br#"{"a":"x","n":3,"b":true,"z":null}"#,
        )
        .unwrap();
        assert_eq!(
            (j["a"].as_str(), j["n"].as_str(), j["b"].as_str()),
            ("x", "3", "true")
        );
        assert!(!j.contains_key("z"));
        let f = parse_body(
            Some("application/x-www-form-urlencoded; charset=\"UTF-8\""),
            b"a=1&b=2",
        )
        .unwrap();
        assert_eq!((f["a"].as_str(), f["b"].as_str()), ("1", "2"));
        assert_eq!(
            parse_body(Some("application/json"), br#"{"a":{"b":1}}"#),
            Err(BodyError::Malformed)
        );
        assert_eq!(
            parse_body(Some("application/json"), b"[1]"),
            Err(BodyError::Malformed)
        );
        assert_eq!(
            parse_body(Some("text/plain"), b"a=1"),
            Err(BodyError::UnsupportedMediaType)
        );
        assert_eq!(
            parse_body(None, b"a=1"),
            Err(BodyError::UnsupportedMediaType)
        );
        // Unsupported Content-Type with binary (non-UTF-8) payload must return 415, not 400.
        assert_eq!(
            parse_body(Some("application/octet-stream"), &[0xff, 0xfe]),
            Err(BodyError::UnsupportedMediaType)
        );
        assert_eq!(
            parse_body(None, &[0xff, 0xfe]),
            Err(BodyError::UnsupportedMediaType)
        );
        assert_eq!(
            parse_body(Some("application/json; charset=utf-16"), b"{}"),
            Err(BodyError::UnsupportedMediaType)
        );
        let big = vec![b'a'; MAX_BODY_BYTES + 1];
        assert_eq!(
            parse_body(Some("application/x-www-form-urlencoded"), &big),
            Err(BodyError::TooLarge)
        );
        assert_eq!(
            parse_body(Some("application/json"), &[0xff, 0xfe]),
            Err(BodyError::Malformed)
        );
    }

    #[test]
    fn email_shapes() {
        for ok in [
            "a@b.co",
            "first.last+tag@sub.example.test",
            "a@sub.xn--p1ai",
        ] {
            assert!(is_valid_email(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "a@b",
            "@b.co",
            "a@@b.co",
            "a b@c.co",
            ".a@b.co",
            "a..b@c.co",
            "a@-b.co",
            "a@b..co",
            "a@b.c_o",
            "a@b.c",
            "a@127.0.0.1",
            "a@1.23",
        ] {
            assert!(!is_valid_email(bad), "{bad}");
        }
    }

    #[test]
    fn phone_shapes() {
        assert!(is_valid_phone("+1 (555) 010-0100"));
        assert!(is_valid_phone("+1 (555) 010-0100 X42"));
        assert!(!is_valid_phone("call me"));
        assert!(!is_valid_phone("123-456"));
        assert!(!is_valid_phone("() +-."));
    }

    #[test]
    fn text_and_token_validation() {
        let mut errs = FieldErrors::new();
        let f = fields(&[
            ("name", "  Alex  "),
            ("bad_ctrl", "bad\u{0007}name"),
            ("long", &"x".repeat(5)),
            (TURNSTILE_FIELD, &"t".repeat(MAX_TURNSTILE_TOKEN + 1)),
        ]);
        assert_eq!(
            validate_text_field(&f, "name", 100, true, false, &mut errs),
            Some("Alex".to_string())
        );
        assert_eq!(
            validate_text_field(&f, "missing", 100, true, false, &mut errs),
            None
        );
        assert_eq!(
            validate_text_field(&f, "bad_ctrl", 100, true, false, &mut errs),
            None
        );
        assert_eq!(
            validate_text_field(&f, "long", 4, true, false, &mut errs),
            None
        );
        assert_eq!(validate_token_field(&f, &mut errs), None);
        assert!(errs.contains(&("missing".to_string(), "required")));
        assert!(errs.contains(&("bad_ctrl".to_string(), "invalid_characters")));
        assert!(errs.contains(&("long".to_string(), "too_long")));
        assert!(errs.contains(&(TURNSTILE_FIELD.to_string(), "too_long")));
    }

    #[test]
    fn honeypot_returns_dedicated_variant() {
        let nf = fields(&[
            ("email", "pat@example.test"),
            ("source_route", "/"),
            ("consent", "yes"),
            (HONEYPOT_FIELD, "bot"),
        ]);
        assert_eq!(
            validate_newsletter(&nf).unwrap_err(),
            ValidationError::Honeypot
        );
    }

    #[test]
    fn newsletter_requires_explicit_consent() {
        let base = [("email", "pat@example.test"), ("source_route", "/")];
        let ValidationError::Fields(errs) = validate_newsletter(&fields(&base)).unwrap_err() else {
            panic!("expected FieldErrors for missing consent");
        };
        assert_eq!(errs, vec![("consent".to_string(), "required")]);
        for v in ["false", "no", "0", ""] {
            let mut f = fields(&base);
            f.insert("consent".into(), v.into());
            assert!(validate_newsletter(&f).is_err(), "{v}");
        }
        for v in ["true", "yes", "on", "1"] {
            let mut f = fields(&base);
            f.insert("consent".into(), v.into());
            let n = validate_newsletter(&f).unwrap();
            assert_eq!(n.source_route, "/");
        }
        let f = fields(&[
            ("email", "pat@example.test"),
            ("source_route", "javascript:x"),
            ("consent", "yes"),
        ]);
        assert!(matches!(
            validate_newsletter(&f).unwrap_err(),
            ValidationError::Fields(errs) if errs.contains(&("source_route".to_string(), "invalid"))
        ));
    }

    #[test]
    fn dates_and_routes() {
        assert!(is_valid_date("2028-02-29"));
        assert!(!is_valid_date("2027-02-29"));
        assert!(!is_valid_date("2027-13-01"));
        assert!(!is_valid_date("27-01-01"));
        assert!(!is_valid_date("2027-1-001"));
        assert!(is_valid_route("/about"));
        assert!(!is_valid_route("//evil.test"));
        assert!(!is_valid_route("/a?b"));
        // `..` segments must be rejected for parity with the export template
        // validator (crates/sws-cli/src/templates.rs).
        assert!(!is_valid_route("/about/../x"));
        assert!(!is_valid_route("/about/../"));
        assert!(!is_valid_route("/about/.."));
        assert!(!is_valid_route("/.."));
        assert!(is_valid_route("/a.b"));
        assert!(is_valid_route("/about/.x"));
    }

    #[test]
    fn iso8601() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601_utc(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(iso8601_utc(1_795_478_400_000), "2026-11-24T00:00:00.000Z");
    }

    #[test]
    fn truncate_str_respects_char_boundaries() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("café", 4), "caf");
        assert_eq!(truncate_str("café", 5), "café");
        assert_eq!(truncate_str("é", 1), "");
    }

    #[test]
    fn turnstile_test_secret_recognition() {
        assert!(is_turnstile_test_secret("1x0000000000000000000000000000000AA"));
        assert!(is_turnstile_test_secret("2x0000000000000000000000000000000AA"));
        assert!(is_turnstile_test_secret("3x0000000000000000000000000000000AA"));
        assert!(!is_turnstile_test_secret("0x4AAAAAARealLiveSecretKey"));
        assert!(!is_turnstile_test_secret(""));
    }
}
