//! Preview origin configuration and request dispatch (pure, host-tested).
//!
//! Drafts are served by the Studio Worker, but only on a SEPARATE host named by
//! `STUDIO_PREVIEW_ORIGIN`. A request whose host is the preview host reaches
//! only the preview routes; every other host is the Studio, where preview paths
//! are never served. Configuration fails closed: preview serving is disabled
//! (`503 preview_origin_not_configured`) unless both origins are valid
//! ([`frame_origin`]: `https://host[:port]`, or `http://` for `localhost` and
//! `127.0.0.1` only) and the preview HOST differs from the Studio host.
//!
//! The comparison is by host name, not origin: browsers scope cookies by host
//! and ignore the port, so `http://127.0.0.1:8788` and `http://127.0.0.1:8789`
//! would share cookies. A host that is a dot-suffix of the other (either way
//! round) is refused too, because a parent domain can set cookies its
//! subdomains receive. Neither check knows the Public Suffix List, so a
//! different REGISTRABLE domain (for example `studio.example.com` and
//! `example-preview.net`) is the recommended deployment: sibling subdomains of
//! one site are same-site to the browser (SameSite cookies flow between them).

use sws_security::headers::frame_origin;

/// Var naming the preview origin.
pub const PREVIEW_ORIGIN_VAR: &str = "STUDIO_PREVIEW_ORIGIN";
/// Var naming the Studio origin (also used for CSRF and framing).
pub const STUDIO_ORIGIN_VAR: &str = "STUDIO_PUBLIC_ORIGIN";

/// Validated origins. `studio` and `preview` are normalised by
/// [`frame_origin`] (lowercase, default port removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origins {
    pub studio: String,
    pub preview: String,
    preview_host: String,
    preview_port: u16,
}

impl Origins {
    /// The preview URL root for a token: `{preview}/p/{site}/{revision}/{token}/`.
    pub fn preview_url(&self, site: &str, revision: &str, token: &str) -> String {
        format!("{}/p/{site}/{revision}/{token}/", self.preview)
    }
}

/// Why preview serving is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disabled {
    /// `STUDIO_PREVIEW_ORIGIN` is unset or empty.
    Unset,
    /// It is not a plain origin `frame_origin` accepts.
    InvalidPreview,
    /// `STUDIO_PUBLIC_ORIGIN` is unset or not a plain origin.
    InvalidStudio,
    /// Same host as the Studio, or a dot-suffix either way round.
    SharesHost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Config {
    Enabled(Origins),
    Disabled(Disabled),
}

impl Config {
    pub fn origins(&self) -> Option<&Origins> {
        match self {
            Config::Enabled(o) => Some(o),
            Config::Disabled(_) => None,
        }
    }
}

/// Which surface a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Preview routes only, `preview` header profile, no credentials read.
    Preview,
    /// Everything else (the existing Studio routes).
    Studio,
}

/// `(host, effective port)` of an origin already normalised by [`frame_origin`].
fn split_origin(origin: &str) -> Option<(String, u16)> {
    let (scheme, rest) = origin.split_once("://")?;
    let default = if scheme == "https" { 443 } else { 80 };
    Some(match rest.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (rest.to_string(), default),
    })
}

/// Same host, or one a dot-suffix of the other.
fn shares_host(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

/// Build the configuration from the two vars.
pub fn configure(studio_public: Option<&str>, preview: Option<&str>) -> Config {
    let Some(raw_preview) = preview.map(str::trim).filter(|p| !p.is_empty()) else {
        return Config::Disabled(Disabled::Unset);
    };
    let Some(preview) = frame_origin(raw_preview) else {
        return Config::Disabled(Disabled::InvalidPreview);
    };
    let Some(studio) = studio_public.map(str::trim).and_then(frame_origin) else {
        return Config::Disabled(Disabled::InvalidStudio);
    };
    let (Some((studio_host, _)), Some((preview_host, preview_port))) =
        (split_origin(&studio), split_origin(&preview))
    else {
        return Config::Disabled(Disabled::InvalidPreview);
    };
    if shares_host(&studio_host, &preview_host) {
        return Config::Disabled(Disabled::SharesHost);
    }
    Config::Enabled(Origins {
        studio,
        preview,
        preview_host,
        preview_port,
    })
}

/// Decide the surface from the request URL's host and effective port (the
/// scheme default when the URL has none). Only an exact host and port match on
/// an enabled configuration is the preview surface.
pub fn surface(config: &Config, host: &str, port: Option<u16>) -> Surface {
    match config {
        Config::Enabled(o)
            if host.eq_ignore_ascii_case(&o.preview_host) && port == Some(o.preview_port) =>
        {
            Surface::Preview
        }
        _ => Surface::Studio,
    }
}

/// `/p` or anything under `/p/`: never served on the Studio surface.
pub fn is_preview_path(path: &str) -> bool {
    path == "/p" || path.starts_with("/p/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUDIO: &str = "https://studio.example.com";
    const PREVIEW: &str = "https://drafts.example-preview.net";

    fn enabled(studio: &str, preview: &str) -> Origins {
        match configure(Some(studio), Some(preview)) {
            Config::Enabled(o) => o,
            other => panic!("{studio} / {preview}: {other:?}"),
        }
    }

    #[test]
    fn a_separate_host_enables_previews_with_normalised_origins() {
        let o = enabled(STUDIO, "HTTPS://Drafts.Example-Preview.NET:443");
        assert_eq!(o.studio, STUDIO);
        assert_eq!(o.preview, PREVIEW);
        assert_eq!(
            o.preview_url("site-1", "rev-1", "0123456789abcdef0123456789abcdef"),
            "https://drafts.example-preview.net/p/site-1/rev-1/0123456789abcdef0123456789abcdef/"
        );
        // The local test layout: two loopback NAMES on one port.
        let l = enabled("http://127.0.0.1:8796", "http://localhost:8796");
        assert_eq!(l.preview, "http://localhost:8796");
    }

    #[test]
    fn misconfiguration_fails_closed() {
        let cases: &[(Option<&str>, Option<&str>, Disabled)] = &[
            (Some(STUDIO), None, Disabled::Unset),
            (Some(STUDIO), Some(""), Disabled::Unset),
            (Some(STUDIO), Some("   "), Disabled::Unset),
            (
                Some(STUDIO),
                Some("drafts.example-preview.net"),
                Disabled::InvalidPreview,
            ),
            (
                Some(STUDIO),
                Some("https://drafts.example-preview.net/"),
                Disabled::InvalidPreview,
            ),
            (
                Some(STUDIO),
                Some("https://drafts.example-preview.net/p"),
                Disabled::InvalidPreview,
            ),
            (
                Some(STUDIO),
                Some("http://drafts.example-preview.net"),
                Disabled::InvalidPreview,
            ),
            (
                Some(STUDIO),
                Some("https://*.example-preview.net"),
                Disabled::InvalidPreview,
            ),
            (
                Some(STUDIO),
                Some("https://u@drafts.example-preview.net"),
                Disabled::InvalidPreview,
            ),
            (
                Some(STUDIO),
                Some("https://[::1]:8789"),
                Disabled::InvalidPreview,
            ),
            (
                Some(STUDIO),
                Some("javascript:alert(1)"),
                Disabled::InvalidPreview,
            ),
            (None, Some(PREVIEW), Disabled::InvalidStudio),
            (
                Some("studio.example.com"),
                Some(PREVIEW),
                Disabled::InvalidStudio,
            ),
            // Same host: identical, different port, different scheme, case.
            (Some(STUDIO), Some(STUDIO), Disabled::SharesHost),
            (
                Some(STUDIO),
                Some("https://studio.example.com:8443"),
                Disabled::SharesHost,
            ),
            (
                Some(STUDIO),
                Some("https://STUDIO.example.com"),
                Disabled::SharesHost,
            ),
            (
                Some("http://127.0.0.1:8788"),
                Some("http://127.0.0.1:8789"),
                Disabled::SharesHost,
            ),
            // Dot-suffix either way round.
            (
                Some(STUDIO),
                Some("https://p.studio.example.com"),
                Disabled::SharesHost,
            ),
            (
                Some(STUDIO),
                Some("https://example.com"),
                Disabled::SharesHost,
            ),
        ];
        for (s, p, want) in cases {
            assert_eq!(
                configure(*s, *p),
                Config::Disabled(*want),
                "studio {s:?} preview {p:?}"
            );
        }
        // A shared suffix that is not a dot-suffix is allowed (different hosts).
        assert!(matches!(
            configure(Some(STUDIO), Some("https://xstudio.example.com")),
            Config::Enabled(_)
        ));
    }

    #[test]
    fn dispatch_matrix() {
        let on = Config::Enabled(enabled(STUDIO, PREVIEW));
        let local = Config::Enabled(enabled("http://127.0.0.1:8796", "http://localhost:8796"));
        let cases: &[(&Config, &str, Option<u16>, Surface)] = &[
            (
                &on,
                "drafts.example-preview.net",
                Some(443),
                Surface::Preview,
            ),
            (
                &on,
                "DRAFTS.example-preview.net",
                Some(443),
                Surface::Preview,
            ),
            (
                &on,
                "drafts.example-preview.net",
                Some(8443),
                Surface::Studio,
            ),
            (&on, "drafts.example-preview.net", None, Surface::Studio),
            (&on, "studio.example.com", Some(443), Surface::Studio),
            (
                &on,
                "x.drafts.example-preview.net",
                Some(443),
                Surface::Studio,
            ),
            (
                &on,
                "drafts.example-preview.net.evil.example",
                Some(443),
                Surface::Studio,
            ),
            (&on, "sws-studio.workers.dev", Some(443), Surface::Studio),
            (&on, "", Some(443), Surface::Studio),
            (&local, "localhost", Some(8796), Surface::Preview),
            (&local, "127.0.0.1", Some(8796), Surface::Studio),
            (&local, "localhost", Some(8797), Surface::Studio),
        ];
        for (cfg, host, port, want) in cases {
            assert_eq!(surface(cfg, host, *port), *want, "{host:?} {port:?}");
        }
        // Disabled: every host is the Studio, including the would-be preview host.
        for d in [
            configure(Some(STUDIO), None),
            configure(Some(STUDIO), Some(STUDIO)),
            configure(None, Some(PREVIEW)),
        ] {
            assert_eq!(
                surface(&d, "drafts.example-preview.net", Some(443)),
                Surface::Studio
            );
            assert_eq!(
                surface(&d, "studio.example.com", Some(443)),
                Surface::Studio
            );
            assert!(d.origins().is_none());
        }
    }

    #[test]
    fn preview_paths_are_recognised_exactly() {
        for p in ["/p", "/p/", "/p/s/r/t/", "/p/x"] {
            assert!(is_preview_path(p), "{p}");
        }
        for p in ["/", "/pp", "/p.html", "/api/p/", "/P/s", "/preview"] {
            assert!(!is_preview_path(p), "{p}");
        }
    }
}
