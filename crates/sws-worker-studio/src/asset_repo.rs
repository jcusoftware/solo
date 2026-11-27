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

use sws_assets::limits::{
    AssetLimits, GeneratedKind, MAX_DATABASE_BLOB_BYTES, MAX_WORKSPACE_GENERATED_BYTES,
};
use sws_assets::time::utc_text;

use crate::repo::{Param, Statement};

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

fn int(n: u64) -> Param {
    Param::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

// --- Asset reservations ------------------------------------------------------

/// ?1 workspace, ?2 site, ?3 reservation id, ?4 bytes, ?5 now, ?6 expires_at,
/// ?7 site byte cap, ?8 workspace byte cap, ?9 site asset-count cap, ?10 database-wide
/// blob byte cap. The database sum (every workspace's D1-backed assets, generated
/// artifacts and live reservations of any kind) is the only non-tenant read: it
/// yields a single aggregate, never rows. Reservations are counted whatever their
/// backend (conservative: an R2 reservation is not D1 data but is not told apart).
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
    WHERE workspace_id = ?1 AND site_id = ?2 AND kind = 'asset' AND expires_at > ?5) < ?9 \
AND (SELECT COALESCE(SUM(byte_size), 0) FROM assets WHERE storage_backend = 'd1') \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM generated_artifacts) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations WHERE expires_at > ?5) + ?4 <= ?10";

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
  WHERE workspace_id = ?1 AND kind = 'asset' AND expires_at > ?3) AS workspace_reserved, \
(SELECT COALESCE(SUM(byte_size), 0) FROM assets WHERE storage_backend = 'd1') \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM generated_artifacts) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations WHERE expires_at > ?3) AS database_blob_bytes";

/// ?1 workspace, ?2 site, ?3 reservation id.
pub const RESERVATION_RELEASE: &str =
    "DELETE FROM asset_reservations WHERE workspace_id = ?1 AND site_id = ?2 AND id = ?3";

/// ?1 workspace, ?2 reservation id. Used as the last statement of a commit batch.
pub const RESERVATION_DELETE: &str =
    "DELETE FROM asset_reservations WHERE workspace_id = ?1 AND id = ?2";

// --- Asset commit / read / delete -------------------------------------------

/// ?1 workspace, ?2 site, ?3 asset id, ?4 sha256, ?5 actual bytes, ?6 content type,
/// ?7 backend, ?8 created_at (request start), ?9 reservation id, ?10 R2 object key
/// (NULL for D1), ?11 commit time. Inserts only while the reservation is live AT
/// COMMIT TIME (`?11`, a clock read immediately before the commit batch, never the
/// request-start time in `?8`) and large enough; an existing (workspace, site, sha256)
/// is left untouched.
pub const ASSET_COMMIT: &str = "INSERT INTO assets \
(workspace_id, site_id, id, sha256, byte_size, content_type, storage_backend, created_at, object_key) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?10 \
WHERE EXISTS (SELECT 1 FROM asset_reservations WHERE workspace_id = ?1 AND site_id = ?2 \
  AND id = ?9 AND kind = 'asset' AND expires_at > ?11 AND byte_size >= ?5) \
ON CONFLICT DO NOTHING";

/// ?1 workspace, ?2 site, ?3 sha256. The row plus its R2 key, for upload dedupe,
/// download and delete (the registry `ASSET_BY_SHA256` predates `object_key`).
pub const ASSET_GET: &str = "SELECT id, sha256, byte_size, content_type, storage_backend, object_key \
FROM assets WHERE workspace_id = ?1 AND site_id = ?2 AND sha256 = ?3";

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

/// ?1 workspace, ?2 site, ?3 sha256. The `trg_assets_after_delete` trigger removes the
/// asset's chunks and tombstones its R2 key in the same transaction.
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

/// ?1 workspace, ?2 exact R2 object key. Is any asset row referencing this key?
pub const ASSET_OBJECT_REFERENCED: &str = "SELECT 1 AS referenced FROM assets \
WHERE workspace_id = ?1 AND object_key = ?2";

/// ?1 workspace, ?2 batch size. R2 keys of deleted rows whose objects are still to be removed.
pub const TOMBSTONES_LIST: &str = "SELECT object_key FROM r2_tombstones WHERE workspace_id = ?1 ORDER BY created_at, object_key LIMIT ?2";

/// ?1 workspace, ?2 object key. Clear a tombstone after its object is gone.
pub const TOMBSTONE_CLEAR: &str =
    "DELETE FROM r2_tombstones WHERE workspace_id = ?1 AND object_key = ?2";

// --- Generated preview/export artifacts -------------------------------------

/// ?1 workspace, ?2 site, ?3 reservation id, ?4 kind, ?5 bytes, ?6 now, ?7 expires_at,
/// ?8 per-site cap for this kind, ?9 workspace-wide generated cap, ?10 per-artifact
/// cap for this kind, ?11 database-wide blob byte cap. Only UNEXPIRED artifacts and
/// reservations count toward the generated budgets (an artifact past `expires_at` is
/// pending deletion and no longer holds budget; the database-wide sum still counts
/// every row, because expired rows still occupy space until swept).
pub const RESERVE_GENERATED: &str = "INSERT INTO asset_reservations \
(workspace_id, site_id, id, kind, byte_size, created_at, expires_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
WHERE ?5 <= ?10 \
AND (SELECT COALESCE(SUM(byte_size), 0) FROM generated_artifacts \
    WHERE workspace_id = ?1 AND site_id = ?2 AND kind = ?4 AND expires_at > ?6) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
    WHERE workspace_id = ?1 AND site_id = ?2 AND kind = ?4 AND expires_at > ?6) + ?5 <= ?8 \
AND (SELECT COALESCE(SUM(byte_size), 0) FROM generated_artifacts \
    WHERE workspace_id = ?1 AND expires_at > ?6) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations \
    WHERE workspace_id = ?1 AND kind IN ('preview', 'export') AND expires_at > ?6) + ?5 <= ?9 \
AND (SELECT COALESCE(SUM(byte_size), 0) FROM assets WHERE storage_backend = 'd1') \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM generated_artifacts) \
 + (SELECT COALESCE(SUM(byte_size), 0) FROM asset_reservations WHERE expires_at > ?6) + ?5 <= ?11";

/// ?1 workspace, ?2 site, ?3 artifact id, ?4 kind, ?5 sha256, ?6 bytes, ?7 backend,
/// ?8 created_at, ?9 expires_at, ?10 reservation id, ?11 commit time, ?12 retention
/// seconds for this kind. Commits only while the reservation is live AT COMMIT TIME
/// (`?11`, read immediately before the commit batch) and only with an `expires_at`
/// within `?12` seconds of `created_at`; [`generated_commit`] clamps it first.
pub const GENERATED_COMMIT: &str = "INSERT INTO generated_artifacts \
(workspace_id, site_id, id, kind, sha256, byte_size, storage_backend, created_at, expires_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
WHERE ?9 <= strftime('%Y-%m-%dT%H:%M:%SZ', ?8, '+' || ?12 || ' seconds') \
AND EXISTS (SELECT 1 FROM asset_reservations WHERE workspace_id = ?1 AND site_id = ?2 \
  AND id = ?10 AND kind = ?4 AND expires_at > ?11 AND byte_size >= ?6) \
ON CONFLICT DO NOTHING";

/// ?1 workspace, ?2 site, ?3 now, ?4 batch size. Retention: past `expires_at`.
pub const GENERATED_EXPIRED_SWEEP: &str = "DELETE FROM generated_artifacts WHERE workspace_id = ?1 \
AND id IN (SELECT id FROM generated_artifacts WHERE workspace_id = ?1 AND site_id = ?2 \
AND expires_at <= ?3 LIMIT ?4)";

/// ?1 workspace, ?2 site, ?3 kind, ?4 keep-latest count, ?5 batch size. Retention: all
/// but the newest N, at most `?5` rows per call.
pub const GENERATED_EXCESS_SWEEP: &str = "DELETE FROM generated_artifacts WHERE workspace_id = ?1 \
AND id IN (SELECT id FROM generated_artifacts WHERE workspace_id = ?1 AND site_id = ?2 \
AND kind = ?3 AND id NOT IN (SELECT id FROM generated_artifacts \
WHERE workspace_id = ?1 AND site_id = ?2 AND kind = ?3 ORDER BY created_at DESC, id DESC LIMIT ?4) LIMIT ?5)";

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
    ("ASSET_GET", ASSET_GET, true),
    ("ASSET_COMMIT", ASSET_COMMIT, true),
    ("CHUNK_INSERT", CHUNK_INSERT, true),
    ("CHUNK_GET", CHUNK_GET, true),
    ("CHUNK_COUNT", CHUNK_COUNT, true),
    ("ASSET_DELETE", ASSET_DELETE, true),
    ("RESERVATIONS_SWEEP", RESERVATIONS_SWEEP, true),
    ("CHUNKS_ORPHAN_SWEEP", CHUNKS_ORPHAN_SWEEP, true),
    ("ASSETS_INCOMPLETE_SWEEP", ASSETS_INCOMPLETE_SWEEP, true),
    ("ASSET_OBJECT_REFERENCED", ASSET_OBJECT_REFERENCED, true),
    ("TOMBSTONES_LIST", TOMBSTONES_LIST, true),
    ("TOMBSTONE_CLEAR", TOMBSTONE_CLEAR, true),
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
            int(limits.max_database_bytes),
        ],
    }
}

pub fn asset_get(ws: &str, site: &str, sha256: &str) -> Statement {
    Statement {
        sql: ASSET_GET,
        params: vec![t(ws), t(site), t(sha256)],
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
    created_at: &str,
    reservation_id: &str,
    object_key: Option<&str>,
    commit_now: &str,
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
            t(created_at),
            t(reservation_id),
            object_key.map_or(Param::Null, t),
            t(commit_now),
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

pub fn asset_object_referenced(ws: &str, object_key: &str) -> Statement {
    Statement {
        sql: ASSET_OBJECT_REFERENCED,
        params: vec![t(ws), t(object_key)],
    }
}

pub fn tombstones_list(ws: &str, batch: u32) -> Statement {
    Statement {
        sql: TOMBSTONES_LIST,
        params: vec![t(ws), Param::Int(i64::from(batch))],
    }
}

pub fn tombstone_clear(ws: &str, object_key: &str) -> Statement {
    Statement {
        sql: TOMBSTONE_CLEAR,
        params: vec![t(ws), t(object_key)],
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
            int(kind.policy().max_artifact_bytes),
            int(MAX_DATABASE_BLOB_BYTES),
        ],
    }
}

/// Commit a generated artifact. `expires_at` is clamped to `created_at` + the kind's
/// retention, so a caller can never keep an artifact longer than policy allows (the
/// SQL refuses it as well). `commit_now` MUST be a clock read immediately before the
/// commit batch, not the request-start time.
#[allow(clippy::too_many_arguments)]
pub fn generated_commit(
    ws: &str,
    site: &str,
    artifact_id: &str,
    kind: GeneratedKind,
    sha256: &str,
    bytes: u64,
    created_at: u64,
    requested_expires_at: u64,
    reservation_id: &str,
    commit_now: u64,
) -> Statement {
    let retention = kind.policy().retention_secs;
    let expires_at = requested_expires_at.min(created_at.saturating_add(retention));
    Statement {
        sql: GENERATED_COMMIT,
        params: vec![
            t(ws),
            t(site),
            t(artifact_id),
            t(kind.as_str()),
            t(sha256),
            int(bytes),
            t("d1"),
            t(&utc_text(created_at)),
            t(&utc_text(expires_at)),
            t(reservation_id),
            t(&utc_text(commit_now)),
            int(retention),
        ],
    }
}

pub fn generated_expired_sweep(ws: &str, site: &str, now: &str, batch: u32) -> Statement {
    Statement {
        sql: GENERATED_EXPIRED_SWEEP,
        params: vec![t(ws), t(site), t(now), Param::Int(i64::from(batch))],
    }
}

pub fn generated_excess_sweep(ws: &str, site: &str, kind: GeneratedKind, batch: u32) -> Statement {
    Statement {
        sql: GENERATED_EXCESS_SWEEP,
        params: vec![
            t(ws),
            t(site),
            t(kind.as_str()),
            Param::Int(i64::from(kind.policy().keep_latest)),
            Param::Int(i64::from(batch)),
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
            asset_get("w", "s", "h"),
            usage("w", "s", "n"),
            reservation_release("w", "s", "r"),
            reservation_delete("w", "r"),
            asset_commit(
                "w",
                "s",
                "a",
                "h",
                1,
                "image/png",
                "d1",
                "n",
                "r",
                None,
                "n",
            ),
            chunk_insert("w", "a", 0, &[1, 2]),
            chunk_get("w", "a", 0),
            asset_delete("w", "s", "h"),
            reservations_sweep("w", "n", 10),
            chunks_orphan_sweep("w", 10),
            assets_incomplete_sweep("w", "s", 10),
            asset_object_referenced("w", "k"),
            tombstones_list("w", 10),
            tombstone_clear("w", "k"),
            reserve_generated("w", "s", "r", GeneratedKind::Preview, 5, "n", "e"),
            generated_expired_sweep("w", "s", "n", 10),
            generated_excess_sweep("w", "s", GeneratedKind::Export, 10),
            generated_commit(
                "w",
                "s",
                "g",
                GeneratedKind::Preview,
                "h",
                1,
                100,
                200,
                "r",
                100,
            ),
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

    #[test]
    fn generated_commit_clamps_expiry_to_the_kind_retention() {
        let kind = GeneratedKind::Preview;
        let r = kind.policy().retention_secs;
        let created = 1_795_737_600;
        let st = generated_commit(
            "w",
            "s",
            "g",
            kind,
            "h",
            1,
            created,
            created + r * 10,
            "r",
            created,
        );
        assert_eq!(st.params[8], Param::Text(utc_text(created + r)));
        let st = generated_commit(
            "w",
            "s",
            "g",
            kind,
            "h",
            1,
            created,
            created + 5,
            "r",
            created,
        );
        assert_eq!(st.params[8], Param::Text(utc_text(created + 5)));
        assert_eq!(st.params[11], Param::Int(r as i64));
    }
}
