//! Session and token lifetime rules (pure, host-tested). Times are unix
//! seconds here and canonical UTC text (`YYYY-MM-DDTHH:MM:SSZ`) in D1.
//!
//! Maintainer decisions (#60): browser sessions expire after 24 h idle or 14 d
//! absolute; publish, activate, member changes and token changes need a sign-in
//! within the last 15 min. CLI/MCP tokens live at most 30 d (edit), 7 d
//! (publish) or 24 h (activate); a rotation chain lives at most 90 d; a user
//! holds at most 10 active tokens; rotation only from a fresh browser session.
//!
//! Boundaries: a lifetime of `L` seconds means valid while `age < L`; at exactly
//! `L` it has expired. "Fresh" likewise means `age < REAUTH_WINDOW_SECS`.

use crate::model::Scope;

pub const HOUR: i64 = 3600;
pub const DAY: i64 = 24 * HOUR;

pub const SESSION_IDLE_SECS: i64 = DAY;
pub const SESSION_ABSOLUTE_SECS: i64 = 14 * DAY;
pub const REAUTH_WINDOW_SECS: i64 = 15 * 60;
/// `last_seen_at` is written at most this often (bounded D1 writes). The idle
/// timeout is therefore enforced at 24 h minus up to this much, never later.
pub const SESSION_TOUCH_SECS: i64 = 5 * 60;
/// `api_tokens.last_used_at` granularity.
pub const TOKEN_TOUCH_SECS: i64 = 15 * 60;

pub const TOKEN_CHAIN_MAX_SECS: i64 = 90 * DAY;
pub const MAX_ACTIVE_TOKENS: i64 = 10;

/// Lifetime of the browser-bound OAuth state cookie (`__Host-sws_oauth`).
/// Nothing is stored server-side for a started sign-in.
pub const OAUTH_STATE_TTL_SECS: i64 = 10 * 60;
pub const INVITATION_TTL_SECS: i64 = 7 * DAY;
/// Pending invitations per workspace.
pub const MAX_PENDING_INVITATIONS: i64 = 50;

/// Lifetime of a token of `scope`.
pub fn token_lifetime(scope: Scope) -> i64 {
    match scope {
        Scope::Edit => 30 * DAY,
        Scope::Publish => 7 * DAY,
        Scope::Activate => DAY,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Canonical UTC text for unix seconds (`0` or later).
pub fn to_text(secs: i64) -> String {
    sws_assets::time::utc_text(secs.max(0) as u64)
}

/// Parse canonical UTC text back to unix seconds. Anything that does not
/// round-trip exactly (another format, an impossible date) is `None`.
pub fn parse_text(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let secs = days_from_civil(y, mo, d) * DAY + h * HOUR + mi * 60 + se;
    (to_text(secs) == s).then_some(secs)
}

/// What a stored browser session means at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Valid { fresh: bool },
    Revoked,
    IdleExpired,
    Expired,
}

/// Decide a session from its stored times. `expires_at` is the absolute end
/// (created + 14 d at insert; it is also re-derived from `created_at`, so a
/// tampered row cannot extend it).
pub fn session_status(
    created_at: i64,
    last_seen_at: i64,
    expires_at: i64,
    revoked: bool,
    now: i64,
) -> SessionStatus {
    if revoked {
        return SessionStatus::Revoked;
    }
    let absolute_end = expires_at.min(created_at.saturating_add(SESSION_ABSOLUTE_SECS));
    if now >= absolute_end || now < created_at.saturating_sub(300) {
        return SessionStatus::Expired;
    }
    if now.saturating_sub(last_seen_at) >= SESSION_IDLE_SECS {
        return SessionStatus::IdleExpired;
    }
    SessionStatus::Valid {
        fresh: now.saturating_sub(created_at) < REAUTH_WINDOW_SECS,
    }
}

/// Should `last_seen_at` be written now?
pub fn should_touch(last: Option<i64>, now: i64, every: i64) -> bool {
    last.is_none_or(|l| now.saturating_sub(l) >= every)
}

/// Is a stored token usable at `now`?
pub fn token_active(expires_at: i64, revoked: bool, now: i64) -> bool {
    !revoked && now < expires_at
}

/// Times for a new token (fresh chain: `chain_started_at = None`) or a
/// rotation (the chain start of the token being replaced).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenTimes {
    pub created_at: i64,
    pub expires_at: i64,
    pub chain_started_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// The 90-day chain is used up: create a new token instead.
    ChainExpired,
    /// Already at [`MAX_ACTIVE_TOKENS`].
    TooManyTokens,
}

impl TokenError {
    pub fn code(self) -> &'static str {
        match self {
            TokenError::ChainExpired => "token_chain_expired",
            TokenError::TooManyTokens => "too_many_tokens",
        }
    }
}

/// Plan a token's lifetime: the scope's lifetime, cut at the chain's 90-day end.
/// A chain with less than a minute left cannot be extended.
pub fn plan_token(
    scope: Scope,
    now: i64,
    chain_started_at: Option<i64>,
) -> Result<TokenTimes, TokenError> {
    let chain = chain_started_at.unwrap_or(now).min(now);
    let chain_end = chain.saturating_add(TOKEN_CHAIN_MAX_SECS);
    let expires_at = now.saturating_add(token_lifetime(scope)).min(chain_end);
    if expires_at - now < 60 {
        return Err(TokenError::ChainExpired);
    }
    Ok(TokenTimes {
        created_at: now,
        expires_at,
        chain_started_at: chain,
    })
}

/// May one more token be issued given the user's current active count?
/// (`excluding_rotated`: the token being rotated is revoked in the same batch.)
pub fn may_issue(active: i64, excluding_rotated: bool) -> Result<(), TokenError> {
    let effective = if excluding_rotated {
        active - 1
    } else {
        active
    };
    if effective >= MAX_ACTIVE_TOKENS {
        Err(TokenError::TooManyTokens)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;

    #[test]
    fn canonical_text_round_trips_and_rejects_everything_else() {
        for s in [0, 951_782_400, T0, 4_102_444_799, 1_709_164_800] {
            assert_eq!(parse_text(&to_text(s)), Some(s), "{s}");
        }
        assert_eq!(parse_text("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        for bad in [
            "2023-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-00-10T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01 00:00:00Z",
            "2026-01-01T00:00:00+00:00",
            "2026-1-01T00:00:00Z",
            "+026-01-01T00:00:00Z",
            "",
            "garbage",
            "2026-01-01T00:00:0aZ",
        ] {
            assert_eq!(parse_text(bad), None, "{bad}");
        }
    }

    #[test]
    fn session_idle_boundary_is_exactly_24_hours() {
        let c = T0;
        let abs = c + SESSION_ABSOLUTE_SECS;
        let last = c + 1000;
        assert!(matches!(
            session_status(c, last, abs, false, last + SESSION_IDLE_SECS - 1),
            SessionStatus::Valid { .. }
        ));
        assert_eq!(
            session_status(c, last, abs, false, last + SESSION_IDLE_SECS),
            SessionStatus::IdleExpired
        );
    }

    #[test]
    fn session_absolute_boundary_is_exactly_14_days() {
        let c = T0;
        let abs = c + SESSION_ABSOLUTE_SECS;
        let now = abs - 1;
        assert!(matches!(
            session_status(c, now - 10, abs, false, now),
            SessionStatus::Valid { fresh: false }
        ));
        assert_eq!(
            session_status(c, abs - 10, abs, false, abs),
            SessionStatus::Expired
        );
        // A stored expiry beyond 14 days (tampered or buggy row) is capped.
        assert_eq!(
            session_status(c, abs - 10, abs + 10 * DAY, false, abs),
            SessionStatus::Expired
        );
        // A session "created in the future" (clock skew beyond 5 min) is refused.
        assert_eq!(
            session_status(c, c, abs, false, c - 301),
            SessionStatus::Expired
        );
        assert_eq!(
            session_status(c, c, abs, true, c + 1),
            SessionStatus::Revoked
        );
    }

    #[test]
    fn reauth_window_boundary_is_exactly_15_minutes() {
        let c = T0;
        let abs = c + SESSION_ABSOLUTE_SECS;
        assert_eq!(
            session_status(c, c, abs, false, c + REAUTH_WINDOW_SECS - 1),
            SessionStatus::Valid { fresh: true }
        );
        assert_eq!(
            session_status(c, c, abs, false, c + REAUTH_WINDOW_SECS),
            SessionStatus::Valid { fresh: false }
        );
    }

    #[test]
    fn touches_are_throttled() {
        assert!(should_touch(None, T0, SESSION_TOUCH_SECS));
        assert!(!should_touch(
            Some(T0),
            T0 + SESSION_TOUCH_SECS - 1,
            SESSION_TOUCH_SECS
        ));
        assert!(should_touch(
            Some(T0),
            T0 + SESSION_TOUCH_SECS,
            SESSION_TOUCH_SECS
        ));
    }

    #[test]
    fn token_lifetimes_per_scope() {
        assert_eq!(token_lifetime(Scope::Edit), 30 * DAY);
        assert_eq!(token_lifetime(Scope::Publish), 7 * DAY);
        assert_eq!(token_lifetime(Scope::Activate), DAY);
        for s in Scope::ALL {
            let t = plan_token(*s, T0, None).unwrap();
            assert_eq!(t.expires_at - t.created_at, token_lifetime(*s));
            assert_eq!(t.chain_started_at, T0);
        }
        assert!(token_active(T0 + 1, false, T0));
        assert!(
            !token_active(T0, false, T0),
            "expires exactly at expires_at"
        );
        assert!(!token_active(T0 + 100, true, T0));
    }

    #[test]
    fn rotation_never_extends_a_chain_beyond_90_days() {
        let start = T0;
        // Day 80: an edit token would get 30 days but the chain ends at day 90.
        let t = plan_token(Scope::Edit, start + 80 * DAY, Some(start)).unwrap();
        assert_eq!(t.expires_at, start + TOKEN_CHAIN_MAX_SECS);
        assert_eq!(t.chain_started_at, start);
        // Exactly at (or past) the end: refused.
        assert_eq!(
            plan_token(Scope::Edit, start + TOKEN_CHAIN_MAX_SECS, Some(start)),
            Err(TokenError::ChainExpired)
        );
        assert_eq!(
            plan_token(
                Scope::Activate,
                start + TOKEN_CHAIN_MAX_SECS + 5,
                Some(start)
            ),
            Err(TokenError::ChainExpired)
        );
        // Under a minute left is not worth a token.
        assert_eq!(
            plan_token(Scope::Edit, start + TOKEN_CHAIN_MAX_SECS - 59, Some(start)),
            Err(TokenError::ChainExpired)
        );
        // A chain start in the future (bad row) is clamped to now, never extended.
        let t = plan_token(Scope::Edit, T0, Some(T0 + 50 * DAY)).unwrap();
        assert_eq!(t.chain_started_at, T0);
    }

    #[test]
    fn eleventh_active_token_is_refused() {
        assert_eq!(may_issue(9, false), Ok(()));
        assert_eq!(may_issue(10, false), Err(TokenError::TooManyTokens));
        // Rotation replaces one: at 10 it is still allowed, at 11 (bad state) not.
        assert_eq!(may_issue(10, true), Ok(()));
        assert_eq!(may_issue(11, true), Err(TokenError::TooManyTokens));
    }
}
