//! Cloudflare Workers API client for Direct Upload (host-testable through
//! [`HttpClient`]). Endpoints and envelopes as documented on 2026-11-27 (see the
//! module docs of [`super`]).
//!
//! Retry rules:
//! - upload session, asset upload, version upload and the deployments list retry
//!   429 / 5xx / transport errors up to [`RetryPolicy::max_attempts`] with exponential
//!   backoff, honouring `Retry-After` (capped). A version upload retried after an
//!   ambiguous attempt can at worst leave an unreferenced extra version, never a
//!   traffic change.
//! - creating a deployment is sent ONCE per call ([`CfApi::create_deployment_once`]);
//!   the caller reconciles any non-definitive outcome before trying again.

use std::cell::RefCell;

use serde_json::{Value, json};

use super::http::{HttpClient, HttpRequest, HttpResponse, Runtime, TransportError};
use super::multipart::{self, FormPart};
use super::pipeline::WorkerModule;
use super::secret::{ApiToken, Redactor};

/// Activation attempts per publish; every retry follows a reconcile proving the
/// previous attempt did not apply.
pub const MAX_ACTIVATION_ATTEMPTS: u32 = 3;
/// Upper bound for one activation request (far below [`super::store::LEASE_SECS`]).
pub const ACTIVATION_TIMEOUT_MS: u64 = 30_000;
pub const REQUEST_TIMEOUT_MS: u64 = 60_000;
/// Longest `Retry-After` honoured.
pub const MAX_RETRY_AFTER_MS: u64 = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl RetryPolicy {
    pub const DEFAULT: RetryPolicy = RetryPolicy {
        max_attempts: 4,
        base_delay_ms: 500,
        max_delay_ms: MAX_RETRY_AFTER_MS,
    };

    /// Delay before attempt `attempt + 1` (attempt is 1-based).
    pub fn delay_ms(&self, attempt: u32, retry_after_secs: Option<u64>) -> u64 {
        let backoff = self
            .base_delay_ms
            .saturating_mul(1u64 << attempt.saturating_sub(1).min(16));
        retry_after_secs
            .map_or(backoff, |s| s.saturating_mul(1000))
            .min(self.max_delay_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// A definitive rejection (4xx other than 429, or `success: false` on a 2xx).
    /// `after_ambiguous`: an earlier attempt of the same call may have taken effect.
    Rejected {
        status: u16,
        message: String,
        after_ambiguous: bool,
    },
    /// 429 / 5xx / transport / unparseable answer, after the retry budget (or the single
    /// attempt of a non-retried call). Whether the request took effect is unknown.
    Unavailable {
        status: Option<u16>,
        message: String,
    },
}

impl ApiError {
    pub fn message(&self) -> &str {
        match self {
            ApiError::Rejected { message, .. } | ApiError::Unavailable { message, .. } => message,
        }
    }

    /// May the whole operation be retried later?
    pub fn retryable(&self) -> bool {
        matches!(self, ApiError::Unavailable { .. })
            || matches!(
                self,
                ApiError::Rejected {
                    after_ambiguous: true,
                    ..
                }
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadSession {
    pub jwt: String,
    pub buckets: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Deployment {
    pub id: String,
    pub versions: Vec<(String, f64)>,
}

impl Deployment {
    /// Exactly this version at 100%.
    pub fn is_only(&self, version_id: &str) -> bool {
        self.versions.len() == 1 && self.versions[0].0 == version_id && self.versions[0].1 == 100.0
    }
}

/// One file in an upload request: hash, content type, base64 body.
pub struct UploadFile {
    pub hash: String,
    pub content_type: &'static str,
    pub b64: String,
}

pub struct CfApi<'a, C: HttpClient, R: Runtime> {
    http: &'a C,
    rt: &'a R,
    base: &'a str,
    account_id: &'a str,
    script: &'a str,
    token: &'a ApiToken,
    pub retry: RetryPolicy,
    redactor: RefCell<Redactor>,
}

fn is_retryable_status(s: u16) -> bool {
    s == 429 || (500..=599).contains(&s)
}

impl<'a, C: HttpClient, R: Runtime> CfApi<'a, C, R> {
    pub fn new(
        http: &'a C,
        rt: &'a R,
        base: &'a str,
        account_id: &'a str,
        script: &'a str,
        token: &'a ApiToken,
    ) -> Self {
        Self {
            http,
            rt,
            base,
            account_id,
            script,
            token,
            retry: RetryPolicy::DEFAULT,
            redactor: RefCell::new(Redactor::new(token)),
        }
    }

    /// Scrub every secret seen so far (token, upload and completion JWTs).
    pub fn clean(&self, text: &str) -> String {
        self.redactor.borrow().clean(text)
    }

    fn remember(&self, secret: &str) {
        self.redactor.borrow_mut().add(secret);
    }

    fn script_url(&self, tail: &str) -> String {
        format!(
            "{}/accounts/{}/workers/scripts/{}/{tail}",
            self.base, self.account_id, self.script
        )
    }

    fn bearer(&self) -> (String, String) {
        (
            "Authorization".into(),
            format!("Bearer {}", self.token.expose()),
        )
    }

    fn json_request(
        &self,
        method: &'static str,
        url: String,
        body: Option<&Value>,
        timeout_ms: u64,
    ) -> HttpRequest {
        let mut headers = vec![self.bearer()];
        let body = match body {
            Some(b) => {
                headers.push(("Content-Type".into(), "application/json".into()));
                serde_json::to_vec(b).unwrap_or_default()
            }
            None => Vec::new(),
        };
        HttpRequest {
            method,
            url,
            headers,
            body,
            timeout_ms,
        }
    }

    /// Summarize a Cloudflare error envelope as redacted `code: message` text.
    fn describe(&self, resp: &HttpResponse) -> String {
        let parsed: Option<Value> = serde_json::from_slice(&resp.body).ok();
        let errors = parsed
            .as_ref()
            .and_then(|v| v.get("errors"))
            .and_then(Value::as_array)
            .map(|errs| {
                errs.iter()
                    .take(3)
                    .map(|e| {
                        format!(
                            "{}: {}",
                            e.get("code").and_then(Value::as_i64).unwrap_or(0),
                            e.get("message").and_then(Value::as_str).unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|s| !s.is_empty());
        self.clean(&format!(
            "HTTP {}{}",
            resp.status,
            errors.map(|e| format!(" ({e})")).unwrap_or_default()
        ))
    }

    fn transport_message(&self, e: &TransportError) -> String {
        match e {
            TransportError::Timeout => "request timed out".into(),
            TransportError::Network(m) => self.clean(&format!("network error: {m}")),
        }
    }

    /// Send with the retry policy (`retry = false`: one attempt). Returns the
    /// envelope's `result` and the HTTP status.
    async fn call(
        &self,
        what: &str,
        retry: bool,
        build: impl Fn() -> HttpRequest,
    ) -> Result<(u16, Value), ApiError> {
        let attempts = if retry {
            self.retry.max_attempts.max(1)
        } else {
            1
        };
        let mut ambiguous = false;
        let mut last = ApiError::Unavailable {
            status: None,
            message: "not attempted".into(),
        };
        for attempt in 1..=attempts {
            let mut retry_after = None;
            match self.http.send(build()).await {
                Ok(resp) if (200..300).contains(&resp.status) => {
                    let env: Option<Value> = serde_json::from_slice(&resp.body).ok();
                    match env {
                        Some(v) if v.get("success") == Some(&Value::Bool(true)) => {
                            return Ok((
                                resp.status,
                                v.get("result").cloned().unwrap_or(Value::Null),
                            ));
                        }
                        Some(_) => {
                            return Err(ApiError::Rejected {
                                status: resp.status,
                                message: self.describe(&resp),
                                after_ambiguous: ambiguous,
                            });
                        }
                        None => {
                            ambiguous = true;
                            last = ApiError::Unavailable {
                                status: Some(resp.status),
                                message: format!("HTTP {} with an unparseable body", resp.status),
                            };
                        }
                    }
                }
                Ok(resp) if is_retryable_status(resp.status) => {
                    // A 429 was not processed; a 5xx may have been.
                    ambiguous |= resp.status != 429;
                    retry_after = resp.retry_after_secs;
                    last = ApiError::Unavailable {
                        status: Some(resp.status),
                        message: self.describe(&resp),
                    };
                }
                Ok(resp) => {
                    return Err(ApiError::Rejected {
                        status: resp.status,
                        message: self.describe(&resp),
                        after_ambiguous: ambiguous,
                    });
                }
                Err(e) => {
                    ambiguous = true;
                    last = ApiError::Unavailable {
                        status: None,
                        message: self.transport_message(&e),
                    };
                }
            }
            if attempt < attempts {
                let delay = self.retry.delay_ms(attempt, retry_after);
                self.rt.log(&format!(
                    "cloudflare {what}: attempt {attempt} failed, retrying in {delay} ms"
                ));
                self.rt.sleep_ms(delay).await;
            }
        }
        Err(last)
    }

    fn malformed(&self, what: &str) -> ApiError {
        ApiError::Unavailable {
            status: None,
            message: format!("unexpected {what} response shape"),
        }
    }

    /// `POST .../assets-upload-session` with the full manifest.
    pub async fn upload_session(&self, body: &Value) -> Result<UploadSession, ApiError> {
        let url = self.script_url("assets-upload-session");
        let (_, r) = self
            .call("upload session", true, || {
                self.json_request("POST", url.clone(), Some(body), REQUEST_TIMEOUT_MS)
            })
            .await?;
        let jwt = r
            .get("jwt")
            .and_then(Value::as_str)
            .ok_or_else(|| self.malformed("upload session"))?;
        self.remember(jwt);
        let buckets = match r.get("buckets") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(bs)) => bs
                .iter()
                .map(|b| {
                    b.as_array()
                        .map(|hs| {
                            hs.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .ok_or_else(|| self.malformed("upload session"))
                })
                .collect::<Result<Vec<Vec<String>>, ApiError>>()?,
            Some(_) => return Err(self.malformed("upload session")),
        };
        Ok(UploadSession {
            jwt: jwt.to_string(),
            buckets,
        })
    }

    /// `POST /workers/assets/upload?base64=true` with the upload JWT (NOT the account
    /// token). Returns the completion JWT when this request completed the set.
    pub async fn upload_files(
        &self,
        upload_jwt: &str,
        files: &[UploadFile],
    ) -> Result<Option<String>, ApiError> {
        let url = format!(
            "{}/accounts/{}/workers/assets/upload?base64=true",
            self.base, self.account_id
        );
        let parts: Vec<FormPart<'_>> = files
            .iter()
            .map(|f| FormPart {
                name: &f.hash,
                filename: Some(&f.hash),
                content_type: Some(f.content_type),
                body: f.b64.as_bytes(),
            })
            .collect();
        let (ct, body) = multipart::encode(&parts).ok_or_else(|| self.malformed("upload"))?;
        drop(parts);
        let (_, r) = self
            .call("asset upload", true, || HttpRequest {
                method: "POST",
                url: url.clone(),
                headers: vec![
                    ("Authorization".into(), format!("Bearer {upload_jwt}")),
                    ("Content-Type".into(), ct.clone()),
                ],
                body: body.clone(),
                timeout_ms: REQUEST_TIMEOUT_MS,
            })
            .await?;
        let jwt = r.get("jwt").and_then(Value::as_str).map(str::to_string);
        if let Some(j) = &jwt {
            self.remember(j);
        }
        Ok(jwt)
    }

    /// `POST .../versions`: creates a version WITHOUT deploying it. Returns its id.
    pub async fn create_version(
        &self,
        metadata: &Value,
        modules: &[WorkerModule],
    ) -> Result<String, ApiError> {
        let url = self.script_url("versions");
        let meta = serde_json::to_vec(metadata).unwrap_or_default();
        let mut parts = vec![FormPart {
            name: "metadata",
            filename: None,
            content_type: Some("application/json"),
            body: &meta,
        }];
        parts.extend(modules.iter().map(|m| FormPart {
            name: &m.name,
            filename: Some(&m.name),
            content_type: Some(&m.content_type),
            body: &m.bytes,
        }));
        let (ct, body) = multipart::encode(&parts).ok_or_else(|| self.malformed("version"))?;
        drop(parts);
        let (_, r) = self
            .call("version upload", true, || HttpRequest {
                method: "POST",
                url: url.clone(),
                headers: vec![self.bearer(), ("Content-Type".into(), ct.clone())],
                body: body.clone(),
                timeout_ms: REQUEST_TIMEOUT_MS,
            })
            .await?;
        r.get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_version_id(id))
            .map(str::to_string)
            .ok_or_else(|| self.malformed("version"))
    }

    /// `POST .../deployments` with the version at 100%. ONE attempt, never `force`.
    pub async fn create_deployment_once(&self, version_id: &str) -> Result<String, ApiError> {
        let url = self.script_url("deployments");
        let body = json!({
            "strategy": "percentage",
            "versions": [{ "version_id": version_id, "percentage": 100 }],
            "annotations": { "workers/triggered_by": "solo-studio" },
        });
        let (_, r) = self
            .call("activation", false, || {
                self.json_request("POST", url.clone(), Some(&body), ACTIVATION_TIMEOUT_MS)
            })
            .await?;
        // A 2xx is a definitive success even if the body lacks an id.
        Ok(r.get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// `GET .../deployments`: newest first; the first serves traffic.
    pub async fn list_deployments(&self) -> Result<Vec<Deployment>, ApiError> {
        let url = self.script_url("deployments");
        let (_, r) = self
            .call("deployments list", true, || {
                self.json_request("GET", url.clone(), None, REQUEST_TIMEOUT_MS)
            })
            .await?;
        let list = r
            .get("deployments")
            .and_then(Value::as_array)
            .ok_or_else(|| self.malformed("deployments list"))?;
        Ok(list
            .iter()
            .map(|d| Deployment {
                id: d
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                versions: d
                    .get("versions")
                    .and_then(Value::as_array)
                    .map(|vs| {
                        vs.iter()
                            .filter_map(|v| {
                                Some((
                                    v.get("version_id")?.as_str()?.to_string(),
                                    v.get("percentage")?.as_f64()?,
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect())
    }
}

/// Cloudflare version ids are UUIDs.
pub fn valid_version_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_capped_and_honours_retry_after() {
        let p = RetryPolicy::DEFAULT;
        assert_eq!(p.delay_ms(1, None), 500);
        assert_eq!(p.delay_ms(2, None), 1000);
        assert_eq!(p.delay_ms(3, None), 2000);
        assert_eq!(p.delay_ms(10, None), MAX_RETRY_AFTER_MS);
        assert_eq!(p.delay_ms(1, Some(3)), 3000);
        assert_eq!(p.delay_ms(1, Some(3600)), MAX_RETRY_AFTER_MS);
    }

    #[test]
    fn version_ids_are_uuids() {
        assert!(valid_version_id("18f97339-c287-4872-9bdd-e2135c07ec12"));
        assert!(!valid_version_id("18f97339c28748729bdde2135c07ec12xxxx"));
        assert!(!valid_version_id(""));
    }
}
