//! Credential formats, hashing, constant-time comparison and log redaction
//! (pure, host-tested). Random bytes are supplied by the caller (the Worker
//! uses `crypto.getRandomValues`), so the formats are tested exactly.
//!
//! Every credential is `lookup id` (12 random bytes, 24 lowercase hex) plus a
//! secret (32 random bytes, 64 lowercase hex). Only the SHA-256 hex digest of
//! the WHOLE credential string is stored; the id finds the row, then the digest
//! is compared in constant time. With 256 random bits a plain SHA-256 is enough
//! (no password stretching or server-side pepper is needed), so the Worker has
//! no signing key to manage.
//!
//! * CLI/MCP token: `swst_<id>_<secret>` (sent as `Authorization: Bearer`).
//! * Invitation code: `swsi_<id>_<secret>` (typed into the invitation form).
//! * Session cookie value: `<id>.<secret>`.
//! * OAuth state: `<secret>` (64 hex; stored hashed, single use).

use sws_assets::hash::{sha256_hex, to_hex};

pub const ID_BYTES: usize = 12;
pub const SECRET_BYTES: usize = 32;
/// Random bytes one credential consumes.
pub const RANDOM_BYTES: usize = ID_BYTES + SECRET_BYTES;

pub const TOKEN_PREFIX: &str = "swst_";
pub const INVITE_PREFIX: &str = "swsi_";

/// `[0-9a-f]{n}`.
fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A credential lookup id: exactly 24 lowercase hex characters.
pub fn is_lookup_id(s: &str) -> bool {
    is_hex(s, ID_BYTES * 2)
}

/// A freshly minted credential. `value` is shown to its holder once and never
/// stored or logged; `hash` is what D1 keeps.
#[derive(Clone, PartialEq, Eq)]
pub struct Minted {
    pub id: String,
    pub value: String,
    pub hash: String,
}

impl std::fmt::Debug for Minted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minted")
            .field("id", &self.id)
            .field("value", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Token,
    Invite,
    Session,
}

/// Build a credential of `kind` from `RANDOM_BYTES` random bytes.
pub fn mint(kind: Kind, random: &[u8; RANDOM_BYTES]) -> Minted {
    let id = to_hex(&random[..ID_BYTES]);
    let secret = to_hex(&random[ID_BYTES..]);
    let value = match kind {
        Kind::Token => format!("{TOKEN_PREFIX}{id}_{secret}"),
        Kind::Invite => format!("{INVITE_PREFIX}{id}_{secret}"),
        Kind::Session => format!("{id}.{secret}"),
    };
    let hash = hash(&value);
    Minted { id, value, hash }
}

/// An OAuth `state`: 64 hex characters from 32 random bytes, and its digest.
pub fn mint_state(random: &[u8; SECRET_BYTES]) -> (String, String) {
    let s = to_hex(random);
    let h = hash(&s);
    (s, h)
}

pub fn is_state(s: &str) -> bool {
    is_hex(s, SECRET_BYTES * 2)
}

/// Stored digest of a credential string.
pub fn hash(value: &str) -> String {
    sha256_hex(value.as_bytes())
}

/// Parse a presented credential of `kind`; returns its lookup id. Strict: exact
/// length, prefix and lowercase hex, so nothing odd ever reaches SQL or logs.
pub fn parse(kind: Kind, value: &str) -> Option<&str> {
    let (id, secret) = match kind {
        Kind::Token => value.strip_prefix(TOKEN_PREFIX)?.split_once('_')?,
        Kind::Invite => value.strip_prefix(INVITE_PREFIX)?.split_once('_')?,
        Kind::Session => value.split_once('.')?,
    };
    (is_lookup_id(id) && is_hex(secret, SECRET_BYTES * 2)).then_some(id)
}

/// `Authorization: Bearer <token>` -> the token. The scheme is case-insensitive
/// (RFC 7235); exactly one space-separated credential is accepted.
pub fn bearer(header: &str) -> Option<&str> {
    let (scheme, rest) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    (!token.is_empty() && !token.contains(char::is_whitespace)).then_some(token)
}

/// Constant-time equality for equal-length inputs (digest comparison). The
/// length itself is not secret (digests are always 64 bytes).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// Does `presented` hash to `stored`? Constant time in the digest comparison.
pub fn verify(presented: &str, stored_hash: &str) -> bool {
    ct_eq(hash(presented).as_bytes(), stored_hash.as_bytes())
}

/// Remove anything credential-shaped from text before it is logged or echoed:
/// `swst_`/`swsi_` credentials, session cookie values, `Bearer` credentials,
/// the two auth cookies and OAuth `code`/`state`/`access_token` parameters.
pub fn redact(text: &str) -> String {
    const MARK: &str = "[redacted]";
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    let is_tok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'~');
    while i < bytes.len() {
        let rest = &text[i..];
        let lower_rest = rest
            .get(..rest.len().min(32))
            .unwrap_or("")
            .to_ascii_lowercase();
        // Prefixed credentials.
        if rest.starts_with(TOKEN_PREFIX) || rest.starts_with(INVITE_PREFIX) {
            out.push_str(MARK);
            i += 5;
            while i < bytes.len() && is_tok(bytes[i]) {
                i += 1;
            }
            continue;
        }
        // `name=value` pairs whose value is a secret.
        let mut matched = false;
        for key in [
            "__host-sws_session=",
            "__host-sws_oauth=",
            "code=",
            "state=",
            "access_token=",
            "client_secret=",
        ] {
            let at_boundary = i == 0 || !is_tok(bytes[i - 1]);
            if at_boundary && lower_rest.starts_with(key) {
                out.push_str(&rest[..key.len()]);
                out.push_str(MARK);
                i += key.len();
                while i < bytes.len() && is_tok(bytes[i]) {
                    i += 1;
                }
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if lower_rest.starts_with("bearer ") {
            out.push_str(&rest[..7]);
            out.push_str(MARK);
            i += 7;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'"' {
                i += 1;
            }
            continue;
        }
        // A bare session cookie value `<24 hex>.<64 hex>`.
        if rest.len() >= 89
            && (i == 0 || !is_tok(bytes[i - 1]))
            && parse(Kind::Session, &rest[..89]).is_some()
            && rest.as_bytes().get(89).is_none_or(|b| !is_tok(*b))
        {
            out.push_str(MARK);
            i += 89;
            continue;
        }
        let ch = rest.chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnd(seed: u8) -> [u8; RANDOM_BYTES] {
        let mut r = [0u8; RANDOM_BYTES];
        for (i, b) in r.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u8).wrapping_mul(31);
        }
        r
    }

    #[test]
    fn minted_formats_parse_back_and_hash_the_whole_value() {
        for kind in [Kind::Token, Kind::Invite, Kind::Session] {
            let m = mint(kind, &rnd(7));
            assert_eq!(m.id.len(), 24);
            assert_eq!(parse(kind, &m.value), Some(m.id.as_str()), "{kind:?}");
            assert_eq!(m.hash, hash(&m.value));
            assert_eq!(m.hash.len(), 64);
            assert!(verify(&m.value, &m.hash));
            assert!(!m.value.contains(&m.hash));
        }
        let t = mint(Kind::Token, &rnd(1));
        assert!(t.value.starts_with("swst_") && t.value.len() == 94);
        assert_eq!(mint(Kind::Session, &rnd(1)).value.len(), 89);
        assert_ne!(
            mint(Kind::Token, &rnd(1)).value,
            mint(Kind::Token, &rnd(2)).value
        );
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let m = mint(Kind::Token, &rnd(3));
        let dbg = format!("{m:?}");
        assert!(!dbg.contains(&m.value));
        assert!(dbg.contains("[redacted]"));
    }

    #[test]
    fn parse_is_strict() {
        let good = mint(Kind::Token, &rnd(9)).value;
        assert!(parse(Kind::Token, &good).is_some());
        assert_eq!(parse(Kind::Session, &good), None);
        assert_eq!(parse(Kind::Invite, &good), None);
        let cases = [
            good.to_uppercase(),
            good.replace("swst_", "SWST_"),
            format!("{good}0"),
            good[..good.len() - 1].to_string(),
            format!(" {good}"),
            good.replacen('_', "-", 2),
            "swst__".into(),
            "".into(),
            format!("swst_{}_{}", "g".repeat(24), "0".repeat(64)),
            format!("swst_{}_{}%00", "0".repeat(24), "0".repeat(61)),
        ];
        for bad in cases {
            assert_eq!(parse(Kind::Token, &bad), None, "{bad}");
        }
        assert!(is_state(&"a".repeat(64)));
        assert!(!is_state(&"A".repeat(64)));
        assert!(!is_state(&"a".repeat(63)));
    }

    #[test]
    fn bearer_header_parsing() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer   abc  "), Some("abc"));
        assert_eq!(bearer("BEARER abc"), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("Bearer"), None);
        assert_eq!(bearer("Bearer "), None);
        assert_eq!(bearer("Bearer a b"), None);
        assert_eq!(bearer("Bearerabc"), None);
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(ct_eq(b"", b""));
        let h = hash("x");
        assert!(verify("x", &h));
        assert!(!verify("y", &h));
        assert!(!verify("x", &h.to_uppercase()));
        assert!(!verify("x", ""));
    }

    #[test]
    fn redaction_removes_every_credential_shape() {
        let tok = mint(Kind::Token, &rnd(4)).value;
        let inv = mint(Kind::Invite, &rnd(5)).value;
        let ses = mint(Kind::Session, &rnd(6)).value;
        let (state, _) = mint_state(&[7u8; 32]);
        let text = format!(
            "fetch failed: Authorization: Bearer {tok} cookie __Host-sws_session={ses}; \
             __Host-sws_oauth={state} url https://x/cb?code=abc123&state={state} \
             body access_token=gho_secret&client_secret=shh invite {inv} raw {ses} end \
             json {{\"token\":\"{tok}\"}}"
        );
        let r = redact(&text);
        for secret in [
            tok.as_str(),
            inv.as_str(),
            ses.as_str(),
            state.as_str(),
            "abc123",
            "gho_secret",
            "shh",
        ] {
            assert!(!r.contains(secret), "{secret} leaked in {r}");
        }
        assert!(r.contains("fetch failed"));
        assert!(r.contains("end"));
        // Ordinary text and digests that are not credentials survive.
        let plain = "revision ck-0123 sha256 abcdef seq=5 user 7 decode=x";
        assert_eq!(redact(plain), plain);
        assert_eq!(redact("über ✓"), "über ✓");
    }
}
