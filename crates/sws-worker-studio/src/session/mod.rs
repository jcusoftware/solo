//! Hibernating Durable Object command sessions (issue #62).
//!
//! One Durable Object per site (`SiteSession`, id derived from
//! `workspace_id/site_id`) owns the live document: a per-site SQLite snapshot,
//! a monotonic command log and expiring soft presence, all in DO storage. D1
//! (the registry) is written only at a checkpoint (every [`CHECKPOINT_EVERY`]
//! commands or on save/publish), one immutable `site_revisions` row each.
//!
//! Everything that decides behaviour is pure and host-tested:
//! [`protocol`] (versioned JSON messages), [`engine`] (apply, dedupe, rebase,
//! undo, checkpoint policy, presence, compaction over the [`store::Store`]
//! trait), [`rebase`] (the conflict rules), [`authz`] (roles and the recheck
//! bound) and [`attach`] (the WebSocket attachment). Canonical command
//! reduction is always `sws_ir::edit`; nothing here reimplements it.
//!
//! The wasm-only glue (`object`: the Durable Object, `do_store`: DO SQL,
//! `entry`: the Worker-side upgrade route) sits behind
//! `#[cfg(target_arch = "wasm32")]`.

pub mod attach;
pub mod authz;
pub mod devfence;
pub mod engine;
pub mod fifo;
pub mod protocol;
pub mod rebase;
pub mod sha256;
pub mod sql;
pub mod store;
pub mod txn;

#[cfg(all(target_arch = "wasm32", feature = "insecure-dev-auth"))]
pub mod dev;
#[cfg(target_arch = "wasm32")]
pub mod do_store;
#[cfg(target_arch = "wasm32")]
pub mod entry;
#[cfg(target_arch = "wasm32")]
pub mod http;
#[cfg(target_arch = "wasm32")]
pub mod object;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod origin_tests {
    use super::origin_allowed;

    #[test]
    fn upgrade_origin_must_match_the_public_origin() {
        let p = Some("https://studio.example");
        assert!(origin_allowed(None, p), "CLI/MCP clients send no Origin");
        assert!(origin_allowed(None, None));
        assert!(origin_allowed(Some("https://studio.example"), p));
        assert!(origin_allowed(
            Some("https://studio.example"),
            Some("https://studio.example/")
        ));
        for bad in [
            "https://evil.example",
            "http://studio.example",
            "https://studio.example:8443",
            "https://studio.example.evil.example",
            "null",
            "",
        ] {
            assert!(!origin_allowed(Some(bad), p), "{bad}");
        }
        assert!(!origin_allowed(Some("https://studio.example"), None));
        assert!(!origin_allowed(Some("https://studio.example"), Some("")));
    }

    #[test]
    fn origins_are_normalised_before_comparison() {
        let p = Some("https://Studio.Example:443/");
        for same in [
            "https://studio.example",
            "HTTPS://STUDIO.EXAMPLE",
            "https://studio.example:443",
            " https://studio.example/ ",
        ] {
            assert!(origin_allowed(Some(same), p), "{same}");
        }
        assert!(origin_allowed(
            Some("http://127.0.0.1:8790"),
            Some("HTTP://127.0.0.1:8790")
        ));
        assert!(origin_allowed(
            Some("http://localhost:80"),
            Some("http://localhost")
        ));
        for bad in [
            "https://studio.example:444",
            "http://studio.example",
            "https://studio.example/path",
            "https://studio.example?x",
            "https://user@studio.example",
            "https://studio.example.evil.example",
            "https://evilstudio.example",
            "ftp://studio.example",
            "null",
            "",
            "   ",
            "https://",
            "https://studio.example:",
            "https://studio.example:99999",
        ] {
            assert!(!origin_allowed(Some(bad), p), "{bad}");
        }
        // An unparsable setting never matches anything (not even itself).
        assert!(!origin_allowed(Some("null"), Some("null")));
        assert!(!origin_allowed(
            Some("studio.example"),
            Some("studio.example")
        ));
    }

    #[test]
    fn a_failed_alarm_plan_falls_back_to_the_idle_sweep_while_sockets_are_attached() {
        use super::alarm_after;
        // Planned: whatever the engine said (including "nothing to do").
        assert_eq!(alarm_after(Some(Some(5)), 100, 2, 60_000), Some(5));
        assert_eq!(alarm_after(Some(None), 100, 2, 60_000), None);
        // The engine could not open: sockets still get a sweep, an empty object none.
        assert_eq!(alarm_after(None, 100, 2, 60_000), Some(60_100));
        assert_eq!(alarm_after(None, 100, 0, 60_000), None);
    }

    #[test]
    fn idle_sweep_is_long_by_default_and_capped() {
        use super::*;
        let d = SessionConfig::default();
        assert!(d.idle_sweep_ms >= 60_000);
        assert!(d.idle_sweep_ms > d.authz_recheck_ms);
        let c = SessionConfig::from_vars(None, None, Some("99999999"));
        assert_eq!(c.idle_sweep_ms, MAX_IDLE_SWEEP_MS);
        assert_eq!(MAX_IDLE_SWEEP_MS, 60_000, "the #60 worst case");
        // Floor: a second, and never faster than the recheck interval.
        assert_eq!(
            SessionConfig::from_vars(None, None, Some("1")).idle_sweep_ms,
            DEFAULT_AUTHZ_RECHECK_MS.max(1_000)
        );
        assert_eq!(
            SessionConfig::from_vars(None, Some("250"), Some("1")).idle_sweep_ms,
            1_000
        );
        assert_eq!(
            SessionConfig::from_vars(None, Some("20000"), Some("5000")).idle_sweep_ms,
            20_000
        );
        // Delivery to a removed member is bounded by the gate, whatever the sweep does.
        const { assert!(2 * MAX_AUTHZ_RECHECK_MS <= 60_000) };
    }
}

#[cfg(test)]
mod close_code_tests {
    use super::echo_close_code;

    #[test]
    fn reserved_close_codes_are_never_echoed() {
        for reserved in [0, 1005, 1006, 1015, 1001, 2999, 5000, 65_535, usize::MAX] {
            assert_eq!(echo_close_code(reserved), 1000, "{reserved}");
        }
        assert_eq!(echo_close_code(1000), 1000);
        assert_eq!(echo_close_code(4403), 4403);
    }
}

/// Wire protocol version carried in every message (`"v"`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Hard cap on any inbound WebSocket message (only `seed` may approach it).
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Cap on every inbound message except `seed`.
pub const MAX_COMMAND_MESSAGE_BYTES: usize = 256 * 1024;
/// Cap on the canonical document text (one DO SQLite row; the row limit is 2 MB).
pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Commands per session batch (tighter than the reducer's own 1000).
pub const MAX_SESSION_BATCH_COMMANDS: usize = 200;
/// Concurrent sockets per site, and per user within a site.
pub const MAX_CONNECTIONS: usize = 16;
pub const MAX_CONNECTIONS_PER_USER: usize = 4;

/// Checkpoint after this many commands since the last one (or on save/publish).
pub const CHECKPOINT_EVERY: i64 = 50;
/// While a checkpoint's D1 write is unresolved, refuse edits beyond this many
/// commands since the last checkpoint (bounded backlog under a D1 outage).
pub const MAX_UNCHECKPOINTED: i64 = 200;
/// Log entries kept below the latest snapshot so slightly stale batches can
/// still be rebased; older entries are removed by the compaction alarm.
pub const LOG_RETAIN: i64 = 50;
/// How long an operation id is remembered for idempotent retries, and the cap.
pub const OP_TTL_MS: i64 = 24 * 60 * 60 * 1000;
pub const MAX_OPS: i64 = 5000;
/// Largest number of log entries a `sync` replay will return before falling
/// back to a full `state`.
pub const REPLAY_MAX: i64 = 500;

pub const PRESENCE_STATE_MAX_BYTES: usize = 512;
pub const PRESENCE_MIN_INTERVAL_MS: i64 = 50;

/// Defaults and clamps for the two operational knobs (Worker vars).
pub const DEFAULT_PRESENCE_TTL_MS: i64 = 30_000;
pub const DEFAULT_AUTHZ_RECHECK_MS: i64 = 15_000;
/// Upper bound for the membership recheck interval. The delivery gate is twice
/// the interval ([`authz::delivery_allowed`]), so the worst-case time a removed
/// member can still be sent anything is `2 * MAX_AUTHZ_RECHECK_MS` = 60 s,
/// independent of when the object next wakes.
pub const MAX_AUTHZ_RECHECK_MS: i64 = 30_000;
/// Idle membership sweep (alarm) cadence while sockets are attached. Deliberately
/// long so an idle site can hibernate between sweeps (the object is billed for
/// wall-clock time while awake); security does not depend on it, because
/// delivery is gated by the age of each socket's last verification and every
/// state-changing message re-reads membership. The sweep only closes idle
/// sockets of removed members.
pub const DEFAULT_IDLE_SWEEP_MS: i64 = 60_000;
/// Cap: an idle removed member's socket is closed within this plus one D1 read
/// (the #60 "60 s worst case").
pub const MAX_IDLE_SWEEP_MS: i64 = 60_000;
/// Inbound messages that may be queued (held or waiting) in the per-object FIFO;
/// more are answered `rate_limited` without being queued, so a flooding socket
/// cannot grow memory without bound.
pub const MAX_QUEUED_MESSAGES: u64 = 32;

/// Application close codes (4000-4999 are reserved for applications).
pub const CLOSE_PROTOCOL: u16 = 4400;
pub const CLOSE_UNAUTHENTICATED: u16 = 4401;
pub const CLOSE_REVOKED: u16 = 4403;
pub const CLOSE_TOO_LARGE: u16 = 1009;
pub const CLOSE_UNSUPPORTED_DATA: u16 = 1003;

/// Internal Worker -> Durable Object headers. The DO is reachable only through
/// the Worker binding, and the Worker always overwrites these.
pub const H_WORKSPACE: &str = "X-Sws-Internal-Workspace";
pub const H_SITE: &str = "X-Sws-Internal-Site";
pub const H_USER: &str = "X-Sws-Internal-User";
pub const H_ROLE: &str = "X-Sws-Internal-Role";
/// Reserved for #65: the id of the session or token the caller authenticated
/// with, kept in the attachment so revalidation can check it.
pub const H_CREDENTIAL: &str = "X-Sws-Internal-Credential";
/// Internal DO paths.
pub const PATH_SESSION: &str = "/session";
pub const PATH_REVOKE: &str = "/internal/revoke";
/// `GET /internal/revision?id=<revision_id>`: the snapshot bytes of a checkpoint (#64).
pub const PATH_REVISION: &str = "/internal/revision";
/// Test-build only: prove DO SQL `transactionSync` commits and rolls back.
#[cfg(feature = "insecure-dev-auth")]
pub const PATH_TX_PROBE: &str = "/internal/tx-probe";
/// Test-build only: `POST /internal/d1-fault?mode=down|checkpoint|up` makes this object's D1
/// calls (down: membership reads and checkpoint writes; checkpoint: writes only) fail,
/// or (slow) every membership read takes 400 ms, to prove fail-closed behaviour.
#[cfg(feature = "insecure-dev-auth")]
pub const PATH_D1_FAULT: &str = "/internal/d1-fault";

/// Operational settings read from Worker vars, clamped to safe ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    pub presence_ttl_ms: i64,
    pub authz_recheck_ms: i64,
    pub idle_sweep_ms: i64,
}

impl SessionConfig {
    pub fn from_vars(
        presence_ttl: Option<&str>,
        authz_recheck: Option<&str>,
        idle_sweep: Option<&str>,
    ) -> Self {
        let get = |v: Option<&str>, default: i64, lo: i64, hi: i64| {
            v.and_then(|s| s.trim().parse::<i64>().ok())
                .map_or(default, |n| n.clamp(lo, hi))
        };
        let authz_recheck_ms = get(
            authz_recheck,
            DEFAULT_AUTHZ_RECHECK_MS,
            250,
            MAX_AUTHZ_RECHECK_MS,
        );
        Self {
            presence_ttl_ms: get(presence_ttl, DEFAULT_PRESENCE_TTL_MS, 500, 300_000),
            authz_recheck_ms,
            // Floor: never sweep (one D1 read per distinct user) more often than the
            // recheck interval, nor than once a second.
            idle_sweep_ms: get(
                idle_sweep,
                DEFAULT_IDLE_SWEEP_MS,
                authz_recheck_ms.max(1_000),
                MAX_IDLE_SWEEP_MS,
            ),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::from_vars(None, None, None)
    }
}

/// Browser-origin gate for the WebSocket upgrade (CSWSH defence). A request
/// without an `Origin` header (CLI and MCP token clients are not browsers) is
/// allowed; one that carries an `Origin` must equal the configured
/// `STUDIO_PUBLIC_ORIGIN` after normalisation ([`normalize_origin`]: scheme and
/// host are case-insensitive, a default port and a trailing `/` do not matter).
/// `null`, an empty value, a different port or a host suffix never match, and an
/// unset or unparsable setting fails closed for browser-originated requests.
pub fn origin_allowed(origin: Option<&str>, public_origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    match (
        public_origin.and_then(normalize_origin),
        normalize_origin(origin),
    ) {
        (Some(allowed), Some(got)) => allowed == got,
        _ => false,
    }
}

/// `scheme://host[:port]` with a lowercase scheme and host and the default port
/// (443 for https, 80 for http) dropped; `None` for anything that is not a bare
/// http(s) origin (a path, query, credentials, `null`, an empty string).
pub fn normalize_origin(raw: &str) -> Option<String> {
    let s = raw.trim().trim_end_matches('/');
    let (scheme, rest) = s.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port: u16 = match scheme.as_str() {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    if rest.is_empty()
        || rest
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')))
    {
        return None;
    }
    let (host, port) = match rest.rfind(':') {
        Some(i) if !rest[i..].contains(']') => (&rest[..i], Some(&rest[i + 1..])),
        _ => (rest, None),
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) => {
            Some(p.parse::<u16>().ok()?)
        }
        Some(_) => return None,
    };
    let host = host.to_ascii_lowercase();
    Some(match port.filter(|p| *p != default_port) {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    })
}

/// When to arm the alarm. `planned` is the engine's plan (`None` if the engine
/// could not be opened). A failed plan must not leave sockets without a sweep, or
/// a removed member's idle socket would stay open: fall back to the idle sweep.
pub fn alarm_after(
    planned: Option<Option<i64>>,
    now_ms: i64,
    connections: usize,
    idle_sweep_ms: i64,
) -> Option<i64> {
    match planned {
        Some(at) => at,
        None if connections > 0 => Some(now_ms + idle_sweep_ms),
        None => None,
    }
}

/// Close code to echo back for a peer's close frame. Reserved codes (1005 "no
/// status", 1006, 1015) can never be sent, and the runtime only accepts 1000 and
/// the application range, so everything else is answered with a normal 1000.
pub fn echo_close_code(code: usize) -> u16 {
    match code {
        1000 | 3000..=4999 => code as u16,
        _ => 1000,
    }
}

/// `workspace_id/site_id`: the Durable Object name. Ids are `[a-z0-9_-]{1,64}`
/// (no `/`), so distinct pairs can never collide.
pub fn object_name(workspace_id: &str, site_id: &str) -> String {
    format!("{workspace_id}/{site_id}")
}

/// Same grammar as the registry's `workspaces.id` / `sites.id` CHECK.
pub fn is_valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
