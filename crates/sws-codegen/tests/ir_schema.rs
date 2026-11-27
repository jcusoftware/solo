//! Proof that the site-document schema generated from the Rust types is not
//! looser than the original hand-written `schema.rs` schema.
//!
//! `fixtures/site-document.schema.legacy.json` is a frozen copy of the schema
//! the hand-written generator produced before codegen (`main` at the time of
//! issue #56). The tests below compare it with the generated schema by
//! constraint inventory and by accept/reject agreement over a rich document,
//! a table of targeted edits and automatic structural mutations. The only
//! deliberate difference is the editor `uid` field (issue #55), which the
//! legacy schema could not know about.

mod common;

use std::collections::BTreeSet;

use common::{rich_base, schema_of, with};
use serde_json::{Value, json};
use sws_codegen::validate::{unresolved_refs, unsupported_keywords, validate};

const LEGACY: &str = include_str!("fixtures/site-document.schema.legacy.json");

fn legacy() -> Value {
    serde_json::from_str(LEGACY).expect("legacy schema parses")
}

fn generated() -> Value {
    schema_of("site-document")
}

fn ok(schema: &Value, doc: &Value) -> bool {
    validate(schema, doc).is_empty()
}

#[test]
fn both_schemas_use_only_validator_supported_keywords() {
    for schema in [legacy(), generated()] {
        assert_eq!(unsupported_keywords(&schema), Vec::<String>::new());
        assert_eq!(unresolved_refs(&schema), Vec::<String>::new());
    }
}

#[test]
fn schema_keeps_the_original_id_and_draft() {
    let (old, new) = (legacy(), generated());
    assert_eq!(old["$id"], new["$id"]);
    assert_eq!(old["$schema"], new["$schema"]);
}

#[test]
fn rich_base_is_valid_under_both_schemas() {
    let base = rich_base();
    assert_eq!(validate(&legacy(), &base), Vec::<String>::new());
    assert_eq!(validate(&generated(), &base), Vec::<String>::new());
}

/// Pointers of every string that the legacy schema gave `minLength: 1`.
const NON_EMPTY: &[&str] = &[
    "/name",
    "/locale",
    "/tokens/fonts/body",
    "/tokens/contrast_pairs/0/foreground",
    "/tokens/contrast_pairs/0/background",
    "/breakpoints/0/id",
    "/breakpoints/0/media_query",
    "/styles/0/id",
    "/styles/0/selector",
    "/components/card/name",
    "/components/card/template/0/tag",
    "/components/card/template/0/children/0/name",
    "/forms/contact/id",
    "/forms/contact/fields/0/name",
    "/pages/0/id",
    "/pages/0/scripts/0/src",
    "/scripts/0/src",
    "/pages/0/body/0/tag",
    "/pages/0/body/1/name",
    "/pages/0/body/2/component",
    "/pages/0/body/3/expr",
    "/pages/0/body/4/item_var",
    "/pages/0/body/4/collection_expr",
    "/pages/1/route/params/0",
];

fn targeted() -> Vec<(String, Option<Value>)> {
    let mut rows: Vec<(String, Option<Value>)> = Vec::new();
    let mut add = |ptr: &str, value: Option<Value>| rows.push((ptr.to_string(), value));
    for ptr in NON_EMPTY {
        add(ptr, Some(json!("")));
        add(ptr, Some(json!("x")));
        add(ptr, Some(json!(" ")));
        add(ptr, None);
    }
    for v in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(1),
        json!(2),
    ] {
        add("/schema_version", Some(v));
    }
    for v in ["A", "a--b", "-a", "a-", "", "a_b", "a b", "a-b", "a1", "0"] {
        add("/slug", Some(json!(v)));
    }
    for v in [
        "http://x.test",
        "https://",
        "https://a b",
        "https://x.test",
        "https://x.test/p",
        "https://x.test/p q",
        "ftp://x.test",
        "https:///x",
        "HTTPS://x.test",
        "",
    ] {
        add("/base_url", Some(json!(v)));
    }
    add("/pages", Some(json!([])));
    add("/pages", None);
    for v in [
        "#fff", "#GGGGGG", "#abcdef", "#ABCDEF", "ffffff", "#1234567", "red", "",
    ] {
        add("/tokens/colors/bg", Some(json!(v)));
        add("/tokens/colors/extra", Some(json!(v)));
    }
    for v in [
        json!(0),
        json!(-1),
        json!(0.1),
        json!(21),
        json!("4"),
        json!(null),
    ] {
        add("/tokens/contrast_pairs/0/min_ratio", Some(v));
    }
    for v in [
        json!(-1),
        json!(0),
        json!(1.5),
        json!(null),
        json!(10),
        json!("1"),
    ] {
        add("/forms/contact/fields/0/max_chars", Some(v));
    }
    for v in ["x", "/x", "", "/"] {
        add("/pages/0/route/path", Some(json!(v)));
        add("/pages/1/route/pattern", Some(json!(v)));
    }
    add("/pages/1/route/params", Some(json!([])));
    add("/pages/1/route/params", None);
    add("/pages/1/route/params", Some(json!([""])));
    add("/pages/1/route/params", Some(json!(["a", "b"])));
    add(
        "/pages/1/route",
        Some(json!({ "kind": "dynamic", "pattern": "/p" })),
    );
    add(
        "/pages/1/route",
        Some(json!({ "kind": "dynamic", "pattern": "/p", "params": ["a"] })),
    );
    add("/pages/1/route", Some(json!({ "kind": "static" })));
    add(
        "/pages/1/route",
        Some(json!({ "kind": "other", "path": "/" })),
    );
    add(
        "/pages/1/route",
        Some(json!({ "kind": "static", "path": "/", "pattern": "/x" })),
    );
    for kind in [
        "text", "textarea", "email", "phone", "date", "integer", "select", "checkbox", "hidden",
        "route", "number", "",
    ] {
        add("/forms/contact/fields/0/kind", Some(json!(kind)));
    }
    for kind in [
        "string", "number", "boolean", "array", "object", "json", "date", "",
    ] {
        add("/components/card/props/title/kind", Some(json!(kind)));
    }
    for v in [
        json!({ "type": "element" }),
        json!({ "type": "zz", "tag": "p" }),
    ] {
        add("/pages/0/body/0", Some(v));
    }
    for (ptr, v) in [
        ("/tokens/fonts/heading", json!(null)),
        ("/tokens/fonts/heading", json!("")),
        ("/tokens/fonts/mono", json!("m")),
        ("/tokens/fonts/mono", json!(1)),
        ("/pages/0/description", json!("d")),
        ("/pages/0/description", json!(1)),
        ("/forms/contact/method", json!("GET")),
        ("/forms/contact/method", json!(1)),
        ("/components/card/props/title/default", json!({ "a": 1 })),
        ("/data/new", json!(null)),
        ("/metadata/new", json!({ "a": 1 })),
    ] {
        add(ptr, Some(v));
    }
    for v in [json!("x"), json!({ "a": "b" }), json!([1])] {
        add("/pages/0/json_ld/0/data", Some(v));
    }
    add("/pages/0/json_ld/0/data", None);
    rows
}

#[test]
fn targeted_constraint_table_agrees_with_the_legacy_schema() {
    let (old, new) = (legacy(), generated());
    let base = rich_base();
    let (mut accepted, mut rejected) = (0, 0);
    for (ptr, value) in targeted() {
        let doc = with(&base, &ptr, value.clone());
        let (a, b) = (ok(&old, &doc), ok(&new, &doc));
        assert_eq!(
            a,
            b,
            "schemas disagree at {ptr} = {value:?}\nlegacy: {:?}\ngenerated: {:?}",
            validate(&old, &doc),
            validate(&new, &doc)
        );
        if a {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    println!("targeted: {accepted} accepted / {rejected} rejected by both schemas");
    assert!(
        accepted > 40 && rejected > 100,
        "{accepted} accepted / {rejected} rejected"
    );
}

fn collect_paths(value: &Value, here: &str, out: &mut Vec<String>) {
    out.push(here.to_string());
    match value {
        Value::Object(m) => {
            for (k, v) in m {
                let key = k.replace('~', "~0").replace('/', "~1");
                collect_paths(v, &format!("{here}/{key}"), out);
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                collect_paths(v, &format!("{here}/{i}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn structural_mutations_agree_with_the_legacy_schema() {
    let (old, new) = (legacy(), generated());
    let base = rich_base();
    let mut paths = Vec::new();
    collect_paths(&base, "", &mut paths);
    let swaps = [
        json!(null),
        json!(true),
        json!(-1),
        json!(0),
        json!(""),
        json!("zz"),
        json!([]),
        json!({}),
    ];
    let (mut checked, mut rejected) = (0, 0);
    for path in paths.iter().filter(|p| !p.is_empty()) {
        let mut cases = vec![with(&base, path, None)];
        cases.extend(swaps.iter().map(|s| with(&base, path, Some(s.clone()))));
        if base.pointer(path).is_some_and(Value::is_object) {
            cases.push(with(&base, &format!("{path}/zz_unknown"), Some(json!(1))));
        }
        for doc in cases {
            let (a, b) = (ok(&old, &doc), ok(&new, &doc));
            assert_eq!(
                a,
                b,
                "schemas disagree on a mutation at {path}\nlegacy: {:?}\ngenerated: {:?}",
                validate(&old, &doc),
                validate(&new, &doc)
            );
            checked += 1;
            rejected += usize::from(!a);
        }
    }
    let doc = with(&base, "/zz_unknown", Some(json!(1)));
    assert!(!ok(&old, &doc) && !ok(&new, &doc));
    println!("structural: {checked} mutations ({rejected} rejected) agree");
    assert!(
        checked > 1000 && rejected > 500,
        "{checked} checked / {rejected} rejected"
    );
}

/// Every distinct (keyword, value) a schema uses, in a comparable form.
fn constraint_inventory(schema: &Value) -> BTreeSet<String> {
    const KEYWORDS: &[&str] = &[
        "pattern",
        "enum",
        "const",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "uniqueItems",
        "format",
        "minProperties",
        "required",
        "if",
        "then",
        "not",
    ];
    fn walk(v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, child) in m {
                    // children of these keys are names (a property may itself be
                    // called `required`), not keywords
                    if matches!(k.as_str(), "properties" | "$defs" | "patternProperties") {
                        if let Value::Object(named) = child {
                            named.values().for_each(|c| walk(c, out));
                        }
                        continue;
                    }
                    if KEYWORDS.contains(&k.as_str()) {
                        let canon = match child {
                            Value::Array(a) if k == "required" || k == "enum" => {
                                let mut items: Vec<String> =
                                    a.iter().map(Value::to_string).collect();
                                items.sort();
                                format!("[{}]", items.join(","))
                            }
                            other => other.to_string(),
                        };
                        out.insert(format!("{k}={canon}"));
                    }
                    if k == "additionalProperties" && child == &Value::Bool(false) {
                        out.insert("additionalProperties=false".to_string());
                    }
                    if k == "oneOf" {
                        out.insert(format!(
                            "oneOf[{} branches]",
                            child.as_array().map_or(0, Vec::len)
                        ));
                    }
                    walk(child, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(schema, &mut out);
    out
}

/// Legacy constraints that the generated schema does not repeat verbatim.
/// Asserted exactly, so a new gap (or a closed one) forces a conscious edit.
const MISSING_FROM_GENERATED: &[&str] = &[];

/// Constraints only the generated schema has: the deliberate additions.
const ADDED_BY_GENERATED: &[&str] = &[r#"pattern="^[a-z0-9][a-z0-9._-]{0,63}$""#];

#[test]
fn every_legacy_constraint_is_present_in_the_generated_schema() {
    let old = constraint_inventory(&legacy());
    let new = constraint_inventory(&generated());
    let missing: Vec<&str> = old.difference(&new).map(String::as_str).collect();
    assert_eq!(
        missing, MISSING_FROM_GENERATED,
        "legacy constraints missing from the generated schema"
    );
    let added: Vec<&str> = new.difference(&old).map(String::as_str).collect();
    assert_eq!(
        added, ADDED_BY_GENERATED,
        "generated schema has constraints the legacy schema did not"
    );
    assert!(old.len() > 25, "inventory too small: {}", old.len());
}

#[test]
fn uid_is_the_only_deliberate_difference() {
    let (old, new) = (legacy(), generated());
    let element = json!({ "type": "element", "tag": "p", "uid": "index.p" });
    let with_uid = |node: Value| with(&rich_base(), "/pages/0/body/0", Some(node));
    assert!(!ok(&old, &with_uid(element.clone())), "legacy rejects uid");
    assert!(
        ok(&new, &with_uid(element)),
        "generated accepts a valid uid"
    );
    let variants = [
        json!({ "type": "text", "value": "v", "uid": "a" }),
        json!({ "type": "slot_ref", "name": "n", "uid": "a" }),
        json!({ "type": "component_instance", "component": "card", "uid": "a" }),
        json!({ "type": "conditional", "expr": "x", "uid": "a" }),
        json!({ "type": "loop", "item_var": "i", "collection_expr": "c", "uid": "a" }),
    ];
    for node in variants {
        assert!(ok(&new, &with_uid(node.clone())), "{node}");
        for bad in ["", "A", "-a", "a b", "a/b", "é", &"a".repeat(65)] {
            let mut n = node.clone();
            n["uid"] = json!(bad);
            assert!(!ok(&new, &with_uid(n)), "uid {bad:?} must be rejected");
        }
        let mut n = node.clone();
        n["uid"] = json!(7);
        assert!(!ok(&new, &with_uid(n)), "non-string uid must be rejected");
    }
    // boundary: 64 characters is the longest valid uid
    let mut n = json!({ "type": "text", "value": "v" });
    n["uid"] = json!("a".repeat(64));
    assert!(ok(&new, &with_uid(n)));
    // uid is never required (legacy documents have none)
    assert!(ok(&new, &rich_base()));
}
