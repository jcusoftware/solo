//! Preview-host routing, tokens, the expiry decision and response headers
//! (pure, host-tested). The wasm glue (`preview::http`) only performs I/O.
//!
//! Preview host routes (nothing else exists there; every other path is 404):
//!
//! | Path | Answer |
//! | --- | --- |
//! | `/robots.txt` | `User-agent: *` / `Disallow: /` |
//! | `/p/{site}/{revision}/{token}` | `308` to the same path with a trailing `/` |
//! | `/p/{site}/{revision}/{token}/{path...}` | the file (empty or `/`-ending path: `index.html`) |
//!
//! Only `GET` and `HEAD` (others `405`). The request's `Cookie` and
//! `Authorization` headers are never read, no response sets a cookie, and every
//! response carries the `preview` profile resolved against the Studio origin
//! (framing by the Studio only), `Cache-Control: no-store` and
//! `X-Robots-Tag: noindex, nofollow` (part of the profile).
//!
//! Unknown token, a token presented with another site or revision, or a
//! malformed path: `404`. Expired, revoked, or evicted (the artifact was
//! removed by retention): `410`. Both carry the same generic plain-text body
//! shape and the full header set. `410` only tells someone who already holds a
//! 128-bit token that it once worked; it is not an oracle for guessing.

use sws_assets::hash::{sha256_hex, to_hex};
use sws_assets::validate::is_id;
use sws_security::headers::{API_CACHE_CONTROL, HeaderProfile};

/// Random bytes in a preview token (128 bits).
pub const TOKEN_BYTES: usize = 16;
/// How long a preview row outlives its expiry, so an expired link answers
/// `410` rather than `404` for a while. After this the row is swept and the
/// link is indistinguishable from one that never existed.
pub const ROW_RETENTION_AFTER_EXPIRY_SECS: u64 = 7 * 24 * 3600;
pub const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /\n";
pub const ALLOW: &str = "GET, HEAD";

/// A token: exactly 32 lowercase hex characters.
pub fn is_token(s: &str) -> bool {
    s.len() == TOKEN_BYTES * 2 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Token from caller-supplied random bytes (the Worker uses `crypto.getRandomValues`).
pub fn mint_token(random: &[u8; TOKEN_BYTES]) -> String {
    to_hex(random)
}

/// What D1 stores: the SHA-256 hex of the token string. The token is never stored.
pub fn token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// A parsed preview URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewUrl<'a> {
    pub site: &'a str,
    pub revision: &'a str,
    pub token: &'a str,
    /// The raw path after `{token}/` (may be empty).
    pub rest: &'a str,
}

impl PreviewUrl<'_> {
    /// `/p/{site}/{revision}/{token}/`.
    pub fn root(&self) -> String {
        format!("/p/{}/{}/{}/", self.site, self.revision, self.token)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewPath<'a> {
    Robots,
    File(PreviewUrl<'a>),
    /// `/p/{site}/{revision}/{token}` without the trailing slash.
    AddSlash(PreviewUrl<'a>),
    NotFound,
}

/// Route a preview-host path. Ids must be registry ids and the token a token,
/// otherwise `NotFound` (before any D1 read).
pub fn parse_path(path: &str) -> PreviewPath<'_> {
    if path == "/robots.txt" {
        return PreviewPath::Robots;
    }
    let Some(tail) = path.strip_prefix("/p/") else {
        return PreviewPath::NotFound;
    };
    let mut it = tail.splitn(4, '/');
    let (Some(site), Some(revision), Some(token)) = (it.next(), it.next(), it.next()) else {
        return PreviewPath::NotFound;
    };
    if !is_id(site) || !is_id(revision) || !is_token(token) {
        return PreviewPath::NotFound;
    }
    match it.next() {
        None => PreviewPath::AddSlash(PreviewUrl {
            site,
            revision,
            token,
            rest: "",
        }),
        Some(rest) => PreviewPath::File(PreviewUrl {
            site,
            revision,
            token,
            rest,
        }),
    }
}

/// The stored facts about a token, as read by one D1 query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub site_id: String,
    pub revision_id: String,
    pub expires_at: u64,
    pub revoked: bool,
    /// The generated artifact, when it still exists.
    pub artifact: Option<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub expires_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// `404`.
    Unknown,
    /// `410`.
    Gone,
    Live,
}

/// Decide access at `now` (unix seconds). Expired means `now >= expires_at`
/// (the SQL treats `expires_at > now` as live), using the earlier of the
/// preview's and the artifact's expiry.
pub fn access(row: Option<&Row>, site: &str, revision: &str, now: u64) -> Access {
    let Some(row) = row else {
        return Access::Unknown;
    };
    if row.site_id != site || row.revision_id != revision {
        return Access::Unknown;
    }
    let Some(artifact) = &row.artifact else {
        return Access::Gone;
    };
    if row.revoked || now >= row.expires_at.min(artifact.expires_at) {
        return Access::Gone;
    }
    Access::Live
}

/// A preview-host response: status, headers and an optional fixed body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewResp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<&'static str>,
}

impl PreviewResp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Every preview-host header: `Content-Type`, `Cache-Control: no-store`, any
/// `extra`, then the `preview` profile resolved against `studio_origin`.
pub fn headers(
    content_type: &str,
    studio_origin: Option<&str>,
    extra: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = vec![
        ("Content-Type".into(), content_type.into()),
        ("Cache-Control".into(), API_CACHE_CONTROL.into()),
    ];
    h.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    h.extend(HeaderProfile::Preview.resolve(studio_origin));
    h
}

const TEXT: &str = "text/plain; charset=utf-8";

fn text(
    status: u16,
    body: &'static str,
    studio: Option<&str>,
    extra: &[(&str, &str)],
) -> PreviewResp {
    PreviewResp {
        status,
        headers: headers(TEXT, studio, extra),
        body: Some(body),
    }
}

pub fn not_found(studio: Option<&str>) -> PreviewResp {
    text(404, "Not found\n", studio, &[])
}

pub fn gone(studio: Option<&str>) -> PreviewResp {
    text(
        410,
        "This preview link has expired or was revoked.\n",
        studio,
        &[],
    )
}

pub fn method_not_allowed(studio: Option<&str>) -> PreviewResp {
    text(405, "Method not allowed\n", studio, &[("Allow", ALLOW)])
}

pub fn uri_too_long(studio: Option<&str>) -> PreviewResp {
    text(414, "URI too long\n", studio, &[])
}

pub fn busy(studio: Option<&str>) -> PreviewResp {
    text(
        503,
        "Busy, retry shortly\n",
        studio,
        &[("Retry-After", "1")],
    )
}

pub fn internal_error(studio: Option<&str>) -> PreviewResp {
    text(500, "Preview unavailable\n", studio, &[])
}

pub fn robots(studio: Option<&str>) -> PreviewResp {
    text(200, ROBOTS_TXT, studio, &[])
}

/// `308` to `location` (a same-origin absolute path built from validated parts).
pub fn add_slash(location: &str, studio: Option<&str>) -> PreviewResp {
    PreviewResp {
        status: 308,
        headers: headers(TEXT, studio, &[("Location", location)]),
        body: None,
    }
}

/// Headers for a served file; the body is streamed by the glue.
pub fn file(content_type: &str, studio: Option<&str>) -> PreviewResp {
    PreviewResp {
        status: 200,
        headers: headers(content_type, studio, &[]),
        body: None,
    }
}

/// Studio-origin producer routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProducerRoute {
    /// `POST /api/sites/{ws}/{site}/previews?revision_id=..&revision_sha256=..`
    Create { ws: String, site: String },
    /// `DELETE /api/sites/{ws}/{site}/previews/{preview_id}`
    Revoke {
        ws: String,
        site: String,
        id: String,
    },
}

impl ProducerRoute {
    pub fn allow(&self) -> &'static str {
        match self {
            ProducerRoute::Create { .. } => "POST",
            ProducerRoute::Revoke { .. } => "DELETE",
        }
    }

    pub fn ids(&self) -> (&str, &str) {
        match self {
            ProducerRoute::Create { ws, site } | ProducerRoute::Revoke { ws, site, .. } => {
                (ws, site)
            }
        }
    }
}

/// Match `/api/sites/{ws}/{site}/previews[/{id}]` with registry ids.
pub fn parse_producer(path: &str) -> Option<ProducerRoute> {
    let rest = path.strip_prefix("/api/sites/")?;
    let mut it = rest.split('/');
    let (ws, site, kw) = (it.next()?, it.next()?, it.next()?);
    if kw != "previews" || !is_id(ws) || !is_id(site) {
        return None;
    }
    let (ws, site) = (ws.to_string(), site.to_string());
    match (it.next(), it.next()) {
        (None, _) => Some(ProducerRoute::Create { ws, site }),
        (Some(id), None) if is_id(id) => Some(ProducerRoute::Revoke {
            ws,
            site,
            id: id.to_string(),
        }),
        _ => None,
    }
}

/// `revision_id` and `revision_sha256` from a query string (each exactly once).
pub fn revision_pin(query: Option<&str>) -> Option<(String, String)> {
    let mut id = None;
    let mut sha = None;
    for pair in query?.split('&') {
        let (k, v) = pair.split_once('=')?;
        let slot = match k {
            "revision_id" => &mut id,
            "revision_sha256" => &mut sha,
            _ => return None,
        };
        if slot.replace(v.to_string()).is_some() {
            return None;
        }
    }
    let (id, sha) = (id?, sha?);
    (is_id(&id) && sws_assets::validate::is_sha256(&sha)).then_some((id, sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sws_security::headers::{FRAME_ANCESTORS_NONE, PREVIEW_HEADERS};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const STUDIO: &str = "https://studio.example.com";

    #[test]
    fn tokens_are_128_bit_lowercase_hex_and_stored_hashed() {
        let t = mint_token(&[0xab; TOKEN_BYTES]);
        assert_eq!(t, "ab".repeat(16));
        assert!(is_token(&t));
        for bad in [
            "",
            &"a".repeat(31),
            &"a".repeat(33),
            &"A".repeat(32),
            &"g".repeat(32),
            "0123456789abcdef0123456789abcde/",
        ] {
            assert!(!is_token(bad), "{bad}");
        }
        let h = token_hash(TOKEN);
        assert_eq!(h.len(), 64);
        assert_ne!(h, TOKEN);
        assert_eq!(h, token_hash(TOKEN));
    }

    #[test]
    fn preview_paths_parse_strictly() {
        assert_eq!(parse_path("/robots.txt"), PreviewPath::Robots);
        let u = |rest| PreviewUrl {
            site: "site-1",
            revision: "rev_1",
            token: TOKEN,
            rest,
        };
        assert_eq!(
            parse_path(&format!("/p/site-1/rev_1/{TOKEN}/")),
            PreviewPath::File(u(""))
        );
        assert_eq!(
            parse_path(&format!("/p/site-1/rev_1/{TOKEN}/a/b.css")),
            PreviewPath::File(u("a/b.css"))
        );
        assert_eq!(
            parse_path(&format!("/p/site-1/rev_1/{TOKEN}")),
            PreviewPath::AddSlash(u(""))
        );
        assert_eq!(u("").root(), format!("/p/site-1/rev_1/{TOKEN}/"));
        for bad in [
            "/",
            "/index.html",
            "/api/health",
            "/robots.txt/",
            "/p",
            "/p/",
            "/p/site-1",
            "/p/site-1/rev_1",
            "/p/site-1/rev_1/",
            &format!("/p/Site/rev_1/{TOKEN}/"),
            &format!("/p/site-1/rev.1/{TOKEN}/"),
            &format!("/p/site-1/rev_1/{}/", TOKEN.to_uppercase()),
            &format!("/p/site-1/rev_1/{TOKEN}0/"),
            &format!("/p/site-1/%2e%2e/{TOKEN}/"),
            &format!("/p//rev_1/{TOKEN}/"),
            &format!("/P/site-1/rev_1/{TOKEN}/"),
        ] {
            assert_eq!(parse_path(bad), PreviewPath::NotFound, "{bad}");
        }
    }

    fn row(expires_at: u64, revoked: bool, artifact: Option<u64>) -> Row {
        Row {
            site_id: "s".into(),
            revision_id: "r".into(),
            expires_at,
            revoked,
            artifact: artifact.map(|expires_at| Artifact { expires_at }),
        }
    }

    #[test]
    fn access_decision_and_expiry_boundary() {
        let live = row(1000, false, Some(1000));
        assert_eq!(access(None, "s", "r", 0), Access::Unknown);
        assert_eq!(access(Some(&live), "s", "r", 999), Access::Live);
        // The expiry instant itself is already expired.
        assert_eq!(access(Some(&live), "s", "r", 1000), Access::Gone);
        assert_eq!(access(Some(&live), "s", "r", 1001), Access::Gone);
        // A token presented with another site or revision is unknown, not gone.
        assert_eq!(access(Some(&live), "x", "r", 0), Access::Unknown);
        assert_eq!(access(Some(&live), "s", "x", 0), Access::Unknown);
        let expired_revoked = row(10, true, None);
        assert_eq!(
            access(Some(&expired_revoked), "x", "r", 999),
            Access::Unknown
        );
        // Revoked, evicted, or an earlier artifact expiry.
        assert_eq!(
            access(Some(&row(1000, true, Some(1000))), "s", "r", 0),
            Access::Gone
        );
        assert_eq!(
            access(Some(&row(1000, false, None)), "s", "r", 0),
            Access::Gone
        );
        let early = row(1000, false, Some(500));
        assert_eq!(access(Some(&early), "s", "r", 499), Access::Live);
        assert_eq!(access(Some(&early), "s", "r", 500), Access::Gone);
    }

    /// Exactly: Content-Type, no-store, extras, then the resolved `preview` profile.
    fn assert_preview_headers(r: &PreviewResp, extra: &[(&str, &str)]) {
        let mut want: Vec<(String, String)> = vec![
            (
                "Content-Type".into(),
                r.header("Content-Type").unwrap().into(),
            ),
            ("Cache-Control".into(), "no-store".into()),
        ];
        want.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        want.extend(HeaderProfile::Preview.resolve(Some(STUDIO)));
        assert_eq!(r.headers, want, "status {}", r.status);
        assert_eq!(r.header("X-Robots-Tag"), Some("noindex, nofollow"));
        assert_eq!(r.header("X-Frame-Options"), None);
        assert!(r.header("Set-Cookie").is_none());
        let csp = r.header("Content-Security-Policy").unwrap();
        assert!(csp.contains(&format!("frame-ancestors {STUDIO}")), "{csp}");
        assert!(!csp.contains(FRAME_ANCESTORS_NONE));
        for d in [
            "connect-src 'none'",
            "form-action 'none'",
            "frame-src 'none'",
        ] {
            assert!(csp.contains(d), "{d}");
        }
        assert!(!csp.contains("challenges.cloudflare.com"));
    }

    #[test]
    fn every_preview_response_carries_the_full_profile() {
        let s = Some(STUDIO);
        for (r, status, extra) in [
            (not_found(s), 404, vec![]),
            (gone(s), 410, vec![]),
            (robots(s), 200, vec![]),
            (internal_error(s), 500, vec![]),
            (uri_too_long(s), 414, vec![]),
            (method_not_allowed(s), 405, vec![("Allow", ALLOW)]),
            (busy(s), 503, vec![("Retry-After", "1")]),
            (
                add_slash("/p/s/r/t/", s),
                308,
                vec![("Location", "/p/s/r/t/")],
            ),
            (file("text/html; charset=utf-8", s), 200, vec![]),
        ] {
            assert_eq!(r.status, status);
            assert_preview_headers(&r, &extra);
        }
        assert_eq!(robots(s).body, Some("User-agent: *\nDisallow: /\n"));
        // The bodies of 404 and 410 never mention a site, revision or token.
        for r in [not_found(s), gone(s)] {
            assert!(!r.body.unwrap().contains(TOKEN));
        }
    }

    #[test]
    fn an_invalid_studio_origin_leaves_framing_closed() {
        for bad in [None, Some("not an origin"), Some("https://x.example/")] {
            let r = not_found(bad);
            let csp = r.header("Content-Security-Policy").unwrap();
            assert!(csp.contains(FRAME_ANCESTORS_NONE), "{bad:?}");
            assert_eq!(
                r.headers.len(),
                2 + PREVIEW_HEADERS.len(),
                "no other header is added"
            );
        }
    }

    #[test]
    fn producer_routes_parse_strictly() {
        assert_eq!(
            parse_producer("/api/sites/ws-a/site-1/previews"),
            Some(ProducerRoute::Create {
                ws: "ws-a".into(),
                site: "site-1".into()
            })
        );
        assert_eq!(
            parse_producer("/api/sites/ws-a/site-1/previews/p0123"),
            Some(ProducerRoute::Revoke {
                ws: "ws-a".into(),
                site: "site-1".into(),
                id: "p0123".into()
            })
        );
        for bad in [
            "/api/sites/ws-a/site-1/preview",
            "/api/sites/ws-a/site-1/previews/",
            "/api/sites/ws-a/site-1/previews/a/b",
            "/api/sites/WS/site-1/previews",
            "/api/sites/ws-a/si.te/previews",
            "/api/sites/ws-a/site-1/previews/P0",
            "/api/sites/ws-a/site-1/previews/..",
        ] {
            assert_eq!(parse_producer(bad), None, "{bad}");
        }
        let c = parse_producer("/api/sites/w/s/previews").unwrap();
        assert_eq!((c.allow(), c.ids()), ("POST", ("w", "s")));
    }

    #[test]
    fn revision_pin_needs_both_exactly_once() {
        let sha = "a".repeat(64);
        assert_eq!(
            revision_pin(Some(&format!("revision_id=r-1&revision_sha256={sha}"))),
            Some(("r-1".into(), sha.clone()))
        );
        for bad in [
            None,
            Some(""),
            Some("revision_id=r-1"),
            Some(&format!("revision_sha256={sha}")[..]),
            Some(&format!("revision_id=r-1&revision_id=r-2&revision_sha256={sha}")[..]),
            Some(&format!("revision_id=R&revision_sha256={sha}")[..]),
            Some("revision_id=r-1&revision_sha256=abc"),
            Some(&format!("revision_id=r-1&revision_sha256={sha}&x=1")[..]),
            Some(&format!("revision_id=r%2D1&revision_sha256={sha}")[..]),
        ] {
            assert_eq!(revision_pin(bad), None, "{bad:?}");
        }
    }
}
