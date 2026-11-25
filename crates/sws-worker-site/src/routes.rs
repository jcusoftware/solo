//! Host-testable request routing and health payload for `sws-worker-site`.

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Health,
    HealthHead,
    FormSubmit(String),
    InquireCompat,
    Newsletter,
    /// `GET /api/events` — public events calendar listing (issue #43).
    Events,
    /// `GET /api/events/:id/ics` — single-event calendar download.
    EventIcs(String),
    /// Sandbox demo checkout: validates the cart, verifies Turnstile, then
    /// redirects (303) to a hosted Stripe Checkout session. No storage.
    Checkout,
    /// Known API path, wrong method. Carries the `Allow` header value.
    MethodNotAllowed(&'static str),
    /// Unknown `/api/*` path.
    NotFound,
    /// Anything outside `/api/` is delegated to the static-assets binding.
    Asset,
}

/// Validates a D1 event id for the ICS route: decimal digits only, no
/// overflow, so the bound `id = ?2` lookup cannot be probed with crafted text.
pub fn is_valid_event_id(id: &str) -> bool {
    id.len() <= 19
        && !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<i64>().is_ok()
}

/// Validates a form identifier against `[a-z0-9][a-z0-9_-]{0,63}`.
pub fn is_valid_form_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first_ok = matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9');
    let rest_ok = bytes[1..]
        .iter()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'));
    first_ok && rest_ok
}

fn is_non_public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let octets = ip.octets();
    let is_cgnat = octets[0] == 100 && (octets[1] & 0b1100_0000) == 64;
    let is_benchmark = octets[0] == 198 && (octets[1] & 0b1111_1110) == 18;
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || is_cgnat
        || is_benchmark
        || octets[0] >= 240
}

fn is_non_public_ipv6(ip: std::net::Ipv6Addr) -> bool {
    let segs = ip.segments();
    let is_ula = (segs[0] & 0xfe00) == 0xfc00;
    let is_link_local = (segs[0] & 0xffc0) == 0xfe80;
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || is_ula || is_link_local {
        return true;
    }
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_non_public_ipv4(mapped);
    }
    if segs[0..6] == [0, 0, 0, 0, 0, 0] {
        let compat = std::net::Ipv4Addr::from(((segs[6] as u32) << 16) | (segs[7] as u32));
        return is_non_public_ipv4(compat);
    }
    false
}

fn is_valid_port(port_str: &str) -> bool {
    !port_str.is_empty()
        && port_str.bytes().all(|b| b.is_ascii_digit())
        && matches!(port_str.parse::<u16>(), Ok(1..=u16::MAX))
}

/// Validates an external HTTPS webhook target, rejecting credentials, whitespace/control chars,
/// localhost/internal suffixes, single-label hosts, and private/loopback/link-local IPs.
pub fn is_safe_webhook_url(url: &str) -> bool {
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return false;
    }

    if let Some(after_open) = authority.strip_prefix('[') {
        let Some((ip_str, suffix)) = after_open.split_once(']') else {
            return false;
        };
        if !suffix.is_empty() && !suffix.strip_prefix(':').is_some_and(is_valid_port) {
            return false;
        }
        let Ok(ip6) = ip_str.parse::<std::net::Ipv6Addr>() else {
            return false;
        };
        return !is_non_public_ipv6(ip6);
    }

    let (host, port_opt) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if port_opt.is_some_and(|p| !is_valid_port(p)) {
        return false;
    }

    let lower = host.to_ascii_lowercase();
    if lower.is_empty() || lower.starts_with('.') || lower.ends_with('.') || lower.contains("..") {
        return false;
    }
    if lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || lower.ends_with(".home.arpa")
    {
        return false;
    }
    if let Ok(ip4) = lower.parse::<std::net::Ipv4Addr>() {
        return !is_non_public_ipv4(ip4);
    }
    if !lower.contains('.') {
        return false;
    }
    lower.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

pub fn route(method: &str, path: &str) -> Route {
    let m = method.to_ascii_uppercase();
    match path {
        "/api/health" => match m.as_str() {
            "GET" => Route::Health,
            "HEAD" => Route::HealthHead,
            _ => Route::MethodNotAllowed("GET, HEAD"),
        },
        "/api/inquire" => match m.as_str() {
            "POST" => Route::InquireCompat,
            _ => Route::MethodNotAllowed("POST"),
        },
        "/api/newsletter" => match m.as_str() {
            "POST" => Route::Newsletter,
            _ => Route::MethodNotAllowed("POST"),
        },
        "/api/events" => match m.as_str() {
            "GET" => Route::Events,
            _ => Route::MethodNotAllowed("GET"),
        },
        p if let Some(id) = p
            .strip_prefix("/api/events/")
            .and_then(|p| p.strip_suffix("/ics")) =>
        {
            if is_valid_event_id(id) {
                match m.as_str() {
                    "GET" => Route::EventIcs(id.to_string()),
                    _ => Route::MethodNotAllowed("GET"),
                }
            } else {
                Route::NotFound
            }
        }
        "/api/checkout" => match m.as_str() {
            "POST" => Route::Checkout,
            _ => Route::MethodNotAllowed("POST"),
        },
        p if p.starts_with("/api/forms/") => {
            let id = &p["/api/forms/".len()..];
            if is_valid_form_id(id) {
                match m.as_str() {
                    "POST" => Route::FormSubmit(id.to_string()),
                    _ => Route::MethodNotAllowed("POST"),
                }
            } else {
                Route::NotFound
            }
        }
        p if p == "/api" || p.starts_with("/api/") => Route::NotFound,
        _ => Route::Asset,
    }
}

pub fn health_body(version: &str) -> Value {
    json!({
        "status": "ok",
        "service": "sws-worker-site",
        "version": version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_id_validation() {
        assert!(is_valid_form_id("contact"));
        assert!(is_valid_form_id("a"));
        assert!(is_valid_form_id("0-lead_form_2"));
        assert!(is_valid_form_id(&"a".repeat(64)));

        assert!(!is_valid_form_id(""));
        assert!(!is_valid_form_id(&"a".repeat(65)));
        assert!(!is_valid_form_id("-contact"));
        assert!(!is_valid_form_id("_contact"));
        assert!(!is_valid_form_id("Contact"));
        assert!(!is_valid_form_id("a/b"));
        assert!(!is_valid_form_id("a.b"));
    }

    #[test]
    fn routing_table() {
        assert_eq!(route("GET", "/api/health"), Route::Health);
        assert_eq!(route("head", "/api/health"), Route::HealthHead);
        assert_eq!(route("HEAD", "/api/health"), Route::HealthHead);
        assert_eq!(
            route("POST", "/api/health"),
            Route::MethodNotAllowed("GET, HEAD")
        );

        assert_eq!(
            route("POST", "/api/forms/contact"),
            Route::FormSubmit("contact".to_string())
        );
        assert_eq!(
            route("post", "/api/forms/event_lead-2027"),
            Route::FormSubmit("event_lead-2027".to_string())
        );
        assert_eq!(
            route("GET", "/api/forms/contact"),
            Route::MethodNotAllowed("POST")
        );
        assert_eq!(route("POST", "/api/forms/"), Route::NotFound);
        assert_eq!(route("POST", "/api/forms/-bad"), Route::NotFound);
        assert_eq!(route("POST", "/api/forms/BAD"), Route::NotFound);
        assert_eq!(route("POST", "/api/forms/a/b"), Route::NotFound);

        assert_eq!(route("POST", "/api/inquire"), Route::InquireCompat);
        assert_eq!(
            route("GET", "/api/inquire"),
            Route::MethodNotAllowed("POST")
        );

        assert_eq!(route("POST", "/api/newsletter"), Route::Newsletter);
        assert_eq!(
            route("DELETE", "/api/newsletter"),
            Route::MethodNotAllowed("POST")
        );

        assert_eq!(route("GET", "/api/events"), Route::Events);
        assert_eq!(route("get", "/api/events"), Route::Events);
        assert_eq!(
            route("POST", "/api/events"),
            Route::MethodNotAllowed("GET")
        );

        assert_eq!(
            route("GET", "/api/events/900000001/ics"),
            Route::EventIcs("900000001".to_string())
        );
        assert_eq!(
            route("POST", "/api/events/900000001/ics"),
            Route::MethodNotAllowed("GET")
        );
        assert_eq!(route("GET", "/api/events//ics"), Route::NotFound);
        assert_eq!(route("GET", "/api/events/1x/ics"), Route::NotFound);
        assert_eq!(route("GET", "/api/events/-1/ics"), Route::NotFound);
        assert_eq!(
            route("GET", "/api/events/99999999999999999999/ics"),
            Route::NotFound
        );
        assert_eq!(route("GET", "/api/events/900000001"), Route::NotFound);
        assert_eq!(route("GET", "/api/events/900000001/Ics"), Route::NotFound);

        assert_eq!(route("POST", "/api/checkout"), Route::Checkout);
        assert_eq!(
            route("GET", "/api/checkout"),
            Route::MethodNotAllowed("POST")
        );

        assert_eq!(route("GET", "/api/nope"), Route::NotFound);
        assert_eq!(route("GET", "/api"), Route::NotFound);
        assert_eq!(route("GET", "/"), Route::Asset);
        assert_eq!(route("GET", "/apiary"), Route::Asset);
    }

    #[test]
    fn event_id_validation() {
        assert!(is_valid_event_id("1"));
        assert!(is_valid_event_id("900000001"));
        assert!(is_valid_event_id("9223372036854775807")); // i64::MAX

        assert!(!is_valid_event_id(""));
        assert!(!is_valid_event_id("01x"));
        assert!(!is_valid_event_id("+1"));
        assert!(!is_valid_event_id("-1"));
        assert!(!is_valid_event_id(" 1"));
        assert!(!is_valid_event_id("9223372036854775808")); // i64::MAX + 1
        assert!(!is_valid_event_id(&"9".repeat(20)));
    }

    #[test]
    fn health_payload() {
        let body = health_body("0.1.0");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "sws-worker-site");
        assert_eq!(body["version"], "0.1.0");
    }

    #[test]
    fn webhook_url_validation() {
        assert!(is_safe_webhook_url("https://hooks.example.test/solo"));
        assert!(is_safe_webhook_url("https://api.example.test:8443/v1/hook?a=1"));
        assert!(is_safe_webhook_url("https://[2606:4700:4700::1111]/hook"));

        for bad in [
            "http://hooks.example.test/solo",
            "https://user:pass@hooks.example.test/solo",
            "https://hooks.example.test/a b",
            "https://hooks.example.test/\n",
            "https://localhost/hook",
            "https://sub.localhost/hook",
            "https://db.local/hook",
            "https://metadata.internal/hook",
            "https://singlelabel/hook",
            "https://127.0.0.1/hook",
            "https://10.0.0.5/hook",
            "https://172.16.1.1/hook",
            "https://192.168.1.1/hook",
            "https://169.254.169.254/latest/meta-data",
            "https://100.64.0.1/hook",
            "https://[::1]/hook",
            "https://[fc00::1]/hook",
            "https://[fe80::1]/hook",
            "https://[::ffff:127.0.0.1]/hook",
            "https://hooks.example.test:0/hook",
            "https://[not-ipv6]/hook",
        ] {
            assert!(!is_safe_webhook_url(bad), "expected rejection for {bad}");
        }
    }
}
