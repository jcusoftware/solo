//! Worker-side HTTP `/sync` entry (wasm only, #68).
//!
//! Order (fixed): method (405), framing/bounds (400/411/413/414/415),
//! authenticate (401), CSRF Origin gate on cookie POSTs (403), workspace
//! membership + credential scope check (401/403), site existence (404), body
//! stream under cap, per-message permission check (`Permission::Publish` for
//! `seed` and `publish` checkpoints, `Permission::Edit` for batches/undo/save),
//! then forward to the site's `SiteSession` Durable Object (`/internal/sync`).

use futures_util::StreamExt;
use worker::{Env, Headers, Method, Request, RequestInit, Response, Result};

use crate::auth::policy::{Permission, authorize, effective_role};
use crate::bounds::{self, BodyLimiter, MAX_URL_BYTES};
use crate::repo;
use crate::respond::{self, Resp, json_error};
use crate::session::entry::{member_role, stub};
use crate::session::http::to_response;
use crate::session::protocol;
use crate::session::sync::{self, SyncRoute};
use crate::session::{
    H_CREDENTIAL, H_ROLE, H_SITE, H_USER, H_WORKSPACE, MAX_MESSAGE_BYTES, PATH_SYNC,
};

pub async fn handle(mut req: Request, env: &Env, route: SyncRoute) -> Result<Response> {
    let method = req.method().to_string();
    if method != "GET" && method != "POST" {
        return to_response(Resp::new(
            405,
            Some(sws_security::response::error_body("method_not_allowed")),
            &[("Allow", "GET, POST")],
        ));
    }
    let url = req.url()?;
    let url_len = url.path().len() + url.query().map_or(0, |q| q.len() + 1);
    if url_len > MAX_URL_BYTES {
        return to_response(respond::reject(bounds::Reject::UriTooLong));
    }
    let headers = req.headers();
    let content_length = headers.get("Content-Length")?;
    let cap = if method == "POST" {
        MAX_MESSAGE_BYTES
    } else {
        bounds::MAX_BODY_BYTES
    };
    if let Err(rej) = bounds::check_request_with_cap(
        &method,
        url_len,
        content_length.as_deref(),
        headers.get("Transfer-Encoding")?.as_deref(),
        headers.get("Content-Type")?.as_deref(),
        cap,
    ) {
        return to_response(respond::reject(rej));
    }

    let Some(principal) = crate::auth_http::authenticate(&req, env).await? else {
        return to_response(json_error(401, "unauthenticated", &[]));
    };
    if !crate::auth_http::csrf_ok(&req, env, &principal, &method) {
        return to_response(json_error(403, "forbidden_origin", &[]));
    }

    let role = member_role(env, &route.ws, principal.user_id).await?;
    let base_perm = if method == "POST" {
        Permission::Edit
    } else {
        Permission::Read
    };
    if let Err(d) = authorize(&principal.credential, role, &route.ws, base_perm) {
        return to_response(json_error(d.status(), d.code(), &[]));
    }
    let Some(role) = role else {
        return to_response(json_error(403, "forbidden", &[]));
    };

    let db = crate::d1::database(env)?;
    if crate::d1::first(&db, &repo::site_get(&route.ws, &route.site))
        .await?
        .is_none()
    {
        return to_response(json_error(404, "not_found", &[]));
    }

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

    if method == "POST" {
        let Ok(text) = std::str::from_utf8(&body) else {
            return to_response(json_error(400, "invalid_utf8", &[]));
        };
        let msg = match protocol::parse_client(text) {
            Ok(m) => m,
            Err(pe) => {
                let err_body = protocol::error(
                    pe.code,
                    &pe.message,
                    pe.op_id.as_deref(),
                    pe.command_error.as_ref(),
                );
                return to_response(Resp::new(400, Some(err_body), &[]));
            }
        };
        let Some(perm) = sync::required_permission(&msg) else {
            let err_body = protocol::error(
                "invalid_message",
                "message type is not valid for POST /sync",
                None,
                None,
            );
            return to_response(Resp::new(400, Some(err_body), &[]));
        };
        if let Err(d) = authorize(&principal.credential, Some(role), &route.ws, perm) {
            return to_response(json_error(d.status(), d.code(), &[]));
        }
    }

    let eff_role = effective_role(role, &principal.credential);
    let fwd_headers = Headers::new();
    fwd_headers.set(H_WORKSPACE, &route.ws)?;
    fwd_headers.set(H_SITE, &route.site)?;
    fwd_headers.set(H_USER, &principal.user_id.to_string())?;
    fwd_headers.set(H_ROLE, eff_role.as_str())?;
    if let Some(tag) = principal.credential.tag() {
        fwd_headers.set(H_CREDENTIAL, &tag)?;
    }

    let mut init = RequestInit::new();
    if method == "POST" {
        fwd_headers.set("Content-Type", "application/json")?;
        init.with_method(Method::Post)
            .with_headers(fwd_headers)
            .with_body(Some(
                String::from_utf8_lossy(&body).into_owned().into(),
            ));
    } else {
        init.with_method(Method::Get).with_headers(fwd_headers);
    }

    let target = match url.query() {
        Some(q) if !q.is_empty() && method == "GET" => {
            format!("https://session.internal{PATH_SYNC}?{q}")
        }
        _ => format!("https://session.internal{PATH_SYNC}"),
    };
    let forward = Request::new_with_init(&target, &init)?;
    stub(env, &route.ws, &route.site)?
        .fetch_with_request(forward)
        .await
}
