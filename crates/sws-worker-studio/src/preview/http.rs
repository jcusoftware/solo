//! wasm-only glue for previews (#67). Every decision lives in the host-tested
//! [`super::origin`], [`super::bundle`], [`super::serve`] and [`super::sql`];
//! this module performs I/O only.
//!
//! Preview host ([`serve`]): reads nothing but the method and URL. The request's
//! `Cookie` and `Authorization` headers are never read, nothing is forwarded,
//! and every response is built here from scratch (no `Set-Cookie`), with the
//! `preview` profile resolved against the Studio origin.
//!
//! Studio host ([`produce`]): `POST /api/sites/{ws}/{site}/previews` stores a
//! validated bundle as one generated artifact (Preview budget, #63) plus a
//! `previews` row, and returns the token URL once; `DELETE .../previews/{id}`
//! revokes it and frees the artifact.

use std::cell::RefCell;
use std::rc::Rc;

use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use sws_assets::chunk;
use sws_assets::error::AssetError;
use sws_assets::hash::{StreamHasher, sha256_hex, to_hex};
use sws_assets::limits::{CHUNK_BYTES, GeneratedKind, RECONCILE_BATCH, RESERVATION_TTL_SECS};
use sws_assets::quota::{UploadMode, upload_memory_cost};
use sws_assets::time::utc_text;
use worker::{D1Database, Date, Env, Headers, Request, Response, Result, console_error};

use super::bundle;
use super::origin::{Config, Origins};
use super::serve::{self, Access, Artifact, PreviewPath, PreviewResp, ProducerRoute, Row};
use super::sql as q;
use crate::asset_repo as aq;
use crate::assets_http::{DownloadSlot, Inflight};
use crate::auth::expiry::parse_text;
use crate::auth::policy::Permission;
use crate::auth_http::{guard, now_secs, random_bytes};
use crate::bounds::{self, MAX_URL_BYTES};
use crate::d1;
use crate::repo;
use crate::respond::{self, Resp};

// --- Preview host ---------------------------------------------------------------------

fn preview_response(r: &PreviewResp, body: Option<Response>) -> Result<Response> {
    let resp = match (body, r.body) {
        (Some(b), _) => b,
        (None, Some(text)) => Response::from_bytes(text.as_bytes().to_vec())?,
        (None, None) => Response::empty()?,
    }
    .with_status(r.status);
    let headers = Headers::new();
    for (k, v) in &r.headers {
        headers.append(k, v)?;
    }
    Ok(resp.with_headers(headers))
}

/// HEAD answers carry the GET headers and no body.
fn send(r: PreviewResp, head: bool) -> Result<Response> {
    if head {
        let mut r = r;
        r.body = None;
        return preview_response(&r, None);
    }
    preview_response(&r, None)
}

/// Serve one preview-host request. Errors become a generic 500 with the full
/// preview header set (never the Studio profile, never a detail).
pub async fn serve(req: Request, env: &Env, studio: Option<&str>) -> Result<Response> {
    match serve_inner(&req, env, studio).await {
        Ok(r) => Ok(r),
        Err(e) => {
            // The error never contains the token (it is only ever hashed).
            console_error!("preview: serving failed: {e}");
            send(serve::internal_error(studio), false)
        }
    }
}

async fn serve_inner(req: &Request, env: &Env, studio: Option<&str>) -> Result<Response> {
    let method = req.method().to_string();
    let url = req.url()?;
    let path = url.path();
    let url_len = path.len() + url.query().map_or(0, |q| q.len() + 1);
    if url_len > MAX_URL_BYTES {
        return send(serve::uri_too_long(studio), false);
    }
    let head = match method.as_str() {
        "GET" => false,
        "HEAD" => true,
        _ => return send(serve::method_not_allowed(studio), false),
    };
    match serve::parse_path(path) {
        PreviewPath::Robots => send(serve::robots(studio), head),
        PreviewPath::NotFound => send(serve::not_found(studio), head),
        PreviewPath::AddSlash(u) => send(serve::add_slash(&u.root(), studio), head),
        PreviewPath::File(u) => serve_file(env, &u, head, studio).await,
    }
}

#[derive(Deserialize)]
struct Chunk {
    data: Vec<u8>,
}

fn text<'a>(row: &'a Value, k: &str) -> Option<&'a str> {
    row.get(k).and_then(Value::as_str)
}

/// The D1 row as the pure decision's [`Row`], plus what serving needs.
struct Found {
    row: Row,
    workspace_id: String,
    artifact: Option<(String, u64)>,
}

fn found(v: &Value) -> Option<Found> {
    let secs = |k: &str| {
        text(v, k)
            .and_then(parse_text)
            .and_then(|n| u64::try_from(n).ok())
    };
    let artifact_id = text(v, "artifact_id");
    let artifact = match artifact_id {
        Some(id) => Some((
            id.to_string(),
            v.get("artifact_bytes").and_then(Value::as_u64)?,
            secs("artifact_expires_at")?,
        )),
        None => None,
    };
    Some(Found {
        workspace_id: text(v, "workspace_id")?.to_string(),
        row: Row {
            site_id: text(v, "site_id")?.to_string(),
            revision_id: text(v, "revision_id")?.to_string(),
            expires_at: secs("expires_at")?,
            revoked: v.get("revoked_at").is_some_and(|r| !r.is_null()),
            artifact: artifact.as_ref().map(|a| Artifact { expires_at: a.2 }),
        },
        artifact: artifact.map(|(id, bytes, _)| (id, bytes)),
    })
}

async fn serve_file(
    env: &Env,
    u: &serve::PreviewUrl<'_>,
    head: bool,
    studio: Option<&str>,
) -> Result<Response> {
    let db = d1::database(env)?;
    let now = u64::try_from(now_secs()).unwrap_or(0);
    let raw = d1::first(&db, &q::preview_by_token(&serve::token_hash(u.token))).await?;
    // A row that does not parse is treated as unknown (fail closed: never served).
    let f = raw.as_ref().and_then(found);
    match serve::access(f.as_ref().map(|f| &f.row), u.site, u.revision, now) {
        Access::Unknown => return send(serve::not_found(studio), head),
        Access::Gone => return send(serve::gone(studio), head),
        Access::Live => {}
    }
    let Some((f, (artifact_id, artifact_bytes))) =
        f.and_then(|f| f.artifact.clone().map(|a| (f, a)))
    else {
        return send(serve::gone(studio), head);
    };
    let ws = f.workspace_id;

    let Some(slot) = DownloadSlot::admit(now) else {
        return send(serve::busy(studio), head);
    };
    // The header always sits in chunk 0 (bundle::MAX_HEADER_BYTES < CHUNK_BYTES).
    let Some(first) = d1::first_as::<Chunk>(&db, &aq::chunk_get(&ws, &artifact_id, 0)).await?
    else {
        console_error!("preview: artifact {artifact_id} has no first chunk");
        return send(serve::internal_error(studio), head);
    };
    let index = match bundle::parse_header(&first.data) {
        Ok(i) if i.total_len == artifact_bytes => i,
        _ => {
            console_error!("preview: artifact {artifact_id} has a damaged header");
            return send(serve::internal_error(studio), head);
        }
    };
    let entry = match bundle::resolve(&index, u.rest) {
        bundle::Resolved::NotFound => return send(serve::not_found(studio), head),
        bundle::Resolved::AddSlash => {
            let location = format!("{}{}/", u.root(), u.rest);
            return send(serve::add_slash(&location, studio), head);
        }
        bundle::Resolved::File(e) => e.clone(),
    };
    let headers = serve::file(entry.kind.content_type(), studio);
    if head || entry.size == 0 {
        return preview_response(&headers, None);
    }
    let spans = bundle::chunk_span(entry.offset, entry.size, CHUNK_BYTES);
    let stream = file_stream(
        env.clone(),
        ws,
        artifact_id,
        spans,
        first.data,
        entry.sha256,
        entry.size,
        slot,
    );
    preview_response(&headers, Some(Response::from_stream(stream)?))
}

/// One file's bytes, one stored chunk per poll (chunk 0 is reused), hashed as
/// they flow; the tail errors the stream if size or SHA-256 differ from the
/// header, so a damaged file is a failed transfer, never silent corruption.
#[allow(clippy::too_many_arguments)]
fn file_stream(
    env: Env,
    ws: String,
    blob: String,
    spans: Vec<(u32, usize, usize)>,
    first: Vec<u8>,
    sha: String,
    size: u64,
    slot: DownloadSlot,
) -> impl Stream<Item = Result<Vec<u8>>> {
    let body = futures_util::stream::unfold((0usize, Some(first)), move |(i, mut first)| {
        let (env, ws, blob) = (env.clone(), ws.clone(), blob.clone());
        let span = spans.get(i).copied();
        async move {
            let (seq, start, end) = span?;
            let data = match (seq, first.take()) {
                (0, Some(d)) => Ok(d),
                _ => {
                    async {
                        let db = d1::database(&env)?;
                        d1::first_as::<Chunk>(&db, &aq::chunk_get(&ws, &blob, seq))
                            .await?
                            .map(|c| c.data)
                            .ok_or_else(|| worker::Error::RustError("missing chunk".into()))
                    }
                    .await
                }
            };
            let piece = data.and_then(|d| {
                d.get(start..end)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| worker::Error::RustError("short chunk".into()))
            });
            Some((piece, (i + 1, None)))
        }
    });
    let state = Rc::new(RefCell::new((StreamHasher::new(), 0u64)));
    let seen = Rc::clone(&state);
    let hashed = body.map(move |piece| -> Result<Vec<u8>> {
        let piece = piece?;
        let mut st = seen.borrow_mut();
        st.0.update(&piece);
        st.1 += piece.len() as u64;
        Ok(piece)
    });
    let tail = futures_util::stream::once(async move {
        let _slot = slot;
        let (hasher, n) = std::mem::take(&mut *state.borrow_mut());
        if n == size && hasher.finish_hex() == sha {
            None
        } else {
            console_error!("preview: file integrity check failed");
            Some(Err(worker::Error::RustError(
                "preview integrity check failed".into(),
            )))
        }
    })
    .filter_map(|r: Option<Result<Vec<u8>>>| std::future::ready(r));
    hashed.chain(tail)
}

// --- Studio host: producer routes --------------------------------------------------------

fn json_resp(r: Resp) -> Result<Response> {
    let resp = match &r.body {
        Some(body) => Response::from_bytes(serde_json::to_vec(body)?)?,
        None => Response::empty()?,
    }
    .with_status(r.status);
    for (k, v) in &r.headers {
        resp.headers().set(k, v)?;
    }
    Ok(resp)
}

fn err(status: u16, code: &str) -> Result<Response> {
    json_resp(respond::json_error(status, code, &[]))
}

fn asset_err(e: AssetError) -> Result<Response> {
    json_resp(crate::assets_api::error_resp(e, None))
}

fn ok(status: u16, body: Value) -> Result<Response> {
    let mut r = respond::json_ok(body);
    r.status = status;
    json_resp(r)
}

/// Opaque, time-ordered id: `prefix` + 12 hex ms + 16 hex random. Time order makes
/// the keep-newest retention sweep evict the oldest of same-second uploads.
fn new_id(prefix: char) -> Result<String> {
    Ok(format!(
        "{prefix}{:012x}{}",
        Date::now().as_millis(),
        to_hex(&random_bytes::<8>()?)
    ))
}

pub async fn produce(
    req: Request,
    env: &Env,
    config: &Config,
    route: ProducerRoute,
) -> Result<Response> {
    let method = req.method().to_string();
    if method != route.allow() {
        return json_resp(respond::json_error(
            405,
            "method_not_allowed",
            &[("Allow", route.allow())],
        ));
    }
    let Some(origins) = config.origins() else {
        return err(503, "preview_origin_not_configured");
    };
    let url = req.url()?;
    let url_len = url.path().len() + url.query().map_or(0, |q| q.len() + 1);
    if url_len > MAX_URL_BYTES {
        return json_resp(respond::reject(bounds::Reject::UriTooLong));
    }
    match &route {
        ProducerRoute::Create { ws, site } => {
            let pin = serve::revision_pin(url.query());
            create(req, env, origins, ws, site, pin).await
        }
        ProducerRoute::Revoke { ws, site, id } => revoke(&req, env, ws, site, id, url_len).await,
    }
}

async fn create(
    mut req: Request,
    env: &Env,
    origins: &Origins,
    ws: &str,
    site: &str,
    pin: Option<(String, String)>,
) -> Result<Response> {
    let headers = req.headers();
    let content_type = headers.get("Content-Type")?;
    let content_length = headers.get("Content-Length")?;
    let media = content_type
        .as_deref()
        .and_then(|c| c.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase());
    if media.as_deref() != Some(bundle::CONTENT_TYPE) {
        return json_resp(respond::reject(bounds::Reject::UnsupportedMediaType));
    }
    // The bundle is buffered (validated and hashed before any write), so its size
    // must be declared up front and is then enforced on the bytes actually read.
    let Some(declared) = content_length.as_deref().map(str::trim) else {
        return json_resp(respond::reject(bounds::Reject::LengthRequired));
    };
    if declared.is_empty() || !declared.bytes().all(|b| b.is_ascii_digit()) {
        return json_resp(respond::reject(bounds::Reject::BadRequest));
    }
    let declared: u64 = match declared.parse() {
        Ok(0) => return err(400, bundle::BundleError::Empty.code()),
        Ok(n) if n <= bundle::MAX_BUNDLE_BYTES => n,
        _ => return err(413, bundle::BundleError::TooLarge.code()),
    };
    let Some((revision_id, revision_sha)) = pin else {
        return err(400, "invalid_revision_pin");
    };

    let (principal, _role) = match guard(&req, env, "POST", ws, Permission::Edit).await? {
        Ok(p) => p,
        Err(resp) => return Ok(resp),
    };
    let db = d1::database(env)?;
    if d1::first(&db, &repo::site_get(ws, site)).await?.is_none() {
        return asset_err(AssetError::SiteNotFound);
    }
    let Some(rev) = d1::first(&db, &repo::revision_get(ws, site, &revision_id)).await? else {
        return err(404, "revision_not_found");
    };
    if text(&rev, "ir_sha256") != Some(revision_sha.as_str()) {
        return err(409, "revision_mismatch");
    }

    let Some(_inflight) = Inflight::admit(upload_memory_cost(UploadMode::BufferedD1, declared))
    else {
        let mut r = crate::assets_api::error_resp(AssetError::UploadBusy, None);
        r.headers.push(("Retry-After".into(), "1".into()));
        return json_resp(r);
    };
    let mut data: Vec<u8> = Vec::with_capacity(declared as usize);
    if let Ok(mut stream) = req.stream() {
        while let Some(piece) = stream.next().await {
            let piece = piece?;
            if data.len() as u64 + piece.len() as u64 > declared {
                return err(413, "payload_too_large");
            }
            data.extend_from_slice(&piece);
        }
    }
    if data.len() as u64 != declared {
        return err(400, "content_length_mismatch");
    }
    let index = match bundle::validate(&data) {
        Ok(i) => i,
        Err(e) => {
            let mut r = respond::json_error(e.status(), e.code(), &[]);
            if let (Some(i), Some(body)) =
                (e.index(), r.body.as_mut().and_then(Value::as_object_mut))
            {
                body.insert("file_index".into(), json!(i));
            }
            return json_resp(r);
        }
    };

    let now = u64::try_from(now_secs()).unwrap_or(0);
    let now_text = utc_text(now);
    // Bounded housekeeping first, so expired previews stop holding budget and old
    // rows go away (each touches at most one batch of rows).
    d1::run(
        &db,
        &aq::generated_expired_sweep(ws, site, &now_text, RECONCILE_BATCH),
    )
    .await?;
    let cutoff = utc_text(now.saturating_sub(serve::ROW_RETENTION_AFTER_EXPIRY_SECS));
    d1::run(&db, &q::previews_sweep(ws, site, &cutoff, RECONCILE_BATCH)).await?;

    let size = data.len() as u64;
    let kind = GeneratedKind::Preview;
    let rid = new_id('r')?;
    let reserved = d1::run(
        &db,
        &aq::reserve_generated(
            ws,
            site,
            &rid,
            kind,
            size,
            &now_text,
            &utc_text(now + RESERVATION_TTL_SECS),
        ),
    )
    .await?;
    if reserved == 0 {
        return asset_err(AssetError::GeneratedBudgetExceeded);
    }

    let token = serve::mint_token(&random_bytes::<{ serve::TOKEN_BYTES }>()?);
    let preview_id = new_id('p')?;
    let artifact_id = new_id('g')?;
    let digest = sha256_hex(&data);
    let expires = now + kind.policy().retention_secs;
    let mut stmts = vec![aq::generated_commit(
        ws,
        site,
        &artifact_id,
        kind,
        &digest,
        size,
        now,
        expires,
        &rid,
        u64::try_from(now_secs()).unwrap_or(0),
    )];
    for (seq, start, end) in chunk::plan(data.len()) {
        stmts.push(aq::generated_chunk_insert(
            ws,
            &artifact_id,
            seq,
            &data[start..end],
        ));
    }
    drop(data);
    stmts.push(q::preview_insert(
        ws,
        site,
        &preview_id,
        &revision_id,
        &artifact_id,
        &serve::token_hash(&token),
        principal.user_id,
    ));
    stmts.push(aq::reservation_delete(ws, &rid));
    let changes = match d1::batch(&db, &stmts).await {
        Ok(c) => c,
        Err(e) => {
            release(&db, ws, site, &rid).await;
            return Err(e);
        }
    };
    if changes.first().copied().unwrap_or(0) == 0 {
        release(&db, ws, site, &rid).await;
        return asset_err(AssetError::ReservationExpired);
    }
    // Keep only the newest N previews of this site (the oldest links turn 410).
    if let Err(e) = d1::run(
        &db,
        &aq::generated_excess_sweep(ws, site, kind, RECONCILE_BATCH),
    )
    .await
    {
        console_error!("preview: retention sweep failed (reconcile will retry): {e}");
    }
    ok(
        201,
        json!({
            "ok": true,
            "preview_id": preview_id,
            "url": origins.preview_url(site, &revision_id, &token),
            "expires_at": utc_text(expires),
            "revision_id": revision_id,
            "sha256": digest,
            "byte_size": size,
            "file_count": index.files.len(),
        }),
    )
}

/// Best-effort: a failed release only delays space until the reservation expires.
async fn release(db: &D1Database, ws: &str, site: &str, rid: &str) {
    if let Err(e) = d1::run(db, &aq::reservation_release(ws, site, rid)).await {
        console_error!("preview: reservation release failed (it will expire): {e}");
    }
}

async fn revoke(
    req: &Request,
    env: &Env,
    ws: &str,
    site: &str,
    id: &str,
    url_len: usize,
) -> Result<Response> {
    let h = req.headers();
    if let Err(rej) = bounds::check_request(
        "DELETE",
        url_len,
        h.get("Content-Length")?.as_deref(),
        h.get("Transfer-Encoding")?.as_deref(),
        h.get("Content-Type")?.as_deref(),
    ) {
        return json_resp(respond::reject(rej));
    }
    if let Err(resp) = guard(req, env, "DELETE", ws, Permission::Edit).await? {
        return Ok(resp);
    }
    let db = d1::database(env)?;
    let now = utc_text(u64::try_from(now_secs()).unwrap_or(0));
    let changes = d1::batch(
        &db,
        &[
            q::preview_revoke(ws, site, id, &now),
            q::preview_artifact_delete(ws, site, id),
        ],
    )
    .await?;
    if changes.first().copied().unwrap_or(0) == 0 {
        return match d1::first(&db, &q::preview_get(ws, site, id)).await? {
            None => err(404, "preview_not_found"),
            Some(_) => ok(
                200,
                json!({ "ok": true, "preview_id": id, "revoked": true, "already_revoked": true }),
            ),
        };
    }
    ok(
        200,
        json!({ "ok": true, "preview_id": id, "revoked": true }),
    )
}
