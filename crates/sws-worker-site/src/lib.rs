//! sws-worker-site: domain-agnostic published-site Worker (workers-rs).
//!
//! Endpoints (all responses pass through [`sws_security::headers::with_security_headers`]):
//! - `GET|HEAD /api/health` -> `{status, service, version}`
//! - `POST /api/forms/<id>` -> schema-driven validation + D1 `form_submissions` + optional email/webhook
//! - `POST /api/inquire` -> backward-compatible alias using the `"inquire"` form schema
//! - `POST /api/newsletter` -> D1 `newsletter_subscribers` (explicit CAN-SPAM consent)
//! - `GET /api/events` -> D1 public events calendar listing (config-gated marker asset)
//! - `GET /api/events/:id/ics` -> single-event RFC 5545 calendar download
//! - `GET /api/bookings/slots` and `POST /api/bookings` -> call booking
//!   (`scheduling.bookings`; config-gated marker asset, D1 `bookings`,
//!   Google Calendar free/busy + invitations, optional Zoom; issue #44)
//! - `POST /api/checkout` -> sandbox-only hosted Stripe Checkout handoff (no storage)
//! - Any other path -> static assets binding (`ASSETS`)
//!
//! Events modules (issue #43): `events` (reader + ICS), `google_sync` /
//! `google_auth` / `timestamp` (config-gated Google Calendar import). The
//! scheduled handler runs the snapshot sync only when the deployment ships an
//! enabled `events-config.json` with `google_calendar_sync`; cron triggers stay
//! commented out in `wrangler.jsonc` by default.
//! Booking modules (issue #44): `bookings` (slot math + Worker handlers) and
//! `zoom` (config-gated meeting creation), both gated on the enabled
//! `booking-config.json` marker and inert without the provisioning secrets.

pub mod bookings;
pub mod checkout;
pub mod events;
pub mod forms;
pub mod google_auth;
pub mod newsletter;
pub mod routes;
pub mod timestamp;
pub mod zoom;

#[cfg(target_arch = "wasm32")]
mod google_sync;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::time::Duration;

    use futures_util::StreamExt;
    use serde::Deserialize;
    use serde_json::Value;
    use worker::wasm_bindgen::JsValue;
    use worker::{
        AbortSignal, ByteStream, Context, Env, Fetch, Headers, Method, Request, RequestInit,
        RequestRedirect, Response, Result, ScheduleContext, ScheduledEvent, console_error,
        console_log, console_warn, event, web_sys,
    };

    use sws_security::headers::{API_CACHE_CONTROL, with_security_headers};
    use sws_security::response::{error_body, validation_error_body};
    use sws_security::validate::{
        ValidationError, form_encode, iso8601_utc, is_turnstile_test_secret, truncate_str,
        validate_newsletter,
    };
    use sws_security::worker_utils::{
        json_response, read_fields, secret, var, verify_turnstile, with_timeout,
    };

    use crate::checkout;
    use crate::forms::{
        FormSchema, FormSchemaDiagnosticLevel, ValidatedSubmission, load_form_schemas,
        validate_submission,
    };
    use crate::routes::{Route, health_body, is_safe_webhook_url, route};

    const RESEND_URL: &str = "https://api.resend.com/emails";
    const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);
    const MAX_ERROR_LOG_BYTES: usize = 256;
    // Same Turnstile endpoint the shared verifier in sws-security uses; the
    // checkout demo needs a checkout-specific acceptance policy (see below).
    const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
    // Pinned Stripe API version for the sandbox demo; never a live call target.
    const STRIPE_API_VERSION: &str = "2025-06-30.basil";
    const MAX_UPSTREAM_BODY: usize = 64 * 1024;

    #[event(start)]
    fn start() {
        #[cfg(feature = "console_error_panic_hook")]
        console_error_panic_hook::set_once();
    }

    // Optional Google Calendar import (issue #43): only fires when the
    // deployment ships an enabled `events-config.json` marker with
    // `source: "google_calendar_sync"`. Enable `triggers.crons` in
    // wrangler.jsonc (e.g. `*/30 * * * *`) only after provisioning the
    // GOOGLE_* secrets. Failed runs retain the stored snapshot.
    #[event(scheduled)]
    async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
        if crate::google_sync::sync(&env, event.schedule()).await.is_err() {
            console_error!("Google Calendar sync failed; existing snapshot retained");
            // workers-rs 0.9 discards the scheduled handler's return value.
            worker::wasm_bindgen::throw_str("Google Calendar sync failed");
        }
    }

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
        let resp = match handle(req, &env, &ctx).await {
            Ok(r) => r,
            Err(e) => {
                console_error!("unhandled error: {e}");
                json_response(500, &error_body("internal_error"))?
            }
        };
        with_security_headers(resp)
    }

    async fn handle(mut req: Request, env: &Env, ctx: &Context) -> Result<Response> {
        let method = req.method();
        let path = req.path();
        match route(method.as_ref(), &path) {
            Route::Health => {
                let version = var(env, "BUILD_VERSION")
                    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
                json_response(200, &health_body(&version))
            }
            Route::HealthHead => {
                let resp = Response::empty()?.with_status(200);
                resp.headers().set("Content-Type", "application/json")?;
                resp.headers().set("Cache-Control", API_CACHE_CONTROL)?;
                Ok(resp)
            }
            Route::FormSubmit(form_id) => submit_form(&mut req, env, ctx, &form_id, false).await,
            Route::InquireCompat => submit_form(&mut req, env, ctx, "inquire", true).await,
            Route::Newsletter => newsletter(&mut req, env, ctx).await,
            Route::Events => crate::events::wasm::handle(&req, env, None).await,
            Route::EventIcs(id) => crate::events::wasm::handle(&req, env, Some(&id)).await,
            Route::BookingSlots => crate::bookings::wasm::handle(&mut req, env, true).await,
            Route::Bookings => crate::bookings::wasm::handle(&mut req, env, false).await,
            Route::Checkout => demo_checkout(&mut req, env).await,
            Route::MethodNotAllowed(allow) => {
                let resp = json_response(405, &error_body("method_not_allowed"))?;
                resp.headers().set("Allow", allow)?;
                Ok(resp)
            }
            Route::NotFound => json_response(404, &error_body("not_found")),
            Route::Asset => env.assets("ASSETS")?.fetch_request(req).await,
        }
    }

    fn resolve_schema(env: &Env, form_id: &str) -> Option<FormSchema> {
        let (mut schemas, diagnostics) = load_form_schemas(var(env, "FORM_SCHEMAS_JSON").as_deref());
        for diag in &diagnostics {
            match diag.level {
                FormSchemaDiagnosticLevel::Error => console_error!("{}", diag.message),
                FormSchemaDiagnosticLevel::Warning => console_warn!("{}", diag.message),
            }
        }
        schemas.remove(form_id)
    }

    async fn submit_form(
        req: &mut Request,
        env: &Env,
        ctx: &Context,
        form_id: &str,
        compat_inquire: bool,
    ) -> Result<Response> {
        let Some(schema) = resolve_schema(env, form_id) else {
            return json_response(404, &error_body("unknown_form"));
        };

        let fields = match read_fields(req).await? {
            Ok(f) => f,
            Err(resp) => return Ok(resp),
        };

        let submission = match validate_submission(&schema, &fields) {
            Ok(s) => s,
            Err(ValidationError::Honeypot) => {
                return json_response(400, &error_body("submission_rejected"));
            }
            Err(ValidationError::Fields(errs)) => {
                return json_response(400, &validation_error_body(&errs));
            }
        };

        let turnstile_action = if compat_inquire {
            "inquire"
        } else {
            schema.turnstile_action.as_str()
        };
        if let Some(resp) =
            verify_turnstile(env, req, submission.turnstile_token.as_deref(), turnstile_action)
                .await?
        {
            return Ok(resp);
        }

        let payload_json = match serde_json::to_string(&submission.values) {
            Ok(s) => s,
            Err(e) => {
                console_error!("submission serialize failed for {form_id}: {e}");
                return json_response(500, &error_body("storage_failed"));
            }
        };

        let stmt = env
            .d1("DB")?
            .prepare(
                "INSERT INTO form_submissions (form_id, payload_json, source_route) \
                 VALUES (?1, ?2, ?3) RETURNING id",
            )
            .bind(&[
                JsValue::from_str(form_id),
                JsValue::from_str(&payload_json),
                JsValue::from_str(&submission.source_route),
            ])?;

        let id = match stmt.first::<f64>(Some("id")).await {
            Ok(Some(id)) => id as i64,
            Ok(None) => {
                console_error!("form_submissions insert returned no id for {form_id}");
                return json_response(500, &error_body("storage_failed"));
            }
            Err(e) => {
                console_error!("form_submissions insert failed for {form_id}: {e}");
                return json_response(500, &error_body("storage_failed"));
            }
        };

        schedule_notifications(env, ctx, form_id, &submission, id);

        if compat_inquire {
            json_response(201, &serde_json::json!({ "ok": true, "id": id }))
        } else {
            json_response(
                201,
                &serde_json::json!({ "ok": true, "id": id, "form_id": form_id }),
            )
        }
    }

    fn schedule_notifications(
        env: &Env,
        ctx: &Context,
        form_id: &str,
        submission: &ValidatedSubmission,
        id: i64,
    ) {
        if let Some(api_key) = secret(env, "RESEND_API_KEY") {
            let to = var(env, "FORM_TO").or_else(|| var(env, "INQUIRY_TO"));
            if let (Some(from), Some(to)) = (var(env, "RESEND_FROM"), to) {
                let payload = build_resend_payload(form_id, &from, &to, submission, id);
                let label = format!("resend submission #{id} ({form_id})");
                ctx.wait_until(async move {
                    post_json_best_effort(&label, RESEND_URL, Some(&api_key), &payload).await;
                });
            } else {
                console_warn!(
                    "resend stub: skipped (RESEND_FROM/FORM_TO not set) for submission #{id}"
                );
            }
        } else {
            console_log!("resend stub: skipped (RESEND_API_KEY not set) for submission #{id}");
        }

        if let Some(webhook_url) = var(env, "FORM_WEBHOOK_URL") {
            if is_safe_webhook_url(&webhook_url) {
                let webhook_secret = secret(env, "FORM_WEBHOOK_SECRET");
                let payload = serde_json::json!({
                    "id": id,
                    "form_id": form_id,
                    "source_route": submission.source_route,
                    "values": submission.values,
                });
                let label = format!("webhook submission #{id} ({form_id})");
                ctx.wait_until(async move {
                    post_json_best_effort(
                        &label,
                        &webhook_url,
                        webhook_secret.as_deref(),
                        &payload,
                    )
                    .await;
                });
            } else {
                console_warn!("webhook skipped: FORM_WEBHOOK_URL must use https://");
            }
        }
    }

    fn build_resend_payload(
        form_id: &str,
        from: &str,
        to: &str,
        submission: &ValidatedSubmission,
        id: i64,
    ) -> Value {
        let mut lines = vec![
            format!("Form: {form_id}"),
            format!("Route: {}", submission.source_route),
        ];
        for (k, v) in &submission.values {
            if k == "source_route" {
                continue;
            }
            let rendered = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            lines.push(format!("{k}: {rendered}"));
        }
        let mut payload = serde_json::json!({
            "from": from,
            "to": [to],
            "subject": format!("New {form_id} submission #{id}"),
            "text": lines.join("\n"),
        });
        if let Some(reply_to) = submission.values.get("email").and_then(Value::as_str)
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert("reply_to".to_string(), Value::String(reply_to.to_string()));
        }
        payload
    }

    /// Best-effort JSON POST executed inside `ctx.wait_until`; never fails the request.
    async fn post_json_best_effort(
        label: &str,
        url: &str,
        auth_bearer: Option<&str>,
        payload: &Value,
    ) {
        let controller = match web_sys::AbortController::new() {
            Ok(c) => c,
            Err(e) => {
                console_error!("{label}: AbortController failed: {e:?} (ignored)");
                return;
            }
        };
        let signal = AbortSignal::from(controller.signal());
        let outcome = with_timeout(OUTBOUND_TIMEOUT, async {
            let headers = Headers::new();
            headers.set("Content-Type", "application/json")?;
            if let Some(token) = auth_bearer {
                headers.set("Authorization", &format!("Bearer {token}"))?;
            }
            let mut init = RequestInit::new();
            init.with_method(Method::Post)
                .with_headers(headers)
                .with_body(Some(JsValue::from_str(&payload.to_string())));
            let mut r = Fetch::Request(Request::new_with_init(url, &init)?)
                .send_with_signal(&signal)
                .await?;
            let status = r.status_code();
            if (200..300).contains(&status) {
                Result::<(u16, Option<String>)>::Ok((status, None))
            } else {
                let err_body = r.text().await.unwrap_or_default();
                Result::<(u16, Option<String>)>::Ok((status, Some(err_body)))
            }
        })
        .await;

        match outcome {
            Ok(Ok((s, None))) => console_log!("{label}: sent (HTTP {s})"),
            Ok(Ok((s, Some(body)))) => {
                let snippet = truncate_str(body.trim(), MAX_ERROR_LOG_BYTES);
                console_error!("{label}: HTTP {s}: {snippet} (ignored)");
            }
            Ok(Err(e)) => console_error!("{label}: error: {e} (ignored)"),
            Err(()) => {
                controller.abort();
                console_error!("{label}: timed out (ignored)");
            }
        }
    }

    async fn newsletter(req: &mut Request, env: &Env, _ctx: &Context) -> Result<Response> {        let fields = match read_fields(req).await? {
            Ok(f) => f,
            Err(resp) => return Ok(resp),
        };
        let signup = match validate_newsletter(&fields) {
            Ok(s) => s,
            Err(ValidationError::Honeypot) => {
                return json_response(400, &error_body("submission_rejected"));
            }
            Err(ValidationError::Fields(errs)) => {
                return json_response(400, &validation_error_body(&errs));
            }
        };
        if let Some(resp) =
            verify_turnstile(env, req, signup.turnstile_token.as_deref(), "newsletter").await?
        {
            return Ok(resp);
        }
        let now = iso8601_utc(worker::Date::now().as_millis());
        // Host-testable statement builder (crates/sws-worker-site/src/newsletter.rs):
        // CAN-SPAM: preserve existing subscriber consent audit records
        // (`consent_text`, `consent_timestamp`, `source_route`) and status
        // (`unsubscribed`/`bounced`) against anonymous overwrites.
        let up = crate::newsletter::newsletter_upsert(
            &signup.email,
            &now,
            &signup.source_route,
            signup.consent_text.as_deref(),
        );
        let binds: Vec<JsValue> = up
            .binds
            .into_iter()
            .map(|bind| match bind {
                Some(value) => JsValue::from_str(&value),
                None => JsValue::NULL,
            })
            .collect();
        let stmt = env.d1("DB")?.prepare(&up.sql).bind(&binds)?;
        if let Err(e) = stmt.run().await {
            console_error!("newsletter insert failed: {e}");
            return json_response(500, &error_body("storage_failed"));
        }
        json_response(
            201,
            // Generic body: never reveal whether an address was already on the list.
            &serde_json::json!({ "ok": true, "status": "received", "consent_timestamp": now }),
        )
    }

    // ---------- Sandbox demo checkout (no storage, no webhooks) ----------

    /// Read at most `max` bytes from a body stream; `None` when the cap is exceeded.
    async fn bounded_body(mut stream: ByteStream, max: usize) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.ok()?;
            if chunk.len() > max - bytes.len() {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        Some(bytes)
    }

    /// Outbound GET/POST returning parsed JSON; `None` on any failure. Used for
    /// Turnstile siteverify and the sandbox Stripe API. Never logs upstream errors.
    async fn checkout_fetch(
        url: &str,
        authorization: Option<&str>,
        body: Option<String>,
    ) -> Option<Value> {
        let headers = Headers::new();
        if let Some(key) = authorization {
            headers
                .set("Authorization", &format!("Bearer {key}"))
                .ok()?;
            headers.set("Stripe-Version", STRIPE_API_VERSION).ok()?;
        }
        let mut init = RequestInit::new();
        // Never follow upstream redirects: workerd rejects redirect:"error", so
        // "manual" surfaces any 3xx as a non-200 response and fails closed below.
        init.with_redirect(RequestRedirect::Manual)
            .with_headers(headers);
        if let Some(body) = body {
            init.headers
                .set("Content-Type", "application/x-www-form-urlencoded")
                .ok()?;
            init.with_method(Method::Post)
                .with_body(Some(JsValue::from_str(&body)));
        }
        let req = Request::new_with_init(url, &init).ok()?;
        let mut resp = with_timeout(OUTBOUND_TIMEOUT, Fetch::Request(req).send())
            .await
            .ok()?
            .ok()?;
        if resp.status_code() != 200 {
            return None;
        }
        let bytes = bounded_body(resp.stream().ok()?, MAX_UPSTREAM_BODY).await?;
        serde_json::from_slice(&bytes).ok()
    }

    #[derive(Deserialize)]
    struct CheckoutSiteVerify {
        success: bool,
        #[serde(default)]
        action: String,
        #[serde(default)]
        hostname: String,
    }

    /// `Ok(url)` = redirect the browser to the hosted sandbox session.
    /// `Err((status, message))` = render a plain HTML error page.
    ///
    /// This path never uses the legacy `DEV_MODE` Turnstile bypass or logs
    /// upstream error details; it fails closed whenever configuration, the
    /// Turnstile verdict, or any Stripe response is missing or unexpected.
    async fn demo_checkout(req: &mut Request, env: &Env) -> Result<Response> {
        match demo_checkout_inner(req, env).await {
            Ok(url) => {
                let resp = Response::empty()?.with_status(303);
                resp.headers().set("Location", &url)?;
                resp.headers().set("Cache-Control", API_CACHE_CONTROL)?;
                Ok(resp)
            }
            Err((status, message)) => {
                let resp = Response::from_html(format!(
                    "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Demo checkout unavailable</title><main><h1>Demo checkout unavailable</h1><p>{message}</p><p><a href=\"/takeout\">Return to demo checkout</a></p></main></html>"
                ))?
                .with_status(status);
                resp.headers().set("Cache-Control", API_CACHE_CONTROL)?;
                Ok(resp)
            }
        }
    }

    async fn demo_checkout_inner(
        req: &mut Request,
        env: &Env,
    ) -> std::result::Result<String, (u16, &'static str)> {
        const INVALID: (u16, &str) = (
            400,
            "Check the pickup quantities and complete the security check before trying again.",
        );
        const LARGE: (u16, &str) = (
            413,
            "The checkout form was too large. Go back and try again.",
        );
        const UNAVAILABLE: (u16, &str) = (
            503,
            "Demo checkout is not available right now. Please try again later.",
        );
        const STRIPE_FAILED: (u16, &str) = (
            502,
            "Stripe checkout could not be started. Please try again later.",
        );

        let content_type = req.headers().get("Content-Type").ok().flatten();
        if !content_type
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("application/x-www-form-urlencoded"))
        {
            return Err((415, "Use the demo checkout form to continue."));
        }
        let request_url = req.url().map_err(|_| INVALID)?;
        if request_url.query().is_some() {
            return Err(INVALID);
        }
        if req
            .headers()
            .get("Content-Length")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<usize>().ok())
            .is_some_and(|n| n > checkout::MAX_BODY)
        {
            return Err(LARGE);
        }
        let body = bounded_body(req.stream().map_err(|_| INVALID)?, checkout::MAX_BODY)
            .await
            .ok_or(LARGE)?;
        let origin = var(env, "DEMO_CHECKOUT_ORIGIN")
            .and_then(|v| checkout::return_origin(&v))
            .ok_or(UNAVAILABLE)?;
        let local = matches!(origin.host_str(), Some("localhost" | "127.0.0.1"));
        let ts_key = secret(env, "TURNSTILE_SECRET_KEY").ok_or(UNAVAILABLE)?;
        // Cloudflare's documented dummy secrets are for local testing only, and
        // only when the request origin exactly matches the configured loopback
        // origin. (sws-security's DEV_MODE bypass is deliberately not used here.)
        let dummy = is_turnstile_test_secret(&ts_key);
        if dummy && (!local || request_url.origin() != origin.origin()) {
            return Err(UNAVAILABLE);
        }
        let key = secret(env, "STRIPE_DEMO_SECRET_KEY")
            .filter(|s| checkout::sandbox_key(s))
            .ok_or(UNAVAILABLE)?;
        let ids = var(env, "STRIPE_DEMO_PRICE_IDS")
            .and_then(|v| serde_json::from_str(&v).ok())
            .ok_or(UNAVAILABLE)?;
        let catalog_url = origin
            .join("/demo-checkout-catalog.json")
            .map_err(|_| UNAVAILABLE)?;
        let mut asset = env
            .assets("ASSETS")
            .map_err(|_| UNAVAILABLE)?
            .fetch(catalog_url.as_str(), None)
            .await
            .map_err(|_| UNAVAILABLE)?;
        if asset.status_code() != 200 {
            return Err(UNAVAILABLE);
        }
        // First-party rendered asset, bounded in size by pack validation
        // (at most 5 items + 1 subscription, cents integers only).
        let catalog = asset.json::<Value>().await.map_err(|_| UNAVAILABLE)?;
        let selection = checkout::select(&body, &catalog).ok_or(INVALID)?;
        let ts_body = form_encode(&[
            ("secret", ts_key.as_str()),
            ("response", selection.token.as_str()),
        ]);
        let verdict = checkout_fetch(SITEVERIFY_URL, None, Some(ts_body))
            .await
            .and_then(|v| serde_json::from_value::<CheckoutSiteVerify>(v).ok())
            .ok_or(UNAVAILABLE)?;
        if !verdict.success
            || (!dummy
                && (verdict.action != "demo_checkout"
                    || Some(verdict.hostname.as_str()) != origin.host_str()))
        {
            return Err((
                403,
                "The security check failed. Go back, complete the check again, and retry.",
            ));
        }
        checkout::create(&selection, &ids, &origin, |path, body| {
            let key = key.clone();
            async move {
                checkout_fetch(
                    &format!("https://api.stripe.com{path}"),
                    Some(&key),
                    body,
                )
                .await
            }
        })
        .await
        .ok_or(STRIPE_FAILED)
    }
}
