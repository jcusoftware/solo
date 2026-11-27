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
pub mod engine;
pub mod protocol;
pub mod rebase;
pub mod sha256;
pub mod sql;
pub mod store;

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
/// Documented upper bound for the membership recheck interval (and the
/// worst-case delivery window after a membership removal, plus the alarm tick).
pub const MAX_AUTHZ_RECHECK_MS: i64 = 30_000;

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

/// Operational settings read from Worker vars, clamped to safe ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    pub presence_ttl_ms: i64,
    pub authz_recheck_ms: i64,
}

impl SessionConfig {
    pub fn from_vars(presence_ttl: Option<&str>, authz_recheck: Option<&str>) -> Self {
        let get = |v: Option<&str>, default: i64, lo: i64, hi: i64| {
            v.and_then(|s| s.trim().parse::<i64>().ok())
                .map_or(default, |n| n.clamp(lo, hi))
        };
        Self {
            presence_ttl_ms: get(presence_ttl, DEFAULT_PRESENCE_TTL_MS, 500, 300_000),
            authz_recheck_ms: get(
                authz_recheck,
                DEFAULT_AUTHZ_RECHECK_MS,
                250,
                MAX_AUTHZ_RECHECK_MS,
            ),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::from_vars(None, None)
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
