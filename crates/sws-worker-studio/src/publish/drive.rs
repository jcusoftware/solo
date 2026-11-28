//! The publish job driver (wasm only). The Cron Trigger, the optional Queue consumer
//! and the dev-build test hook all call [`drive_one`] (through [`run_due`] or
//! [`drive_pointer`]); nothing else advances a job.
//!
//! Every decision is the pure [`super::job`] state machine; this module performs the
//! D1 reads and the conditional writes from [`super::sql`]. A write that changes zero
//! rows means another driver owns or finished the job, and the driver stops. An error
//! inside a phase step is transient (retried with backoff); an error while writing a
//! transition leaves the lease in place, so the job is recovered once it expires.

use serde::Deserialize;
use serde_json::{Value, json};
use sws_assets::limits::PUBLISH_ARTIFACT;
use worker::{D1Database, Date, Env, Result, console_error};

use super::bundle::{self, BundleArtifactSource};
use super::job::{self, Claim, Phase, StepError};
use super::{QueuePointer, StoredArtifact, sql};
use crate::auth::policy::{self, Permission};
use crate::deploy::store::{AuditAction, AuditResult, AuditRow, PublishStore};
use crate::deploy::wasm::{D1Store, EnvSecrets, FetchClient, WorkerRuntime, api_base};
use crate::deploy::{PublishEnv, PublishJobRef, Stage};
use crate::{d1, repo};

/// Test-only fault injection. Populated solely by the `insecure-dev-auth` build's
/// `/api/dev/publish-drive` route (behind the runtime fence); always empty otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Faults {
    /// Stop dead right after the transition out of this phase is written, as if the
    /// isolate died: no further write, the lease stays behind.
    pub crash_after: Option<Phase>,
    /// Make this phase's step fail transiently.
    pub transient_in: Option<Phase>,
}

#[derive(Debug, Clone, Copy)]
pub struct DriveOptions {
    /// Unix seconds.
    pub now: u64,
    pub max_jobs: u32,
    pub server_compile: bool,
    pub faults: Faults,
}

impl DriveOptions {
    /// Production options: the real clock and the configured flags.
    pub fn from_env(env: &Env) -> Self {
        Self {
            now: real_now(),
            max_jobs: super::MAX_JOBS_PER_RUN,
            server_compile: super::server_compile_enabled(
                sws_security::worker_utils::var(env, super::SERVER_COMPILE_VAR).as_deref(),
            ),
            faults: Faults::default(),
        }
    }
}

pub fn real_now() -> u64 {
    Date::now().as_millis() / 1000
}

/// What happened to one job in one drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No such job in that workspace/site (or the pointer named another revision).
    Missing,
    Terminal(Phase),
    Leased,
    NotDue,
    /// A conditional write matched nothing: a duplicate delivery or a racing driver.
    Lost,
    /// Stopped in this phase (ready, failed, or backing off).
    Stopped(Phase),
    /// Test fault: stopped dead after reaching this phase.
    Crashed(Phase),
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        let (kind, phase) = match self {
            Outcome::Missing => ("missing", None),
            Outcome::Terminal(p) => ("terminal", Some(*p)),
            Outcome::Leased => ("leased", None),
            Outcome::NotDue => ("not_due", None),
            Outcome::Lost => ("lost", None),
            Outcome::Stopped(p) => ("stopped", Some(*p)),
            Outcome::Crashed(p) => ("crashed", Some(*p)),
        };
        json!({ "outcome": kind, "phase": phase.map(Phase::as_str) })
    }
}

#[derive(Deserialize)]
struct BundleChunkRow {
    data: Vec<u8>,
}

/// Read and re-verify the uploaded bundle (if present), and if `publish_targets`
/// exists AND the job was enqueued with `"activate": true`, perform the
/// activation-time D1 authorization recheck and call `deploy::run_publish`.
/// When neither a `publish_targets` row nor a bundle exists (WP-04 manifest-only
/// staging tests), succeeds with `bytes_verified: false`.
async fn stage_for_activation(
    env: &Env,
    db: &D1Database,
    ws: &str,
    site: &str,
    job_id: &str,
    revision_id: &str,
    now: u64,
) -> std::result::Result<(), StepError> {
    let unavailable = |_| StepError::Transient("d1_unavailable");
    let act_row = d1::first(db, &crate::deploy::sql::activation_get(ws, job_id))
        .await
        .map_err(unavailable)?;
    let auto_activate = act_row
        .as_ref()
        .and_then(|r| r.get("auto_activate"))
        .and_then(Value::as_i64)
        == Some(1);
    let cred_tag = act_row
        .as_ref()
        .and_then(|r| r.get("credential_tag"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let target = d1::first(db, &crate::deploy::sql::target_get(ws, site))
        .await
        .map_err(unavailable)?;

    let artifact_row = d1::first(db, &sql::artifact_get(ws, job_id))
        .await
        .map_err(unavailable)?;
    let Some(stored) = artifact_row.as_ref().and_then(StoredArtifact::from_row) else {
        if target.is_some() || auto_activate {
            return Err(StepError::Permanent("artifact_missing"));
        }
        return Ok(());
    };

    let bundle_row = d1::first(
        db,
        &crate::deploy::sql::bundle_get(ws, site, &stored.manifest_sha256),
    )
    .await
    .map_err(unavailable)?;

    let Some(bundle_row) = bundle_row else {
        if target.is_some() && auto_activate {
            return Err(StepError::Permanent("bundle_missing"));
        }
        return Ok(());
    };

    let total_bytes = bundle_row
        .get("total_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let chunk_rows = d1::all(
        db,
        &crate::deploy::sql::bundle_chunks_get(ws, site, &stored.manifest_sha256),
    )
    .await
    .map_err(unavailable)?;
    let mut raw = Vec::with_capacity(total_bytes);
    for row in chunk_rows {
        let chunk: BundleChunkRow =
            serde_json::from_value(row).map_err(|_| StepError::Permanent("bundle_malformed"))?;
        raw.extend_from_slice(&chunk.data);
    }

    let verified = bundle::verify_against_stored(&raw, &stored, &PUBLISH_ARTIFACT)
        .map_err(StepError::Permanent)?;
    drop(raw);

    d1::run(
        db,
        &crate::deploy::sql::activation_set_verified(ws, site, job_id),
    )
    .await
    .map_err(unavailable)?;

    let Some(target_val) = target else {
        return Ok(());
    };
    if !auto_activate {
        return Ok(());
    }

    let target_id = target_val
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let job_row = d1::first(db, &sql::job_get(ws, site, job_id))
        .await
        .map_err(unavailable)?
        .ok_or(StepError::Permanent("job_missing"))?;
    let created_by = job_row
        .get("created_by")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let rev_row = d1::first(db, &repo::revision_get(ws, site, revision_id))
        .await
        .map_err(unavailable)?
        .ok_or(StepError::Permanent("revision_missing"))?;
    let revision_seq = rev_row
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or(StepError::Permanent("revision_missing"))?;

    let store = D1Store {
        db: d1::database(env).map_err(unavailable)?,
    };

    // Activation-time authorization recheck against current D1 membership & credential state.
    let role = crate::auth_http::role_in(db, ws, created_by)
        .await
        .map_err(unavailable)?;
    let cred = crate::auth_http::revalidate(db, ws, created_by, cred_tag.as_deref(), now as i64)
        .await
        .map_err(unavailable)?;
    let authorized = cred
        .as_ref()
        .is_some_and(|c| policy::authorize(c, role, ws, Permission::Activate).is_ok());
    if !authorized {
        let _ = store
            .audit(&AuditRow {
                workspace_id: ws.to_string(),
                site_id: site.to_string(),
                target_id,
                actor_user_id: (created_by > 0).then_some(created_by),
                action: AuditAction::PublishActivate,
                result: AuditResult::Refused,
                cf_version_id: None,
                published_id: None,
                revision_id: Some(revision_id.to_string()),
                job_id: Some(job_id.to_string()),
                stage: Some(Stage::Prepare.as_str()),
                error_code: Some("activation_unauthorized"),
                at: super::now_text(now),
            })
            .await;
        return Err(StepError::Permanent("activation_unauthorized"));
    }

    let base = api_base(None, env);
    let secrets = EnvSecrets(env);
    let publish_env = PublishEnv {
        http: &FetchClient,
        rt: &WorkerRuntime,
        store: &store,
        secrets: &secrets,
        api_base: &base,
    };
    let job_ref = PublishJobRef {
        workspace_id: ws.to_string(),
        site_id: site.to_string(),
        job_id: job_id.to_string(),
        revision_id: revision_id.to_string(),
        revision_seq,
        requested_by: created_by,
        artifact_sha256: None,
    };
    let source = BundleArtifactSource::new(verified);
    match crate::deploy::run_publish(&job_ref, &source, &publish_env).await {
        Ok(out) => {
            d1::run(
                db,
                &crate::deploy::sql::activation_set_deployed(
                    ws,
                    site,
                    job_id,
                    &out.published_id,
                    &out.cf_version_id,
                ),
            )
            .await
            .map_err(unavailable)?;
            Ok(())
        }
        Err(f) => {
            if f.retryable {
                Err(StepError::Transient(f.code))
            } else {
                Err(StepError::Permanent(f.code))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_step(
    env: &Env,
    db: &D1Database,
    ws: &str,
    site: &str,
    job_id: &str,
    revision_id: &str,
    phase: Phase,
    opts: &DriveOptions,
) -> std::result::Result<(), StepError> {
    if opts.faults.transient_in == Some(phase) {
        return Err(StepError::Transient("injected_fault"));
    }
    match phase {
        Phase::Queued => Ok(()),
        Phase::Compiling => super::compile_step(opts.server_compile),
        Phase::Validating => {
            let unavailable = |_| StepError::Transient("d1_unavailable");
            let artifact = d1::first(db, &sql::artifact_get(ws, job_id))
                .await
                .map_err(unavailable)?;
            let revision = d1::first(db, &repo::revision_get(ws, site, revision_id))
                .await
                .map_err(unavailable)?;
            let ir = revision
                .as_ref()
                .and_then(|r| r.get("ir_sha256"))
                .and_then(Value::as_str);
            let stored = artifact.as_ref().and_then(StoredArtifact::from_row);
            super::validate_step(revision_id, ir, stored.as_ref(), &PUBLISH_ARTIFACT)
        }
        Phase::Uploading => {
            stage_for_activation(env, db, ws, site, job_id, revision_id, opts.now).await
        }
        Phase::Ready | Phase::Failed => Err(StepError::Permanent("illegal_phase")),
    }
}

/// Drive one job as far as it goes. `expected_revision` (from a queue pointer) must
/// match the row, or the job is treated as missing.
pub async fn drive_one(
    env: &Env,
    db: &D1Database,
    ws: &str,
    site: &str,
    job_id: &str,
    expected_revision: Option<&str>,
    opts: &DriveOptions,
) -> Result<Outcome> {
    let now = opts.now;
    let Some(row) = d1::first(db, &sql::job_get(ws, site, job_id)).await? else {
        return Ok(Outcome::Missing);
    };
    let revision_id = row
        .get("revision_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if expected_revision.is_some_and(|r| r != revision_id) {
        return Ok(Outcome::Missing);
    }
    let Some(state) = super::state_from_row(&row) else {
        console_error!("publish job {ws}/{job_id}: unreadable row, left alone");
        return Ok(Outcome::Missing);
    };
    let attempt = match job::plan_claim(&state, now) {
        Claim::Terminal => return Ok(Outcome::Terminal(state.phase)),
        Claim::Leased => return Ok(Outcome::Leased),
        Claim::NotDue => return Ok(Outcome::NotDue),
        Claim::Exhausted => {
            let n = d1::run(
                db,
                &sql::exhaust(ws, job_id, state.phase, state.attempt, now),
            )
            .await?;
            return Ok(if n == 1 {
                Outcome::Stopped(Phase::Failed)
            } else {
                Outcome::Lost
            });
        }
        Claim::Claim { attempt } => attempt,
    };
    if d1::run(db, &sql::claim(ws, job_id, state.phase, state.attempt, now)).await? != 1 {
        return Ok(Outcome::Lost);
    }
    let mut phase = state.phase;
    loop {
        let result = run_step(env, db, ws, site, job_id, &revision_id, phase, opts).await;
        let tr = job::on_step(phase, state.origin, attempt, result, now);
        if d1::run(db, &sql::transition(ws, job_id, &tr, attempt, now)).await? != 1 {
            return Ok(Outcome::Lost);
        }
        if opts.faults.crash_after == Some(phase) {
            return Ok(Outcome::Crashed(tr.to()));
        }
        if !tr.continues() {
            return Ok(Outcome::Stopped(tr.to()));
        }
        phase = tr.to();
    }
}

/// Queue consumer entry: the pointer names the job; the row is the truth.
pub async fn drive_pointer(env: &Env, p: &QueuePointer, opts: &DriveOptions) -> Result<Outcome> {
    let db = d1::database(env)?;
    drive_one(
        env,
        &db,
        &p.workspace_id,
        &p.site_id,
        &p.job_id,
        Some(&p.revision_id),
        opts,
    )
    .await
}

/// Scheduled entry (Cron Trigger, and the recovery path when there is no Queue):
/// drive up to `opts.max_jobs` due jobs, oldest first, then prune old finished jobs.
/// One job's error is logged and does not stop the others.
pub async fn run_due(env: &Env, opts: &DriveOptions) -> Result<Value> {
    let db = d1::database(env)?;
    let due = d1::all(&db, &sql::jobs_due(opts.now, opts.max_jobs)).await?;
    let mut jobs = Vec::with_capacity(due.len());
    for row in due.iter().take(opts.max_jobs as usize) {
        let s = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or_default();
        let (ws, site, id) = (s("workspace_id"), s("site_id"), s("id"));
        let mut entry = match drive_one(env, &db, ws, site, id, None, opts).await {
            Ok(o) => o.to_json(),
            Err(e) => {
                console_error!("publish job {ws}/{id}: {e}");
                json!({ "outcome": "error" })
            }
        };
        if let Some(o) = entry.as_object_mut() {
            o.insert("job_id".into(), json!(id));
        }
        jobs.push(entry);
    }
    let pruned = match d1::run(&db, &sql::jobs_prune(opts.now)).await {
        Ok(n) => n,
        Err(e) => {
            console_error!("publish job prune: {e}");
            0
        }
    };
    Ok(json!({ "jobs": jobs, "pruned": pruned }))
}
