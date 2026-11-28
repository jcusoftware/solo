//! SQL for previews (`migrations/0006_previews.sql`).
//!
//! Same rules as [`crate::repo`] and [`crate::asset_repo`]: bound parameters
//! only, and every statement binds `workspace_id` as `?1`, except
//! [`PREVIEW_BY_TOKEN`]: on the preview host the token IS the credential, so it
//! is looked up by its SHA-256 digest (a UNIQUE column) and the caller then
//! checks the site and revision in the URL against the row. The SQL is run
//! against the real migrations by `tests/unit/studio-previews.test.mjs` (via
//! `examples/dump_sql.rs --previews`).

use crate::repo::{Param, Statement};

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

/// ?1 workspace, ?2 site, ?3 preview id, ?4 revision id, ?5 artifact id, ?6 token
/// hash, ?7 user id. Inserts only when the artifact was committed in the same
/// batch, copying ITS `created_at`/`expires_at`, so the link can never outlive
/// the bytes' retention.
pub const PREVIEW_INSERT: &str = "INSERT INTO previews \
(workspace_id, site_id, id, revision_id, artifact_id, token_hash, created_by, created_at, expires_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, g.created_at, g.expires_at FROM generated_artifacts g \
WHERE g.workspace_id = ?1 AND g.site_id = ?2 AND g.id = ?5 AND g.kind = 'preview'";

/// ?1 token hash. NOT tenant-scoped (see the module docs). The artifact columns
/// are NULL once retention, eviction or revocation removed it.
pub const PREVIEW_BY_TOKEN: &str = "SELECT p.workspace_id, p.site_id, p.id, p.revision_id, \
p.expires_at, p.revoked_at, g.id AS artifact_id, g.sha256 AS artifact_sha256, \
g.byte_size AS artifact_bytes, g.expires_at AS artifact_expires_at \
FROM previews p LEFT JOIN generated_artifacts g \
ON g.workspace_id = p.workspace_id AND g.site_id = p.site_id AND g.id = p.artifact_id AND g.kind = 'preview' \
WHERE p.token_hash = ?1";

/// ?1 workspace, ?2 site, ?3 preview id.
pub const PREVIEW_GET: &str = "SELECT id, revision_id, expires_at, revoked_at FROM previews \
WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3";

/// ?1 workspace, ?2 site, ?3 preview id, ?4 now. One-way.
pub const PREVIEW_REVOKE: &str = "UPDATE previews SET revoked_at = ?4 \
WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3 AND revoked_at IS NULL";

/// ?1 workspace, ?2 site, ?3 preview id. Frees the preview's budget at once; the
/// `trg_generated_after_delete` trigger removes its chunks in the same statement.
pub const PREVIEW_ARTIFACT_DELETE: &str = "DELETE FROM generated_artifacts \
WHERE workspace_id = ?1 AND site_id = ?2 AND kind = 'preview' AND id IN \
(SELECT artifact_id FROM previews WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3)";

/// ?1 workspace, ?2 site, ?3 cutoff (rows that expired at or before it), ?4 batch.
pub const PREVIEWS_SWEEP: &str = "DELETE FROM previews WHERE workspace_id = ?1 AND id IN \
(SELECT id FROM previews WHERE workspace_id = ?1 AND site_id = ?2 AND expires_at <= ?3 LIMIT ?4)";

/// Every statement (name, sql, tenant-scoped).
pub const ALL: &[(&str, &str, bool)] = &[
    ("PREVIEW_INSERT", PREVIEW_INSERT, true),
    ("PREVIEW_BY_TOKEN", PREVIEW_BY_TOKEN, false),
    ("PREVIEW_GET", PREVIEW_GET, true),
    ("PREVIEW_REVOKE", PREVIEW_REVOKE, true),
    ("PREVIEW_ARTIFACT_DELETE", PREVIEW_ARTIFACT_DELETE, true),
    ("PREVIEWS_SWEEP", PREVIEWS_SWEEP, true),
];

pub fn preview_insert(
    ws: &str,
    site: &str,
    id: &str,
    revision_id: &str,
    artifact_id: &str,
    token_hash: &str,
    user_id: i64,
) -> Statement {
    Statement {
        sql: PREVIEW_INSERT,
        params: vec![
            t(ws),
            t(site),
            t(id),
            t(revision_id),
            t(artifact_id),
            t(token_hash),
            Param::Int(user_id),
        ],
    }
}

pub fn preview_by_token(token_hash: &str) -> Statement {
    Statement {
        sql: PREVIEW_BY_TOKEN,
        params: vec![t(token_hash)],
    }
}

pub fn preview_get(ws: &str, site: &str, id: &str) -> Statement {
    Statement {
        sql: PREVIEW_GET,
        params: vec![t(ws), t(site), t(id)],
    }
}

pub fn preview_revoke(ws: &str, site: &str, id: &str, now: &str) -> Statement {
    Statement {
        sql: PREVIEW_REVOKE,
        params: vec![t(ws), t(site), t(id), t(now)],
    }
}

pub fn preview_artifact_delete(ws: &str, site: &str, id: &str) -> Statement {
    Statement {
        sql: PREVIEW_ARTIFACT_DELETE,
        params: vec![t(ws), t(site), t(id)],
    }
}

pub fn previews_sweep(ws: &str, site: &str, cutoff: &str, batch: u32) -> Statement {
    Statement {
        sql: PREVIEWS_SWEEP,
        params: vec![t(ws), t(site), t(cutoff), Param::Int(i64::from(batch))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_flags_match_the_sql() {
        for (name, sql, tenant) in ALL {
            assert_eq!(sql.contains("workspace_id = ?1"), *tenant, "{name}");
        }
        assert!(PREVIEW_BY_TOKEN.contains("p.token_hash = ?1"));
    }

    #[test]
    fn builders_bind_every_placeholder() {
        let stmts = [
            preview_insert("w", "s", "p", "r", "g", "h", 1),
            preview_by_token("h"),
            preview_get("w", "s", "p"),
            preview_revoke("w", "s", "p", "n"),
            preview_artifact_delete("w", "s", "p"),
            previews_sweep("w", "s", "n", 10),
        ];
        for s in stmts {
            let max = (1..=12)
                .rev()
                .find(|n| s.sql.contains(&format!("?{n}")))
                .unwrap_or(0);
            assert_eq!(s.params.len(), max, "{}", s.sql);
        }
    }
}
