//! Rollback: an authorized, audited action that re-activates a RETAINED VERIFIED
//! deployment (`published_versions`) on Cloudflare.
//!
//! What it does not do, by construction: it never uploads assets, never creates a
//! Cloudflare version, never touches `site_revisions` or the editing session (editable
//! IR is not rewound), and never re-runs a publish job. It only creates one deployment
//! of an existing version (same lease and reconcile protocol as a publish) and records
//! it. The revision high-water mark is left alone, so jobs for older revisions stay
//! refused; the way forward after a rollback is a NEWER revision (a forward fix).
//!
//! Compatibility: a retained version records the site D1 migration level its Worker was
//! built for (`schema_compat`). If the target's current level differs, the old Worker
//! could misread the live database, so the rollback is refused and a forward fix is
//! required. Cloudflare may also refuse (version deleted after 30 days of retention,
//! secrets changed); Solo never retries with `force`.

use sws_assets::time::utc_text;

use super::api::CfApi;
use super::http::{HttpClient, Runtime, Secrets};
use super::pipeline::{PublishEnv, lock_and_activate, valid_account_id, valid_script_name};
use super::store::{AuditAction, AuditResult, AuditRow, PublishStore, Target};
use super::{Activation, Stage};
use crate::auth::Principal;
use crate::model::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackRequest {
    pub workspace_id: String,
    pub site_id: String,
    /// `published_versions.id`.
    pub published_id: String,
    pub principal: Principal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackOutcome {
    pub target_id: String,
    pub published_id: String,
    pub cf_version_id: String,
    pub revision_id: String,
    pub previous_live_version_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub activation: Activation,
}

fn rerr(status: u16, code: &'static str, message: impl Into<String>) -> RollbackError {
    RollbackError {
        status,
        code,
        message: message.into(),
        activation: Activation::NotAttempted,
    }
}

const FORWARD_FIX: &str = "publish a forward fix (a new revision) instead";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzError {
    /// Not a member of the workspace, or token for another workspace: uniform 403.
    NotMember,
    /// Member, but role does not allow activation: 403 `forbidden_role`.
    InsufficientRole,
    /// Token scope below `activate`: 403 `insufficient_scope`.
    InsufficientScope,
    /// Browser session outside the 15-minute re-auth window: 401 `reauth_required`.
    ReauthRequired,
    Unavailable,
}

/// Owner and publisher may activate (#60 roles).
pub fn activate_allowed(role: Role) -> bool {
    matches!(role, Role::Owner | Role::Publisher)
}

/// The ONE authorization seam for activation-class actions (rollback and manual
/// activation): checks D1 membership role, token scope (`activate`), workspace
/// binding, and the 15-minute re-authentication window for interactive sessions (#65).
pub async fn require_activate<S: PublishStore>(
    store: &S,
    principal: &Principal,
    workspace_id: &str,
    _site_id: &str,
) -> Result<Role, AuthzError> {
    let role = store
        .role(workspace_id, principal.user_id)
        .await
        .map_err(|_| AuthzError::Unavailable)?;
    match crate::auth::policy::authorize(
        &principal.credential,
        role,
        workspace_id,
        crate::auth::policy::Permission::Activate,
    ) {
        Ok(()) => Ok(role.unwrap_or(Role::Editor)),
        Err(crate::auth::policy::Denied::Forbidden) => Err(AuthzError::NotMember),
        Err(crate::auth::policy::Denied::Role) => Err(AuthzError::InsufficientRole),
        Err(crate::auth::policy::Denied::Scope | crate::auth::policy::Denied::SessionRequired) => {
            Err(AuthzError::InsufficientScope)
        }
        Err(crate::auth::policy::Denied::ReauthRequired) => Err(AuthzError::ReauthRequired),
    }
}

pub async fn run_rollback<C, R, S, K>(
    req: &RollbackRequest,
    env: &PublishEnv<'_, C, R, S, K>,
) -> Result<RollbackOutcome, RollbackError>
where
    C: HttpClient,
    R: Runtime,
    S: PublishStore,
    K: Secrets,
{
    let (rt, store) = (env.rt, env.store);
    let now = || utc_text(rt.now_secs());
    let ws = req.workspace_id.as_str();
    let site = req.site_id.as_str();

    let auth = require_activate(store, &req.principal, ws, site).await;
    if auth == Err(AuthzError::Unavailable) {
        return Err(rerr(503, "store_unavailable", "D1 is unavailable"));
    }
    if auth == Err(AuthzError::NotMember) {
        return Err(rerr(403, "forbidden", "not allowed"));
    }
    if auth == Err(AuthzError::ReauthRequired) {
        return Err(rerr(
            401,
            "reauth_required",
            "recent sign-in required for activation",
        ));
    }
    let mut target: Target = match store.target(ws, site).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err(rerr(
                404,
                "target_not_found",
                "no publish target configured for this site",
            ));
        }
        Err(_) => return Err(rerr(503, "store_unavailable", "D1 is unavailable")),
    };
    let audit = |t: &Target,
                 result: AuditResult,
                 stage: Option<&'static str>,
                 code: Option<&'static str>,
                 version: Option<String>,
                 revision: Option<String>| AuditRow {
        workspace_id: t.workspace_id.clone(),
        site_id: t.site_id.clone(),
        target_id: t.id.clone(),
        actor_user_id: Some(req.principal.user_id),
        action: AuditAction::Rollback,
        result,
        cf_version_id: version,
        published_id: Some(req.published_id.clone()).filter(|p| crate::session::is_valid_id(p)),
        revision_id: revision,
        job_id: None,
        stage,
        error_code: code,
        at: now(),
    };
    if let Err(e) = auth {
        let code = match e {
            AuthzError::InsufficientRole => "forbidden_role",
            AuthzError::InsufficientScope => "insufficient_scope",
            _ => "forbidden",
        };
        let _ = store
            .audit(&audit(
                &target,
                AuditResult::Refused,
                None,
                Some(code),
                None,
                None,
            ))
            .await;
        return Err(rerr(403, code, "not allowed"));
    }

    let pv = match store.published(ws, site, &req.published_id).await {
        Ok(Some(p)) if p.target_id == target.id => p,
        Ok(_) => {
            return Err(rerr(
                404,
                "deployment_not_found",
                "no retained verified deployment with this id",
            ));
        }
        Err(_) => return Err(rerr(503, "store_unavailable", "D1 is unavailable")),
    };
    let refuse = |t: &Target, code: &'static str| {
        let row = audit(
            t,
            AuditResult::Refused,
            Some(Stage::Prepare.as_str()),
            Some(code),
            Some(pv.cf_version_id.clone()),
            Some(pv.revision_id.clone()),
        );
        async move {
            let _ = store.audit(&row).await;
        }
    };
    if target.live_version_id.as_deref() == Some(pv.cf_version_id.as_str()) {
        return Err(rerr(
            409,
            "already_live",
            "this deployment is already serving traffic",
        ));
    }
    if pv.schema_compat != target.site_schema_level {
        refuse(&target, "rollback_schema_incompatible").await;
        return Err(rerr(
            409,
            "rollback_schema_incompatible",
            format!(
                "this deployment was built for site database level {} but the site is now at level {}; rolling back is unsafe, {FORWARD_FIX}",
                pv.schema_compat, target.site_schema_level
            ),
        ));
    }
    if !valid_account_id(&target.account_id) || !valid_script_name(&target.script_name) {
        return Err(rerr(
            500,
            "invalid_target",
            "publish target account or script name is invalid",
        ));
    }
    let Some(token) = env
        .secrets
        .api_token(&target.token_secret)
        .filter(|t| !t.is_empty())
    else {
        return Err(rerr(
            503,
            "token_missing",
            "the Cloudflare API token secret is not configured",
        ));
    };

    // Activation-time authorization recheck right before taking the target lease.
    match require_activate(store, &req.principal, ws, site).await {
        Ok(_) => {}
        Err(AuthzError::Unavailable) => {
            return Err(rerr(503, "store_unavailable", "D1 is unavailable"));
        }
        Err(AuthzError::ReauthRequired) => {
            refuse(&target, "activation_unauthorized").await;
            return Err(rerr(
                401,
                "reauth_required",
                "recent sign-in required for activation",
            ));
        }
        Err(_) => {
            refuse(&target, "activation_unauthorized").await;
            return Err(rerr(403, "activation_unauthorized", "not allowed"));
        }
    }

    let previous = target.live_version_id.clone();
    let (account, script) = (target.account_id.clone(), target.script_name.clone());
    let api = CfApi::new(env.http, rt, env.api_base, &account, &script, &token);
    let job = format!("rb-{}-{}", rt.now_secs(), req.principal.user_id);
    if let Err(f) =
        lock_and_activate(&api, rt, store, &mut target, &job, None, &pv.cf_version_id).await
    {
        let (status, code, message) = match f.code {
            "target_busy" | "lease_lost" => (409, "target_busy", f.message.clone()),
            "activation_rejected" => (
                409,
                "rollback_rejected",
                format!(
                    "Cloudflare refused to re-activate this version ({}); {FORWARD_FIX}",
                    api.clean(&f.message)
                ),
            ),
            "activation_superseded" => (409, "activation_superseded", f.message.clone()),
            "store_unavailable" => (503, "store_unavailable", f.message.clone()),
            code => (502, code, api.clean(&f.message)),
        };
        if f.stage != Stage::Lock {
            let _ = store
                .audit(&audit(
                    &target,
                    AuditResult::Failed,
                    Some(f.stage.as_str()),
                    Some(code),
                    Some(pv.cf_version_id.clone()),
                    Some(pv.revision_id.clone()),
                ))
                .await;
        }
        rt.log(&format!(
            "rollback target={} stage={} code={code}",
            target.id,
            f.stage.as_str()
        ));
        return Err(RollbackError {
            status,
            code,
            message,
            activation: f.activation,
        });
    }
    let ok = audit(
        &target,
        AuditResult::Succeeded,
        Some(Stage::Activate.as_str()),
        None,
        Some(pv.cf_version_id.clone()),
        Some(pv.revision_id.clone()),
    );
    match store.record_rollback(&target, &job, &pv, &ok).await {
        Ok(true) => {
            rt.log(&format!(
                "rollback target={} to {} by user {}",
                target.id, pv.id, req.principal.user_id
            ));
            Ok(RollbackOutcome {
                target_id: target.id.clone(),
                published_id: pv.id.clone(),
                cf_version_id: pv.cf_version_id.clone(),
                revision_id: pv.revision_id.clone(),
                previous_live_version_id: previous,
            })
        }
        Ok(false) => Err(RollbackError {
            status: 409,
            code: "lease_lost_after_activation",
            message: "the version went live but the lease was lost before it was recorded".into(),
            activation: Activation::Activated,
        }),
        Err(_) => Err(RollbackError {
            status: 503,
            code: "store_unavailable",
            message: "the version went live but D1 was unavailable; it will be reconciled".into(),
            activation: Activation::Activated,
        }),
    }
}
