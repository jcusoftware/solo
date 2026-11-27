//! Tenant-scoped repository SQL for the Studio registry (host-testable).
//!
//! Every statement that touches tenant-owned data binds `workspace_id` as `?1`
//! and uses it in the `WHERE` clause (or the inserted row), so a lookup with
//! the wrong workspace matches nothing and callers surface "not found".
//! All values are bound parameters; no SQL is built from user input.
//!
//! Write-frequency invariant: these statements are only reached from explicit
//! save/publish/checkpoint actions. `REVISION_CHECKPOINT` is the only write
//! that records an edited state, and there is intentionally no per-edit route
//! or statement; per-edit state belongs to the session layer (#62).

/// A bound parameter. `Int` is bound as a JS number on D1, which is exact up
/// to 2^53 (GitHub numeric ids are far below that).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    Int(i64),
    Text(String),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub sql: &'static str,
    pub params: Vec<Param>,
}

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

/// Hard cap for list queries.
pub const MAX_LIST: i64 = 200;

fn limit(n: i64) -> Param {
    Param::Int(n.clamp(1, MAX_LIST))
}

// --- Global (not tenant-owned) tables: workspaces and users. -----------------

pub const WORKSPACE_GET: &str = "SELECT id, name, created_at FROM workspaces WHERE id = ?1";
pub const WORKSPACE_INSERT: &str = "INSERT INTO workspaces (id, name) VALUES (?1, ?2)";
pub const USER_UPSERT: &str = "INSERT INTO users (github_id, github_login) VALUES (?1, ?2) \
ON CONFLICT (github_id) DO UPDATE SET github_login = excluded.github_login, \
updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";
pub const USER_BY_GITHUB_ID: &str =
    "SELECT id, github_id, github_login FROM users WHERE github_id = ?1";

// --- Tenant-scoped statements: `workspace_id` is always ?1. ------------------

pub const MEMBERSHIP_INSERT: &str =
    "INSERT INTO memberships (workspace_id, user_id, role) VALUES (?1, ?2, ?3)";
pub const MEMBERSHIP_GET: &str =
    "SELECT user_id, role FROM memberships WHERE workspace_id = ?1 AND user_id = ?2";

/// The workspace's earliest-id owner (the author a checkpoint is re-attributed
/// to when its original author can no longer be referenced).
pub const MEMBERSHIP_OWNER: &str = "SELECT user_id, role FROM memberships \
WHERE workspace_id = ?1 AND role = 'owner' ORDER BY user_id LIMIT 1";

pub const SITE_INSERT: &str =
    "INSERT INTO sites (workspace_id, id, slug, name) VALUES (?1, ?2, ?3, ?4)";
pub const SITE_GET: &str =
    "SELECT id, slug, name, created_at FROM sites WHERE workspace_id = ?1 AND id = ?2";
pub const SITE_BY_SLUG: &str =
    "SELECT id, slug, name, created_at FROM sites WHERE workspace_id = ?1 AND slug = ?2";
pub const SITE_LIST: &str =
    "SELECT id, slug, name, created_at FROM sites WHERE workspace_id = ?1 ORDER BY slug LIMIT ?2";

pub const TOKEN_INSERT: &str = "INSERT INTO api_tokens \
(workspace_id, id, user_id, token_hash, scope, label, created_at, expires_at, chain_started_at) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
pub const TOKEN_GET: &str = "SELECT id, user_id, scope, label, created_at, expires_at, \
chain_started_at, last_used_at, revoked_at FROM api_tokens WHERE workspace_id = ?1 AND id = ?2";
pub const TOKEN_REVOKE: &str = "UPDATE api_tokens SET revoked_at = ?3 \
WHERE workspace_id = ?1 AND id = ?2 AND revoked_at IS NULL";

pub const ASSET_INSERT: &str = "INSERT INTO assets \
(workspace_id, site_id, id, sha256, byte_size, content_type, storage_backend) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
pub const ASSET_BY_SHA256: &str = "SELECT id, sha256, byte_size, content_type, storage_backend \
FROM assets WHERE workspace_id = ?1 AND site_id = ?2 AND sha256 = ?3";
pub const ASSET_LIST: &str = "SELECT id, sha256, byte_size, content_type, storage_backend \
FROM assets WHERE workspace_id = ?1 AND site_id = ?2 ORDER BY created_at, id LIMIT ?3";

/// Explicit checkpoint only: the next revision number is allocated in the
/// statement itself. Never call this per edit.
pub const REVISION_CHECKPOINT: &str = "INSERT INTO site_revisions \
(workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) \
VALUES (?1, ?2, ?3, \
(SELECT COALESCE(MAX(revision), 0) + 1 FROM site_revisions WHERE workspace_id = ?1 AND site_id = ?2), \
?4, ?5, ?6)";
/// Retry-safe form used by the session layer (#62): the caller supplies a
/// deterministic id, so repeating the insert after an ambiguous failure (D1
/// timeout, object eviction) changes nothing. Only an `id` conflict is ignored;
/// every CHECK / foreign key still fails loudly.
pub const REVISION_CHECKPOINT_IDEMPOTENT: &str = "INSERT INTO site_revisions \
(workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) \
VALUES (?1, ?2, ?3, \
(SELECT COALESCE(MAX(revision), 0) + 1 FROM site_revisions WHERE workspace_id = ?1 AND site_id = ?2), \
?4, ?5, ?6) ON CONFLICT (id) DO NOTHING";
pub const REVISION_GET: &str = "SELECT id, revision, ir_sha256, byte_size, created_by, created_at \
FROM site_revisions WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3";
pub const REVISION_LATEST: &str = "SELECT id, revision, ir_sha256, byte_size, created_by, created_at \
FROM site_revisions WHERE workspace_id = ?1 AND site_id = ?2 ORDER BY revision DESC LIMIT 1";

pub const DEPLOYMENT_INSERT: &str = "INSERT INTO deployments \
(workspace_id, site_id, id, revision_id, created_by) VALUES (?1, ?2, ?3, ?4, ?5)";
pub const DEPLOYMENT_GET: &str = "SELECT id, revision_id, status, error_code, created_by, \
created_at, updated_at, finished_at FROM deployments \
WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3";
pub const DEPLOYMENT_LIST: &str = "SELECT id, revision_id, status, created_at FROM deployments \
WHERE workspace_id = ?1 AND site_id = ?2 ORDER BY created_at DESC, id LIMIT ?3";
pub const DEPLOYMENT_SET_STATUS: &str = "UPDATE deployments SET status = ?4, error_code = ?5, \
updated_at = ?6, finished_at = ?7 WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3";

/// Every named statement; used by the SQL-level test (`dump_sql` example) and
/// the tenant-scoping unit test below. The flag marks tenant-scoped statements.
pub const ALL: &[(&str, &str, bool)] = &[
    ("WORKSPACE_GET", WORKSPACE_GET, false),
    ("WORKSPACE_INSERT", WORKSPACE_INSERT, false),
    ("USER_UPSERT", USER_UPSERT, false),
    ("USER_BY_GITHUB_ID", USER_BY_GITHUB_ID, false),
    ("MEMBERSHIP_INSERT", MEMBERSHIP_INSERT, true),
    ("MEMBERSHIP_GET", MEMBERSHIP_GET, true),
    ("MEMBERSHIP_OWNER", MEMBERSHIP_OWNER, true),
    ("SITE_INSERT", SITE_INSERT, true),
    ("SITE_GET", SITE_GET, true),
    ("SITE_BY_SLUG", SITE_BY_SLUG, true),
    ("SITE_LIST", SITE_LIST, true),
    ("TOKEN_INSERT", TOKEN_INSERT, true),
    ("TOKEN_GET", TOKEN_GET, true),
    ("TOKEN_REVOKE", TOKEN_REVOKE, true),
    ("ASSET_INSERT", ASSET_INSERT, true),
    ("ASSET_BY_SHA256", ASSET_BY_SHA256, true),
    ("ASSET_LIST", ASSET_LIST, true),
    ("REVISION_CHECKPOINT", REVISION_CHECKPOINT, true),
    (
        "REVISION_CHECKPOINT_IDEMPOTENT",
        REVISION_CHECKPOINT_IDEMPOTENT,
        true,
    ),
    ("REVISION_GET", REVISION_GET, true),
    ("REVISION_LATEST", REVISION_LATEST, true),
    ("DEPLOYMENT_INSERT", DEPLOYMENT_INSERT, true),
    ("DEPLOYMENT_GET", DEPLOYMENT_GET, true),
    ("DEPLOYMENT_LIST", DEPLOYMENT_LIST, true),
    ("DEPLOYMENT_SET_STATUS", DEPLOYMENT_SET_STATUS, true),
];

// --- Statement builders (workspace id is the mandatory first argument). ------

pub fn workspace_get(id: &str) -> Statement {
    Statement {
        sql: WORKSPACE_GET,
        params: vec![t(id)],
    }
}

pub fn membership_get(workspace_id: &str, user_id: i64) -> Statement {
    Statement {
        sql: MEMBERSHIP_GET,
        params: vec![t(workspace_id), Param::Int(user_id)],
    }
}

pub fn membership_owner(workspace_id: &str) -> Statement {
    Statement {
        sql: MEMBERSHIP_OWNER,
        params: vec![t(workspace_id)],
    }
}

pub fn site_get(workspace_id: &str, site_id: &str) -> Statement {
    Statement {
        sql: SITE_GET,
        params: vec![t(workspace_id), t(site_id)],
    }
}

pub fn site_by_slug(workspace_id: &str, slug: &str) -> Statement {
    Statement {
        sql: SITE_BY_SLUG,
        params: vec![t(workspace_id), t(slug)],
    }
}

pub fn site_list(workspace_id: &str, max: i64) -> Statement {
    Statement {
        sql: SITE_LIST,
        params: vec![t(workspace_id), limit(max)],
    }
}

pub fn token_get(workspace_id: &str, token_id: &str) -> Statement {
    Statement {
        sql: TOKEN_GET,
        params: vec![t(workspace_id), t(token_id)],
    }
}

pub fn token_revoke(workspace_id: &str, token_id: &str, now: &str) -> Statement {
    Statement {
        sql: TOKEN_REVOKE,
        params: vec![t(workspace_id), t(token_id), t(now)],
    }
}

pub fn asset_by_sha256(workspace_id: &str, site_id: &str, sha256: &str) -> Statement {
    Statement {
        sql: ASSET_BY_SHA256,
        params: vec![t(workspace_id), t(site_id), t(sha256)],
    }
}

pub fn asset_list(workspace_id: &str, site_id: &str, max: i64) -> Statement {
    Statement {
        sql: ASSET_LIST,
        params: vec![t(workspace_id), t(site_id), limit(max)],
    }
}

/// Statement for the D1 side of a session checkpoint (see
/// [`REVISION_CHECKPOINT_IDEMPOTENT`]). One call writes at most one row.
pub fn revision_checkpoint_idempotent(
    workspace_id: &str,
    site_id: &str,
    revision_id: &str,
    ir_sha256: &str,
    byte_size: i64,
    created_by: i64,
) -> Statement {
    Statement {
        sql: REVISION_CHECKPOINT_IDEMPOTENT,
        params: vec![
            t(workspace_id),
            t(site_id),
            t(revision_id),
            t(ir_sha256),
            Param::Int(byte_size),
            Param::Int(created_by),
        ],
    }
}

pub fn revision_get(workspace_id: &str, site_id: &str, revision_id: &str) -> Statement {
    Statement {
        sql: REVISION_GET,
        params: vec![t(workspace_id), t(site_id), t(revision_id)],
    }
}

pub fn revision_latest(workspace_id: &str, site_id: &str) -> Statement {
    Statement {
        sql: REVISION_LATEST,
        params: vec![t(workspace_id), t(site_id)],
    }
}

pub fn deployment_get(workspace_id: &str, site_id: &str, deployment_id: &str) -> Statement {
    Statement {
        sql: DEPLOYMENT_GET,
        params: vec![t(workspace_id), t(site_id), t(deployment_id)],
    }
}

pub fn deployment_list(workspace_id: &str, site_id: &str, max: i64) -> Statement {
    Statement {
        sql: DEPLOYMENT_LIST,
        params: vec![t(workspace_id), t(site_id), limit(max)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_statements_bind_workspace_first() {
        for (name, sql, tenant) in ALL {
            if *tenant {
                assert!(
                    sql.contains("workspace_id"),
                    "{name} must reference workspace_id"
                );
                assert!(sql.contains("?1"), "{name} must bind ?1");
            }
        }
    }

    #[test]
    fn tenant_reads_filter_on_workspace_id() {
        for (name, sql, tenant) in ALL {
            let is_write = sql.starts_with("INSERT");
            if *tenant && !is_write {
                assert!(
                    sql.contains("workspace_id = ?1"),
                    "{name} must filter on workspace_id = ?1"
                );
            }
        }
    }

    #[test]
    fn no_per_edit_write_path() {
        // Revisions are immutable checkpoints: inserted only, never updated.
        for (name, sql, _) in ALL {
            assert!(
                !(sql.starts_with("UPDATE") && sql.contains("site_revisions")),
                "{name} must not update site_revisions"
            );
        }
    }

    #[test]
    fn builders_bind_workspace_id_first() {
        let ws = "ws-a";
        let stmts = [
            membership_get(ws, 1),
            membership_owner(ws),
            site_get(ws, "s"),
            site_by_slug(ws, "s"),
            site_list(ws, 10),
            token_get(ws, "t"),
            token_revoke(ws, "t", "now"),
            asset_by_sha256(ws, "s", "h"),
            asset_list(ws, "s", 10),
            revision_get(ws, "s", "r"),
            revision_checkpoint_idempotent(ws, "s", "r", "h", 1, 1),
            revision_latest(ws, "s"),
            deployment_get(ws, "s", "d"),
            deployment_list(ws, "s", 10),
        ];
        for s in stmts {
            assert_eq!(s.params[0], Param::Text(ws.to_string()), "{}", s.sql);
        }
    }

    #[test]
    fn list_limits_are_clamped() {
        assert_eq!(site_list("w", 10_000).params[1], Param::Int(MAX_LIST));
        assert_eq!(site_list("w", -5).params[1], Param::Int(1));
    }
}
