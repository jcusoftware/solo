//! sws-worker-site: domain-agnostic published-site Worker (workers-rs).
//!
//! Endpoints (all responses pass through [`sws_security::headers::with_security_headers`]):
//! - `GET|HEAD /api/health` -> `{status, service, version}`
//! - `POST /api/forms/<id>` -> schema-driven validation + D1 `form_submissions` + optional email/webhook
//! - `POST /api/inquire` -> backward-compatible alias using the `"inquire"` form schema
//! - `POST /api/newsletter` -> D1 `newsletter_subscribers` (explicit CAN-SPAM consent)
//! - Any other path -> static assets binding (`ASSETS`)

pub mod forms;
pub mod routes;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::time::Duration;

    use serde_json::Value;
    use worker::wasm_bindgen::JsValue;
    use worker::{
        AbortSignal, Context, Env, Fetch, Headers, Method, Request, RequestInit, Response, Result,
        console_error, console_log, console_warn, event, web_sys,
    };

    use sws_security::headers::{API_CACHE_CONTROL, with_security_headers};
    use sws_security::response::{error_body, validation_error_body};
    use sws_security::validate::{ValidationError, iso8601_utc, truncate_str, validate_newsletter};
    use sws_security::worker_utils::{
        json_response, opt_str, read_fields, secret, var, verify_turnstile, with_timeout,
    };

    use crate::forms::{
        FormSchema, ValidatedSubmission, load_form_schemas, validate_submission,
    };
    use crate::routes::{Route, health_body, route};

    const RESEND_URL: &str = "https://api.resend.com/emails";
    const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);
    const MAX_ERROR_LOG_BYTES: usize = 256;

    #[event(start)]
    fn start() {
        #[cfg(feature = "console_error_panic_hook")]
        console_error_panic_hook::set_once();
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
        let mut schemas = load_form_schemas(var(env, "FORM_SCHEMAS_JSON").as_deref());
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
            if webhook_url.starts_with("https://") {
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

    async fn newsletter(req: &mut Request, env: &Env, _ctx: &Context) -> Result<Response> {
        let fields = match read_fields(req).await? {
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
        // CAN-SPAM: preserve existing subscriber consent audit records
        // (`consent_text`, `consent_timestamp`, `source_route`) and status
        // (`unsubscribed`/`bounced`) against anonymous overwrites.
        let stmt = env
            .d1("DB")?
            .prepare(
                "INSERT INTO newsletter_subscribers (email, status, consent_timestamp, source_route, consent_text) \
                 VALUES (?1, 'subscribed', ?2, ?3, ?4) \
                 ON CONFLICT(email) DO NOTHING",
            )
            .bind(&[
                JsValue::from_str(&signup.email),
                JsValue::from_str(&now),
                JsValue::from_str(&signup.source_route),
                opt_str(&signup.consent_text),
            ])?;
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
}
