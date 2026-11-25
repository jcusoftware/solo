//! Host-testable JSON error response helpers.

use serde_json::{Map, Value, json};

use crate::validate::FieldErrors;

pub fn error_body(code: &str) -> Value {
    json!({ "ok": false, "error": code })
}

pub fn validation_error_body(errs: &FieldErrors) -> Value {
    let fields: Map<String, Value> = errs
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(*v)))
        .collect();
    json!({ "ok": false, "error": "validation_failed", "fields": fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_and_validation_bodies() {
        assert_eq!(error_body("x"), json!({ "ok": false, "error": "x" }));
        let v = validation_error_body(&vec![("email".to_string(), "invalid")]);
        assert_eq!(
            v,
            json!({ "ok": false, "error": "validation_failed", "fields": { "email": "invalid" } })
        );
    }
}
