//! Host-only JSON Schema (Draft 2020-12) generation helpers.
//!
//! Compiled only with the `jsonschema` feature, which is never enabled for the
//! `wasm32` / Worker builds. The schemas are derived from the serde types, so
//! they describe what the types actually (de)serialize, not a hand-kept copy.
//!
//! # Unknown-field policy
//!
//! The Rust parsers ignore unknown keys (no `deny_unknown_fields`), so
//! `solo build` / `solo check` stay tolerant. The *schema* is deliberately
//! strict: [`forbid_unknown_properties`] adds `additionalProperties: false` to
//! every struct-like object so authoring tools and editors reject typos early.
//! Free-form maps (`BTreeMap<_, _>`, `serde_json::Value`) are left open.

pub use schemars;
use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::Value;

/// Generate the Draft 2020-12 schema for `T` as a deterministic JSON value.
///
/// `id` becomes the root `$id`. With `option_is_nullable` true, `Option<T>` is
/// `T | null` (matches the IR, which serializes absent options as `null`);
/// with false, [`strip_option_null`] makes `Option<T>` just `T` (matches the
/// pack manifest, which omits absent options and never writes `null`).
#[must_use]
pub fn root_schema<T: JsonSchema>(id: &str, option_is_nullable: bool) -> Value {
    let mut value = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    if let Value::Object(map) = &mut value {
        map.insert("$id".to_string(), Value::String(id.to_string()));
    }
    if !option_is_nullable {
        strip_option_null(&mut value);
    }
    strip_numeric_formats(&mut value);
    forbid_unknown_properties(&mut value);
    value
}

/// Drop schemars' Rust-width annotations (`uint16`, `double`, ...): they are
/// not registered `format` names, so strict validators (Ajv) reject them. The
/// accompanying `minimum`/`maximum` and `type` keep the real constraint.
pub fn strip_numeric_formats(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            let numeric = match map.get("type") {
                Some(Value::String(t)) => t == "integer" || t == "number",
                Some(Value::Array(types)) => types.iter().any(|t| t == "integer" || t == "number"),
                _ => false,
            };
            if numeric {
                map.remove("format");
            }
            map.values_mut().for_each(strip_numeric_formats);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_numeric_formats),
        _ => {}
    }
}

/// Remove the `null` branch schemars adds for `Option<T>`: `type: [T, "null"]`
/// becomes `type: T` and `anyOf: [S, {type: null}]` becomes `S`. Fields that
/// must stay nullable declare it with `oneOf` (never rewritten here).
pub fn strip_option_null(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get("type")
                && types.len() == 2
                && let Some(other) = types.iter().find(|t| *t != "null").cloned()
                && types.iter().any(|t| t == "null")
            {
                map.insert("type".to_string(), other);
            }
            if map.get("default").is_some_and(Value::is_null) {
                map.remove("default");
            }
            if let Some(Value::Array(branches)) = map.get("anyOf")
                && branches.len() == 2
                && let Some(null_at) = branches
                    .iter()
                    .position(|b| b.get("type").is_some_and(|t| t == "null"))
                && let Some(Value::Object(inner)) = branches.get(1 - null_at).cloned()
            {
                map.remove("anyOf");
                for (k, v) in inner {
                    map.insert(k, v);
                }
            }
            for v in map.values_mut() {
                strip_option_null(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_option_null),
        _ => {}
    }
}

/// Add `additionalProperties: false` to every `type: object` schema that
/// declares `properties` and does not already say anything about extra keys.
pub fn forbid_unknown_properties(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            let is_object = map.get("type").is_some_and(|t| t == "object");
            if is_object
                && map.contains_key("properties")
                && !map.contains_key("additionalProperties")
            {
                map.insert("additionalProperties".to_string(), Value::Bool(false));
            }
            for v in map.values_mut() {
                forbid_unknown_properties(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(forbid_unknown_properties),
        _ => {}
    }
}
