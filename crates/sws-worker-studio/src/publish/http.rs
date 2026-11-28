//! wasm glue for the publish routes. Decisions live in the pure [`super`] modules;
//! this file reads the request, performs the D1 I/O and adapts responses.
//!
//! Order (fixed): method (405), framing (411/413/414/415), then
//! [`super::authz::require_publish`] (authenticate 401, cookie Origin 403, #65 policy
//! 401/403, site 404), then the body (read under the cap) and the route. Enqueue
//! validates the manifest, answers an existing job for the same request id (or
//! `409 idempotency_conflict`) before it looks at the revision, then checks the
//! revision, writes the job and its artifact in one D1 batch and answers `202`; it
//! never compiles. It then
//! sends a queue pointer if a Queue is bound; a failed or absent send is fine,
//! because the Cron Trigger recovers every due job.

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sws_assets::chunk;
use sws_assets::limits::PUBLISH_ARTIFACT;
use worker::{Env, Request, Response, Result, console_error};

use super::authz::require_publish;
use super::bundle::{self, BundleArtifactSource};
use super::manifest::{parse_request, validate_request};
use super::{Enqueued, PublishRoute, QUEUE_BINDING, QueuePointer, StoredArtifact, sql};
use crate::auth::Principal;
use crate::auth::policy::{self, Permission};
use crate::bounds::{self, BodyLimiter, MAX_URL_BYTES};
use crate::deploy::store::{AuditAction, AuditResult, AuditRow, PublishStore};
use crate::deploy::wasm::{D1Store, EnvSecrets, FetchClient, WorkerRuntime, api_base};
use crate::deploy::{PublishEnv, PublishJobRef, Stage};
use crate::model::Role;
use crate::respond::{self, Resp};
use crate::{d1, repo};

fn to_response(r: Resp) -> Result<Response> {
    crate::session::http::to_response(r)
}

pub async fn handle(mut req: Request, env: &Env, route: PublishRoute) -> Result<Response> {
    let method = req.method().to_string();
    if method != route.allow() {
        return to_response(super::method_not_allowed(&route));
    }
    let url = req.url()?;
    let url_len = url.path().len() + url.query().map_or(0, |q| q.len() + 1);
    if url_len > MAX_URL_BYTES {
        return to_response(respond::reject(bounds::Reject::UriTooLong));
    }
    let headers = req.headers();
    let content_length = headers.get("Content-Length")?;
    let transfer_encoding = headers.get("Transfer-Encoding")?;
    let content_type = headers.get("Content-Type")?;
    match &route {
        PublishRoute::BundleUpload { .. } => {
            if transfer_encoding.is_some() {
                return to_response(respond::reject(bounds::Reject::LengthRequired));
            }
            let Some(cl) = content_length.as_deref() else {
                return to_response(respond::reject(bounds::Reject::LengthRequired));
            };
            let Ok(len) = cl.trim().parse::<usize>() else {
                return to_response(respond::reject(bounds::Reject::PayloadTooLarge));
            };
            if len > bundle::MAX_BUNDLE_BYTES {
                return to_response(respond::reject(bounds::Reject::PayloadTooLarge));
            }
            if let Some(ct) = content_type.as_deref() {
                let mime = ct.split(';').next().unwrap_or("").trim();
                if !mime.is_empty() && !mime.eq_ignore_ascii_case("application/octet-stream") {
                    return to_response(respond::reject(bounds::Reject::UnsupportedMediaType));
                }
            }
        }
        _ => {
            if let Err(rej) = bounds::check_request_with_cap(
                &method,
                url_len,
                content_length.as_deref(),
                transfer_encoding.as_deref(),
                content_type.as_deref(),
                PUBLISH_ARTIFACT.max_manifest_bytes,
            ) {
                return to_response(respond::reject(rej));
            }
        }
    }

    // Authenticate and authorize before buffering any body byte.
    let (ws, site) = route.ids();
    let (principal, role) = match require_publish(&req, env, &method, ws, site).await? {
        Ok(v) => v,
        Err(d) => return to_response(super::error(d.status(), d.code(), None)),
    };

    // Count the bytes actually streamed (never trust Content-Length alone). GET drains
    // under the standard cap and any byte is a 400.
    let cap = match &route {
        PublishRoute::BundleUpload { .. } => bundle::MAX_BUNDLE_BYTES,
        PublishRoute::Enqueue { .. } => PUBLISH_ARTIFACT.max_manifest_bytes,
        _ => bounds::MAX_BODY_BYTES,
    };
    let declared = content_length
        .as_deref()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(cap);
    let mut limiter = BodyLimiter::with_cap(declared, cap);
    let mut body = Vec::new();
    if let Ok(mut stream) = req.stream() {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if limiter.push(chunk.len()).is_err() {
                return to_response(respond::reject(bounds::Reject::PayloadTooLarge));
            }
            body.extend_from_slice(&chunk);
        }
    }
    if method == "GET" && !body.is_empty() {
        return to_response(respond::reject(bounds::Reject::BadRequest));
    }

    let db = d1::database(env)?;
    match &route {
        PublishRoute::Status { job, .. } => status(&db, ws, site, job).await,
        PublishRoute::Enqueue { .. } => {
            enqueue(env, &db, ws, site, &principal, role, &body).await
        }
        PublishRoute::BundleUpload {
            manifest_sha256, ..
        } => bundle_upload(&db, ws, site, manifest_sha256, principal.user_id, &body).await,
        PublishRoute::Activate { job, .. } => {
            if !body.is_empty()
                && !serde_json::from_slice::<Value>(&body).is_ok_and(|v| v.is_object())
            {
                return to_response(respond::reject(bounds::Reject::BadRequest));
            }
            activate(&req, env, &db, ws, site, job, &principal, role).await
        }
    }
}

async fn status(
    db: &worker::D1Database,
    ws: &str,
    site: &str,
    job: &str,
) -> Result<Response> {
    let Some(row) = d1::first(db, &sql::job_get(ws, site, job)).await? else {
        return to_response(super::error(404, "job_not_found", None));
    };
    let mut body = super::job_json(ws, &row);
    let act = d1::first(db, &crate::deploy::sql::activation_get(ws, job)).await?;
    let bytes_verified = act
        .as_ref()
        .and_then(|a| a.get("bytes_verified"))
        .and_then(Value::as_i64)
        == Some(1);
    if let Some(o) = body.as_object_mut() {
        o.insert("bytes_verified".into(), json!(bytes_verified));
        if let Some(pid) = act
            .as_ref()
            .and_then(|a| a.get("published_id"))
            .and_then(Value::as_str)
        {
            let vid = act
                .as_ref()
                .and_then(|a| a.get("cf_version_id"))
                .and_then(Value::as_str);
            o.insert(
                "deployment".into(),
                json!({ "published_id": pid, "cf_version_id": vid }),
            );
        }
    }
    to_response(super::ok(200, body))
}

async fn bundle_upload(
    db: &worker::D1Database,
    ws: &str,
    site: &str,
    manifest_sha256: &str,
    user_id: i64,
    body: &[u8],
) -> Result<Response> {
    let parsed = match bundle::parse_and_verify(body, Some(manifest_sha256), &PUBLISH_ARTIFACT) {
        Ok(b) => b,
        Err(e) => return to_response(super::error(e.status(), e.code(), None)),
    };
    if let Some(existing) =
        d1::first(db, &crate::deploy::sql::bundle_get(ws, site, manifest_sha256)).await?
    {
        return to_response(super::ok(
            200,
            json!({
                "ok": true,
                "created": false,
                "manifest_sha256": manifest_sha256,
                "total_bytes": existing.get("total_bytes"),
                "file_count": parsed.valid_manifest.manifest.file_count,
            }),
        ));
    }
    let now_str = super::now_text(super::drive::real_now());
    let plan = chunk::plan(body.len());
    let chunk_count = u32::try_from(plan.len()).unwrap_or(u32::MAX);
    let mut stmts = Vec::with_capacity(1 + plan.len());
    stmts.push(crate::deploy::sql::bundle_insert(
        ws,
        site,
        manifest_sha256,
        body.len() as u64,
        chunk_count,
        user_id,
        &now_str,
    ));
    for (seq, start, end) in plan {
        stmts.push(crate::deploy::sql::bundle_chunk_insert(
            ws,
            site,
            manifest_sha256,
            seq,
            &body[start..end],
        ));
    }
    d1::batch(db, &stmts).await?;
    to_response(super::ok(
        201,
        json!({
            "ok": true,
            "created": true,
            "manifest_sha256": manifest_sha256,
            "total_bytes": body.len(),
            "file_count": parsed.valid_manifest.manifest.file_count,
        }),
    ))
}

#[derive(Deserialize)]
struct ChunkRow {
    data: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
async fn activate(
    req: &Request,
    env: &Env,
    db: &worker::D1Database,
    ws: &str,
    site: &str,
    job: &str,
    principal: &Principal,
    role: Role,
) -> Result<Response> {
    if let Err(d) = policy::authorize(&principal.credential, Some(role), ws, Permission::Activate) {
        return to_response(super::error(d.status(), d.code(), None));
    }
    let Some(job_row) = d1::first(db, &sql::job_get(ws, site, job)).await? else {
        return to_response(super::error(404, "job_not_found", None));
    };
    if job_row.get("phase").and_then(Value::as_str) != Some("ready") {
        return to_response(super::error(409, "job_not_ready", None));
    }
    let Some(target_row) = d1::first(db, &crate::deploy::sql::target_get(ws, site)).await? else {
        return to_response(super::error(404, "target_not_found", None));
    };
    let target_id = target_row
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let artifact_row = d1::first(db, &sql::artifact_get(ws, job)).await?;
    let Some(stored) = artifact_row.as_ref().and_then(StoredArtifact::from_row) else {
        return to_response(super::error(409, "artifact_missing", None));
    };
    let Some(bundle_row) = d1::first(
        db,
        &crate::deploy::sql::bundle_get(ws, site, &stored.manifest_sha256),
    )
    .await?
    else {
        return to_response(super::error(409, "bundle_missing", None));
    };
    let total_bytes = bundle_row
        .get("total_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let chunk_rows = d1::all(
        db,
        &crate::deploy::sql::bundle_chunks_get(ws, site, &stored.manifest_sha256),
    )
    .await?;
    let mut raw = Vec::with_capacity(total_bytes);
    for row in chunk_rows {
        let chunk: ChunkRow = serde_json::from_value(row)
            .map_err(|e| worker::Error::RustError(format!("bundle chunk decode: {e}")))?;
        raw.extend_from_slice(&chunk.data);
    }
    let verified = match bundle::verify_against_stored(&raw, &stored, &PUBLISH_ARTIFACT) {
        Ok(v) => v,
        Err(code) => return to_response(super::error(422, code, None)),
    };
    drop(raw);

    let revision_id = job_row
        .get("revision_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(rev_row) = d1::first(db, &repo::revision_get(ws, site, &revision_id)).await? else {
        return to_response(super::error(409, "revision_missing", None));
    };
    let Some(revision_seq) = rev_row.get("revision").and_then(Value::as_i64) else {
        return to_response(super::error(409, "revision_missing", None));
    };

    let store = D1Store {
        db: d1::database(env)?,
    };
    let now = crate::auth_http::now_secs();
    // Activation-time authorization recheck right before lock_and_activate.
    let fresh_role = crate::auth_http::role_in(db, ws, principal.user_id).await?;
    let tag = principal.credential.tag();
    let fresh_cred =
        crate::auth_http::revalidate(db, ws, principal.user_id, tag.as_deref(), now).await?;
    let recheck = fresh_cred
        .as_ref()
        .map(|c| policy::authorize(c, fresh_role, ws, Permission::Activate));
    if !matches!(recheck, Some(Ok(()))) {
        let _ = store
            .audit(&AuditRow {
                workspace_id: ws.to_string(),
                site_id: site.to_string(),
                target_id,
                actor_user_id: Some(principal.user_id),
                action: AuditAction::PublishActivate,
                result: AuditResult::Refused,
                cf_version_id: None,
                published_id: None,
                revision_id: Some(revision_id),
                job_id: Some(job.to_string()),
                stage: Some(Stage::Prepare.as_str()),
                error_code: Some("activation_unauthorized"),
                at: super::now_text(now.max(0) as u64),
            })
            .await;
        return match recheck {
            Some(Err(policy::Denied::ReauthRequired)) => {
                to_response(super::error(401, "reauth_required", None))
            }
            _ => to_response(super::error(403, "activation_unauthorized", None)),
        };
    }

    let base = api_base(Some(req), env);
    let secrets = EnvSecrets(env);
    let publish_env = PublishEnv {
        http: &FetchClient,
        rt: &WorkerRuntime,
        store: &store,
        secrets: &secrets,
        api_base: &base,
    };
    let job_ref = PublishJobRef {
        workspace_id: ws.to_string(),
        site_id: site.to_string(),
        job_id: job.to_string(),
        revision_id,
        revision_seq,
        requested_by: principal.user_id,
        artifact_sha256: None,
    };
    let source = BundleArtifactSource::new(verified);
    match crate::deploy::run_publish(&job_ref, &source, &publish_env).await {
        Ok(out) => {
            d1::run(
                db,
                &crate::deploy::sql::activation_set_deployed(
                    ws,
                    site,
                    job,
                    &out.published_id,
                    &out.cf_version_id,
                ),
            )
            .await?;
            to_response(super::ok(
                200,
                json!({
                    "ok": true,
                    "deployment": {
                        "target_id": out.target_id,
                        "published_id": out.published_id,
                        "cf_version_id": out.cf_version_id,
                        "revision_id": out.revision_id,
                        "revision_seq": out.revision_seq,
                        "manifest_sha256": out.manifest_sha256,
                        "uploaded_files": out.uploaded_files,
                        "total_files": out.total_files,
                    }
                }),
            ))
        }
        Err(f) => {
            let status = match f.code {
                "target_not_found" => 404,
                "token_missing" | "store_unavailable" => 503,
                "stale_revision"
                | "target_busy"
                | "lease_lost"
                | "schema_mismatch"
                | "artifact_mismatch"
                | "activation_rejected"
                | "activation_superseded"
                | "lease_lost_after_activation" => 409,
                _ => 502,
            };
            to_response(super::ok(
                status,
                json!({
                    "ok": false,
                    "error": f.code,
                    "message": f.message,
                    "stage": f.stage.as_str(),
                    "activation": f.activation.as_str(),
                }),
            ))
        }
    }
}

async fn enqueue(
    env: &Env,
    db: &worker::D1Database,
    ws: &str,
    site: &str,
    principal: &Principal,
    role: Role,
    body: &[u8],
) -> Result<Response> {
    let req_parsed = match parse_request(body) {
        Ok(r) => r,
        Err(e) => return to_response(super::manifest_error(&e)),
    };
    let activate = req_parsed.activate;
    if activate
        && let Err(d) = policy::authorize(&principal.credential, Some(role), ws, Permission::Activate)
    {
        return to_response(super::error(d.status(), d.code(), None));
    }
    let (request_id, m) = match validate_request(req_parsed, &PUBLISH_ARTIFACT) {
        Ok(v) => v,
        Err(e) => return to_response(super::manifest_error(&e)),
    };
    let revision_id = m.manifest.revision_id.clone();
    let job_id = super::job_id(ws, site, &request_id);

    // Idempotency first: a job already filed under this request id answers on its own
    // (the same job, or a conflict for another manifest), before the revision check.
    let existing = d1::first(db, &sql::job_get(ws, site, &job_id)).await?;
    if let Some(outcome) = super::existing_outcome(existing.as_ref(), &m.digest, &revision_id) {
        return respond_enqueued(env, ws, site, &job_id, &revision_id, outcome, existing).await;
    }

    // The artifact must be compiled from an existing, immutable revision of THIS site.
    let Some(rev) = d1::first(db, &repo::revision_get(ws, site, &revision_id)).await? else {
        return to_response(super::error(404, "revision_not_found", None));
    };
    if rev.get("ir_sha256").and_then(Value::as_str) != Some(m.manifest.revision_sha256.as_str()) {
        return to_response(super::error(409, "revision_mismatch", None));
    }

    let new = sql::NewJob {
        workspace_id: ws,
        site_id: site,
        job_id: &job_id,
        revision_id: &revision_id,
        request_id: &request_id,
        created_by: principal.user_id,
        now: super::drive::real_now(),
    };
    let mut stmts = vec![sql::job_insert(&new), sql::artifact_insert(&new, &m)];
    if activate {
        let tag = principal.credential.tag();
        stmts.push(crate::deploy::sql::activation_upsert(
            ws,
            site,
            &job_id,
            true,
            tag.as_deref(),
            false,
        ));
    }
    let changes = d1::batch(db, &stmts).await?;
    let inserted = changes.first().copied() == Some(1);
    let row = d1::first(db, &sql::job_get(ws, site, &job_id)).await?;
    let outcome = super::enqueue_outcome(inserted, row.as_ref(), &m.digest, &revision_id);
    respond_enqueued(env, ws, site, &job_id, &revision_id, outcome, row).await
}

async fn respond_enqueued(
    env: &Env,
    ws: &str,
    site: &str,
    job_id: &str,
    revision_id: &str,
    outcome: Enqueued,
    row: Option<Value>,
) -> Result<Response> {
    match outcome {
        Enqueued::Busy => to_response(super::busy()),
        Enqueued::Conflict => to_response(super::error(409, "idempotency_conflict", None)),
        outcome @ (Enqueued::Created | Enqueued::Existing) => {
            let row = row.unwrap_or(Value::Null);
            // Re-send for an existing job that is still queued too: that is how a
            // client retry recovers a lost queue send. Duplicates are harmless.
            let still_queued = row.get("phase").and_then(Value::as_str) == Some("queued");
            let dispatch = if still_queued {
                dispatch(env, &QueuePointer::new(ws, site, job_id, revision_id)).await
            } else {
                "none"
            };
            to_response(super::accepted(
                ws,
                &row,
                outcome == Enqueued::Created,
                dispatch,
            ))
        }
    }
}

/// Send the pointer when a Queue is bound. Returns how the job will be picked up:
/// `queue`, or `scheduled` (no binding, or the send failed: the Cron Trigger's
/// recovery sweep drives it).
async fn dispatch(env: &Env, p: &QueuePointer) -> &'static str {
    let Ok(queue) = env.queue(QUEUE_BINDING) else {
        return "scheduled";
    };
    match queue.send(p.to_text()).await {
        Ok(()) => "queue",
        Err(e) => {
            console_error!("publish queue send failed (the scheduled sweep recovers it): {e}");
            "scheduled"
        }
    }
}
