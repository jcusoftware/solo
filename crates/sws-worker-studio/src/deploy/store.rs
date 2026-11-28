//! Persistence seam for publish targets, the activation lease, retained verified
//! versions and the audit log (`migrations/0005_publish_targets.sql`).
//!
//! The D1 implementation (`publish::wasm::D1Store`) runs the statements in
//! [`super::sql`] (SQL-tested with node:sqlite in `tests/unit/studio-publish-sql.test.mjs`);
//! host tests use an in-memory implementation with the same conditional semantics.

use serde::Deserialize;

use crate::model::Role;

/// Lease length. Renewed immediately before every activation request; each request is
/// bounded well below it ([`super::api::ACTIVATION_TIMEOUT_MS`]).
pub const LEASE_SECS: u64 = 120;

/// One publish target (one site Worker on one Cloudflare account) per site.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Target {
    pub workspace_id: String,
    pub site_id: String,
    pub id: String,
    pub account_id: String,
    pub script_name: String,
    /// NAME of the Worker secret holding the API token (never the value).
    pub token_secret: String,
    /// Non-secret bindings (JSON array), validated by [`super::bindings`].
    pub bindings_json: String,
    pub compatibility_date: String,
    /// The site D1 migration level currently applied to this target's database.
    pub site_schema_level: i64,
    pub live_version_id: Option<String>,
    pub live_published_id: Option<String>,
    pub live_revision_id: Option<String>,
    /// Highest revision seq ever activated here (rollback does not lower it).
    pub high_water_seq: i64,
    pub activating_job: Option<String>,
    pub activating_seq: Option<i64>,
    pub activating_version_id: Option<String>,
    pub lease_expires_at: Option<String>,
}

impl Target {
    /// Held by someone and not yet expired at `now` (canonical UTC text compares
    /// lexicographically).
    pub fn lease_live(&self, now: &str) -> bool {
        self.activating_job.is_some() && self.lease_expires_at.as_deref().is_some_and(|e| e > now)
    }

    pub fn lease_stale(&self, now: &str) -> bool {
        self.activating_job.is_some() && !self.lease_live(now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseClaim {
    pub job: String,
    /// `None` for a rollback (no revision order).
    pub seq: Option<i64>,
    pub version_id: String,
    pub until: String,
    pub now: String,
}

/// A retained, verified deployment: a Cloudflare version Solo activated and then saw
/// serving 100% of traffic. Rows are immutable; they are the only rollback targets.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PublishedVersion {
    pub workspace_id: String,
    pub site_id: String,
    pub id: String,
    pub target_id: String,
    pub cf_version_id: String,
    pub revision_id: String,
    pub revision_seq: i64,
    pub job_id: Option<String>,
    /// Site D1 migration level the version's Worker was built for.
    pub schema_compat: i64,
    pub manifest_sha256: String,
    pub verified_at: String,
    pub created_by: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    PublishActivate,
    Rollback,
    LeaseTakeover,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditAction::PublishActivate => "publish_activate",
            AuditAction::Rollback => "rollback",
            AuditAction::LeaseTakeover => "lease_takeover",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditResult {
    Succeeded,
    Failed,
    Refused,
}

impl AuditResult {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditResult::Succeeded => "succeeded",
            AuditResult::Failed => "failed",
            AuditResult::Refused => "refused",
        }
    }
}

/// Append-only audit row. Carries ids and codes only: never a token, JWT or body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    pub workspace_id: String,
    pub site_id: String,
    pub target_id: String,
    /// `None` = the system (a publish job without an interactive actor is still
    /// attributed to `requested_by`; `None` is used for lease takeovers).
    pub actor_user_id: Option<i64>,
    pub action: AuditAction,
    pub result: AuditResult,
    pub cf_version_id: Option<String>,
    pub published_id: Option<String>,
    pub revision_id: Option<String>,
    pub job_id: Option<String>,
    pub stage: Option<&'static str>,
    pub error_code: Option<&'static str>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

/// D1 operations. Every conditional write returns whether it applied, so lease loss
/// and ordering races surface as `false`, never as a silent overwrite.
#[allow(async_fn_in_trait)]
pub trait PublishStore {
    async fn target(&self, ws: &str, site: &str) -> Result<Option<Target>, StoreError>;
    async fn role(&self, ws: &str, user_id: i64) -> Result<Option<Role>, StoreError>;
    /// Take a FREE lease. A publish claim (`seq` set) applies only while
    /// `high_water_seq < seq`; stale leases must be cleared first ([`Self::clear_stale`]).
    async fn acquire(&self, t: &Target, c: &LeaseClaim) -> Result<bool, StoreError>;
    /// Extend a lease the job still holds and that has not expired.
    async fn renew(
        &self,
        t: &Target,
        job: &str,
        until: &str,
        now: &str,
    ) -> Result<bool, StoreError>;
    async fn release(&self, t: &Target, job: &str) -> Result<(), StoreError>;
    /// Clear an EXPIRED lease held by `stale_job`. With `adopt`, first record the
    /// stale holder's version as live (reconciliation proved it is serving) and raise
    /// `high_water_seq` to its seq.
    async fn clear_stale(
        &self,
        t: &Target,
        stale_job: &str,
        now: &str,
        adopt: bool,
    ) -> Result<bool, StoreError>;
    /// Atomically: set the target live (only while `job` holds the lease and the seq is
    /// above the high-water mark), insert the retained version (only if the target
    /// update applied), release the lease, append `audit`. Returns whether the target
    /// update applied.
    async fn record_publish(
        &self,
        t: &Target,
        job: &str,
        v: &PublishedVersion,
        audit: &AuditRow,
    ) -> Result<bool, StoreError>;
    /// Atomically: set the target live to a retained version (lease held; high-water
    /// mark unchanged), release the lease, append `audit`.
    async fn record_rollback(
        &self,
        t: &Target,
        job: &str,
        v: &PublishedVersion,
        audit: &AuditRow,
    ) -> Result<bool, StoreError>;
    async fn published(
        &self,
        ws: &str,
        site: &str,
        id: &str,
    ) -> Result<Option<PublishedVersion>, StoreError>;
    async fn audit(&self, row: &AuditRow) -> Result<(), StoreError>;
}
