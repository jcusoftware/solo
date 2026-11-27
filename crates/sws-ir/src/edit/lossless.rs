//! Lossless-save check: refuse to save a typed document when the original JSON
//! carries fields the typed model would silently drop.

use serde_json::Value;

use super::command::{CommandError, ErrorCode};
use crate::types::SiteDocument;

const MAX_PATHS: usize = 100;

/// Paths (`$.pages[0].future`) present in `original` that are missing from, or
/// differ in, the re-serialized `doc`. Keys that exist only in the output
/// (serde defaults such as `"description": null`) are allowed; an original
/// `null` may be absent in the output; numbers compare by value, so `1.0` and
/// `1` are equivalent and object key order is not significant.
#[must_use]
pub fn lossy_paths(original: &Value, doc: &SiteDocument) -> Vec<String> {
    let Ok(output) = serde_json::to_value(doc) else {
        return vec!["$".to_string()];
    };
    let mut paths = Vec::new();
    compare(original, &output, "$", &mut paths);
    paths
}

/// `Err(lossy_document)` listing every path that would be lost.
pub fn check_lossless(original: &Value, doc: &SiteDocument) -> Result<(), CommandError> {
    let paths = lossy_paths(original, doc);
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

fn compare(original: &Value, output: &Value, path: &str, paths: &mut Vec<String>) {
    if paths.len() >= MAX_PATHS {
        return;
    }
    match (original, output) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, av) in a {
                let child = format!("{path}.{key}");
                match b.get(key) {
                    Some(bv) => compare(av, bv, &child, paths),
                    None if av.is_null() => {}
                    None => paths.push(child),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                paths.push(path.to_string());
                return;
            }
            for (idx, (av, bv)) in a.iter().zip(b).enumerate() {
                compare(av, bv, &format!("{path}[{idx}]"), paths);
            }
        }
        (Value::Number(a), Value::Number(b)) => {
            let same = a == b || matches!((a.as_f64(), b.as_f64()), (Some(x), Some(y)) if x == y);
            if !same {
                paths.push(path.to_string());
            }
        }
        (a, b) => {
            if a != b {
                paths.push(path.to_string());
            }
        }
    }
}
