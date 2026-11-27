//! Shared fixtures for the schema integration tests.
#![allow(dead_code)]

use serde_json::{Value, json};
use sws_codegen::core_roots;

/// The generated schema of the root with this file stem.
pub fn schema_of(stem: &str) -> Value {
    core_roots()
        .into_iter()
        .find(|r| r.file_stem == stem)
        .map(|r| r.schema)
        .unwrap_or_else(|| panic!("no root named {stem}"))
}

/// Set (`Some`) or remove (`None`) the value at JSON pointer `ptr`.
pub fn with(base: &Value, ptr: &str, value: Option<Value>) -> Value {
    let mut doc = base.clone();
    let (parent, key) = ptr.rsplit_once('/').expect("pointer has a parent");
    let target = doc
        .pointer_mut(parent)
        .unwrap_or_else(|| panic!("parent of {ptr} missing in base"));
    match (target, value) {
        (Value::Object(m), Some(v)) => {
            m.insert(key.to_string(), v);
        }
        (Value::Object(m), None) => {
            m.remove(key);
        }
        (Value::Array(a), Some(v)) => {
            a[key.parse::<usize>().expect("index")] = v;
        }
        (Value::Array(a), None) => {
            a.remove(key.parse::<usize>().expect("index"));
        }
        _ => panic!("cannot edit {ptr}"),
    }
    doc
}

/// A fictional, schema-valid and `validate_ir`-clean document that uses every
/// node variant, route kind, prop kind and optional section, and spells out
/// every key (so each constraint has a value to attack). Written as the
/// legacy (pre-`uid`) shape: no `uid`, no `sws.ir` marker.
pub fn rich_base() -> Value {
    json!({
        "schema_version": 1,
        "slug": "example-site",
        "name": "Example Site",
        "locale": "en-US",
        "base_url": "https://example.test/",
        "tokens": {
            "colors": { "bg": "#ffffff", "fg": "#000000" },
            "fonts": { "body": "system-ui, sans-serif", "heading": "Georgia, serif", "mono": null },
            "spacing": { "s": "1rem" },
            "radii": { "r": "2px" },
            "contrast_pairs": [
                { "foreground": "fg", "background": "bg", "min_ratio": 4.5, "usage": "body" }
            ]
        },
        "breakpoints": [
            { "id": "sm", "media_query": "(min-width: 40em)", "min_width": "40em" }
        ],
        "styles": [
            {
                "id": "r1", "selector": ".c", "breakpoint": "sm", "pseudo": ":hover",
                "declarations": { "color": "red" }
            }
        ],
        "components": {
            "card": {
                "name": "card",
                "props": {
                    "title": { "kind": "string", "required": true, "default": "x", "description": "d" }
                },
                "slots": ["body"],
                "template": [
                    { "type": "element", "id": null, "tag": "article", "attrs": {}, "classes": [],
                      "style_refs": [],
                      "children": [
                        { "type": "slot_ref", "name": "body",
                          "fallback": [ { "type": "text", "value": "empty" } ] }
                      ] }
                ]
            }
        },
        "forms": {
            "contact": {
                "id": "contact",
                "action": "/api/contact",
                "method": "POST",
                "turnstile_action": "contact",
                "fields": [
                    {
                        "name": "email", "label": "Email", "kind": "email", "required": true,
                        "placeholder": null, "help_text": null, "default_value": null,
                        "options": [ { "value": "a", "label": "A" } ],
                        "max_chars": 10, "min": 1, "max": 5, "autocomplete": "email"
                    }
                ]
            }
        },
        "pages": [
            {
                "id": "index",
                "route": { "kind": "static", "path": "/" },
                "title": "Home",
                "description": null,
                "canonical": null,
                "noindex": false,
                "is_not_found": false,
                "json_ld": [ { "id": "j", "data": { "a": 1 } } ],
                "scripts": [ { "src": "/a.js", "async_load": false, "defer": true } ],
                "body": [
                    { "type": "element", "id": "top", "tag": "section",
                      "attrs": { "lang": "en" }, "classes": ["c"], "style_refs": ["r1"],
                      "children": [ { "type": "text", "value": "Hello" } ] },
                    { "type": "slot_ref", "name": "body", "fallback": [] },
                    { "type": "component_instance", "component": "card",
                      "props": { "title": "x" },
                      "slots": { "body": [ { "type": "text", "value": "in card" } ] } },
                    { "type": "conditional", "expr": "x", "then_nodes": [], "else_nodes": [] },
                    { "type": "loop", "item_var": "i", "collection_expr": "items", "body": [] }
                ]
            },
            {
                "id": "post",
                "route": { "kind": "dynamic", "pattern": "/p/{slug}", "params": ["slug"] },
                "title": "Post",
                "description": null,
                "canonical": null,
                "noindex": false,
                "is_not_found": false,
                "json_ld": [],
                "scripts": [],
                "body": []
            }
        ],
        "seo": {
            "title_template": "%s | Example", "default_description": null,
            "robots_allow": ["/"], "robots_disallow": ["/x"], "sitemap_routes": ["/"]
        },
        "scripts": [ { "src": "/site.js", "async_load": true, "defer": false } ],
        "data": { "k": [1, "two", null] },
        "metadata": { "m": { "nested": 1 } }
    })
}
