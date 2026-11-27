//! Minimal Draft 2020-12 instance validator for the keyword subset our
//! generated schemas (and the legacy pack schema) use.
//!
//! No third-party validator crate is used: the maintained candidates
//! (`jsonschema`, `boon`) pull `MIT-0` / `Zlib` dependencies that the
//! `cargo deny` allowlist rejects. Unknown keywords are an error, never
//! silently ignored, so this validator cannot quietly accept more than the
//! schema means. `format` is annotation-only except `email` (a simple
//! shape check), matching how the schemas use it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use regex::Regex;
use serde_json::{Map, Value};

const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
];

/// Validate `instance` against `schema` (the root document, used for local
/// `$ref` resolution). Returns human-readable errors; empty means valid.
#[must_use]
pub fn validate(schema: &Value, instance: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    check(schema, schema, instance, "", &mut errors);
    errors
}

/// Return the keywords in `schema` this validator does not understand.
#[must_use]
pub fn unsupported_keywords(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_keywords(schema, "#", &mut out);
    out
}

/// Return every local `$ref` in `schema` that does not resolve.
#[must_use]
pub fn unresolved_refs(schema: &Value) -> Vec<String> {
    fn walk(root: &Value, v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(r)) = m.get("$ref")
                    && resolve(root, r).is_none()
                {
                    out.push(r.clone());
                }
                m.values().for_each(|c| walk(root, c, out));
            }
            Value::Array(a) => a.iter().for_each(|c| walk(root, c, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(schema, schema, &mut out);
    out
}

const KNOWN: &[&str] = &[
    "$ref",
    "type",
    "enum",
    "const",
    "properties",
    "required",
    "additionalProperties",
    "patternProperties",
    "minProperties",
    "items",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minLength",
    "maxLength",
    "pattern",
    "minimum",
    "maximum",
    "format",
    "oneOf",
    "anyOf",
    "allOf",
    "not",
    "if",
    "then",
    "else",
];

fn walk_keywords(schema: &Value, at: &str, out: &mut Vec<String>) {
    let Value::Object(map) = schema else { return };
    for (k, v) in map {
        let here = format!("{at}/{k}");
        if k == "$defs" || k == "properties" || k == "patternProperties" {
            if let Value::Object(children) = v {
                for (name, child) in children {
                    walk_keywords(child, &format!("{here}/{name}"), out);
                }
            }
            continue;
        }
        if !KNOWN.contains(&k.as_str()) && !ANNOTATIONS.contains(&k.as_str()) {
            out.push(here.clone());
        }
        match v {
            Value::Object(_)
                if matches!(
                    k.as_str(),
                    "items" | "additionalProperties" | "not" | "if" | "then" | "else"
                ) =>
            {
                walk_keywords(v, &here, out);
            }
            Value::Array(items) if matches!(k.as_str(), "oneOf" | "anyOf" | "allOf") => {
                for (i, child) in items.iter().enumerate() {
                    walk_keywords(child, &format!("{here}/{i}"), out);
                }
            }
            _ => {}
        }
    }
}

fn type_matches(name: &str, v: &Value) -> bool {
    match name {
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        _ => false,
    }
}

fn resolve<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        Some(root)
    } else {
        root.pointer(pointer)
    }
}

thread_local! {
    static REGEX_CACHE: RefCell<HashMap<String, Option<Rc<Regex>>>> = RefCell::new(HashMap::new());
}

fn regex_of(pattern: &str) -> Option<Rc<Regex>> {
    REGEX_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(pattern.to_string())
            .or_insert_with(|| Regex::new(pattern).ok().map(Rc::new))
            .clone()
    })
}

fn is_valid(root: &Value, schema: &Value, instance: &Value) -> bool {
    let mut errs = Vec::new();
    check(root, schema, instance, "", &mut errs);
    errs.is_empty()
}

#[allow(clippy::too_many_lines)]
fn check(root: &Value, schema: &Value, inst: &Value, at: &str, errs: &mut Vec<String>) {
    let map: &Map<String, Value> = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            errs.push(format!("{at}: schema is `false`"));
            return;
        }
        Value::Object(m) => m,
        _ => {
            errs.push(format!("{at}: schema is not an object or boolean"));
            return;
        }
    };
    let here = if at.is_empty() { "/" } else { at };
    for k in map.keys() {
        if !KNOWN.contains(&k.as_str()) && !ANNOTATIONS.contains(&k.as_str()) {
            errs.push(format!("{here}: unsupported schema keyword `{k}`"));
        }
    }
    if let Some(Value::String(r)) = map.get("$ref") {
        match resolve(root, r) {
            Some(target) => check(root, target, inst, at, errs),
            None => errs.push(format!("{here}: unresolved $ref `{r}`")),
        }
    }
    if let Some(t) = map.get("type") {
        let ok = match t {
            Value::String(s) => type_matches(s, inst),
            Value::Array(ts) => ts
                .iter()
                .filter_map(Value::as_str)
                .any(|s| type_matches(s, inst)),
            _ => false,
        };
        if !ok {
            errs.push(format!("{here}: expected type {t}"));
            return;
        }
    }
    if let Some(Value::Array(values)) = map.get("enum")
        && !values.contains(inst)
    {
        errs.push(format!("{here}: not one of the enum values"));
    }
    if let Some(c) = map.get("const")
        && c != inst
    {
        errs.push(format!("{here}: expected const {c}"));
    }
    if let Some(n) = inst.as_f64() {
        if let Some(min) = map.get("minimum").and_then(Value::as_f64)
            && n < min
        {
            errs.push(format!("{here}: {n} < minimum {min}"));
        }
        if let Some(max) = map.get("maximum").and_then(Value::as_f64)
            && n > max
        {
            errs.push(format!("{here}: {n} > maximum {max}"));
        }
    }
    if let Value::String(s) = inst {
        let len = s.chars().count() as u64;
        if let Some(min) = map.get("minLength").and_then(Value::as_u64)
            && len < min
        {
            errs.push(format!("{here}: shorter than minLength {min}"));
        }
        if let Some(max) = map.get("maxLength").and_then(Value::as_u64)
            && len > max
        {
            errs.push(format!("{here}: longer than maxLength {max}"));
        }
        if let Some(Value::String(p)) = map.get("pattern") {
            match regex_of(p) {
                Some(re) if !re.is_match(s) => {
                    errs.push(format!("{here}: does not match pattern `{p}`"));
                }
                None => errs.push(format!("{here}: unsupported regex `{p}`")),
                _ => {}
            }
        }
        if map.get("format").is_some_and(|f| f == "email") && !looks_like_email(s) {
            errs.push(format!("{here}: not an email address"));
        }
    }
    if let Value::Array(items) = inst {
        let len = items.len() as u64;
        if let Some(min) = map.get("minItems").and_then(Value::as_u64)
            && len < min
        {
            errs.push(format!("{here}: fewer than minItems {min}"));
        }
        if let Some(max) = map.get("maxItems").and_then(Value::as_u64)
            && len > max
        {
            errs.push(format!("{here}: more than maxItems {max}"));
        }
        if map.get("uniqueItems").is_some_and(|u| u == true) {
            for (i, a) in items.iter().enumerate() {
                if items[..i].contains(a) {
                    errs.push(format!("{here}: items are not unique"));
                    break;
                }
            }
        }
        if let Some(item_schema) = map.get("items") {
            for (i, item) in items.iter().enumerate() {
                check(root, item_schema, item, &format!("{at}/{i}"), errs);
            }
        }
    }
    if let Value::Object(obj) = inst {
        if let Some(Value::Array(req)) = map.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    errs.push(format!("{here}: missing required property `{r}`"));
                }
            }
        }
        if let Some(min) = map.get("minProperties").and_then(Value::as_u64)
            && (obj.len() as u64) < min
        {
            errs.push(format!("{here}: fewer than minProperties {min}"));
        }
        let props = map.get("properties").and_then(Value::as_object);
        let patterns = map.get("patternProperties").and_then(Value::as_object);
        for (key, value) in obj {
            let sub_at = format!("{at}/{key}");
            let mut matched = false;
            if let Some(sub) = props.and_then(|p| p.get(key)) {
                matched = true;
                check(root, sub, value, &sub_at, errs);
            }
            if let Some(pats) = patterns {
                for (p, sub) in pats {
                    if regex_of(p).is_some_and(|re| re.is_match(key)) {
                        matched = true;
                        check(root, sub, value, &sub_at, errs);
                    }
                }
            }
            if !matched && let Some(extra) = map.get("additionalProperties") {
                if extra == &Value::Bool(false) {
                    errs.push(format!("{here}: unknown property `{key}`"));
                } else {
                    check(root, extra, value, &sub_at, errs);
                }
            }
        }
    }
    if let Some(Value::Array(branches)) = map.get("oneOf") {
        let n = branches.iter().filter(|b| is_valid(root, b, inst)).count();
        if n != 1 {
            errs.push(format!("{here}: matches {n} oneOf branches, expected 1"));
        }
    }
    if let Some(Value::Array(branches)) = map.get("anyOf")
        && !branches.iter().any(|b| is_valid(root, b, inst))
    {
        errs.push(format!("{here}: matches no anyOf branch"));
    }
    if let Some(Value::Array(branches)) = map.get("allOf") {
        for b in branches {
            check(root, b, inst, at, errs);
        }
    }
    if let Some(not) = map.get("not")
        && is_valid(root, not, inst)
    {
        errs.push(format!("{here}: matches a `not` schema"));
    }
    if let Some(cond) = map.get("if") {
        if is_valid(root, cond, inst) {
            if let Some(then) = map.get("then") {
                check(root, then, inst, at, errs);
            }
        } else if let Some(other) = map.get("else") {
            check(root, other, inst, at, errs);
        }
    }
}

fn looks_like_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(char::is_whitespace)
        && s.matches('@').count() == 1
}
