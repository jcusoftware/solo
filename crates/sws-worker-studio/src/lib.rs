//! sws-worker-studio: optional Studio control-plane Worker (workers-rs) scaffold.
//!
//! Scope of issue #61: the D1 workspace registry (`migrations/0001_registry.sql`),
//! tenant-scoped repository SQL ([`repo`]), strict request bounds ([`bounds`])
//! and `GET|HEAD /api/health`. Everything else answers a JSON 404/405.
//! Sessions/OAuth (#65), Durable Object edit sessions (#62), blob storage (#63)
//! and the publish pipeline (#64/#66) are out of scope.
//!
//! Write-frequency invariant: the registry is written only on explicit
//! save/publish/checkpoint actions, never once per edit. There is deliberately
//! no edit route; per-edit state belongs to the session layer (#62).
//!
//! Configuration is separate from the visitor Worker: its own Wrangler config
//! (`wrangler.studio.jsonc`), its own `STUDIO_DB` binding and database. All
//! responses pass through [`sws_security::headers::with_security_headers`] and
//! carry `Cache-Control: no-store`.

pub mod bounds;
pub mod model;
pub mod repo;
pub mod routes;

#[cfg(target_arch = "wasm32")]
pub mod d1;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use futures_util::StreamExt;
    use worker::{Context, Env, Request, Response, Result, console_error, event};

    use sws_security::headers::{API_CACHE_CONTROL, with_security_headers};
    use sws_security::response::error_body;
    use sws_security::worker_utils::{json_response, var};

    use crate::bounds::{self, BodyLimiter, Reject};
    use crate::routes::{Route, health_body, route};

    #[event(start)]
    fn start() {
        #[cfg(feature = "console_error_panic_hook")]
        console_error_panic_hook::set_once();
    }

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        let resp = match handle(req, &env).await {
            Ok(r) => r,
            Err(e) => {
                console_error!("unhandled error: {e}");
                json_response(500, &error_body("internal_error"))?
            }
        };
        with_security_headers(resp)
    }

    fn reject_response(r: Reject) -> Result<Response> {
        let resp = json_response(r.status(), &error_body(r.code()))?;
        if r == Reject::MethodNotAllowed {
            resp.headers().set("Allow", bounds::ALLOW_HEADER)?;
        }
        Ok(resp)
    }

    async fn handle(mut req: Request, env: &Env) -> Result<Response> {
        let method = req.method().to_string();
        let url = req.url()?;
        let url_len = url.path().len() + url.query().map_or(0, |q| q.len() + 1);
        let headers = req.headers();
        let content_length = headers.get("Content-Length")?;
        let transfer_encoding = headers.get("Transfer-Encoding")?;
        let content_type = headers.get("Content-Type")?;

        if let Err(r) = bounds::check_request(
            &method,
            url_len,
            content_length.as_deref(),
            transfer_encoding.as_deref(),
            content_type.as_deref(),
        ) {
            return reject_response(r);
        }

        // Do not trust the header: count the bytes actually streamed. No route
        // accepts a body yet, so the body is drained under the cap and dropped.
        if let Some(declared) = content_length
            .as_deref()
            .and_then(|v| v.trim().parse().ok())
            && declared > 0
        {
            let mut limiter = BodyLimiter::new(declared);
            let mut stream = req.stream()?;
            while let Some(chunk) = stream.next().await {
                if let Err(r) = limiter.push(chunk?.len()) {
                    return reject_response(r);
                }
            }
        }

        match route(&method, url.path()) {
            Route::Health => {
                let version = var(env, "STUDIO_VERSION")
                    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
                json_response(200, &health_body(&version))
            }
            Route::HealthHead => {
                let resp = Response::empty()?.with_status(200);
                resp.headers().set("Content-Type", "application/json")?;
                resp.headers().set("Cache-Control", API_CACHE_CONTROL)?;
                Ok(resp)
            }
            Route::MethodNotAllowed(allow) => {
                let resp = json_response(405, &error_body("method_not_allowed"))?;
                resp.headers().set("Allow", allow)?;
                Ok(resp)
            }
            Route::NotFound => json_response(404, &error_body("not_found")),
        }
    }
}
