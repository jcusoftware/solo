//! Lossless-save check: refuse to save a typed document when the original JSON
//! carries fields the typed model would silently drop.

use serde_json::Value;

use super::command::{CommandError, ErrorCode};
use crate::types::SiteDocument;

const MAX_PATHS: usize = 100;

/// Paths (`$.pages[0].future`) present in `original` that are missing from, or
/// differ in, the re-serialized `doc`. Keys that exist only in the output
/// (serde defaults such as `"description": null`) are allowed; an original
/// `null` may be absent in the output only when the key is a known optional
/// field (an unknown key holding `null` is dropped, so it is reported); numbers
/// compare by value, so `1.0` and `1` are equivalent, and object key order is
/// not significant.
#[must_use]
pub fn lossy_paths(original: &Value, doc: &SiteDocument) -> Vec<String> {
    let Ok(output) = serde_json::to_value(doc) else {
        return vec!["$".to_string()];
    };
    lossy_value_paths(original, &output, &|candidate| {
        serde_json::from_value::<SiteDocument>(candidate.clone())
            .ok()
            .and_then(|d| serde_json::to_value(d).ok())
    })
}

/// Generic form of [`lossy_paths`]: `output` is the typed re-serialization of
/// `original`; `reserialize` parses a candidate JSON value with the same typed
/// model and re-serializes it (`None` when the typed parse fails). It is used to
/// tell a known optional `null` from an unknown, silently dropped one.
#[must_use]
pub fn lossy_value_paths(
    original: &Value,
    output: &Value,
    reserialize: &dyn Fn(&Value) -> Option<Value>,
) -> Vec<String> {
    let mut ctx = Ctx {
        root: original,
        reserialize,
        probes_left: MAX_NULL_PROBES,
        trail: Vec::new(),
        paths: Vec::new(),
    };
    compare(original, output, "$", &mut ctx);
    ctx.paths
}

/// Problems only visible in the source text, because both JSON parses agree on
/// them: duplicate object keys (the last value silently wins), integers outside
/// the `i64`/`u64` range and floats whose decimal spelling carries more
/// precision than an `f64` keeps. Returns `path (reason)` entries; the text must
/// already be known to be valid JSON.
#[must_use]
pub fn source_issues(text: &str) -> Vec<String> {
    let mut scanner = Scanner {
        bytes: text.as_bytes(),
        text,
        pos: 0,
        out: Vec::new(),
    };
    scanner.value("$");
    scanner.out
}

/// `Err(lossy_document)` listing every path that would be lost.
pub fn check_lossless(original: &Value, doc: &SiteDocument) -> Result<(), CommandError> {
    let paths = lossy_paths(original, doc);
    lossy_error(paths)
}

/// `Err(lossy_document)` for a non-empty list of lost paths.
pub fn lossy_error(paths: Vec<String>) -> Result<(), CommandError> {
    if paths.is_empty() {
        return Ok(());
    }
    Err(CommandError::new(
        ErrorCode::LossyDocument,
        format!(
            "{} field(s) in the source would be dropped or changed by saving the typed document",
            paths.len()
        ),
    )
    .with_paths(paths))
}

const MAX_NULL_PROBES: usize = 256;
const PROBE_MARKER: &str = "\u{1}sws-null-probe\u{1}";

enum Seg {
    Key(String),
    Index(usize),
}

struct Ctx<'a> {
    root: &'a Value,
    reserialize: &'a dyn Fn(&Value) -> Option<Value>,
    probes_left: usize,
    trail: Vec<Seg>,
    paths: Vec<String>,
}

impl Ctx<'_> {
    /// True when an original `null` at the current trail is a known optional
    /// field (the typed model accepts it); false when the key is unknown (it is
    /// dropped on save) or the probe budget is exhausted.
    fn null_is_known(&mut self) -> bool {
        if self.probes_left == 0 {
            return false;
        }
        self.probes_left -= 1;
        let mut candidate = self.root.clone();
        if set_at(
            &mut candidate,
            &self.trail,
            Value::String(PROBE_MARKER.into()),
        )
        .is_none()
        {
            return false;
        }
        match (self.reserialize)(&candidate) {
            // A typed parse failure means the field exists with another type.
            None => true,
            Some(out) => get_at(&out, &self.trail).is_some_and(|v| v == PROBE_MARKER),
        }
    }
}

fn set_at(root: &mut Value, trail: &[Seg], value: Value) -> Option<()> {
    let mut cur = root;
    for seg in trail {
        cur = match seg {
            Seg::Key(k) => cur.get_mut(k.as_str())?,
            Seg::Index(i) => cur.get_mut(*i)?,
        };
    }
    *cur = value;
    Some(())
}

fn get_at<'a>(root: &'a Value, trail: &[Seg]) -> Option<&'a Value> {
    let mut cur = root;
    for seg in trail {
        cur = match seg {
            Seg::Key(k) => cur.get(k.as_str())?,
            Seg::Index(i) => cur.get(*i)?,
        };
    }
    Some(cur)
}

fn compare(original: &Value, output: &Value, path: &str, ctx: &mut Ctx<'_>) {
    if ctx.paths.len() >= MAX_PATHS {
        return;
    }
    match (original, output) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, av) in a {
                let child = format!("{path}.{key}");
                ctx.trail.push(Seg::Key(key.clone()));
                match b.get(key) {
                    Some(bv) => compare(av, bv, &child, ctx),
                    None if av.is_null() && ctx.null_is_known() => {}
                    None => ctx.paths.push(child),
                }
                ctx.trail.pop();
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                ctx.paths.push(path.to_string());
                return;
            }
            for (idx, (av, bv)) in a.iter().zip(b).enumerate() {
                ctx.trail.push(Seg::Index(idx));
                compare(av, bv, &format!("{path}[{idx}]"), ctx);
                ctx.trail.pop();
            }
        }
        (Value::Number(a), Value::Number(b)) => {
            let same = a == b || matches!((a.as_f64(), b.as_f64()), (Some(x), Some(y)) if x == y);
            if !same {
                ctx.paths.push(path.to_string());
            }
        }
        (a, b) => {
            if a != b {
                ctx.paths.push(path.to_string());
            }
        }
    }
}

/// Minimal JSON text walker (input already parsed successfully, so nesting is
/// bounded by the parser's recursion limit).
struct Scanner<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
    out: Vec<String>,
}

impl Scanner<'_> {
    fn note(&mut self, entry: String) {
        if self.out.len() < MAX_PATHS {
            self.out.push(entry);
        }
    }

    fn skip_ws(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }

    /// Consume a string literal and return its decoded value.
    fn string(&mut self) -> String {
        let start = self.pos;
        self.pos += 1;
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b'\\' => self.pos += 2,
                b'"' => {
                    self.pos += 1;
                    break;
                }
                _ => self.pos += 1,
            }
        }
        let end = self.pos.min(self.text.len());
        serde_json::from_str::<String>(&self.text[start.min(end)..end]).unwrap_or_default()
    }

    fn value(&mut self, path: &str) {
        self.skip_ws();
        match self.bytes.get(self.pos) {
            Some(b'{') => {
                self.pos += 1;
                let mut seen = std::collections::BTreeSet::new();
                loop {
                    self.skip_ws();
                    match self.bytes.get(self.pos) {
                        Some(b'}') | None => {
                            self.pos += 1;
                            break;
                        }
                        Some(b',') => self.pos += 1,
                        Some(b'"') => {
                            let key = self.string();
                            self.skip_ws();
                            if self.bytes.get(self.pos) == Some(&b':') {
                                self.pos += 1;
                            }
                            let child = format!("{path}.{key}");
                            if !seen.insert(key) {
                                self.note(format!("{child} (duplicate key)"));
                            }
                            self.value(&child);
                        }
                        Some(_) => self.pos += 1,
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                let mut idx = 0;
                loop {
                    self.skip_ws();
                    match self.bytes.get(self.pos) {
                        Some(b']') | None => {
                            self.pos += 1;
                            break;
                        }
                        Some(b',') => self.pos += 1,
                        Some(_) => {
                            self.value(&format!("{path}[{idx}]"));
                            idx += 1;
                        }
                    }
                }
            }
            Some(b'"') => {
                self.string();
            }
            Some(_) => {
                let start = self.pos;
                while self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|b| !matches!(b, b',' | b']' | b'}') && !b.is_ascii_whitespace())
                {
                    self.pos += 1;
                }
                let token = &self.text[start..self.pos];
                if token.starts_with(|c: char| c == '-' || c.is_ascii_digit())
                    && !number_is_exact(token)
                {
                    self.note(format!("{path} (number {token} loses precision)"));
                }
            }
            None => {}
        }
    }
}

/// True when `token` survives parsing into `i64`/`u64`/`f64` without changing
/// its decimal value (spelling such as `1.0` or `1e3` is not a loss).
fn number_is_exact(token: &str) -> bool {
    if !token.contains(['.', 'e', 'E']) {
        return token.parse::<i64>().is_ok() || token.parse::<u64>().is_ok();
    }
    let Ok(parsed) = token.parse::<f64>() else {
        return false;
    };
    if !parsed.is_finite() {
        return false;
    }
    match (
        normalize_decimal(token),
        normalize_decimal(&parsed.to_string()),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// `(digits without leading/trailing zeros, power of ten)`; zero is `("", 0)`.
fn normalize_decimal(token: &str) -> Option<(String, i64)> {
    let unsigned = token.strip_prefix('-').unwrap_or(token);
    let (mantissa, exp) = match unsigned.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().ok()?),
        None => (unsigned, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{int_part}{frac_part}");
    let mut exp10 = exp.checked_sub(i64::try_from(frac_part.len()).ok()?)?;
    let trimmed = digits.trim_start_matches('0').len();
    digits.drain(..digits.len() - trimmed);
    while digits.ends_with('0') {
        digits.pop();
        exp10 = exp10.checked_add(1)?;
    }
    if digits.is_empty() {
        exp10 = 0;
    }
    Some((digits, exp10))
}
