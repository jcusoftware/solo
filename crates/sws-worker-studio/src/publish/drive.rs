//! The publish job driver (wasm only). The Cron Trigger, the optional Queue consumer
//! and the dev-build test hook all call [`drive_one`] (through [`run_due`] or
//! [`drive_pointer`]); nothing else advances a job.
//!
//! Every decision is the pure [`super::job`] state machine; this module performs the
//! D1 reads and the conditional writes from [`super::sql`]. A write that changes zero
//! rows means another driver owns or finished the job, and the driver stops. An error
//! inside a phase step is transient (retried with backoff); an error while writing a
//! transition leaves the lease in place, so the job is recovered once it expires.

use serde_json::{Value, json};
use sws_assets::limits::PUBLISH_ARTIFACT;
use worker::{D1Database, Date, Env, Result, console_error};

use super::job::{self, Claim, Phase, StepError};
use super::{QueuePointer, StoredArtifact, sql};
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

/// The `uploading` phase: the seam #66 fills (upload the validated files to
/// Cloudflare, verify each received file against its manifest SHA-256, record the
/// upload). Until then it performs no I/O and no Cloudflare call; reaching `ready`
/// means "validated and ready to publish", not "live".
async fn stage_for_activation(
    _db: &D1Database,
    _ws: &str,
    _job_id: &str,
) -> std::result::Result<(), StepError> {
    Ok(())
}

async fn run_step(
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
        Phase::Uploading => stage_for_activation(db, ws, job_id).await,
        Phase::Ready | Phase::Failed => Err(StepError::Permanent("illegal_phase")),
    }
}

/// Drive one job as far as it goes. `expected_revision` (from a queue pointer) must
/// match the row, or the job is treated as missing.
pub async fn drive_one(
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
        let result = run_step(db, ws, site, job_id, &revision_id, phase, opts).await;
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
        let mut entry = match drive_one(&db, ws, site, id, None, opts).await {
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
