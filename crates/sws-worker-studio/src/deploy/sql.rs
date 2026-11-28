//! D1 statements for publish targets, the activation lease, retained versions and the
//! audit log (`migrations/0005_publish_targets.sql`). Host-testable builders; every
//! statement is tenant-scoped with `workspace_id` bound as `?1`, and all values are
//! bound parameters. Printed by `cargo run --example dump_sql -- --publish` for the
//! node:sqlite test `tests/unit/studio-publish-sql.test.mjs`.
//!
//! None of these statements writes `site_revisions`, `deployments` (the #64 job table)
//! or any session state: publishing and rollback never rewind editable IR.

use crate::repo::{Param, Statement};

use super::store::{AuditRow, LeaseClaim, PublishedVersion, Target};

pub const TARGET_INSERT: &str = "INSERT INTO publish_targets \
(workspace_id, site_id, id, account_id, script_name, token_secret, bindings_json, compatibility_date, \
site_schema_level, live_version_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

pub const TARGET_GET: &str = "SELECT workspace_id, site_id, id, account_id, script_name, token_secret, \
bindings_json, compatibility_date, site_schema_level, live_version_id, live_published_id, \
live_revision_id, high_water_seq, activating_job, activating_seq, activating_version_id, lease_expires_at \
FROM publish_targets WHERE workspace_id = ?1 AND site_id = ?2";

/// Run by whoever applies the site D1 migrations (operator / CLI, #68).
pub const TARGET_SET_SCHEMA_LEVEL: &str = "UPDATE publish_targets SET site_schema_level = ?3, \
updated_at = ?4 WHERE workspace_id = ?1 AND id = ?2";

/// Take a FREE lease; a publish claim (?4 not NULL) also needs ?4 above the high-water mark.
pub const TARGET_ACQUIRE: &str = "UPDATE publish_targets SET activating_job = ?3, activating_seq = ?4, \
activating_version_id = ?5, lease_expires_at = ?6, updated_at = ?7 \
WHERE workspace_id = ?1 AND id = ?2 AND activating_job IS NULL AND (?4 IS NULL OR high_water_seq < ?4)";

pub const TARGET_RENEW: &str = "UPDATE publish_targets SET lease_expires_at = ?4, updated_at = ?5 \
WHERE workspace_id = ?1 AND id = ?2 AND activating_job = ?3 AND lease_expires_at > ?5";

pub const TARGET_RELEASE: &str = "UPDATE publish_targets SET activating_job = NULL, activating_seq = NULL, \
activating_version_id = NULL, lease_expires_at = NULL \
WHERE workspace_id = ?1 AND id = ?2 AND activating_job = ?3";

pub const TARGET_CLEAR_STALE: &str = "UPDATE publish_targets SET activating_job = NULL, activating_seq = NULL, \
activating_version_id = NULL, lease_expires_at = NULL, updated_at = ?4 \
WHERE workspace_id = ?1 AND id = ?2 AND activating_job = ?3 AND lease_expires_at <= ?4";

/// Reconciliation proved the expired holder's version is serving: record it (SQLite
/// evaluates every right-hand side against the row BEFORE the update).
pub const TARGET_ADOPT_STALE: &str = "UPDATE publish_targets SET live_version_id = activating_version_id, \
live_published_id = NULL, live_revision_id = NULL, \
high_water_seq = MAX(high_water_seq, COALESCE(activating_seq, 0)), \
activating_job = NULL, activating_seq = NULL, activating_version_id = NULL, lease_expires_at = NULL, \
updated_at = ?4 WHERE workspace_id = ?1 AND id = ?2 AND activating_job = ?3 AND lease_expires_at <= ?4";

pub const TARGET_RECORD_PUBLISH: &str = "UPDATE publish_targets SET live_version_id = ?4, \
live_published_id = ?5, live_revision_id = ?6, high_water_seq = ?7, activating_job = NULL, \
activating_seq = NULL, activating_version_id = NULL, lease_expires_at = NULL, updated_at = ?8 \
WHERE workspace_id = ?1 AND id = ?2 AND activating_job = ?3 AND high_water_seq < ?7";

/// Inserted in the same batch, AFTER [`TARGET_RECORD_PUBLISH`], and only if that update
/// applied (the target now points at this row): a lost lease leaves no retained row.
pub const VERSION_INSERT_IF_LIVE: &str = "INSERT INTO published_versions \
(workspace_id, site_id, id, target_id, cf_version_id, revision_id, revision_seq, job_id, \
schema_compat, manifest_sha256, verified_at, created_by) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12 \
WHERE EXISTS (SELECT 1 FROM publish_targets WHERE workspace_id = ?1 AND id = ?4 AND live_published_id = ?3) \
ON CONFLICT (id) DO NOTHING";

/// Rollback: point the target at a retained row of THIS target; high-water mark unchanged.
pub const TARGET_RECORD_ROLLBACK: &str = "UPDATE publish_targets SET live_version_id = ?4, \
live_published_id = ?5, live_revision_id = ?6, activating_job = NULL, activating_seq = NULL, \
activating_version_id = NULL, lease_expires_at = NULL, updated_at = ?7 \
WHERE workspace_id = ?1 AND id = ?2 AND activating_job = ?3 AND EXISTS (SELECT 1 FROM published_versions \
WHERE workspace_id = ?1 AND target_id = ?2 AND id = ?5 AND cf_version_id = ?4)";

pub const VERSION_GET: &str = "SELECT workspace_id, site_id, id, target_id, cf_version_id, revision_id, \
revision_seq, job_id, schema_compat, manifest_sha256, verified_at, created_by \
FROM published_versions WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3";

pub const VERSION_LIST: &str = "SELECT workspace_id, site_id, id, target_id, cf_version_id, revision_id, \
revision_seq, job_id, schema_compat, manifest_sha256, verified_at, created_by \
FROM published_versions WHERE workspace_id = ?1 AND site_id = ?2 ORDER BY revision_seq DESC, id LIMIT ?3";

pub const AUDIT_INSERT: &str = "INSERT INTO publish_audit \
(workspace_id, site_id, target_id, actor_user_id, action, result, cf_version_id, published_id, \
revision_id, job_id, stage, error_code, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";

pub const AUDIT_LIST: &str = "SELECT id, actor_user_id, action, result, cf_version_id, published_id, \
revision_id, job_id, stage, error_code, at FROM publish_audit \
WHERE workspace_id = ?1 AND site_id = ?2 ORDER BY id DESC LIMIT ?3";

pub const BUNDLE_GET: &str = "SELECT workspace_id, site_id, manifest_sha256, total_bytes, \
chunk_count, created_by, created_at FROM publish_bundles \
WHERE workspace_id = ?1 AND site_id = ?2 AND manifest_sha256 = ?3";

pub const BUNDLE_INSERT: &str = "INSERT INTO publish_bundles \
(workspace_id, site_id, manifest_sha256, total_bytes, chunk_count, created_by, created_at) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING";

pub const BUNDLE_CHUNK_INSERT: &str = "INSERT INTO publish_bundle_chunks \
(workspace_id, site_id, manifest_sha256, seq, data) \
SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM publish_bundles \
WHERE workspace_id = ?1 AND site_id = ?2 AND manifest_sha256 = ?3) ON CONFLICT DO NOTHING";

pub const BUNDLE_CHUNKS_GET: &str = "SELECT seq, data FROM publish_bundle_chunks \
WHERE workspace_id = ?1 AND site_id = ?2 AND manifest_sha256 = ?3 ORDER BY seq";

pub const ACTIVATION_UPSERT: &str = "INSERT INTO publish_activations \
(workspace_id, site_id, job_id, auto_activate, credential_tag, bytes_verified) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM publish_jobs \
WHERE workspace_id = ?1 AND id = ?3) ON CONFLICT (workspace_id, job_id) DO NOTHING";

pub const ACTIVATION_SET_VERIFIED: &str = "INSERT INTO publish_activations \
(workspace_id, site_id, job_id, bytes_verified) VALUES (?1, ?2, ?3, 1) \
ON CONFLICT (workspace_id, job_id) DO UPDATE SET bytes_verified = 1";

pub const ACTIVATION_SET_DEPLOYED: &str = "INSERT INTO publish_activations \
(workspace_id, site_id, job_id, bytes_verified, published_id, cf_version_id) \
VALUES (?1, ?2, ?3, 1, ?4, ?5) ON CONFLICT (workspace_id, job_id) DO UPDATE SET \
bytes_verified = 1, published_id = excluded.published_id, cf_version_id = excluded.cf_version_id";

pub const ACTIVATION_GET: &str = "SELECT workspace_id, site_id, job_id, auto_activate, \
credential_tag, bytes_verified, published_id, cf_version_id FROM publish_activations \
WHERE workspace_id = ?1 AND job_id = ?2";

/// Every statement (name, sql, tenant-scoped) for the SQL-level test.
pub const ALL: &[(&str, &str, bool)] = &[
    ("TARGET_INSERT", TARGET_INSERT, true),
    ("TARGET_GET", TARGET_GET, true),
    ("TARGET_SET_SCHEMA_LEVEL", TARGET_SET_SCHEMA_LEVEL, true),
    ("TARGET_ACQUIRE", TARGET_ACQUIRE, true),
    ("TARGET_RENEW", TARGET_RENEW, true),
    ("TARGET_RELEASE", TARGET_RELEASE, true),
    ("TARGET_CLEAR_STALE", TARGET_CLEAR_STALE, true),
    ("TARGET_ADOPT_STALE", TARGET_ADOPT_STALE, true),
    ("TARGET_RECORD_PUBLISH", TARGET_RECORD_PUBLISH, true),
    ("VERSION_INSERT_IF_LIVE", VERSION_INSERT_IF_LIVE, true),
    ("TARGET_RECORD_ROLLBACK", TARGET_RECORD_ROLLBACK, true),
    ("VERSION_GET", VERSION_GET, true),
    ("VERSION_LIST", VERSION_LIST, true),
    ("AUDIT_INSERT", AUDIT_INSERT, true),
    ("AUDIT_LIST", AUDIT_LIST, true),
    ("BUNDLE_GET", BUNDLE_GET, true),
    ("BUNDLE_INSERT", BUNDLE_INSERT, true),
    ("BUNDLE_CHUNK_INSERT", BUNDLE_CHUNK_INSERT, true),
    ("BUNDLE_CHUNKS_GET", BUNDLE_CHUNKS_GET, true),
    ("ACTIVATION_UPSERT", ACTIVATION_UPSERT, true),
    ("ACTIVATION_SET_VERIFIED", ACTIVATION_SET_VERIFIED, true),
    ("ACTIVATION_SET_DEPLOYED", ACTIVATION_SET_DEPLOYED, true),
    ("ACTIVATION_GET", ACTIVATION_GET, true),
];

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

fn opt(s: Option<&str>) -> Param {
    s.map_or(Param::Null, t)
}

fn opt_int(i: Option<i64>) -> Param {
    i.map_or(Param::Null, Param::Int)
}

fn st(sql: &'static str, params: Vec<Param>) -> Statement {
    Statement { sql, params }
}

pub fn target_get(ws: &str, site: &str) -> Statement {
    st(TARGET_GET, vec![t(ws), t(site)])
}

pub fn acquire(tg: &Target, c: &LeaseClaim) -> Statement {
    st(
        TARGET_ACQUIRE,
        vec![
            t(&tg.workspace_id),
            t(&tg.id),
            t(&c.job),
            opt_int(c.seq),
            t(&c.version_id),
            t(&c.until),
            t(&c.now),
        ],
    )
}

pub fn renew(tg: &Target, job: &str, until: &str, now: &str) -> Statement {
    st(
        TARGET_RENEW,
        vec![t(&tg.workspace_id), t(&tg.id), t(job), t(until), t(now)],
    )
}

pub fn release(tg: &Target, job: &str) -> Statement {
    st(TARGET_RELEASE, vec![t(&tg.workspace_id), t(&tg.id), t(job)])
}

pub fn clear_stale(tg: &Target, stale_job: &str, now: &str, adopt: bool) -> Statement {
    let sql = if adopt {
        TARGET_ADOPT_STALE
    } else {
        TARGET_CLEAR_STALE
    };
    st(
        sql,
        vec![t(&tg.workspace_id), t(&tg.id), t(stale_job), t(now)],
    )
}

pub fn record_publish(tg: &Target, job: &str, v: &PublishedVersion, now: &str) -> Statement {
    st(
        TARGET_RECORD_PUBLISH,
        vec![
            t(&tg.workspace_id),
            t(&tg.id),
            t(job),
            t(&v.cf_version_id),
            t(&v.id),
            t(&v.revision_id),
            Param::Int(v.revision_seq),
            t(now),
        ],
    )
}

pub fn version_insert_if_live(v: &PublishedVersion) -> Statement {
    st(
        VERSION_INSERT_IF_LIVE,
        vec![
            t(&v.workspace_id),
            t(&v.site_id),
            t(&v.id),
            t(&v.target_id),
            t(&v.cf_version_id),
            t(&v.revision_id),
            Param::Int(v.revision_seq),
            opt(v.job_id.as_deref()),
            Param::Int(v.schema_compat),
            t(&v.manifest_sha256),
            t(&v.verified_at),
            Param::Int(v.created_by),
        ],
    )
}

pub fn record_rollback(tg: &Target, job: &str, v: &PublishedVersion, now: &str) -> Statement {
    st(
        TARGET_RECORD_ROLLBACK,
        vec![
            t(&tg.workspace_id),
            t(&tg.id),
            t(job),
            t(&v.cf_version_id),
            t(&v.id),
            t(&v.revision_id),
            t(now),
        ],
    )
}

pub fn version_get(ws: &str, site: &str, id: &str) -> Statement {
    st(VERSION_GET, vec![t(ws), t(site), t(id)])
}

pub fn audit_insert(a: &AuditRow) -> Statement {
    st(
        AUDIT_INSERT,
        vec![
            t(&a.workspace_id),
            t(&a.site_id),
            t(&a.target_id),
            opt_int(a.actor_user_id),
            t(a.action.as_str()),
            t(a.result.as_str()),
            opt(a.cf_version_id.as_deref()),
            opt(a.published_id.as_deref()),
            opt(a.revision_id.as_deref()),
            opt(a.job_id.as_deref()),
            opt(a.stage),
            opt(a.error_code),
            t(&a.at),
        ],
    )
}

pub fn bundle_get(ws: &str, site: &str, manifest_sha256: &str) -> Statement {
    st(BUNDLE_GET, vec![t(ws), t(site), t(manifest_sha256)])
}

pub fn bundle_insert(
    ws: &str,
    site: &str,
    manifest_sha256: &str,
    total_bytes: u64,
    chunk_count: u32,
    created_by: i64,
    created_at: &str,
) -> Statement {
    st(
        BUNDLE_INSERT,
        vec![
            t(ws),
            t(site),
            t(manifest_sha256),
            Param::Int(i64::try_from(total_bytes).unwrap_or(i64::MAX)),
            Param::Int(i64::from(chunk_count)),
            Param::Int(created_by),
            t(created_at),
        ],
    )
}

pub fn bundle_chunk_insert(
    ws: &str,
    site: &str,
    manifest_sha256: &str,
    seq: u32,
    data: &[u8],
) -> Statement {
    st(
        BUNDLE_CHUNK_INSERT,
        vec![
            t(ws),
            t(site),
            t(manifest_sha256),
            Param::Int(i64::from(seq)),
            Param::Blob(data.to_vec()),
        ],
    )
}

pub fn bundle_chunks_get(ws: &str, site: &str, manifest_sha256: &str) -> Statement {
    st(BUNDLE_CHUNKS_GET, vec![t(ws), t(site), t(manifest_sha256)])
}

pub fn activation_upsert(
    ws: &str,
    site: &str,
    job_id: &str,
    auto_activate: bool,
    credential_tag: Option<&str>,
    bytes_verified: bool,
) -> Statement {
    st(
        ACTIVATION_UPSERT,
        vec![
            t(ws),
            t(site),
            t(job_id),
            Param::Int(i64::from(auto_activate)),
            opt(credential_tag),
            Param::Int(i64::from(bytes_verified)),
        ],
    )
}

pub fn activation_set_verified(ws: &str, site: &str, job_id: &str) -> Statement {
    st(ACTIVATION_SET_VERIFIED, vec![t(ws), t(site), t(job_id)])
}

pub fn activation_set_deployed(
    ws: &str,
    site: &str,
    job_id: &str,
    published_id: &str,
    cf_version_id: &str,
) -> Statement {
    st(
        ACTIVATION_SET_DEPLOYED,
        vec![
            t(ws),
            t(site),
            t(job_id),
            t(published_id),
            t(cf_version_id),
        ],
    )
}

pub fn activation_get(ws: &str, job_id: &str) -> Statement {
    st(ACTIVATION_GET, vec![t(ws), t(job_id)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET_COLS: &str = "workspace_id, site_id, id, account_id, script_name, token_secret, \
bindings_json, compatibility_date, site_schema_level, live_version_id, live_published_id, \
live_revision_id, high_water_seq, activating_job, activating_seq, activating_version_id, lease_expires_at";
    const VERSION_COLS: &str = "workspace_id, site_id, id, target_id, cf_version_id, revision_id, \
revision_seq, job_id, schema_compat, manifest_sha256, verified_at, created_by";

    #[test]
    fn tenant_scoped_and_bound() {
        for (name, sql, tenant) in ALL {
            assert!(*tenant);
            assert!(sql.contains("workspace_id"), "{name}");
            assert!(sql.contains("?1"), "{name}");
            if !sql.starts_with("INSERT") {
                assert!(
                    sql.contains("workspace_id = ?1"),
                    "{name} must filter on workspace_id"
                );
            }
        }
    }

    #[test]
    fn never_writes_revisions_jobs_or_sessions() {
        for (name, sql, _) in ALL {
            let s = sql.to_lowercase();
            for table in ["site_revisions", "deployments", "snapshots", "log "] {
                assert!(
                    !(s.contains(&format!("update {table}"))
                        || s.contains(&format!("insert into {table}"))
                        || s.contains(&format!("delete from {table}"))),
                    "{name} writes {table}"
                );
            }
        }
    }

    #[test]
    fn column_lists_stay_in_sync() {
        assert!(TARGET_GET.contains(TARGET_COLS));
        assert!(VERSION_GET.contains(VERSION_COLS));
        assert!(VERSION_LIST.contains(VERSION_COLS));
    }
}
