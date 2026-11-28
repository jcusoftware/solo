//! SQL for publish jobs (`migrations/0004_publish_jobs.sql`), host-testable.
//!
//! Tenant-scoped statements bind `workspace_id` as `?1`. Every phase change is
//! guarded by `phase = ? AND attempt = ?` (the predicate [`super::job::guard_matches`]
//! models), plus `phase NOT IN ('ready', 'failed')`, so a duplicate or stale driver
//! changes zero rows. Two statements are deliberately cross-tenant: the recovery
//! sweep's due scan and the retention prune. They run only from the scheduled
//! handler (no HTTP route reaches them) and return or delete ids, never content.
//! `tests/unit/studio-publish-sql.test.mjs` runs every statement against SQLite.

use sws_assets::time::utc_text;

use super::job::{LEASE_SECS, MAX_ATTEMPTS, Phase, Transition};
use crate::repo::{Param, Statement};

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

fn ts(secs: u64) -> Param {
    Param::Text(utc_text(secs))
}

fn int(n: u64) -> Param {
    Param::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Enqueue a CLI job, only while the site has fewer than ?8 unfinished jobs and
/// fewer than ?10 jobs created after ?9 (a rolling day). A conflict (the same
/// request id again) or a refused cap inserts nothing; the caller reads the row back.
pub const PUBLISH_JOB_INSERT: &str = "INSERT INTO publish_jobs \
(workspace_id, site_id, id, revision_id, request_id, origin, next_attempt_at, created_by, created_at, updated_at) \
SELECT ?1, ?2, ?3, ?4, ?5, 'cli', ?6, ?7, ?6, ?6 \
WHERE (SELECT COUNT(*) FROM publish_jobs WHERE workspace_id = ?1 AND site_id = ?2 \
AND phase NOT IN ('ready', 'failed')) < ?8 \
AND (SELECT COUNT(*) FROM publish_jobs WHERE workspace_id = ?1 AND site_id = ?2 AND created_at > ?9) < ?10 \
ON CONFLICT DO NOTHING";

/// The job's artifact, in the same batch as [`PUBLISH_JOB_INSERT`]: written only when
/// the job row exists, never over an existing artifact (rows are immutable).
pub const PUBLISH_ARTIFACT_INSERT: &str = "INSERT INTO publish_artifacts \
(workspace_id, job_id, manifest_sha256, revision_sha256, compiler_name, compiler_version, \
file_count, total_bytes, manifest, created_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10 \
WHERE EXISTS (SELECT 1 FROM publish_jobs WHERE workspace_id = ?1 AND id = ?2) \
ON CONFLICT DO NOTHING";

/// One job, with its artifact identity (status route, enqueue read-back, driver).
pub const PUBLISH_JOB_GET: &str = "SELECT j.id, j.site_id, j.revision_id, j.request_id, j.origin, \
j.server_verified, j.phase, j.attempt, j.next_attempt_at, j.lease_expires_at, j.error_code, \
j.error_phase, j.last_error, j.created_by, j.created_at, j.updated_at, j.finished_at, \
a.manifest_sha256, a.revision_sha256, a.file_count, a.total_bytes \
FROM publish_jobs j LEFT JOIN publish_artifacts a ON a.workspace_id = j.workspace_id AND a.job_id = j.id \
WHERE j.workspace_id = ?1 AND j.site_id = ?2 AND j.id = ?3";

/// The stored canonical manifest (the `validating` phase re-checks it).
pub const PUBLISH_ARTIFACT_GET: &str = "SELECT manifest_sha256, revision_sha256, file_count, \
total_bytes, manifest FROM publish_artifacts WHERE workspace_id = ?1 AND job_id = ?2";

/// Claim: bump `attempt` (the new value is the driver's token) and take the lease,
/// re-checking at write time that the job is unfinished, due, unleased and below the
/// attempt cap. ?3 phase and ?4 attempt are the values the driver read.
pub const PUBLISH_JOB_CLAIM: &str = "UPDATE publish_jobs SET attempt = attempt + 1, \
lease_expires_at = ?6, updated_at = ?5 \
WHERE workspace_id = ?1 AND id = ?2 AND phase = ?3 AND attempt = ?4 \
AND phase NOT IN ('ready', 'failed') AND next_attempt_at <= ?5 \
AND (lease_expires_at IS NULL OR lease_expires_at <= ?5) AND attempt < ?7";

/// Forward edge ?3 -> ?5. Reaching `ready` stamps `finished_at` and drops the lease.
pub const PUBLISH_JOB_ADVANCE: &str = "UPDATE publish_jobs SET phase = ?5, lease_expires_at = ?6, \
updated_at = ?7, finished_at = CASE WHEN ?5 IN ('ready', 'failed') THEN ?7 ELSE NULL END \
WHERE workspace_id = ?1 AND id = ?2 AND phase = ?3 AND attempt = ?4 \
AND phase NOT IN ('ready', 'failed')";

/// Permanent failure in phase ?3 with code ?5 (?6: the transient cause, if any).
pub const PUBLISH_JOB_FAIL: &str = "UPDATE publish_jobs SET phase = 'failed', error_code = ?5, \
error_phase = ?3, last_error = COALESCE(?6, last_error), lease_expires_at = NULL, updated_at = ?7, \
finished_at = ?7 \
WHERE workspace_id = ?1 AND id = ?2 AND phase = ?3 AND attempt = ?4 \
AND phase NOT IN ('ready', 'failed')";

/// Transient failure: stay in phase ?3, release the lease, come back at ?5.
pub const PUBLISH_JOB_RETRY: &str = "UPDATE publish_jobs SET next_attempt_at = ?5, last_error = ?6, \
lease_expires_at = NULL, updated_at = ?7 \
WHERE workspace_id = ?1 AND id = ?2 AND phase = ?3 AND attempt = ?4 \
AND phase NOT IN ('ready', 'failed')";

/// The last allowed attempt never finished (a crash, or a lease lost mid-step): fail
/// instead of claiming again. Same write-time checks as the claim, but `attempt >= ?6`.
pub const PUBLISH_JOB_EXHAUST: &str = "UPDATE publish_jobs SET phase = 'failed', \
error_code = 'retries_exhausted', error_phase = ?3, last_error = 'lease_expired', \
lease_expires_at = NULL, updated_at = ?5, finished_at = ?5 \
WHERE workspace_id = ?1 AND id = ?2 AND phase = ?3 AND attempt = ?4 \
AND phase NOT IN ('ready', 'failed') AND next_attempt_at <= ?5 \
AND (lease_expires_at IS NULL OR lease_expires_at <= ?5) AND attempt >= ?6";

/// CROSS-TENANT (scheduled handler only): unfinished jobs that are due and unleased,
/// oldest first. Uses `idx_publish_jobs_due`.
pub const PUBLISH_JOBS_DUE: &str = "SELECT workspace_id, site_id, id FROM publish_jobs \
WHERE phase NOT IN ('ready', 'failed') AND next_attempt_at <= ?1 \
AND (lease_expires_at IS NULL OR lease_expires_at <= ?1) \
ORDER BY next_attempt_at, id LIMIT ?2";

/// CROSS-TENANT (scheduled handler only): delete finished jobs (and, by cascade,
/// their artifacts) that finished before ?1, at most ?2 per run, always keeping each
/// site's newest `ready` job (the one #66 would activate).
pub const PUBLISH_JOBS_PRUNE: &str = "DELETE FROM publish_jobs WHERE rowid IN (\
SELECT j.rowid FROM publish_jobs j WHERE j.phase IN ('ready', 'failed') AND j.finished_at < ?1 \
AND NOT (j.phase = 'ready' AND j.id = (SELECT k.id FROM publish_jobs k \
WHERE k.workspace_id = j.workspace_id AND k.site_id = j.site_id AND k.phase = 'ready' \
ORDER BY k.finished_at DESC, k.id DESC LIMIT 1)) \
ORDER BY j.finished_at LIMIT ?2)";

/// Every statement, for the SQL-level test (`dump_sql --publish`). The flag marks
/// tenant-scoped statements (`workspace_id = ?1`).
pub const ALL: &[(&str, &str, bool)] = &[
    ("PUBLISH_JOB_INSERT", PUBLISH_JOB_INSERT, true),
    ("PUBLISH_ARTIFACT_INSERT", PUBLISH_ARTIFACT_INSERT, true),
    ("PUBLISH_JOB_GET", PUBLISH_JOB_GET, true),
    ("PUBLISH_ARTIFACT_GET", PUBLISH_ARTIFACT_GET, true),
    ("PUBLISH_JOB_CLAIM", PUBLISH_JOB_CLAIM, true),
    ("PUBLISH_JOB_ADVANCE", PUBLISH_JOB_ADVANCE, true),
    ("PUBLISH_JOB_FAIL", PUBLISH_JOB_FAIL, true),
    ("PUBLISH_JOB_RETRY", PUBLISH_JOB_RETRY, true),
    ("PUBLISH_JOB_EXHAUST", PUBLISH_JOB_EXHAUST, true),
    ("PUBLISH_JOBS_DUE", PUBLISH_JOBS_DUE, false),
    ("PUBLISH_JOBS_PRUNE", PUBLISH_JOBS_PRUNE, false),
];

/// Enqueue caps (provisional): unfinished jobs per site, and jobs per site per day.
/// With the retention below they bound `publish_jobs` to
/// `MAX_JOBS_PER_SITE_PER_DAY * JOB_RETENTION_SECS / 86400` rows per site.
pub const MAX_ACTIVE_JOBS_PER_SITE: u64 = 3;
pub const MAX_JOBS_PER_SITE_PER_DAY: u64 = 50;
/// Finished jobs older than this are pruned (except each site's newest `ready` job).
pub const JOB_RETENTION_SECS: u64 = 30 * 86_400;
/// Rows the prune deletes per scheduled run.
pub const PRUNE_BATCH: u64 = 50;

/// Input of [`job_insert`] / [`artifact_insert`].
pub struct NewJob<'a> {
    pub workspace_id: &'a str,
    pub site_id: &'a str,
    pub job_id: &'a str,
    pub revision_id: &'a str,
    pub request_id: &'a str,
    pub created_by: i64,
    pub now: u64,
}

pub fn job_insert(j: &NewJob) -> Statement {
    Statement {
        sql: PUBLISH_JOB_INSERT,
        params: vec![
            t(j.workspace_id),
            t(j.site_id),
            t(j.job_id),
            t(j.revision_id),
            t(j.request_id),
            ts(j.now),
            Param::Int(j.created_by),
            int(MAX_ACTIVE_JOBS_PER_SITE),
            ts(j.now.saturating_sub(86_400)),
            int(MAX_JOBS_PER_SITE_PER_DAY),
        ],
    }
}

pub fn artifact_insert(j: &NewJob, m: &super::manifest::ValidManifest) -> Statement {
    Statement {
        sql: PUBLISH_ARTIFACT_INSERT,
        params: vec![
            t(j.workspace_id),
            t(j.job_id),
            t(&m.digest),
            t(&m.manifest.revision_sha256),
            t(&m.manifest.compiler.name),
            t(&m.manifest.compiler.version),
            int(m.manifest.file_count),
            int(m.manifest.total_bytes),
            t(&m.canonical),
            ts(j.now),
        ],
    }
}

pub fn job_get(workspace_id: &str, site_id: &str, job_id: &str) -> Statement {
    Statement {
        sql: PUBLISH_JOB_GET,
        params: vec![t(workspace_id), t(site_id), t(job_id)],
    }
}

pub fn artifact_get(workspace_id: &str, job_id: &str) -> Statement {
    Statement {
        sql: PUBLISH_ARTIFACT_GET,
        params: vec![t(workspace_id), t(job_id)],
    }
}

pub fn claim(workspace_id: &str, job_id: &str, phase: Phase, attempt: u32, now: u64) -> Statement {
    Statement {
        sql: PUBLISH_JOB_CLAIM,
        params: vec![
            t(workspace_id),
            t(job_id),
            t(phase.as_str()),
            Param::Int(attempt.into()),
            ts(now),
            ts(now.saturating_add(LEASE_SECS)),
            Param::Int(MAX_ATTEMPTS.into()),
        ],
    }
}

/// The conditional write for `tr`, guarded by the owning `attempt`.
pub fn transition(
    workspace_id: &str,
    job_id: &str,
    tr: &Transition,
    attempt: u32,
    now: u64,
) -> Statement {
    let head = |sql| {
        (
            sql,
            vec![
                t(workspace_id),
                t(job_id),
                t(tr.from().as_str()),
                Param::Int(attempt.into()),
            ],
        )
    };
    let (sql, mut params) = match *tr {
        Transition::Advance { .. } => head(PUBLISH_JOB_ADVANCE),
        Transition::Fail { .. } => head(PUBLISH_JOB_FAIL),
        Transition::Retry { .. } => head(PUBLISH_JOB_RETRY),
    };
    match *tr {
        Transition::Advance {
            to,
            lease_expires_at,
            ..
        } => {
            params.push(t(to.as_str()));
            params.push(lease_expires_at.map_or(Param::Null, ts));
        }
        Transition::Fail {
            code, last_error, ..
        } => {
            params.push(t(code));
            params.push(last_error.map_or(Param::Null, t));
        }
        Transition::Retry {
            last_error,
            next_attempt_at,
            ..
        } => {
            params.push(ts(next_attempt_at));
            params.push(t(last_error));
        }
    }
    params.push(ts(now));
    Statement { sql, params }
}

pub fn exhaust(
    workspace_id: &str,
    job_id: &str,
    phase: Phase,
    attempt: u32,
    now: u64,
) -> Statement {
    Statement {
        sql: PUBLISH_JOB_EXHAUST,
        params: vec![
            t(workspace_id),
            t(job_id),
            t(phase.as_str()),
            Param::Int(attempt.into()),
            ts(now),
            Param::Int(MAX_ATTEMPTS.into()),
        ],
    }
}

pub fn jobs_due(now: u64, max: u32) -> Statement {
    Statement {
        sql: PUBLISH_JOBS_DUE,
        params: vec![ts(now), Param::Int(max.into())],
    }
}

pub fn jobs_prune(now: u64) -> Statement {
    Statement {
        sql: PUBLISH_JOBS_PRUNE,
        params: vec![ts(now.saturating_sub(JOB_RETENTION_SECS)), int(PRUNE_BATCH)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::job::{Origin, StepError, on_step};

    #[test]
    fn tenant_statements_filter_on_workspace_first() {
        for (name, sql, tenant) in ALL {
            if *tenant {
                assert!(sql.contains("workspace_id = ?1"), "{name}");
            } else {
                assert!(
                    !sql.contains("workspace_id = ?1"),
                    "{name} is documented as cross-tenant"
                );
            }
        }
    }

    #[test]
    fn every_phase_change_is_guarded_by_phase_and_attempt() {
        for sql in [
            PUBLISH_JOB_CLAIM,
            PUBLISH_JOB_ADVANCE,
            PUBLISH_JOB_FAIL,
            PUBLISH_JOB_RETRY,
            PUBLISH_JOB_EXHAUST,
        ] {
            assert!(sql.starts_with("UPDATE publish_jobs"), "{sql}");
            assert!(sql.contains("AND phase = ?3 AND attempt = ?4"), "{sql}");
            assert!(sql.contains("phase NOT IN ('ready', 'failed')"), "{sql}");
        }
        // Nothing updates an artifact or a revision.
        for (name, sql, _) in ALL {
            assert!(!sql.contains("UPDATE publish_artifacts"), "{name}");
            assert!(!sql.contains("UPDATE site_revisions"), "{name}");
        }
    }

    #[test]
    fn nothing_can_mark_a_job_server_verified() {
        for (name, sql, _) in ALL {
            assert!(!sql.contains("server_verified ="), "{name}");
            assert!(!sql.contains("server_verified)"), "{name}");
        }
        assert!(PUBLISH_JOB_INSERT.contains("'cli'"));
    }

    #[test]
    fn transition_params_line_up_with_the_placeholders() {
        let now = 1_795_737_600;
        let cases = [
            on_step(Phase::Validating, Origin::Cli, 1, Ok(()), now),
            on_step(Phase::Uploading, Origin::Cli, 1, Ok(()), now),
            on_step(
                Phase::Validating,
                Origin::Cli,
                1,
                Err(StepError::Permanent("revision_missing")),
                now,
            ),
            on_step(
                Phase::Validating,
                Origin::Cli,
                1,
                Err(StepError::Transient("d1_unavailable")),
                now,
            ),
            on_step(
                Phase::Validating,
                Origin::Cli,
                MAX_ATTEMPTS,
                Err(StepError::Transient("d1_unavailable")),
                now,
            ),
        ];
        for tr in cases {
            let s = transition("ws", "j", &tr, 1, now);
            assert_eq!(s.params.len(), 7, "{tr:?}");
            assert!(s.sql.contains("?7") && !s.sql.contains("?8"), "{tr:?}");
            assert_eq!(s.params[0], Param::Text("ws".into()));
            assert_eq!(s.params[2], Param::Text(tr.from().as_str().into()));
            assert_eq!(s.params[3], Param::Int(1));
        }
        assert_eq!(claim("ws", "j", Phase::Queued, 0, now).params.len(), 7);
        assert_eq!(exhaust("ws", "j", Phase::Queued, 5, now).params.len(), 6);
        let j = NewJob {
            workspace_id: "ws",
            site_id: "s",
            job_id: "j",
            revision_id: "r",
            request_id: "q",
            created_by: 1,
            now,
        };
        assert_eq!(job_insert(&j).params.len(), 10);
    }

    #[test]
    fn retained_rows_are_bounded() {
        const {
            assert!(MAX_ACTIVE_JOBS_PER_SITE <= MAX_JOBS_PER_SITE_PER_DAY);
            assert!(MAX_JOBS_PER_SITE_PER_DAY * (JOB_RETENTION_SECS / 86_400) <= 2_000);
        }
    }
}
