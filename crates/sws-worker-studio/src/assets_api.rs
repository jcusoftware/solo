//! Host-testable pieces of the asset HTTP API: route parsing, backend selection,
//! role checks, and response construction. The wasm glue (`assets_http.rs`) only
//! performs I/O and calls into this module and `sws-assets`.
//!
//! Routes (all authenticated, all tenant-scoped, none accept user HTML):
//!
//! | Route | Methods |
//! | --- | --- |
//! | `/api/sites/{workspace}/{site}/assets` | `GET` (list + usage + limits) |
//! | `/api/sites/{workspace}/{site}/assets/{sha256}` | `PUT` (upload), `GET`, `DELETE` |
//! | `/api/sites/{workspace}/{site}/assets/reconcile` | `POST` |

use serde_json::{Value, json};
use sws_assets::error::AssetError;
use sws_assets::limits::{AssetLimits, CARDLESS_D1, R2_OPT_IN};
use sws_assets::validate::{ContentKind, is_id};

use crate::model::{Role, StorageBackend};
use crate::respond::Resp;

/// R2 bucket binding name (opt-in; see `deploy/cloudflare/wrangler.studio.example.jsonc`).
pub const R2_BINDING: &str = "MEDIA_BUCKET";
/// Var selecting the byte backend: `d1` (default) or `r2`.
pub const BACKEND_VAR: &str = "STUDIO_ASSET_BACKEND";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRoute {
    List {
        ws: String,
        site: String,
    },
    Reconcile {
        ws: String,
        site: String,
    },
    /// `sha` is the raw path segment; validate it with `validate::is_sha256`.
    Asset {
        ws: String,
        site: String,
        sha: String,
    },
}

impl AssetRoute {
    pub fn allow(&self) -> &'static str {
        match self {
            AssetRoute::List { .. } => "GET",
            AssetRoute::Reconcile { .. } => "POST",
            AssetRoute::Asset { .. } => "GET, PUT, DELETE",
        }
    }

    pub fn permits(&self, method: &str) -> bool {
        self.allow().split(", ").any(|m| m == method)
    }

    pub fn ids(&self) -> (&str, &str) {
        match self {
            AssetRoute::List { ws, site }
            | AssetRoute::Reconcile { ws, site }
            | AssetRoute::Asset { ws, site, .. } => (ws, site),
        }
    }
}

/// Match `/api/sites/{ws}/{site}/assets[/{tail}]`. Anything with an id that is not
/// a plain `[a-z0-9_-]{1,64}` is not a route (404), so no id can carry `/`, `..` or
/// encoded separators into a storage key.
pub fn parse_route(path: &str) -> Option<AssetRoute> {
    let rest = path.strip_prefix("/api/sites/")?;
    let mut it = rest.split('/');
    let (ws, site, kw) = (it.next()?, it.next()?, it.next()?);
    if kw != "assets" || !is_id(ws) || !is_id(site) {
        return None;
    }
    let (ws, site) = (ws.to_string(), site.to_string());
    match (it.next(), it.next()) {
        (None, _) => Some(AssetRoute::List { ws, site }),
        (Some("reconcile"), None) => Some(AssetRoute::Reconcile { ws, site }),
        (Some(sha), None) if !sha.is_empty() && sha.len() <= 128 => Some(AssetRoute::Asset {
            ws,
            site,
            sha: sha.to_string(),
        }),
        _ => None,
    }
}

/// Every role may write today (owner, publisher, editor are all "editor or
/// higher"); the function exists so a future read-only role is one `match` away.
pub fn can_write(role: Role) -> bool {
    matches!(role, Role::Owner | Role::Publisher | Role::Editor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    D1,
    R2,
}

impl Backend {
    pub fn limits(self) -> AssetLimits {
        match self {
            Backend::D1 => CARDLESS_D1,
            Backend::R2 => R2_OPT_IN,
        }
    }

    pub fn storage(self) -> StorageBackend {
        match self {
            Backend::D1 => StorageBackend::D1,
            Backend::R2 => StorageBackend::R2,
        }
    }
}

/// Pick the backend for NEW uploads. Cardless D1 is the default; R2 is opt-in and
/// needs both `STUDIO_ASSET_BACKEND=r2` and the bucket binding. An explicit opt-in
/// without a binding is a clear diagnostic, never a silent fallback (the caps differ).
pub fn select_backend(var: Option<&str>, r2_bound: bool) -> Result<Backend, AssetError> {
    match var.map(str::trim).filter(|v| !v.is_empty()) {
        None | Some("d1") => Ok(Backend::D1),
        Some("r2") if r2_bound => Ok(Backend::R2),
        Some("r2") => Err(AssetError::R2NotConfigured),
        Some(_) => Err(AssetError::BackendInvalid),
    }
}

/// JSON error with an actionable hint and optional structured `details`.
pub fn error_resp(e: AssetError, details: Option<Value>) -> Resp {
    let mut body = json!({ "ok": false, "error": e.code(), "message": e.hint() });
    if let (Some(d), Some(obj)) = (details, body.as_object_mut()) {
        obj.insert("details".into(), d);
    }
    Resp::new(e.status(), Some(body), &[])
}

pub fn method_not_allowed(route: &AssetRoute) -> Resp {
    Resp::new(
        405,
        Some(json!({ "ok": false, "error": "method_not_allowed" })),
        &[("Allow", route.allow())],
    )
}

pub fn ok_json(status: u16, body: Value) -> Resp {
    Resp::new(status, Some(body), &[])
}

/// Headers for an asset download. User bytes are only ever served as an allowlisted
/// non-HTML type, as an attachment, with `nosniff` and the Studio CSP (all from
/// `SECURITY_HEADERS`) and `no-store`.
pub fn download_resp(kind: ContentKind, sha256: &str) -> Resp {
    let mut r = Resp::new(
        200,
        None,
        &[(
            "Content-Disposition",
            &format!("attachment; filename=\"{sha256}.{}\"", kind.ext()),
        )],
    );
    r.headers
        .retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
    r.headers.push(("Content-Type".into(), kind.mime().into()));
    r
}

pub fn limits_json(l: &AssetLimits) -> Value {
    json!({
        "max_asset_bytes": l.max_asset_bytes,
        "max_site_bytes": l.max_site_bytes,
        "max_workspace_bytes": l.max_workspace_bytes,
        "max_site_assets": l.max_site_assets,
        // null: this backend's bytes do not live in D1, so there is no database cap.
        "max_database_blob_bytes": (l.max_database_bytes != u64::MAX).then_some(l.max_database_bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_security::headers::SECURITY_HEADERS;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn routes_parse() {
        assert_eq!(
            parse_route("/api/sites/ws-1/s1/assets"),
            Some(AssetRoute::List {
                ws: "ws-1".into(),
                site: "s1".into()
            })
        );
        assert_eq!(
            parse_route("/api/sites/ws-1/s1/assets/reconcile"),
            Some(AssetRoute::Reconcile {
                ws: "ws-1".into(),
                site: "s1".into()
            })
        );
        assert_eq!(
            parse_route(&format!("/api/sites/ws-1/s1/assets/{SHA}")),
            Some(AssetRoute::Asset {
                ws: "ws-1".into(),
                site: "s1".into(),
                sha: SHA.into()
            })
        );
    }

    #[test]
    fn malformed_or_traversing_paths_do_not_route() {
        for p in [
            "/api/sites/ws/s/assets/",
            "/api/sites/ws/s/assets/a/b",
            "/api/sites/../s/assets",
            "/api/sites/ws/../assets",
            "/api/sites/WS/s/assets",
            "/api/sites/ws/s%2Fx/assets",
            "/api/sites/ws/s/other",
            "/api/sites/ws/assets",
            "/api/sites//s/assets",
            "/api/health",
        ] {
            assert_eq!(parse_route(p), None, "{p}");
        }
    }

    #[test]
    fn method_permissions() {
        let a = parse_route(&format!("/api/sites/w/s/assets/{SHA}")).unwrap();
        assert!(a.permits("PUT") && a.permits("GET") && a.permits("DELETE"));
        assert!(!a.permits("POST") && !a.permits("HEAD") && !a.permits("PATCH"));
        let r = parse_route("/api/sites/w/s/assets/reconcile").unwrap();
        assert!(r.permits("POST") && !r.permits("GET"));
        assert_eq!(method_not_allowed(&r).header("Allow"), Some("POST"));
    }

    #[test]
    fn backend_selection() {
        assert_eq!(select_backend(None, false), Ok(Backend::D1));
        assert_eq!(select_backend(Some("d1"), true), Ok(Backend::D1));
        assert_eq!(select_backend(Some(""), true), Ok(Backend::D1));
        assert_eq!(select_backend(Some("r2"), true), Ok(Backend::R2));
        assert_eq!(
            select_backend(Some("r2"), false),
            Err(AssetError::R2NotConfigured)
        );
        assert_eq!(
            select_backend(Some("s3"), true),
            Err(AssetError::BackendInvalid)
        );
        assert_eq!(Backend::D1.limits(), CARDLESS_D1);
        assert_eq!(Backend::R2.storage(), StorageBackend::R2);
    }

    #[test]
    fn errors_are_actionable_json_with_security_headers() {
        let r = error_resp(AssetError::R2NotConfigured, None);
        assert_eq!(r.status, 503);
        let body = r.body.as_ref().unwrap();
        assert_eq!(body["error"], "r2_not_configured");
        assert!(body["message"].as_str().unwrap().contains("MEDIA_BUCKET"));
        assert_eq!(r.header("Cache-Control"), Some("no-store"));
        for (k, v) in SECURITY_HEADERS {
            assert_eq!(r.header(k), Some(*v), "{k}");
        }
        let r = error_resp(
            AssetError::AssetTooLarge,
            Some(json!({"max_asset_bytes": 1})),
        );
        assert_eq!(r.body.unwrap()["details"]["max_asset_bytes"], 1);
    }

    #[test]
    fn downloads_are_never_html_and_never_cached() {
        let r = download_resp(ContentKind::Png, SHA);
        assert_eq!(r.header("Content-Type"), Some("image/png"));
        assert_eq!(r.header("X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(r.header("Cache-Control"), Some("no-store"));
        assert!(
            r.header("Content-Disposition")
                .unwrap()
                .starts_with("attachment;")
        );
        assert_eq!(
            r.headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .count(),
            1
        );
        for k in ContentKind::ALL {
            assert!(!k.mime().contains("html") && !k.mime().contains("svg"));
        }
    }

    #[test]
    fn everyone_with_a_role_can_write() {
        for r in Role::ALL {
            assert!(can_write(*r));
        }
    }
}
