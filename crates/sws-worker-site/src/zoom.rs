//! Zoom Server-to-Server OAuth meeting creation for the call booking flow
//! (issue #44 port). Pure host-id validation stays host-testable; the WASM
//! client is config-gated and only runs when `auto_zoom` is enabled and the
//! ZOOM_* secrets are provisioned in the Worker environment.

/// Validates the opaque Zoom host id used in `users/{host}/meetings` URLs:
/// never an email, never the `me` alias, and only URL-safe opaque characters.
pub fn valid_zoom_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.eq_ignore_ascii_case("me")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_requires_an_explicit_opaque_host_id() {
        assert!(valid_zoom_host("aB12_c-D"));
        for value in ["", "me", "ME", "..", "a/b", "a%2Fb", " a", "host@example.test"] {
            assert!(!valid_zoom_host(value));
        }
        assert!(!valid_zoom_host(&"x".repeat(129)));
    }
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use std::cell::RefCell;

    use serde::Deserialize;
    use serde_json::json;
    use worker::{
        Date, Env, Error, Headers, Method, Result, Url, js_sys::global, wasm_bindgen::JsCast,
        web_sys::WorkerGlobalScope,
    };

    use crate::google_auth::{cache_seconds, wasm::fetch_json};

    #[derive(Deserialize)]
    struct Token {
        access_token: String,
        token_type: String,
        expires_in: u64,
    }

    #[derive(PartialEq, Eq)]
    struct Key {
        tenant: String,
        account: String,
        client: String,
    }

    struct Cached {
        key: Key,
        bearer: String,
        expires: u64,
    }

    thread_local! {
        static CACHE: RefCell<Option<Cached>> = const { RefCell::new(None) };
    }

    fn failure() -> Error {
        Error::RustError("Zoom provider unavailable".into())
    }

    pub async fn access_token(env: &Env, tenant: &str) -> Result<String> {
        let setting = |name| {
            env.secret(name)
                .map(|s| s.to_string())
                .map_err(|_| failure())
        };
        let key = Key {
            tenant: tenant.into(),
            account: setting("ZOOM_ACCOUNT_ID")?,
            client: setting("ZOOM_CLIENT_ID")?,
        };
        if key.account.is_empty() || key.client.is_empty() || key.client.contains(':') {
            return Err(failure());
        }
        let now = Date::now().as_millis() / 1000;
        if let Some(token) = CACHE.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .filter(|entry| entry.key == key && now < entry.expires)
                .map(|entry| entry.bearer.clone())
        }) {
            return Ok(token);
        }
        let secret = setting("ZOOM_CLIENT_SECRET")?;
        if secret.is_empty() || !secret.is_ascii() || !key.client.is_ascii() {
            return Err(failure());
        }
        let scope: WorkerGlobalScope = global().unchecked_into();
        let basic = scope
            .btoa(&format!("{}:{secret}", key.client))
            .map_err(|_| failure())?;
        let mut url = Url::parse("https://zoom.us/oauth/token").map_err(|_| failure())?;
        url.query_pairs_mut()
            .append_pair("grant_type", "account_credentials")
            .append_pair("account_id", &key.account);
        let headers = Headers::new();
        headers.set("Authorization", &format!("Basic {basic}"))?;
        let token: Token = fetch_json(url.as_str(), Method::Post, headers, None, 16 * 1024).await?;
        if !token.token_type.eq_ignore_ascii_case("bearer")
            || token.access_token.is_empty()
            || token.expires_in == 0
        {
            return Err(failure());
        }
        let bearer = token.access_token.clone();
        CACHE.with(|cache| {
            *cache.borrow_mut() = Some(Cached {
                key,
                bearer: token.access_token,
                expires: now + cache_seconds(token.expires_in),
            })
        });
        Ok(bearer)
    }

    #[derive(Deserialize)]
    pub struct Meeting {
        pub id: u64,
        pub join_url: String,
        // Never persist or return Zoom's standalone password.
        #[serde(rename = "password")]
        _password: Option<String>,
    }

    pub fn meetings_url(env: &Env) -> Result<Url> {
        let host = env
            .secret("ZOOM_HOST_USER_ID")
            .map_err(|_| failure())?
            .to_string();
        if !super::valid_zoom_host(&host) {
            return Err(failure());
        }
        let mut url = Url::parse("https://api.zoom.us/v2/users").map_err(|_| failure())?;
        url.path_segments_mut()
            .map_err(|_| failure())?
            .push(&host)
            .push("meetings");
        Ok(url)
    }

    pub async fn create(
        url: &Url,
        bearer: &str,
        reference: &str,
        start: &str,
        duration: u16,
    ) -> Result<Meeting> {
        let headers = Headers::new();
        headers.set("Authorization", &format!("Bearer {bearer}"))?;
        headers.set("Content-Type", "application/json")?;
        let body = json!({ "topic": format!("Venue call [{reference}]"), "type": 2,
            "start_time": start, "duration": duration, "timezone": "UTC",
            "settings": { "waiting_room": true, "join_before_host": false } })
        .to_string();
        let meeting: Meeting = fetch_json(
            url.as_str(),
            Method::Post,
            headers,
            Some(&body),
            64 * 1024,
        )
        .await?;
        let url = Url::parse(&meeting.join_url).map_err(|_| failure())?;
        if meeting.id == 0
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || !url
                .host_str()
                .is_some_and(|host| host == "zoom.us" || host.ends_with(".zoom.us"))
        {
            return Err(failure());
        }
        Ok(meeting)
    }
}
