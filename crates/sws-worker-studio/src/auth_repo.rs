//! D1 statements for authentication, tokens, invitations and memberships (#65),
//! host-testable. All values are bound parameters. The Node SQL test
//! (`tests/unit/studio-auth.test.mjs`) runs these exact strings (dumped by
//! `cargo run --example dump_sql -- --auth`) against the real migrations.
//!
//! Limits that must hold under concurrency (at most 10 active tokens per user,
//! pending invitations per workspace) are enforced
//! INSIDE the inserting statement (`INSERT ... SELECT ... WHERE (SELECT COUNT(*)
//! ...) < ?`), so two racing requests cannot both pass a separate count.

use crate::repo::{Param, Statement};

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

// --- Browser sessions -----------------------------------------------------------

pub const SESSION_INSERT: &str = "INSERT INTO auth_sessions \
(id, user_id, secret_hash, created_at, last_seen_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?4, ?5)";
pub const SESSION_GET: &str = "SELECT id, user_id, secret_hash, created_at, last_seen_at, \
expires_at, revoked_at FROM auth_sessions WHERE id = ?1";
/// Revalidation from the session Durable Object: bound to the socket's user.
pub const SESSION_CHECK: &str = "SELECT created_at, last_seen_at, expires_at, revoked_at \
FROM auth_sessions WHERE id = ?1 AND user_id = ?2";
pub const SESSION_TOUCH: &str = "UPDATE auth_sessions SET last_seen_at = ?2 \
WHERE id = ?1 AND revoked_at IS NULL AND last_seen_at < ?2";
pub const SESSION_REVOKE: &str = "UPDATE auth_sessions SET revoked_at = ?3 \
WHERE id = ?1 AND user_id = ?2 AND revoked_at IS NULL";
/// Housekeeping at sign-in: drop this user's dead sessions (bounded table).
pub const SESSION_PRUNE_USER: &str = "DELETE FROM auth_sessions WHERE user_id = ?1 \
AND (revoked_at IS NOT NULL OR expires_at <= ?2 OR last_seen_at <= ?3)";

// --- CLI/MCP tokens (table from 0001_registry.sql) --------------------------------

/// Bearer authentication: by public id across workspaces; the digest is then
/// compared in constant time and the workspace is enforced by `authorize`.
pub const TOKEN_AUTH: &str = "SELECT id, workspace_id, user_id, token_hash, scope, expires_at, \
revoked_at, last_used_at FROM api_tokens WHERE id = ?1";
/// Revalidation from the session Durable Object: bound to its workspace and the socket's user.
pub const TOKEN_CHECK: &str = "SELECT scope, expires_at, revoked_at FROM api_tokens \
WHERE workspace_id = ?1 AND id = ?2 AND user_id = ?3";
pub const TOKEN_TOUCH: &str =
    "UPDATE api_tokens SET last_used_at = ?2 WHERE id = ?1 AND revoked_at IS NULL";
/// Insert unless the user already holds `?11` active tokens (`?10`: a token id
/// excluded from the count, the one being rotated; `''` on create). A rotation
/// inserts only while the old token is still unrevoked, checked in this same
/// statement, so two concurrent rotations of one token cannot fork its chain:
/// the batch that commits second inserts nothing.
pub const TOKEN_INSERT_LIMITED: &str = "INSERT INTO api_tokens \
(workspace_id, id, user_id, token_hash, scope, label, created_at, expires_at, chain_started_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
WHERE (SELECT COUNT(*) FROM api_tokens WHERE user_id = ?3 AND revoked_at IS NULL \
AND expires_at > ?7 AND id <> ?10) < ?11 \
AND (?10 = '' OR EXISTS (SELECT 1 FROM api_tokens WHERE id = ?10 AND user_id = ?3 \
AND revoked_at IS NULL))";
/// Second half of a rotation batch: revoke the old token only if its successor exists.
pub const TOKEN_REVOKE_ROTATED: &str = "UPDATE api_tokens SET revoked_at = ?3 \
WHERE id = ?1 AND user_id = ?2 AND revoked_at IS NULL \
AND EXISTS (SELECT 1 FROM api_tokens WHERE id = ?4)";
pub const TOKEN_GET_OWN: &str = "SELECT id, workspace_id, user_id, scope, label, created_at, \
expires_at, chain_started_at, revoked_at FROM api_tokens WHERE id = ?1 AND user_id = ?2";
pub const TOKEN_LIST_OWN: &str = "SELECT id, workspace_id, scope, label, created_at, expires_at, \
chain_started_at, last_used_at, revoked_at FROM api_tokens WHERE user_id = ?1 \
ORDER BY created_at DESC, id LIMIT ?2";
pub const TOKEN_REVOKE_OWN: &str = "UPDATE api_tokens SET revoked_at = ?3 \
WHERE id = ?1 AND user_id = ?2 AND revoked_at IS NULL";
pub const TOKEN_COUNT_ACTIVE: &str = "SELECT COUNT(*) AS n FROM api_tokens \
WHERE user_id = ?1 AND revoked_at IS NULL AND expires_at > ?2";
/// On demotion to editor: revoke that member's publish/activate tokens in the workspace.
pub const TOKEN_REVOKE_ABOVE_EDIT: &str = "UPDATE api_tokens SET revoked_at = ?3 \
WHERE workspace_id = ?1 AND user_id = ?2 AND scope <> 'edit' AND revoked_at IS NULL";

// --- Users, owner bootstrap and memberships -----------------------------------------

pub const USER_GET: &str = "SELECT id, github_id, github_login FROM users WHERE id = ?1";
pub const USER_UPDATE_LOGIN: &str = "UPDATE users SET github_login = ?2, updated_at = ?3 \
WHERE github_id = ?1 AND github_login <> ?2";
pub const USER_MEMBERSHIP_COUNT: &str = "SELECT COUNT(*) AS n FROM memberships WHERE user_id = ?1";
pub const USER_WORKSPACES: &str = "SELECT workspace_id, role FROM memberships \
WHERE user_id = ?1 ORDER BY workspace_id LIMIT ?2";
pub const BOOTSTRAP_WORKSPACE: &str =
    "INSERT INTO workspaces (id, name) VALUES (?1, ?2) ON CONFLICT (id) DO NOTHING";
/// The configured owner becomes owner only while the workspace has none (the
/// unique owner index backs this up), so a second bootstrap is impossible.
pub const BOOTSTRAP_OWNER: &str = "INSERT INTO memberships (workspace_id, user_id, role) \
SELECT ?1, u.id, 'owner' FROM users u WHERE u.github_id = ?2 \
AND NOT EXISTS (SELECT 1 FROM memberships WHERE workspace_id = ?1 AND role = 'owner') \
ON CONFLICT (workspace_id, user_id) DO NOTHING";
pub const MEMBERS_LIST: &str = "SELECT m.user_id, u.github_login, m.role, m.created_at \
FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.workspace_id = ?1 \
ORDER BY m.created_at, m.user_id LIMIT ?2";
/// Never touches the owner and never grants owner.
pub const MEMBER_SET_ROLE: &str = "UPDATE memberships SET role = ?3 \
WHERE workspace_id = ?1 AND user_id = ?2 AND role <> 'owner' AND ?3 IN ('publisher', 'editor')";
/// Never removes the owner. `api_tokens` rows cascade (FK to memberships).
pub const MEMBER_REMOVE: &str =
    "DELETE FROM memberships WHERE workspace_id = ?1 AND user_id = ?2 AND role <> 'owner'";

// --- Invitations ------------------------------------------------------------------

pub const INVITE_INSERT_LIMITED: &str = "INSERT INTO invitations \
(workspace_id, id, secret_hash, github_login, role, invited_by, created_at, expires_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8 \
WHERE (SELECT COUNT(*) FROM invitations WHERE workspace_id = ?1 AND accepted_at IS NULL \
AND revoked_at IS NULL AND expires_at > ?7) < ?9";
pub const INVITE_LIST: &str = "SELECT id, github_login, role, invited_by, created_at, expires_at, \
accepted_at, accepted_by, revoked_at FROM invitations WHERE workspace_id = ?1 \
ORDER BY created_at DESC, id LIMIT ?2";
pub const INVITE_REVOKE: &str = "UPDATE invitations SET revoked_at = ?3 \
WHERE workspace_id = ?1 AND id = ?2 AND accepted_at IS NULL AND revoked_at IS NULL";
/// Accept form and callback: by public id; the digest is compared by the Worker.
pub const INVITE_BY_ID: &str = "SELECT id, workspace_id, secret_hash, github_login, role, \
expires_at, accepted_at, revoked_at FROM invitations WHERE id = ?1";
/// Single use, unexpired, unrevoked, and only for the invited login.
pub const INVITE_ACCEPT: &str = "UPDATE invitations SET accepted_at = ?2, \
accepted_by = (SELECT id FROM users WHERE github_id = ?3) \
WHERE id = ?1 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ?2 \
AND github_login = ?4";
/// Membership from the invitation this very batch accepted (an existing member keeps their role).
pub const INVITE_MEMBERSHIP: &str = "INSERT INTO memberships (workspace_id, user_id, role) \
SELECT i.workspace_id, i.accepted_by, i.role FROM invitations i \
WHERE i.id = ?1 AND i.accepted_at = ?2 AND i.accepted_by = (SELECT id FROM users WHERE github_id = ?3) \
ON CONFLICT (workspace_id, user_id) DO NOTHING";

/// Every named statement (name, sql), for the SQL-level test.
pub const ALL: &[(&str, &str)] = &[
    ("SESSION_INSERT", SESSION_INSERT),
    ("SESSION_GET", SESSION_GET),
    ("SESSION_CHECK", SESSION_CHECK),
    ("SESSION_TOUCH", SESSION_TOUCH),
    ("SESSION_REVOKE", SESSION_REVOKE),
    ("SESSION_PRUNE_USER", SESSION_PRUNE_USER),
    ("TOKEN_AUTH", TOKEN_AUTH),
    ("TOKEN_CHECK", TOKEN_CHECK),
    ("TOKEN_TOUCH", TOKEN_TOUCH),
    ("TOKEN_INSERT_LIMITED", TOKEN_INSERT_LIMITED),
    ("TOKEN_REVOKE_ROTATED", TOKEN_REVOKE_ROTATED),
    ("TOKEN_GET_OWN", TOKEN_GET_OWN),
    ("TOKEN_LIST_OWN", TOKEN_LIST_OWN),
    ("TOKEN_REVOKE_OWN", TOKEN_REVOKE_OWN),
    ("TOKEN_COUNT_ACTIVE", TOKEN_COUNT_ACTIVE),
    ("TOKEN_REVOKE_ABOVE_EDIT", TOKEN_REVOKE_ABOVE_EDIT),
    ("USER_GET", USER_GET),
    ("USER_UPDATE_LOGIN", USER_UPDATE_LOGIN),
    ("USER_MEMBERSHIP_COUNT", USER_MEMBERSHIP_COUNT),
    ("USER_WORKSPACES", USER_WORKSPACES),
    ("BOOTSTRAP_WORKSPACE", BOOTSTRAP_WORKSPACE),
    ("BOOTSTRAP_OWNER", BOOTSTRAP_OWNER),
    ("MEMBERS_LIST", MEMBERS_LIST),
    ("MEMBER_SET_ROLE", MEMBER_SET_ROLE),
    ("MEMBER_REMOVE", MEMBER_REMOVE),
    ("INVITE_INSERT_LIMITED", INVITE_INSERT_LIMITED),
    ("INVITE_LIST", INVITE_LIST),
    ("INVITE_REVOKE", INVITE_REVOKE),
    ("INVITE_BY_ID", INVITE_BY_ID),
    ("INVITE_ACCEPT", INVITE_ACCEPT),
    ("INVITE_MEMBERSHIP", INVITE_MEMBERSHIP),
];

fn st(sql: &'static str, params: Vec<Param>) -> Statement {
    Statement { sql, params }
}

// --- Builders ---------------------------------------------------------------------

pub fn session_insert(
    id: &str,
    user_id: i64,
    hash: &str,
    created: &str,
    expires: &str,
) -> Statement {
    st(
        SESSION_INSERT,
        vec![t(id), Param::Int(user_id), t(hash), t(created), t(expires)],
    )
}
pub fn session_get(id: &str) -> Statement {
    st(SESSION_GET, vec![t(id)])
}
pub fn session_check(id: &str, user_id: i64) -> Statement {
    st(SESSION_CHECK, vec![t(id), Param::Int(user_id)])
}
pub fn session_touch(id: &str, now: &str) -> Statement {
    st(SESSION_TOUCH, vec![t(id), t(now)])
}
pub fn session_revoke(id: &str, user_id: i64, now: &str) -> Statement {
    st(SESSION_REVOKE, vec![t(id), Param::Int(user_id), t(now)])
}
pub fn session_prune_user(user_id: i64, now: &str, idle_cutoff: &str) -> Statement {
    st(
        SESSION_PRUNE_USER,
        vec![Param::Int(user_id), t(now), t(idle_cutoff)],
    )
}

pub fn token_auth(id: &str) -> Statement {
    st(TOKEN_AUTH, vec![t(id)])
}
pub fn token_check(workspace_id: &str, id: &str, user_id: i64) -> Statement {
    st(
        TOKEN_CHECK,
        vec![t(workspace_id), t(id), Param::Int(user_id)],
    )
}
pub fn token_touch(id: &str, now: &str) -> Statement {
    st(TOKEN_TOUCH, vec![t(id), t(now)])
}

/// Fields of a token row to insert.
pub struct NewToken<'a> {
    pub workspace_id: &'a str,
    pub id: &'a str,
    pub user_id: i64,
    pub hash: &'a str,
    pub scope: &'a str,
    pub label: &'a str,
    pub created_at: &'a str,
    pub expires_at: &'a str,
    pub chain_started_at: &'a str,
    /// The token being rotated (excluded from the active count), if any.
    pub replaces: Option<&'a str>,
}

pub fn token_insert_limited(n: &NewToken<'_>, max_active: i64) -> Statement {
    st(
        TOKEN_INSERT_LIMITED,
        vec![
            t(n.workspace_id),
            t(n.id),
            Param::Int(n.user_id),
            t(n.hash),
            t(n.scope),
            t(n.label),
            t(n.created_at),
            t(n.expires_at),
            t(n.chain_started_at),
            t(n.replaces.unwrap_or("")),
            Param::Int(max_active),
        ],
    )
}
pub fn token_revoke_rotated(old_id: &str, user_id: i64, now: &str, new_id: &str) -> Statement {
    st(
        TOKEN_REVOKE_ROTATED,
        vec![t(old_id), Param::Int(user_id), t(now), t(new_id)],
    )
}
pub fn token_get_own(id: &str, user_id: i64) -> Statement {
    st(TOKEN_GET_OWN, vec![t(id), Param::Int(user_id)])
}
pub fn token_list_own(user_id: i64, max: i64) -> Statement {
    st(
        TOKEN_LIST_OWN,
        vec![
            Param::Int(user_id),
            Param::Int(max.clamp(1, crate::repo::MAX_LIST)),
        ],
    )
}
pub fn token_revoke_own(id: &str, user_id: i64, now: &str) -> Statement {
    st(TOKEN_REVOKE_OWN, vec![t(id), Param::Int(user_id), t(now)])
}
pub fn token_count_active(user_id: i64, now: &str) -> Statement {
    st(TOKEN_COUNT_ACTIVE, vec![Param::Int(user_id), t(now)])
}
pub fn token_revoke_above_edit(workspace_id: &str, user_id: i64, now: &str) -> Statement {
    st(
        TOKEN_REVOKE_ABOVE_EDIT,
        vec![t(workspace_id), Param::Int(user_id), t(now)],
    )
}

pub fn user_get(id: i64) -> Statement {
    st(USER_GET, vec![Param::Int(id)])
}
pub fn user_by_github_id(github_id: i64) -> Statement {
    st(crate::repo::USER_BY_GITHUB_ID, vec![Param::Int(github_id)])
}
pub fn user_upsert(github_id: i64, login: &str) -> Statement {
    st(
        crate::repo::USER_UPSERT,
        vec![Param::Int(github_id), t(login)],
    )
}
pub fn user_update_login(github_id: i64, login: &str, now: &str) -> Statement {
    st(
        USER_UPDATE_LOGIN,
        vec![Param::Int(github_id), t(login), t(now)],
    )
}
pub fn user_membership_count(user_id: i64) -> Statement {
    st(USER_MEMBERSHIP_COUNT, vec![Param::Int(user_id)])
}
pub fn user_workspaces(user_id: i64, max: i64) -> Statement {
    st(
        USER_WORKSPACES,
        vec![
            Param::Int(user_id),
            Param::Int(max.clamp(1, crate::repo::MAX_LIST)),
        ],
    )
}
pub fn bootstrap_workspace(id: &str, name: &str) -> Statement {
    st(BOOTSTRAP_WORKSPACE, vec![t(id), t(name)])
}
pub fn bootstrap_owner(workspace_id: &str, github_id: i64) -> Statement {
    st(
        BOOTSTRAP_OWNER,
        vec![t(workspace_id), Param::Int(github_id)],
    )
}
pub fn members_list(workspace_id: &str, max: i64) -> Statement {
    st(
        MEMBERS_LIST,
        vec![
            t(workspace_id),
            Param::Int(max.clamp(1, crate::repo::MAX_LIST)),
        ],
    )
}
pub fn member_set_role(workspace_id: &str, user_id: i64, role: &str) -> Statement {
    st(
        MEMBER_SET_ROLE,
        vec![t(workspace_id), Param::Int(user_id), t(role)],
    )
}
pub fn member_remove(workspace_id: &str, user_id: i64) -> Statement {
    st(MEMBER_REMOVE, vec![t(workspace_id), Param::Int(user_id)])
}

/// Fields of an invitation row to insert.
pub struct NewInvitation<'a> {
    pub workspace_id: &'a str,
    pub id: &'a str,
    pub hash: &'a str,
    pub github_login: &'a str,
    pub role: &'a str,
    pub invited_by: i64,
    pub created_at: &'a str,
    pub expires_at: &'a str,
}

pub fn invite_insert_limited(n: &NewInvitation<'_>, max_pending: i64) -> Statement {
    st(
        INVITE_INSERT_LIMITED,
        vec![
            t(n.workspace_id),
            t(n.id),
            t(n.hash),
            t(n.github_login),
            t(n.role),
            Param::Int(n.invited_by),
            t(n.created_at),
            t(n.expires_at),
            Param::Int(max_pending),
        ],
    )
}
pub fn invite_list(workspace_id: &str, max: i64) -> Statement {
    st(
        INVITE_LIST,
        vec![
            t(workspace_id),
            Param::Int(max.clamp(1, crate::repo::MAX_LIST)),
        ],
    )
}
pub fn invite_revoke(workspace_id: &str, id: &str, now: &str) -> Statement {
    st(INVITE_REVOKE, vec![t(workspace_id), t(id), t(now)])
}
pub fn invite_by_id(id: &str) -> Statement {
    st(INVITE_BY_ID, vec![t(id)])
}
pub fn invite_accept(id: &str, now: &str, github_id: i64, login_lower: &str) -> Statement {
    st(
        INVITE_ACCEPT,
        vec![t(id), t(now), Param::Int(github_id), t(login_lower)],
    )
}
pub fn invite_membership(id: &str, now: &str, github_id: i64) -> Statement {
    st(
        INVITE_MEMBERSHIP,
        vec![t(id), t(now), Param::Int(github_id)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Highest `?N` placeholder in a statement.
    fn max_placeholder(sql: &str) -> usize {
        let b = sql.as_bytes();
        let mut max = 0;
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'?' {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if let Ok(n) = sql[i + 1..j].parse::<usize>() {
                    max = max.max(n);
                }
                i = j;
            } else {
                i += 1;
            }
        }
        max
    }

    #[test]
    fn builders_bind_exactly_the_placeholders() {
        let nt = NewToken {
            workspace_id: "w",
            id: "i",
            user_id: 1,
            hash: "h",
            scope: "edit",
            label: "",
            created_at: "c",
            expires_at: "e",
            chain_started_at: "c",
            replaces: None,
        };
        let ni = NewInvitation {
            workspace_id: "w",
            id: "i",
            hash: "h",
            github_login: "l",
            role: "editor",
            invited_by: 1,
            created_at: "c",
            expires_at: "e",
        };
        let all = [
            session_insert("i", 1, "h", "c", "e"),
            session_get("i"),
            session_check("i", 1),
            session_touch("i", "n"),
            session_revoke("i", 1, "n"),
            session_prune_user(1, "n", "c"),
            token_auth("i"),
            token_check("w", "i", 1),
            token_touch("i", "n"),
            token_insert_limited(&nt, 10),
            token_revoke_rotated("o", 1, "n", "i"),
            token_get_own("i", 1),
            token_list_own(1, 5),
            token_revoke_own("i", 1, "n"),
            token_count_active(1, "n"),
            token_revoke_above_edit("w", 1, "n"),
            user_get(1),
            user_by_github_id(1),
            user_upsert(1, "l"),
            user_update_login(1, "l", "n"),
            user_membership_count(1),
            user_workspaces(1, 5),
            bootstrap_workspace("w", "W"),
            bootstrap_owner("w", 1),
            members_list("w", 5),
            member_set_role("w", 1, "editor"),
            member_remove("w", 1),
            invite_insert_limited(&ni, 50),
            invite_list("w", 5),
            invite_revoke("w", "i", "n"),
            invite_by_id("i"),
            invite_accept("i", "n", 1, "l"),
            invite_membership("i", "n", 1),
        ];
        for s in &all {
            assert_eq!(s.params.len(), max_placeholder(s.sql), "{}", s.sql);
        }
        // Every named statement has a builder above (keeps ALL honest).
        for (name, sql) in ALL {
            assert!(all.iter().any(|s| s.sql == *sql), "{name} has no builder");
        }
    }

    #[test]
    fn no_statement_selects_a_raw_credential_column() {
        // The schema has no raw-credential columns at all; reads return digests
        // only where the Worker compares them.
        for (name, sql) in ALL {
            for banned in ["token_value", "cookie", "password", "access_token"] {
                assert!(!sql.contains(banned), "{name}");
            }
        }
        assert!(
            !TOKEN_LIST_OWN.contains("token_hash"),
            "listing never returns digests"
        );
        assert!(!INVITE_LIST.contains("secret_hash"));
        assert!(!MEMBERS_LIST.contains("hash"));
    }

    #[test]
    fn owner_is_protected_in_sql() {
        assert!(MEMBER_SET_ROLE.contains("role <> 'owner'"));
        assert!(MEMBER_SET_ROLE.contains("?3 IN ('publisher', 'editor')"));
        assert!(MEMBER_REMOVE.contains("role <> 'owner'"));
        assert!(BOOTSTRAP_OWNER.contains("NOT EXISTS"));
    }
}
