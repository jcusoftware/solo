//! wasm-only glue for the asset routes: authenticate, authorize against D1
//! membership, then upload/download/delete/list/reconcile against the selected
//! backend (cardless D1 blobs by default, opt-in R2). All decisions live in the
//! host-tested [`crate::assets_api`], [`crate::asset_repo`] and `sws-assets`; this
//! module only performs I/O. There is no filesystem here: the local store is
//! native-only (`sws_assets::local`).

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sws_assets::chunk;
use sws_assets::error::AssetError;
use sws_assets::hash::{asset_row_id, sha256_hex};
use sws_assets::limits::{
    GeneratedKind, MAX_INFLIGHT_UPLOAD_BYTES, RECONCILE_BATCH, RESERVATION_TTL_SECS,
};
use sws_assets::quota::{StreamCounter, Usage, admit_inflight, diagnose, plan_upload};
use sws_assets::reconcile::object_is_orphan;
use sws_assets::time::utc_text;
use sws_assets::validate::{
    ContentKind, check_declared_vs_sniffed, declared_kind, is_sha256, r2_key, r2_site_prefix,
};
use worker::{D1Database, Date, Env, Request, Response, Result, console_error, js_sys};

use sws_security::worker_utils::var;

use crate::asset_repo as q;
use crate::assets_api::{
    self, AssetRoute, BACKEND_VAR, Backend, R2_BINDING, error_resp, limits_json, ok_json,
};
use crate::auth;
use crate::bounds::{self, MAX_URL_BYTES};
use crate::d1;
use crate::model::{Role, StorageBackend};
use crate::repo;
use crate::respond::{self, Resp};

fn to_response(r: Resp, bytes: Option<Vec<u8>>) -> Result<Response> {
    let resp = match (bytes, &r.body) {
        (Some(b), _) => Response::from_bytes(b)?,
        (None, Some(body)) => Response::from_bytes(serde_json::to_vec(body)?)?,
        (None, None) => Response::empty()?,
    }
    .with_status(r.status);
    for (k, v) in &r.headers {
        resp.headers().set(k, v)?;
    }
    Ok(resp)
}

fn json_resp(r: Resp) -> Result<Response> {
    to_response(r, None)
}

fn err(e: AssetError) -> Result<Response> {
    json_resp(error_resp(e, None))
}

/// Wall-clock seconds. The `insecure-dev-auth` test build may override it with
/// `X-Sws-Dev-Now` so integration tests can age reservations and objects.
fn now_secs(req: &Request) -> u64 {
    #[cfg(feature = "insecure-dev-auth")]
    if let Some(v) = req
        .headers()
        .get("X-Sws-Dev-Now")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return v;
    }
    let _ = req;
    Date::now().as_millis() / 1000
}

/// Opaque reservation id (not a secret; uniqueness only): `r` + ms clock + 64 random bits.
fn new_reservation_id() -> String {
    let r = || (js_sys::Math::random() * 4_294_967_296.0) as u32;
    format!("r{:012x}{:08x}{:08x}", Date::now().as_millis(), r(), r())
}

thread_local! {
    /// Reserved bytes of uploads currently buffered in this isolate.
    static INFLIGHT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Holds an in-flight allowance; releases it on drop (including a cancelled request).
struct Inflight(u64);

impl Inflight {
    fn admit(bytes: u64) -> Option<Self> {
        INFLIGHT.with(|c| {
            admit_inflight(c.get(), bytes, MAX_INFLIGHT_UPLOAD_BYTES).then(|| {
                c.set(c.get() + bytes);
                Inflight(bytes)
            })
        })
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        INFLIGHT.with(|c| c.set(c.get().saturating_sub(self.0)));
    }
}

/// The dev-auth header is only ever read by the `insecure-dev-auth` test build; the
/// release build neither reads it nor embeds its name (checked by
/// `scripts/check-studio-no-dev-auth.mjs`).
#[cfg(feature = "insecure-dev-auth")]
fn dev_user_header(headers: &worker::Headers) -> Result<Option<String>> {
    headers.get(auth::DEV_USER_HEADER)
}

#[cfg(not(feature = "insecure-dev-auth"))]
fn dev_user_header(_headers: &worker::Headers) -> Result<Option<String>> {
    Ok(None)
}

fn r2_bucket(env: &Env) -> Option<worker::Bucket> {
    env.bucket(R2_BINDING).ok()
}

fn u64_of(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

pub async fn handle(mut req: Request, env: &Env, route: AssetRoute) -> Result<Response> {
    let method = req.method().to_string();
    if !route.permits(&method) {
        return json_resp(assets_api::method_not_allowed(&route));
    }
    let url = req.url()?;
    let url_len = url.path().len() + url.query().map_or(0, |q| q.len() + 1);
    let headers = req.headers();
    let content_length = headers.get("Content-Length")?;
    let transfer_encoding = headers.get("Transfer-Encoding")?;
    let content_type = headers.get("Content-Type")?;
    let dev_user = dev_user_header(headers)?;

    // Framing. Uploads have their own, larger bound (checked in `upload`); every
    // other asset route keeps the standard 64 KiB JSON/no-body rules.
    if url_len > MAX_URL_BYTES {
        return json_resp(respond::reject(bounds::Reject::UriTooLong));
    }
    if method != "PUT"
        && let Err(rej) = bounds::check_request(
            &method,
            url_len,
            content_length.as_deref(),
            transfer_encoding.as_deref(),
            content_type.as_deref(),
        )
    {
        return json_resp(respond::reject(rej));
    }

    let principal = match auth::authenticate(dev_user.as_deref()) {
        Ok(p) => p,
        Err(_) => return err(AssetError::Unauthenticated),
    };
    let (ws, site) = route.ids();
    let db = d1::database(env)?;

    // Tenant boundary: membership of THIS workspace, else a uniform 403 (it does not
    // reveal whether the workspace exists).
    let Some(member) = d1::first(&db, &repo::membership_get(ws, principal.user_id)).await? else {
        return err(AssetError::Forbidden);
    };
    let role = member
        .get("role")
        .and_then(Value::as_str)
        .and_then(Role::parse);
    let Some(role) = role else {
        return err(AssetError::Forbidden);
    };
    if method != "GET" && !assets_api::can_write(role) {
        return err(AssetError::Forbidden);
    }
    if d1::first(&db, &repo::site_get(ws, site)).await?.is_none() {
        return err(AssetError::SiteNotFound);
    }

    let now = now_secs(&req);
    match &route {
        AssetRoute::List { .. } => list(env, &db, ws, site, now).await,
        AssetRoute::Reconcile { .. } => reconcile(env, &db, ws, site, now).await,
        AssetRoute::Asset { sha, .. } if !is_sha256(sha) => err(AssetError::InvalidSha256),
        AssetRoute::Asset { sha, .. } => match method.as_str() {
            "PUT" => {
                let up = Upload {
                    env,
                    db: &db,
                    ws,
                    site,
                    sha,
                    now,
                    content_type: content_type.as_deref(),
                    content_length: content_length.as_deref(),
                };
                upload(&mut req, up).await
            }
            "GET" => download(env, &db, ws, site, sha).await,
            _ => delete(env, &db, ws, site, sha).await,
        },
    }
}

struct Upload<'a> {
    env: &'a Env,
    db: &'a D1Database,
    ws: &'a str,
    site: &'a str,
    sha: &'a str,
    now: u64,
    content_type: Option<&'a str>,
    content_length: Option<&'a str>,
}

async fn upload(req: &mut Request, u: Upload<'_>) -> Result<Response> {
    let bucket = r2_bucket(u.env);
    let backend =
        match assets_api::select_backend(var(u.env, BACKEND_VAR).as_deref(), bucket.is_some()) {
            Ok(b) => b,
            Err(e) => return err(e),
        };
    let limits = backend.limits();
    let kind = match declared_kind(u.content_type) {
        Ok(k) => k,
        Err(e) => return err(e),
    };
    // Never trust Content-Length: it only sizes the reservation. A missing header
    // (chunked upload) reserves the per-asset cap; the bytes actually streamed are
    // counted below either way.
    let plan = match plan_upload(u.content_length, &limits) {
        Ok(p) => p,
        Err(e) => {
            let details = (e == AssetError::AssetTooLarge)
                .then(|| json!({ "max_asset_bytes": limits.max_asset_bytes }));
            return json_resp(error_resp(e, details));
        }
    };

    let Some(_inflight) = Inflight::admit(plan.reserve_bytes) else {
        let mut r = error_resp(AssetError::UploadBusy, None);
        r.headers.push(("Retry-After".into(), "1".into()));
        return json_resp(r);
    };

    // Content-addressed dedupe: identical bytes already stored cost nothing.
    if let Some(row) = d1::first(u.db, &repo::asset_by_sha256(u.ws, u.site, u.sha)).await? {
        return json_resp(ok_json(200, stored_json(&row, false)));
    }

    let now_text = utc_text(u.now);
    let rid = new_reservation_id();
    let reserved = d1::run(
        u.db,
        &q::reserve_asset(
            u.ws,
            u.site,
            &rid,
            plan.reserve_bytes,
            &now_text,
            &utc_text(u.now + RESERVATION_TTL_SECS),
            &limits,
        ),
    )
    .await?;
    if reserved == 0 {
        let usage = read_usage(u.db, u.ws, u.site, &now_text).await?;
        let e = diagnose(&usage, plan.reserve_bytes, &limits);
        return json_resp(error_resp(e, Some(usage_json(&usage, &limits))));
    }

    // From here the reservation exists: every failure path releases it.
    match receive_and_store(
        req,
        &u,
        backend,
        bucket.as_ref(),
        kind,
        plan,
        &rid,
        &now_text,
    )
    .await
    {
        Ok(Ok(resp)) => json_resp(resp),
        Ok(Err(e)) => {
            release(u.db, u.ws, u.site, &rid).await;
            let details = (e == AssetError::AssetTooLarge)
                .then(|| json!({ "max_asset_bytes": limits.max_asset_bytes }));
            json_resp(error_resp(e, details))
        }
        Err(e) => {
            release(u.db, u.ws, u.site, &rid).await;
            Err(e)
        }
    }
}

/// Best-effort: a failed release only delays space until the reservation expires.
async fn release(db: &D1Database, ws: &str, site: &str, rid: &str) {
    if let Err(e) = d1::run(db, &q::reservation_release(ws, site, rid)).await {
        console_error!("reservation release failed (it will expire): {e}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn receive_and_store(
    req: &mut Request,
    u: &Upload<'_>,
    backend: Backend,
    bucket: Option<&worker::Bucket>,
    kind: ContentKind,
    plan: sws_assets::quota::UploadPlan,
    rid: &str,
    now_text: &str,
) -> Result<std::result::Result<Resp, AssetError>> {
    // Stream with a hard cap on the bytes ACTUALLY received.
    let mut counter = StreamCounter::new(plan);
    let mut data: Vec<u8> = Vec::with_capacity(plan.reserve_bytes.min(1 << 20) as usize);
    if let Ok(mut stream) = req.stream() {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Err(e) = counter.push(chunk.len()) {
                return Ok(Err(e));
            }
            data.extend_from_slice(&chunk);
        }
    }
    let size = match counter.finish() {
        Ok(n) => n,
        Err(e) => return Ok(Err(e)),
    };
    if let Err(e) = check_declared_vs_sniffed(kind, &data) {
        return Ok(Err(e));
    }
    if sha256_hex(&data) != u.sha {
        return Ok(Err(AssetError::Sha256Mismatch));
    }

    let asset_id = asset_row_id(u.ws, u.site, u.sha);
    let mut stmts = vec![q::asset_commit(
        u.ws,
        u.site,
        &asset_id,
        u.sha,
        size,
        kind.mime(),
        backend.storage().as_str(),
        now_text,
        rid,
    )];
    match backend {
        Backend::D1 => {
            for (seq, start, end) in chunk::plan(data.len()) {
                stmts.push(q::chunk_insert(u.ws, &asset_id, seq, &data[start..end]));
            }
        }
        Backend::R2 => {
            let (Some(bucket), Some(key)) = (bucket, r2_key(u.ws, u.site, u.sha)) else {
                return Ok(Err(AssetError::R2NotConfigured));
            };
            // A put that is never followed by a commit leaves an unreferenced object:
            // reconciliation sweeps it once it is older than the reservation TTL.
            bucket.put(key, data).execute().await?;
        }
    }
    stmts.push(q::reservation_delete(u.ws, rid));

    // One atomic batch: asset row + chunks + reservation release.
    let changes = d1::batch(u.db, &stmts).await?;
    if changes.first().copied().unwrap_or(0) == 0 {
        // Either an identical upload won the race (fine) or our reservation expired.
        return Ok(
            match d1::first(u.db, &repo::asset_by_sha256(u.ws, u.site, u.sha)).await? {
                Some(row) => Ok(ok_json(200, stored_json(&row, false))),
                None => Err(AssetError::ReservationExpired),
            },
        );
    }
    Ok(Ok(ok_json(
        201,
        json!({
            "ok": true,
            "created": true,
            "sha256": u.sha,
            "byte_size": size,
            "content_type": kind.mime(),
            "storage_backend": backend.storage().as_str(),
        }),
    )))
}

fn stored_json(row: &Value, created: bool) -> Value {
    json!({
        "ok": true,
        "created": created,
        "sha256": row.get("sha256"),
        "byte_size": row.get("byte_size"),
        "content_type": row.get("content_type"),
        "storage_backend": row.get("storage_backend"),
    })
}

async fn read_usage(db: &D1Database, ws: &str, site: &str, now: &str) -> Result<Usage> {
    let row = d1::first(db, &q::usage(ws, site, now))
        .await?
        .unwrap_or(Value::Null);
    Ok(Usage {
        site_bytes: u64_of(&row, "site_stored") + u64_of(&row, "site_reserved"),
        site_assets: u64_of(&row, "site_assets") + u64_of(&row, "site_reserved_assets"),
        workspace_bytes: u64_of(&row, "workspace_stored") + u64_of(&row, "workspace_reserved"),
    })
}

fn usage_json(u: &Usage, l: &sws_assets::limits::AssetLimits) -> Value {
    json!({
        "usage": {
            "site_bytes": u.site_bytes,
            "site_assets": u.site_assets,
            "workspace_bytes": u.workspace_bytes,
        },
        "limits": limits_json(l),
    })
}

#[derive(Deserialize)]
struct ChunkRow {
    data: Vec<u8>,
}

async fn download(env: &Env, db: &D1Database, ws: &str, site: &str, sha: &str) -> Result<Response> {
    let Some(row) = d1::first(db, &repo::asset_by_sha256(ws, site, sha)).await? else {
        return err(AssetError::AssetNotFound);
    };
    let (Some(id), Some(ct), size) = (
        row.get("id").and_then(Value::as_str),
        row.get("content_type").and_then(Value::as_str),
        u64_of(&row, "byte_size"),
    ) else {
        return err(AssetError::StoreFailed);
    };
    let Some(kind) = ContentKind::from_mime(ct) else {
        return err(AssetError::StoreFailed);
    };
    let backend = row
        .get("storage_backend")
        .and_then(Value::as_str)
        .and_then(StorageBackend::parse);
    let bytes = match backend {
        Some(StorageBackend::D1) => {
            let mut out = Vec::with_capacity(size as usize);
            for (seq, _, _) in chunk::plan(size as usize) {
                match d1::first_as::<ChunkRow>(db, &q::chunk_get(ws, id, seq)).await? {
                    Some(c) => out.extend_from_slice(&c.data),
                    None => return err(AssetError::StoreFailed),
                }
            }
            out
        }
        Some(StorageBackend::R2) => {
            let Some(bucket) = r2_bucket(env) else {
                return err(AssetError::R2NotConfigured);
            };
            let Some(key) = r2_key(ws, site, sha) else {
                return err(AssetError::StoreFailed);
            };
            let Some(obj) = bucket.get(key).execute().await? else {
                return err(AssetError::StoreFailed);
            };
            match obj.body() {
                Some(b) => b.bytes().await?,
                None => return err(AssetError::StoreFailed),
            }
        }
        None => return err(AssetError::StoreFailed),
    };
    // Integrity: never serve bytes that do not hash to their address.
    if bytes.len() as u64 != size || sha256_hex(&bytes) != sha {
        console_error!("asset integrity check failed for {ws}/{site}/{sha}");
        return err(AssetError::StoreFailed);
    }
    to_response(assets_api::download_resp(kind, sha), Some(bytes))
}

async fn delete(env: &Env, db: &D1Database, ws: &str, site: &str, sha: &str) -> Result<Response> {
    let Some(row) = d1::first(db, &repo::asset_by_sha256(ws, site, sha)).await? else {
        return err(AssetError::AssetNotFound);
    };
    d1::batch(
        db,
        &[
            q::chunks_delete_for_asset(ws, site, sha),
            q::asset_delete(ws, site, sha),
        ],
    )
    .await?;
    if row.get("storage_backend").and_then(Value::as_str) == Some("r2")
        && let (Some(bucket), Some(key)) = (r2_bucket(env), r2_key(ws, site, sha))
        && let Err(e) = bucket.delete(key).await
    {
        // The metadata is gone, so the object is an orphan that reconciliation sweeps.
        console_error!("r2 delete failed (reconcile will sweep it): {e}");
    }
    json_resp(ok_json(
        200,
        json!({ "ok": true, "deleted": true, "sha256": sha }),
    ))
}

async fn list(env: &Env, db: &D1Database, ws: &str, site: &str, now: u64) -> Result<Response> {
    let now_text = utc_text(now);
    let bucket = r2_bucket(env);
    let selected = assets_api::select_backend(var(env, BACKEND_VAR).as_deref(), bucket.is_some());
    let limits = selected
        .map(Backend::limits)
        .unwrap_or(sws_assets::limits::CARDLESS_D1);
    let usage = read_usage(db, ws, site, &now_text).await?;
    let assets = d1::all(db, &repo::asset_list(ws, site, repo::MAX_LIST)).await?;
    let generated = d1::all(db, &q::generated_usage(ws, site)).await?;
    let (backend, backend_error) = match selected {
        Ok(b) => (Some(b.storage().as_str()), None),
        Err(e) => (
            None,
            Some(json!({ "error": e.code(), "message": e.hint() })),
        ),
    };
    let gen_policy: Vec<Value> = GeneratedKind::ALL
        .iter()
        .map(|k| {
            let p = k.policy();
            json!({
                "kind": k.as_str(),
                "max_artifact_bytes": p.max_artifact_bytes,
                "max_site_bytes": p.max_site_bytes,
                "retention_secs": p.retention_secs,
                "keep_latest": p.keep_latest,
            })
        })
        .collect();
    json_resp(ok_json(
        200,
        json!({
            "ok": true,
            "backend": backend,
            "backend_error": backend_error,
            "r2_bound": bucket.is_some(),
            "limits": limits_json(&limits),
            "usage": {
                "site_bytes": usage.site_bytes,
                "site_assets": usage.site_assets,
                "workspace_bytes": usage.workspace_bytes,
            },
            "assets": assets,
            "generated": { "usage": generated, "policy": gen_policy },
            "provisional": true,
        }),
    ))
}

/// Bounded, idempotent cleanup. Every step touches at most `RECONCILE_BATCH` rows;
/// `more: true` means call again.
async fn reconcile(env: &Env, db: &D1Database, ws: &str, site: &str, now: u64) -> Result<Response> {
    let now_text = utc_text(now);
    let batch = RECONCILE_BATCH;
    let mut report = serde_json::Map::new();
    let mut more = false;
    let mut step = |name: &str, n: usize| {
        report.insert(name.into(), json!(n));
        more |= n >= batch as usize;
    };

    step(
        "expired_reservations",
        d1::run(db, &q::reservations_sweep(ws, &now_text, batch)).await?,
    );
    step(
        "expired_generated",
        d1::run(db, &q::generated_expired_sweep(ws, site, &now_text, batch)).await?,
    );
    let mut excess = 0;
    for kind in GeneratedKind::ALL {
        excess += d1::run(db, &q::generated_excess_sweep(ws, site, kind)).await?;
    }
    step("excess_generated", excess);
    step(
        "incomplete_assets",
        d1::run(db, &q::assets_incomplete_sweep(ws, site, batch)).await?,
    );
    // After the deletes above so their chunks are swept in the same pass.
    step(
        "orphan_chunks",
        d1::run(db, &q::chunks_orphan_sweep(ws, batch)).await?,
    );

    let mut orphan_objects = 0usize;
    if let (Some(bucket), Some(prefix)) = (r2_bucket(env), r2_site_prefix(ws, site)) {
        let listed = bucket
            .list()
            .prefix(prefix.clone())
            .limit(batch)
            .execute()
            .await?;
        more |= listed.truncated();
        for obj in listed.objects() {
            let key = obj.key();
            let sha = key.strip_prefix(&prefix).unwrap_or_default();
            let referenced = is_sha256(sha)
                && d1::first(db, &q::asset_r2_referenced(ws, site, sha))
                    .await?
                    .is_some();
            let uploaded = obj.uploaded().as_millis() / 1000;
            if object_is_orphan(&prefix, &key, referenced, uploaded, now) {
                bucket.delete(key).await?;
                orphan_objects += 1;
            }
        }
    }
    report.insert("orphan_objects".into(), json!(orphan_objects));

    json_resp(ok_json(
        200,
        json!({ "ok": true, "reconciled": Value::Object(report), "more": more }),
    ))
}
