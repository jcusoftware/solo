//! wasm-only glue for Studio authentication (#65): resolve the caller from a
//! bearer token or the session cookie, the GitHub OAuth flow, logout, token
//! and membership management, and the Turnstile-protected invitation form.
//! Every decision lives in the host-tested [`crate::auth`] modules and every
//! statement in [`crate::auth_repo`]; this file performs I/O only.
//!
//! Secrets: `GITHUB_OAUTH_CLIENT_SECRET` and `TURNSTILE_SECRET_KEY` are Worker
//! secrets. Nothing credential-shaped is ever logged: every logged error goes
//! through [`redact`], and credentials are only ever returned in the one
//! response that mints them.

use futures_util::StreamExt;
use serde_json::{Value, json};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::{
    D1Database, Date, Env, Fetch, Headers, Method, Request, RequestInit, Response, Result,
    console_error, console_log, js_sys,
};

use sws_security::headers::HeaderProfile;
use sws_security::validate::{TURNSTILE_FIELD, parse_form_urlencoded};
use sws_security::worker_utils::{secret as env_secret, var, verify_turnstile, with_timeout};

use crate::auth::cookie::{self, OAUTH_COOKIE, SESSION_COOKIE};
use crate::auth::expiry::{self, SessionStatus};
use crate::auth::html;
use crate::auth::oauth::{self, SignIn};
use crate::auth::policy::{self, Denied, Permission};
use crate::auth::routes::{AuthRoute, BodyKind, MAX_AUTH_BODY, check_framing};
use crate::auth::secret::{self, Kind, redact};
use crate::auth::{Credential, CredentialTag, Principal, parse_tag};
use crate::auth_repo as q;
use crate::bounds::BodyLimiter;
use crate::d1;
use crate::model::{Role, Scope};
use crate::repo;
use crate::respond::{self, Resp};

/// Outbound GitHub calls give up after this long.
const OUTBOUND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Bootstrap workspace id when `STUDIO_BOOTSTRAP_WORKSPACE` is unset.
pub const DEFAULT_BOOTSTRAP_WORKSPACE: &str = "studio";
/// Durable Object calls one revocation may make for an immediate socket close.
/// Beyond it, sockets stop within the recheck bound anyway (Free plan
/// subrequest limits make an unbounded fan-out unsafe).
const MAX_REVOKE_FANOUT: usize = 25;
/// Where a completed sign-in lands.
const AFTER_SIGN_IN: &str = "/auth/session";

pub fn now_secs() -> i64 {
    (Date::now().as_millis() / 1000) as i64
}

/// Cryptographically secure random bytes (`crypto.getRandomValues`).
pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let global = js_sys::global();
    let crypto = js_sys::Reflect::get(&global, &JsValue::from_str("crypto"))?;
    let f: js_sys::Function = js_sys::Reflect::get(&crypto, &JsValue::from_str("getRandomValues"))?
        .dyn_into()
        .map_err(|_| worker::Error::RustError("crypto.getRandomValues unavailable".into()))?;
    let arr = js_sys::Uint8Array::new_with_length(N as u32);
    f.call1(&crypto, &arr)?;
    let mut out = [0u8; N];
    arr.copy_to(&mut out);
    Ok(out)
}

fn log_err(what: &str, e: &dyn std::fmt::Display) {
    console_error!("auth: {what}: {}", redact(&e.to_string()));
}

// --- Responses ----------------------------------------------------------------------

/// Turn a decided response into a `worker::Response`. Headers are APPENDED so a
/// `Set-Cookie` can never be merged or dropped; `Resp` already carries the full
/// security header set and `Cache-Control: no-store`.
fn send(r: Resp, html_body: Option<String>) -> Result<Response> {
    let resp = match (&html_body, &r.body) {
        (Some(h), _) => Response::from_bytes(h.clone().into_bytes())?,
        (None, Some(b)) => Response::from_bytes(serde_json::to_vec(b)?)?,
        (None, None) => Response::empty()?,
    }
    .with_status(r.status);
    let headers = Headers::new();
    for (k, v) in &r.headers {
        if html_body.is_some() && k.eq_ignore_ascii_case("Content-Type") {
            headers.append(k, "text/html; charset=utf-8")?;
        } else {
            headers.append(k, v)?;
        }
    }
    Ok(resp.with_headers(headers))
}

fn json_err(status: u16, code: &str) -> Result<Response> {
    send(respond::json_error(status, code, &[]), None)
}

fn json_ok(status: u16, body: Value, extra: &[(&str, &str)]) -> Result<Response> {
    let mut r = respond::json_ok(body);
    r.status = status;
    r.headers
        .extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    send(r, None)
}

fn html_page(
    status: u16,
    page: String,
    extra: &[(&str, &str)],
    profile: HeaderProfile,
) -> Result<Response> {
    send(respond::page(status, extra, profile), Some(page))
}

fn message(status: u16, title: &str, text: &str) -> Result<Response> {
    html_page(
        status,
        html::message_page(title, text),
        &[],
        HeaderProfile::Studio,
    )
}

fn redirect(location: &str, set_cookie: Option<&str>) -> Result<Response> {
    let mut extra = vec![("Location", location)];
    if let Some(c) = set_cookie {
        extra.push(("Set-Cookie", c));
    }
    let mut r = respond::json_error(303, "see_other", &extra);
    r.body = None;
    send(r, None)
}

fn denied(d: Denied) -> Result<Response> {
    json_err(d.status(), d.code())
}

// --- Row helpers ----------------------------------------------------------------------

fn s<'a>(row: &'a Value, k: &str) -> Option<&'a str> {
    row.get(k).and_then(Value::as_str)
}

fn i(row: &Value, k: &str) -> Option<i64> {
    row.get(k).and_then(Value::as_i64)
}

fn time(row: &Value, k: &str) -> Option<i64> {
    s(row, k).and_then(expiry::parse_text)
}

// --- Authentication ---------------------------------------------------------------

/// Bearer token -> principal. `None` for anything invalid (unknown id, wrong
/// digest, revoked, expired, malformed).
async fn from_token(db: &D1Database, token: &str, now: i64) -> Result<Option<Principal>> {
    let Some(id) = secret::parse(Kind::Token, token) else {
        return Ok(None);
    };
    let Some(row) = d1::first(db, &q::token_auth(id)).await? else {
        return Ok(None);
    };
    let (Some(stored), Some(user_id), Some(ws), Some(scope), Some(expires)) = (
        s(&row, "token_hash"),
        i(&row, "user_id"),
        s(&row, "workspace_id"),
        s(&row, "scope").and_then(Scope::parse),
        time(&row, "expires_at"),
    ) else {
        return Ok(None);
    };
    if !secret::verify(token, stored)
        || !expiry::token_active(
            expires,
            row.get("revoked_at").is_some_and(|v| !v.is_null()),
            now,
        )
    {
        return Ok(None);
    }
    if expiry::should_touch(time(&row, "last_used_at"), now, expiry::TOKEN_TOUCH_SECS)
        && let Err(e) = d1::run(db, &q::token_touch(id, &expiry::to_text(now))).await
    {
        log_err("token last_used_at update failed", &e);
    }
    Ok(Some(Principal {
        user_id,
        credential: Credential::Token {
            id: id.to_string(),
            workspace_id: ws.to_string(),
            scope,
        },
    }))
}

/// Session cookie value -> principal.
async fn from_session(db: &D1Database, value: &str, now: i64) -> Result<Option<Principal>> {
    let Some(id) = secret::parse(Kind::Session, value) else {
        return Ok(None);
    };
    let Some(row) = d1::first(db, &q::session_get(id)).await? else {
        return Ok(None);
    };
    let (Some(stored), Some(user_id), Some(created), Some(seen), Some(expires)) = (
        s(&row, "secret_hash"),
        i(&row, "user_id"),
        time(&row, "created_at"),
        time(&row, "last_seen_at"),
        time(&row, "expires_at"),
    ) else {
        return Ok(None);
    };
    if !secret::verify(value, stored) {
        return Ok(None);
    }
    let revoked = row.get("revoked_at").is_some_and(|v| !v.is_null());
    let SessionStatus::Valid { fresh } =
        expiry::session_status(created, seen, expires, revoked, now)
    else {
        return Ok(None);
    };
    if expiry::should_touch(Some(seen), now, expiry::SESSION_TOUCH_SECS)
        && let Err(e) = d1::run(db, &q::session_touch(id, &expiry::to_text(now))).await
    {
        log_err("session last_seen_at update failed", &e);
    }
    Ok(Some(Principal {
        user_id,
        credential: Credential::Session {
            id: id.to_string(),
            fresh,
        },
    }))
}

/// Resolve the caller. An `Authorization` header is authoritative: when present
/// it must be a valid bearer token (no fallback to the cookie). Then the session
/// cookie; then, in the `insecure-dev-auth` test build only and behind its
/// runtime fence, the dev header.
pub async fn authenticate(req: &Request, env: &Env) -> Result<Option<Principal>> {
    let db = d1::database(env)?;
    let now = now_secs();
    if let Some(h) = req.headers().get("Authorization")? {
        return match secret::bearer(&h) {
            Some(t) => from_token(&db, t, now).await,
            None => Ok(None),
        };
    }
    let cookies = req.headers().get("Cookie")?;
    if let Some(v) = cookie::get(cookies.as_deref(), SESSION_COOKIE) {
        return from_session(&db, v, now).await;
    }
    #[cfg(feature = "insecure-dev-auth")]
    {
        if crate::session::entry::dev_fence_open(req, env) {
            let dev = req.headers().get(crate::auth::DEV_USER_HEADER)?;
            return Ok(crate::auth::authenticate(dev.as_deref()).ok());
        }
    }
    Ok(None)
}

/// The caller's current role in `workspace_id`, from D1.
pub async fn role_in(db: &D1Database, workspace_id: &str, user_id: i64) -> Result<Option<Role>> {
    let row = d1::first(db, &repo::membership_get(workspace_id, user_id)).await?;
    Ok(row.as_ref().and_then(crate::session::authz::role_from_row))
}

/// CSRF: a state-changing request carried by the session cookie must come from
/// the Studio's own origin. Token and dev requests are not cookie-borne.
pub fn csrf_ok(req: &Request, env: &Env, principal: &Principal, method: &str) -> bool {
    if matches!(method, "GET" | "HEAD")
        || !matches!(principal.credential, Credential::Session { .. })
    {
        return true;
    }
    let origin = req.headers().get("Origin").ok().flatten();
    policy::csrf_ok(
        origin.as_deref(),
        var(env, "STUDIO_PUBLIC_ORIGIN").as_deref(),
    )
}

/// Authenticate, CSRF-check, read the role and authorize, in that order.
pub async fn guard(
    req: &Request,
    env: &Env,
    method: &str,
    workspace_id: &str,
    perm: Permission,
) -> Result<std::result::Result<(Principal, Role), Response>> {
    let Some(p) = authenticate(req, env).await? else {
        return Ok(Err(json_err(401, "unauthenticated")?));
    };
    if !csrf_ok(req, env, &p, method) {
        return Ok(Err(json_err(403, "forbidden_origin")?));
    }
    let db = d1::database(env)?;
    let role = role_in(&db, workspace_id, p.user_id).await?;
    match policy::authorize(&p.credential, role, workspace_id, perm) {
        Ok(()) => Ok(Ok((p, role.unwrap_or(Role::Editor)))),
        Err(d) => Ok(Err(denied(d)?)),
    }
}

/// Revalidate a socket's credential from the session Durable Object (#62
/// recheck). `None` = no longer valid (revoked, expired, wrong user/workspace).
/// A missing tag is only acceptable in the `insecure-dev-auth` test build.
pub async fn revalidate(
    db: &D1Database,
    workspace_id: &str,
    user_id: i64,
    tag: Option<&str>,
    now: i64,
) -> Result<Option<Credential>> {
    let Some(tag) = tag else {
        return Ok(cfg!(feature = "insecure-dev-auth").then_some(Credential::Dev));
    };
    match parse_tag(tag) {
        Some(CredentialTag::Session(id)) => {
            let Some(row) = d1::first(db, &q::session_check(id, user_id)).await? else {
                return Ok(None);
            };
            let (Some(c), Some(seen), Some(exp)) = (
                time(&row, "created_at"),
                time(&row, "last_seen_at"),
                time(&row, "expires_at"),
            ) else {
                return Ok(None);
            };
            let revoked = row.get("revoked_at").is_some_and(|v| !v.is_null());
            Ok(match expiry::session_status(c, seen, exp, revoked, now) {
                SessionStatus::Valid { fresh } => Some(Credential::Session {
                    id: id.to_string(),
                    fresh,
                }),
                _ => None,
            })
        }
        Some(CredentialTag::Token(id)) => {
            let Some(row) = d1::first(db, &q::token_check(workspace_id, id, user_id)).await? else {
                return Ok(None);
            };
            let (Some(scope), Some(exp)) = (
                s(&row, "scope").and_then(Scope::parse),
                time(&row, "expires_at"),
            ) else {
                return Ok(None);
            };
            let revoked = row.get("revoked_at").is_some_and(|v| !v.is_null());
            Ok(
                expiry::token_active(exp, revoked, now).then(|| Credential::Token {
                    id: id.to_string(),
                    workspace_id: workspace_id.to_string(),
                    scope,
                }),
            )
        }
        None => Ok(None),
    }
}

/// Close a user's session sockets at once (all of them, or only those opened
/// with credential `tag`) on every site of `workspaces`, bounded by
/// [`MAX_REVOKE_FANOUT`]. Best effort: a failure is logged and the Durable
/// Object's own recheck still stops delivery within the documented bound.
pub async fn close_sockets(env: &Env, user_id: i64, tag: Option<&str>, workspaces: &[String]) {
    let Ok(db) = d1::database(env) else { return };
    let mut calls = 0usize;
    for ws in workspaces {
        let sites = match d1::all(&db, &repo::site_list(ws, MAX_REVOKE_FANOUT as i64)).await {
            Ok(rows) => rows,
            Err(e) => {
                log_err("site list for revocation failed", &e);
                continue;
            }
        };
        for site in sites.iter().filter_map(|r| s(r, "id")) {
            if calls >= MAX_REVOKE_FANOUT {
                console_log!(
                    "auth: revocation fan-out capped; remaining sockets stop within the recheck bound"
                );
                return;
            }
            calls += 1;
            if let Err(e) = crate::session::entry::revoke_user(env, ws, site, user_id, tag).await {
                log_err("immediate socket revocation failed", &e);
            }
        }
    }
}

async fn user_workspaces(db: &D1Database, user_id: i64) -> Result<Vec<String>> {
    Ok(d1::all(db, &q::user_workspaces(user_id, 50))
        .await?
        .iter()
        .filter_map(|r| s(r, "workspace_id").map(str::to_string))
        .collect())
}

// --- Request plumbing ------------------------------------------------------------------

async fn read_body(req: &mut Request, cap: usize) -> Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let mut limiter = BodyLimiter::new(cap);
    if let Ok(mut stream) = req.stream() {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if limiter.push(chunk.len()).is_err() {
                return Ok(None);
            }
            out.extend_from_slice(&chunk);
        }
    }
    Ok(Some(out))
}

fn json_field<'a>(body: &'a Value, k: &str) -> Option<&'a str> {
    body.get(k).and_then(Value::as_str)
}

fn config_origin(env: &Env) -> Option<String> {
    var(env, "STUDIO_PUBLIC_ORIGIN").and_then(|o| crate::session::normalize_origin(&o))
}

pub async fn handle(mut req: Request, env: &Env, route: AuthRoute) -> Result<Response> {
    let method = req.method().to_string();
    let url = req.url()?;
    let url_len = url.path().len() + url.query().map_or(0, |q| q.len() + 1);
    let h = req.headers();
    let (cl, te, ct) = (
        h.get("Content-Length")?,
        h.get("Transfer-Encoding")?,
        h.get("Content-Type")?,
    );
    match &route {
        AuthRoute::MethodNotAllowed(allow) => {
            return send(
                respond::json_error(405, "method_not_allowed", &[("Allow", allow)]),
                None,
            );
        }
        AuthRoute::NotFound => return json_err(404, "not_found"),
        _ => {}
    }
    let kind = match check_framing(
        &route,
        &method,
        url_len,
        cl.as_deref(),
        te.as_deref(),
        ct.as_deref(),
    ) {
        Ok(k) => k,
        Err(r) => return send(respond::reject(r), None),
    };
    let raw = read_body(&mut req, MAX_AUTH_BODY).await?;
    let Some(raw) = raw else {
        return json_err(413, "payload_too_large");
    };
    if kind == BodyKind::None && !raw.is_empty() {
        return json_err(400, "bad_request");
    }
    let body: Value = match kind {
        BodyKind::Json => match serde_json::from_slice::<Value>(&raw) {
            Ok(v @ Value::Object(_)) => v,
            _ => return json_err(400, "invalid_json"),
        },
        _ => Value::Null,
    };
    let query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let qp = |k: &str| query.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());

    let res = match route {
        AuthRoute::Start => start(&req, env, None).await,
        AuthRoute::Callback => {
            callback(&req, env, qp("code"), qp("state"), qp("error").is_some()).await
        }
        AuthRoute::Me => me(&req, env).await,
        AuthRoute::Logout => logout(&req, env).await,
        AuthRoute::InvitePage => invite_page(env),
        AuthRoute::InviteAccept => invite_accept(&req, env, &raw).await,
        AuthRoute::TokensList => tokens_list(&req, env).await,
        AuthRoute::TokenCreate { ws } => token_create(&req, env, &method, &ws, &body).await,
        AuthRoute::TokenRotate { id } => token_rotate(&req, env, &method, &id).await,
        AuthRoute::TokenRevoke { id } => token_revoke(&req, env, &method, &id).await,
        AuthRoute::TokenRevokeCurrent => token_revoke_current(&req, env).await,
        AuthRoute::MembersList { ws } => members_list(&req, env, &method, &ws).await,
        AuthRoute::MemberUpdate { ws, user_id } => {
            member_update(&req, env, &method, &ws, user_id, &body).await
        }
        AuthRoute::MemberRemove { ws, user_id } => {
            member_remove(&req, env, &method, &ws, user_id).await
        }
        AuthRoute::InvitationsList { ws } => invitations_list(&req, env, &method, &ws).await,
        AuthRoute::InvitationCreate { ws } => {
            invitation_create(&req, env, &method, &ws, &body).await
        }
        AuthRoute::InvitationRevoke { ws, id } => {
            invitation_revoke(&req, env, &method, &ws, &id).await
        }
        AuthRoute::MethodNotAllowed(_) | AuthRoute::NotFound => json_err(404, "not_found"),
    };
    match res {
        Ok(r) => Ok(r),
        Err(e) => {
            log_err("unhandled error", &e);
            send(respond::internal_error(), None)
        }
    }
}

// --- OAuth ------------------------------------------------------------------------------

/// Begin the GitHub flow: a single-use, server-side state (optionally carrying
/// an accepted invitation), bound to this browser by the `__Host-sws_oauth` cookie.
async fn start(_req: &Request, env: &Env, invitation_id: Option<&str>) -> Result<Response> {
    let (Some(client_id), Some(origin), Some(base)) = (
        var(env, "GITHUB_OAUTH_CLIENT_ID"),
        config_origin(env),
        oauth::endpoint_base(
            var(env, "GITHUB_OAUTH_BASE_URL").as_deref(),
            oauth::DEFAULT_OAUTH_BASE,
        ),
    ) else {
        console_error!(
            "auth: GitHub sign-in is not configured (GITHUB_OAUTH_CLIENT_ID, STUDIO_PUBLIC_ORIGIN, GITHUB_OAUTH_BASE_URL)"
        );
        return message(
            503,
            "Sign-in unavailable",
            "GitHub sign-in is not configured on this Studio.",
        );
    };
    let Some(redirect_uri) = oauth::redirect_uri(&origin) else {
        return message(
            503,
            "Sign-in unavailable",
            "GitHub sign-in is not configured on this Studio.",
        );
    };
    let db = d1::database(env)?;
    let now = now_secs();
    let (state, state_hash) = secret::mint_state(&random_bytes::<{ secret::SECRET_BYTES }>()?);
    d1::run(&db, &q::state_prune(&expiry::to_text(now))).await?;
    let inserted = d1::run(
        &db,
        &q::state_insert_limited(
            &state_hash,
            invitation_id,
            &expiry::to_text(now),
            &expiry::to_text(now + expiry::OAUTH_STATE_TTL_SECS),
            expiry::MAX_OAUTH_STATES,
        ),
    )
    .await?;
    if inserted == 0 {
        return message(
            503,
            "Sign-in busy",
            "Too many sign-ins are in progress. Try again in a few minutes.",
        );
    }
    let c = cookie::set(OAUTH_COOKIE, &state, expiry::OAUTH_STATE_TTL_SECS);
    redirect(
        &oauth::authorize_url(&base, &client_id, &redirect_uri, &state),
        Some(&c),
    )
}

async fn github_json(req: Request) -> std::result::Result<Value, String> {
    match with_timeout(OUTBOUND_TIMEOUT, async {
        let mut r = Fetch::Request(req)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = r.status_code();
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}"));
        }
        r.json::<Value>().await.map_err(|e| e.to_string())
    })
    .await
    {
        Ok(v) => v,
        Err(()) => Err("timeout".into()),
    }
}

/// Exchange the code and read the numeric GitHub id. The access token lives
/// only inside this function and is dropped on return.
async fn github_identity(
    env: &Env,
    code: &str,
    redirect_uri: &str,
) -> std::result::Result<oauth::GithubUser, &'static str> {
    let (Some(client_id), Some(client_secret), Some(base), Some(api)) = (
        var(env, "GITHUB_OAUTH_CLIENT_ID"),
        env_secret(env, "GITHUB_OAUTH_CLIENT_SECRET"),
        oauth::endpoint_base(
            var(env, "GITHUB_OAUTH_BASE_URL").as_deref(),
            oauth::DEFAULT_OAUTH_BASE,
        ),
        oauth::endpoint_base(
            var(env, "GITHUB_API_BASE_URL").as_deref(),
            oauth::DEFAULT_API_BASE,
        ),
    ) else {
        return Err("not_configured");
    };
    let build = |url: &str,
                 method: Method,
                 body: Option<String>,
                 bearer: Option<&str>|
     -> Result<Request> {
        let headers = Headers::new();
        headers.set("Accept", "application/json")?;
        headers.set("User-Agent", "sws-studio")?;
        if body.is_some() {
            headers.set("Content-Type", "application/x-www-form-urlencoded")?;
        }
        if let Some(t) = bearer {
            headers.set("Authorization", &format!("Bearer {t}"))?;
        }
        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        if let Some(b) = body {
            init.with_body(Some(JsValue::from_str(&b)));
        }
        Request::new_with_init(url, &init)
    };
    let exchange = build(
        &oauth::token_url(&base),
        Method::Post,
        Some(oauth::token_request_body(
            &client_id,
            &client_secret,
            code,
            redirect_uri,
        )),
        None,
    )
    .map_err(|_| "exchange")?;
    let v = github_json(exchange).await.map_err(|e| {
        console_error!("auth: GitHub code exchange failed: {}", redact(&e));
        "exchange"
    })?;
    let access = oauth::parse_token_response(&v).map_err(|_| "exchange")?;
    let user_req =
        build(&oauth::user_url(&api), Method::Get, None, Some(&access)).map_err(|_| "profile")?;
    drop(access);
    let v = github_json(user_req).await.map_err(|e| {
        console_error!("auth: GitHub user lookup failed: {}", redact(&e));
        "profile"
    })?;
    oauth::parse_user(&v).map_err(|_| "profile")
}

/// An invitation row that is still acceptable now.
fn invitation_pending(row: &Value, now: i64) -> bool {
    row.get("accepted_at").is_some_and(Value::is_null)
        && row.get("revoked_at").is_some_and(Value::is_null)
        && time(row, "expires_at").is_some_and(|e| now < e)
}

async fn callback(
    req: &Request,
    env: &Env,
    code: Option<&str>,
    state: Option<&str>,
    errored: bool,
) -> Result<Response> {
    const EXPIRED: &str = "This sign-in link expired or was already used. Start again.";
    if errored {
        return message(
            400,
            "Sign-in cancelled",
            "GitHub did not complete the sign-in.",
        );
    }
    let (Some(code), Some(state)) = (code, state) else {
        return message(400, "Sign-in failed", EXPIRED);
    };
    if code.is_empty()
        || code.len() > 256
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return message(400, "Sign-in failed", EXPIRED);
    }
    // The state must be ours (format), bound to THIS browser (cookie, constant
    // time), and unused (consumed atomically below).
    let cookies = req.headers().get("Cookie")?;
    let bound = cookie::get(cookies.as_deref(), OAUTH_COOKIE)
        .is_some_and(|c| secret::ct_eq(c.as_bytes(), state.as_bytes()));
    if !secret::is_state(state) || !bound {
        return message(400, "Sign-in failed", EXPIRED);
    }
    let db = d1::database(env)?;
    let now = now_secs();
    let now_t = expiry::to_text(now);
    let Some(consumed) = d1::first(&db, &q::state_consume(&secret::hash(state), &now_t)).await?
    else {
        return message(400, "Sign-in failed", EXPIRED);
    };
    let invitation_id = s(&consumed, "invitation_id").map(str::to_string);
    let Some(redirect_uri) = config_origin(env).and_then(|o| oauth::redirect_uri(&o)) else {
        return message(
            503,
            "Sign-in unavailable",
            "GitHub sign-in is not configured on this Studio.",
        );
    };
    let gh = match github_identity(env, code, &redirect_uri).await {
        Ok(u) => u,
        Err("not_configured") => {
            return message(
                503,
                "Sign-in unavailable",
                "GitHub sign-in is not configured on this Studio.",
            );
        }
        Err(_) => {
            return message(
                502,
                "Sign-in failed",
                "GitHub could not confirm the sign-in. Start again.",
            );
        }
    };
    let login_lower = gh.login.to_ascii_lowercase();

    // Facts for the decision.
    let invitation = match &invitation_id {
        Some(id) => d1::first(&db, &q::invite_by_id(id)).await?,
        None => None,
    };
    if invitation_id.is_some() {
        let ok = invitation.as_ref().is_some_and(|r| {
            invitation_pending(r, now) && s(r, "github_login") == Some(login_lower.as_str())
        });
        if !ok {
            return message(
                403,
                "Invitation not valid",
                "This invitation is not valid for this GitHub account, or it expired or was already used.",
            );
        }
    }
    let existing = d1::first(&db, &q::user_by_github_id(gh.id)).await?;
    let existing_id = existing.as_ref().and_then(|r| i(r, "id"));
    let has_membership = match existing_id {
        Some(uid) => {
            d1::first(&db, &q::user_membership_count(uid))
                .await?
                .and_then(|r| i(&r, "n"))
                .unwrap_or(0)
                > 0
        }
        None => false,
    };
    let owner_cfg = oauth::owner_id(var(env, "STUDIO_OWNER_GITHUB_ID").as_deref());
    let decision = oauth::decide(gh.id, owner_cfg, invitation_id.is_some(), has_membership);
    match decision {
        SignIn::Refuse => {
            console_log!("auth: sign-in refused for an uninvited GitHub account");
            return message(
                403,
                "Not invited",
                "This GitHub account has not been invited to this Studio. Ask the owner for an invitation.",
            );
        }
        SignIn::Bootstrap => {
            let ws = var(env, "STUDIO_BOOTSTRAP_WORKSPACE")
                .filter(|w| crate::session::is_valid_id(w))
                .unwrap_or_else(|| DEFAULT_BOOTSTRAP_WORKSPACE.to_string());
            d1::batch(
                &db,
                &[
                    q::bootstrap_workspace(&ws, &ws),
                    q::user_upsert(gh.id, &gh.login),
                    q::bootstrap_owner(&ws, gh.id),
                ],
            )
            .await?;
        }
        SignIn::AcceptInvitation => {
            let Some(id) = invitation_id.as_deref() else {
                return message(400, "Sign-in failed", EXPIRED);
            };
            let changed = d1::batch(
                &db,
                &[
                    q::user_upsert(gh.id, &gh.login),
                    q::invite_accept(id, &now_t, gh.id, &login_lower),
                    q::invite_membership(id, &now_t, gh.id),
                ],
            )
            .await?;
            if changed.get(1).copied().unwrap_or(0) == 0 {
                return message(
                    403,
                    "Invitation not valid",
                    "This invitation was already used or has expired.",
                );
            }
            console_log!("auth: invitation {id} accepted");
        }
        SignIn::Existing => {
            d1::run(&db, &q::user_update_login(gh.id, &gh.login, &now_t)).await?;
        }
    }
    // Re-read: the user must exist and belong somewhere now (a configured owner
    // whose workspace already has another owner gets nothing).
    let Some(user_id) = d1::first(&db, &q::user_by_github_id(gh.id))
        .await?
        .and_then(|r| i(&r, "id"))
    else {
        return message(
            403,
            "Not invited",
            "This GitHub account has not been invited to this Studio.",
        );
    };
    let members = d1::first(&db, &q::user_membership_count(user_id))
        .await?
        .and_then(|r| i(&r, "n"))
        .unwrap_or(0);
    if members == 0 {
        return message(
            403,
            "Not invited",
            "This GitHub account has not been invited to this Studio.",
        );
    }
    // A brand-new session id at every sign-in (no fixation); dead sessions pruned.
    d1::run(
        &db,
        &q::session_prune_user(
            user_id,
            &now_t,
            &expiry::to_text(now - expiry::SESSION_IDLE_SECS),
        ),
    )
    .await?;
    let m = secret::mint(Kind::Session, &random_bytes::<{ secret::RANDOM_BYTES }>()?);
    d1::run(
        &db,
        &q::session_insert(
            &m.id,
            user_id,
            &m.hash,
            &now_t,
            &expiry::to_text(now + expiry::SESSION_ABSOLUTE_SECS),
        ),
    )
    .await?;
    console_log!("auth: user {user_id} signed in ({decision:?})");
    let c = cookie::set(SESSION_COOKIE, &m.value, expiry::SESSION_ABSOLUTE_SECS);
    redirect(AFTER_SIGN_IN, Some(&c))
}

async fn me(req: &Request, env: &Env) -> Result<Response> {
    let Some(p) = authenticate(req, env).await? else {
        return json_err(401, "unauthenticated");
    };
    let db = d1::database(env)?;
    let user = d1::first(&db, &q::user_get(p.user_id)).await?;
    let memberships: Vec<Value> = d1::all(&db, &q::user_workspaces(p.user_id, 50))
        .await?
        .into_iter()
        .filter(|r| match &p.credential {
            Credential::Token { workspace_id, .. } => s(r, "workspace_id") == Some(workspace_id),
            _ => true,
        })
        .collect();
    let credential = match &p.credential {
        Credential::Session { id, fresh } => json!({ "kind": "session", "id": id, "fresh": fresh }),
        Credential::Token {
            id,
            workspace_id,
            scope,
        } => {
            json!({ "kind": "token", "id": id, "workspace_id": workspace_id, "scope": scope.as_str() })
        }
        Credential::Dev => json!({ "kind": "dev" }),
    };
    json_ok(
        200,
        json!({
            "user_id": p.user_id,
            "github_login": user.as_ref().and_then(|u| s(u, "github_login")),
            "credential": credential,
            "memberships": memberships,
        }),
        &[],
    )
}

async fn logout(req: &Request, env: &Env) -> Result<Response> {
    let clear = cookie::clear(SESSION_COOKIE);
    let Some(p) = authenticate(req, env).await? else {
        return json_ok(
            200,
            json!({ "signed_out": true }),
            &[("Set-Cookie", &clear)],
        );
    };
    let Credential::Session { id, .. } = &p.credential else {
        return json_err(400, "use_token_revocation");
    };
    if !csrf_ok(req, env, &p, "POST") {
        return json_err(403, "forbidden_origin");
    }
    let db = d1::database(env)?;
    d1::run(
        &db,
        &q::session_revoke(id, p.user_id, &expiry::to_text(now_secs())),
    )
    .await?;
    let workspaces = user_workspaces(&db, p.user_id).await?;
    close_sockets(env, p.user_id, p.credential.tag().as_deref(), &workspaces).await;
    json_ok(
        200,
        json!({ "signed_out": true }),
        &[("Set-Cookie", &clear)],
    )
}

// --- Invitations (public form) -----------------------------------------------------------

fn invite_page(env: &Env) -> Result<Response> {
    let key = var(env, "TURNSTILE_SITE_KEY").unwrap_or_default();
    html_page(
        200,
        html::invite_page(&key),
        &[],
        AuthRoute::InvitePage.header_profile(),
    )
}

async fn invite_accept(req: &Request, env: &Env, raw: &[u8]) -> Result<Response> {
    const INVALID: &str = "This invitation code is not valid, has expired, or was already used.";
    let origin = req.headers().get("Origin")?;
    if !policy::csrf_ok(
        origin.as_deref(),
        var(env, "STUDIO_PUBLIC_ORIGIN").as_deref(),
    ) {
        return message(
            403,
            "Request refused",
            "This form must be submitted from the Studio itself.",
        );
    }
    let Ok(text) = std::str::from_utf8(raw) else {
        return message(400, "Invitation not accepted", INVALID);
    };
    let Ok(fields) = parse_form_urlencoded(text) else {
        return message(400, "Invitation not accepted", INVALID);
    };
    let token = fields
        .get(TURNSTILE_FIELD)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    if let Some(rejected) = verify_turnstile(env, req, token, html::INVITE_ACTION).await? {
        let status = rejected.status_code();
        return message(
            status,
            "Human check failed",
            "The human check did not pass. Go back and try again.",
        );
    }
    let code = fields.get(html::CODE_FIELD).map(|s| s.trim()).unwrap_or("");
    let Some(id) = secret::parse(Kind::Invite, code) else {
        return message(400, "Invitation not accepted", INVALID);
    };
    let db = d1::database(env)?;
    let now = now_secs();
    let row = d1::first(&db, &q::invite_by_id(id)).await?;
    let valid = row.as_ref().is_some_and(|r| {
        s(r, "secret_hash").is_some_and(|h| secret::verify(code, h)) && invitation_pending(r, now)
    });
    if !valid {
        return message(400, "Invitation not accepted", INVALID);
    }
    start(req, env, Some(id)).await
}

// --- Tokens -------------------------------------------------------------------------------

fn token_json(row: &Value) -> Value {
    json!({
        "id": s(row, "id"),
        "workspace_id": s(row, "workspace_id"),
        "scope": s(row, "scope"),
        "label": s(row, "label"),
        "created_at": s(row, "created_at"),
        "expires_at": s(row, "expires_at"),
        "chain_started_at": s(row, "chain_started_at"),
        "last_used_at": s(row, "last_used_at"),
        "revoked_at": s(row, "revoked_at"),
    })
}

fn valid_label(l: &str) -> bool {
    l.chars().count() <= 120 && !l.chars().any(char::is_control)
}

/// A session principal, else the right refusal.
async fn session_principal(
    req: &Request,
    env: &Env,
    method: &str,
    need_fresh: bool,
) -> Result<std::result::Result<Principal, Response>> {
    let Some(p) = authenticate(req, env).await? else {
        return Ok(Err(json_err(401, "unauthenticated")?));
    };
    match &p.credential {
        Credential::Session { fresh, .. } => {
            if !csrf_ok(req, env, &p, method) {
                return Ok(Err(json_err(403, "forbidden_origin")?));
            }
            if need_fresh && !fresh {
                return Ok(Err(denied(Denied::ReauthRequired)?));
            }
            Ok(Ok(p))
        }
        Credential::Dev => Ok(Ok(p)),
        Credential::Token { .. } => Ok(Err(denied(Denied::SessionRequired)?)),
    }
}

async fn tokens_list(req: &Request, env: &Env) -> Result<Response> {
    let p = match session_principal(req, env, "GET", false).await? {
        Ok(p) => p,
        Err(r) => return Ok(r),
    };
    let db = d1::database(env)?;
    let rows = d1::all(&db, &q::token_list_own(p.user_id, 100)).await?;
    json_ok(
        200,
        json!({ "tokens": rows.iter().map(token_json).collect::<Vec<_>>() }),
        &[],
    )
}

async fn issue(
    db: &D1Database,
    ws: &str,
    user_id: i64,
    scope: Scope,
    label: &str,
    now: i64,
    rotation: Option<(&str, i64)>,
) -> Result<std::result::Result<(secret::Minted, expiry::TokenTimes), Response>> {
    // A rotation replaces (old id) and keeps the chain start of the old token.
    let replaces = rotation.map(|(old, _)| old);
    let times = match expiry::plan_token(scope, now, rotation.map(|(_, chain)| chain)) {
        Ok(t) => t,
        Err(e) => return Ok(Err(json_err(409, e.code())?)),
    };
    let m = secret::mint(Kind::Token, &random_bytes::<{ secret::RANDOM_BYTES }>()?);
    let (c, e, ch) = (
        expiry::to_text(times.created_at),
        expiry::to_text(times.expires_at),
        expiry::to_text(times.chain_started_at),
    );
    let new = q::NewToken {
        workspace_id: ws,
        id: &m.id,
        user_id,
        hash: &m.hash,
        scope: scope.as_str(),
        label,
        created_at: &c,
        expires_at: &e,
        chain_started_at: &ch,
        replaces,
    };
    let inserted = match replaces {
        None => {
            d1::run(
                db,
                &q::token_insert_limited(&new, expiry::MAX_ACTIVE_TOKENS),
            )
            .await?
        }
        Some(old) => {
            let r = d1::batch(
                db,
                &[
                    q::token_insert_limited(&new, expiry::MAX_ACTIVE_TOKENS),
                    q::token_revoke_rotated(old, user_id, &c, &m.id),
                ],
            )
            .await?;
            r.first().copied().unwrap_or(0)
        }
    };
    if inserted == 0 {
        return Ok(Err(json_err(
            409,
            expiry::TokenError::TooManyTokens.code(),
        )?));
    }
    Ok(Ok((m, times)))
}

fn minted_json(
    m: &secret::Minted,
    ws: &str,
    scope: Scope,
    label: &str,
    t: &expiry::TokenTimes,
) -> Value {
    json!({
        "token": m.value,
        "id": m.id,
        "workspace_id": ws,
        "scope": scope.as_str(),
        "label": label,
        "created_at": expiry::to_text(t.created_at),
        "expires_at": expiry::to_text(t.expires_at),
        "chain_started_at": expiry::to_text(t.chain_started_at),
        "chain_expires_at": expiry::to_text(t.chain_started_at + expiry::TOKEN_CHAIN_MAX_SECS),
        "note": "Shown once. Store it in a secret manager; Studio keeps only a SHA-256 digest.",
    })
}

async fn token_create(
    req: &Request,
    env: &Env,
    method: &str,
    ws: &str,
    body: &Value,
) -> Result<Response> {
    let (p, role) = match guard(req, env, method, ws, Permission::ManageTokens).await? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let Some(scope) = json_field(body, "scope").and_then(Scope::parse) else {
        return json_err(400, "invalid_scope");
    };
    let label = json_field(body, "label").unwrap_or("");
    if !valid_label(label) {
        return json_err(400, "invalid_label");
    }
    if !policy::role_may_hold(role, scope) {
        return denied(Denied::Role);
    }
    let db = d1::database(env)?;
    match issue(&db, ws, p.user_id, scope, label, now_secs(), None).await? {
        Ok((m, t)) => {
            console_log!(
                "auth: user {} created {} token {}",
                p.user_id,
                scope.as_str(),
                m.id
            );
            json_ok(201, minted_json(&m, ws, scope, label, &t), &[])
        }
        Err(r) => Ok(r),
    }
}

async fn token_rotate(req: &Request, env: &Env, method: &str, id: &str) -> Result<Response> {
    let p = match session_principal(req, env, method, true).await? {
        Ok(p) => p,
        Err(r) => return Ok(r),
    };
    let db = d1::database(env)?;
    let Some(row) = d1::first(&db, &q::token_get_own(id, p.user_id)).await? else {
        return json_err(404, "not_found");
    };
    if row.get("revoked_at").is_some_and(|v| !v.is_null()) {
        return json_err(409, "token_revoked");
    }
    let (Some(ws), Some(scope), Some(chain)) = (
        s(&row, "workspace_id"),
        s(&row, "scope").and_then(Scope::parse),
        time(&row, "chain_started_at"),
    ) else {
        return send(respond::internal_error(), None);
    };
    let role = role_in(&db, ws, p.user_id).await?;
    if let Err(d) = policy::authorize(&p.credential, role, ws, Permission::ManageTokens) {
        return denied(d);
    }
    if !role.is_some_and(|r| policy::role_may_hold(r, scope)) {
        return denied(Denied::Role);
    }
    let label = s(&row, "label").unwrap_or("").to_string();
    match issue(
        &db,
        ws,
        p.user_id,
        scope,
        &label,
        now_secs(),
        Some((id, chain)),
    )
    .await?
    {
        Ok((m, t)) => {
            close_sockets(env, p.user_id, Some(&format!("t:{id}")), &[ws.to_string()]).await;
            console_log!("auth: user {} rotated token {id} -> {}", p.user_id, m.id);
            json_ok(201, minted_json(&m, ws, scope, &label, &t), &[])
        }
        Err(r) => Ok(r),
    }
}

async fn revoke_token(env: &Env, db: &D1Database, user_id: i64, id: &str) -> Result<Response> {
    let Some(row) = d1::first(db, &q::token_get_own(id, user_id)).await? else {
        return json_err(404, "not_found");
    };
    let changed = d1::run(
        db,
        &q::token_revoke_own(id, user_id, &expiry::to_text(now_secs())),
    )
    .await?;
    if let Some(ws) = s(&row, "workspace_id") {
        close_sockets(env, user_id, Some(&format!("t:{id}")), &[ws.to_string()]).await;
    }
    console_log!("auth: user {user_id} revoked token {id}");
    json_ok(
        200,
        json!({ "id": id, "revoked": true, "already_revoked": changed == 0 }),
        &[],
    )
}

async fn token_revoke(req: &Request, env: &Env, method: &str, id: &str) -> Result<Response> {
    let p = match session_principal(req, env, method, true).await? {
        Ok(p) => p,
        Err(r) => return Ok(r),
    };
    let db = d1::database(env)?;
    revoke_token(env, &db, p.user_id, id).await
}

/// A CLI/MCP client revoking its own token (logout); needs nothing but the token.
async fn token_revoke_current(req: &Request, env: &Env) -> Result<Response> {
    let Some(p) = authenticate(req, env).await? else {
        return json_err(401, "unauthenticated");
    };
    let Credential::Token { id, .. } = &p.credential else {
        return json_err(400, "token_required");
    };
    let db = d1::database(env)?;
    revoke_token(env, &db, p.user_id, id).await
}

// --- Members and invitations ---------------------------------------------------------------

async fn members_list(req: &Request, env: &Env, method: &str, ws: &str) -> Result<Response> {
    if let Err(r) = guard(req, env, method, ws, Permission::Read).await? {
        return Ok(r);
    }
    let db = d1::database(env)?;
    let rows = d1::all(&db, &q::members_list(ws, 200)).await?;
    json_ok(200, json!({ "workspace_id": ws, "members": rows }), &[])
}

async fn target_role(db: &D1Database, ws: &str, user_id: i64) -> Result<Option<Role>> {
    role_in(db, ws, user_id).await
}

async fn member_update(
    req: &Request,
    env: &Env,
    method: &str,
    ws: &str,
    target: i64,
    body: &Value,
) -> Result<Response> {
    let (p, _) = match guard(req, env, method, ws, Permission::ManageMembers).await? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let Some(role) = json_field(body, "role")
        .and_then(Role::parse)
        .filter(|r| *r != Role::Owner)
    else {
        return json_err(400, "invalid_role");
    };
    let db = d1::database(env)?;
    match target_role(&db, ws, target).await? {
        None => return json_err(404, "not_found"),
        Some(Role::Owner) => return json_err(409, "owner_is_fixed"),
        Some(_) => {}
    }
    let now_t = expiry::to_text(now_secs());
    let mut stmts = vec![q::member_set_role(ws, target, role.as_str())];
    if role == Role::Editor {
        stmts.push(q::token_revoke_above_edit(ws, target, &now_t));
    }
    d1::batch(&db, &stmts).await?;
    close_sockets(env, target, None, &[ws.to_string()]).await;
    console_log!(
        "auth: user {} set user {target} to {} in {ws}",
        p.user_id,
        role.as_str()
    );
    json_ok(
        200,
        json!({ "workspace_id": ws, "user_id": target, "role": role.as_str() }),
        &[],
    )
}

async fn member_remove(
    req: &Request,
    env: &Env,
    method: &str,
    ws: &str,
    target: i64,
) -> Result<Response> {
    let (p, _) = match guard(req, env, method, ws, Permission::ManageMembers).await? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let db = d1::database(env)?;
    match target_role(&db, ws, target).await? {
        None => return json_err(404, "not_found"),
        Some(Role::Owner) => return json_err(409, "owner_is_fixed"),
        Some(_) => {}
    }
    d1::run(&db, &q::member_remove(ws, target)).await?;
    close_sockets(env, target, None, &[ws.to_string()]).await;
    console_log!("auth: user {} removed user {target} from {ws}", p.user_id);
    json_ok(
        200,
        json!({ "workspace_id": ws, "user_id": target, "removed": true }),
        &[],
    )
}

async fn invitations_list(req: &Request, env: &Env, method: &str, ws: &str) -> Result<Response> {
    let (p, role) = match guard(req, env, method, ws, Permission::Read).await? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    if matches!(p.credential, Credential::Token { .. }) {
        return denied(Denied::SessionRequired);
    }
    if !policy::role_allows(role, Permission::ManageMembers) {
        return denied(Denied::Role);
    }
    let db = d1::database(env)?;
    let rows = d1::all(&db, &q::invite_list(ws, 100)).await?;
    json_ok(200, json!({ "workspace_id": ws, "invitations": rows }), &[])
}

async fn invitation_create(
    req: &Request,
    env: &Env,
    method: &str,
    ws: &str,
    body: &Value,
) -> Result<Response> {
    let (p, _) = match guard(req, env, method, ws, Permission::ManageMembers).await? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let Some(login) = json_field(body, "github_login")
        .map(str::trim)
        .filter(|l| oauth::is_github_login(l))
    else {
        return json_err(400, "invalid_github_login");
    };
    let Some(role) = json_field(body, "role")
        .and_then(Role::parse)
        .filter(|r| *r != Role::Owner)
    else {
        return json_err(400, "invalid_role");
    };
    let login = login.to_ascii_lowercase();
    let db = d1::database(env)?;
    let now = now_secs();
    let m = secret::mint(Kind::Invite, &random_bytes::<{ secret::RANDOM_BYTES }>()?);
    let (c, e) = (
        expiry::to_text(now),
        expiry::to_text(now + expiry::INVITATION_TTL_SECS),
    );
    let n = q::NewInvitation {
        workspace_id: ws,
        id: &m.id,
        hash: &m.hash,
        github_login: &login,
        role: role.as_str(),
        invited_by: p.user_id,
        created_at: &c,
        expires_at: &e,
    };
    if d1::run(
        &db,
        &q::invite_insert_limited(&n, expiry::MAX_PENDING_INVITATIONS),
    )
    .await?
        == 0
    {
        return json_err(409, "too_many_invitations");
    }
    console_log!(
        "auth: user {} invited a {} to {ws} (invitation {})",
        p.user_id,
        role.as_str(),
        m.id
    );
    let accept_url = config_origin(env).map(|o| format!("{o}/invite"));
    json_ok(
        201,
        json!({
            "code": m.value,
            "id": m.id,
            "workspace_id": ws,
            "github_login": login,
            "role": role.as_str(),
            "expires_at": e,
            "accept_url": accept_url,
            "note": "Shown once. Send the code to the invitee over a private channel; it works once.",
        }),
        &[],
    )
}

async fn invitation_revoke(
    req: &Request,
    env: &Env,
    method: &str,
    ws: &str,
    id: &str,
) -> Result<Response> {
    if let Err(r) = guard(req, env, method, ws, Permission::ManageMembers).await? {
        return Ok(r);
    }
    let db = d1::database(env)?;
    if d1::run(&db, &q::invite_revoke(ws, id, &expiry::to_text(now_secs()))).await? == 0 {
        return json_err(404, "not_found");
    }
    json_ok(200, json!({ "id": id, "revoked": true }), &[])
}
