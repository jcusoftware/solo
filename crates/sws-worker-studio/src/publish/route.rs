//! `POST /api/sites/{workspace_id}/{site_id}/deployments/{published_id}/rollback`
//! (host-testable parse and response mapping). `published_id` is a
//! `published_versions.id`. Responses use the [`crate::respond`] helpers: the `studio`
//! header profile and `Cache-Control: no-store`.

use serde_json::json;

use crate::respond::Resp;
use crate::session::is_valid_id;

use super::rollback::{RollbackError, RollbackOutcome};

pub const ALLOW: &str = "POST";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackRoute {
    pub workspace_id: String,
    pub site_id: String,
    pub published_id: String,
}

pub fn parse(path: &str) -> Option<RollbackRoute> {
    let rest = path.strip_prefix("/api/sites/")?;
    let p: Vec<&str> = rest.split('/').collect();
    match p.as_slice() {
        [ws, site, "deployments", id, "rollback"]
            if is_valid_id(ws) && is_valid_id(site) && is_valid_id(id) =>
        {
            Some(RollbackRoute {
                workspace_id: ws.to_string(),
                site_id: site.to_string(),
                published_id: id.to_string(),
            })
        }
        _ => None,
    }
}

pub fn ok(o: &RollbackOutcome) -> Resp {
    Resp::new(
        200,
        Some(json!({
            "ok": true,
            "rollback": {
                "target_id": o.target_id,
                "published_id": o.published_id,
                "cf_version_id": o.cf_version_id,
                "revision_id": o.revision_id,
                "previous_live_version_id": o.previous_live_version_id,
            }
        })),
        &[],
    )
}

pub fn err(e: &RollbackError) -> Resp {
    Resp::new(
        e.status,
        Some(json!({
            "ok": false,
            "error": e.code,
            "message": e.message,
            "activation": e.activation.as_str(),
        })),
        &[],
    )
}

pub fn method_not_allowed() -> Resp {
    Resp::new(
        405,
        Some(json!({ "ok": false, "error": "method_not_allowed" })),
        &[("Allow", ALLOW)],
    )
}

pub fn unauthenticated() -> Resp {
    Resp::new(
        401,
        Some(json!({ "ok": false, "error": "unauthenticated" })),
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::Activation;
    use sws_security::headers::STUDIO_HEADERS;

    #[test]
    fn parses_strictly() {
        assert_eq!(
            parse("/api/sites/ws-a/site_1/deployments/pv-1/rollback"),
            Some(RollbackRoute {
                workspace_id: "ws-a".into(),
                site_id: "site_1".into(),
                published_id: "pv-1".into()
            })
        );
        for p in [
            "/api/sites/ws-a/site_1/deployments/pv-1",
            "/api/sites/ws-a/site_1/deployments/pv-1/rollback/",
            "/api/sites/ws-a/site_1/deployments//rollback",
            "/api/sites/WS/site_1/deployments/pv-1/rollback",
            "/api/sites/ws-a/site_1/deployment/pv-1/rollback",
            "/api/sites/ws-a/site_1/deployments/pv.1/rollback",
        ] {
            assert_eq!(parse(p), None, "{p}");
        }
    }

    #[test]
    fn responses_carry_the_studio_profile_and_no_store() {
        let e = RollbackError {
            status: 409,
            code: "rollback_schema_incompatible",
            message: "forward fix".into(),
            activation: Activation::NotAttempted,
        };
        for r in [err(&e), method_not_allowed(), unauthenticated()] {
            assert_eq!(r.header("Cache-Control"), Some("no-store"));
            for (k, v) in STUDIO_HEADERS {
                assert_eq!(r.header(k), Some(*v), "{k}");
            }
        }
        assert_eq!(
            err(&e).body.as_ref().map(|b| b["error"].clone()),
            Some("rollback_schema_incompatible".into())
        );
        assert_eq!(method_not_allowed().header("Allow"), Some("POST"));
    }
}
