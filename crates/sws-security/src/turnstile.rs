//! Pure Turnstile siteverify verdict classification.
//!
//! Host-testable by design (AGENTS.md §3): no Workers types here, so
//! `cargo test` exercises every verdict arm on the host target and the
//! wasm-only callers in `worker_utils.rs` (and the export template) stay thin.

/// The siteverify response fields the verdict logic depends on.
///
/// `None` is passed for `outcome` when the siteverify call itself failed:
/// network error, non-2xx HTTP status, unparseable JSON, or timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteVerifyOutcome {
    pub success: bool,
    pub action: String,
    pub hostname: String,
    pub error_codes: Vec<String>,
}

/// Why a submission was rejected after siteverify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// siteverify was unreachable, errored, or returned unparseable JSON.
    Unavailable,
    /// siteverify answered but the token did not verify (`success: false`).
    Failed,
    /// Token verified but was minted for a different form action.
    ActionMismatch,
    /// Token verified but the siteverify hostname is not allowlisted.
    HostnameRejected,
}

impl RejectReason {
    /// HTTP status the API answers with for this rejection.
    pub fn status(self) -> u16 {
        match self {
            RejectReason::Unavailable => 503,
            RejectReason::Failed
            | RejectReason::ActionMismatch
            | RejectReason::HostnameRejected => 403,
        }
    }

    /// Machine-readable error code in the JSON error body.
    pub fn code(self) -> &'static str {
        match self {
            RejectReason::Unavailable => "turnstile_unavailable",
            RejectReason::Failed
            | RejectReason::ActionMismatch
            | RejectReason::HostnameRejected => "turnstile_failed",
        }
    }
}

/// Verdict for a submission after siteverify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnstileVerdict {
    /// Token verified against the expected action and (when configured)
    /// hostname; allow the submission through.
    Allow,
    /// Reject the submission for the given reason.
    Reject(RejectReason),
}

/// Classify a siteverify outcome against the submission's expectations.
///
/// - `outcome: None` means the siteverify call itself failed (HTTP error,
///   JSON parse failure, timeout) and is treated as unavailable (503).
/// - `allowed_hostnames` is the raw comma-separated `ALLOWED_HOSTNAMES` var;
///   `None` or an empty list disables the hostname defense-in-depth check.
/// - `relax_dev_checks` mirrors `DEV_MODE=true` paired with the TEST dummy
///   secret: action and hostname matching are skipped for local dev, but the
///   token still had to verify (the always-pass TEST secret accepts anything).
pub fn turnstile_verdict(
    outcome: Option<&SiteVerifyOutcome>,
    expected_action: &str,
    allowed_hostnames: Option<&str>,
    relax_dev_checks: bool,
) -> TurnstileVerdict {
    let Some(outcome) = outcome else {
        return TurnstileVerdict::Reject(RejectReason::Unavailable);
    };
    if !outcome.success {
        return TurnstileVerdict::Reject(RejectReason::Failed);
    }
    if !relax_dev_checks {
        if outcome.action != expected_action {
            return TurnstileVerdict::Reject(RejectReason::ActionMismatch);
        }
        if let Some(raw_hosts) = allowed_hostnames {
            let allowed: Vec<&str> = raw_hosts
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let hostname = outcome.hostname.trim();
            if !allowed.is_empty() && !allowed.iter().any(|h| h.eq_ignore_ascii_case(hostname)) {
                return TurnstileVerdict::Reject(RejectReason::HostnameRejected);
            }
        }
    }
    TurnstileVerdict::Allow
}

#[cfg(test)]
mod tests {
    use super::{RejectReason, SiteVerifyOutcome, TurnstileVerdict, turnstile_verdict};

    fn outcome(success: bool, action: &str, hostname: &str) -> SiteVerifyOutcome {
        SiteVerifyOutcome {
            success,
            action: action.to_string(),
            hostname: hostname.to_string(),
            error_codes: Vec::new(),
        }
    }

    #[test]
    fn siteverify_failure_outcomes_reject_unavailable() {
        // Covers network error, non-2xx HTTP status, JSON parse failure and
        // timeout: the wasm caller maps all of them to `outcome: None`.
        assert_eq!(
            turnstile_verdict(None, "inquire", None, false),
            TurnstileVerdict::Reject(RejectReason::Unavailable)
        );
        let reason = RejectReason::Unavailable;
        assert_eq!(reason.status(), 503);
        assert_eq!(reason.code(), "turnstile_unavailable");
    }

    #[test]
    fn invalid_token_rejects_with_403() {
        let o = outcome(false, "inquire", "example.com");
        assert_eq!(
            turnstile_verdict(Some(&o), "inquire", None, false),
            TurnstileVerdict::Reject(RejectReason::Failed)
        );
        assert_eq!(RejectReason::Failed.status(), 403);
        assert_eq!(RejectReason::Failed.code(), "turnstile_failed");
    }

    #[test]
    fn valid_token_with_matching_action_allows() {
        let o = outcome(true, "inquire", "example.com");
        assert_eq!(
            turnstile_verdict(Some(&o), "inquire", None, false),
            TurnstileVerdict::Allow
        );
    }

    #[test]
    fn action_mismatch_rejects_with_403() {
        let o = outcome(true, "newsletter", "example.com");
        assert_eq!(
            turnstile_verdict(Some(&o), "inquire", None, false),
            TurnstileVerdict::Reject(RejectReason::ActionMismatch)
        );
        assert_eq!(RejectReason::ActionMismatch.status(), 403);
        assert_eq!(RejectReason::ActionMismatch.code(), "turnstile_failed");
    }

    #[test]
    fn hostname_allowlist_match_allows() {
        let o = outcome(true, "inquire", "example.com");
        assert_eq!(
            turnstile_verdict(Some(&o), "inquire", Some("www.example.com, example.com"), false),
            TurnstileVerdict::Allow
        );
    }

    #[test]
    fn hostname_rejection_rejects_with_403() {
        let o = outcome(true, "inquire", "evil.example.org");
        assert_eq!(
            turnstile_verdict(Some(&o), "inquire", Some("example.com"), false),
            TurnstileVerdict::Reject(RejectReason::HostnameRejected)
        );
        assert_eq!(RejectReason::HostnameRejected.status(), 403);
        assert_eq!(RejectReason::HostnameRejected.code(), "turnstile_failed");
    }

    #[test]
    fn hostname_match_is_case_insensitive_and_trims() {
        let o = outcome(true, "inquire", "  Example.COM ");
        assert_eq!(
            turnstile_verdict(Some(&o), "inquire", Some("example.com"), false),
            TurnstileVerdict::Allow
        );
    }

    #[test]
    fn empty_or_blank_allowlist_disables_hostname_check() {
        // Matches the pre-existing caller behavior: unset or effectively empty
        // ALLOWED_HOSTNAMES leaves the check off (defense-in-depth is opt-in).
        for raw in [None, Some(""), Some(" , , ")] {
            let o = outcome(true, "inquire", "any.hostname.example");
            assert_eq!(
                turnstile_verdict(Some(&o), "inquire", raw, false),
                TurnstileVerdict::Allow
            );
        }
    }

    #[test]
    fn dev_mode_relaxation_skips_action_and_hostname_checks() {
        // DEV_MODE=true + TEST secret: verification itself still had to
        // succeed, but action/hostname mismatches no longer reject.
        let mismatched = outcome(true, "newsletter", "evil.example.org");
        assert_eq!(
            turnstile_verdict(Some(&mismatched), "inquire", Some("example.com"), true),
            TurnstileVerdict::Allow
        );
        // A failed verification is still rejected even when relaxed.
        let failed = outcome(false, "newsletter", "example.com");
        assert_eq!(
            turnstile_verdict(Some(&failed), "inquire", Some("example.com"), true),
            TurnstileVerdict::Reject(RejectReason::Failed)
        );
    }
}
