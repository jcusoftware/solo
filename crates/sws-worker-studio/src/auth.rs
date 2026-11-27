//! Authentication boundary for Studio routes.
//!
//! Real authentication (GitHub OAuth sessions and scoped CLI/MCP tokens) is #65.
//! Until then every protected route is closed: [`authenticate`] returns
//! [`AuthError::Unauthenticated`] unless the crate is built with the
//! `insecure-dev-auth` feature, which exists only for local integration tests
//! and is never enabled by the release build or CI's release build.

/// Identity of the caller, resolved before any protected route runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// `users.id` in the registry.
    pub user_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Unauthenticated,
}

/// Header read only by the `insecure-dev-auth` test build.
pub const DEV_USER_HEADER: &str = "X-Sws-Dev-User";

/// Resolve the caller. `dev_user` is the value of [`DEV_USER_HEADER`], if any;
/// it is ignored unless the `insecure-dev-auth` feature is enabled.
pub fn authenticate(dev_user: Option<&str>) -> Result<Principal, AuthError> {
    #[cfg(feature = "insecure-dev-auth")]
    {
        if let Some(user_id) = dev_user
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|id| *id > 0)
        {
            return Ok(Principal { user_id });
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
}
