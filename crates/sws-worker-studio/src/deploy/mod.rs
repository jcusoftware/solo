//! Publish compiled sites to the standalone edge (site) Worker through Cloudflare's
//! Workers static-assets Direct Upload API (#66).
//!
//! # API contract (Cloudflare docs read on 2026-11-27)
//!
//! - <https://developers.cloudflare.com/workers/static-assets/direct-upload/>
//! - <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/assets/subresources/upload/methods/create/>
//! - <https://developers.cloudflare.com/api/resources/workers/subresources/assets/subresources/upload/methods/create/>
//! - <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/versions/methods/create/>
//! - <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/methods/update/> (`assets.config`)
//! - <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/create/>
//! - <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/list/>
//! - <https://developers.cloudflare.com/workers/configuration/multipart-upload-metadata/>
//! - <https://developers.cloudflare.com/workers/static-assets/headers/> (`_headers` is parsed, not served)
//! - <https://developers.cloudflare.com/workers/versions-and-deployments/rollbacks/>
//! - <https://developers.cloudflare.com/workers/platform/limits/#static-assets>
//!
//! Sequence ([`pipeline::run_publish`]):
//!
//! 1. `POST /accounts/{account}/workers/scripts/{script}/assets-upload-session` with the
//!    full manifest (`{path: {hash, size}}`, every path the version needs). The answer
//!    is an upload JWT plus `buckets` of hashes Cloudflare does NOT already hold; empty
//!    buckets mean the JWT is already the completion token (dedup).
//! 2. For each bucket: `POST /accounts/{account}/workers/assets/upload?base64=true`,
//!    `multipart/form-data`, part name = hash, base64 body, `Authorization: Bearer
//!    <upload JWT>` (never the account token). The last upload answers 201 + completion JWT.
//! 3. `POST /accounts/{account}/workers/scripts/{script}/versions` (multipart: JSON
//!    `metadata` + Worker modules) with `assets.jwt` = completion token,
//!    `assets.config.run_worker_first = ["/api/*"]`, the target's D1 / vars bindings and
//!    `keep_bindings: ["secret_text", "secret_key"]` (Turnstile and other secrets stay
//!    on the Worker; Solo never sees them). This creates a version WITHOUT deploying it.
//! 4. Only now, under the per-target D1 lease and the revision-order check:
//!    `POST /accounts/{account}/workers/scripts/{script}/deployments`
//!    (`strategy: "percentage"`, the version at 100%), then
//!    `GET .../deployments` to verify it is first ("the latest deployment actively
//!    serving traffic").
//!
//! Asset hash: first 32 hex chars of `sha256(base64(content) + extension)` (extension
//! without the dot), as in Cloudflare's programmatic example on the Direct Upload page.
//!
//! Assumptions the docs did not settle, flagged for the human publish check (#69):
//! the Version Upload endpoint accepts the same `assets.config` keys (`run_worker_first`,
//! `_headers`, `_redirects`, `html_handling`, `not_found_handling`) that the Script Upload
//! reference lists (the multipart-metadata page lists `assets` for both); upload parts
//! carry `filename` = hash and the file's `Content-Type`, which Cloudflare stores for
//! serving; `keep_bindings` keeps secrets from the latest uploaded version.
//!
//! # Safety properties
//!
//! - Nothing touches live traffic before step 4, so any earlier failure leaves the last
//!   known good version serving and is reported as [`PublishFailure`] with its
//!   [`Stage`] and revision.
//! - Idempotent steps (1, 2, 3, the deployments list) retry 429 / 5xx / transport errors
//!   with bounded backoff ([`api::RetryPolicy`]). Activation is NEVER blindly retried: an
//!   ambiguous outcome (timeout, transport error, 429, 5xx) is reconciled against the
//!   deployments list first ([`pipeline`]).
//! - Activation is serialized per target by a D1 lease (`publish_targets.activating_job`)
//!   and ordered by the immutable revision sequence (`high_water_seq`): an older job can
//!   never activate over a newer one, including after a rollback.
//! - The API token is a Worker secret resolved by name ([`http::Secrets`]), sent only to
//!   the account endpoints, and redacted from every error and log line ([`secret`]).
//!
//! # Seams
//!
//! - #64 (publish jobs) calls [`pipeline::run_publish`] from its uploading/activation
//!   phase with a [`PublishJobRef`] (immutable revision pin) and an
//!   [`pipeline::ArtifactSource`] (its rendered output plus the site Worker bundle).
//!   No queueing lives here.
//! - #65 (auth) extends [`rollback::require_activate`] (scopes, 15-minute re-auth) and
//!   replaces [`crate::auth::authenticate`]; the rollback route answers 401 until then.

pub mod api;
pub mod bindings;
pub mod http;
pub mod manifest;
pub mod multipart;
pub mod pipeline;
pub mod rollback;
pub mod route;
pub mod secret;
pub mod sql;
pub mod store;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

#[cfg(test)]
mod memstore;
#[cfg(test)]
mod tests;

pub use pipeline::{ArtifactSource, PublishEnv, run_publish};
pub use rollback::{
    RollbackError, RollbackOutcome, RollbackRequest, require_activate, run_rollback,
};

/// Production Cloudflare API base. The wasm glue always uses this constant (no
/// override), so a configuration change can never send the token elsewhere.
pub const CLOUDFLARE_API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Default Worker secret holding the Cloudflare API token.
pub const DEFAULT_TOKEN_SECRET: &str = "CLOUDFLARE_API_TOKEN";

/// Where a publish (or rollback activation) stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Input validation, revision order, schema and artifact pin checks. No API call.
    Prepare,
    UploadSession,
    UploadAssets,
    CreateVersion,
    /// Acquiring or keeping the per-target activation lease.
    Lock,
    Activate,
    /// Post-activation check that the version is the one serving 100% of traffic.
    Verify,
    /// Recording the activation in D1.
    Record,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Prepare => "prepare",
            Stage::UploadSession => "upload_session",
            Stage::UploadAssets => "upload_assets",
            Stage::CreateVersion => "create_version",
            Stage::Lock => "lock",
            Stage::Activate => "activate",
            Stage::Verify => "verify",
            Stage::Record => "record",
        }
    }

    /// True for stages that can never have changed live traffic.
    pub fn pre_activation(self) -> bool {
        matches!(
            self,
            Stage::Prepare
                | Stage::UploadSession
                | Stage::UploadAssets
                | Stage::CreateVersion
                | Stage::Lock
        )
    }
}

/// What a publish job (#64) hands to [`run_publish`]. The revision is immutable
/// (`site_revisions` rows never change), so this pins exactly what is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishJobRef {
    pub workspace_id: String,
    pub site_id: String,
    /// Job id (1..64 chars of `[a-z0-9_-]`); the activation lease holder.
    pub job_id: String,
    pub revision_id: String,
    /// `site_revisions.revision` (monotonic per site): the activation order key.
    pub revision_seq: i64,
    /// `users.id` of the member who requested the publish (audit actor).
    pub requested_by: i64,
    /// Digest the job recorded when it rendered the revision; the artifacts must still
    /// hash to it ([`manifest::Manifest::digest`]). `None` skips the check.
    pub artifact_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishOutcome {
    pub target_id: String,
    pub cf_version_id: String,
    /// `published_versions.id`: the retained, verified deployment (rollback handle).
    pub published_id: String,
    pub revision_id: String,
    pub revision_seq: i64,
    pub manifest_sha256: String,
    /// Files sent to Cloudflare in this publish (the rest were deduplicated).
    pub uploaded_files: usize,
    pub total_files: usize,
}

/// Whether live traffic may have changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// Failed before any activation request.
    NotAttempted,
    /// Cloudflare rejected the activation, or reconciliation proved it did not apply.
    NotActivated,
    /// The outcome could not be confirmed either way. The lease is kept until it
    /// expires, so the next holder reconciles with Cloudflare before acting.
    Unknown,
    /// The version went live (failure came after, e.g. verify or record).
    Activated,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::NotAttempted => "not_attempted",
            Activation::NotActivated => "not_activated",
            Activation::Unknown => "unknown",
            Activation::Activated => "activated",
        }
    }
}

/// A failed publish: the stage and the pinned revision, plus whether live traffic can
/// have changed. `message` is redacted (no token, no upload JWT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishFailure {
    pub stage: Stage,
    pub revision_id: String,
    pub revision_seq: i64,
    pub code: &'static str,
    pub message: String,
    /// Safe for the job runner to retry the whole publish later.
    pub retryable: bool,
    pub activation: Activation,
    /// The target's last known good version per D1 when the failure was reported.
    pub live_version_id: Option<String>,
}

impl std::fmt::Display for PublishFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "publish of revision {} (#{}) failed at {}: {} ({}; activation {})",
            self.revision_id,
            self.revision_seq,
            self.stage.as_str(),
            self.code,
            self.message,
            self.activation.as_str()
        )
    }
}
