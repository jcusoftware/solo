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
//! * OAuth state: `<secret>` (64 hex; never stored server-side: it lives only
//!   in the authorize URL and the browser-bound `__Host-sws_oauth` cookie).

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

/// An OAuth `state`: 64 hex characters from 32 random bytes.
pub fn mint_state(random: &[u8; SECRET_BYTES]) -> String {
    to_hex(random)
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
///
/// Total on every input: matching works on bytes (every pattern is ASCII) and
/// never slices a `&str` at a byte offset, so multi-byte characters anywhere,
/// including straddling a pattern or credential length, cannot panic or hide a
/// credential. Bytes are only ever dropped in whole runs that start and end on
/// ASCII bytes (char boundaries), so the output is valid UTF-8.
pub fn redact(text: &str) -> String {
    const MARK: &[u8] = b"[redacted]";
    const KEYS: [&str; 6] = [
        "__host-sws_session=",
        "__host-sws_oauth=",
        "code=",
        "state=",
        "access_token=",
        "client_secret=",
    ];
    const BEARER: &str = "bearer ";
    const SESSION_LEN: usize = ID_BYTES * 2 + 1 + SECRET_BYTES * 2;
    let bytes = text.as_bytes();
    let is_tok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'~');
    let at_ci = |at: usize, key: &str| {
        at.checked_add(key.len())
            .and_then(|end| bytes.get(at..end))
            .is_some_and(|w| w.eq_ignore_ascii_case(key.as_bytes()))
    };
    let boundary_before = |at: usize| at == 0 || bytes.get(at - 1).is_none_or(|b| !is_tok(*b));
    let skip_tok = |mut at: usize| {
        while bytes.get(at).is_some_and(|b| is_tok(*b)) {
            at += 1;
        }
        at
    };
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Prefixed credentials (case-sensitive, like the formats themselves).
        if bytes[i..].starts_with(TOKEN_PREFIX.as_bytes())
            || bytes[i..].starts_with(INVITE_PREFIX.as_bytes())
        {
            out.extend_from_slice(MARK);
            i = skip_tok(i + TOKEN_PREFIX.len());
            continue;
        }
        // `name=value` pairs whose value is a secret.
        if boundary_before(i)
            && let Some(key) = KEYS.iter().find(|k| at_ci(i, k))
        {
            out.extend_from_slice(&bytes[i..i + key.len()]);
            out.extend_from_slice(MARK);
            i = skip_tok(i + key.len());
            continue;
        }
        if at_ci(i, BEARER) {
            out.extend_from_slice(&bytes[i..i + BEARER.len()]);
            out.extend_from_slice(MARK);
            i += BEARER.len();
            while bytes
                .get(i)
                .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'"')
            {
                i += 1;
            }
            continue;
        }
        // A bare session cookie value `<24 hex>.<64 hex>`.
        let session = boundary_before(i)
            && bytes
                .get(i..i + SESSION_LEN)
                .and_then(|w| std::str::from_utf8(w).ok())
                .is_some_and(|w| parse(Kind::Session, w).is_some())
            && bytes.get(i + SESSION_LEN).is_none_or(|b| !is_tok(*b));
        if session {
            out.extend_from_slice(MARK);
            i += SESSION_LEN;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    // Always valid UTF-8 (see above); the lossy fallback only keeps this total.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
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
        let state = mint_state(&[7u8; 32]);
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

    /// The panic repro from review: byte 89 falls inside `é`.
    #[test]
    fn redaction_never_slices_inside_a_multibyte_char() {
        let repro = format!("{}é tail", "a".repeat(88));
        assert_eq!(redact(&repro), repro);
        let ses = mint(Kind::Session, &rnd(11)).value;
        let state = mint_state(&[9u8; 32]);
        for ch in ['é', '✓', '𝄞'] {
            for k in 0..=100 {
                for fill in ['a', '0', '.'] {
                    let s = format!(
                        "{}{ch}{}",
                        fill.to_string().repeat(k),
                        fill.to_string().repeat(100 - k)
                    );
                    assert_eq!(redact(&s), s, "{ch} at {k} in {fill}");
                }
                // A char cut into a credential-shaped prefix of every length.
                let cut = k.min(ses.len());
                let s = format!("{}{ch}{}", &ses[..cut], &ses[cut..]);
                let _ = redact(&s);
                // Pattern keys followed by a value with a char at every offset:
                // the part before the char is still redacted.
                let head = &state[..k.min(state.len())];
                let s = format!("code={head}{ch} tail");
                let r = redact(&s);
                assert!(r.starts_with("code=[redacted]"), "{r}");
                assert!(r.ends_with(&format!("{ch} tail")), "{r}");
                if head.len() >= 8 {
                    assert!(!r.contains(head), "{head} leaked in {r}");
                }
            }
        }
    }

    #[test]
    fn credentials_next_to_non_ascii_are_still_redacted() {
        let tok = mint(Kind::Token, &rnd(12)).value;
        let inv = mint(Kind::Invite, &rnd(13)).value;
        let ses = mint(Kind::Session, &rnd(14)).value;
        let state = mint_state(&[3u8; 32]);
        for ch in ["é", "✓", "𝄞", "\u{a0}"] {
            let text = format!(
                "{ch}{tok}{ch} {ch}{inv}{ch} {ch}{ses}{ch} {ch}state={state}{ch} \
                 {ch}__Host-sws_oauth={state}{ch} {ch}Bearer {tok}{ch} \
                 {}{ch}code={state}",
                "x".repeat(26)
            );
            let r = redact(&text);
            for secret in [tok.as_str(), inv.as_str(), ses.as_str(), state.as_str()] {
                assert!(!r.contains(secret), "{secret} leaked in {r}");
            }
            assert!(r.contains(ch), "{r}");
        }
        // A pattern straddling byte 32 of the scan window used to be missed.
        let s = format!("{}é code={state}", "y".repeat(28));
        assert!(!redact(&s).contains(&state));
    }

    #[test]
    fn redaction_of_empty_and_long_inputs() {
        assert_eq!(redact(""), "");
        assert_eq!(redact("é"), "é");
        assert_eq!(redact("swst_"), "[redacted]");
        assert_eq!(redact("Bearer "), "Bearer [redacted]");
        let tok = mint(Kind::Token, &rnd(15)).value;
        let filler = "lorem ✓ ipsum é 𝄞 ".repeat(20_000);
        let long = format!("{filler}{tok} {filler}");
        let r = redact(&long);
        assert!(!r.contains(&tok));
        assert_eq!(r.len(), long.len() - tok.len() + "[redacted]".len());
    }

    /// Property-style: thousands of random UTF-8 strings built from
    /// credential fragments and multi-byte chars never panic, and plain
    /// (credential-free) text is returned unchanged.
    #[test]
    fn redaction_is_total_on_random_utf8() {
        let ses = mint(Kind::Session, &rnd(16)).value;
        let tok = mint(Kind::Token, &rnd(17)).value;
        let pieces: Vec<String> = vec![
            "a".into(),
            "0".into(),
            "f".into(),
            ".".into(),
            "_".into(),
            "-".into(),
            " ".into(),
            "=".into(),
            "\"".into(),
            "é".into(),
            "✓".into(),
            "𝄞".into(),
            "\u{0}".into(),
            "swst_".into(),
            "swsi_".into(),
            "code=".into(),
            "STATE=".into(),
            "Bearer ".into(),
            "__Host-sws_session=".into(),
            ses[..40].into(),
            ses.clone(),
            tok[..50].into(),
            tok.clone(),
        ];
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..5_000 {
            let n = (next() % 40) as usize;
            let s: String = (0..n)
                .map(|_| pieces[(next() % pieces.len() as u64) as usize].as_str())
                .collect();
            let r = redact(&s);
            // Prefixed tokens are redacted in every context (a bare session
            // value only at token boundaries, so it is not asserted here).
            assert!(!r.contains(&tok), "{s:?} -> {r:?}");
            // Ordinary multi-byte text is never altered.
            let plain: String = (0..n)
                .map(|_| ["a", " ", "é", "✓", "𝄞", "0", "\n"][(next() % 7) as usize])
                .collect();
            assert_eq!(redact(&plain), plain);
        }
    }
}
