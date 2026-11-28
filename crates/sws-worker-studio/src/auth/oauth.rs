//! GitHub OAuth (web application flow) helpers, pure and host-tested.
//!
//! The Worker redirects to `{base}/login/oauth/authorize`, exchanges the code at
//! `{base}/login/oauth/access_token` (`Accept: application/json`, form body, so
//! no secret travels in a URL), reads the numeric user id from `{api}/user`,
//! and then DISCARDS the GitHub access token: Studio never stores or reuses it.
//! No GitHub scope is requested (public profile only).

use serde_json::Value;

pub const DEFAULT_OAUTH_BASE: &str = "https://github.com";
pub const DEFAULT_API_BASE: &str = "https://api.github.com";
pub const CALLBACK_PATH: &str = "/auth/github/callback";

/// Largest integer a JS number (D1 binding) holds exactly.
const MAX_SAFE_INT: i64 = (1 << 53) - 1;

/// RFC 3986 unreserved characters pass through; everything else is `%XX`.
pub fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `application/x-www-form-urlencoded` body (also used for query strings).
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A configurable endpoint base: `https://host[:port]`, or plain `http` only for
/// a loopback host (local tests with a fake GitHub). No path, query or
/// credentials. Trailing `/` is dropped. Anything else is refused (`None`), so
/// a misconfiguration can never send the client secret over plain HTTP.
pub fn endpoint_base(raw: Option<&str>, default: &str) -> Option<String> {
    let raw = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default);
    let norm = crate::session::normalize_origin(raw)?;
    if norm.starts_with("https://") {
        return Some(norm);
    }
    let host = norm.strip_prefix("http://")?;
    crate::session::devfence::is_loopback_host(host).then_some(norm)
}

/// `{public_origin}/auth/github/callback`, from the configured public origin.
pub fn redirect_uri(public_origin: &str) -> Option<String> {
    let o = crate::session::normalize_origin(public_origin)?;
    Some(format!("{o}{CALLBACK_PATH}"))
}

pub fn authorize_url(base: &str, client_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "{base}/login/oauth/authorize?{}",
        form(&[
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("state", state),
            ("allow_signup", "false"),
        ])
    )
}

pub fn token_url(base: &str) -> String {
    format!("{base}/login/oauth/access_token")
}

pub fn user_url(api: &str) -> String {
    format!("{api}/user")
}

pub fn token_request_body(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> String {
    form(&[
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthError {
    /// GitHub refused the code (expired, reused, wrong redirect) or answered
    /// something unusable.
    Exchange,
    /// The user endpoint answered something unusable.
    Profile,
}

/// The access token from the exchange response. The value is never logged.
pub fn parse_token_response(v: &Value) -> Result<String, OAuthError> {
    if v.get("error").is_some() {
        return Err(OAuthError::Exchange);
    }
    let t = v
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or(OAuthError::Exchange)?;
    if t.is_empty() || t.len() > 1024 || t.bytes().any(|b| !b.is_ascii_graphic()) {
        return Err(OAuthError::Exchange);
    }
    Ok(t.to_string())
}

/// GitHub login rules: 1-39 chars, ASCII alphanumeric or `-`, not starting or
/// ending with `-`.
pub fn is_github_login(s: &str) -> bool {
    (1..=39).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubUser {
    pub id: i64,
    pub login: String,
}

pub fn parse_user(v: &Value) -> Result<GithubUser, OAuthError> {
    let id = v
        .get("id")
        .and_then(Value::as_i64)
        .ok_or(OAuthError::Profile)?;
    let login = v
        .get("login")
        .and_then(Value::as_str)
        .ok_or(OAuthError::Profile)?;
    if !(1..=MAX_SAFE_INT).contains(&id) || !is_github_login(login) {
        return Err(OAuthError::Profile);
    }
    Ok(GithubUser {
        id,
        login: login.to_string(),
    })
}

/// The configured bootstrap owner id (`STUDIO_OWNER_GITHUB_ID`), if valid.
pub fn owner_id(raw: Option<&str>) -> Option<i64> {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| (1..=MAX_SAFE_INT).contains(n))
}

/// The value of the `__Host-sws_oauth` cookie that binds a sign-in to the
/// browser that started it: `<state>` or, from the invitation form,
/// `<state>.<invitation code>`. Nothing about a started sign-in is stored
/// server-side, so `GET /auth/github/start` writes nothing to D1. The invitation
/// code is the one the invitee just typed (already verified, Turnstile passed);
/// carrying it here, instead of only its id, means the callback re-verifies
/// possession of the code and a cookie edited by its own holder gains nothing.
pub fn binding_value(state: &str, invite_code: Option<&str>) -> String {
    match invite_code {
        Some(code) => format!("{state}.{code}"),
        None => state.to_string(),
    }
}

/// A verified browser binding: the cookie parsed strictly and its state equal
/// (constant time) to the callback's `state` query parameter.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Binding<'a> {
    /// `(lookup id, full code)` of the invitation the flow started from.
    pub invitation: Option<(&'a str, &'a str)>,
}

impl std::fmt::Debug for Binding<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("invitation", &self.invitation.map(|(id, _)| id))
            .finish()
    }
}

/// Check the callback's `state` against the binding cookie. `None` (refuse)
/// unless both are present, well formed, and the states are equal. The state
/// is single use because the callback always clears the cookie and GitHub
/// codes are single use.
pub fn check_binding<'a>(cookie: Option<&'a str>, query_state: &str) -> Option<Binding<'a>> {
    use super::secret::{self, Kind};
    let (state, invite) = match cookie?.split_once('.') {
        Some((s, code)) => (s, Some(code)),
        None => (cookie?, None),
    };
    if !secret::is_state(state)
        || !secret::is_state(query_state)
        || !secret::ct_eq(state.as_bytes(), query_state.as_bytes())
    {
        return None;
    }
    let invitation = match invite {
        Some(code) => Some((secret::parse(Kind::Invite, code)?, code)),
        None => None,
    };
    Some(Binding { invitation })
}

/// What a completed GitHub sign-in may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// The pre-declared owner: create the bootstrap workspace and owner
    /// membership if no owner exists yet, then sign in.
    Bootstrap,
    /// Accept the invitation carried by the OAuth binding cookie, then sign in.
    AcceptInvitation,
    /// A known user with at least one membership.
    Existing,
    /// Nobody invited this account.
    Refuse,
}

/// Decide from facts read in D1. Invitations win (they are explicit); the
/// configured owner is next; otherwise only users who already belong somewhere
/// may sign in. A stranger never gets a `users` row.
pub fn decide(
    github_id: i64,
    configured_owner: Option<i64>,
    has_invitation: bool,
    has_membership: bool,
) -> SignIn {
    if has_invitation {
        SignIn::AcceptInvitation
    } else if configured_owner == Some(github_id) {
        SignIn::Bootstrap
    } else if has_membership {
        SignIn::Existing
    } else {
        SignIn::Refuse
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn authorize_url_encodes_everything() {
        let u = authorize_url(
            "https://github.com",
            "Iv1.abc",
            "https://studio.example/auth/github/callback",
            "s&t",
        );
        assert_eq!(
            u,
            "https://github.com/login/oauth/authorize?client_id=Iv1.abc&redirect_uri=https%3A%2F%2Fstudio.example%2Fauth%2Fgithub%2Fcallback&state=s%26t&allow_signup=false"
        );
        assert!(!u.contains("scope="), "no GitHub scope is requested");
        assert_eq!(
            token_request_body("id", "se cret", "c=1", "r"),
            "client_id=id&client_secret=se%20cret&code=c%3D1&redirect_uri=r"
        );
    }

    #[test]
    fn endpoint_bases_refuse_plain_http_except_loopback() {
        assert_eq!(
            endpoint_base(None, DEFAULT_OAUTH_BASE).as_deref(),
            Some("https://github.com")
        );
        assert_eq!(
            endpoint_base(Some("http://127.0.0.1:9911/"), DEFAULT_OAUTH_BASE).as_deref(),
            Some("http://127.0.0.1:9911")
        );
        assert_eq!(
            endpoint_base(Some("  "), DEFAULT_API_BASE).as_deref(),
            Some("https://api.github.com")
        );
        for bad in [
            "http://github.com",
            "http://127.0.0.1.evil.example",
            "https://github.com/path",
            "https://user@github.com",
            "ftp://github.com",
            "javascript:alert(1)",
        ] {
            assert_eq!(endpoint_base(Some(bad), DEFAULT_OAUTH_BASE), None, "{bad}");
        }
        assert_eq!(
            redirect_uri("https://Studio.Example/").as_deref(),
            Some("https://studio.example/auth/github/callback")
        );
        assert_eq!(redirect_uri("null"), None);
    }

    #[test]
    fn token_response_parsing() {
        assert_eq!(
            parse_token_response(&json!({"access_token":"gho_x","token_type":"bearer"})),
            Ok("gho_x".into())
        );
        for bad in [
            json!({"error":"bad_verification_code","access_token":"x"}),
            json!({}),
            json!({"access_token": ""}),
            json!({"access_token": 5}),
            json!({"access_token": "a b"}),
        ] {
            assert_eq!(
                parse_token_response(&bad),
                Err(OAuthError::Exchange),
                "{bad}"
            );
        }
    }

    #[test]
    fn user_parsing_validates_id_and_login() {
        assert_eq!(
            parse_user(&json!({"id": 1001, "login": "octo-cat"})),
            Ok(GithubUser {
                id: 1001,
                login: "octo-cat".into()
            })
        );
        for bad in [
            json!({"id": 0, "login": "a"}),
            json!({"id": -1, "login": "a"}),
            json!({"id": "1001", "login": "a"}),
            json!({"id": 9_007_199_254_740_992_i64, "login": "a"}),
            json!({"id": 1, "login": ""}),
            json!({"id": 1, "login": "-a"}),
            json!({"id": 1, "login": "a".repeat(40)}),
            json!({"id": 1, "login": "<script>"}),
            json!({"id": 1}),
        ] {
            assert_eq!(parse_user(&bad), Err(OAuthError::Profile), "{bad}");
        }
        assert_eq!(owner_id(Some(" 42 ")), Some(42));
        assert_eq!(owner_id(Some("0")), None);
        assert_eq!(owner_id(Some("x")), None);
        assert_eq!(owner_id(None), None);
    }

    #[test]
    fn sign_in_decisions() {
        assert_eq!(decide(7, Some(7), false, false), SignIn::Bootstrap);
        assert_eq!(decide(7, Some(7), false, true), SignIn::Bootstrap);
        assert_eq!(
            decide(8, Some(7), false, false),
            SignIn::Refuse,
            "non-owner first sign-in"
        );
        assert_eq!(
            decide(8, None, false, false),
            SignIn::Refuse,
            "no owner configured"
        );
        assert_eq!(decide(8, Some(7), false, true), SignIn::Existing);
        assert_eq!(decide(8, Some(7), true, false), SignIn::AcceptInvitation);
    }

    #[test]
    fn browser_binding_requires_matching_state_and_valid_invitation() {
        use crate::auth::secret::{Kind, mint, mint_state};
        let state = mint_state(&[0xabu8; 32]);
        let other = mint_state(&[6u8; 32]);
        let inv = mint(Kind::Invite, &[8u8; 44]);
        let plain = binding_value(&state, None);
        assert_eq!(plain, state);
        assert_eq!(
            check_binding(Some(&plain), &state),
            Some(Binding { invitation: None })
        );
        let with = binding_value(&state, Some(&inv.value));
        assert_eq!(with, format!("{state}.{}", inv.value));
        assert_eq!(
            check_binding(Some(&with), &state),
            Some(Binding {
                invitation: Some((inv.id.as_str(), inv.value.as_str()))
            })
        );
        assert!(!format!("{:?}", check_binding(Some(&with), &state)).contains(&inv.value));
        // Missing cookie, mismatched state, malformed parts: refused.
        assert_eq!(check_binding(None, &state), None);
        assert_eq!(check_binding(Some(&plain), &other), None);
        assert_eq!(check_binding(Some(&with), &other), None);
        assert_eq!(check_binding(Some(""), ""), None);
        assert_eq!(
            check_binding(Some(&state.to_uppercase()), &state.to_uppercase()),
            None
        );
        assert_eq!(check_binding(Some(&state[..63]), &state[..63]), None);
        assert_eq!(check_binding(Some(&format!("{state}.")), &state), None);
        assert_eq!(
            check_binding(Some(&format!("{state}.swsi_x")), &state),
            None
        );
        assert_eq!(
            check_binding(Some(&format!("{state}.{}x", inv.value)), &state),
            None
        );
        assert_eq!(
            check_binding(Some(&format!("{state}.{}.x", inv.value)), &state),
            None
        );
        assert_eq!(check_binding(Some("<script>"), "<script>"), None);
    }
}
