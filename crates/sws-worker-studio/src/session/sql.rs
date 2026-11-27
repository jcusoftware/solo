//! SQL for the per-site Durable Object SQLite database.
//!
//! This is *not* the D1 registry (`migrations/`): it is private to one site's
//! Durable Object. Plain constants so `examples/dump_session_sql.rs` can print
//! them and `tests/unit/studio-session-sql.test.mjs` can run the exact text
//! against a real SQLite (node:sqlite) on the host.
//!
//! Atomicity: statements issued in one synchronous turn of the object (no
//! `await` between them) commit together, so "append log row + record op id"
//! and "snapshot + meta" are each all-or-nothing.

/// Idempotent schema (run on every cold start / hibernation wake).
pub const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    // Latest snapshot only (older ones are deleted after the new one is written).
    "CREATE TABLE IF NOT EXISTS snapshots (\
       seq INTEGER PRIMARY KEY, doc TEXT NOT NULL, sha256 TEXT NOT NULL, created_ms INTEGER NOT NULL)",
    // The command log. `seq` is contiguous and strictly increasing (trigger below).
    "CREATE TABLE IF NOT EXISTS log (\
       seq INTEGER PRIMARY KEY, op_id TEXT NOT NULL, user_id INTEGER NOT NULL, \
       client_id TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('batch', 'undo')), \
       target_seq INTEGER, base_seq INTEGER NOT NULL, rebased INTEGER NOT NULL, \
       touched TEXT NOT NULL, tx TEXT NOT NULL, ts_ms INTEGER NOT NULL)",
    "CREATE TRIGGER IF NOT EXISTS log_monotonic BEFORE INSERT ON log \
       WHEN NEW.seq <> max(COALESCE((SELECT MAX(seq) FROM log), 0), \
                           COALESCE((SELECT MAX(seq) FROM snapshots), 0)) + 1 \
       BEGIN SELECT RAISE(ABORT, 'log seq must be the next integer'); END",
    "CREATE TRIGGER IF NOT EXISTS log_immutable BEFORE UPDATE ON log \
       BEGIN SELECT RAISE(ABORT, 'log rows are immutable'); END",
    // Idempotency index; outlives log compaction.
    "CREATE TABLE IF NOT EXISTS ops (\
       op_id TEXT PRIMARY KEY, user_id INTEGER NOT NULL, seq INTEGER NOT NULL, \
       rebased INTEGER NOT NULL, expires_ms INTEGER NOT NULL)",
    "CREATE INDEX IF NOT EXISTS ops_expiry ON ops (expires_ms)",
    "CREATE TABLE IF NOT EXISTS presence (\
       client_id TEXT PRIMARY KEY, user_id INTEGER NOT NULL, state TEXT NOT NULL, \
       updated_ms INTEGER NOT NULL, expires_ms INTEGER NOT NULL)",
    "CREATE INDEX IF NOT EXISTS presence_expiry ON presence (expires_ms)",
];

/// The single JSON meta row lives under [`META_KEY`] (bound as `?1`).
pub const META_KEY: &str = "meta";
pub const META_GET: &str = "SELECT value FROM meta WHERE key = ?1";
pub const META_PUT: &str = "INSERT INTO meta (key, value) VALUES (?1, ?2) \
ON CONFLICT (key) DO UPDATE SET value = excluded.value";

pub const SNAPSHOT_LATEST: &str =
    "SELECT seq, doc, sha256 FROM snapshots ORDER BY seq DESC LIMIT 1";
pub const SNAPSHOT_PUT: &str = "INSERT INTO snapshots (seq, doc, sha256, created_ms) \
VALUES (?1, ?2, ?3, ?4) ON CONFLICT (seq) DO UPDATE SET doc = excluded.doc, \
sha256 = excluded.sha256, created_ms = excluded.created_ms";
pub const SNAPSHOT_DELETE_OLDER: &str = "DELETE FROM snapshots WHERE seq < ?1";

pub const LOG_COLUMNS: &str = "seq, op_id, user_id, client_id, kind, target_seq, base_seq, \
rebased, touched, tx, ts_ms";
pub const LOG_APPEND: &str = "INSERT INTO log \
(seq, op_id, user_id, client_id, kind, target_seq, base_seq, rebased, touched, tx, ts_ms) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";
pub const LOG_AFTER: &str = "SELECT seq, op_id, user_id, client_id, kind, target_seq, base_seq, \
rebased, touched, tx, ts_ms FROM log WHERE seq > ?1 ORDER BY seq LIMIT ?2";
pub const LOG_GET: &str = "SELECT seq, op_id, user_id, client_id, kind, target_seq, base_seq, \
rebased, touched, tx, ts_ms FROM log WHERE seq = ?1";
pub const LOG_TOUCHED_AFTER: &str = "SELECT seq, touched FROM log WHERE seq > ?1 ORDER BY seq";
pub const LOG_MAX_SEQ: &str = "SELECT COALESCE(MAX(seq), 0) AS seq FROM log";
/// Compaction: idempotent (deleting already-deleted rows is a no-op).
pub const LOG_COMPACT: &str = "DELETE FROM log WHERE seq <= ?1";

pub const OP_GET: &str =
    "SELECT op_id, user_id, seq, rebased, expires_ms FROM ops WHERE op_id = ?1 AND expires_ms > ?2";
pub const OP_PUT: &str = "INSERT INTO ops (op_id, user_id, seq, rebased, expires_ms) \
VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (op_id) DO UPDATE SET user_id = excluded.user_id, \
seq = excluded.seq, rebased = excluded.rebased, expires_ms = excluded.expires_ms";
pub const OP_PRUNE_EXPIRED: &str = "DELETE FROM ops WHERE expires_ms <= ?1";
/// Keep at most `?1` newest rows (by seq).
pub const OP_PRUNE_EXCESS: &str =
    "DELETE FROM ops WHERE op_id IN (SELECT op_id FROM ops ORDER BY seq DESC LIMIT -1 OFFSET ?1)";
pub const OP_NEXT_EXPIRY: &str = "SELECT MIN(expires_ms) AS at FROM ops";

pub const PRESENCE_GET: &str =
    "SELECT client_id, user_id, state, updated_ms, expires_ms FROM presence WHERE client_id = ?1";
pub const PRESENCE_PUT: &str = "INSERT INTO presence (client_id, user_id, state, updated_ms, expires_ms) \
VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (client_id) DO UPDATE SET user_id = excluded.user_id, \
state = excluded.state, updated_ms = excluded.updated_ms, expires_ms = excluded.expires_ms";
pub const PRESENCE_DELETE: &str = "DELETE FROM presence WHERE client_id = ?1";
pub const PRESENCE_LIST: &str = "SELECT client_id, user_id, state, updated_ms, expires_ms \
FROM presence WHERE expires_ms > ?1 ORDER BY client_id";
pub const PRESENCE_PRUNE: &str = "DELETE FROM presence WHERE expires_ms <= ?1";
pub const PRESENCE_NEXT_EXPIRY: &str = "SELECT MIN(expires_ms) AS at FROM presence";
pub const PRESENCE_COUNT_EXPIRED: &str =
    "SELECT COUNT(*) AS n FROM presence WHERE expires_ms <= ?1";

/// Every named statement, for the host SQL test and the example dumper.
pub const ALL: &[(&str, &str)] = &[
    ("META_GET", META_GET),
    ("META_PUT", META_PUT),
    ("SNAPSHOT_LATEST", SNAPSHOT_LATEST),
    ("SNAPSHOT_PUT", SNAPSHOT_PUT),
    ("SNAPSHOT_DELETE_OLDER", SNAPSHOT_DELETE_OLDER),
    ("LOG_APPEND", LOG_APPEND),
    ("LOG_AFTER", LOG_AFTER),
    ("LOG_GET", LOG_GET),
    ("LOG_TOUCHED_AFTER", LOG_TOUCHED_AFTER),
    ("LOG_MAX_SEQ", LOG_MAX_SEQ),
    ("LOG_COMPACT", LOG_COMPACT),
    ("OP_GET", OP_GET),
    ("OP_PUT", OP_PUT),
    ("OP_PRUNE_EXPIRED", OP_PRUNE_EXPIRED),
    ("OP_PRUNE_EXCESS", OP_PRUNE_EXCESS),
    ("OP_NEXT_EXPIRY", OP_NEXT_EXPIRY),
    ("PRESENCE_GET", PRESENCE_GET),
    ("PRESENCE_PUT", PRESENCE_PUT),
    ("PRESENCE_DELETE", PRESENCE_DELETE),
    ("PRESENCE_LIST", PRESENCE_LIST),
    ("PRESENCE_PRUNE", PRESENCE_PRUNE),
    ("PRESENCE_NEXT_EXPIRY", PRESENCE_NEXT_EXPIRY),
    ("PRESENCE_COUNT_EXPIRED", PRESENCE_COUNT_EXPIRED),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_use_bound_parameters_only() {
        for (name, sql) in ALL {
            assert!(!sql.contains('\''), "{name} must not inline literals");
        }
    }

    #[test]
    fn log_is_never_updated() {
        for (name, sql) in ALL {
            assert!(
                !(sql.starts_with("UPDATE") && sql.contains(" log ")),
                "{name} must not update the log"
            );
        }
    }
}
