//! Host-testable request bounds for the Studio Worker.
//!
//! Every request is checked here before routing. Header values are never
//! trusted on their own: [`BodyLimiter`] re-enforces the cap on the bytes
//! actually read from the stream.

/// Maximum request URL length (path + query) in bytes.
pub const MAX_URL_BYTES: usize = 2048;
/// Maximum request body in bytes (64 KiB).
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Methods the Worker will route at all; anything else is a 405.
pub const ALLOWED_METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"];
/// `Allow` header value for methods outside [`ALLOWED_METHODS`].
pub const ALLOW_HEADER: &str = "GET, HEAD, POST, PUT, PATCH, DELETE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    BadRequest,
    UriTooLong,
    MethodNotAllowed,
    LengthRequired,
    PayloadTooLarge,
    UnsupportedMediaType,
}

impl Reject {
    pub fn status(self) -> u16 {
        match self {
            Reject::BadRequest => 400,
            Reject::MethodNotAllowed => 405,
            Reject::LengthRequired => 411,
            Reject::PayloadTooLarge => 413,
            Reject::UriTooLong => 414,
            Reject::UnsupportedMediaType => 415,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Reject::BadRequest => "bad_request",
            Reject::MethodNotAllowed => "method_not_allowed",
            Reject::LengthRequired => "length_required",
            Reject::PayloadTooLarge => "payload_too_large",
            Reject::UriTooLong => "uri_too_long",
            Reject::UnsupportedMediaType => "unsupported_media_type",
        }
    }
}

fn is_body_method(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH")
}

/// `application/json` with an optional UTF-8 charset; nothing else is accepted.
pub fn is_supported_content_type(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    let mut parts = value.split(';');
    if !parts
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
    {
        return false;
    }
    parts.all(|p| match p.split_once('=') {
        Some((k, v)) if k.trim().eq_ignore_ascii_case("charset") => {
            let cs = v.trim().trim_matches('"');
            cs.eq_ignore_ascii_case("utf-8") || cs.eq_ignore_ascii_case("utf8")
        }
        Some(_) => true,
        None => p.trim().is_empty(),
    })
}

fn parse_content_length(raw: &str) -> Result<usize, Reject> {
    let raw = raw.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Reject::BadRequest);
    }
    // Digits that overflow usize are certainly over the cap.
    raw.parse::<usize>().map_err(|_| Reject::PayloadTooLarge)
}

/// Pre-routing checks on the request line and headers only.
///
/// Order: URL length (414), method allowlist (405), then body framing.
/// Body methods need a `Content-Length` (411), no `Transfer-Encoding`, a
/// length within [`MAX_BODY_BYTES`] (413) and, for non-empty bodies, a JSON
/// content type (415). Bodyless methods must not carry a body (400).
pub fn check_request(
    method: &str,
    url_len: usize,
    content_length: Option<&str>,
    transfer_encoding: Option<&str>,
    content_type: Option<&str>,
) -> Result<(), Reject> {
    if url_len > MAX_URL_BYTES {
        return Err(Reject::UriTooLong);
    }
    if !ALLOWED_METHODS.contains(&method) {
        return Err(Reject::MethodNotAllowed);
    }
    if !is_body_method(method) {
        let has_body = transfer_encoding.is_some()
            || match content_length {
                Some(v) => parse_content_length(v)? != 0,
                None => false,
            };
        return if has_body {
            Err(Reject::BadRequest)
        } else {
            Ok(())
        };
    }
    if transfer_encoding.is_some() {
        return Err(Reject::LengthRequired);
    }
    let len = parse_content_length(content_length.ok_or(Reject::LengthRequired)?)?;
    if len > MAX_BODY_BYTES {
        return Err(Reject::PayloadTooLarge);
    }
    if len > 0 && !is_supported_content_type(content_type) {
        return Err(Reject::UnsupportedMediaType);
    }
    Ok(())
}

/// Byte cap for draining the request body stream. Body methods drain up to
/// their declared (already validated) length; bodyless methods (GET, HEAD,
/// DELETE) drain up to [`MAX_BODY_BYTES`] and any byte at all is then a 400.
pub fn drain_limit(method: &str, content_length: Option<&str>) -> usize {
    if is_body_method(method) {
        content_length
            .and_then(|v| v.trim().parse::<usize>().ok())
            .map_or(MAX_BODY_BYTES, |n| n.min(MAX_BODY_BYTES))
    } else {
        MAX_BODY_BYTES
    }
}

/// Counts bytes as they are streamed so a lying `Content-Length` cannot
/// smuggle more than the declared (and capped) amount.
#[derive(Debug)]
pub struct BodyLimiter {
    limit: usize,
    seen: usize,
}

impl BodyLimiter {
    /// `declared` is the already-validated `Content-Length`.
    pub fn new(declared: usize) -> Self {
        Self {
            limit: declared.min(MAX_BODY_BYTES),
            seen: 0,
        }
    }

    pub fn push(&mut self, chunk_len: usize) -> Result<(), Reject> {
        self.seen = self.seen.saturating_add(chunk_len);
        if self.seen > self.limit {
            Err(Reject::PayloadTooLarge)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: Option<&str> = Some("application/json");

    #[test]
    fn get_without_body_is_ok() {
        assert_eq!(check_request("GET", 20, None, None, None), Ok(()));
        assert_eq!(check_request("HEAD", 20, Some("0"), None, None), Ok(()));
    }

    #[test]
    fn url_length_cap() {
        assert_eq!(
            check_request("GET", MAX_URL_BYTES, None, None, None),
            Ok(())
        );
        assert_eq!(
            check_request("GET", MAX_URL_BYTES + 1, None, None, None),
            Err(Reject::UriTooLong)
        );
    }

    #[test]
    fn method_allowlist() {
        for m in ["TRACE", "CONNECT", "OPTIONS", "get", "PROPFIND"] {
            assert_eq!(
                check_request(m, 1, None, None, None),
                Err(Reject::MethodNotAllowed),
                "{m}"
            );
        }
    }

    #[test]
    fn body_methods_require_content_length() {
        assert_eq!(
            check_request("POST", 1, None, None, JSON),
            Err(Reject::LengthRequired)
        );
        assert_eq!(
            check_request("PUT", 1, None, Some("chunked"), JSON),
            Err(Reject::LengthRequired)
        );
        assert_eq!(
            check_request("POST", 1, Some("abc"), None, JSON),
            Err(Reject::BadRequest)
        );
        assert_eq!(
            check_request("POST", 1, Some("-1"), None, JSON),
            Err(Reject::BadRequest)
        );
    }

    #[test]
    fn body_cap() {
        let at = MAX_BODY_BYTES.to_string();
        let over = (MAX_BODY_BYTES + 1).to_string();
        assert_eq!(check_request("POST", 1, Some(&at), None, JSON), Ok(()));
        assert_eq!(
            check_request("POST", 1, Some(&over), None, JSON),
            Err(Reject::PayloadTooLarge)
        );
        assert_eq!(
            check_request("POST", 1, Some("99999999999999999999999"), None, JSON),
            Err(Reject::PayloadTooLarge)
        );
        // 413 wins over 415.
        assert_eq!(
            check_request("POST", 1, Some(&over), None, Some("text/plain")),
            Err(Reject::PayloadTooLarge)
        );
    }

    #[test]
    fn content_type_allowlist() {
        assert_eq!(
            check_request("POST", 1, Some("2"), None, None),
            Err(Reject::UnsupportedMediaType)
        );
        for ct in [
            "text/plain",
            "application/x-www-form-urlencoded",
            "application/jsonx",
            "application/json; charset=latin1",
        ] {
            assert_eq!(
                check_request("POST", 1, Some("2"), None, Some(ct)),
                Err(Reject::UnsupportedMediaType),
                "{ct}"
            );
        }
        for ct in [
            "application/json",
            "Application/JSON; charset=UTF-8",
            "application/json;charset=\"utf-8\"",
        ] {
            assert_eq!(
                check_request("POST", 1, Some("2"), None, Some(ct)),
                Ok(()),
                "{ct}"
            );
        }
        // Empty body needs no content type.
        assert_eq!(check_request("POST", 1, Some("0"), None, None), Ok(()));
    }

    #[test]
    fn bodyless_methods_reject_bodies() {
        assert_eq!(
            check_request("GET", 1, Some("5"), None, JSON),
            Err(Reject::BadRequest)
        );
        assert_eq!(
            check_request("DELETE", 1, None, Some("chunked"), None),
            Err(Reject::BadRequest)
        );
    }

    #[test]
    fn limiter_enforces_actual_bytes() {
        let mut l = BodyLimiter::new(10);
        assert_eq!(l.push(6), Ok(()));
        assert_eq!(l.push(4), Ok(()));
        assert_eq!(l.push(1), Err(Reject::PayloadTooLarge));
        let mut big = BodyLimiter::new(usize::MAX);
        assert_eq!(big.push(MAX_BODY_BYTES), Ok(()));
        assert_eq!(big.push(1), Err(Reject::PayloadTooLarge));
        let mut sat = BodyLimiter::new(10);
        assert_eq!(sat.push(usize::MAX), Err(Reject::PayloadTooLarge));
        assert_eq!(sat.push(usize::MAX), Err(Reject::PayloadTooLarge));
    }

    #[test]
    fn drain_limits() {
        assert_eq!(drain_limit("POST", Some("10")), 10);
        assert_eq!(drain_limit("POST", Some("99999999")), MAX_BODY_BYTES);
        assert_eq!(drain_limit("POST", None), MAX_BODY_BYTES);
        assert_eq!(drain_limit("GET", None), MAX_BODY_BYTES);
        assert_eq!(drain_limit("DELETE", Some("5")), MAX_BODY_BYTES);
    }

    #[test]
    fn reject_status_codes() {
        assert_eq!(Reject::UriTooLong.status(), 414);
        assert_eq!(Reject::UnsupportedMediaType.status(), 415);
        assert_eq!(Reject::PayloadTooLarge.code(), "payload_too_large");
    }
}
