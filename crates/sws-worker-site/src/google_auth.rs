//! Google OAuth2 service-account (JWT bearer) authentication for the events
//! calendar sync (issue #43 port). Pure helpers are host-testable; the WASM
//! crypto path is gated behind `target_arch = "wasm32"`.

#[cfg(any(target_arch = "wasm32", test))]
use serde_json::{Value, json};

// Pure helpers and the token URL are consumed by the wasm module and pinned by
// the host tests; the `test` arm keeps them compiled for `cargo test` on host.
#[cfg(any(target_arch = "wasm32", test))]
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[cfg(any(target_arch = "wasm32", test))]
fn claims(issuer: &str, subject: &str, scope: &str, now: u64) -> Value {
    json!({ "iss": issuer, "sub": subject, "scope": scope, "aud": TOKEN_URL,
        "iat": now, "exp": now + 300 })
}

#[cfg(any(target_arch = "wasm32", test))]
pub(super) fn cache_seconds(expires_in: u64) -> u64 {
    expires_in.saturating_sub(60).min(55 * 60)
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use std::cell::RefCell;

    use futures_util::StreamExt;
    use serde::{Deserialize, de::DeserializeOwned};
    use sws_security::validate::form_encode;
    use worker::{
        AbortSignal, Date, Env, Error, Fetch, Headers, Method, Request, RequestInit,
        RequestRedirect, Result,
        js_sys::{Array, Uint8Array, global},
        wasm_bindgen::{JsCast, JsValue},
        wasm_bindgen_futures::JsFuture,
        web_sys::{CryptoKey, WorkerGlobalScope},
    };

    use super::{TOKEN_URL, cache_seconds, claims};

    const ALGORITHM: &str = "RSASSA-PKCS1-v1_5";

    #[derive(PartialEq)]
    struct CacheKey {
        tenant: String,
        issuer: String,
        subject: String,
        scope: String,
    }

    struct Cached {
        key: CacheKey,
        token: String,
        expires: u64,
    }

    thread_local! {
        // One entry per isolate; distinct tenants, subjects and scopes never share a token.
        static CACHE: RefCell<Option<Cached>> = const { RefCell::new(None) };
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        expires_in: u64,
        token_type: String,
    }

    fn auth_error() -> Error {
        Error::RustError("Google authentication failed".into())
    }

    fn b64url(scope: &WorkerGlobalScope, bytes: &[u8]) -> Result<String> {
        let binary: String = bytes.iter().copied().map(char::from).collect();
        Ok(scope
            .btoa(&binary)
            .map_err(|_| auth_error())?
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_"))
    }

    async fn assertion(key: &CacheKey, pem: &str, now: u64) -> Result<String> {
        let scope: WorkerGlobalScope = global().unchecked_into();
        let pem = pem.trim();
        let der = pem
            .strip_prefix("-----BEGIN PRIVATE KEY-----")
            .and_then(|s| s.strip_suffix("-----END PRIVATE KEY-----"))
            .ok_or_else(auth_error)?;
        let der: String = der.chars().filter(|ch| !ch.is_ascii_whitespace()).collect();
        let der: Vec<u8> = scope
            .atob(&der)
            .map_err(|_| auth_error())?
            .chars()
            .map(|ch| ch as u8)
            .collect();
        let crypto = scope.crypto().map_err(|_| auth_error())?.subtle();
        let algorithm =
            worker::js_sys::JSON::parse(r#"{"name":"RSASSA-PKCS1-v1_5","hash":"SHA-256"}"#)
                .map_err(|_| auth_error())?;
        let usages = Array::of1(&JsValue::from_str("sign"));
        let private_key: CryptoKey = JsFuture::from(
            crypto
                .import_key_with_object(
                    "pkcs8",
                    Uint8Array::from(der.as_slice()).as_ref(),
                    algorithm.unchecked_ref(),
                    false,
                    usages.as_ref(),
                )
                .map_err(|_| auth_error())?,
        )
        .await
        .map_err(|_| auth_error())?
        .unchecked_into();
        let input = format!(
            "{}.{}",
            b64url(&scope, br#"{"alg":"RS256","typ":"JWT"}"#)?,
            b64url(
                &scope,
                claims(&key.issuer, &key.subject, &key.scope, now)
                    .to_string()
                    .as_bytes()
            )?
        );
        let signature = JsFuture::from(
            crypto
                .sign_with_str_and_js_u8_array(
                    ALGORITHM,
                    &private_key,
                    &Uint8Array::from(input.as_bytes()),
                )
                .map_err(|_| auth_error())?,
        )
        .await
        .map_err(|_| auth_error())?;
        Ok(format!(
            "{input}.{}",
            b64url(&scope, &Uint8Array::new(&signature).to_vec())?
        ))
    }

    pub async fn fetch_json<T: DeserializeOwned>(
        url: &str,
        method: Method,
        headers: Headers,
        body: Option<&str>,
        limit: usize,
    ) -> Result<T> {
        let failure = || Error::RustError("Scheduling provider request failed".into());
        let mut init = RequestInit::new();
        init.with_method(method)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Error)
            .with_body(body.map(JsValue::from_str));
        let signal = AbortSignal::from(worker::web_sys::AbortSignal::timeout_with_u32(15_000));
        let mut response =
            Fetch::Request(Request::new_with_init(url, &init).map_err(|_| failure())?)
                .send_with_signal(&signal)
                .await
                .map_err(|_| failure())?;
        if !(200..300).contains(&response.status_code()) {
            return Err(failure());
        }
        let mut stream = response.stream().map_err(|_| failure())?;
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| failure())?;
            if chunk.len() > limit - bytes.len() {
                return Err(failure());
            }
            bytes.extend(chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| failure())
    }

    pub async fn access_token(
        env: &Env,
        tenant: &str,
        subject: &str,
        scope: &str,
    ) -> Result<String> {
        let issuer = env
            .secret("GOOGLE_SA_EMAIL")
            .map_err(|_| auth_error())?
            .to_string();
        let key = CacheKey {
            tenant: tenant.into(),
            issuer: issuer.trim().into(),
            subject: subject.into(),
            scope: scope.into(),
        };
        if key.issuer.is_empty() || key.subject.is_empty() {
            return Err(auth_error());
        }
        let now = Date::now().as_millis() / 1_000;
        if let Some(token) = CACHE.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .filter(|entry| entry.key == key && now < entry.expires)
                .map(|entry| entry.token.clone())
        }) {
            return Ok(token);
        }
        let pem = env
            .secret("GOOGLE_SA_PRIVATE_KEY")
            .map_err(|_| auth_error())?
            .to_string();
        let jwt = assertion(&key, &pem, now).await?;
        let body = form_encode(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &jwt),
        ]);
        let headers = Headers::new();
        headers.set("Content-Type", "application/x-www-form-urlencoded")?;
        let token: TokenResponse =
            fetch_json(TOKEN_URL, Method::Post, headers, Some(&body), 16 * 1024).await?;
        if !token.token_type.eq_ignore_ascii_case("bearer")
            || token.access_token.is_empty()
            || token.expires_in == 0
        {
            return Err(auth_error());
        }
        let value = token.access_token.clone();
        CACHE.with(|cache| {
            *cache.borrow_mut() = Some(Cached {
                key,
                token: token.access_token,
                expires: now + cache_seconds(token.expires_in),
            })
        });
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegated_assertion_claims_and_cache_expiry() {
        let value = claims("issuer@example.test", "user@example.test", "scope", 1_000);
        assert_eq!(
            value,
            json!({"iss":"issuer@example.test", "sub":"user@example.test", "scope":"scope", "aud":TOKEN_URL, "iat":1000, "exp":1300})
        );
        assert_eq!(cache_seconds(7200), 3300);
        assert_eq!(cache_seconds(300), 240);
        assert_eq!(cache_seconds(60), 0);
    }
}
