//! Authentication boundary for Studio routes (#65).
//!
//! Two real credentials resolve a [`Principal`]:
//!
//! * a browser session: the host-only `__Host-sws_session` cookie
//!   ([`cookie`]), minted by the GitHub OAuth callback; and
//! * a scoped CLI/MCP token: `Authorization: Bearer swst_...` ([`secret`]),
//!   bound to one workspace and one scope (`edit` < `publish` < `activate`).
//!
//! Only SHA-256 digests of either credential are stored. What a principal may
//! do is decided by one pure function, [`policy::authorize`], used by every
//! protected HTTP route and (through [`policy::effective_role`]) by the session
//! Durable Object. Expiry maths live in [`expiry`], OAuth parsing in [`oauth`],
//! the routes in [`routes`]; the wasm glue is `crate::auth_http`.
//!
//! The `insecure-dev-auth` feature (local integration tests only, never the
//! release build) additionally accepts [`DEV_USER_HEADER`], and only behind the
//! runtime fence in `session::devfence`.

pub mod cookie;
pub mod expiry;
pub mod html;
pub mod oauth;
pub mod policy;
pub mod routes;
pub mod secret;

use crate::model::Scope;

/// Which credential authenticated the caller. Ids are the public lookup ids,
/// never secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// Browser session. `fresh` = signed in within [`expiry::REAUTH_WINDOW_SECS`].
    Session { id: String, fresh: bool },
    /// CLI/MCP token, valid only in `workspace_id`, capped at `scope`.
    Token {
        id: String,
        workspace_id: String,
        scope: Scope,
    },
    /// `insecure-dev-auth` test build only.
    Dev,
}

impl Credential {
    /// Tag stored in the session socket attachment (`s:<id>` / `t:<id>`) so the
    /// Durable Object can revalidate the credential on every recheck.
    pub fn tag(&self) -> Option<String> {
        match self {
            Credential::Session { id, .. } => Some(format!("s:{id}")),
            Credential::Token { id, .. } => Some(format!("t:{id}")),
            Credential::Dev => None,
        }
    }
}

/// A parsed credential tag (see [`Credential::tag`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialTag<'a> {
    Session(&'a str),
    Token(&'a str),
}

/// Parse an attachment credential tag; anything malformed is `None` (and the
/// Durable Object treats that as revoked).
pub fn parse_tag(tag: &str) -> Option<CredentialTag<'_>> {
    let (kind, id) = tag.split_once(':')?;
    if !secret::is_lookup_id(id) {
        return None;
    }
    match kind {
        "s" => Some(CredentialTag::Session(id)),
        "t" => Some(CredentialTag::Token(id)),
        _ => None,
    }
}

/// Identity of the caller, resolved before any protected route runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// `users.id` in the registry.
    pub user_id: i64,
    pub credential: Credential,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Unauthenticated,
}

/// Header read only by the `insecure-dev-auth` test build.
pub const DEV_USER_HEADER: &str = "X-Sws-Dev-User";

/// The dev-auth identity. `dev_user` is the value of [`DEV_USER_HEADER`]; it is
/// ignored unless the `insecure-dev-auth` feature is enabled (callers also open
/// the runtime fence first).
pub fn authenticate(dev_user: Option<&str>) -> Result<Principal, AuthError> {
    #[cfg(feature = "insecure-dev-auth")]
    {
        if let Some(user_id) = dev_user
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|id| *id > 0)
        {
            return Ok(Principal {
                user_id,
                credential: Credential::Dev,
            });
        }
    }
    let _ = dev_user;
    Err(AuthError::Unauthenticated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "insecure-dev-auth"))]
    #[test]
    fn closed_without_dev_feature() {
        assert_eq!(authenticate(Some("1")), Err(AuthError::Unauthenticated));
        assert_eq!(authenticate(None), Err(AuthError::Unauthenticated));
    }

    #[test]
    fn credential_tags_round_trip_and_reject_garbage() {
        let id = "0123456789abcdef01234567";
        let s = Credential::Session {
            id: id.into(),
            fresh: true,
        };
        let t = Credential::Token {
            id: id.into(),
            workspace_id: "ws".into(),
            scope: Scope::Edit,
        };
        assert_eq!(s.tag().as_deref(), Some("s:0123456789abcdef01234567"));
        assert_eq!(
            parse_tag(&s.tag().unwrap()),
            Some(CredentialTag::Session(id))
        );
        assert_eq!(parse_tag(&t.tag().unwrap()), Some(CredentialTag::Token(id)));
        assert_eq!(Credential::Dev.tag(), None);
        for bad in [
            "",
            "s:",
            "x:0123456789abcdef01234567",
            "s:0123456789ABCDEF01234567",
            "s:0123456789abcdef0123456",
            "s:0123456789abcdef01234567:x",
            "t:../../etc",
            "s0123456789abcdef01234567",
        ] {
            assert_eq!(parse_tag(bad), None, "{bad}");
        }
    }
}
