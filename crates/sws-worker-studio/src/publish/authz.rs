//! The single authorization seam for every publish route (wasm only).
//!
//! [`require_publish`] is the ONLY place that decides whether a principal may
//! enqueue or inspect publish jobs for a site. Today it follows the asset-route
//! pattern: the caller is already authenticated ([`crate::auth`]), then D1
//! membership of THIS workspace, then the role (owner or publisher,
//! [`super::can_publish`]), then the site. #65 extends this function (and only this
//! function) with session and token checks, e.g. requiring a `publish`-scoped token
//! for CLI callers; no route handler decides publish authorization itself.

use worker::{Env, Result};

use crate::auth::Principal;
use crate::model::Role;
use crate::repo;
use crate::session::authz::role_from_row;

/// Why publishing was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// Not a member of the workspace, no such workspace, or a role that may not
    /// publish. One answer for all three, so ids cannot be probed.
    Forbidden,
    /// A member who may publish, but the site is not in this workspace.
    SiteNotFound,
}

/// Decide whether `principal` may publish `site` in workspace `ws`.
pub async fn require_publish(
    env: &Env,
    principal: &Principal,
    ws: &str,
    site: &str,
) -> Result<std::result::Result<Role, Denied>> {
    let db = crate::d1::database(env)?;
    let member = crate::d1::first(&db, &repo::membership_get(ws, principal.user_id)).await?;
    let Some(role) = member.as_ref().and_then(role_from_row) else {
        return Ok(Err(Denied::Forbidden));
    };
    if !super::can_publish(role) {
        return Ok(Err(Denied::Forbidden));
    }
    if crate::d1::first(&db, &repo::site_get(ws, site))
        .await?
        .is_none()
    {
        return Ok(Err(Denied::SiteNotFound));
    }
    Ok(Ok(role))
}
