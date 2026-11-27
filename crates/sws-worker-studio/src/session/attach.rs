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
    /// Reserved for #65: the session/token id this socket authenticated with.
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
    fn connection_bounds() {
        let many: Vec<i64> = (0..crate::session::MAX_CONNECTIONS as i64).collect();
        assert_eq!(admit(&many, 99), Err("too_many_connections"));
        assert_eq!(admit(&[1, 2, 3], 4), Ok(()));
        let same = vec![7; crate::session::MAX_CONNECTIONS_PER_USER];
        assert_eq!(admit(&same, 7), Err("too_many_connections_for_user"));
        assert_eq!(admit(&same, 8), Ok(()));
    }
}
