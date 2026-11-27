//! Host-testable response construction for `sws-worker-studio`.
//!
//! The wasm glue in `lib.rs` only adapts a `worker::Request` into
//! [`RequestFacts`] plus the number of body bytes actually read, calls
//! [`respond`], and turns the returned [`Resp`] into a `worker::Response`.
//! Status, body and the full header set (the `studio` security-header profile,
//! [`sws_security::headers::STUDIO_HEADERS`], and `Cache-Control: no-store`)
//! are decided here, so host tests cover the real response contract.

use serde_json::Value;
use sws_security::headers::{API_CACHE_CONTROL, HeaderProfile};
use sws_security::response::error_body;

use crate::bounds::{self, MAX_BODY_BYTES, Reject};
use crate::routes::{Route, health_body, route};

/// Request facts the pure layer needs. Header values are passed through raw.
#[derive(Debug, Clone, Copy)]
pub struct RequestFacts<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// Path plus query length in bytes.
    pub url_len: usize,
    pub content_length: Option<&'a str>,
    pub transfer_encoding: Option<&'a str>,
    pub content_type: Option<&'a str>,
}

/// A fully decided response: status, optional JSON body, and every header.
#[derive(Debug, Clone, PartialEq)]
pub struct Resp {
    pub status: u16,
    pub body: Option<Value>,
    pub headers: Vec<(String, String)>,
}

impl Resp {
    pub(crate) fn new(status: u16, body: Option<Value>, extra: &[(&str, &str)]) -> Self {
        let mut headers: Vec<(String, String)> = vec![
            ("Content-Type".into(), "application/json".into()),
            ("Cache-Control".into(), API_CACHE_CONTROL.into()),
        ];
        headers.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        headers.extend(
            HeaderProfile::Studio
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        Self {
            status,
            body,
            headers,
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// JSON error response for a bounds rejection (405 carries `Allow`).
pub fn reject(r: Reject) -> Resp {
    let extra: &[(&str, &str)] = if r == Reject::MethodNotAllowed {
        &[("Allow", bounds::ALLOW_HEADER)]
    } else {
        &[]
    };
    Resp::new(r.status(), Some(error_body(r.code())), extra)
}

/// JSON error with the standard header set, for glue that decides a status
/// itself (the session routes).
pub fn json_error(status: u16, code: &str, extra: &[(&str, &str)]) -> Resp {
    Resp::new(status, Some(error_body(code)), extra)
}

/// JSON 200 body with the standard header set (internal glue and test-only routes).
pub fn json_ok(body: Value) -> Resp {
    Resp::new(200, Some(body), &[])
}

pub fn internal_error() -> Resp {
    Resp::new(500, Some(error_body("internal_error")), &[])
}

/// Header-only checks that run before any body byte is read.
pub fn precheck(f: &RequestFacts) -> Result<(), Resp> {
    bounds::check_request(
        f.method,
        f.url_len,
        f.content_length,
        f.transfer_encoding,
        f.content_type,
    )
    .map_err(reject)
}

/// Decide the response. `body_len` is the number of body bytes actually read
/// from the stream (never trust `Content-Length` alone): any byte on a bodyless
/// method (GET, HEAD, DELETE) is a 400, and a body longer than its declared,
/// capped length is a 413.
pub fn respond(f: &RequestFacts, body_len: usize, version: &str) -> Resp {
    if let Err(r) = precheck(f) {
        return r;
    }
    let limit = bounds::drain_limit(f.method, f.content_length);
    if matches!(f.method, "GET" | "HEAD" | "DELETE") {
        if body_len > 0 {
            return reject(Reject::BadRequest);
        }
    } else if body_len > limit.min(MAX_BODY_BYTES) {
        return reject(Reject::PayloadTooLarge);
    }
    match route(f.method, f.path) {
        Route::Health => Resp::new(200, Some(health_body(version)), &[]),
        Route::HealthHead => Resp::new(200, None, &[]),
        // The WebSocket upgrade is intercepted by the wasm glue before this layer
        // (auth, membership, then the Durable Object); a plain GET lands here.
        Route::Session { .. } => Resp::new(
            426,
            Some(error_body("upgrade_required")),
            &[("Upgrade", "websocket")],
        ),
        Route::MethodNotAllowed(allow) => Resp::new(
            405,
            Some(error_body("method_not_allowed")),
            &[("Allow", allow)],
        ),
        Route::NotFound => Resp::new(404, Some(error_body("not_found")), &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_security::headers::{SECURITY_HEADERS, STUDIO_HEADERS};

    fn facts<'a>(method: &'a str, path: &'a str) -> RequestFacts<'a> {
        RequestFacts {
            method,
            path,
            url_len: path.len(),
            content_length: None,
            transfer_encoding: None,
            content_type: None,
        }
    }

    /// The applied header set is exactly: the `studio` profile + Cache-Control:
    /// no-store + Content-Type (+ optional Allow), with no duplicates.
    fn assert_headers(r: &Resp, allow: Option<&str>) {
        for (k, v) in STUDIO_HEADERS {
            assert_eq!(r.header(k), Some(*v), "{k} on {}", r.status);
        }
        assert_eq!(r.header("Cache-Control"), Some("no-store"));
        assert_eq!(r.header("Content-Type"), Some("application/json"));
        assert_eq!(r.header("Allow"), allow, "Allow on {}", r.status);
        let expected = STUDIO_HEADERS.len() + 2 + usize::from(allow.is_some());
        assert_eq!(
            r.headers.len(),
            expected,
            "unexpected extra headers: {:?}",
            r.headers
        );
        let mut names: Vec<String> = r.headers.iter().map(|(k, _)| k.to_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), r.headers.len(), "duplicate header names");
    }

    fn error_code(r: &Resp) -> &str {
        r.body
            .as_ref()
            .and_then(|b| b["error"].as_str())
            .unwrap_or("")
    }

    #[test]
    fn status_200_health_get_and_head() {
        let r = respond(&facts("GET", "/api/health"), 0, "9.9.9");
        assert_eq!(r.status, 200);
        assert_eq!(
            r.body.as_ref().map(|b| b["version"].clone()),
            Some("9.9.9".into())
        );
        assert_headers(&r, None);
        let h = respond(&facts("HEAD", "/api/health"), 0, "9.9.9");
        assert_eq!(h.status, 200);
        assert_eq!(h.body, None);
        assert_headers(&h, None);
    }

    #[test]
    fn session_route_without_upgrade_is_426_with_the_full_header_set() {
        let r = respond(&facts("GET", "/api/sites/ws-a/site_1/session"), 0, "v");
        assert_eq!((r.status, error_code(&r)), (426, "upgrade_required"));
        assert_eq!(r.header("Upgrade"), Some("websocket"));
        for (k, v) in STUDIO_HEADERS {
            assert_eq!(r.header(k), Some(*v), "{k}");
        }
        assert_eq!(r.header("Cache-Control"), Some("no-store"));
        // Other methods on the session path are 405 with Allow: GET.
        let mut p = facts("POST", "/api/sites/ws-a/site_1/session");
        p.content_length = Some("0");
        let r = respond(&p, 0, "v");
        assert_eq!(r.status, 405);
        assert_eq!(r.header("Allow"), Some("GET"));
        // A body on the upgrade GET is still a 400.
        assert_eq!(
            respond(&facts("GET", "/api/sites/ws-a/site_1/session"), 3, "v").status,
            400
        );
    }

    #[test]
    fn json_error_carries_the_standard_headers() {
        let r = json_error(401, "unauthenticated", &[]);
        assert_eq!((r.status, error_code(&r)), (401, "unauthenticated"));
        assert_headers(&r, None);
    }

    #[test]
    fn status_404() {
        let r = respond(&facts("GET", "/nope"), 0, "v");
        assert_eq!((r.status, error_code(&r)), (404, "not_found"));
        assert_headers(&r, None);
    }

    #[test]
    fn status_405_known_path_and_unknown_method() {
        let mut p = facts("POST", "/api/health");
        p.content_length = Some("0");
        let r = respond(&p, 0, "v");
        assert_eq!((r.status, error_code(&r)), (405, "method_not_allowed"));
        assert_headers(&r, Some("GET, HEAD"));
        let t = respond(&facts("TRACE", "/api/health"), 0, "v");
        assert_eq!(t.status, 405);
        assert_headers(&t, Some(bounds::ALLOW_HEADER));
    }

    #[test]
    fn status_411_413_415_400_414() {
        let mut f = facts("POST", "/api/health");
        let r = respond(&f, 0, "v");
        assert_eq!((r.status, error_code(&r)), (411, "length_required"));
        assert_headers(&r, None);

        let over = (MAX_BODY_BYTES + 1).to_string();
        f.content_length = Some(&over);
        f.content_type = Some("application/json");
        let r = respond(&f, 0, "v");
        assert_eq!((r.status, error_code(&r)), (413, "payload_too_large"));
        assert_headers(&r, None);

        f.content_length = Some("2");
        f.content_type = Some("text/plain");
        let r = respond(&f, 2, "v");
        assert_eq!((r.status, error_code(&r)), (415, "unsupported_media_type"));
        assert_headers(&r, None);

        let mut g = facts("GET", "/api/health");
        g.content_length = Some("abc");
        let r = respond(&g, 0, "v");
        assert_eq!((r.status, error_code(&r)), (400, "bad_request"));
        assert_headers(&r, None);

        let mut u = facts("GET", "/api/health");
        u.url_len = bounds::MAX_URL_BYTES + 1;
        let r = respond(&u, 0, "v");
        assert_eq!((r.status, error_code(&r)), (414, "uri_too_long"));
        assert_headers(&r, None);
    }

    #[test]
    fn streamed_bytes_are_enforced_not_just_headers() {
        // Bodyless methods: any streamed byte is a 400 even with no headers.
        for m in ["GET", "HEAD", "DELETE"] {
            let r = respond(&facts(m, "/api/health"), 1, "v");
            assert_eq!((r.status, error_code(&r)), (400, "bad_request"), "{m}");
            assert_headers(&r, None);
        }
        // Body methods: a body longer than declared is a 413.
        let mut f = facts("POST", "/api/health");
        f.content_length = Some("2");
        f.content_type = Some("application/json");
        assert_eq!(respond(&f, 2, "v").status, 405);
        let r = respond(&f, 3, "v");
        assert_eq!((r.status, error_code(&r)), (413, "payload_too_large"));
        assert_headers(&r, None);
    }

    #[test]
    fn internal_error_has_the_same_header_set() {
        let r = internal_error();
        assert_eq!((r.status, error_code(&r)), (500, "internal_error"));
        assert_headers(&r, None);
    }

    #[test]
    fn responses_carry_exactly_the_studio_profile_not_the_site_set() {
        let mut want: Vec<(String, String)> = vec![
            ("Content-Type".into(), "application/json".into()),
            ("Cache-Control".into(), "no-store".into()),
        ];
        want.extend(HeaderProfile::Studio.resolve(None));
        for r in [
            respond(&facts("GET", "/api/health"), 0, "v"),
            respond(&facts("GET", "/nope"), 0, "v"),
            json_error(403, "forbidden_origin", &[]),
            json_ok(serde_json::json!({ "ok": true })),
            internal_error(),
        ] {
            assert_eq!(r.headers, want, "on {}", r.status);
            assert_ne!(
                r.header("Content-Security-Policy"),
                SECURITY_HEADERS
                    .iter()
                    .find(|(k, _)| *k == "Content-Security-Policy")
                    .map(|(_, v)| *v)
            );
        }
        assert_eq!(
            internal_error().header("Cross-Origin-Resource-Policy"),
            Some("same-origin")
        );
    }
}
