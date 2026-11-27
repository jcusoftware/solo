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
    /// `GET /api/sites/{workspace_id}/{site_id}/session`: WebSocket command
    /// session (#62). Ids already satisfy the registry id grammar.
    Session {
        workspace_id: String,
        site_id: String,
    },
    /// Known path, wrong method. Carries the `Allow` header value.
    MethodNotAllowed(&'static str),
    NotFound,
}

/// `/api/sites/{workspace_id}/{site_id}/session` -> the two ids.
fn session_ids(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/sites/")?;
    let mut parts = rest.split('/');
    let (w, s, tail) = (parts.next()?, parts.next()?, parts.next()?);
    if tail != "session" || parts.next().is_some() {
        return None;
    }
    (crate::session::is_valid_id(w) && crate::session::is_valid_id(s)).then_some((w, s))
}

pub fn route(method: &str, path: &str) -> Route {
    if let Some((w, s)) = session_ids(path) {
        return match method {
            "GET" => Route::Session {
                workspace_id: w.to_string(),
                site_id: s.to_string(),
            },
            _ => Route::MethodNotAllowed("GET"),
        };
    }
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
    fn session_route_parses_ids_strictly() {
        assert_eq!(
            route("GET", "/api/sites/ws-a/site_1/session"),
            Route::Session {
                workspace_id: "ws-a".into(),
                site_id: "site_1".into()
            }
        );
        assert_eq!(
            route("POST", "/api/sites/ws-a/site_1/session"),
            Route::MethodNotAllowed("GET")
        );
        for p in [
            "/api/sites/ws-a/site_1",
            "/api/sites/ws-a/site_1/session/",
            "/api/sites/ws-a/site_1/session/x",
            "/api/sites//site_1/session",
            "/api/sites/WS/site_1/session",
            "/api/sites/ws.a/site_1/session",
            "/api/sites/ws-a/si%20te/session",
            "/api/sites/ws-a/site_1/sessions",
        ] {
            assert_eq!(route("GET", p), Route::NotFound, "{p}");
        }
        let long = "a".repeat(65);
        assert_eq!(
            route("GET", &format!("/api/sites/{long}/s/session")),
            Route::NotFound
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
