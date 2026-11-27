//! Tenant-scoped SQL for the asset store (`migrations/0002_asset_store.sql`).
//!
//! Same rules as [`crate::repo`]: every statement binds `workspace_id` as `?1`, all
//! values are bound parameters, and the SQL is exercised against the real migration
//! by `tests/unit/studio-assets.test.mjs` (via `examples/dump_sql.rs`).
//!
//! Capacity model: usage = committed rows + live (`expires_at > now`) reservations.
//! [`RESERVE_ASSET`] is ONE conditional `INSERT ... SELECT ... WHERE`: D1 serialises
//! writes, so two concurrent reservations can never both pass a check that only one
//! of them fits, and there is no counter that can drift.

use sws_assets::limits::{AssetLimits, GeneratedKind, MAX_WORKSPACE_GENERATED_BYTES};

use crate::repo::{Param, Statement};

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

fn int(n: u64) -> Param {
    Param::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

// --- Asset reservations ------------------------------------------------------

/// ?1 workspace, ?2 site, ?3 reservation id, ?4 bytes, ?5 now, ?6 expires_at,
/// ?7 site byte cap, ?8 workspace byte cap, ?9 site asset-count cap.
/// `changes == 0` means refused: nothing was reserved.
pub const RESERVE_ASSET: &str = "INSERT INTO asset_reservations \
(workspace_id, site_id, id, kind, byte_size, created_at, expires_at) \
SELECT ?1, ?2, ?3, 'asset', ?4, ?5, ?6 \
WHERE (SELECT COALESCE(SUM(byte_size), 0) FROM assets WHERE workspace_id = ?1 AND site_id = ?2) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
    WHERE workspace_id = ?1 AND site_id = ?2 AND kind = 'asset' AND expires_at > ?5) + ?4 <= ?7 \
AND (SELECT COALESCE(SUM(byte_size), 0) FROM assets WHERE workspace_id = ?1) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
    WHERE workspace_id = ?1 AND kind = 'asset' AND expires_at > ?5) + ?4 <= ?8 \
AND (SELECT COUNT(*) FROM assets WHERE workspace_id = ?1 AND site_id = ?2) \
 + (SELECT COUNT(*) FROM asset_reservations \
    WHERE workspace_id = ?1 AND site_id = ?2 AND kind = 'asset' AND expires_at > ?5) < ?9";

/// ?1 workspace, ?2 site, ?3 now. Stored and reserved usage, for diagnostics after a
/// refused reservation and for the usage listing.
pub const USAGE: &str = "SELECT \
(SELECT COALESCE(SUM(byte_size), 0) FROM assets WHERE workspace_id = ?1 AND site_id = ?2) AS site_stored, \
(SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
  WHERE workspace_id = ?1 AND site_id = ?2 AND kind = 'asset' AND expires_at > ?3) AS site_reserved, \
(SELECT COUNT(*) FROM assets WHERE workspace_id = ?1 AND site_id = ?2) AS site_assets, \
(SELECT COUNT(*) FROM asset_reservations \
  WHERE workspace_id = ?1 AND site_id = ?2 AND kind = 'asset' AND expires_at > ?3) AS site_reserved_assets, \
(SELECT COALESCE(SUM(byte_size), 0) FROM assets WHERE workspace_id = ?1) AS workspace_stored, \
(SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
  WHERE workspace_id = ?1 AND kind = 'asset' AND expires_at > ?3) AS workspace_reserved";

/// ?1 workspace, ?2 site, ?3 reservation id.
pub const RESERVATION_RELEASE: &str =
    "DELETE FROM asset_reservations WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3";

/// ?1 workspace, ?2 reservation id. Used as the last statement of a commit batch.
pub const RESERVATION_DELETE: &str =
    "DELETE FROM asset_reservations WHERE workspace_id = ?1 AND id = ?2";

// --- Asset commit / read / delete -------------------------------------------

/// ?1 workspace, ?2 site, ?3 asset id, ?4 sha256, ?5 actual bytes, ?6 content type,
/// ?7 backend, ?8 now, ?9 reservation id. Inserts only while the reservation is live
/// and large enough; an existing (workspace, site, sha256) is left untouched.
pub const ASSET_COMMIT: &str = "INSERT INTO assets \
(workspace_id, site_id, id, sha256, byte_size, content_type, storage_backend, created_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8 \
WHERE EXISTS (SELECT 1 FROM asset_reservations WHERE workspace_id = ?1 AND site_id = ?2 \
  AND id = ?9 AND kind = 'asset' AND expires_at > ?8 AND byte_size >= ?5) \
ON CONFLICT DO NOTHING";

/// ?1 workspace, ?2 blob id, ?3 seq, ?4 chunk bytes, ?5 data. Only when the owning
/// asset row exists. A duplicate upload of identical bytes resolves to the same asset
/// id and chunks, so only a primary-key conflict is skipped (`ON CONFLICT DO NOTHING`);
/// deliberately NOT `OR IGNORE`, which would also swallow a violated row-size CHECK.
pub const CHUNK_INSERT: &str = "INSERT INTO blob_chunks \
(workspace_id, blob_id, seq, byte_size, data) \
SELECT ?1, ?2, ?3, ?4, ?5 \
WHERE EXISTS (SELECT 1 FROM assets WHERE workspace_id = ?1 AND id = ?2) \
ON CONFLICT (workspace_id, blob_id, seq) DO NOTHING";

/// ?1 workspace, ?2 blob id, ?3 seq.
pub const CHUNK_GET: &str =
    "SELECT data FROM blob_chunks WHERE workspace_id = ?1 AND blob_id = ?2 AND seq = ?3";

/// ?1 workspace, ?2 blob id.
pub const CHUNK_COUNT: &str = "SELECT COUNT(*) AS chunks, COALESCE(SUM(byte_size), 0) AS bytes \
FROM blob_chunks WHERE workspace_id = ?1 AND blob_id = ?2";

/// ?1 workspace, ?2 site, ?3 sha256. Deletes the chunks of the asset, then the row.
pub const CHUNKS_DELETE_FOR_ASSET: &str = "DELETE FROM blob_chunks WHERE workspace_id = ?1 \
AND blob_id = (SELECT id FROM assets WHERE workspace_id = ?1 AND site_id = ?2 AND sha256 = ?3)";
pub const ASSET_DELETE: &str =
    "DELETE FROM assets WHERE workspace_id = ?1 AND site_id = ?2 AND sha256 = ?3";

// --- Reconciliation ----------------------------------------------------------

/// ?1 workspace, ?2 now, ?3 batch size. Removes reservations past their expiry.
pub const RESERVATIONS_SWEEP: &str = "DELETE FROM asset_reservations WHERE workspace_id = ?1 \
AND id IN (SELECT id FROM asset_reservations WHERE workspace_id = ?1 AND expires_at <= ?2 LIMIT ?3)";

/// ?1 workspace, ?2 batch size. Chunks whose owner (asset or generated artifact) is gone.
pub const CHUNKS_ORPHAN_SWEEP: &str = "DELETE FROM blob_chunks WHERE workspace_id = ?1 AND rowid IN ( \
SELECT c.rowid FROM blob_chunks c WHERE c.workspace_id = ?1 \
AND NOT EXISTS (SELECT 1 FROM assets a WHERE a.workspace_id = ?1 AND a.id = c.blob_id) \
AND NOT EXISTS (SELECT 1 FROM generated_artifacts g WHERE g.workspace_id = ?1 AND g.id = c.blob_id) \
LIMIT ?2)";

/// ?1 workspace, ?2 site, ?3 batch size. D1-backed asset rows whose chunk bytes do not
/// add up to `byte_size` (a partial write that escaped the atomic commit).
pub const ASSETS_INCOMPLETE_SWEEP: &str = "DELETE FROM assets WHERE workspace_id = ?1 AND id IN ( \
SELECT a.id FROM assets a WHERE a.workspace_id = ?1 AND a.site_id = ?2 AND a.storage_backend = 'd1' \
AND a.byte_size <> COALESCE((SELECT SUM(c.byte_size) FROM blob_chunks c \
  WHERE c.workspace_id = ?1 AND c.blob_id = a.id), 0) LIMIT ?3)";

/// ?1 workspace, ?2 site, ?3 batch size. R2-backed rows, for orphan-object checks.
pub const ASSET_R2_REFERENCED: &str = "SELECT 1 AS referenced FROM assets \
WHERE workspace_id = ?1 AND site_id = ?2 AND sha256 = ?3 AND storage_backend = 'r2'";

// --- Generated preview/export artifacts -------------------------------------

/// ?1 workspace, ?2 site, ?3 reservation id, ?4 kind, ?5 bytes, ?6 now, ?7 expires_at,
/// ?8 per-site cap for this kind, ?9 workspace-wide generated cap.
pub const RESERVE_GENERATED: &str = "INSERT INTO asset_reservations \
(workspace_id, site_id, id, kind, byte_size, created_at, expires_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
WHERE (SELECT COALESCE(SUM(byte_size), 0) FROM generated_artifacts \
    WHERE workspace_id = ?1 AND site_id = ?2 AND kind = ?4) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
    WHERE workspace_id = ?1 AND site_id = ?2 AND kind = ?4 AND expires_at > ?6) + ?5 <= ?8 \
AND (SELECT COALESCE(SUM(byte_size), 0) FROM generated_artifacts WHERE workspace_id = ?1) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
    WHERE workspace_id = ?1 AND kind IN ('preview', 'export') AND expires_at > ?6) + ?5 <= ?9";

/// ?1 workspace, ?2 site, ?3 artifact id, ?4 kind, ?5 sha256, ?6 bytes, ?7 backend,
/// ?8 created_at, ?9 expires_at, ?10 reservation id.
pub const GENERATED_COMMIT: &str = "INSERT INTO generated_artifacts \
(workspace_id, site_id, id, kind, sha256, byte_size, storage_backend, created_at, expires_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
WHERE EXISTS (SELECT 1 FROM asset_reservations WHERE workspace_id = ?1 AND site_id = ?2 \
  AND id = ?10 AND kind = ?4 AND expires_at > ?8 AND byte_size >= ?6) \
ON CONFLICT DO NOTHING";

/// ?1 workspace, ?2 site, ?3 now, ?4 batch size. Retention: past `expires_at`.
pub const GENERATED_EXPIRED_SWEEP: &str = "DELETE FROM generated_artifacts WHERE workspace_id = ?1 \
AND id IN (SELECT id FROM generated_artifacts WHERE workspace_id = ?1 AND site_id = ?2 \
AND expires_at <= ?3 LIMIT ?4)";

/// ?1 workspace, ?2 site, ?3 kind, ?4 keep-latest count. Retention: all but the newest N.
pub const GENERATED_EXCESS_SWEEP: &str = "DELETE FROM generated_artifacts WHERE workspace_id = ?1 \
AND site_id = ?2 AND kind = ?3 AND id NOT IN (SELECT id FROM generated_artifacts \
WHERE workspace_id = ?1 AND site_id = ?2 AND kind = ?3 ORDER BY created_at DESC, id DESC LIMIT ?4)";

/// ?1 workspace, ?2 site.
pub const GENERATED_USAGE: &str = "SELECT kind, COUNT(*) AS artifacts, COALESCE(SUM(byte_size), 0) AS bytes \
FROM generated_artifacts WHERE workspace_id = ?1 AND site_id = ?2 GROUP BY kind";

/// The chunk plan must never exceed the `blob_chunks.byte_size` CHECK in the migration (512 KiB).
const _: () = assert!(sws_assets::limits::CHUNK_BYTES <= 524_288);

/// Every statement here (name, sql, tenant-scoped); all are tenant-scoped.
pub const ALL: &[(&str, &str, bool)] = &[
    ("RESERVE_ASSET", RESERVE_ASSET, true),
    ("USAGE", USAGE, true),
    ("RESERVATION_RELEASE", RESERVATION_RELEASE, true),
    ("RESERVATION_DELETE", RESERVATION_DELETE, true),
    ("ASSET_COMMIT", ASSET_COMMIT, true),
    ("CHUNK_INSERT", CHUNK_INSERT, true),
    ("CHUNK_GET", CHUNK_GET, true),
    ("CHUNK_COUNT", CHUNK_COUNT, true),
    ("CHUNKS_DELETE_FOR_ASSET", CHUNKS_DELETE_FOR_ASSET, true),
    ("ASSET_DELETE", ASSET_DELETE, true),
    ("RESERVATIONS_SWEEP", RESERVATIONS_SWEEP, true),
    ("CHUNKS_ORPHAN_SWEEP", CHUNKS_ORPHAN_SWEEP, true),
    ("ASSETS_INCOMPLETE_SWEEP", ASSETS_INCOMPLETE_SWEEP, true),
    ("ASSET_R2_REFERENCED", ASSET_R2_REFERENCED, true),
    ("RESERVE_GENERATED", RESERVE_GENERATED, true),
    ("GENERATED_COMMIT", GENERATED_COMMIT, true),
    ("GENERATED_EXPIRED_SWEEP", GENERATED_EXPIRED_SWEEP, true),
    ("GENERATED_EXCESS_SWEEP", GENERATED_EXCESS_SWEEP, true),
    ("GENERATED_USAGE", GENERATED_USAGE, true),
];

// --- Builders ----------------------------------------------------------------

pub fn reserve_asset(
    ws: &str,
    site: &str,
    reservation_id: &str,
    bytes: u64,
    now: &str,
    expires_at: &str,
    limits: &AssetLimits,
) -> Statement {
    Statement {
        sql: RESERVE_ASSET,
        params: vec![
            t(ws),
            t(site),
            t(reservation_id),
            int(bytes),
            t(now),
            t(expires_at),
            int(limits.max_site_bytes),
            int(limits.max_workspace_bytes),
            int(limits.max_site_assets),
        ],
    }
}

pub fn usage(ws: &str, site: &str, now: &str) -> Statement {
    Statement {
        sql: USAGE,
        params: vec![t(ws), t(site), t(now)],
    }
}

pub fn reservation_release(ws: &str, site: &str, reservation_id: &str) -> Statement {
    Statement {
        sql: RESERVATION_RELEASE,
        params: vec![t(ws), t(site), t(reservation_id)],
    }
}

pub fn reservation_delete(ws: &str, reservation_id: &str) -> Statement {
    Statement {
        sql: RESERVATION_DELETE,
        params: vec![t(ws), t(reservation_id)],
    }
}

#[allow(clippy::too_many_arguments)]
pub fn asset_commit(
    ws: &str,
    site: &str,
    asset_id: &str,
    sha256: &str,
    bytes: u64,
    content_type: &str,
    backend: &str,
    now: &str,
    reservation_id: &str,
) -> Statement {
    Statement {
        sql: ASSET_COMMIT,
        params: vec![
            t(ws),
            t(site),
            t(asset_id),
            t(sha256),
            int(bytes),
            t(content_type),
            t(backend),
            t(now),
            t(reservation_id),
        ],
    }
}

pub fn chunk_insert(ws: &str, blob_id: &str, seq: u32, data: &[u8]) -> Statement {
    Statement {
        sql: CHUNK_INSERT,
        params: vec![
            t(ws),
            t(blob_id),
            Param::Int(i64::from(seq)),
            int(data.len() as u64),
            Param::Blob(data.to_vec()),
        ],
    }
}

pub fn chunk_get(ws: &str, blob_id: &str, seq: u32) -> Statement {
    Statement {
        sql: CHUNK_GET,
        params: vec![t(ws), t(blob_id), Param::Int(i64::from(seq))],
    }
}

pub fn chunks_delete_for_asset(ws: &str, site: &str, sha256: &str) -> Statement {
    Statement {
        sql: CHUNKS_DELETE_FOR_ASSET,
        params: vec![t(ws), t(site), t(sha256)],
    }
}

pub fn asset_delete(ws: &str, site: &str, sha256: &str) -> Statement {
    Statement {
        sql: ASSET_DELETE,
        params: vec![t(ws), t(site), t(sha256)],
    }
}

pub fn reservations_sweep(ws: &str, now: &str, batch: u32) -> Statement {
    Statement {
        sql: RESERVATIONS_SWEEP,
        params: vec![t(ws), t(now), Param::Int(i64::from(batch))],
    }
}

pub fn chunks_orphan_sweep(ws: &str, batch: u32) -> Statement {
    Statement {
        sql: CHUNKS_ORPHAN_SWEEP,
        params: vec![t(ws), Param::Int(i64::from(batch))],
    }
}

pub fn assets_incomplete_sweep(ws: &str, site: &str, batch: u32) -> Statement {
    Statement {
        sql: ASSETS_INCOMPLETE_SWEEP,
        params: vec![t(ws), t(site), Param::Int(i64::from(batch))],
    }
}

pub fn asset_r2_referenced(ws: &str, site: &str, sha256: &str) -> Statement {
    Statement {
        sql: ASSET_R2_REFERENCED,
        params: vec![t(ws), t(site), t(sha256)],
    }
}

pub fn reserve_generated(
    ws: &str,
    site: &str,
    reservation_id: &str,
    kind: GeneratedKind,
    bytes: u64,
    now: &str,
    expires_at: &str,
) -> Statement {
    Statement {
        sql: RESERVE_GENERATED,
        params: vec![
            t(ws),
            t(site),
            t(reservation_id),
            t(kind.as_str()),
            int(bytes),
            t(now),
            t(expires_at),
            int(kind.policy().max_site_bytes),
            int(MAX_WORKSPACE_GENERATED_BYTES),
        ],
    }
}

pub fn generated_expired_sweep(ws: &str, site: &str, now: &str, batch: u32) -> Statement {
    Statement {
        sql: GENERATED_EXPIRED_SWEEP,
        params: vec![t(ws), t(site), t(now), Param::Int(i64::from(batch))],
    }
}

pub fn generated_excess_sweep(ws: &str, site: &str, kind: GeneratedKind) -> Statement {
    Statement {
        sql: GENERATED_EXCESS_SWEEP,
        params: vec![
            t(ws),
            t(site),
            t(kind.as_str()),
            Param::Int(i64::from(kind.policy().keep_latest)),
        ],
    }
}

pub fn generated_usage(ws: &str, site: &str) -> Statement {
    Statement {
        sql: GENERATED_USAGE,
        params: vec![t(ws), t(site)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_assets::limits::CARDLESS_D1;

    #[test]
    fn every_statement_is_tenant_scoped() {
        for (name, sql, _) in ALL {
            assert!(
                sql.contains("workspace_id = ?1") || sql.contains("?1,") || sql.contains("?1 "),
                "{name}"
            );
            // Each table reference must be filtered by the workspace.
            if !sql.starts_with("INSERT") {
                assert!(
                    sql.contains("workspace_id = ?1"),
                    "{name} must filter on workspace_id = ?1"
                );
            }
        }
    }

    #[test]
    fn builders_bind_workspace_first_and_param_counts_match() {
        let l = CARDLESS_D1;
        let stmts = [
            reserve_asset("w", "s", "r", 5, "n", "e", &l),
            usage("w", "s", "n"),
            reservation_release("w", "s", "r"),
            reservation_delete("w", "r"),
            asset_commit("w", "s", "a", "h", 1, "image/png", "d1", "n", "r"),
            chunk_insert("w", "a", 0, &[1, 2]),
            chunk_get("w", "a", 0),
            chunks_delete_for_asset("w", "s", "h"),
            asset_delete("w", "s", "h"),
            reservations_sweep("w", "n", 10),
            chunks_orphan_sweep("w", 10),
            assets_incomplete_sweep("w", "s", 10),
            asset_r2_referenced("w", "s", "h"),
            reserve_generated("w", "s", "r", GeneratedKind::Preview, 5, "n", "e"),
            generated_expired_sweep("w", "s", "n", 10),
            generated_excess_sweep("w", "s", GeneratedKind::Export),
            generated_usage("w", "s"),
        ];
        for s in stmts {
            assert_eq!(s.params[0], Param::Text("w".into()), "{}", s.sql);
            let max = (1..=12)
                .rev()
                .find(|n| s.sql.contains(&format!("?{n}")))
                .unwrap();
            assert_eq!(s.params.len(), max, "{}", s.sql);
        }
    }
}
