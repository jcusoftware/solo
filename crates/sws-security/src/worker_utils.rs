//! Cloudflare Worker runtime utilities for `sws-security`.

use std::future::Future;
use std::task::Poll;
use std::time::Duration;

use serde::Deserialize;
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::{
    AbortSignal, Delay, Env, Fetch, Headers, Method, Request, RequestInit, Response, Result,
    console_error, console_log, console_warn, js_sys, web_sys,
};

use crate::headers::API_CACHE_CONTROL;
use crate::response::error_body;
use crate::validate::{self, BodyError, Fields, MAX_BODY_BYTES, form_encode};

const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);

pub fn json_response(status: u16, body: &serde_json::Value) -> Result<Response> {
    let resp = Response::from_json(body)?.with_status(status);
    resp.headers().set("Cache-Control", API_CACHE_CONTROL)?;
    Ok(resp)
}

pub fn non_empty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn secret(env: &Env, name: &str) -> Option<String> {
    non_empty(env.secret(name).ok().map(|s| s.to_string()))
}

pub fn var(env: &Env, name: &str) -> Option<String> {
    non_empty(env.var(name).ok().map(|s| s.to_string()))
}

pub fn opt_str(v: &Option<String>) -> JsValue {
    v.as_deref().map(JsValue::from_str).unwrap_or(JsValue::NULL)
}

#[allow(clippy::result_unit_err)]
pub async fn with_timeout<F, T>(dur: Duration, fut: F) -> std::result::Result<T, ()>
where
    F: Future<Output = T>,
{
    let mut fut = std::pin::pin!(fut);
    let mut delay = std::pin::pin!(Delay::from(dur));
    std::future::poll_fn(|cx| {
        if let Poll::Ready(v) = fut.as_mut().poll(cx) {
            return Poll::Ready(Ok(v));
        }
        if delay.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(()));
        }
        Poll::Pending
    })
    .await
}

/// Pre-check `Content-Type` and stream the request body with a hard 16 KB cap.
/// `Err(resp)` is a ready HTTP error response.
pub async fn read_fields(req: &mut Request) -> Result<std::result::Result<Fields, Response>> {
    let content_type = req.headers().get("Content-Type")?;
    if validate::parse_content_type(content_type.as_deref()).is_none() {
        return Ok(Err(json_response(
            415,
            &error_body(BodyError::UnsupportedMediaType.code()),
        )?));
    }
    if let Some(len) = req
        .headers()
        .get("Content-Length")?
        .and_then(|v| v.trim().parse::<usize>().ok())
        && len > MAX_BODY_BYTES
    {
        return Ok(Err(json_response(
            413,
            &error_body(BodyError::TooLarge.code()),
        )?));
    }
    let body = match req.inner().body() {
        None => Vec::new(),
        Some(stream) => {
            let Ok(reader) = stream
                .get_reader()
                .dyn_into::<web_sys::ReadableStreamDefaultReader>()
            else {
                return Ok(Err(json_response(
                    400,
                    &error_body(BodyError::Malformed.code()),
                )?));
            };
            let mut buf = Vec::new();
            loop {
                let chunk_js = match JsFuture::from(reader.read()).await {
                    Ok(v) => v,
                    Err(_) => {
                        reader.release_lock();
                        return Ok(Err(json_response(
                            400,
                            &error_body(BodyError::Malformed.code()),
                        )?));
                    }
                };
                let done = js_sys::Reflect::get(&chunk_js, &JsValue::from_str("done"))
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if done {
                    break;
                }
                let Ok(val) = js_sys::Reflect::get(&chunk_js, &JsValue::from_str("value")) else {
                    reader.release_lock();
                    return Ok(Err(json_response(
                        400,
                        &error_body(BodyError::Malformed.code()),
                    )?));
                };
                if val.is_undefined() || val.is_null() {
                    continue;
                }
                let Ok(arr) = val.dyn_into::<js_sys::Uint8Array>() else {
                    reader.release_lock();
                    return Ok(Err(json_response(
                        400,
                        &error_body(BodyError::Malformed.code()),
                    )?));
                };
                let chunk_len = arr.length() as usize;
                if buf.len().saturating_add(chunk_len) > MAX_BODY_BYTES {
                    let _ = reader.cancel();
                    reader.release_lock();
                    return Ok(Err(json_response(
                        413,
                        &error_body(BodyError::TooLarge.code()),
                    )?));
                }
                let old_len = buf.len();
                buf.resize(old_len + chunk_len, 0);
                arr.copy_to(&mut buf[old_len..]);
            }
            reader.release_lock();
            buf
        }
    };
    match validate::parse_body(content_type.as_deref(), &body) {
        Ok(f) => Ok(Ok(f)),
        Err(e) => Ok(Err(json_response(e.status(), &error_body(e.code()))?)),
    }
}

#[derive(Deserialize)]
struct SiteVerify {
    success: bool,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
    #[serde(default)]
    action: String,
    #[serde(default)]
    hostname: String,
}

/// `Ok(None)` = verified (or dev-skipped); `Ok(Some(resp))` = reject with resp.
pub async fn verify_turnstile(
    env: &Env,
    req: &Request,
    token: Option<&str>,
    expected_action: &str,
) -> Result<Option<Response>> {
    let dev_mode = var(env, "DEV_MODE").as_deref() == Some("true");
    let Some(secret_key) = secret(env, "TURNSTILE_SECRET_KEY") else {
        if dev_mode {
            console_warn!("TURNSTILE_SECRET_KEY missing; DEV_MODE=true so verification is SKIPPED");
            return Ok(None);
        }
        console_error!("TURNSTILE_SECRET_KEY missing; refusing submission");
        return Ok(Some(json_response(
            503,
            &error_body("turnstile_unconfigured"),
        )?));
    };
    // Fail closed in production: the always-pass TEST secret must never
    // silently disable bot protection when DEV_MODE is unset (issue #6).
    let is_test_secret = validate::is_turnstile_test_secret(&secret_key);
    if is_test_secret && !dev_mode {
        console_error!(
            "TURNSTILE_SECRET_KEY is a Cloudflare TEST key but DEV_MODE is unset; \
             refusing submission (fail closed)"
        );
        return Ok(Some(json_response(
            503,
            &error_body("turnstile_unconfigured"),
        )?));
    }
    let relax_dev_checks = dev_mode && is_test_secret;
    let Some(token) = token else {
        return Ok(Some(json_response(
            400,
            &error_body("turnstile_required"),
        )?));
    };
    let ip = req.headers().get("CF-Connecting-IP")?.unwrap_or_default();
    let mut pairs = vec![("secret", secret_key.as_str()), ("response", token)];
    if !ip.is_empty() {
        pairs.push(("remoteip", ip.as_str()));
    }
    let headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&form_encode(&pairs))));
    let outbound = Request::new_with_init(SITEVERIFY_URL, &init)?;
    let controller = web_sys::AbortController::new()?;
    let signal = AbortSignal::from(controller.signal());
    let verdict = match with_timeout(OUTBOUND_TIMEOUT, async {
        match Fetch::Request(outbound).send_with_signal(&signal).await {
            Ok(mut r) => {
                let status = r.status_code();
                if !(200..300).contains(&status) {
                    console_error!("turnstile siteverify HTTP {status}");
                    return None;
                }
                match r.json::<SiteVerify>().await {
                    Ok(v) => Some(v),
                    Err(e) => {
                        console_error!("turnstile siteverify JSON parse failed: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                console_error!("turnstile siteverify fetch failed: {e}");
                None
            }
        }
    })
    .await
    {
        Ok(v) => v,
        Err(()) => {
            controller.abort();
            console_error!("turnstile siteverify fetch timed out");
            None
        }
    };
    let outcome = verdict.map(|v| crate::turnstile::SiteVerifyOutcome {
        success: v.success,
        action: v.action,
        hostname: v.hostname,
        error_codes: v.error_codes,
    });
    let allowed_hosts = var(env, "ALLOWED_HOSTNAMES");
    match crate::turnstile::turnstile_verdict(
        outcome.as_ref(),
        expected_action,
        allowed_hosts.as_deref(),
        relax_dev_checks,
    ) {
        crate::turnstile::TurnstileVerdict::Allow => Ok(None),
        crate::turnstile::TurnstileVerdict::Reject(reason) => {
            match reason {
                crate::turnstile::RejectReason::Unavailable => {
                    // Already logged at the fetch/parse site above.
                }
                crate::turnstile::RejectReason::Failed => {
                    console_log!(
                        "turnstile rejected: {:?}",
                        outcome.map(|o| o.error_codes).unwrap_or_default()
                    );
                }
                crate::turnstile::RejectReason::ActionMismatch => {
                    console_warn!(
                        "turnstile action mismatch: expected {expected_action}, got {:?}",
                        outcome.map(|o| o.action).unwrap_or_default()
                    );
                }
                crate::turnstile::RejectReason::HostnameRejected => {
                    console_warn!(
                        "turnstile hostname rejected: {:?}",
                        outcome.map(|o| o.hostname).unwrap_or_default()
                    );
                }
            }
            Ok(Some(json_response(reason.status(), &error_body(reason.code()))?))
        }
    }
}
