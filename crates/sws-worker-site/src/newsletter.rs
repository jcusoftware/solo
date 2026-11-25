//! Host-testable CAN-SPAM newsletter upsert statement builder for `sws-worker-site`.
//!
//! Pure logic kept out of `#[cfg(target_arch = "wasm32")]` per AGENTS.md §3: the
//! wasm-only caller in `lib.rs` consumes [`newsletter_upsert`] and binds the
//! returned values to D1, while the unit tests below pin the SQL contract.

/// One bound value for the D1 statement; `None` binds SQL `NULL`.
pub type Bind = Option<String>;

/// A D1 statement plus its positional bound values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsletterUpsert {
    /// SQL to pass to `D1Database::prepare`.
    pub sql: String,
    /// Values for binds `?1`, `?2`, `?3`, `?4` in order.
    pub binds: Vec<Bind>,
}

/// Builds the D1 statement that records a newsletter signup's CAN-SPAM consent.
///
/// CAN-SPAM hard rule (AGENTS.md §8): never re-subscribe someone marked
/// `unsubscribed` or `bounced`. The conflict arm is therefore
/// `ON CONFLICT(email) DO NOTHING` — never a `DO UPDATE` arm or any `SET` on
/// `status` — so an existing row keeps its status and its original consent
/// audit record (`consent_timestamp`, `source_route`, `consent_text`).
///
/// `consent_timestamp` is server-generated (the caller derives it from
/// `worker::Date::now()`); `?4` is `NULL` when no consent text was supplied.
pub fn newsletter_upsert(
    email: &str,
    consent_timestamp: &str,
    source_route: &str,
    consent_text: Option<&str>,
) -> NewsletterUpsert {
    let sql = "INSERT INTO newsletter_subscribers (email, status, consent_timestamp, source_route, consent_text) \
               VALUES (?1, 'subscribed', ?2, ?3, ?4) \
               ON CONFLICT(email) DO NOTHING"
        .to_string();
    NewsletterUpsert {
        sql,
        binds: vec![
            Some(email.to_string()),
            Some(consent_timestamp.to_string()),
            Some(source_route.to_string()),
            consent_text.map(str::to_string),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NewsletterUpsert {
        newsletter_upsert(
            "pat@example.test",
            "2026-11-25T12:00:00Z",
            "/menu",
            Some("I consent."),
        )
    }

    #[test]
    fn is_insert_with_conflict_do_nothing() {
        let up = sample();
        assert!(up.sql.contains("INSERT INTO newsletter_subscribers"));
        assert!(up.sql.contains("ON CONFLICT(email) DO NOTHING"));
        assert!(up.sql.contains("VALUES (?1, 'subscribed', ?2, ?3, ?4)"));
    }

    #[test]
    fn status_can_never_be_mutated() {
        // The guard regression scenario for issue #22: a refactor introducing a
        // `DO UPDATE` arm (or any `SET` on `status`) would silently re-subscribe
        // opted-out users. Neither may appear, now or in any future edit.
        let lower = sample().sql.to_lowercase();
        assert!(!lower.contains("do update"), "no DO UPDATE arm allowed");
        assert!(!lower.contains("set "), "no SET clause allowed");
        assert!(!lower.contains("update newsletter_subscribers"));
        // `status` is only the literal column list/VALUES constant, never written elsewhere.
        assert_eq!(sample().sql.matches("'subscribed'").count(), 1);
    }

    #[test]
    fn binds_store_consent_fields() {
        let up = sample();
        assert_eq!(
            up.binds,
            vec![
                Some("pat@example.test".to_string()),
                Some("2026-11-25T12:00:00Z".to_string()),
                Some("/menu".to_string()),
                Some("I consent.".to_string()),
            ]
        );
    }

    #[test]
    fn missing_consent_text_binds_null() {
        let up = newsletter_upsert("pat@example.test", "2026-11-25T12:00:00Z", "/", None);
        assert_eq!(up.binds[3], None);
        assert_eq!(up.binds.len(), 4);
    }

    #[test]
    fn deterministic_for_the_same_email() {
        let a = sample();
        let b = sample();
        assert_eq!(a, b);
        // And different inputs produce different binds, so no cross-signup bleed.
        let other = newsletter_upsert(
            "other@example.test",
            "2026-11-25T11:59:59Z",
            "/",
            Some("Other consent."),
        );
        assert_ne!(a, other);
    }
}
