//! Asynchronous publish jobs (#64).
//!
//! Every publish runs as a job against an immutable `site_revisions` row. The HTTP
//! handler validates, writes the job row and its artifact manifest, and answers
//! `202 {job_id}`; it never compiles. Jobs are driven off the request path by
//! [`drive`] (wasm), from two entry points that call the same function:
//!
//! * the Cron Trigger (`#[event(scheduled)]`), which needs no Queue and is therefore
//!   the Workers Free default: it recovers jobs that are due (queued, backing off, or
//!   whose driver's lease expired) and drives at most [`MAX_JOBS_PER_RUN`] per run;
//! * an OPTIONAL Queue consumer (`#[event(queue)]`), used only when a `PUBLISH_QUEUE`
//!   producer binding exists; the message is a [`QueuePointer`] well under 1 KiB.
//!
//! Pure, host-tested modules: [`job`] (state machine), [`manifest`] (CLI artifact
//! schema and validation), [`sql`] (statements); this file holds routing, the queue
//! pointer, phase-step decisions and response shapes. Authorization for every
//! publish route goes through the one seam `authz::require_publish`, which applies
//! the #65 policy through [`authorize_publish`]. Server-side compilation is not implemented (maintainer
//! decision on #59: the CLI compile-and-upload path is the v0.2.0 default); a
//! `server` job fails in `compiling`. Upload to Cloudflare and activation are #66:
//! the `uploading` phase is the seam (`drive::stage_for_activation`) and today only
//! marks the validated artifact ready to publish.

pub mod job;
pub mod manifest;
pub mod sql;

#[cfg(target_arch = "wasm32")]
pub mod authz;
#[cfg(target_arch = "wasm32")]
pub mod drive;
#[cfg(target_arch = "wasm32")]
pub mod http;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sws_assets::hash::sha256_hex;
use sws_assets::limits::ArtifactLimits;
use sws_assets::time::{parse_utc_text, utc_text};
use sws_assets::validate::is_id;

use crate::auth::Credential;
use crate::auth::policy::{self, Permission};
use crate::model::Role;
use crate::respond::Resp;
use job::{JobState, MAX_ATTEMPTS, Origin, Phase, StepError};

/// Optional Queue producer binding. Absent (the cardless default): the Cron Trigger
/// alone drives jobs.
pub const QUEUE_BINDING: &str = "PUBLISH_QUEUE";
/// Var that would enable server-side compilation. It is not implemented, so a
/// `server` job fails either way, with a code that says which.
pub const SERVER_COMPILE_VAR: &str = "STUDIO_SERVER_COMPILE";
/// Hard cap on a serialized queue message (the acceptance criterion is "sub-1 KB").
pub const MAX_POINTER_BYTES: usize = 1024;

/// Jobs one scheduled run drives.
pub const MAX_JOBS_PER_RUN: u32 = 5;
/// D1 queries one CLI job costs in the worst case: read, claim, advance out of
/// `queued`, artifact + revision reads and the advance out of `validating`, and the
/// advance out of `uploading`.
pub const QUERIES_PER_JOB: u32 = 7;
/// Per-run queries outside the jobs: the due scan and the retention prune.
pub const QUERIES_PER_RUN: u32 = 2;
/// Assumed Workers Free cap on D1 queries per invocation (provisional, #59/#69).
pub const ASSUMED_FREE_QUERIES_PER_INVOCATION: u32 = 50;

/// The publish decision for every publish route (enqueue and job status): the #65
/// policy with [`Permission::Publish`]. `role` is the caller's current membership in
/// `workspace_id` (`None` = not a member). Owner and publisher roles only; a token
/// must be bound to this workspace with scope `publish` or `activate`; a browser
/// session must be fresh (signed in within the 15-minute re-auth window).
pub fn authorize_publish(
    credential: &Credential,
    role: Option<Role>,
    workspace_id: &str,
) -> Result<(), policy::Denied> {
    policy::authorize(credential, role, workspace_id, Permission::Publish)
}

/// `STUDIO_SERVER_COMPILE=1`. Default off.
pub fn server_compile_enabled(var: Option<&str>) -> bool {
    var.map(str::trim) == Some("1")
}

/// Deterministic job id for (workspace, site, request id): a retried enqueue maps to
/// the same row. `j` + 47 hex chars (fits the registry id grammar).
pub fn job_id(workspace_id: &str, site_id: &str, request_id: &str) -> String {
    format!(
        "j{}",
        &sha256_hex(format!("publish\0{workspace_id}\0{site_id}\0{request_id}").as_bytes())[..47]
    )
}

/// The queue message: a pointer to the job, never the artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuePointer {
    pub v: u8,
    pub workspace_id: String,
    pub site_id: String,
    pub job_id: String,
    pub revision_id: String,
}

impl QueuePointer {
    pub fn new(workspace_id: &str, site_id: &str, job_id: &str, revision_id: &str) -> Self {
        Self {
            v: 1,
            workspace_id: workspace_id.into(),
            site_id: site_id.into(),
            job_id: job_id.into(),
            revision_id: revision_id.into(),
        }
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Parse a queue message body. Oversized, malformed, unknown-version or bad-id
/// messages are `None` (the consumer acknowledges and drops them).
pub fn parse_pointer(text: &str) -> Option<QueuePointer> {
    if text.len() > MAX_POINTER_BYTES {
        return None;
    }
    let p: QueuePointer = serde_json::from_str(text).ok()?;
    (p.v == 1
        && is_id(&p.workspace_id)
        && is_id(&p.site_id)
        && is_id(&p.job_id)
        && is_id(&p.revision_id))
    .then_some(p)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishRoute {
    /// `POST /api/sites/{ws}/{site}/publish`
    Enqueue { ws: String, site: String },
    /// `GET /api/sites/{ws}/{site}/publish/{job_id}`
    Status {
        ws: String,
        site: String,
        job: String,
    },
}

impl PublishRoute {
    pub fn allow(&self) -> &'static str {
        match self {
            PublishRoute::Enqueue { .. } => "POST",
            PublishRoute::Status { .. } => "GET",
        }
    }

    pub fn ids(&self) -> (&str, &str) {
        match self {
            PublishRoute::Enqueue { ws, site } | PublishRoute::Status { ws, site, .. } => {
                (ws, site)
            }
        }
    }
}

/// Match `/api/sites/{ws}/{site}/publish[/{job_id}]`; every id must satisfy the
/// registry grammar, otherwise it is not a route (404).
pub fn parse_route(path: &str) -> Option<PublishRoute> {
    let rest = path.strip_prefix("/api/sites/")?;
    let mut it = rest.split('/');
    let (ws, site, kw) = (it.next()?, it.next()?, it.next()?);
    if kw != "publish" || !is_id(ws) || !is_id(site) {
        return None;
    }
    let (ws, site) = (ws.to_string(), site.to_string());
    match (it.next(), it.next()) {
        (None, _) => Some(PublishRoute::Enqueue { ws, site }),
        (Some(job), None) if is_id(job) => Some(PublishRoute::Status {
            ws,
            site,
            job: job.to_string(),
        }),
        _ => None,
    }
}

// --- Phase steps (decisions; the wasm driver performs the reads). -------------

/// `compiling`: not implemented. Fails with `server_compile_disabled` by default, or
/// `server_compile_unimplemented` when the flag asks for it anyway.
pub fn compile_step(enabled: bool) -> Result<(), StepError> {
    Err(StepError::Permanent(if enabled {
        "server_compile_unimplemented"
    } else {
        "server_compile_disabled"
    }))
}

/// The artifact row as the driver read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredArtifact {
    pub manifest_sha256: String,
    pub revision_sha256: String,
    pub manifest: String,
}

impl StoredArtifact {
    pub fn from_row(row: &Value) -> Option<Self> {
        let s = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            manifest_sha256: s("manifest_sha256")?,
            revision_sha256: s("revision_sha256")?,
            manifest: s("manifest")?,
        })
    }
}

/// `validating`: off the request path, re-check that the immutable revision still
/// exists with the digest the artifact was compiled from, and that the stored
/// manifest is intact (canonical, same digest) and within today's limits.
/// `revision_ir_sha256` is `site_revisions.ir_sha256` for the job's revision.
pub fn validate_step(
    job_revision_id: &str,
    revision_ir_sha256: Option<&str>,
    artifact: Option<&StoredArtifact>,
    limits: &ArtifactLimits,
) -> Result<(), StepError> {
    let Some(ir) = revision_ir_sha256 else {
        return Err(StepError::Permanent("revision_missing"));
    };
    let Some(a) = artifact else {
        return Err(StepError::Permanent("artifact_missing"));
    };
    let m = manifest::revalidate(&a.manifest, &a.manifest_sha256, limits)
        .map_err(|_| StepError::Permanent("manifest_invalid"))?;
    if m.manifest.revision_id != job_revision_id
        || m.manifest.revision_sha256 != a.revision_sha256
        || a.revision_sha256 != ir
    {
        return Err(StepError::Permanent("revision_mismatch"));
    }
    Ok(())
}

// --- Rows and responses. -------------------------------------------------------

fn str_of<'a>(row: &'a Value, k: &str) -> Option<&'a str> {
    row.get(k).and_then(Value::as_str)
}

/// The state-machine columns of a `PUBLISH_JOB_GET` row. `None` for a row the
/// migration's CHECKs would never allow (the driver then leaves it alone).
pub fn state_from_row(row: &Value) -> Option<JobState> {
    let lease = match row.get("lease_expires_at") {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_utc_text(v.as_str()?)?),
    };
    Some(JobState {
        phase: Phase::parse(str_of(row, "phase")?)?,
        origin: Origin::parse(str_of(row, "origin")?)?,
        attempt: u32::try_from(row.get("attempt")?.as_u64()?).ok()?,
        next_attempt_at: parse_utc_text(str_of(row, "next_attempt_at")?)?,
        lease_expires_at: lease,
    })
}

/// Public JSON for a job row (status route and enqueue answer).
pub fn job_json(ws: &str, row: &Value) -> Value {
    let get = |k: &str| row.get(k).cloned().unwrap_or(Value::Null);
    let site = str_of(row, "site_id").unwrap_or_default();
    let id = str_of(row, "id").unwrap_or_default();
    let error = match str_of(row, "error_code") {
        Some(code) => json!({ "code": code, "phase": get("error_phase") }),
        None => Value::Null,
    };
    let artifact = if row.get("manifest_sha256").is_some_and(|v| !v.is_null()) {
        json!({
            "manifest_sha256": get("manifest_sha256"),
            "revision_sha256": get("revision_sha256"),
            "file_count": get("file_count"),
            "total_bytes": get("total_bytes"),
        })
    } else {
        Value::Null
    };
    json!({
        "job_id": id,
        "workspace_id": ws,
        "site_id": site,
        "revision_id": get("revision_id"),
        "request_id": get("request_id"),
        // `cli`: compiled by the publisher's own CLI. Studio validated the manifest,
        // limits and revision; it did not compile or independently verify the output.
        "origin": get("origin"),
        "server_verified": get("server_verified").as_i64() == Some(1),
        "phase": get("phase"),
        "attempt": get("attempt"),
        "max_attempts": MAX_ATTEMPTS,
        "next_attempt_at": get("next_attempt_at"),
        "error": error,
        "last_error": get("last_error"),
        "artifact": artifact,
        "created_at": get("created_at"),
        "updated_at": get("updated_at"),
        "finished_at": get("finished_at"),
        "status_url": format!("/api/sites/{ws}/{site}/publish/{id}"),
    })
}

/// What an enqueue amounted to, from the batch result and the row read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    Created,
    /// The same request id and manifest again: the existing job.
    Existing,
    /// The same request id with a different manifest or revision.
    Conflict,
    /// Refused by the per-site caps (no row was written).
    Busy,
}

/// Idempotency check BEFORE any other enqueue work: an existing job for the same
/// (workspace, site, request id) decides the answer on its own, so a reused request
/// id with another manifest is always `409 idempotency_conflict`, whatever that
/// manifest's revision is (missing, mismatched or valid). `None` = no such job yet.
pub fn existing_outcome(row: Option<&Value>, digest: &str, revision_id: &str) -> Option<Enqueued> {
    row.map(|r| enqueue_outcome(false, Some(r), digest, revision_id))
}

pub fn enqueue_outcome(
    inserted: bool,
    row: Option<&Value>,
    digest: &str,
    revision_id: &str,
) -> Enqueued {
    match row {
        None => Enqueued::Busy,
        Some(r)
            if str_of(r, "manifest_sha256") != Some(digest)
                || str_of(r, "revision_id") != Some(revision_id) =>
        {
            Enqueued::Conflict
        }
        Some(_) if inserted => Enqueued::Created,
        Some(_) => Enqueued::Existing,
    }
}

/// JSON error with the Studio header profile and `no-store`.
pub fn error(status: u16, code: &str, details: Option<Value>) -> Resp {
    let mut body = json!({ "ok": false, "error": code });
    if let (Some(d), Some(obj)) = (details, body.as_object_mut()) {
        obj.insert("details".into(), d);
    }
    Resp::new(status, Some(body), &[])
}

pub fn ok(status: u16, body: Value) -> Resp {
    Resp::new(status, Some(body), &[])
}

pub fn method_not_allowed(route: &PublishRoute) -> Resp {
    Resp::new(
        405,
        Some(json!({ "ok": false, "error": "method_not_allowed" })),
        &[("Allow", route.allow())],
    )
}

/// 400 for a refused manifest, naming the offending `files[]` entry when there is one.
pub fn manifest_error(e: &manifest::ManifestError) -> Resp {
    error(400, e.code(), e.index().map(|i| json!({ "index": i })))
}

/// 429 when the per-site caps refuse a new job.
pub fn busy() -> Resp {
    let mut r = error(
        429,
        "publish_busy",
        Some(json!({
            "max_active_jobs_per_site": sql::MAX_ACTIVE_JOBS_PER_SITE,
            "max_jobs_per_site_per_day": sql::MAX_JOBS_PER_SITE_PER_DAY,
        })),
    );
    r.headers.push(("Retry-After".into(), "60".into()));
    r
}

/// `202 Accepted` for a created or existing job.
pub fn accepted(ws: &str, row: &Value, created: bool, dispatch: &str) -> Resp {
    let mut body = job_json(ws, row);
    if let Some(o) = body.as_object_mut() {
        o.insert("ok".into(), json!(true));
        o.insert("created".into(), json!(created));
        o.insert("dispatch".into(), json!(dispatch));
    }
    ok(202, body)
}

/// Canonical text of `now` (re-exported for the glue).
pub fn now_text(now: u64) -> String {
    utc_text(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_assets::limits::PUBLISH_ARTIFACT;

    const H: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const T0: u64 = 1_795_737_600;

    #[test]
    fn the_queue_pointer_is_well_under_1_kib_even_at_maximum_id_lengths() {
        let max = "a".repeat(64);
        let p = QueuePointer::new(&max, &max, &max, &max);
        let text = p.to_text();
        assert!(text.len() < 1024, "{} bytes", text.len());
        assert!(text.len() <= MAX_POINTER_BYTES);
        assert_eq!(parse_pointer(&text), Some(p));
        // A real pointer carries ids and nothing else: no manifest, no document.
        let real = QueuePointer::new(
            "ws-a",
            "site-1",
            &job_id("ws-a", "site-1", "req-1"),
            "rev-1",
        )
        .to_text();
        assert!(real.len() < 200, "{real}");
        for k in ["manifest", "files", "document", "sha256"] {
            assert!(!real.contains(k), "{k} in {real}");
        }
    }

    #[test]
    fn hostile_queue_messages_are_dropped() {
        let good = QueuePointer::new("ws", "s", "j", "r").to_text();
        assert!(parse_pointer(&good).is_some());
        for bad in [
            String::new(),
            "null".into(),
            "{}".into(),
            good.replace("\"v\":1", "\"v\":2"),
            good.replace("\"ws\"", "\"WS\""),
            good.replace("\"s\"", "\"../x\""),
            good.replace('}', ",\"extra\":1}"),
            format!("{good:<2000}"),
        ] {
            assert_eq!(parse_pointer(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn job_ids_are_deterministic_tenant_unique_and_valid() {
        let a = job_id("ws-a", "s", "req");
        assert_eq!(a, job_id("ws-a", "s", "req"));
        assert_ne!(a, job_id("ws-b", "s", "req"));
        assert_ne!(a, job_id("ws-a", "t", "req"));
        assert_ne!(a, job_id("ws-a", "s", "req2"));
        assert_eq!(a.len(), 48);
        assert!(is_id(&a));
    }

    #[test]
    fn routes() {
        assert_eq!(
            parse_route("/api/sites/ws-a/site_1/publish"),
            Some(PublishRoute::Enqueue {
                ws: "ws-a".into(),
                site: "site_1".into()
            })
        );
        let j = job_id("ws-a", "site_1", "r");
        assert_eq!(
            parse_route(&format!("/api/sites/ws-a/site_1/publish/{j}")),
            Some(PublishRoute::Status {
                ws: "ws-a".into(),
                site: "site_1".into(),
                job: j
            })
        );
        for p in [
            "/api/sites/ws-a/site_1/publish/",
            "/api/sites/ws-a/site_1/publish/a/b",
            "/api/sites/ws-a/site_1/publish/J1",
            "/api/sites/ws-a/site_1/publish/..",
            "/api/sites/WS/site_1/publish",
            "/api/sites/ws-a/site_1/publishx",
            "/api/sites/ws-a/publish",
            "/api/sites//s/publish",
        ] {
            assert_eq!(parse_route(p), None, "{p}");
        }
        assert_eq!(
            parse_route("/api/sites/ws-a/site_1/publish")
                .unwrap()
                .allow(),
            "POST"
        );
        // Other site routes are not ours.
        assert_eq!(parse_route("/api/sites/ws-a/site_1/assets"), None);
        assert_eq!(parse_route("/api/sites/ws-a/site_1/session"), None);
        // ...and ours is not theirs.
        assert_eq!(
            crate::assets_api::parse_route("/api/sites/ws-a/site_1/publish"),
            None
        );
        assert_eq!(
            crate::routes::route("POST", "/api/sites/ws-a/site_1/publish"),
            crate::routes::Route::NotFound
        );
    }

    #[test]
    fn publish_authorization_uses_the_auth_policy() {
        use crate::model::Scope;
        let token = |scope, ws: &str| Credential::Token {
            id: "1".repeat(24),
            workspace_id: ws.into(),
            scope,
        };
        let session = |fresh| Credential::Session {
            id: "0".repeat(24),
            fresh,
        };
        // Same role rule as a session publish checkpoint.
        for role in Role::ALL {
            assert_eq!(
                authorize_publish(&session(true), Some(*role), "ws-a").is_ok(),
                crate::session::authz::allows(*role, crate::session::authz::Action::Publish),
                "{role:?}"
            );
        }
        // Edit-only tokens never publish, whatever the role.
        for role in Role::ALL {
            assert_eq!(
                authorize_publish(&token(Scope::Edit, "ws-a"), Some(*role), "ws-a"),
                Err(if *role == Role::Editor {
                    policy::Denied::Role
                } else {
                    policy::Denied::Scope
                })
            );
        }
        for scope in [Scope::Publish, Scope::Activate] {
            assert_eq!(
                authorize_publish(&token(scope, "ws-a"), Some(Role::Publisher), "ws-a"),
                Ok(())
            );
            // A token from another workspace, even for a member of this one.
            assert_eq!(
                authorize_publish(&token(scope, "ws-b"), Some(Role::Owner), "ws-a"),
                Err(policy::Denied::Forbidden)
            );
        }
        assert_eq!(
            authorize_publish(&session(true), Some(Role::Owner), "ws-a"),
            Ok(())
        );
        assert_eq!(
            authorize_publish(&session(true), Some(Role::Editor), "ws-a"),
            Err(policy::Denied::Role)
        );
        // Past the 15-minute re-auth window (#60): 401 reauth_required.
        assert_eq!(
            authorize_publish(&session(false), Some(Role::Owner), "ws-a"),
            Err(policy::Denied::ReauthRequired)
        );
        assert_eq!(policy::Denied::ReauthRequired.status(), 401);
        assert_eq!(
            authorize_publish(&session(true), None, "ws-a"),
            Err(policy::Denied::Forbidden)
        );
    }

    #[test]
    fn server_compile_flag() {
        assert!(!server_compile_enabled(None));
        assert!(!server_compile_enabled(Some("true")));
        assert!(!server_compile_enabled(Some("0")));
        assert!(server_compile_enabled(Some(" 1 ")));
        assert_eq!(
            compile_step(false),
            Err(StepError::Permanent("server_compile_disabled"))
        );
        assert_eq!(
            compile_step(true),
            Err(StepError::Permanent("server_compile_unimplemented"))
        );
    }

    #[test]
    fn a_run_fits_the_assumed_free_plan_query_budget() {
        const {
            assert!(
                MAX_JOBS_PER_RUN * QUERIES_PER_JOB + QUERIES_PER_RUN
                    <= ASSUMED_FREE_QUERIES_PER_INVOCATION
            );
        }
    }

    fn stored(files: &[(&str, u64)], revision_sha: &str) -> StoredArtifact {
        let m = manifest::Manifest {
            format: manifest::MANIFEST_FORMAT.into(),
            revision_id: "rev-1".into(),
            revision_sha256: revision_sha.into(),
            compiler: manifest::Compiler {
                name: "sws-cli".into(),
                version: "0.2.0".into(),
            },
            files: files
                .iter()
                .map(|(p, s)| manifest::FileEntry {
                    path: (*p).into(),
                    sha256: H.into(),
                    size: *s,
                })
                .collect(),
            file_count: files.len() as u64,
            total_bytes: files.iter().map(|f| f.1).sum(),
        };
        let v = manifest::validate(m, &PUBLISH_ARTIFACT).unwrap();
        StoredArtifact {
            manifest_sha256: v.digest,
            revision_sha256: revision_sha.into(),
            manifest: v.canonical,
        }
    }

    #[test]
    fn validate_step_outcomes() {
        let a = stored(&[("index.html", 5)], H);
        let l = &PUBLISH_ARTIFACT;
        assert_eq!(validate_step("rev-1", Some(H), Some(&a), l), Ok(()));
        let p = |c| Err(StepError::Permanent(c));
        assert_eq!(
            validate_step("rev-1", None, Some(&a), l),
            p("revision_missing")
        );
        assert_eq!(
            validate_step("rev-1", Some(H), None, l),
            p("artifact_missing")
        );
        assert_eq!(
            validate_step("rev-2", Some(H), Some(&a), l),
            p("revision_mismatch")
        );
        let other = "b".repeat(64);
        assert_eq!(
            validate_step("rev-1", Some(&other), Some(&a), l),
            p("revision_mismatch")
        );
        let mut t = a.clone();
        t.manifest = t.manifest.replace("\"size\":5", "\"size\":6");
        assert_eq!(
            validate_step("rev-1", Some(H), Some(&t), l),
            p("manifest_invalid")
        );
        let mut t = a.clone();
        t.revision_sha256 = other.clone();
        assert_eq!(
            validate_step("rev-1", Some(&other), Some(&t), l),
            p("revision_mismatch")
        );
    }

    fn row(phase: &str, lease: Value) -> Value {
        json!({
            "id": "j1", "site_id": "s1", "revision_id": "rev-1", "request_id": "req-1",
            "origin": "cli", "server_verified": 0, "phase": phase, "attempt": 2,
            "next_attempt_at": utc_text(T0), "lease_expires_at": lease,
            "error_code": null, "error_phase": null, "last_error": null,
            "created_at": utc_text(T0), "updated_at": utc_text(T0), "finished_at": null,
            "manifest_sha256": H, "revision_sha256": H, "file_count": 1, "total_bytes": 5,
        })
    }

    #[test]
    fn rows_parse_into_state() {
        assert_eq!(
            state_from_row(&row("validating", Value::Null)),
            Some(JobState {
                phase: Phase::Validating,
                origin: Origin::Cli,
                attempt: 2,
                next_attempt_at: T0,
                lease_expires_at: None
            })
        );
        let leased = state_from_row(&row("uploading", json!(utc_text(T0 + 5)))).unwrap();
        assert_eq!(leased.lease_expires_at, Some(T0 + 5));
        assert_eq!(state_from_row(&row("succeeded", Value::Null)), None);
        assert_eq!(state_from_row(&row("queued", json!("soon"))), None);
        let mut r = row("queued", Value::Null);
        r["attempt"] = json!(-1);
        assert_eq!(state_from_row(&r), None);
    }

    #[test]
    fn job_json_never_claims_server_verification_for_cli() {
        let j = job_json("ws-a", &row("ready", Value::Null));
        assert_eq!(j["origin"], "cli");
        assert_eq!(j["server_verified"], false);
        assert_eq!(j["error"], Value::Null);
        assert_eq!(j["artifact"]["file_count"], 1);
        assert_eq!(j["status_url"], "/api/sites/ws-a/s1/publish/j1");
        let mut failed = row("failed", Value::Null);
        failed["error_code"] = json!("revision_missing");
        failed["error_phase"] = json!("validating");
        let j = job_json("ws-a", &failed);
        assert_eq!(
            j["error"],
            json!({ "code": "revision_missing", "phase": "validating" })
        );
    }

    #[test]
    fn enqueue_outcomes() {
        let r = row("queued", Value::Null);
        assert_eq!(
            enqueue_outcome(true, Some(&r), H, "rev-1"),
            Enqueued::Created
        );
        assert_eq!(
            enqueue_outcome(false, Some(&r), H, "rev-1"),
            Enqueued::Existing
        );
        assert_eq!(
            enqueue_outcome(false, Some(&r), &"b".repeat(64), "rev-1"),
            Enqueued::Conflict
        );
        assert_eq!(
            enqueue_outcome(false, Some(&r), H, "rev-2"),
            Enqueued::Conflict
        );
        assert_eq!(enqueue_outcome(false, None, H, "rev-1"), Enqueued::Busy);
    }

    #[test]
    fn idempotency_is_decided_before_the_revision_check() {
        let r = row("queued", Value::Null);
        assert_eq!(existing_outcome(None, H, "rev-1"), None);
        assert_eq!(
            existing_outcome(Some(&r), H, "rev-1"),
            Some(Enqueued::Existing)
        );
        // Another manifest under the same request id: a conflict, even when the new
        // manifest names a revision that does not exist (the handler never gets to
        // look it up) or the same revision with another digest.
        assert_eq!(
            existing_outcome(Some(&r), H, "rev-missing"),
            Some(Enqueued::Conflict)
        );
        assert_eq!(
            existing_outcome(Some(&r), &"c".repeat(64), "rev-1"),
            Some(Enqueued::Conflict)
        );
    }

    #[test]
    fn responses_carry_the_studio_profile_and_no_store() {
        use sws_security::headers::STUDIO_HEADERS;
        let r = accepted("ws-a", &row("queued", Value::Null), true, "scheduled");
        let resps = [
            r.clone(),
            busy(),
            error(403, "forbidden", None),
            manifest_error(&manifest::ManifestError::InvalidPath { index: 3 }),
            method_not_allowed(&PublishRoute::Enqueue {
                ws: "w".into(),
                site: "s".into(),
            }),
        ];
        for x in &resps {
            for (k, v) in STUDIO_HEADERS {
                assert_eq!(x.header(k), Some(*v), "{k} on {}", x.status);
            }
            assert_eq!(x.header("Cache-Control"), Some("no-store"));
        }
        assert_eq!(r.status, 202);
        let b = r.body.unwrap();
        assert_eq!(
            (b["ok"].clone(), b["created"].clone()),
            (json!(true), json!(true))
        );
        assert_eq!(b["job_id"], "j1");
        assert_eq!(resps[1].status, 429);
        assert_eq!(resps[1].header("Retry-After"), Some("60"));
        assert_eq!(resps[3].body.as_ref().unwrap()["details"]["index"], 3);
        assert_eq!(resps[4].header("Allow"), Some("POST"));
    }
}
