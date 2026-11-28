//! Canonical security headers for Solo Website System (SWS).
//!
//! [`SECURITY_HEADERS`] (the `site` profile) MUST stay byte-identical to the `/*`
//! block in `core/public/_headers`.
//!
//! # Named profiles (#180)
//!
//! Each surface gets exactly one [`HeaderProfile`]. Every profile keeps HSTS
//! preload, `nosniff`, the restrictive `Permissions-Policy` and COOP
//! `same-origin`. None carries `Cache-Control` (set at the application point) or
//! `Set-Cookie`.
//!
//! * `site` ([`SECURITY_HEADERS`]): the published site, static (`_headers`) and
//!   Worker (`/api/*`). Unchanged.
//! * `studio` ([`STUDIO_HEADERS`]): the Studio Worker (JSON API, attachment
//!   downloads, future Builder shell). Differs from `site` only in the CSP,
//!   `default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action
//!   'self'` (its JSON and plain HTML message pages load no script, style or
//!   Turnstile; the invitation form uses `studio-form`), and adds
//!   `Cross-Origin-Resource-Policy: same-origin` so other origins cannot embed
//!   its responses as subresources.
//! * `studio-form` ([`STUDIO_FORM_HEADERS`]): the one Studio HTML page that
//!   embeds Turnstile, the public invitation form (`GET /invite`). The `studio`
//!   set with the CSP widened by exactly what that page needs:
//!   `script-src`, `frame-src` and `connect-src https://challenges.cloudflare.com`
//!   for the widget, and `form-action 'self' https://github.com` because a valid
//!   submission answers with a 303 to GitHub's authorize URL and browsers apply
//!   `form-action` to redirects after a form submission. No `style-src` or
//!   `img-src` (the page has no stylesheet, inline style or image) and no
//!   `'unsafe-inline'`.
//! * `preview` ([`PREVIEW_HEADERS`]): draft site HTML on a separate origin
//!   (#67). Site-like CSP without Turnstile or Stripe, `connect-src 'none'`,
//!   `frame-src 'none'`, `form-action 'none'` (a draft never submits), framing
//!   allowed only by the Studio origin. No `X-Frame-Options` (it cannot express
//!   an allowlist; CSP `frame-ancestors` is the control). `Referrer-Policy:
//!   no-referrer` and `X-Robots-Tag: noindex, nofollow` keep draft origins out
//!   of referrers and indexes.
//! * `canvas` ([`CANVAS_HEADERS`]): the Builder editing canvas iframe (#72). Like
//!   `preview`, but `default-src 'none'` with an explicit allowlist (scripts,
//!   styles, fonts and images from `'self'`, images also `data:`) and
//!   `base-uri 'none'`, since it is a Solo-controlled shell, not arbitrary
//!   compiled output.
//!
//! `preview` and `canvas` carry `frame-ancestors 'none'` in their consts (fail
//! closed). [`HeaderProfile::resolve`] replaces that source with the configured
//! Studio origin only after [`frame_origin`] validates it (scheme, host and
//! optional port; `http` only for loopback); anything else stays `'none'`.

/// Security headers applied to every response: static assets via `_headers`,
/// Worker responses via [`with_security_headers`]. This is the `site` profile.
pub const SECURITY_HEADERS: &[(&str, &str)] = &[
    (
        "Strict-Transport-Security",
        "max-age=63072000; includeSubDomains; preload",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "strict-origin-when-cross-origin"),
    (
        "Permissions-Policy",
        "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=(), browsing-topics=()",
    ),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    (
        "Content-Security-Policy",
        "default-src 'self'; script-src 'self' https://challenges.cloudflare.com; frame-src https://challenges.cloudflare.com; connect-src 'self' https://challenges.cloudflare.com; img-src 'self' data:; style-src 'self'; font-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self' https://checkout.stripe.com; frame-ancestors 'none'; upgrade-insecure-requests",
    ),
];

/// The `studio` profile: Studio Worker responses. Golden copy:
/// `crates/sws-security/profiles/studio.headers`.
pub const STUDIO_HEADERS: &[(&str, &str)] = &[
    (
        "Strict-Transport-Security",
        "max-age=63072000; includeSubDomains; preload",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "strict-origin-when-cross-origin"),
    (
        "Permissions-Policy",
        "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=(), browsing-topics=()",
    ),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
    (
        "Content-Security-Policy",
        "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    ),
];

/// The `studio-form` profile: the Turnstile invitation form (`GET /invite`,
/// #65) only. Golden copy: `crates/sws-security/profiles/studio-form.headers`.
pub const STUDIO_FORM_HEADERS: &[(&str, &str)] = &[
    (
        "Strict-Transport-Security",
        "max-age=63072000; includeSubDomains; preload",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "strict-origin-when-cross-origin"),
    (
        "Permissions-Policy",
        "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=(), browsing-topics=()",
    ),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
    (
        "Content-Security-Policy",
        "default-src 'none'; script-src https://challenges.cloudflare.com; frame-src https://challenges.cloudflare.com; connect-src https://challenges.cloudflare.com; frame-ancestors 'none'; base-uri 'none'; form-action 'self' https://github.com",
    ),
];

/// The `preview` profile: draft site HTML on its own origin (#67). Use
/// [`HeaderProfile::resolve`] to allow framing by the Studio origin. Golden
/// copy: `crates/sws-security/profiles/preview.headers`.
pub const PREVIEW_HEADERS: &[(&str, &str)] = &[
    (
        "Strict-Transport-Security",
        "max-age=63072000; includeSubDomains; preload",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    (
        "Permissions-Policy",
        "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=(), browsing-topics=()",
    ),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("X-Robots-Tag", "noindex, nofollow"),
    (
        "Content-Security-Policy",
        "default-src 'self'; script-src 'self'; frame-src 'none'; connect-src 'none'; img-src 'self' data:; style-src 'self'; font-src 'self'; object-src 'none'; base-uri 'self'; form-action 'none'; frame-ancestors 'none'; upgrade-insecure-requests",
    ),
];

/// The `canvas` profile: the Builder editing canvas iframe (#72). Use
/// [`HeaderProfile::resolve`] to allow framing by the Studio origin. Golden
/// copy: `crates/sws-security/profiles/canvas.headers`.
pub const CANVAS_HEADERS: &[(&str, &str)] = &[
    (
        "Strict-Transport-Security",
        "max-age=63072000; includeSubDomains; preload",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    (
        "Permissions-Policy",
        "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=(), browsing-topics=()",
    ),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("X-Robots-Tag", "noindex, nofollow"),
    (
        "Content-Security-Policy",
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ),
];

/// Cache policy applied to Worker API responses (never forced onto static asset passthroughs).
pub const API_CACHE_CONTROL: &str = "no-store";

/// The fail-closed framing directive in the `preview` and `canvas` CSPs, which
/// [`HeaderProfile::resolve`] replaces when a valid Studio origin is configured.
pub const FRAME_ANCESTORS_NONE: &str = "frame-ancestors 'none'";

/// A named security-header set for one kind of surface. See the module docs for
/// what each profile contains and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderProfile {
    Site,
    Studio,
    StudioForm,
    Preview,
    Canvas,
}

impl HeaderProfile {
    pub const ALL: [HeaderProfile; 5] = [
        Self::Site,
        Self::Studio,
        Self::StudioForm,
        Self::Preview,
        Self::Canvas,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Site => "site",
            Self::Studio => "studio",
            Self::StudioForm => "studio-form",
            Self::Preview => "preview",
            Self::Canvas => "canvas",
        }
    }

    /// The profile's static header set. For `preview` and `canvas` this is the
    /// fail-closed form (`frame-ancestors 'none'`).
    pub fn headers(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Site => SECURITY_HEADERS,
            Self::Studio => STUDIO_HEADERS,
            Self::StudioForm => STUDIO_FORM_HEADERS,
            Self::Preview => PREVIEW_HEADERS,
            Self::Canvas => CANVAS_HEADERS,
        }
    }

    /// The header set to emit. `preview` and `canvas` allow framing by
    /// `studio_origin` when [`frame_origin`] accepts it and stay
    /// `frame-ancestors 'none'` otherwise; `site`, `studio` and `studio-form`
    /// ignore it.
    pub fn resolve(self, studio_origin: Option<&str>) -> Vec<(String, String)> {
        let ancestor = match self {
            Self::Preview | Self::Canvas => studio_origin.and_then(frame_origin),
            Self::Site | Self::Studio | Self::StudioForm => None,
        };
        self.headers()
            .iter()
            .map(|(name, value)| {
                let value = match &ancestor {
                    Some(origin) if *name == "Content-Security-Policy" => value.replacen(
                        FRAME_ANCESTORS_NONE,
                        &format!("frame-ancestors {origin}"),
                        1,
                    ),
                    _ => value.to_string(),
                };
                (name.to_string(), value)
            })
            .collect()
    }
}

/// Validate and normalise a configured Studio origin for use as a CSP
/// `frame-ancestors` source. Accepts only `scheme://host[:port]`, where the
/// scheme is `https` (or `http` for `localhost` / `127.0.0.1`), the host is
/// dotted ASCII labels of letters, digits and inner hyphens, and the port is
/// 1-65535. Anything else (paths, a trailing `/`, wildcards, userinfo, quotes,
/// `;`, `,`, whitespace, IPv6 literals, other schemes) is rejected with `None`.
/// The result is lowercase with a default port removed.
pub fn frame_origin(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let (scheme, rest) = if let Some(r) = lower.strip_prefix("https://") {
        ("https", r)
    } else {
        ("http", lower.strip_prefix("http://")?)
    };
    if !rest
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
    {
        return None;
    }
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if host.is_empty() || host.len() > 253 || !host.split('.').all(label_ok) {
        return None;
    }
    if scheme == "http" && !matches!(host, "localhost" | "127.0.0.1") {
        return None;
    }
    let port = match port {
        None => None,
        Some(p) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let n: u16 = p.parse().ok()?;
            let default = if scheme == "https" { 443 } else { 80 };
            match n {
                0 => return None,
                n if n == default => None,
                n => Some(n),
            }
        }
    };
    Some(match port {
        Some(n) => format!("{scheme}://{host}:{n}"),
        None => format!("{scheme}://{host}"),
    })
}

/// Merge a profile into a response's existing headers: every existing entry
/// whose name the profile does not set is kept as-is (repeated `Set-Cookie`
/// entries stay separate), each profile header replaces any existing value of
/// that name and appears exactly once.
pub fn merge_headers<K: AsRef<str>, V: AsRef<str>>(
    existing: impl IntoIterator<Item = (String, String)>,
    profile: &[(K, V)],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = existing
        .into_iter()
        .filter(|(name, _)| {
            !profile
                .iter()
                .any(|(p, _)| p.as_ref().eq_ignore_ascii_case(name))
        })
        .collect();
    out.extend(
        profile
            .iter()
            .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string())),
    );
    out
}

/// A response's header list with one entry per `Set-Cookie`, independent of how
/// the runtime iterates them: `Set-Cookie` entries from `entries` (separate or
/// comma-folded) are dropped and `set_cookies` (from `getAll("Set-Cookie")`)
/// are appended one by one. Comma-folding `Set-Cookie` breaks cookies whose
/// attributes contain commas (`Expires`), so it is never trusted.
pub fn response_header_list(
    entries: impl IntoIterator<Item = (String, String)>,
    set_cookies: Vec<String>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = entries
        .into_iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case("set-cookie"))
        .collect();
    out.extend(
        set_cookies
            .into_iter()
            .map(|c| ("set-cookie".to_string(), c)),
    );
    out
}

/// Apply [`SECURITY_HEADERS`] (the `site` profile) to a Worker response. Headers
/// are copied into a fresh, mutable `Headers` object so this also works on
/// responses whose headers are immutable (e.g. `ASSETS` passthroughs).
///
/// `Cache-Control: no-store` is intentionally NOT added here; API JSON/HEAD
/// responses set [`API_CACHE_CONTROL`] directly so static assets keep their own
/// caching headers.
#[cfg(target_arch = "wasm32")]
pub fn with_security_headers(resp: worker::Response) -> worker::Result<worker::Response> {
    apply_headers(resp, SECURITY_HEADERS)
}

/// Apply `profile` (resolved against `studio_origin`, see
/// [`HeaderProfile::resolve`]) to a Worker response, like
/// [`with_security_headers`]. Never adds `Cache-Control`.
#[cfg(target_arch = "wasm32")]
pub fn with_profile_headers(
    resp: worker::Response,
    profile: HeaderProfile,
    studio_origin: Option<&str>,
) -> worker::Result<worker::Response> {
    apply_headers(resp, &profile.resolve(studio_origin))
}

#[cfg(target_arch = "wasm32")]
fn apply_headers<K: AsRef<str>, V: AsRef<str>>(
    resp: worker::Response,
    profile: &[(K, V)],
) -> worker::Result<worker::Response> {
    let current = resp.headers();
    let existing = response_header_list(current.entries(), current.get_all("Set-Cookie")?);
    let headers = worker::Headers::new();
    for (name, value) in merge_headers(existing, profile) {
        headers.append(&name, &value)?;
    }
    Ok(resp.with_headers(headers))
}

/// Parse the header lines under a given path pattern in a Cloudflare
/// `_headers` file. Returns `(name, value)` pairs in file order.
pub fn parse_headers_block(src: &str, pattern: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            in_block = trimmed == pattern;
            continue;
        }
        if in_block && let Some((name, value)) = trimmed.split_once(':') {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

/// Render a Cloudflare `_headers` file containing the `/*` security headers
/// block plus `/styles.css` and `/js/*` caching rules matching
/// `core/public/_headers`.
pub fn render_headers_file() -> String {
    let mut out = String::from(
        "# Cloudflare Workers Static Assets headers.\n\
         # The `/*` block MUST match SECURITY_HEADERS exactly.\n\
         /*\n",
    );
    for (name, value) in SECURITY_HEADERS {
        out.push_str(&format!("  {name}: {value}\n"));
    }
    out.push_str(
        "\n/styles.css\n  Cache-Control: public, max-age=3600, must-revalidate\n\n/js/*\n  Cache-Control: public, max-age=3600, must-revalidate\n",
    );
    out
}

/// Render a profile's static header set as a `_headers`-format `/*` block (the
/// shape of the golden files in `crates/sws-security/profiles/`).
pub fn render_profile_block(profile: HeaderProfile) -> String {
    let mut out = String::from("/*\n");
    for (name, value) in profile.headers() {
        out.push_str(&format!("  {name}: {value}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn load_workspace_headers_file() -> Option<String> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if workspace_root.join("Cargo.toml").is_file() {
            let headers_path = workspace_root.join("core/public/_headers");
            Some(
                std::fs::read_to_string(&headers_path).unwrap_or_else(|e| {
                    panic!("failed to read {}: {e}", headers_path.display());
                }),
            )
        } else {
            None
        }
    }

    #[test]
    fn headers_file_root_block_matches_const_exactly() {
        let sources = [
            ("rendered _headers", Some(render_headers_file())),
            ("core/public/_headers", load_workspace_headers_file()),
        ];
        for (label, maybe_src) in sources {
            let Some(src) = maybe_src else {
                continue;
            };
            let parsed = parse_headers_block(&src, "/*");
            assert_eq!(
                parsed.len(),
                SECURITY_HEADERS.len(),
                "`/*` block in {label} must contain exactly the contract headers"
            );
            for (name, value) in SECURITY_HEADERS {
                let found: Vec<_> = parsed
                    .iter()
                    .filter(|(n, _)| n.eq_ignore_ascii_case(name))
                    .collect();
                assert_eq!(
                    found.len(),
                    1,
                    "header {name} must appear exactly once in {label}"
                );
                assert_eq!(found[0].1, *value, "value mismatch for {name} in {label}");
            }
        }
    }

    #[test]
    fn render_headers_file_parses_identically() {
        let rendered = render_headers_file();
        assert_eq!(
            parse_headers_block(&rendered, "/*").len(),
            SECURITY_HEADERS.len()
        );
        assert_eq!(parse_headers_block(&rendered, "/styles.css").len(), 1);
        assert_eq!(parse_headers_block(&rendered, "/js/*").len(), 1);

        if let Some(headers_file) = load_workspace_headers_file() {
            for pattern in ["/*", "/styles.css", "/js/*"] {
                assert_eq!(
                    parse_headers_block(&rendered, pattern),
                    parse_headers_block(&headers_file, pattern),
                    "rendered _headers block mismatch for {pattern}"
                );
            }
        }
    }

    #[test]
    fn no_duplicate_names_in_const() {
        for (i, (a, _)) in SECURITY_HEADERS.iter().enumerate() {
            for (b, _) in &SECURITY_HEADERS[i + 1..] {
                assert!(!a.eq_ignore_ascii_case(b), "duplicate header {a}");
            }
        }
    }

    #[test]
    fn parser_ignores_other_blocks_and_comments() {
        let src = "# c\n/*\n  A: 1\n  # x\n/x.css\n  A: 2\n  B: 3\n";
        assert_eq!(
            parse_headers_block(src, "/*"),
            vec![("A".to_string(), "1".to_string())]
        );
    }

    /// Pre-#180 `SECURITY_HEADERS`, copied verbatim. The `site` profile must
    /// stay byte-identical to this list (names, values and order).
    const SITE_GOLDEN: &[(&str, &str)] = &[
        (
            "Strict-Transport-Security",
            "max-age=63072000; includeSubDomains; preload",
        ),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "strict-origin-when-cross-origin"),
        (
            "Permissions-Policy",
            "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=(), browsing-topics=()",
        ),
        ("Cross-Origin-Opener-Policy", "same-origin"),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self' https://challenges.cloudflare.com; frame-src https://challenges.cloudflare.com; connect-src 'self' https://challenges.cloudflare.com; img-src 'self' data:; style-src 'self'; font-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self' https://checkout.stripe.com; frame-ancestors 'none'; upgrade-insecure-requests",
        ),
    ];

    fn owned(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn golden_file(profile: HeaderProfile) -> Option<&'static str> {
        match profile {
            HeaderProfile::Site => None,
            HeaderProfile::Studio => Some(include_str!("../profiles/studio.headers")),
            HeaderProfile::StudioForm => Some(include_str!("../profiles/studio-form.headers")),
            HeaderProfile::Preview => Some(include_str!("../profiles/preview.headers")),
            HeaderProfile::Canvas => Some(include_str!("../profiles/canvas.headers")),
        }
    }

    fn csp(list: &[(String, String)]) -> String {
        list.iter()
            .find(|(k, _)| k == "Content-Security-Policy")
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    #[test]
    fn site_profile_is_byte_identical_to_the_pre_profile_set() {
        assert_eq!(SECURITY_HEADERS, SITE_GOLDEN);
        assert_eq!(HeaderProfile::Site.headers(), SITE_GOLDEN);
        assert_eq!(HeaderProfile::Site.resolve(None), owned(SITE_GOLDEN));
        // A configured origin never changes the site set.
        assert_eq!(
            HeaderProfile::Site.resolve(Some("https://studio.example")),
            owned(SITE_GOLDEN)
        );
    }

    #[test]
    fn every_profile_has_a_unique_name_and_no_duplicate_headers() {
        let names: Vec<_> = HeaderProfile::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(
            names,
            ["site", "studio", "studio-form", "preview", "canvas"]
        );
        for p in HeaderProfile::ALL {
            let h = p.headers();
            for (i, (a, _)) in h.iter().enumerate() {
                for (b, _) in &h[i + 1..] {
                    assert!(!a.eq_ignore_ascii_case(b), "duplicate {a} in {}", p.name());
                }
            }
            assert!(
                !h.iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case("cache-control")
                        || n.eq_ignore_ascii_case("set-cookie")),
                "{} must not carry Cache-Control or Set-Cookie",
                p.name()
            );
        }
    }

    #[test]
    fn non_site_profiles_match_their_golden_files_exactly_both_ways() {
        for p in HeaderProfile::ALL {
            let Some(golden_src) = golden_file(p) else {
                continue;
            };
            let golden = parse_headers_block(golden_src, "/*");
            // const -> render -> parse == golden
            let rendered = parse_headers_block(&render_profile_block(p), "/*");
            assert_eq!(rendered, golden, "rendered {} != golden", p.name());
            // golden -> parse == const (same names, values and order)
            assert_eq!(golden, owned(p.headers()), "golden {} != const", p.name());
            let mut names: Vec<String> = golden.iter().map(|(n, _)| n.to_lowercase()).collect();
            names.sort();
            names.dedup();
            assert_eq!(
                names.len(),
                golden.len(),
                "duplicate in {}.headers",
                p.name()
            );
        }
    }

    #[test]
    fn studio_profile_contents() {
        let h = owned(HeaderProfile::Studio.headers());
        for (k, v) in SECURITY_HEADERS {
            if *k != "Content-Security-Policy" {
                assert!(
                    h.contains(&(k.to_string(), v.to_string())),
                    "studio keeps {k}"
                );
            }
        }
        assert!(h.contains(&("Cross-Origin-Resource-Policy".into(), "same-origin".into())));
        assert_eq!(
            csp(&h),
            "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
        );
        assert!(!csp(&h).contains("challenges.cloudflare.com"));
        assert_eq!(
            HeaderProfile::Studio.resolve(Some("https://studio.example")),
            h
        );
    }

    #[test]
    fn studio_form_profile_is_studio_plus_only_the_turnstile_and_oauth_allowances() {
        let form = owned(HeaderProfile::StudioForm.headers());
        let studio = owned(HeaderProfile::Studio.headers());
        // Same names, order and values as `studio`, except the CSP.
        assert_eq!(form.len(), studio.len());
        for ((fk, fv), (sk, sv)) in form.iter().zip(&studio) {
            assert_eq!(fk, sk);
            if fk != "Content-Security-Policy" {
                assert_eq!(fv, sv, "{fk}");
            }
        }
        assert_eq!(
            csp(&form),
            "default-src 'none'; script-src https://challenges.cloudflare.com; frame-src https://challenges.cloudflare.com; connect-src https://challenges.cloudflare.com; frame-ancestors 'none'; base-uri 'none'; form-action 'self' https://github.com"
        );
        // Every studio directive except form-action is kept verbatim.
        let directives =
            |c: &str| -> Vec<String> { c.split(';').map(|d| d.trim().to_string()).collect() };
        let form_dirs = directives(&csp(&form));
        for d in directives(&csp(&studio)) {
            if !d.starts_with("form-action") {
                assert!(form_dirs.contains(&d), "studio-form keeps {d}");
            }
        }
        let c = csp(&form);
        for banned in [
            "'unsafe-inline'",
            "'unsafe-eval'",
            "style-src",
            "img-src",
            "*",
            "data:",
            "'self' https://challenges",
        ] {
            assert!(
                !c.contains(banned),
                "studio-form CSP must not contain {banned}"
            );
        }
        assert_eq!(c.matches("frame-ancestors").count(), 1);
        assert_eq!(
            HeaderProfile::StudioForm.resolve(Some("https://studio.example")),
            form
        );
    }

    #[test]
    fn preview_and_canvas_are_frameable_only_by_a_validated_studio_origin() {
        for p in [HeaderProfile::Preview, HeaderProfile::Canvas] {
            let raw = owned(p.headers());
            assert!(
                !raw.iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("x-frame-options")),
                "{}: XFO cannot express an allowlist",
                p.name()
            );
            assert!(raw.contains(&("X-Robots-Tag".into(), "noindex, nofollow".into())));
            let c = csp(&raw);
            assert_eq!(c.matches(FRAME_ANCESTORS_NONE).count(), 1, "{}", p.name());
            assert_eq!(c.matches("frame-ancestors").count(), 1, "{}", p.name());
            assert!(c.contains("connect-src 'none'"), "{}", p.name());
            assert!(c.contains("script-src 'self';"), "{}", p.name());
            assert!(!c.contains("challenges.cloudflare.com"), "{}", p.name());
            assert!(!c.contains("stripe"), "{}", p.name());
            // No origin, or an invalid one: the raw, fail-closed set.
            assert_eq!(p.resolve(None), raw);
            assert_eq!(p.resolve(Some("https://*.example")), raw);
            // A valid origin replaces exactly the frame-ancestors source.
            let r = p.resolve(Some("https://Studio.Example:443"));
            assert_eq!(r.len(), raw.len());
            assert_eq!(
                csp(&r),
                c.replace(
                    FRAME_ANCESTORS_NONE,
                    "frame-ancestors https://studio.example"
                )
            );
            for ((rk, rv), (k, v)) in r.iter().zip(&raw) {
                assert_eq!(rk, k);
                if k != "Content-Security-Policy" {
                    assert_eq!(rv, v);
                }
            }
        }
    }

    #[test]
    fn frame_origin_accepts_and_normalises_plain_origins() {
        for (input, want) in [
            ("https://studio.example", "https://studio.example"),
            ("HTTPS://Studio.EXAMPLE", "https://studio.example"),
            ("https://studio.example:443", "https://studio.example"),
            ("https://studio.example:8443", "https://studio.example:8443"),
            ("https://a-b.c1.example", "https://a-b.c1.example"),
            ("http://127.0.0.1:8788", "http://127.0.0.1:8788"),
            ("http://localhost:80", "http://localhost"),
            ("http://localhost:8790", "http://localhost:8790"),
        ] {
            assert_eq!(frame_origin(input).as_deref(), Some(want), "{input}");
        }
    }

    #[test]
    fn frame_origin_rejects_hostile_or_ambiguous_input() {
        for bad in [
            "",
            " ",
            "null",
            "'none'",
            "'self'",
            "*",
            "https://*",
            "https://*.example",
            "*.example",
            "studio.example",
            "//studio.example",
            "https://",
            "https:///",
            "https://studio.example/",
            "https://studio.example/path",
            "https://studio.example?x=1",
            "https://studio.example#x",
            "https://user@studio.example",
            "https://user:pw@studio.example",
            "https://studio.example:",
            "https://studio.example:0",
            "https://studio.example:65536",
            "https://studio.example:99999",
            "https://studio.example:443:443",
            "https://studio.example:+443",
            "https://studio.example: 443",
            " https://studio.example",
            "https://studio.example ",
            "https://studio.example\t",
            "https://studio.example\nfoo",
            "https://studio .example",
            "https://studio.example; script-src *",
            "https://studio.example;",
            "https://studio.example,https://evil.example",
            "https://studio.example https://evil.example",
            "https://studio.example'",
            "'https://studio.example'",
            "\"https://studio.example\"",
            "https://stu\"dio.example",
            "https://studio.example\\",
            "https://studio%2eexample",
            "https://studio..example",
            "https://.studio.example",
            "https://studio.example.",
            "https://-studio.example",
            "https://studio-.example",
            "https://[::1]",
            "https://[::1]:8443",
            "https://stüdio.example",
            "http://studio.example",
            "http://evil.localhost",
            "ftp://studio.example",
            "ws://studio.example",
            "wss://studio.example",
            "javascript://studio.example",
            "data:text/html,x",
            "blob:https://studio.example",
            "https:studio.example",
            "https:/studio.example",
        ] {
            assert_eq!(frame_origin(bad), None, "{bad:?}");
            for p in [HeaderProfile::Preview, HeaderProfile::Canvas] {
                assert_eq!(p.resolve(Some(bad)), owned(p.headers()), "{bad:?}");
            }
        }
        let long_label = format!("https://{}.example", "a".repeat(64));
        assert_eq!(frame_origin(&long_label), None);
        let long_host = format!("https://{}example", "a.".repeat(130));
        assert_eq!(frame_origin(&long_host), None);
    }

    #[test]
    fn two_set_cookie_headers_survive_every_profile_merge() {
        let existing = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Set-Cookie".to_string(), "a=1; Path=/; Secure".to_string()),
            (
                "Set-Cookie".to_string(),
                "b=2; Path=/; HttpOnly".to_string(),
            ),
            // A profile name already present is replaced, never duplicated.
            ("X-Frame-Options".to_string(), "SAMEORIGIN".to_string()),
        ];
        let cookies = |h: &[(String, String)]| -> Vec<String> {
            h.iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
                .map(|(_, v)| v.clone())
                .collect()
        };
        let site = merge_headers(existing.clone(), SECURITY_HEADERS);
        assert_eq!(
            cookies(&site),
            ["a=1; Path=/; Secure", "b=2; Path=/; HttpOnly"]
        );
        assert_eq!(
            site.iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("x-frame-options"))
                .collect::<Vec<_>>(),
            [&("X-Frame-Options".to_string(), "DENY".to_string())]
        );
        assert_eq!(site.len(), 3 + SECURITY_HEADERS.len());
        for p in HeaderProfile::ALL {
            let merged = merge_headers(existing.clone(), &p.resolve(None));
            assert_eq!(cookies(&merged), cookies(&site), "{}", p.name());
            for (k, v) in p.headers() {
                let found: Vec<_> = merged
                    .iter()
                    .filter(|(n, _)| n.eq_ignore_ascii_case(k))
                    .collect();
                assert_eq!(found.len(), 1, "{k} once in {}", p.name());
                assert_eq!(found[0].1, *v);
            }
        }
    }

    #[test]
    fn response_header_list_never_folds_set_cookie() {
        let cookies = vec!["a=1; Path=/".to_string(), "b=2; Path=/".to_string()];
        // Runtime yields each Set-Cookie separately (workerd today) ...
        let separate = vec![
            ("content-type".to_string(), "text/plain".to_string()),
            ("set-cookie".to_string(), "a=1; Path=/".to_string()),
            ("set-cookie".to_string(), "b=2; Path=/".to_string()),
        ];
        // ... or folds them into one comma-joined value (spec-violating).
        let folded = vec![
            ("content-type".to_string(), "text/plain".to_string()),
            (
                "set-cookie".to_string(),
                "a=1; Path=/, b=2; Path=/".to_string(),
            ),
        ];
        let want = vec![
            ("content-type".to_string(), "text/plain".to_string()),
            ("set-cookie".to_string(), "a=1; Path=/".to_string()),
            ("set-cookie".to_string(), "b=2; Path=/".to_string()),
        ];
        assert_eq!(response_header_list(separate, cookies.clone()), want);
        assert_eq!(response_header_list(folded, cookies), want);
        assert_eq!(
            response_header_list(vec![("x".into(), "1".into())], vec![]),
            vec![("x".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn security_headers_do_not_include_cache_control() {
        assert!(
            !SECURITY_HEADERS
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cache-control")),
            "SECURITY_HEADERS must not force Cache-Control onto static asset passthroughs"
        );
        assert_eq!(API_CACHE_CONTROL, "no-store");
    }
}
