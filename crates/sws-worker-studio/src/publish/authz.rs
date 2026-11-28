//! The single authorization seam for every publish route (wasm only).
//!
//! [`require_publish`] is the ONLY place that decides whether a request may
//! enqueue or inspect publish jobs for a site. It uses the #65 credentials and
//! policy, in this order: authenticate (session cookie or bearer token, via
//! [`crate::auth_http::authenticate`]), the CSRF gate for cookie-borne writes
//! (the Studio `Origin`), the caller's CURRENT role in THIS workspace from D1,
//! [`super::authorize_publish`] (the pure [`crate::auth::policy::authorize`] with
//! [`Permission::Publish`](crate::auth::policy::Permission::Publish)), then the
//! site. So an edit-only token, an editor, a token from another workspace and a
//! browser session signed in more than 15 minutes ago are all refused. No route
//! handler decides publish authorization itself.
//!
//! The `insecure-dev-auth` test build additionally accepts the dev identity, only
//! behind the runtime fence (inside `authenticate`); the release build has no such
//! path.

use worker::{Env, Request, Result};

use crate::auth::Principal;
use crate::auth::policy;
use crate::model::Role;
use crate::repo;

/// Why publishing was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// No valid credential (401 `unauthenticated`).
    Unauthenticated,
    /// A cookie-borne write without the Studio `Origin` (403 `forbidden_origin`).
    ForbiddenOrigin,
    /// The #65 policy refused: not a member or a token for another workspace
    /// (403 `forbidden`), a role that may not publish (403 `forbidden_role`), a
    /// token scope below `publish` (403 `insufficient_scope`), or a session that
    /// needs re-authentication (401 `reauth_required`).
    Policy(policy::Denied),
    /// Allowed to publish in this workspace, but the site is not in it.
    SiteNotFound,
}

impl Denied {
    pub fn status(self) -> u16 {
        match self {
            Denied::Unauthenticated => 401,
            Denied::ForbiddenOrigin => 403,
            Denied::Policy(d) => d.status(),
            Denied::SiteNotFound => 404,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Denied::Unauthenticated => "unauthenticated",
            Denied::ForbiddenOrigin => "forbidden_origin",
            Denied::Policy(d) => d.code(),
            Denied::SiteNotFound => "site_not_found",
        }
    }
}

/// Decide whether the caller of `req` may publish `site` in workspace `ws`.
pub async fn require_publish(
    req: &Request,
    env: &Env,
    method: &str,
    ws: &str,
    site: &str,
) -> Result<std::result::Result<(Principal, Role), Denied>> {
    let Some(principal) = crate::auth_http::authenticate(req, env).await? else {
        return Ok(Err(Denied::Unauthenticated));
    };
    if !crate::auth_http::csrf_ok(req, env, &principal, method) {
        return Ok(Err(Denied::ForbiddenOrigin));
    }
    let db = crate::d1::database(env)?;
    let role = crate::auth_http::role_in(&db, ws, principal.user_id).await?;
    if let Err(d) = super::authorize_publish(&principal.credential, role, ws) {
        return Ok(Err(Denied::Policy(d)));
    }
    let Some(role) = role else {
        return Ok(Err(Denied::Policy(policy::Denied::Forbidden)));
    };
    if crate::d1::first(&db, &repo::site_get(ws, site))
        .await?
        .is_none()
    {
        return Ok(Err(Denied::SiteNotFound));
    }
    Ok(Ok((principal, role)))
}
