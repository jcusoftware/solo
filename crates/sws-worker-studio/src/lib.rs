//! sws-worker-studio: optional Studio control-plane Worker (workers-rs) scaffold.
//!
//! Scope of issue #61: the D1 workspace registry (`migrations/0001_registry.sql`),
//! tenant-scoped repository SQL ([`repo`]), strict request bounds ([`bounds`])
//! and `GET|HEAD /api/health`. Everything else answers a JSON 404/405.
//! The bounded asset store (#63: [`assets_api`], [`asset_repo`], `sws-assets`)
//! serves `/api/sites/{workspace_id}/{site_id}/assets[/...]`. Per-site Durable
//! Object command sessions (#62) live in [`session`], reached through
//! `GET /api/sites/{workspace_id}/{site_id}/session` (WebSocket). GitHub OAuth
//! sessions, invitations and scoped CLI/MCP tokens (#65) live in [`auth`]
//! (pure) and `auth_http` (wasm), and guard every protected route. Asynchronous
//! publish jobs (#64) live in [`publish`]: `POST|GET /api/sites/{workspace_id}/{site_id}/publish[/{job_id}]`,
//! driven off the request path by the Cron Trigger (`scheduled` below) and, when a
//! Queue is bound, the `queue` consumer. Direct Upload publishing to the site Worker
//! and the audited rollback route (#66) live in [`deploy`]. Draft previews (#67,
//! [`preview`]) are served by this Worker on a SEPARATE host (`STUDIO_PREVIEW_ORIGIN`)
//! that reaches only the preview routes, with the `preview` header profile.
//!
//! Write-frequency invariant: the registry is written only on explicit
//! save/publish/checkpoint actions, never once per edit. There is deliberately
//! no HTTP edit route; per-edit state belongs to the session layer ([`session`]),
//! which writes one `site_revisions` row per checkpoint.
//!
//! Configuration is separate from the visitor Worker: its own Wrangler config
//! (`wrangler.studio.jsonc`), its own `STUDIO_DB` binding and database. All
//! responses are decided by the host-testable [`respond`] layer, which applies
//! the `studio` security-header profile
//! ([`sws_security::headers::STUDIO_HEADERS`]) and `Cache-Control: no-store`;
//! the wasm glue below only adapts `worker` types to it.

pub mod asset_repo;
pub mod assets_api;
pub mod auth;
pub mod auth_repo;
pub mod bounds;
pub mod deploy;
pub mod model;
pub mod preview;
pub mod publish;
pub mod repo;
pub mod respond;
pub mod routes;
pub mod session;

#[cfg(target_arch = "wasm32")]
pub mod d1;

#[cfg(target_arch = "wasm32")]
mod assets_http;

#[cfg(target_arch = "wasm32")]
pub mod auth_http;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use futures_util::StreamExt;
    use worker::{
        Context, Env, MessageBatch, MessageExt, Request, Response, Result, ScheduleContext,
        ScheduledEvent, console_error, console_log, event,
    };

    use sws_security::headers::{HeaderProfile, with_profile_headers};
    use sws_security::worker_utils::var;

    use crate::bounds::{self, BodyLimiter};
    use crate::respond::{self, RequestFacts, Resp};
    use crate::routes::{self, Route};
    use crate::{preview, session};

    /// Either a response decided by the pure layer (headers already applied) or
    /// a raw one from the Durable Object (a 101 WebSocket upgrade has immutable
    /// headers and must reach the client untouched).
    enum Outcome {
        Local(Resp),
        Raw(Response),
    }

    #[event(start)]
    fn start() {
        #[cfg(feature = "console_error_panic_hook")]
        console_error_panic_hook::set_once();
    }

    /// Cron Trigger: the Workers Free (queue-less) driver and the recovery sweep for
    /// every deployment. Drives due publish jobs (queued, backing off, or whose
    /// driver's lease expired), capped per run. Asset reconciliation stays on its
    /// authenticated per-site route (it needs a site and an R2 listing cursor).
    #[event(scheduled)]
    async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
        let opts = crate::publish::drive::DriveOptions::from_env(&env);
        match crate::publish::drive::run_due(&env, &opts).await {
            Ok(report) => console_log!("publish sweep: {report}"),
            Err(e) => console_error!("publish sweep failed: {e}"),
        }
    }

    /// Optional Queue consumer (only when a `PUBLISH_QUEUE` consumer is configured).
    /// Each message is a pointer; the same driver as the Cron Trigger runs it. A
    /// malformed message is acknowledged and dropped; a D1 error retries the message
    /// (and the Cron Trigger recovers the job regardless).
    #[event(queue)]
    async fn queue(batch: MessageBatch<serde_json::Value>, env: Env, _ctx: Context) -> Result<()> {
        let opts = crate::publish::drive::DriveOptions::from_env(&env);
        for msg in batch.raw_iter() {
            let pointer = msg
                .body()
                .as_string()
                .as_deref()
                .and_then(crate::publish::parse_pointer);
            let Some(p) = pointer else {
                console_error!("dropping a malformed publish queue message");
                msg.ack();
                continue;
            };
            match crate::publish::drive::drive_pointer(&env, &p, &opts).await {
                Ok(_) => msg.ack(),
                Err(e) => {
                    console_error!("publish job {}/{}: {e}", p.workspace_id, p.job_id);
                    msg.retry();
                }
            }
        }
        Ok(())
    }

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        // Draft previews (#67) FIRST: a request for the preview host reaches only the
        // preview routes (no Studio API, auth or asset route exists there), and is
        // answered with the `preview` profile. Every other host is the Studio.
        let preview_cfg = preview::origin::configure(
            var(&env, preview::origin::STUDIO_ORIGIN_VAR).as_deref(),
            var(&env, preview::origin::PREVIEW_ORIGIN_VAR).as_deref(),
        );
        let url = req.url().ok();
        if let Some(u) = &url
            && preview::origin::surface(
                &preview_cfg,
                u.host_str().unwrap_or(""),
                u.port_or_known_default(),
            ) == preview::origin::Surface::Preview
        {
            let studio = preview_cfg.origins().map(|o| o.studio.clone());
            return preview::http::serve(req, &env, studio.as_deref()).await;
        }
        if let Some(route) = url
            .as_ref()
            .and_then(|u| preview::serve::parse_producer(u.path()))
        {
            let resp = match preview::http::produce(req, &env, &preview_cfg, route).await {
                Ok(r) => r,
                Err(e) => {
                    console_error!("unhandled preview error: {e}");
                    to_response(respond::internal_error())?
                }
            };
            return with_profile_headers(resp, HeaderProfile::Studio, None);
        }
        // Preview paths are never served on the Studio host: 404 through the normal
        // routing below, or 503 when no valid, separate preview origin is configured.
        if preview_cfg.origins().is_none()
            && url
                .as_ref()
                .is_some_and(|u| preview::origin::is_preview_path(u.path()))
        {
            return with_profile_headers(
                to_response(respond::json_error(
                    503,
                    "preview_origin_not_configured",
                    &[],
                ))?,
                HeaderProfile::Studio,
                None,
            );
        }
        // Publish routes (#64) carry a manifest body larger than the 64 KiB JSON bound
        // and are authorized by `publish::authz::require_publish`.
        if let Some(route) = url
            .as_ref()
            .and_then(|u| crate::publish::parse_route(u.path()))
        {
            let resp = match crate::publish::http::handle(req, &env, route).await {
                Ok(r) => r,
                Err(e) => {
                    console_error!("unhandled publish error: {e}");
                    to_response(respond::internal_error())?
                }
            };
            return with_profile_headers(resp, HeaderProfile::Studio, None);
        }
        // Asset routes (#63) are authenticated, tenant-scoped, and carry uploads
        // larger than the 64 KiB JSON bound, so they are decided before `handle`.
        if let Some(route) = req
            .url()
            .ok()
            .and_then(|u| crate::assets_api::parse_route(u.path()))
        {
            let resp = match crate::assets_http::handle(req, &env, route).await {
                Ok(r) => r,
                Err(e) => {
                    console_error!("unhandled asset error: {e}");
                    to_response(respond::internal_error())?
                }
            };
            return with_profile_headers(resp, HeaderProfile::Studio, None);
        }
        // Rollback (#66): authenticated, audited re-activation of a retained deployment.
        if let Some(route) = req
            .url()
            .ok()
            .and_then(|u| crate::deploy::route::parse(u.path()))
        {
            let resp = match crate::deploy::wasm::handle_rollback(req, &env, route).await {
                Ok(r) => r,
                Err(e) => {
                    console_error!("unhandled deploy error: {e}");
                    to_response(respond::internal_error())?
                }
            };
            return with_profile_headers(resp, HeaderProfile::Studio, None);
        }
        // Authentication, token and membership routes (#65) read small JSON or
        // form bodies and answer HTML/redirects, so they are decided before `handle`.
        if let Some(route) = req
            .url()
            .ok()
            .and_then(|u| crate::auth::routes::parse(req.method().as_ref(), u.path()))
        {
            return match crate::auth_http::handle(req, &env, route).await {
                Ok(r) => Ok(r),
                Err(e) => {
                    console_error!(
                        "unhandled auth error: {}",
                        crate::auth::secret::redact(&e.to_string())
                    );
                    with_profile_headers(
                        to_response(respond::internal_error())?,
                        HeaderProfile::Studio,
                        None,
                    )
                }
            };
        }
        let resp = match handle(req, &env).await {
            Ok(r) => r,
            Err(e) => {
                console_error!("unhandled error: {e}");
                Outcome::Local(respond::internal_error())
            }
        };
        match resp {
            Outcome::Raw(r) => Ok(r),
            // Idempotent: the pure layer already carries the same `studio` headers.
            Outcome::Local(r) => with_profile_headers(to_response(r)?, HeaderProfile::Studio, None),
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

    async fn handle(mut req: Request, env: &Env) -> Result<Outcome> {
        let method = req.method().to_string();
        let url = req.url()?;
        let path = url.path().to_string();
        let url_len = path.len() + url.query().map_or(0, |q| q.len() + 1);
        let headers = req.headers();
        let content_length = headers.get("Content-Length")?;
        let transfer_encoding = headers.get("Transfer-Encoding")?;
        let content_type = headers.get("Content-Type")?;
        let upgrade = headers.get("Upgrade")?;
        let facts = RequestFacts {
            method: &method,
            path: &path,
            url_len,
            content_length: content_length.as_deref(),
            transfer_encoding: transfer_encoding.as_deref(),
            content_type: content_type.as_deref(),
        };

        if let Err(r) = respond::precheck(&facts) {
            return Ok(Outcome::Local(r));
        }

        // Do not trust headers: count the bytes actually streamed, for every
        // method. No route accepts a body yet, so any body is drained under the
        // cap and dropped; `respond` turns an unexpected one into 400/413.
        let mut seen = 0usize;
        if let Ok(mut stream) = req.stream() {
            let mut limiter =
                BodyLimiter::new(bounds::drain_limit(&method, content_length.as_deref()));
            while let Some(chunk) = stream.next().await {
                let n = chunk?.len();
                seen = seen.saturating_add(n);
                if limiter.push(n).is_err() {
                    break;
                }
            }
        }

        // Site command sessions (#62): a WebSocket upgrade goes to the site's
        // Durable Object after authentication and membership checks. A plain GET
        // falls through to the pure layer, which answers 426.
        if seen == 0
            && let Route::Session {
                workspace_id,
                site_id,
            } = routes::route(&method, &path)
            && upgrade
                .as_deref()
                .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        {
            return Ok(Outcome::Raw(
                session::entry::open(req, env, &workspace_id, &site_id).await?,
            ));
        }

        // Test-only routes: exist only in the `insecure-dev-auth` build, behind the
        // runtime fence; every other build falls straight through.
        #[cfg(feature = "insecure-dev-auth")]
        if seen == 0
            && let Some(r) = session::dev::handle(&req, env, &method, &path).await?
        {
            return Ok(Outcome::Raw(r));
        }

        let version =
            var(env, "STUDIO_VERSION").unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        Ok(Outcome::Local(respond::respond(&facts, seen, &version)))
    }
}
