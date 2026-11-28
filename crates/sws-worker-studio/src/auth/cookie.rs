//! Cookie parsing and serialising for the two Studio auth cookies (pure).
//!
//! Both use the `__Host-` prefix, so a browser only accepts them with `Secure`,
//! `Path=/` and no `Domain` (a sibling subdomain cannot plant or shadow them).
//! Both are `HttpOnly` and `SameSite=Lax`: Lax lets the GitHub callback (a
//! top-level navigation) carry the OAuth cookie, while cross-site subresource
//! requests and cross-site POSTs carry neither. State-changing cookie requests
//! additionally need a matching `Origin` (see `policy::csrf_ok`).

/// Browser session (`<id>.<secret>`).
pub const SESSION_COOKIE: &str = "__Host-sws_session";
/// Binds an OAuth `state` to the browser that started the flow (login CSRF).
pub const OAUTH_COOKIE: &str = "__Host-sws_oauth";
/// A `Cookie` header longer than this is ignored entirely.
pub const MAX_COOKIE_HEADER: usize = 8 * 1024;

/// The value of cookie `name` in a `Cookie` request header. Fails closed
/// (`None`) on an oversized header or when the name appears more than once
/// (an ambiguous, possibly planted pair is never guessed between). Names match
/// exactly (case-sensitive), values are returned untrimmed of inner content and
/// must be validated by the caller.
pub fn get<'a>(header: Option<&'a str>, name: &str) -> Option<&'a str> {
    let header = header?;
    if header.len() > MAX_COOKIE_HEADER {
        return None;
    }
    let mut found = None;
    for pair in header.split(';') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k.trim() == name {
            if found.is_some() {
                return None;
            }
            found = Some(v.trim().trim_matches('"'));
        }
    }
    found.filter(|v| !v.is_empty())
}

/// `Set-Cookie` value for a host-only auth cookie. `value` is one of our own
/// credential formats (hex, `.` and `_` only), never user input.
pub fn set(name: &str, value: &str, max_age_secs: i64) -> String {
    format!(
        "{name}={value}; Path=/; Max-Age={}; HttpOnly; Secure; SameSite=Lax",
        max_age_secs.max(0)
    )
}

/// `Set-Cookie` value that deletes the cookie.
pub fn clear(name: &str) -> String {
    set(name, "", 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_host_only_hardened_cookies() {
        let c = set(SESSION_COOKIE, "abc.def", 1_209_600);
        assert_eq!(
            c,
            "__Host-sws_session=abc.def; Path=/; Max-Age=1209600; HttpOnly; Secure; SameSite=Lax"
        );
        assert!(!c.to_ascii_lowercase().contains("domain="));
        assert_eq!(
            clear(OAUTH_COOKIE),
            "__Host-sws_oauth=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax"
        );
        assert!(set(SESSION_COOKIE, "x", -5).contains("Max-Age=0"));
    }

    #[test]
    fn parses_the_named_cookie_exactly() {
        let h = Some("a=1; __Host-sws_session=id.secret; b=2");
        assert_eq!(get(h, SESSION_COOKIE), Some("id.secret"));
        assert_eq!(get(Some("__Host-sws_session=v"), SESSION_COOKIE), Some("v"));
        assert_eq!(
            get(Some("  __Host-sws_session = v  ;x=y"), SESSION_COOKIE),
            Some("v")
        );
        assert_eq!(
            get(Some("__Host-sws_session=\"v\""), SESSION_COOKIE),
            Some("v")
        );
        assert_eq!(get(None, SESSION_COOKIE), None);
        assert_eq!(get(Some(""), SESSION_COOKIE), None);
    }

    #[test]
    fn hostile_cookie_headers_fail_closed() {
        // Duplicates (cookie tossing / shadowing): refuse rather than guess.
        assert_eq!(
            get(
                Some("__Host-sws_session=a; __Host-sws_session=b"),
                SESSION_COOKIE
            ),
            None
        );
        // Prefix look-alikes and case variants are different cookies.
        assert_eq!(get(Some("sws_session=a"), SESSION_COOKIE), None);
        assert_eq!(get(Some("__host-sws_session=a"), SESSION_COOKIE), None);
        assert_eq!(get(Some("x__Host-sws_session=a"), SESSION_COOKIE), None);
        assert_eq!(get(Some("__Host-sws_session_x=a"), SESSION_COOKIE), None);
        // Empty value, no '=', oversized header.
        assert_eq!(get(Some("__Host-sws_session="), SESSION_COOKIE), None);
        assert_eq!(get(Some("__Host-sws_session"), SESSION_COOKIE), None);
        let big = format!(
            "__Host-sws_session=v; pad={}",
            "x".repeat(MAX_COOKIE_HEADER)
        );
        assert_eq!(get(Some(&big), SESSION_COOKIE), None);
        // A value with '=' inside is returned whole (and then fails format validation).
        assert_eq!(
            get(Some("__Host-sws_session=a=b"), SESSION_COOKIE),
            Some("a=b")
        );
    }
}
