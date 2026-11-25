//! Canonical security headers for Solo Website System (SWS).
//!
//! These strings MUST stay byte-identical to the `/*` block in
//! `core/public/_headers`.

/// Security headers applied to every response: static assets via `_headers`,
/// Worker responses via [`with_security_headers`].
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
        "default-src 'self'; script-src 'self' https://challenges.cloudflare.com; frame-src https://challenges.cloudflare.com; connect-src 'self' https://challenges.cloudflare.com; img-src 'self' data:; style-src 'self'; font-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'; upgrade-insecure-requests",
    ),
];

/// Cache policy applied to Worker API responses (never forced onto static asset passthroughs).
pub const API_CACHE_CONTROL: &str = "no-store";

/// Apply [`SECURITY_HEADERS`] to a Worker response. Headers are copied into a
/// fresh, mutable `Headers` object so this also works on responses whose
/// headers are immutable (e.g. `ASSETS` passthroughs).
///
/// `Cache-Control: no-store` is intentionally NOT added here; API JSON/HEAD
/// responses set [`API_CACHE_CONTROL`] directly so static assets keep their own
/// caching headers.
#[cfg(target_arch = "wasm32")]
pub fn with_security_headers(resp: worker::Response) -> worker::Result<worker::Response> {
    let headers = worker::Headers::new();
    for (name, value) in resp.headers().entries() {
        headers.append(&name, &value)?;
    }
    for (name, value) in SECURITY_HEADERS {
        headers.set(name, value)?;
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
