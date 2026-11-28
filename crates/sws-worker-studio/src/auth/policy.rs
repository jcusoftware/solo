//! The one authorization decision (pure, host-tested).
//!
//! [`authorize`] combines four facts: the caller's CURRENT role in the target
//! workspace (read from D1 by the caller, never from a client), the credential
//! (a browser session, fresh or not, or a token bound to one workspace and one
//! scope), the target workspace, and the [`Permission`] the route needs.
//!
//! Scope lattice: `edit` < `publish` < `activate`. A token can never exceed its
//! scope, its holder's role, or its workspace; an edit-only token can never
//! publish or activate. Members, invitations and tokens are managed from a
//! browser session only. Publish, activate, member and token changes need a
//! session signed in within the last 15 minutes.
//!
//! The session Durable Object applies the same rules through
//! [`effective_role`]: the role it stores per socket is already capped by the
//! socket's credential, so its existing role checks enforce token scopes too.

use crate::auth::Credential;
use crate::model::{Role, Scope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Read site state, assets, the session socket.
    Read,
    /// Edit, save, preview, upload/delete assets.
    Edit,
    Publish,
    /// Activate a deployment in production.
    Activate,
    /// Invite, change roles, remove members.
    ManageMembers,
    /// Create, rotate or revoke one's own tokens in this workspace.
    ManageTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// Not a member of the workspace, or a token for another workspace. One
    /// answer for both so workspace ids cannot be probed.
    Forbidden,
    /// Member, but the role does not allow it.
    Role,
    /// The token's scope does not allow it.
    Scope,
    /// Needs a browser session (tokens cannot manage members or tokens).
    SessionRequired,
    /// Needs a sign-in within the last 15 minutes.
    ReauthRequired,
}

impl Denied {
    pub fn status(self) -> u16 {
        match self {
            Denied::ReauthRequired => 401,
            _ => 403,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Denied::Forbidden => "forbidden",
            Denied::Role => "forbidden_role",
            Denied::Scope => "insufficient_scope",
            Denied::SessionRequired => "session_required",
            Denied::ReauthRequired => "reauth_required",
        }
    }
}

/// What a role may do (#60: owner everything; publisher edit/publish/activate;
/// editor edit and preview).
pub fn role_allows(role: Role, p: Permission) -> bool {
    match p {
        Permission::Read | Permission::Edit | Permission::ManageTokens => true,
        Permission::Publish | Permission::Activate => {
            matches!(role, Role::Owner | Role::Publisher)
        }
        Permission::ManageMembers => role == Role::Owner,
    }
}

/// What a token scope may do (the lattice).
pub fn scope_allows(scope: Scope, p: Permission) -> bool {
    match p {
        Permission::Read | Permission::Edit => true,
        Permission::Publish => matches!(scope, Scope::Publish | Scope::Activate),
        Permission::Activate => scope == Scope::Activate,
        Permission::ManageMembers | Permission::ManageTokens => false,
    }
}

/// Highest token scope a role may be issued (editors: `edit` only).
pub fn role_may_hold(role: Role, scope: Scope) -> bool {
    match scope {
        Scope::Edit => true,
        Scope::Publish | Scope::Activate => matches!(role, Role::Owner | Role::Publisher),
    }
}

fn needs_fresh_session(p: Permission) -> bool {
    matches!(
        p,
        Permission::Publish
            | Permission::Activate
            | Permission::ManageMembers
            | Permission::ManageTokens
    )
}

/// Decide. `role` is the caller's current membership in `workspace_id` (`None`
/// = not a member).
pub fn authorize(
    credential: &Credential,
    role: Option<Role>,
    workspace_id: &str,
    p: Permission,
) -> Result<(), Denied> {
    if let Credential::Token {
        workspace_id: tw, ..
    } = credential
        && tw != workspace_id
    {
        return Err(Denied::Forbidden);
    }
    let Some(role) = role else {
        return Err(Denied::Forbidden);
    };
    if !role_allows(role, p) {
        return Err(Denied::Role);
    }
    match credential {
        Credential::Token { scope, .. } => {
            if matches!(p, Permission::ManageMembers | Permission::ManageTokens) {
                return Err(Denied::SessionRequired);
            }
            if !scope_allows(*scope, p) {
                return Err(Denied::Scope);
            }
        }
        Credential::Session { fresh, .. } => {
            if needs_fresh_session(p) && !fresh {
                return Err(Denied::ReauthRequired);
            }
        }
        Credential::Dev => {}
    }
    Ok(())
}

/// The role the session Durable Object should act on for this credential: the
/// membership role, capped to `editor` when the credential cannot publish (an
/// edit-scoped token, or a browser session that is not fresh). The object's own
/// publish checks then enforce scope and re-authentication with no protocol
/// change, and its recheck recomputes this every interval.
pub fn effective_role(role: Role, credential: &Credential) -> Role {
    let can_publish = match credential {
        Credential::Token { scope, .. } => scope_allows(*scope, Permission::Publish),
        Credential::Session { fresh, .. } => *fresh,
        Credential::Dev => true,
    };
    if can_publish { role } else { Role::Editor }
}

/// CSRF gate for state-changing requests that authenticate with the session
/// cookie: the browser's `Origin` must equal the Studio's public origin. A
/// missing `Origin` is refused for cookie requests (every current browser sends
/// it on non-GET requests); token requests are not cookie-borne and skip this.
pub fn csrf_ok(origin: Option<&str>, public_origin: Option<&str>) -> bool {
    origin.is_some() && crate::session::origin_allowed(origin, public_origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "ws-a";

    fn session(fresh: bool) -> Credential {
        Credential::Session {
            id: "0".repeat(24),
            fresh,
        }
    }

    fn token(scope: Scope, ws: &str) -> Credential {
        Credential::Token {
            id: "1".repeat(24),
            workspace_id: ws.into(),
            scope,
        }
    }

    const ALL_P: [Permission; 6] = [
        Permission::Read,
        Permission::Edit,
        Permission::Publish,
        Permission::Activate,
        Permission::ManageMembers,
        Permission::ManageTokens,
    ];

    #[test]
    fn scope_lattice_is_monotone() {
        // Whatever a lower scope allows, every higher scope allows too.
        let order = [Scope::Edit, Scope::Publish, Scope::Activate];
        for p in ALL_P {
            for w in order.windows(2) {
                assert!(!scope_allows(w[0], p) || scope_allows(w[1], p), "{p:?}");
            }
        }
        assert!(scope_allows(Scope::Edit, Permission::Edit));
        assert!(!scope_allows(Scope::Edit, Permission::Publish));
        assert!(!scope_allows(Scope::Edit, Permission::Activate));
        assert!(scope_allows(Scope::Publish, Permission::Publish));
        assert!(!scope_allows(Scope::Publish, Permission::Activate));
        assert!(scope_allows(Scope::Activate, Permission::Activate));
    }

    #[test]
    fn edit_only_tokens_can_never_publish_or_activate() {
        for role in Role::ALL {
            for p in [Permission::Publish, Permission::Activate] {
                let r = authorize(&token(Scope::Edit, WS), Some(*role), WS, p);
                assert!(r.is_err(), "{role:?} {p:?}");
            }
            assert_eq!(
                effective_role(*role, &token(Scope::Edit, WS)),
                Role::Editor,
                "the session object must see an edit token as editor"
            );
        }
        assert_eq!(
            authorize(
                &token(Scope::Edit, WS),
                Some(Role::Owner),
                WS,
                Permission::Publish
            ),
            Err(Denied::Scope)
        );
    }

    #[test]
    fn roles_cap_tokens_and_sessions() {
        // An editor's publish token (from before a demotion) still cannot publish.
        assert_eq!(
            authorize(
                &token(Scope::Activate, WS),
                Some(Role::Editor),
                WS,
                Permission::Publish
            ),
            Err(Denied::Role)
        );
        assert_eq!(
            authorize(&session(true), Some(Role::Editor), WS, Permission::Publish),
            Err(Denied::Role)
        );
        assert_eq!(
            authorize(
                &session(true),
                Some(Role::Publisher),
                WS,
                Permission::ManageMembers
            ),
            Err(Denied::Role)
        );
        assert_eq!(
            authorize(
                &session(true),
                Some(Role::Owner),
                WS,
                Permission::ManageMembers
            ),
            Ok(())
        );
        assert_eq!(
            authorize(
                &token(Scope::Publish, WS),
                Some(Role::Publisher),
                WS,
                Permission::Publish
            ),
            Ok(())
        );
        assert_eq!(
            authorize(
                &token(Scope::Activate, WS),
                Some(Role::Owner),
                WS,
                Permission::Activate
            ),
            Ok(())
        );
        assert!(role_may_hold(Role::Editor, Scope::Edit));
        assert!(!role_may_hold(Role::Editor, Scope::Publish));
        assert!(!role_may_hold(Role::Editor, Scope::Activate));
        assert!(role_may_hold(Role::Publisher, Scope::Activate));
    }

    #[test]
    fn tenant_denial() {
        // A token minted in workspace B is refused in A, even for a member of A.
        for p in ALL_P {
            assert_eq!(
                authorize(&token(Scope::Activate, "ws-b"), Some(Role::Owner), WS, p),
                Err(Denied::Forbidden),
                "{p:?}"
            );
            assert_eq!(
                authorize(&session(true), None, WS, p),
                Err(Denied::Forbidden)
            );
        }
    }

    #[test]
    fn management_needs_a_fresh_browser_session() {
        for p in [Permission::ManageMembers, Permission::ManageTokens] {
            assert_eq!(
                authorize(&token(Scope::Activate, WS), Some(Role::Owner), WS, p),
                Err(Denied::SessionRequired)
            );
            assert_eq!(
                authorize(&session(false), Some(Role::Owner), WS, p),
                Err(Denied::ReauthRequired)
            );
            assert_eq!(authorize(&session(true), Some(Role::Owner), WS, p), Ok(()));
        }
        for p in [Permission::Publish, Permission::Activate] {
            assert_eq!(
                authorize(&session(false), Some(Role::Publisher), WS, p),
                Err(Denied::ReauthRequired)
            );
        }
        // Reads and edits never need re-authentication.
        assert_eq!(
            authorize(&session(false), Some(Role::Editor), WS, Permission::Edit),
            Ok(())
        );
        assert_eq!(effective_role(Role::Owner, &session(false)), Role::Editor);
        assert_eq!(effective_role(Role::Owner, &session(true)), Role::Owner);
        assert_eq!(
            effective_role(Role::Publisher, &token(Scope::Publish, WS)),
            Role::Publisher
        );
        assert_eq!(Denied::ReauthRequired.status(), 401);
        assert_eq!(Denied::Scope.status(), 403);
    }

    #[test]
    fn csrf_needs_the_public_origin() {
        let p = Some("https://studio.example");
        assert!(csrf_ok(Some("https://studio.example"), p));
        assert!(
            !csrf_ok(None, p),
            "cookie requests without Origin are refused"
        );
        assert!(!csrf_ok(Some("https://evil.example"), p));
        assert!(!csrf_ok(Some("null"), p));
        assert!(!csrf_ok(Some("https://studio.example"), None));
    }
}
