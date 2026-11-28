//! The per-socket attachment, restored after hibernation.
//!
//! The Workers runtime keeps a small structured-clone value with each
//! hibernatable WebSocket (limit 2048 bytes). It carries identity and the last
//! authorization result so a freshly constructed Durable Object knows who is
//! on the socket. It is **identity, not authority**: every state-changing
//! message re-reads the role from D1 (see [`crate::session::authz`]).

use serde::{Deserialize, Serialize};

use crate::model::Role;

/// Attachment size limit imposed by the runtime.
pub const MAX_ATTACHMENT_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Attachment format version.
    pub v: u32,
    pub client_id: String,
    pub user_id: i64,
    /// `owner` / `publisher` / `editor` as last verified against D1.
    pub role: String,
    /// Epoch ms of the last successful D1 membership check.
    pub authz_checked_ms: i64,
    /// Tag of the session or token this socket authenticated with (`s:<id>` /
    /// `t:<id>`, #65); revalidated on every recheck. `None` only in the dev-auth build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    /// Set just before the socket is closed for revocation; never delivered to.
    #[serde(default)]
    pub revoked: bool,
}

impl Attachment {
    pub fn new(client_id: String, user_id: i64, role: Role, now_ms: i64) -> Self {
        Self {
            v: 1,
            client_id,
            user_id,
            role: role.as_str().to_string(),
            authz_checked_ms: now_ms,
            credential: None,
            revoked: false,
        }
    }

    /// The stored role; an unknown value fails closed to `None`.
    pub fn role(&self) -> Option<Role> {
        Role::parse(&self.role)
    }

    pub fn refreshed(mut self, role: Role, now_ms: i64) -> Self {
        self.role = role.as_str().to_string();
        self.authz_checked_ms = now_ms;
        self
    }
}

/// `WebSocket.readyState` 1 = OPEN. Closing (2) and closed (3) sockets must not
/// get new soft state written on their behalf.
pub fn ready_state_open(ready_state: u16) -> bool {
    ready_state == 1
}

/// What an asynchronous membership read means for a socket, decided against the
/// attachment as it is AFTER the await (never the copy taken before it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// The socket is gone or was revoked meanwhile (revocation is sticky):
    /// write nothing, do nothing.
    Gone,
    /// Still a member: store this refreshed attachment.
    Member(Attachment),
    /// No longer a member: mark this attachment revoked and close the socket.
    Removed(Attachment),
}

/// Merge a membership result onto the *fresh* attachment. A pre-await copy
/// must never be written back: a concurrent revocation (`revoke_user`, another
/// recheck) would be reset to `revoked: false`.
pub fn merge_membership(fresh: Option<Attachment>, role: Option<Role>, now_ms: i64) -> Refresh {
    match fresh {
        None => Refresh::Gone,
        Some(a) if a.revoked => Refresh::Gone,
        Some(a) => match role {
            Some(r) => Refresh::Member(a.refreshed(r, now_ms)),
            None => Refresh::Removed(a),
        },
    }
}

/// Decide whether a new connection fits the bounds, given the user ids of the
/// connections already attached. `Err` carries the actionable reason.
pub fn admit(existing: &[i64], user_id: i64) -> Result<(), &'static str> {
    if existing.len() >= crate::session::MAX_CONNECTIONS {
        return Err("too_many_connections");
    }
    if existing.iter().filter(|u| **u == user_id).count()
        >= crate::session::MAX_CONNECTIONS_PER_USER
    {
        return Err("too_many_connections_for_user");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_open_socket_gets_soft_state() {
        assert!(ready_state_open(1));
        for s in [0, 2, 3, 99] {
            assert!(!ready_state_open(s), "{s}");
        }
    }

    #[test]
    fn round_trips_and_stays_small() {
        let a = Attachment::new(
            "c-0123456789abcdef".into(),
            9_007_199_254_740_991,
            Role::Editor,
            1,
        );
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.len() < MAX_ATTACHMENT_BYTES / 4, "{}", json.len());
        let back: Attachment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.role(), Some(Role::Editor));
    }

    #[test]
    fn unknown_role_fails_closed() {
        let mut a = Attachment::new("c".into(), 1, Role::Owner, 0);
        a.role = "admin".into();
        assert_eq!(a.role(), None);
    }

    #[test]
    fn refresh_updates_role_and_time() {
        let a = Attachment::new("c".into(), 1, Role::Publisher, 10).refreshed(Role::Editor, 99);
        assert_eq!((a.role(), a.authz_checked_ms), (Some(Role::Editor), 99));
    }

    #[test]
    fn refresh_never_undoes_a_concurrent_revocation() {
        let stale = Attachment::new("c".into(), 1, Role::Editor, 10);
        // Between the pre-await read and now, the socket was revoked.
        let mut fresh = stale.clone();
        fresh.revoked = true;
        assert_eq!(
            merge_membership(Some(fresh.clone()), Some(Role::Editor), 99),
            Refresh::Gone
        );
        assert_eq!(merge_membership(Some(fresh), None, 99), Refresh::Gone);
        assert_eq!(merge_membership(None, Some(Role::Owner), 99), Refresh::Gone);
        // A live socket takes the new role and time and stays unrevoked.
        match merge_membership(Some(stale.clone()), Some(Role::Publisher), 99) {
            Refresh::Member(a) => {
                assert_eq!(
                    (a.role(), a.authz_checked_ms, a.revoked),
                    (Some(Role::Publisher), 99, false)
                );
                assert_eq!(a.client_id, stale.client_id);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            merge_membership(Some(stale.clone()), None, 99),
            Refresh::Removed(stale)
        );
    }

    #[test]
    fn connection_bounds() {
        let many: Vec<i64> = (0..crate::session::MAX_CONNECTIONS as i64).collect();
        assert_eq!(admit(&many, 99), Err("too_many_connections"));
        assert_eq!(admit(&[1, 2, 3], 4), Ok(()));
        let same = vec![7; crate::session::MAX_CONNECTIONS_PER_USER];
        assert_eq!(admit(&same, 7), Err("too_many_connections_for_user"));
        assert_eq!(admit(&same, 8), Ok(()));
    }
}
