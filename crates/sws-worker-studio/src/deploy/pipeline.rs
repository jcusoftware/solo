//! The publish sequence ([`run_publish`]) and the activation core shared with rollback
//! ([`lock_and_activate`]). Pure orchestration over the injected seams.
//!
//! Activation protocol for one target:
//!
//! 1. Re-read the target. An EXPIRED lease is taken over only after reconciling the
//!    stale holder's version with Cloudflare: if it is serving, it is adopted (live
//!    version and high-water mark recorded) before anything else happens; if the
//!    deployments list cannot be read, the target stays busy.
//! 2. A live lease means busy (retryable). A publish whose revision seq is not above
//!    `high_water_seq` is stale (never retryable).
//! 3. Acquire the lease with a conditional UPDATE that re-checks both conditions.
//! 4. Before EVERY activation request, renew the lease (fails if it was lost) and send
//!    once. A definitive rejection stops. Anything else is reconciled with the
//!    deployments list: serving -> done; not serving -> retry (bounded); unreadable ->
//!    `Unknown`, the lease is KEPT so the next holder reconciles after it expires.
//! 5. Verify that the version is first in the deployments list at 100%.

use serde_json::Value;
use sws_assets::hash::sha256_hex;
use sws_assets::time::utc_text;

use super::api::{ApiError, CfApi, MAX_ACTIVATION_ATTEMPTS, RetryPolicy, UploadFile};
use super::bindings;
use super::http::{HttpClient, Runtime, Secrets};
use super::manifest::{self, MAX_UPLOAD_REQUEST_B64_BYTES, Manifest};
use super::secret::ApiToken;
use super::store::{
    AuditAction, AuditResult, AuditRow, LEASE_SECS, LeaseClaim, PublishStore, PublishedVersion,
    Target,
};
use super::{Activation, PublishFailure, PublishJobRef, PublishOutcome, Stage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerModule {
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The site Worker build (`worker-build` output: JS shim + Wasm) uploaded with every
/// version. Supplied by the job (#64) from a release artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerBundle {
    pub main_module: String,
    pub modules: Vec<WorkerModule>,
}

/// What #64 hands over: the rendered output of the pinned revision plus the site
/// Worker bundle. Files are read one at a time (bounded memory).
#[allow(async_fn_in_trait)]
pub trait ArtifactSource {
    /// Every output path, absolute (`/index.html`), including `/_headers` if present.
    fn paths(&self) -> Vec<String>;
    async fn read(&self, path: &str) -> Result<Vec<u8>, String>;
    fn worker(&self) -> &WorkerBundle;
    /// Site D1 migration level the bundled Worker expects.
    fn schema_level(&self) -> i64;
}

pub struct PublishEnv<'a, C, R, S, K> {
    pub http: &'a C,
    pub rt: &'a R,
    pub store: &'a S,
    pub secrets: &'a K,
    /// [`super::CLOUDFLARE_API_BASE`] in production; tests pass the fake's base.
    pub api_base: &'a str,
}

/// Why an activation (publish or rollback) stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivateFail {
    pub stage: Stage,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub activation: Activation,
}

fn afail(
    stage: Stage,
    code: &'static str,
    message: impl Into<String>,
    retryable: bool,
    activation: Activation,
) -> ActivateFail {
    ActivateFail {
        stage,
        code,
        message: message.into(),
        retryable,
        activation,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    Yes,
    No,
    Unknown,
}

/// Is `version_id` the only version of the deployment serving traffic?
pub async fn reconcile<C: HttpClient, R: Runtime>(api: &CfApi<'_, C, R>, version_id: &str) -> Live {
    match api.list_deployments().await {
        Ok(list) => match list.first() {
            Some(d) if d.is_only(version_id) => Live::Yes,
            _ => Live::No,
        },
        Err(_) => Live::Unknown,
    }
}

pub fn valid_account_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn valid_script_name(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn now_text<R: Runtime>(rt: &R) -> String {
    utc_text(rt.now_secs())
}

fn store_fail(stage: Stage, activation: Activation) -> ActivateFail {
    afail(
        stage,
        "store_unavailable",
        "D1 is unavailable",
        true,
        activation,
    )
}

/// Take the target's activation lease (handling a stale one), then activate and verify
/// `version_id`. On success the lease is STILL HELD by `job` (the caller records and
/// releases it atomically). `actor` attributes lease-takeover audit rows.
pub async fn lock_and_activate<C, R, S>(
    api: &CfApi<'_, C, R>,
    rt: &R,
    store: &S,
    target: &mut Target,
    job: &str,
    seq: Option<i64>,
    version_id: &str,
) -> Result<(), ActivateFail>
where
    C: HttpClient,
    R: Runtime,
    S: PublishStore,
{
    let reload = |t: &Target| {
        let (ws, site) = (t.workspace_id.clone(), t.site_id.clone());
        async move { store.target(&ws, &site).await }
    };
    *target = match reload(target).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err(afail(
                Stage::Lock,
                "target_not_found",
                "publish target removed",
                false,
                Activation::NotAttempted,
            ));
        }
        Err(_) => return Err(store_fail(Stage::Lock, Activation::NotAttempted)),
    };

    // 1. Stale lease: reconcile the previous holder's version before taking over.
    let now = now_text(rt);
    if target.lease_stale(&now) {
        let stale_job = target.activating_job.clone().unwrap_or_default();
        let live = match target.activating_version_id.as_deref() {
            Some(v) => reconcile(api, v).await,
            None => Live::No,
        };
        if live == Live::Unknown {
            return Err(afail(
                Stage::Lock,
                "target_busy",
                "a previous activation's outcome could not be confirmed yet",
                true,
                Activation::NotAttempted,
            ));
        }
        let adopt = live == Live::Yes;
        let cleared = store
            .clear_stale(target, &stale_job, &now, adopt)
            .await
            .map_err(|_| store_fail(Stage::Lock, Activation::NotAttempted))?;
        if cleared {
            let _ = store
                .audit(&AuditRow {
                    workspace_id: target.workspace_id.clone(),
                    site_id: target.site_id.clone(),
                    target_id: target.id.clone(),
                    actor_user_id: None,
                    action: AuditAction::LeaseTakeover,
                    result: AuditResult::Succeeded,
                    cf_version_id: target.activating_version_id.clone(),
                    published_id: None,
                    revision_id: None,
                    job_id: Some(stale_job.clone()),
                    stage: Some(Stage::Lock.as_str()),
                    error_code: Some(if adopt {
                        "stale_holder_live_adopted"
                    } else {
                        "stale_holder_not_live"
                    }),
                    at: now.clone(),
                })
                .await;
            rt.log(&format!(
                "publish target {}: expired lease of {stale_job} cleared (adopted: {adopt})",
                target.id
            ));
        }
        *target = match reload(target).await {
            Ok(Some(t)) => t,
            _ => return Err(store_fail(Stage::Lock, Activation::NotAttempted)),
        };
    }

    // 2. Busy / stale checks, then 3. the conditional acquire (re-checks both).
    let stale = |t: &Target| seq.is_some_and(|s| t.high_water_seq >= s);
    let stale_err = || {
        afail(
            Stage::Lock,
            "stale_revision",
            "a newer revision is already live",
            false,
            Activation::NotAttempted,
        )
    };
    let busy_err = || {
        afail(
            Stage::Lock,
            "target_busy",
            "another activation holds this target",
            true,
            Activation::NotAttempted,
        )
    };
    if stale(target) {
        return Err(stale_err());
    }
    if target.lease_live(&now) {
        return Err(busy_err());
    }
    let claim = LeaseClaim {
        job: job.to_string(),
        seq,
        version_id: version_id.to_string(),
        until: utc_text(rt.now_secs() + LEASE_SECS),
        now: now.clone(),
    };
    let got = store
        .acquire(target, &claim)
        .await
        .map_err(|_| store_fail(Stage::Lock, Activation::NotAttempted))?;
    if !got {
        return match reload(target).await {
            Ok(Some(t)) if stale(&t) => Err(stale_err()),
            _ => Err(busy_err()),
        };
    }

    // 4. Activate: renew, send once, reconcile anything non-definitive.
    let release = || async {
        let _ = store.release(target, job).await;
    };
    let mut prior = Activation::NotAttempted;
    let mut activated = false;
    for attempt in 1..=MAX_ACTIVATION_ATTEMPTS {
        let now = now_text(rt);
        let until = utc_text(rt.now_secs() + LEASE_SECS);
        match store.renew(target, job, &until, &now).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(afail(
                    Stage::Lock,
                    "lease_lost",
                    "the activation lease was lost",
                    true,
                    prior,
                ));
            }
            Err(_) => {
                release().await;
                return Err(store_fail(Stage::Lock, prior));
            }
        }
        match api.create_deployment_once(version_id).await {
            Ok(_) => {
                activated = true;
                break;
            }
            Err(ApiError::Rejected { message, .. }) => {
                release().await;
                return Err(afail(
                    Stage::Activate,
                    "activation_rejected",
                    message,
                    false,
                    Activation::NotActivated,
                ));
            }
            Err(e @ ApiError::Unavailable { .. }) => {
                match reconcile(api, version_id).await {
                    Live::Yes => return Ok(()),
                    Live::No => {
                        prior = Activation::NotActivated;
                        if attempt == MAX_ACTIVATION_ATTEMPTS {
                            release().await;
                            return Err(afail(
                                Stage::Activate,
                                "activation_failed",
                                e.message(),
                                true,
                                Activation::NotActivated,
                            ));
                        }
                        let delay = RetryPolicy::DEFAULT.delay_ms(attempt, None);
                        rt.log(&format!("publish target {}: activation attempt {attempt} did not apply; retrying", target.id));
                        rt.sleep_ms(delay).await;
                    }
                    Live::Unknown => {
                        // Keep the lease: whoever takes over after expiry reconciles first.
                        return Err(afail(
                            Stage::Activate,
                            "activation_unknown",
                            "activation outcome could not be confirmed; it will be reconciled before any retry",
                            true,
                            Activation::Unknown,
                        ));
                    }
                }
            }
        }
    }
    debug_assert!(activated);

    // 5. Verify.
    match reconcile(api, version_id).await {
        Live::Yes => Ok(()),
        Live::No => {
            release().await;
            Err(afail(
                Stage::Verify,
                "activation_superseded",
                "another deployment replaced this version right after activation",
                false,
                Activation::Activated,
            ))
        }
        Live::Unknown => Err(afail(
            Stage::Verify,
            "activation_unverified",
            "activation succeeded but could not be verified; it will be reconciled",
            true,
            Activation::Activated,
        )),
    }
}

/// Stable id of the retained row for (target, Cloudflare version).
pub fn published_id(target_id: &str, cf_version_id: &str) -> String {
    format!(
        "pv-{}",
        &sha256_hex(format!("{target_id}:{cf_version_id}").as_bytes())[..24]
    )
}

fn valid_job(job: &PublishJobRef) -> bool {
    use crate::session::is_valid_id;
    is_valid_id(&job.workspace_id)
        && is_valid_id(&job.site_id)
        && is_valid_id(&job.job_id)
        && is_valid_id(&job.revision_id)
        && job.revision_seq >= 1
        && job.requested_by >= 1
}

/// Upload every asset Cloudflare reports missing; returns (completion JWT, files sent).
/// A failed bucket after an ambiguous attempt is re-planned ONCE with a fresh session,
/// which reports only what is still missing (content-addressed, so nothing is lost).
async fn upload_assets<A, C, R>(
    api: &CfApi<'_, C, R>,
    m: &Manifest,
    artifacts: &A,
) -> Result<(String, usize), (Stage, &'static str, String, bool)>
where
    A: ArtifactSource,
    C: HttpClient,
    R: Runtime,
{
    let mut sent = 0usize;
    for plan in 0..2 {
        let session = api.upload_session(&m.session_body()).await.map_err(|e| {
            (
                Stage::UploadSession,
                "upload_session_failed",
                e.message().to_string(),
                e.retryable(),
            )
        })?;
        if session.buckets.is_empty() {
            return Ok((session.jwt, sent));
        }
        let mut completion = None;
        let mut failure = None;
        'buckets: for bucket in &session.buckets {
            let mut batch: Vec<UploadFile> = Vec::new();
            let mut batch_bytes = 0usize;
            for (i, hash) in bucket.iter().enumerate() {
                let entry = m.by_hash(hash).ok_or((
                    Stage::UploadAssets,
                    "upload_plan_invalid",
                    "Cloudflare asked for a hash that is not in the manifest".to_string(),
                    false,
                ))?;
                let bytes = artifacts.read(&entry.path).await.map_err(|_| {
                    (
                        Stage::UploadAssets,
                        "artifact_unreadable",
                        "artifact file could not be read".to_string(),
                        true,
                    )
                })?;
                let b64 = manifest::base64_encode(&bytes);
                drop(bytes);
                if manifest::asset_hash_b64(&b64, &entry.path) != *hash {
                    return Err((
                        Stage::UploadAssets,
                        "artifact_changed",
                        "artifact changed during publish".to_string(),
                        false,
                    ));
                }
                batch_bytes += b64.len();
                batch.push(UploadFile {
                    hash: hash.clone(),
                    content_type: entry.content_type,
                    b64,
                });
                let last = i + 1 == bucket.len();
                if last || batch_bytes >= MAX_UPLOAD_REQUEST_B64_BYTES {
                    match api.upload_files(&session.jwt, &batch).await {
                        Ok(c) => {
                            sent += batch.len();
                            if c.is_some() {
                                completion = c;
                            }
                        }
                        Err(e) => {
                            failure = Some(e);
                            break 'buckets;
                        }
                    }
                    batch.clear();
                    batch_bytes = 0;
                }
            }
        }
        match failure {
            None => {
                return completion.map(|c| (c, sent)).ok_or((
                    Stage::UploadAssets,
                    "upload_incomplete",
                    "uploads finished without a completion token".to_string(),
                    true,
                ));
            }
            Some(e) if plan == 0 && e.retryable() => continue,
            Some(e) => {
                return Err((
                    Stage::UploadAssets,
                    "upload_failed",
                    e.message().to_string(),
                    e.retryable(),
                ));
            }
        }
    }
    Err((
        Stage::UploadAssets,
        "upload_failed",
        "asset upload did not complete".to_string(),
        true,
    ))
}

/// Publish the pinned revision's artifacts to the target's site Worker. Called by the
/// #64 job runner in its uploading/activation phase. Never queues, never retries the
/// whole job itself: a retryable failure is the runner's to reschedule.
pub async fn run_publish<A, C, R, S, K>(
    job: &PublishJobRef,
    artifacts: &A,
    env: &PublishEnv<'_, C, R, S, K>,
) -> Result<PublishOutcome, PublishFailure>
where
    A: ArtifactSource,
    C: HttpClient,
    R: Runtime,
    S: PublishStore,
    K: Secrets,
{
    let rt = env.rt;
    let mut live: Option<String> = None;
    let fail = |live: &Option<String>,
                stage: Stage,
                code: &'static str,
                message: String,
                retryable: bool,
                activation: Activation| {
        rt.log(&format!(
            "publish job={} revision={} stage={} code={code} activation={}",
            job.job_id,
            job.revision_id,
            stage.as_str(),
            activation.as_str()
        ));
        PublishFailure {
            stage,
            revision_id: job.revision_id.clone(),
            revision_seq: job.revision_seq,
            code,
            message,
            retryable,
            activation,
            live_version_id: live.clone(),
        }
    };
    let prep = |live: &Option<String>, code: &'static str, message: &str, retryable: bool| {
        fail(
            live,
            Stage::Prepare,
            code,
            message.to_string(),
            retryable,
            Activation::NotAttempted,
        )
    };

    if !valid_job(job) {
        return Err(prep(
            &live,
            "invalid_job",
            "malformed publish job reference",
            false,
        ));
    }
    let mut target = match env.store.target(&job.workspace_id, &job.site_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err(prep(
                &live,
                "target_not_found",
                "no publish target configured for this site",
                false,
            ));
        }
        Err(_) => return Err(prep(&live, "store_unavailable", "D1 is unavailable", true)),
    };
    live = target.live_version_id.clone();
    if target.high_water_seq >= job.revision_seq {
        return Err(prep(
            &live,
            "stale_revision",
            "a newer revision is already live",
            false,
        ));
    }
    if !valid_account_id(&target.account_id) || !valid_script_name(&target.script_name) {
        return Err(prep(
            &live,
            "invalid_target",
            "publish target account or script name is invalid",
            false,
        ));
    }
    let target_bindings = match bindings::validate(&target.bindings_json) {
        Ok(b) => b,
        Err(e) => return Err(prep(&live, "invalid_target_bindings", &e, false)),
    };
    if artifacts.schema_level() != target.site_schema_level {
        return Err(prep(
            &live,
            "schema_mismatch",
            "the site Worker bundle expects a different site D1 migration level than the target has",
            false,
        ));
    }
    let token: ApiToken = match env
        .secrets
        .api_token(&target.token_secret)
        .filter(|t| !t.is_empty())
    {
        Some(t) => t,
        None => {
            return Err(prep(
                &live,
                "token_missing",
                "the Cloudflare API token secret is not configured",
                false,
            ));
        }
    };
    let m = match Manifest::build(artifacts).await {
        Ok(m) => m,
        Err(e) => return Err(prep(&live, e.code, &e.message, false)),
    };
    let digest = m.digest();
    if job
        .artifact_sha256
        .as_deref()
        .is_some_and(|pin| pin != digest)
    {
        return Err(prep(
            &live,
            "artifact_mismatch",
            "artifacts do not match the revision pinned by the job",
            false,
        ));
    }

    let (account, script) = (target.account_id.clone(), target.script_name.clone());
    let api = CfApi::new(env.http, rt, env.api_base, &account, &script, &token);

    let (completion, uploaded) = match upload_assets(&api, &m, artifacts).await {
        Ok(v) => v,
        Err((stage, code, msg, retryable)) => {
            return Err(fail(
                &live,
                stage,
                code,
                api.clean(&msg),
                retryable,
                Activation::NotAttempted,
            ));
        }
    };

    let w = artifacts.worker();
    let metadata: Value = bindings::version_metadata(
        &target,
        &target_bindings,
        &w.main_module,
        &completion,
        &m,
        job,
    );
    let version_id = match api.create_version(&metadata, &w.modules).await {
        Ok(v) => v,
        Err(e) => {
            return Err(fail(
                &live,
                Stage::CreateVersion,
                "create_version_failed",
                api.clean(e.message()),
                e.retryable(),
                Activation::NotAttempted,
            ));
        }
    };

    let audit = |t: &Target,
                 result: AuditResult,
                 stage: Option<&'static str>,
                 code: Option<&'static str>,
                 published: Option<String>| AuditRow {
        workspace_id: t.workspace_id.clone(),
        site_id: t.site_id.clone(),
        target_id: t.id.clone(),
        actor_user_id: Some(job.requested_by),
        action: AuditAction::PublishActivate,
        result,
        cf_version_id: Some(version_id.clone()),
        published_id: published,
        revision_id: Some(job.revision_id.clone()),
        job_id: Some(job.job_id.clone()),
        stage,
        error_code: code,
        at: now_text(rt),
    };

    if let Err(f) = lock_and_activate(
        &api,
        rt,
        env.store,
        &mut target,
        &job.job_id,
        Some(job.revision_seq),
        &version_id,
    )
    .await
    {
        live = target.live_version_id.clone();
        if !f.stage.pre_activation() {
            let _ = env
                .store
                .audit(&audit(
                    &target,
                    AuditResult::Failed,
                    Some(f.stage.as_str()),
                    Some(f.code),
                    None,
                ))
                .await;
        }
        return Err(fail(
            &live,
            f.stage,
            f.code,
            api.clean(&f.message),
            f.retryable,
            f.activation,
        ));
    }

    let pid = published_id(&target.id, &version_id);
    let row = PublishedVersion {
        workspace_id: target.workspace_id.clone(),
        site_id: target.site_id.clone(),
        id: pid.clone(),
        target_id: target.id.clone(),
        cf_version_id: version_id.clone(),
        revision_id: job.revision_id.clone(),
        revision_seq: job.revision_seq,
        job_id: Some(job.job_id.clone()),
        schema_compat: artifacts.schema_level(),
        manifest_sha256: digest.clone(),
        verified_at: now_text(rt),
        created_by: job.requested_by,
    };
    let ok_row = audit(
        &target,
        AuditResult::Succeeded,
        Some(Stage::Activate.as_str()),
        None,
        Some(pid.clone()),
    );
    match env
        .store
        .record_publish(&target, &job.job_id, &row, &ok_row)
        .await
    {
        Ok(true) => {
            rt.log(&format!(
                "publish job={} revision={} activated version {version_id}",
                job.job_id, job.revision_id
            ));
            Ok(PublishOutcome {
                target_id: target.id.clone(),
                cf_version_id: version_id,
                published_id: pid,
                revision_id: job.revision_id.clone(),
                revision_seq: job.revision_seq,
                manifest_sha256: digest,
                uploaded_files: uploaded,
                total_files: m.entries.len(),
            })
        }
        Ok(false) => {
            let _ = env
                .store
                .audit(&audit(
                    &target,
                    AuditResult::Failed,
                    Some(Stage::Record.as_str()),
                    Some("lease_lost_after_activation"),
                    None,
                ))
                .await;
            Err(fail(
                &live,
                Stage::Record,
                "lease_lost_after_activation",
                "the version went live but the lease was lost before it was recorded".into(),
                false,
                Activation::Activated,
            ))
        }
        // Lease kept: the next holder reconciles with Cloudflare and adopts the version.
        Err(_) => Err(fail(
            &live,
            Stage::Record,
            "store_unavailable",
            "D1 is unavailable".into(),
            true,
            Activation::Activated,
        )),
    }
}
