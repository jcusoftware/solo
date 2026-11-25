//! Host-testable request routing and health payload for `sws-worker-site`.

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Health,
    HealthHead,
    FormSubmit(String),
    InquireCompat,
    Newsletter,
    /// Known API path, wrong method. Carries the `Allow` header value.
    MethodNotAllowed(&'static str),
    /// Unknown `/api/*` path.
    NotFound,
    /// Anything outside `/api/` is delegated to the static-assets binding.
    Asset,
}

/// Validates a form identifier against `[a-z0-9][a-z0-9_-]{0,63}`.
pub fn is_valid_form_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first_ok = matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9');
    let rest_ok = bytes[1..]
        .iter()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'));
    first_ok && rest_ok
}

pub fn route(method: &str, path: &str) -> Route {
    let m = method.to_ascii_uppercase();
    match path {
        "/api/health" => match m.as_str() {
            "GET" => Route::Health,
            "HEAD" => Route::HealthHead,
            _ => Route::MethodNotAllowed("GET, HEAD"),
        },
        "/api/inquire" => match m.as_str() {
            "POST" => Route::InquireCompat,
            _ => Route::MethodNotAllowed("POST"),
        },
        "/api/newsletter" => match m.as_str() {
            "POST" => Route::Newsletter,
            _ => Route::MethodNotAllowed("POST"),
        },
        p if p.starts_with("/api/forms/") => {
            let id = &p["/api/forms/".len()..];
            if is_valid_form_id(id) {
                match m.as_str() {
                    "POST" => Route::FormSubmit(id.to_string()),
                    _ => Route::MethodNotAllowed("POST"),
                }
            } else {
                Route::NotFound
            }
        }
        p if p == "/api" || p.starts_with("/api/") => Route::NotFound,
        _ => Route::Asset,
    }
}

pub fn health_body(version: &str) -> Value {
    json!({
        "status": "ok",
        "service": "sws-worker-site",
        "version": version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_id_validation() {
        assert!(is_valid_form_id("contact"));
        assert!(is_valid_form_id("a"));
        assert!(is_valid_form_id("0-lead_form_2"));
        assert!(is_valid_form_id(&"a".repeat(64)));

        assert!(!is_valid_form_id(""));
        assert!(!is_valid_form_id(&"a".repeat(65)));
        assert!(!is_valid_form_id("-contact"));
        assert!(!is_valid_form_id("_contact"));
        assert!(!is_valid_form_id("Contact"));
        assert!(!is_valid_form_id("a/b"));
        assert!(!is_valid_form_id("a.b"));
    }

    #[test]
    fn routing_table() {
        assert_eq!(route("GET", "/api/health"), Route::Health);
        assert_eq!(route("head", "/api/health"), Route::HealthHead);
        assert_eq!(route("HEAD", "/api/health"), Route::HealthHead);
        assert_eq!(
            route("POST", "/api/health"),
            Route::MethodNotAllowed("GET, HEAD")
        );

        assert_eq!(
            route("POST", "/api/forms/contact"),
            Route::FormSubmit("contact".to_string())
        );
        assert_eq!(
            route("post", "/api/forms/event_lead-2027"),
            Route::FormSubmit("event_lead-2027".to_string())
        );
        assert_eq!(
            route("GET", "/api/forms/contact"),
            Route::MethodNotAllowed("POST")
        );
        assert_eq!(route("POST", "/api/forms/"), Route::NotFound);
        assert_eq!(route("POST", "/api/forms/-bad"), Route::NotFound);
        assert_eq!(route("POST", "/api/forms/BAD"), Route::NotFound);
        assert_eq!(route("POST", "/api/forms/a/b"), Route::NotFound);

        assert_eq!(route("POST", "/api/inquire"), Route::InquireCompat);
        assert_eq!(
            route("GET", "/api/inquire"),
            Route::MethodNotAllowed("POST")
        );

        assert_eq!(route("POST", "/api/newsletter"), Route::Newsletter);
        assert_eq!(
            route("DELETE", "/api/newsletter"),
            Route::MethodNotAllowed("POST")
        );

        assert_eq!(route("GET", "/api/nope"), Route::NotFound);
        assert_eq!(route("GET", "/api"), Route::NotFound);
        assert_eq!(route("GET", "/"), Route::Asset);
        assert_eq!(route("GET", "/apiary"), Route::Asset);
    }

    #[test]
    fn health_payload() {
        let body = health_body("0.1.0");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "sws-worker-site");
        assert_eq!(body["version"], "0.1.0");
    }
}
