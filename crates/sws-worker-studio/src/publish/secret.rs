//! The Cloudflare API token and the per-session upload JWTs are bearer secrets.
//!
//! [`ApiToken`] never prints its value (`Debug` is redacted, there is no `Display`), and
//! [`Redactor`] scrubs every secret seen during a run from any text that could reach an
//! error, a log line or the audit table (an upstream error body could echo a header).

use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken(String);

impl ApiToken {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The raw value, for the `Authorization` header only.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

pub const REDACTED: &str = "<redacted>";

/// Longest upstream text kept in an error message (after redaction).
pub const MAX_MESSAGE_CHARS: usize = 200;

/// Every secret value in play for one publish or rollback.
#[derive(Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new(token: &ApiToken) -> Self {
        let mut r = Self::default();
        r.add(token.expose());
        r
    }

    /// Register another secret (an upload JWT or completion token as it is issued).
    pub fn add(&mut self, secret: &str) {
        // Very short values cannot be secrets worth matching and would mangle text.
        if secret.len() >= 8 && !self.secrets.iter().any(|s| s == secret) {
            self.secrets.push(secret.to_string());
        }
    }

    /// Scrub every registered secret and anything that looks like a bearer
    /// credential, then cap the length.
    pub fn clean(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in &self.secrets {
            out = out.replace(s.as_str(), REDACTED);
        }
        out = scrub_bearer(&out);
        if out.chars().count() > MAX_MESSAGE_CHARS {
            out = out.chars().take(MAX_MESSAGE_CHARS).collect::<String>() + "...";
        }
        out
    }
}

/// Replace the word after any `Bearer ` (case-insensitive) with the redaction marker,
/// so even a secret the redactor never saw cannot pass through an echoed header.
fn scrub_bearer(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(p) = lower[i..].find("bearer ") {
        let start = i + p + "bearer ".len();
        out.push_str(&text[i..start]);
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || c == '"' || c == ',' || c == ')')
            .map_or(text.len(), |e| start + e);
        if end > start {
            out.push_str(REDACTED);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let t = ApiToken::new("cfat_abcdefghijkl".into());
        assert_eq!(format!("{t:?}"), "ApiToken(<redacted>)");
        assert_eq!(t.expose(), "cfat_abcdefghijkl");
    }

    #[test]
    fn clean_removes_known_secrets_and_bearer_values() {
        let mut r = Redactor::new(&ApiToken::new("cfat_abcdefghijkl".into()));
        r.add("upload-jwt-xyz123");
        let s = r.clean("bad cfat_abcdefghijkl and upload-jwt-xyz123; Authorization: Bearer other-secret-value, done");
        assert!(!s.contains("cfat_"), "{s}");
        assert!(!s.contains("xyz123"), "{s}");
        assert!(!s.contains("other-secret-value"), "{s}");
        assert!(s.contains("done"));
        assert!(r.clean(&"x".repeat(500)).len() <= MAX_MESSAGE_CHARS + 3);
    }
}
