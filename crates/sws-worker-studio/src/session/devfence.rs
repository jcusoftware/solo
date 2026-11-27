//! Runtime fence for the `insecure-dev-auth` test build (pure, host-testable).
//!
//! The dev-auth header and the test-only routes are compiled only under the
//! `insecure-dev-auth` feature, which never ships (`npm run check:studio:release`).
//! This is the second layer: even in that build, they answer only when the test
//! script passes `--var SWS_INSECURE_DEV_AUTH:1` (a var no committed config sets)
//! AND the request's `Host` is loopback. A dev-auth Worker that is ever deployed
//! by mistake therefore still answers 401, because neither condition holds there.

/// Name of the var only the local test script passes.
pub const FENCE_VAR: &str = "SWS_INSECURE_DEV_AUTH";

/// True when the host (with optional port) is a loopback address.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim();
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
            .map_or(h, |(n, _)| n)
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// Open only with the explicit flag AND a loopback Host.
pub fn open(flag: Option<&str>, host: Option<&str>) -> bool {
    flag.map(str::trim) == Some("1") && host.is_some_and(is_loopback_host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_the_flag_and_a_loopback_host() {
        assert!(open(Some("1"), Some("127.0.0.1:8790")));
        assert!(open(Some("1"), Some("localhost")));
        assert!(open(Some("1"), Some("[::1]:8790")));
        assert!(!open(None, Some("127.0.0.1:8790")), "no flag");
        assert!(!open(Some("0"), Some("127.0.0.1")));
        assert!(!open(Some("true"), Some("127.0.0.1")));
        assert!(!open(Some("1"), None));
        for h in [
            "studio.example",
            "sws-studio.workers.dev",
            "127.0.0.1.evil.example",
            "localhost.evil.example:80",
            "0.0.0.0:8790",
            "127.0.0.2",
            "evil.example:127.0.0.1",
        ] {
            assert!(!open(Some("1"), Some(h)), "{h}");
        }
    }
}
