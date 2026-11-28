//! wasm glue for the publish routes. Decisions live in the pure [`super`] modules;
//! this file reads the request, performs the D1 I/O and adapts responses.
//!
//! Order (fixed): method (405), framing (411/413/414/415), authenticate (401),
//! [`super::authz::require_publish`] (403/404), then the body (read under the cap)
//! and the route. Enqueue validates the manifest and the revision, writes the job
//! and its artifact in one D1 batch and answers `202`; it never compiles. It then
//! sends a queue pointer if a Queue is bound; a failed or absent send is fine,
//! because the Cron Trigger recovers every due job.

use futures_util::StreamExt;
use serde_json::Value;
use sws_assets::limits::PUBLISH_ARTIFACT;
use worker::{Env, Request, Response, Result, console_error};

use super::authz::{Denied, require_publish};
use super::manifest::{parse_request, validate_request};
use super::{Enqueued, PublishRoute, QUEUE_BINDING, QueuePointer, sql};
use crate::bounds::{self, BodyLimiter, MAX_URL_BYTES};
use crate::respond::{self, Resp};
use crate::{auth, d1, repo};

fn to_response(r: Resp) -> Result<Response> {
    crate::session::http::to_response(r)
}

#[cfg(feature = "insecure-dev-auth")]
fn dev_user_header(req: &Request, env: &Env) -> Result<Option<String>> {
    if !crate::session::entry::dev_fence_open(req, env) {
        return Ok(None);
    }
    req.headers().get(auth::DEV_USER_HEADER)
}

#[cfg(not(feature = "insecure-dev-auth"))]
fn dev_user_header(_req: &Request, _env: &Env) -> Result<Option<String>> {
    Ok(None)
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
    if let Err(rej) = bounds::check_request_with_cap(
        &method,
        url_len,
        content_length.as_deref(),
        headers.get("Transfer-Encoding")?.as_deref(),
        headers.get("Content-Type")?.as_deref(),
        PUBLISH_ARTIFACT.max_manifest_bytes,
    ) {
        return to_response(respond::reject(rej));
    }
    let dev_user = dev_user_header(&req, env)?;

    // Authenticate and authorize before buffering any body byte.
    let Ok(principal) = auth::authenticate(dev_user.as_deref()) else {
        return to_response(super::error(401, "unauthenticated", None));
    };
    let (ws, site) = route.ids();
    match require_publish(env, &principal, ws, site).await? {
        Ok(_role) => {}
        Err(Denied::Forbidden) => return to_response(super::error(403, "forbidden", None)),
        Err(Denied::SiteNotFound) => return to_response(super::error(404, "site_not_found", None)),
    }

    // Count the bytes actually streamed (never trust Content-Length alone). GET drains
    // under the standard cap and any byte is a 400.
    let cap = match method.as_str() {
        "POST" => PUBLISH_ARTIFACT.max_manifest_bytes,
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
        PublishRoute::Status { job, .. } => {
            match d1::first(&db, &sql::job_get(ws, site, job)).await? {
                Some(row) => to_response(super::ok(200, super::job_json(ws, &row))),
                None => to_response(super::error(404, "job_not_found", None)),
            }
        }
        PublishRoute::Enqueue { .. } => enqueue(env, &db, ws, site, principal.user_id, &body).await,
    }
}

async fn enqueue(
    env: &Env,
    db: &worker::D1Database,
    ws: &str,
    site: &str,
    user_id: i64,
    body: &[u8],
) -> Result<Response> {
    let validated = parse_request(body).and_then(|r| validate_request(r, &PUBLISH_ARTIFACT));
    let (request_id, m) = match validated {
        Ok(v) => v,
        Err(e) => return to_response(super::manifest_error(&e)),
    };
    let revision_id = m.manifest.revision_id.clone();

    // The artifact must be compiled from an existing, immutable revision of THIS site.
    let Some(rev) = d1::first(db, &repo::revision_get(ws, site, &revision_id)).await? else {
        return to_response(super::error(404, "revision_not_found", None));
    };
    if rev.get("ir_sha256").and_then(Value::as_str) != Some(m.manifest.revision_sha256.as_str()) {
        return to_response(super::error(409, "revision_mismatch", None));
    }

    let job_id = super::job_id(ws, site, &request_id);
    let new = sql::NewJob {
        workspace_id: ws,
        site_id: site,
        job_id: &job_id,
        revision_id: &revision_id,
        request_id: &request_id,
        created_by: user_id,
        now: super::drive::real_now(),
    };
    let changes = d1::batch(db, &[sql::job_insert(&new), sql::artifact_insert(&new, &m)]).await?;
    let inserted = changes.first().copied() == Some(1);
    let row = d1::first(db, &sql::job_get(ws, site, &job_id)).await?;
    match super::enqueue_outcome(inserted, row.as_ref(), &m.digest, &revision_id) {
        Enqueued::Busy => to_response(super::busy()),
        Enqueued::Conflict => to_response(super::error(409, "idempotency_conflict", None)),
        outcome @ (Enqueued::Created | Enqueued::Existing) => {
            let row = row.unwrap_or(Value::Null);
            // Re-send for an existing job that is still queued too: that is how a
            // client retry recovers a lost queue send. Duplicates are harmless.
            let still_queued = row.get("phase").and_then(Value::as_str) == Some("queued");
            let dispatch = if still_queued {
                dispatch(env, &QueuePointer::new(ws, site, &job_id, &revision_id)).await
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
