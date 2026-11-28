//! wasm-only glue for publishing: the `worker::Fetch` transport, the Worker runtime
//! (clock, `Delay`, console), the secret lookup, the D1 [`PublishStore`], and the
//! rollback route handler. Every decision lives in the host-tested modules; this file
//! only performs I/O.
//!
//! #64 builds a [`PublishEnv`] from these pieces (`FetchClient`, `WorkerRuntime`,
//! `D1Store`, `EnvSecrets`, [`super::CLOUDFLARE_API_BASE`]) and calls
//! [`super::run_publish`] from its uploading/activation phase.

use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::{Either, select};
use worker::{
    AbortController, D1Database, Date, Delay, Env, Fetch, Headers, Method, Request, RequestInit,
    Response, Result, console_log, js_sys,
};

use crate::bounds::{self, BodyLimiter, MAX_URL_BYTES};
use crate::model::Role;
use crate::respond::{self, Resp};
use crate::{d1, repo};

use super::http::{HttpClient, HttpRequest, HttpResponse, Runtime, Secrets, TransportError};
use super::pipeline::PublishEnv;
use super::rollback::{RollbackRequest, run_rollback};
use super::route::{self, RollbackRoute};
use super::secret::ApiToken;
use super::sql;
use super::store::{AuditRow, LeaseClaim, PublishStore, PublishedVersion, StoreError, Target};

/// `fetch` with a hard timeout (the request is aborted when it fires).
pub struct FetchClient;

impl HttpClient for FetchClient {
    async fn send(&self, req: HttpRequest) -> std::result::Result<HttpResponse, TransportError> {
        let net = |m: &str| TransportError::Network(m.to_string());
        let headers = Headers::new();
        for (k, v) in &req.headers {
            headers
                .set(k, v)
                .map_err(|_| net("invalid request header"))?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::from(req.method.to_string()))
            .with_headers(headers);
        if !req.body.is_empty() {
            init.with_body(Some(js_sys::Uint8Array::from(req.body.as_slice()).into()));
        }
        let request =
            Request::new_with_init(&req.url, &init).map_err(|_| net("invalid request"))?;
        let controller = AbortController::default();
        let signal = controller.signal();
        let fetch = Fetch::Request(request);
        // The timeout covers the whole exchange, response body included.
        let work = std::pin::pin!(async {
            let mut resp = fetch.send_with_signal(&signal).await?;
            let retry_after_secs = resp
                .headers()
                .get("Retry-After")
                .ok()
                .flatten()
                .and_then(|v| v.trim().parse::<u64>().ok());
            let status = resp.status_code();
            let body = resp.bytes().await?;
            Ok::<_, worker::Error>(HttpResponse {
                status,
                retry_after_secs,
                body,
            })
        });
        let timeout = std::pin::pin!(Delay::from(Duration::from_millis(req.timeout_ms)));
        match select(work, timeout).await {
            Either::Left((Ok(resp), _)) => Ok(resp),
            // The JS error text is not forwarded: it carries nothing actionable and must
            // never be a channel for request details.
            Either::Left((Err(_), _)) => Err(net("fetch failed")),
            Either::Right(_) => {
                controller.abort();
                Err(TransportError::Timeout)
            }
        }
    }
}

pub struct WorkerRuntime;

impl Runtime for WorkerRuntime {
    fn now_secs(&self) -> u64 {
        Date::now().as_millis() / 1000
    }

    async fn sleep_ms(&self, ms: u64) {
        Delay::from(Duration::from_millis(ms)).await;
    }

    fn log(&self, line: &str) {
        console_log!("{line}");
    }
}

/// Reads the API token from a Worker secret by name (`wrangler secret put`).
pub struct EnvSecrets<'a>(pub &'a Env);

impl Secrets for EnvSecrets<'_> {
    fn api_token(&self, secret_name: &str) -> Option<ApiToken> {
        self.0
            .secret(secret_name)
            .ok()
            .map(|s| ApiToken::new(s.to_string()))
    }
}

pub struct D1Store {
    pub db: D1Database,
}

fn se(e: worker::Error) -> StoreError {
    // D1 errors carry SQL/constraint text, never secrets; keep them short anyway.
    StoreError(e.to_string().chars().take(200).collect())
}

impl D1Store {
    async fn changed(&self, stmt: &repo::Statement) -> std::result::Result<bool, StoreError> {
        Ok(d1::run(&self.db, stmt).await.map_err(se)? == 1)
    }
}

impl PublishStore for D1Store {
    async fn target(
        &self,
        ws: &str,
        site: &str,
    ) -> std::result::Result<Option<Target>, StoreError> {
        d1::first_as::<Target>(&self.db, &sql::target_get(ws, site))
            .await
            .map_err(se)
    }

    async fn role(&self, ws: &str, user_id: i64) -> std::result::Result<Option<Role>, StoreError> {
        let row = d1::first(&self.db, &repo::membership_get(ws, user_id))
            .await
            .map_err(se)?;
        Ok(row
            .as_ref()
            .and_then(|r| r.get("role"))
            .and_then(|r| r.as_str())
            .and_then(Role::parse))
    }

    async fn acquire(&self, t: &Target, c: &LeaseClaim) -> std::result::Result<bool, StoreError> {
        self.changed(&sql::acquire(t, c)).await
    }

    async fn renew(
        &self,
        t: &Target,
        job: &str,
        until: &str,
        now: &str,
    ) -> std::result::Result<bool, StoreError> {
        self.changed(&sql::renew(t, job, until, now)).await
    }

    async fn release(&self, t: &Target, job: &str) -> std::result::Result<(), StoreError> {
        d1::run(&self.db, &sql::release(t, job)).await.map_err(se)?;
        Ok(())
    }

    async fn clear_stale(
        &self,
        t: &Target,
        stale_job: &str,
        now: &str,
        adopt: bool,
    ) -> std::result::Result<bool, StoreError> {
        self.changed(&sql::clear_stale(t, stale_job, now, adopt))
            .await
    }

    async fn record_publish(
        &self,
        t: &Target,
        job: &str,
        v: &PublishedVersion,
        audit: &AuditRow,
    ) -> std::result::Result<bool, StoreError> {
        // One transaction: target update first; the retained row only if it applied.
        let changes = d1::batch(
            &self.db,
            &[
                sql::record_publish(t, job, v, &audit.at),
                sql::version_insert_if_live(v),
                sql::audit_insert(audit),
            ],
        )
        .await
        .map_err(se)?;
        Ok(changes.first() == Some(&1))
    }

    async fn record_rollback(
        &self,
        t: &Target,
        job: &str,
        v: &PublishedVersion,
        audit: &AuditRow,
    ) -> std::result::Result<bool, StoreError> {
        let changes = d1::batch(
            &self.db,
            &[
                sql::record_rollback(t, job, v, &audit.at),
                sql::audit_insert(audit),
            ],
        )
        .await
        .map_err(se)?;
        Ok(changes.first() == Some(&1))
    }

    async fn published(
        &self,
        ws: &str,
        site: &str,
        id: &str,
    ) -> std::result::Result<Option<PublishedVersion>, StoreError> {
        d1::first_as::<PublishedVersion>(&self.db, &sql::version_get(ws, site, id))
            .await
            .map_err(se)
    }

    async fn audit(&self, row: &AuditRow) -> std::result::Result<(), StoreError> {
        d1::run(&self.db, &sql::audit_insert(row))
            .await
            .map_err(se)?;
        Ok(())
    }
}

fn to_response(r: Resp) -> Result<Response> {
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

/// Cloudflare API base URL. Production builds ALWAYS use [`super::CLOUDFLARE_API_BASE`];
/// the `insecure-dev-auth` test build may point at a loopback fake via
/// `STUDIO_DEV_CF_API_BASE` only when the runtime fence is open.
#[cfg(feature = "insecure-dev-auth")]
pub(crate) fn api_base(req: Option<&Request>, env: &Env) -> String {
    let fence_open = match req {
        Some(r) => crate::session::entry::dev_fence_open(r, env),
        None => {
            sws_security::worker_utils::var(env, crate::session::devfence::FENCE_VAR)
                .as_deref()
                .map(str::trim)
                == Some("1")
        }
    };
    if fence_open
        && let Some(url) = sws_security::worker_utils::var(env, "STUDIO_DEV_CF_API_BASE")
    {
        let u = url.trim().trim_end_matches('/');
        if u.starts_with("http://127.0.0.1:") || u.starts_with("http://localhost:") {
            return u.to_string();
        }
    }
    super::CLOUDFLARE_API_BASE.to_string()
}

#[cfg(not(feature = "insecure-dev-auth"))]
pub(crate) fn api_base(_req: Option<&Request>, _env: &Env) -> String {
    super::CLOUDFLARE_API_BASE.to_string()
}

/// `POST /api/sites/{ws}/{site}/deployments/{published_id}/rollback`. The body is
/// optional; when present it must be a JSON object (no field is read today).
pub async fn handle_rollback(
    mut req: Request,
    env: &Env,
    route: RollbackRoute,
) -> Result<Response> {
    let method = req.method().to_string();
    if method != "POST" {
        return to_response(route::method_not_allowed());
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
    if let Err(rej) = bounds::check_request(
        &method,
        url_len,
        content_length.as_deref(),
        transfer_encoding.as_deref(),
        content_type.as_deref(),
    ) {
        return to_response(respond::reject(rej));
    }

    let Some(principal) = crate::auth_http::authenticate(&req, env).await? else {
        return to_response(route::unauthenticated());
    };
    if !crate::auth_http::csrf_ok(&req, env, &principal, &method) {
        return to_response(respond::json_error(403, "forbidden_origin", &[]));
    }

    let base = api_base(Some(&req), env);

    // Count the bytes actually streamed (never trust Content-Length alone).
    let limit = bounds::drain_limit(&method, content_length.as_deref());
    let mut body = Vec::new();
    if let Ok(mut stream) = req.stream() {
        let mut limiter = BodyLimiter::new(limit);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if limiter.push(chunk.len()).is_err() {
                return to_response(respond::reject(bounds::Reject::PayloadTooLarge));
            }
            body.extend_from_slice(&chunk);
        }
    }
    if !body.is_empty()
        && !serde_json::from_slice::<serde_json::Value>(&body).is_ok_and(|v| v.is_object())
    {
        return to_response(respond::reject(bounds::Reject::BadRequest));
    }

    let store = D1Store {
        db: d1::database(env)?,
    };
    let secrets = EnvSecrets(env);
    let publish_env = PublishEnv {
        http: &FetchClient,
        rt: &WorkerRuntime,
        store: &store,
        secrets: &secrets,
        api_base: &base,
    };
    let request = RollbackRequest {
        workspace_id: route.workspace_id,
        site_id: route.site_id,
        published_id: route.published_id,
        principal,
    };
    match run_rollback(&request, &publish_env).await {
        Ok(out) => to_response(route::ok(&out)),
        Err(e) => to_response(route::err(&e)),
    }
}
