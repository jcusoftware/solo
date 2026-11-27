//! Provisional storage limits. Every number here is an assumption to re-verify on a
//! fresh Cloudflare account before the cardless envelope is advertised (#69).

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * KIB;
pub const GIB: u64 = 1024 * MIB;

/// Bytes per D1 blob row. Assumption: a D1 row/string/BLOB value tops out around
/// 2 MB, so a 256 KiB chunk keeps every row an order of magnitude below it.
pub const CHUNK_BYTES: usize = 256 * 1024;
/// The assumed D1 single-row/value ceiling that [`CHUNK_BYTES`] must stay well under.
pub const D1_ROW_LIMIT_ASSUMED_BYTES: u64 = 2 * MIB;

/// Cardless envelope: pages per site (enforced by the compiler/IR in #69, recorded here).
pub const MAX_PAGES_PER_SITE: u32 = 20;

/// A reservation that is not committed or released within this window stops
/// counting against quota and is swept by reconciliation.
pub const RESERVATION_TTL_SECS: u64 = 600;
/// Isolate memory guard. A Worker isolate (assumed 128 MB limit) admits at most this
/// many bytes of upload working memory at once, where each upload is charged
/// [`crate::quota::upload_memory_cost`] (its buffered copies, not just the body
/// size). Further uploads get `503 upload_busy` + `Retry-After` BEFORE any
/// reservation is written.
pub const MAX_INFLIGHT_UPLOAD_BYTES: u64 = 12 * MIB;
/// Same guard for downloads: concurrent in-flight downloads on one isolate. Each holds
/// at most one [`CHUNK_BYTES`] chunk (D1) or a bounded stream window (R2), so
/// `MAX_INFLIGHT_DOWNLOADS * CHUNK_BYTES` (4 MiB) bounds their working memory.
pub const MAX_INFLIGHT_DOWNLOADS: u32 = 16;
/// Assumption: a D1 database tops out around 500 MB on the free plan.
pub const D1_DATABASE_LIMIT_ASSUMED_BYTES: u64 = 500 * MIB;
/// Database-wide cap on blob bytes held IN D1 (D1-backed assets, generated artifacts
/// and live reservations, summed across every workspace). It keeps headroom under
/// [`D1_DATABASE_LIMIT_ASSUMED_BYTES`] for registry rows and indexes, so many
/// workspaces at their own caps cannot fill the whole database. Provisional.
pub const MAX_DATABASE_BLOB_BYTES: u64 = 400 * MIB;
/// Upper bound on rows touched by one reconciliation pass.
pub const RECONCILE_BATCH: u32 = 100;

/// Bounds for uploaded (user-owned) assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetLimits {
    /// Largest single asset.
    pub max_asset_bytes: u64,
    /// Stored plus reserved bytes per site.
    pub max_site_bytes: u64,
    /// Stored plus reserved bytes across the whole workspace.
    pub max_workspace_bytes: u64,
    /// Stored plus reserved asset count per site.
    pub max_site_assets: u64,
    /// Blob bytes allowed in the whole D1 database across every workspace
    /// (`u64::MAX` for backends whose bytes do not live in D1).
    pub max_database_bytes: u64,
}

/// Cardless default: D1 blobs only.
pub const CARDLESS_D1: AssetLimits = AssetLimits {
    max_asset_bytes: MIB,
    max_site_bytes: 50 * MIB,
    max_workspace_bytes: 150 * MIB,
    max_site_assets: 500,
    max_database_bytes: MAX_DATABASE_BLOB_BYTES,
};

/// Opt-in R2 (needs a bucket and, on Cloudflare, a payment method on file).
pub const R2_OPT_IN: AssetLimits = AssetLimits {
    max_asset_bytes: 5 * MIB,
    max_site_bytes: 500 * MIB,
    max_workspace_bytes: 2 * GIB,
    max_site_assets: 5000,
    // R2 bytes live in the bucket, not in the D1 database.
    max_database_bytes: u64::MAX,
};

/// Offline local filesystem store: same envelope as the cardless default.
pub const LOCAL: AssetLimits = AssetLimits {
    max_database_bytes: u64::MAX,
    ..CARDLESS_D1
};

/// Generated (preview/export) artifact kinds, budgeted separately from assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeneratedKind {
    Preview,
    Export,
}

impl GeneratedKind {
    pub const ALL: [GeneratedKind; 2] = [GeneratedKind::Preview, GeneratedKind::Export];

    pub fn as_str(self) -> &'static str {
        match self {
            GeneratedKind::Preview => "preview",
            GeneratedKind::Export => "export",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "preview" => Some(GeneratedKind::Preview),
            "export" => Some(GeneratedKind::Export),
            _ => None,
        }
    }

    pub fn policy(self) -> GeneratedPolicy {
        match self {
            GeneratedKind::Preview => PREVIEW,
            GeneratedKind::Export => EXPORT,
        }
    }
}

/// Budget and retention for one generated-artifact kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedPolicy {
    pub max_artifact_bytes: u64,
    /// Stored plus reserved bytes of this kind per site.
    pub max_site_bytes: u64,
    /// Artifacts older than this are deleted by reconciliation.
    pub retention_secs: u64,
    /// At most this many artifacts of this kind are kept per site (newest win).
    pub keep_latest: u32,
}

pub const PREVIEW: GeneratedPolicy = GeneratedPolicy {
    max_artifact_bytes: 5 * MIB,
    max_site_bytes: 10 * MIB,
    retention_secs: 24 * 3600,
    keep_latest: 3,
};

pub const EXPORT: GeneratedPolicy = GeneratedPolicy {
    max_artifact_bytes: 10 * MIB,
    max_site_bytes: 20 * MIB,
    retention_secs: 7 * 24 * 3600,
    keep_latest: 2,
};

/// Generated artifacts of every kind and site in one workspace. Separate from, and
/// never traded against, [`AssetLimits::max_workspace_bytes`].
pub const MAX_WORKSPACE_GENERATED_BYTES: u64 = 40 * MIB;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_stay_far_below_the_assumed_row_limit() {
        assert!((CHUNK_BYTES as u64) * 4 <= D1_ROW_LIMIT_ASSUMED_BYTES);
        assert!((256 * KIB..=512 * KIB).contains(&(CHUNK_BYTES as u64)));
    }

    #[test]
    fn envelope_matches_the_issue() {
        assert_eq!(CARDLESS_D1.max_asset_bytes, MIB);
        assert_eq!(CARDLESS_D1.max_site_bytes, 50 * MIB);
        assert_eq!(MAX_PAGES_PER_SITE, 20);
    }

    #[test]
    fn database_cap_leaves_headroom_and_exceeds_one_workspace() {
        const {
            assert!(MAX_DATABASE_BLOB_BYTES < D1_DATABASE_LIMIT_ASSUMED_BYTES);
            assert!(MAX_DATABASE_BLOB_BYTES >= CARDLESS_D1.max_workspace_bytes);
            assert!(MAX_DATABASE_BLOB_BYTES >= MAX_WORKSPACE_GENERATED_BYTES);
        }
        assert_eq!(R2_OPT_IN.max_database_bytes, u64::MAX);
    }

    #[test]
    fn limits_are_internally_consistent() {
        for l in [CARDLESS_D1, R2_OPT_IN] {
            assert!(l.max_asset_bytes <= l.max_site_bytes);
            assert!(l.max_site_bytes <= l.max_workspace_bytes);
        }
        for k in GeneratedKind::ALL {
            let p = k.policy();
            assert!(p.max_artifact_bytes <= p.max_site_bytes);
            assert!(p.max_site_bytes <= MAX_WORKSPACE_GENERATED_BYTES);
            assert_eq!(GeneratedKind::parse(k.as_str()), Some(k));
        }
    }
}
