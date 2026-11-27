//! Host-testable routing and health payload for `sws-worker-studio`.
//!
//! Scaffold scope (#61): only `/api/health` exists. There is deliberately NO
//! route that writes per edit; the registry is written only by explicit
//! save/publish/checkpoint actions added in later issues (#62 onward).

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Health,
    HealthHead,
    /// Known path, wrong method. Carries the `Allow` header value.
    MethodNotAllowed(&'static str),
    NotFound,
}

pub fn route(method: &str, path: &str) -> Route {
    match (path, method) {
        ("/api/health", "GET") => Route::Health,
        ("/api/health", "HEAD") => Route::HealthHead,
        ("/api/health", _) => Route::MethodNotAllowed("GET, HEAD"),
        _ => Route::NotFound,
    }
}

pub fn health_body(version: &str) -> Value {
    json!({
        "status": "ok",
        "service": "sws-worker-studio",
        "version": version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_routes() {
        assert_eq!(route("GET", "/api/health"), Route::Health);
        assert_eq!(route("HEAD", "/api/health"), Route::HealthHead);
        assert_eq!(
            route("POST", "/api/health"),
            Route::MethodNotAllowed("GET, HEAD")
        );
    }

    #[test]
    fn everything_else_is_404() {
        for p in [
            "/",
            "/api",
            "/api/health/",
            "/api/sites",
            "/api/edit",
            "/index.html",
        ] {
            assert_eq!(route("GET", p), Route::NotFound, "{p}");
            assert_eq!(route("POST", p), Route::NotFound, "{p}");
        }
    }

    #[test]
    fn health_body_has_no_secrets() {
        let body = health_body("0.0.0");
        assert_eq!(body["service"], "sws-worker-studio");
        assert_eq!(body.as_object().map(|o| o.len()), Some(3));
    }
}
