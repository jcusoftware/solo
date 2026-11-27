//! Worker-side entry for the session WebSocket (wasm only).
//!
//! Order matters and is fixed: bounds/precheck (done by `lib.rs`), then the
//! browser Origin gate (403), then authenticate (401), then membership from D1
//! (403), then the site (404), then forward to the site's Durable Object. Internal headers are always overwritten
//! here; whatever a client sends under those names (or the dev-auth header) is
//! never forwarded.

use worker::{Env, Headers, Method, Request, RequestInit, Response, Result};

use crate::auth;
use crate::model::Role;
use crate::repo;
use crate::respond::json_error;
use crate::session::authz::role_from_row;
use crate::session::http::to_response;
use crate::session::{
    H_CREDENTIAL, H_ROLE, H_SITE, H_USER, H_WORKSPACE, PATH_REVISION, PATH_REVOKE, PATH_SESSION,
    object_name, origin_allowed,
};

/// Durable Object binding name (`wrangler.studio.jsonc`).
pub const BINDING: &str = "SITE_SESSION";

fn stub(env: &Env, workspace_id: &str, site_id: &str) -> Result<worker::Stub> {
    env.durable_object(BINDING)?
        .id_from_name(&object_name(workspace_id, site_id))?
        .get_stub()
}

/// The caller's current role in the workspace, from D1 (never from a client).
pub async fn member_role(env: &Env, workspace_id: &str, user_id: i64) -> Result<Option<Role>> {
    let db = crate::d1::database(env)?;
    let row = crate::d1::first(&db, &repo::membership_get(workspace_id, user_id)).await?;
    Ok(row.as_ref().and_then(role_from_row))
}

/// Handle `GET /api/sites/{workspace_id}/{site_id}/session` with `Upgrade: websocket`.
/// Returns either a locally decided error (with the full security header set)
/// or the Durable Object's response (101 + the client end of the socket).
pub async fn open(req: Request, env: &Env, workspace_id: &str, site_id: &str) -> Result<Response> {
    // Cross-site WebSocket hijacking: a browser always sends `Origin`, and cookie
    // credentials (#65) would otherwise ride along from any page. A request with
    // an Origin must come from the Studio's own origin; CLI/MCP token clients send
    // none and are allowed.
    let origin = req.headers().get("Origin")?;
    let public_origin = sws_security::worker_utils::var(env, "STUDIO_PUBLIC_ORIGIN");
    if !origin_allowed(origin.as_deref(), public_origin.as_deref()) {
        return to_response(json_error(403, "forbidden_origin", &[]));
    }
    // The dev-auth header is read ONLY in the `insecure-dev-auth` test build; in every
    // other build the name does not even appear in the Wasm (checked by
    // `npm run check:studio:release`). Even there it needs the test-only runtime
    // fence (a var only the test script passes, and a loopback Host).
    #[cfg(feature = "insecure-dev-auth")]
    let dev_user = if dev_fence_open(&req, env) {
        req.headers().get(auth::DEV_USER_HEADER)?
    } else {
        None
    };
    #[cfg(not(feature = "insecure-dev-auth"))]
    let dev_user: Option<String> = None;
    let Ok(principal) = auth::authenticate(dev_user.as_deref()) else {
        return to_response(json_error(401, "unauthenticated", &[]));
    };
    let Some(role) = member_role(env, workspace_id, principal.user_id).await? else {
        // Not a member of this workspace (or no such workspace): same answer, so
        // ids cannot be probed.
        return to_response(json_error(403, "forbidden", &[]));
    };
    let db = crate::d1::database(env)?;
    if crate::d1::first(&db, &repo::site_get(workspace_id, site_id))
        .await?
        .is_none()
    {
        return to_response(json_error(404, "not_found", &[]));
    }

    let headers = Headers::new();
    for (name, value) in req.headers().entries() {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("x-sws-") || lower == "cookie" || lower == "authorization" {
            continue;
        }
        headers.append(&name, &value)?;
    }
    headers.set(H_WORKSPACE, workspace_id)?;
    headers.set(H_SITE, site_id)?;
    headers.set(H_USER, &principal.user_id.to_string())?;
    headers.set(H_ROLE, role.as_str())?;
    // #65: set H_CREDENTIAL to the session/token id once real auth resolves one.
    let _ = H_CREDENTIAL;

    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let forward =
        Request::new_with_init(&format!("https://session.internal{PATH_SESSION}"), &init)?;
    stub(env, workspace_id, site_id)?
        .fetch_with_request(forward)
        .await
}

/// The `insecure-dev-auth` build's runtime fence (see [`crate::session::devfence`]).
#[cfg(feature = "insecure-dev-auth")]
pub fn dev_fence_open(req: &Request, env: &Env) -> bool {
    let flag = sws_security::worker_utils::var(env, crate::session::devfence::FENCE_VAR);
    let host = req.headers().get("Host").ok().flatten();
    crate::session::devfence::open(flag.as_deref(), host.as_deref())
}

/// The snapshot bytes of a checkpoint revision, by `revision_id` (#64 reads the
/// revision's IR through this). `None` when that snapshot is no longer retained
/// (only the pending checkpoint, the latest confirmed one and the latest confirmed
/// publish checkpoint are kept).
pub async fn fetch_revision(
    env: &Env,
    workspace_id: &str,
    site_id: &str,
    revision_id: &str,
) -> Result<Option<serde_json::Value>> {
    let req = Request::new(
        &format!("https://session.internal{PATH_REVISION}?id={revision_id}"),
        Method::Get,
    )?;
    let mut resp = stub(env, workspace_id, site_id)?
        .fetch_with_request(req)
        .await?;
    match resp.status_code() {
        200 => Ok(Some(resp.json().await?)),
        404 => Ok(None),
        n => Err(worker::Error::RustError(format!(
            "revision request failed with HTTP {n}"
        ))),
    }
}

/// #65 hook: close every socket `user_id` holds on this site and stop delivering
/// to them (logout, token or session revocation, membership change). The site
/// object also does this on its own within the recheck interval when the D1
/// membership disappears; this makes it immediate.
pub async fn revoke_user(env: &Env, workspace_id: &str, site_id: &str, user_id: i64) -> Result<()> {
    let mut init = RequestInit::new();
    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(
            serde_json::json!({ "user_id": user_id }).to_string().into(),
        ));
    let req = Request::new_with_init(&format!("https://session.internal{PATH_REVOKE}"), &init)?;
    let resp = stub(env, workspace_id, site_id)?
        .fetch_with_request(req)
        .await?;
    if resp.status_code() >= 400 {
        return Err(worker::Error::RustError(format!(
            "revoke request failed with HTTP {}",
            resp.status_code()
        )));
    }
    Ok(())
}
