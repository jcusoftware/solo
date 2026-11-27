//! Session authorization policy (host-testable).
//!
//! Who the caller is comes from [`crate::auth`] (real auth is #65). What they
//! may do in a session comes from their D1 `memberships` role, which the
//! Durable Object re-reads:
//!
//! * from D1 **before every state-changing message** (seed, batch, undo,
//!   checkpoint), so a removed or demoted member can never apply an edit;
//! * before a read when the cached verification is older than
//!   `authz_recheck_ms` (default 15 s, hard cap
//!   [`crate::session::MAX_AUTHZ_RECHECK_MS`] = 30 s);
//! * for every other connection before each broadcast that originates from a
//!   message, and by the alarm sweep (every
//!   [`crate::session::DEFAULT_IDLE_SWEEP_MS`] while sockets are attached), so a
//!   removed member stops receiving updates and the socket is closed with code
//!   4403; and
//! * as a hard gate at delivery time ([`delivery_allowed`]): a socket whose last
//!   verification is two intervals old is skipped, whatever else happens.
//!
//! #65 hook: `object::fetch_role` is the one D1 lookup; adding session-expiry
//! and token-revocation checks there, and calling `entry::revoke_user` (the
//! Worker -> DO `/internal/revoke` request) on logout, token revocation or
//! membership change, is the whole integration; no protocol change is needed.

use crate::model::Role;

/// What a message wants to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Read state, replay, ping, and read or set presence (presence is soft,
    /// never persisted to the document, and is not an edit).
    Read,
    /// Apply a command batch or an undo.
    Edit,
    /// Explicit save checkpoint.
    Save,
    /// Publish checkpoint and the initial document seed.
    Publish,
}

/// Roles (#60): owner and publisher may publish; every member may edit/save.
pub fn allows(role: Role, action: Action) -> bool {
    match action {
        Action::Read | Action::Edit | Action::Save => true,
        Action::Publish => matches!(role, Role::Owner | Role::Publisher),
    }
}

/// Is this socket, as its attachment is RIGHT NOW (re-read after every await), still
/// allowed to do `action`? `None` (revoked or gone) and an unknown role are no. A
/// demotion that landed during an await (a peer's recheck rewrote the role) is
/// caught here, immediately before the commit.
pub fn allowed_now(live: Option<&crate::session::attach::Attachment>, action: Action) -> bool {
    live.and_then(|a| a.role())
        .is_some_and(|r| allows(r, action))
}

/// True when a recheck is due: `checked_ms` is older than `ttl_ms`.
pub fn needs_recheck(now_ms: i64, checked_ms: i64, ttl_ms: i64) -> bool {
    now_ms.saturating_sub(checked_ms) >= ttl_ms
}

/// True when an update may still be delivered to a connection last verified at
/// `checked_ms`. Delivery is synchronous (ordering) so it cannot await a D1
/// read; a connection is refreshed before the broadcast, and one that is more
/// than two intervals stale (a failed refresh) is skipped until the alarm sweep
/// closes or refreshes it.
pub fn delivery_allowed(now_ms: i64, checked_ms: i64, ttl_ms: i64) -> bool {
    now_ms.saturating_sub(checked_ms) < ttl_ms.saturating_mul(2)
}

/// Parse a `memberships` row (`{ "user_id": n, "role": "editor" }`).
pub fn role_from_row(row: &serde_json::Value) -> Option<Role> {
    row.get("role")
        .and_then(|r| r.as_str())
        .and_then(Role::parse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roles_and_actions() {
        for r in Role::ALL {
            assert!(allows(*r, Action::Read));
            assert!(allows(*r, Action::Edit));
            assert!(allows(*r, Action::Save));
        }
        assert!(allows(Role::Owner, Action::Publish));
        assert!(allows(Role::Publisher, Action::Publish));
        assert!(!allows(Role::Editor, Action::Publish));
    }

    #[test]
    fn a_demotion_after_the_check_is_caught_before_the_commit() {
        use crate::session::attach::Attachment;
        let publisher = Attachment::new("c-1".into(), 1, Role::Publisher, 0);
        assert!(allowed_now(Some(&publisher), Action::Publish));
        // The attachment as re-read after an await: a peer's recheck demoted it.
        let demoted = publisher.clone().refreshed(Role::Editor, 10);
        assert!(!allowed_now(Some(&demoted), Action::Publish));
        assert!(allowed_now(Some(&demoted), Action::Edit));
        // Revoked/gone, or an unreadable role, is never allowed.
        assert!(!allowed_now(None, Action::Read));
        let mut odd = publisher;
        odd.role = "admin".into();
        assert!(!allowed_now(Some(&odd), Action::Read));
    }

    #[test]
    fn recheck_and_delivery_windows() {
        assert!(!needs_recheck(1_000, 500, 1_000));
        assert!(needs_recheck(1_500, 500, 1_000));
        assert!(delivery_allowed(2_400, 500, 1_000));
        assert!(!delivery_allowed(2_500, 500, 1_000));
        // A clock that appears to run backwards never panics or overflows.
        assert!(!needs_recheck(0, i64::MAX, 1_000));
    }

    #[test]
    fn membership_rows() {
        assert_eq!(
            role_from_row(&json!({"user_id": 3, "role": "publisher"})),
            Some(Role::Publisher)
        );
        assert_eq!(role_from_row(&json!({"role": "admin"})), None);
        assert_eq!(role_from_row(&json!({})), None);
    }
}
