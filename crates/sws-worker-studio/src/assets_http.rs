//! wasm-only glue for the asset routes: authenticate, authorize against D1
//! membership, then upload/download/delete/list/reconcile against the selected
//! backend (cardless D1 blobs by default, opt-in R2). All decisions live in the
//! host-tested [`crate::assets_api`], [`crate::asset_repo`] and `sws-assets`; this
//! module only performs I/O. There is no filesystem here: the local store is
//! native-only (`sws_assets::local`).
//!
//! Memory model (the isolate limit is assumed to be 128 MB):
//!
//! - D1 uploads are BUFFERED (the body is hashed and sniffed before any chunk is
//!   written) and charged [`UploadMode::BufferedD1`] against the in-flight guard.
//! - R2 uploads WITH a `Content-Length` are STREAMED through a counting/hashing/sniffing
//!   tee into a `FixedLengthStream`; the object is kept only if the hash matches
//!   (else it is deleted). Without a `Content-Length` they are buffered
//!   ([`UploadMode::BufferedR2`]).
//! - Downloads are streamed (one D1 chunk or one R2 stream window at a time) behind a
//!   separate admission guard; the hash is verified incrementally and a mismatch
//!   errors the stream at its end instead of buffering the whole asset.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use sws_assets::chunk;
use sws_assets::error::AssetError;
use sws_assets::hash::{StreamHasher, asset_row_id, sha256_hex};
use sws_assets::limits::{
    DOWNLOAD_SLOT_DEADLINE_SECS, GeneratedKind, MAX_INFLIGHT_DOWNLOADS, MAX_INFLIGHT_UPLOAD_BYTES,
    RECONCILE_BATCH, RESERVATION_TTL_SECS,
};
use sws_assets::quota::{
    DownloadSlots, StreamCounter, UploadMode, UploadPlan, Usage, admit_inflight, diagnose,
    has_database_headroom, plan_upload, upload_memory_cost,
};
use sws_assets::reconcile::object_is_orphan;
use sws_assets::time::utc_text;
use sws_assets::validate::{
    ContentKind, check_declared_vs_sniffed, declared_kind, is_sha256, r2_key, r2_site_prefix,
};
use worker::{
    D1Database, Date, Env, FixedLengthStream, Request, Response, Result, console_error, js_sys,
};

use sws_security::worker_utils::var;

use crate::asset_repo as q;
use crate::assets_api::{
    self, AssetRoute, BACKEND_VAR, Backend, R2_BINDING, error_resp, limits_json, ok_json,
};
use crate::auth::policy::{Permission, authorize};
use crate::bounds::{self, MAX_URL_BYTES};
use crate::d1;
use crate::model::StorageBackend;
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

fn stream_response<S>(r: Resp, stream: S) -> Result<Response>
where
    S: Stream<Item = Result<Vec<u8>>> + 'static,
{
    let resp = Response::from_stream(stream)?.with_status(r.status);
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

fn busy(e: AssetError) -> Result<Response> {
    let mut r = error_resp(e, None);
    r.headers.push(("Retry-After".into(), "1".into()));
    json_resp(r)
}

/// The Worker's clock for one request. Two reads matter: the request START (reservation
/// and `created_at`) and the COMMIT, re-read immediately before the commit batch so a
/// slow upload that outlived its reservation is refused instead of being committed
/// against a stale "now".
///
/// The `insecure-dev-auth` test build alone may skew it, so integration tests can age
/// a request without waiting: `X-Sws-Dev-Now` is an OFFSET in seconds from the real
/// clock (applied to both reads), `X-Sws-Dev-Commit-Skew` adds seconds to the commit
/// read only (simulating a slow body), `X-Sws-Dev-Batch` shrinks the reconcile batch
/// `X-Sws-Dev-Fail-Put` makes the R2 put fail, `X-Sws-Dev-Fail-Batch-After-Commit`
/// makes the commit batch report an error AFTER it committed (the ambiguous-outcome
/// case) and `X-Sws-Dev-Hold-Slot` leaks the download slot of that request (so a test
/// can fill the table and watch the deadline reclaim it). The release build neither
/// reads nor embeds these names (checked by `scripts/check-studio-release.mjs`), and even
/// the test build honours them only behind the runtime fence
/// ([`crate::session::devfence`]: `SWS_INSECURE_DEV_AUTH=1` AND a loopback `Host`).
#[derive(Clone, Copy, Default)]
struct Clock {
    offset: i64,
    commit_skew: i64,
    batch: Option<u32>,
    fail_put: bool,
    fail_batch_after_commit: bool,
    hold_slot: bool,
}

impl Clock {
    #[cfg(feature = "insecure-dev-auth")]
    fn from_request(req: &Request, env: &Env) -> Self {
        if !crate::session::entry::dev_fence_open(req, env) {
            return Self::default();
        }
        let num = |name: &str| {
            req.headers()
                .get(name)
                .ok()
                .flatten()
                .and_then(|v| v.trim().parse::<i64>().ok())
        };
        Self {
            offset: num("X-Sws-Dev-Now").unwrap_or(0),
            commit_skew: num("X-Sws-Dev-Commit-Skew").unwrap_or(0),
            batch: num("X-Sws-Dev-Batch")
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0),
            fail_put: num("X-Sws-Dev-Fail-Put").is_some(),
            fail_batch_after_commit: num("X-Sws-Dev-Fail-Batch-After-Commit").is_some(),
            hold_slot: num("X-Sws-Dev-Hold-Slot").is_some(),
        }
    }

    #[cfg(not(feature = "insecure-dev-auth"))]
    fn from_request(_req: &Request, _env: &Env) -> Self {
        Self::default()
    }

    fn real() -> i64 {
        (Date::now().as_millis() / 1000) as i64
    }

    /// Request-start time.
    fn start(&self) -> u64 {
        (Self::real() + self.offset).max(0) as u64
    }

    /// A FRESH read for the commit statement (never reuse [`Clock::start`]).
    fn commit(&self) -> u64 {
        (Self::real() + self.offset + self.commit_skew).max(0) as u64
    }

    fn reconcile_batch(&self) -> u32 {
        self.batch.unwrap_or(RECONCILE_BATCH).min(RECONCILE_BATCH)
    }
}

/// Opaque reservation id (not a secret; uniqueness only): `r` + ms clock + 64 random bits.
fn new_reservation_id() -> String {
    let r = || (js_sys::Math::random() * 4_294_967_296.0) as u32;
    format!("r{:012x}{:08x}{:08x}", Date::now().as_millis(), r(), r())
}

thread_local! {
    /// Working-memory bytes charged to uploads currently in flight in this isolate.
    static INFLIGHT: Cell<u64> = const { Cell::new(0) };
    /// Download slots held by this isolate, each stamped with its admit time.
    static DOWNLOADS: RefCell<DownloadSlots> = const { RefCell::new(DownloadSlots::new()) };
}

/// Holds a download slot for as long as the response stream lives (it is moved into the
/// stream, so a cancelled or finished response releases it on drop). If the runtime
/// drops the stream WITHOUT ever running this `Drop`, the slot still stops counting
/// [`DOWNLOAD_SLOT_DEADLINE_SECS`] after it was admitted (see [`DownloadSlots`]); a
/// download legitimately slower than that merely lets one extra download in.
struct DownloadSlot(u64);

impl DownloadSlot {
    fn admit(now_secs: u64) -> Option<Self> {
        DOWNLOADS.with(|d| {
            d.borrow_mut()
                .admit(
                    now_secs,
                    MAX_INFLIGHT_DOWNLOADS,
                    DOWNLOAD_SLOT_DEADLINE_SECS,
                )
                .map(DownloadSlot)
        })
    }
}

impl Drop for DownloadSlot {
    fn drop(&mut self) {
        DOWNLOADS.with(|d| d.borrow_mut().release(self.0));
    }
}

/// Holds an in-flight upload allowance; releases it on drop (including a cancelled request).
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
    let cursor = url
        .query_pairs()
        .find(|(k, _)| k == "cursor")
        .map(|(_, v)| v.into_owned())
        .filter(|c| !c.is_empty() && c.len() <= 1024);
    let headers = req.headers();
    let content_length = headers.get("Content-Length")?;
    let transfer_encoding = headers.get("Transfer-Encoding")?;
    let content_type = headers.get("Content-Type")?;

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

    // Bearer token or session cookie (#65); the dev header only in the
    // `insecure-dev-auth` test build, behind its runtime fence.
    let Some(principal) = crate::auth_http::authenticate(&req, env).await? else {
        return err(AssetError::Unauthenticated);
    };
    // Cookie-borne writes must come from the Studio's own origin (CSRF).
    if !crate::auth_http::csrf_ok(&req, env, &principal, &method) {
        return json_resp(respond::json_error(403, "forbidden_origin", &[]));
    }
    let (ws, site) = route.ids();
    let db = d1::database(env)?;

    // Tenant boundary: membership of THIS workspace (and, for a token, the token's own
    // workspace), else a uniform 403 (it does not reveal whether the workspace exists).
    let role = crate::auth_http::role_in(&db, ws, principal.user_id).await?;
    let perm = if method == "GET" {
        Permission::Read
    } else {
        Permission::Edit
    };
    if authorize(&principal.credential, role, ws, perm).is_err() {
        return err(AssetError::Forbidden);
    }
    let Some(role) = role else {
        return err(AssetError::Forbidden);
    };
    if method != "GET" && !assets_api::can_write(role) {
        return err(AssetError::Forbidden);
    }
    if d1::first(&db, &repo::site_get(ws, site)).await?.is_none() {
        return err(AssetError::SiteNotFound);
    }

    let clock = Clock::from_request(&req, env);
    match &route {
        AssetRoute::List { .. } => list(env, &db, ws, site, clock.start()).await,
        AssetRoute::Reconcile { .. } => reconcile(env, &db, ws, site, clock, cursor).await,
        AssetRoute::Asset { sha, .. } if !is_sha256(sha) => err(AssetError::InvalidSha256),
        AssetRoute::Asset { sha, .. } => match method.as_str() {
            "PUT" => {
                let up = Upload {
                    env,
                    db: &db,
                    ws,
                    site,
                    sha,
                    clock,
                    content_type: content_type.as_deref(),
                    content_length: content_length.as_deref(),
                };
                upload(&mut req, up).await
            }
            "GET" => download(env, &db, ws, site, sha, clock).await,
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
    clock: Clock,
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

    let mode = match backend {
        Backend::D1 => UploadMode::BufferedD1,
        Backend::R2 if plan.declared => UploadMode::StreamedR2,
        Backend::R2 => UploadMode::BufferedR2,
    };
    let Some(_inflight) = Inflight::admit(upload_memory_cost(mode, plan.reserve_bytes)) else {
        return busy(AssetError::UploadBusy);
    };

    // Content-addressed dedupe: identical bytes already stored cost nothing.
    if let Some(row) = d1::first(u.db, &q::asset_get(u.ws, u.site, u.sha)).await? {
        return json_resp(ok_json(200, stored_json(&row, false)));
    }

    let start = u.clock.start();
    let now_text = utc_text(start);
    let rid = new_reservation_id();
    let reserved = d1::run(
        u.db,
        &q::reserve_asset(
            u.ws,
            u.site,
            &rid,
            plan.reserve_bytes,
            &now_text,
            &utc_text(start + RESERVATION_TTL_SECS),
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
        mode,
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

/// Best-effort removal of an object THIS request wrote and that no row references.
/// Failure leaves an unreferenced object that reconciliation sweeps once it is old.
async fn discard_object(bucket: &worker::Bucket, key: &str) {
    if let Err(e) = bucket.delete(key.to_string()).await {
        console_error!("r2 discard failed (reconcile will sweep it): {e}");
    }
}

/// Counts, sniffs and hashes an upload as it streams past (nothing is buffered).
struct Tee {
    counter: StreamCounter,
    hasher: StreamHasher,
    head: Vec<u8>,
    kind: ContentKind,
    err: Option<AssetError>,
}

impl Tee {
    fn new(plan: UploadPlan, kind: ContentKind) -> Self {
        Self {
            counter: StreamCounter::new(plan),
            hasher: StreamHasher::new(),
            head: Vec::with_capacity(16),
            kind,
            err: None,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> std::result::Result<(), AssetError> {
        self.counter.push(chunk.len())?;
        if self.head.len() < 16 {
            let take = (16 - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
            // 12 bytes decide every supported signature: reject a wrong type early.
            if self.head.len() >= 12 {
                check_declared_vs_sniffed(self.kind, &self.head)?;
            }
        }
        self.hasher.update(chunk);
        Ok(())
    }

    /// After the stream ended: the verified size, or why the upload is refused.
    fn finish(&mut self, expected_sha: &str) -> std::result::Result<u64, AssetError> {
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        let size = self.counter.finish()?;
        check_declared_vs_sniffed(self.kind, &self.head)?;
        if std::mem::take(&mut self.hasher).finish_hex() != expected_sha {
            return Err(AssetError::Sha256Mismatch);
        }
        Ok(size)
    }
}

/// R2 upload with a `Content-Length`: stream the body through a [`Tee`] into a
/// fixed-length R2 put; keep the object only if size, type and hash all verify.
async fn stream_to_r2(
    req: &mut Request,
    u: &Upload<'_>,
    bucket: &worker::Bucket,
    kind: ContentKind,
    plan: UploadPlan,
    key: &str,
) -> Result<std::result::Result<u64, AssetError>> {
    if u.clock.fail_put {
        return Err(worker::Error::RustError("injected R2 put failure".into()));
    }
    let Ok(stream) = req.stream() else {
        return Ok(Err(AssetError::EmptyAsset));
    };
    let tee = Rc::new(RefCell::new(Tee::new(plan, kind)));
    let shared = Rc::clone(&tee);
    let mapped = stream.map(move |chunk| -> Result<Vec<u8>> {
        let chunk = chunk?;
        let verdict = shared.borrow_mut().push(&chunk);
        match verdict {
            Ok(()) => Ok(chunk),
            Err(e) => {
                shared.borrow_mut().err = Some(e);
                Err(worker::Error::RustError("upload rejected".into()))
            }
        }
    });
    let put = bucket
        .put(
            key.to_string(),
            FixedLengthStream::wrap(mapped, plan.reserve_bytes),
        )
        .execute()
        .await;
    let verdict = tee.borrow_mut().finish(u.sha);
    match (verdict, put) {
        (Ok(size), Ok(_)) => Ok(Ok(size)),
        (Ok(_), Err(e)) => {
            discard_object(bucket, key).await;
            Err(e)
        }
        (Err(e), _) => {
            discard_object(bucket, key).await;
            Ok(Err(e))
        }
    }
}

/// Buffer the body (hash and sniff need all of it before any write), verifying it.
/// The buffer is allocated at the reserved size up front, which is exactly what the
/// in-flight guard charged for.
async fn read_buffered(
    req: &mut Request,
    u: &Upload<'_>,
    kind: ContentKind,
    plan: UploadPlan,
) -> Result<std::result::Result<Vec<u8>, AssetError>> {
    let mut counter = StreamCounter::new(plan);
    let mut data: Vec<u8> = Vec::with_capacity(plan.reserve_bytes as usize);
    if let Ok(mut stream) = req.stream() {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Err(e) = counter.push(chunk.len()) {
                return Ok(Err(e));
            }
            data.extend_from_slice(&chunk);
        }
    }
    if let Err(e) = counter.finish() {
        return Ok(Err(e));
    }
    if let Err(e) = check_declared_vs_sniffed(kind, &data) {
        return Ok(Err(e));
    }
    if sha256_hex(&data) != u.sha {
        return Ok(Err(AssetError::Sha256Mismatch));
    }
    Ok(Ok(data))
}

#[allow(clippy::too_many_arguments)]
async fn receive_and_store(
    req: &mut Request,
    u: &Upload<'_>,
    backend: Backend,
    bucket: Option<&worker::Bucket>,
    kind: ContentKind,
    plan: UploadPlan,
    mode: UploadMode,
    rid: &str,
    now_text: &str,
) -> Result<std::result::Result<Resp, AssetError>> {
    // Every R2 upload writes its OWN object (`.../<sha256>/<reservation>`): a delete,
    // a re-upload or the orphan sweep can never act on another upload's object.
    let object_key = (backend == Backend::R2)
        .then(|| r2_key(u.ws, u.site, u.sha, rid))
        .flatten();
    if backend == Backend::R2 && (bucket.is_none() || object_key.is_none()) {
        return Ok(Err(AssetError::R2NotConfigured));
    }
    let mut data: Vec<u8> = Vec::new();
    let size = match (mode, bucket, object_key.as_deref()) {
        (UploadMode::StreamedR2, Some(bucket), Some(key)) => {
            match stream_to_r2(req, u, bucket, kind, plan, key).await? {
                Ok(n) => n,
                Err(e) => return Ok(Err(e)),
            }
        }
        (UploadMode::BufferedR2, Some(bucket), Some(key)) => {
            data = match read_buffered(req, u, kind, plan).await? {
                Ok(d) => d,
                Err(e) => return Ok(Err(e)),
            };
            if u.clock.fail_put {
                return Err(worker::Error::RustError("injected R2 put failure".into()));
            }
            let n = data.len() as u64;
            // A put that is never followed by a commit leaves an unreferenced object:
            // reconciliation sweeps it once it is older than the reservation TTL.
            bucket
                .put(key.to_string(), std::mem::take(&mut data))
                .execute()
                .await?;
            n
        }
        _ => {
            data = match read_buffered(req, u, kind, plan).await? {
                Ok(d) => d,
                Err(e) => return Ok(Err(e)),
            };
            data.len() as u64
        }
    };

    let asset_id = asset_row_id(u.ws, u.site, u.sha);
    let mut chunk_stmts = Vec::new();
    if backend == Backend::D1 {
        for (seq, start, end) in chunk::plan(data.len()) {
            chunk_stmts.push(q::chunk_insert(u.ws, &asset_id, seq, &data[start..end]));
        }
    }
    // A FRESH clock read, immediately before the commit batch: the commit statement
    // refuses a reservation that expired while the body was being received.
    let commit_now = u.clock.commit();
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
        object_key.as_deref(),
        commit_now,
    )];
    stmts.extend(chunk_stmts);
    stmts.push(q::reservation_delete(u.ws, rid));

    // One atomic batch: asset row + chunks + reservation release.
    let outcome = match d1::batch(u.db, &stmts).await {
        // Dev-build fault: report an error although the batch committed.
        Ok(_) if u.clock.fail_batch_after_commit => Err(worker::Error::RustError(
            "injected batch error after commit".into(),
        )),
        other => other,
    };
    let changes = match outcome {
        Ok(c) => c,
        Err(e) => {
            // The outcome is AMBIGUOUS: the batch may have committed even though the
            // call failed (a lost response). Discard our object only when a re-read
            // proves no row references it; if the re-read also fails keep it, and
            // the age-gated orphan sweep removes it later if it was never committed.
            if let (Some(b), Some(k)) = (bucket, object_key.as_deref()) {
                match d1::first(u.db, &q::asset_object_referenced(u.ws, k)).await {
                    Ok(None) => discard_object(b, k).await,
                    Ok(Some(_)) => {
                        console_error!("commit batch errored but the row exists; object kept")
                    }
                    Err(re) => console_error!("commit outcome unknown ({re}); object kept"),
                }
            }
            return Err(e);
        }
    };
    if changes.first().copied().unwrap_or(0) == 0 {
        // Either an identical upload won the race (fine) or our reservation expired.
        // Either way no row references OUR object: remove it (never the winner's).
        if let (Some(b), Some(k)) = (bucket, object_key.as_deref()) {
            discard_object(b, k).await;
        }
        return Ok(
            match d1::first(u.db, &q::asset_get(u.ws, u.site, u.sha)).await? {
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
        database_bytes: u64_of(&row, "database_blob_bytes"),
    })
}

/// The caller's own usage plus a headroom boolean for the shared database. The
/// database-wide byte total spans every tenant, so it is never returned.
fn usage_value(u: &Usage, l: &sws_assets::limits::AssetLimits) -> Value {
    json!({
        "site_bytes": u.site_bytes,
        "site_assets": u.site_assets,
        "workspace_bytes": u.workspace_bytes,
        "database_has_headroom": has_database_headroom(u, l),
    })
}

fn usage_json(u: &Usage, l: &sws_assets::limits::AssetLimits) -> Value {
    json!({ "usage": usage_value(u, l), "limits": limits_json(l) })
}

#[derive(Deserialize)]
struct ChunkRow {
    data: Vec<u8>,
}

/// Wrap a download stream so it is hashed as it flows. The tail item errors the
/// stream when the size or hash does not match the content address (the client sees
/// a failed transfer rather than silently-corrupt bytes), and the download slot is
/// released when the stream ends or is dropped.
fn verified<S>(
    inner: S,
    size: u64,
    sha: String,
    slot: DownloadSlot,
) -> impl Stream<Item = Result<Vec<u8>>>
where
    S: Stream<Item = Result<Vec<u8>>> + 'static,
{
    let state = Rc::new(RefCell::new((StreamHasher::new(), 0u64)));
    let seen = Rc::clone(&state);
    let body = inner.map(move |chunk| -> Result<Vec<u8>> {
        let chunk = chunk?;
        let mut st = seen.borrow_mut();
        st.0.update(&chunk);
        st.1 += chunk.len() as u64;
        Ok(chunk)
    });
    let tail = futures_util::stream::once(async move {
        let _slot = slot;
        let (hasher, n) = std::mem::take(&mut *state.borrow_mut());
        if n == size && hasher.finish_hex() == sha {
            None
        } else {
            console_error!("asset integrity check failed for {sha}");
            Some(Err(worker::Error::RustError(
                "asset integrity check failed".into(),
            )))
        }
    })
    .filter_map(|r: Option<Result<Vec<u8>>>| std::future::ready(r));
    body.chain(tail)
}

/// D1 chunks in order, one chunk fetched per poll (at most one chunk resident).
fn d1_chunks(
    env: Env,
    ws: String,
    id: String,
    size: usize,
    first: Vec<u8>,
) -> impl Stream<Item = Result<Vec<u8>>> {
    let plan = chunk::plan(size);
    futures_util::stream::unfold((Some(first), 1usize), move |(first, next)| {
        let (env, ws, id, plan) = (env.clone(), ws.clone(), id.clone(), plan.clone());
        async move {
            if let Some(f) = first {
                return Some((Ok(f), (None, next)));
            }
            let (seq, start, end) = *plan.get(next)?;
            let fetched = async {
                let db = d1::database(&env)?;
                match d1::first_as::<ChunkRow>(&db, &q::chunk_get(&ws, &id, seq)).await? {
                    Some(c) if c.data.len() == end - start => Ok(c.data),
                    _ => Err(worker::Error::RustError("missing or damaged chunk".into())),
                }
            }
            .await;
            Some((fetched, (None, next + 1)))
        }
    })
}

async fn download(
    env: &Env,
    db: &D1Database,
    ws: &str,
    site: &str,
    sha: &str,
    clock: Clock,
) -> Result<Response> {
    let Some(slot) = DownloadSlot::admit(clock.start()) else {
        return busy(AssetError::DownloadBusy);
    };
    let slot = if clock.hold_slot {
        // Dev-build fault: never release this slot (the deadline reclaims it).
        std::mem::forget(slot);
        return json_resp(ok_json(200, json!({ "ok": true, "held": true })));
    } else {
        slot
    };
    let Some(row) = d1::first(db, &q::asset_get(ws, site, sha)).await? else {
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
    let head = assets_api::download_resp(kind, sha);
    match backend {
        Some(StorageBackend::D1) => {
            // Fetch the first chunk BEFORE answering so a missing asset is a clean
            // 502 rather than a broken 200.
            let first = match d1::first_as::<ChunkRow>(db, &q::chunk_get(ws, id, 0)).await? {
                Some(c) if size > 0 => c.data,
                _ => return err(AssetError::StoreFailed),
            };
            let stream = d1_chunks(
                env.clone(),
                ws.to_string(),
                id.to_string(),
                size as usize,
                first,
            );
            stream_response(head, verified(stream, size, sha.to_string(), slot))
        }
        Some(StorageBackend::R2) => {
            let Some(bucket) = r2_bucket(env) else {
                return err(AssetError::R2NotConfigured);
            };
            let Some(key) = row.get("object_key").and_then(Value::as_str) else {
                return err(AssetError::StoreFailed);
            };
            let Some(obj) = bucket.get(key.to_string()).execute().await? else {
                return err(AssetError::StoreFailed);
            };
            if obj.size() != size {
                console_error!("asset size mismatch for {ws}/{site}/{sha}");
                return err(AssetError::StoreFailed);
            }
            let stream = match obj.body() {
                Some(b) => b.stream()?,
                None => return err(AssetError::StoreFailed),
            };
            stream_response(head, verified(stream, size, sha.to_string(), slot))
        }
        None => err(AssetError::StoreFailed),
    }
}

async fn delete(env: &Env, db: &D1Database, ws: &str, site: &str, sha: &str) -> Result<Response> {
    let Some(row) = d1::first(db, &q::asset_get(ws, site, sha)).await? else {
        return err(AssetError::AssetNotFound);
    };
    let key = row.get("object_key").and_then(Value::as_str);
    // The trigger removes the chunks and tombstones the R2 key in this same statement.
    // The delete is EXACT: it binds the key that was read, so a row replaced since then
    // is left alone (and its object is never removed below).
    if d1::run(db, &q::asset_delete(ws, site, sha, key)).await? == 0 {
        return err(AssetError::AssetNotFound);
    }
    // Only THIS row's own key is removed; a concurrent re-upload writes a different key.
    if let (Some(bucket), Some(key)) = (r2_bucket(env), key) {
        match bucket.delete(key.to_string()).await {
            Ok(()) => {
                if let Err(e) = d1::run(db, &q::tombstone_clear(ws, key)).await {
                    console_error!("tombstone clear failed (reconcile will clear it): {e}");
                }
            }
            // The tombstone stays; reconciliation retries the delete.
            Err(e) => console_error!("r2 delete failed (reconcile will retry): {e}"),
        }
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
            "usage": usage_value(&usage, &limits),
            "assets": assets,
            "generated": { "usage": generated, "policy": gen_policy },
            "provisional": true,
        }),
    ))
}

/// Bounded, idempotent cleanup. Every step touches at most one batch of rows;
/// `more: true` means call again. The R2 orphan listing is paged: pass the returned
/// `next_cursor` back as `?cursor=` to continue past the first page.
async fn reconcile(
    env: &Env,
    db: &D1Database,
    ws: &str,
    site: &str,
    clock: Clock,
    cursor: Option<String>,
) -> Result<Response> {
    let now = clock.start();
    let now_text = utc_text(now);
    let batch = clock.reconcile_batch();
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
        excess += d1::run(db, &q::generated_excess_sweep(ws, site, kind, batch)).await?;
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

    let mut tombstones = 0usize;
    let mut orphan_objects = 0usize;
    let mut next_cursor: Option<String> = None;
    if let Some(bucket) = r2_bucket(env) {
        // Objects of deleted assets/sites/workspaces whose own delete did not finish.
        // This drains only the tombstones of THIS workspace and only when somebody
        // calls reconcile here; tombstones left by deleting a whole workspace (or the
        // last site, after which no site route authorises a reconcile) and prefixes
        // under a deleted site need an operator-level drain job (#64 / #66).
        for row in d1::all(db, &q::tombstones_list(ws, batch)).await? {
            if let Some(key) = row.get("object_key").and_then(Value::as_str) {
                bucket.delete(key.to_string()).await?;
                d1::run(db, &q::tombstone_clear(ws, key)).await?;
                tombstones += 1;
            }
        }
        more |= tombstones >= batch as usize;

        if let Some(prefix) = r2_site_prefix(ws, site) {
            let mut lister = bucket.list().prefix(prefix.clone()).limit(batch);
            if let Some(c) = cursor {
                lister = lister.cursor(c);
            }
            let listed = lister.execute().await?;
            if listed.truncated() {
                more = true;
                next_cursor = listed.cursor();
            }
            for obj in listed.objects() {
                let key = obj.key();
                // Referenced = some row holds this EXACT key; only unreferenced,
                // old-enough keys are ever deleted.
                let referenced = d1::first(db, &q::asset_object_referenced(ws, &key))
                    .await?
                    .is_some();
                let uploaded = obj.uploaded().as_millis() / 1000;
                if object_is_orphan(&prefix, &key, referenced, uploaded, now) {
                    bucket.delete(key).await?;
                    orphan_objects += 1;
                }
            }
        }
    }
    report.insert("tombstoned_objects".into(), json!(tombstones));
    report.insert("orphan_objects".into(), json!(orphan_objects));

    json_resp(ok_json(
        200,
        json!({
            "ok": true,
            "reconciled": Value::Object(report),
            "more": more,
            "next_cursor": next_cursor,
        }),
    ))
}
