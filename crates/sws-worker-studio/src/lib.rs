//! sws-worker-studio: optional Studio control-plane Worker (workers-rs) scaffold.
//!
//! Scope of issue #61: the D1 workspace registry (`migrations/0001_registry.sql`),
//! tenant-scoped repository SQL ([`repo`]), strict request bounds ([`bounds`])
//! and `GET|HEAD /api/health`. Everything else answers a JSON 404/405.
//! The bounded asset store (#63: [`assets_api`], [`asset_repo`], `sws-assets`)
//! serves `/api/sites/{workspace_id}/{site_id}/assets[/...]`. Per-site Durable
//! Object command sessions (#62) live in [`session`], reached through
//! `GET /api/sites/{workspace_id}/{site_id}/session` (WebSocket). Sessions/OAuth
//! (#65) and the publish pipeline (#64/#66) are out of scope; [`auth`] keeps
//! every protected route closed until #65.
//!
//! Write-frequency invariant: the registry is written only on explicit
//! save/publish/checkpoint actions, never once per edit. There is deliberately
//! no HTTP edit route; per-edit state belongs to the session layer ([`session`]),
//! which writes one `site_revisions` row per checkpoint.
//!
//! Configuration is separate from the visitor Worker: its own Wrangler config
//! (`wrangler.studio.jsonc`), its own `STUDIO_DB` binding and database. All
//! responses are decided by the host-testable [`respond`] layer, which applies
//! [`sws_security::headers::SECURITY_HEADERS`] and `Cache-Control: no-store`;
//! the wasm glue below only adapts `worker` types to it.

pub mod asset_repo;
pub mod assets_api;
pub mod auth;
pub mod bounds;
pub mod model;
pub mod repo;
pub mod respond;
pub mod routes;
pub mod session;

#[cfg(target_arch = "wasm32")]
pub mod d1;

#[cfg(target_arch = "wasm32")]
mod assets_http;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use futures_util::StreamExt;
    use worker::{Context, Env, Request, Response, Result, console_error, event};

    use sws_security::headers::with_security_headers;
    use sws_security::worker_utils::var;

    use crate::bounds::{self, BodyLimiter};
    use crate::respond::{self, RequestFacts, Resp};
    use crate::routes::{self, Route};
    use crate::session;

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

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
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
            return with_security_headers(resp);
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
            // Idempotent (`set`): the pure layer already carries the same headers.
            Outcome::Local(r) => with_security_headers(to_response(r)?),
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
