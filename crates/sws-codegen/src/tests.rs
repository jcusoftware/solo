use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;
use serde_json::{Value, json};
use sws_ir::{
    ComponentDef, ContrastPair, FormFieldKind, FormFieldOption, FormFieldSchema, FormSchema,
    IrPatch, IrStyleRule, JsonLdBlock, Node, Page, PropKind, PropSpec, RoutePattern, ScriptRef,
    SiteDocument,
};

use super::*;
use crate::validate::{unresolved_refs, unsupported_keywords, validate};

fn repo_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn schema_of(stem: &str) -> Value {
    core_roots()
        .into_iter()
        .find(|r| r.file_stem == stem)
        .map(|r| r.schema)
        .unwrap_or(Value::Null)
}

fn el(tag: &str, children: Vec<Node>) -> Node {
    Node::Element {
        uid: None,
        id: Some(format!("{tag}-1")),
        tag: tag.to_string(),
        attrs: BTreeMap::from([("lang".to_string(), "en".to_string())]),
        classes: vec!["c".to_string()],
        style_refs: vec!["s".to_string()],
        children,
    }
}

/// A fictional document that uses every node variant, route kind, prop kind
/// and form field kind.
fn full_document() -> SiteDocument {
    let mut doc = SiteDocument {
        slug: "example-site".to_string(),
        name: "Example Site".to_string(),
        base_url: "https://example.test".to_string(),
        ..SiteDocument::default()
    };
    doc.tokens
        .colors
        .insert("bg".to_string(), "#ffffff".to_string());
    doc.tokens.fonts.heading = Some("Georgia, serif".to_string());
    doc.tokens.contrast_pairs.push(ContrastPair {
        foreground: "fg".to_string(),
        background: "bg".to_string(),
        min_ratio: 4.5,
        usage: Some("body".to_string()),
    });
    doc.styles.push(IrStyleRule {
        id: "r1".to_string(),
        selector: ".c".to_string(),
        breakpoint: Some("sm".to_string()),
        pseudo: Some(":hover".to_string()),
        declarations: BTreeMap::from([("color".to_string(), "red".to_string())]),
    });
    doc.components.insert(
        "Card".to_string(),
        ComponentDef {
            name: "Card".to_string(),
            props: BTreeMap::from([(
                "title".to_string(),
                PropSpec {
                    kind: PropKind::String,
                    required: true,
                    default: Some(json!("x")),
                    description: Some("t".to_string()),
                },
            )]),
            slots: vec!["body".to_string()],
            template: vec![Node::SlotRef {
                uid: None,
                name: "body".to_string(),
                fallback: vec![Node::Text {
                    uid: None,
                    value: "empty".to_string(),
                }],
            }],
        },
    );
    doc.forms.insert(
        "contact".to_string(),
        FormSchema {
            id: "contact".to_string(),
            action: "/api/contact".to_string(),
            method: "POST".to_string(),
            turnstile_action: "contact".to_string(),
            fields: vec![FormFieldSchema {
                name: "topic".to_string(),
                label: "Topic".to_string(),
                kind: FormFieldKind::Select,
                required: true,
                placeholder: None,
                help_text: Some("h".to_string()),
                default_value: None,
                options: vec![FormFieldOption {
                    value: "a".to_string(),
                    label: "A".to_string(),
                }],
                max_chars: Some(40),
                min: Some(1),
                max: Some(9),
                autocomplete: None,
            }],
        },
    );
    let body = vec![
        el(
            "main",
            vec![Node::Text {
                uid: None,
                value: "hi".to_string(),
            }],
        ),
        Node::ComponentInstance {
            uid: None,
            component: "Card".to_string(),
            props: BTreeMap::from([("title".to_string(), json!({"a": [1, null]}))]),
            slots: BTreeMap::from([("body".to_string(), vec![el("p", vec![])])]),
        },
        Node::Conditional {
            uid: None,
            expr: "flag".to_string(),
            then_nodes: vec![el("b", vec![])],
            else_nodes: vec![],
        },
        Node::Loop {
            uid: None,
            item_var: "item".to_string(),
            collection_expr: "items".to_string(),
            body: vec![el("li", vec![])],
        },
    ];
    doc.pages.push(Page {
        id: "home".to_string(),
        route: RoutePattern::Static {
            path: "/".to_string(),
        },
        title: "Home".to_string(),
        description: None,
        canonical: Some("https://example.test/".to_string()),
        noindex: false,
        is_not_found: false,
        json_ld: vec![JsonLdBlock {
            id: None,
            data: json!({"@type": "Thing"}),
        }],
        scripts: vec![ScriptRef {
            src: "/js/a.js".to_string(),
            async_load: true,
            defer: false,
        }],
        body: body.clone(),
    });
    doc.pages.push(Page {
        id: "post".to_string(),
        route: RoutePattern::Dynamic {
            pattern: "/posts/:slug".to_string(),
            params: vec!["slug".to_string()],
        },
        title: "Post".to_string(),
        description: Some("d".to_string()),
        canonical: None,
        noindex: true,
        is_not_found: false,
        json_ld: vec![],
        scripts: vec![],
        body,
    });
    doc.data
        .insert("k".to_string(), json!([1, "two", null, {"x": true}]));
    doc.metadata
        .insert("m".to_string(), json!({"nested": {"deep": 1}}));
    doc
}

fn site_doc_value() -> Value {
    serde_json::to_value(full_document()).unwrap_or(Value::Null)
}

#[test]
fn committed_artifacts_match_the_generator() {
    let artifacts = core_artifacts().expect("artifacts render");
    let drift = sync(repo_root(), &artifacts, Mode::Check).expect("read committed files");
    assert!(
        drift.is_empty(),
        "run `npm run codegen`; drifted: {drift:?}"
    );
}

#[test]
fn generated_schemas_are_draft_2020_12_and_self_consistent() {
    for root in core_roots() {
        let s = &root.schema;
        assert_eq!(s["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert!(s["$id"].as_str().is_some_and(|id| id.ends_with(".v1.json")));
        assert_eq!(
            unsupported_keywords(s),
            Vec::<String>::new(),
            "{}",
            root.file_stem
        );
        assert_eq!(
            unresolved_refs(s),
            Vec::<String>::new(),
            "{}",
            root.file_stem
        );
    }
    assert_eq!(schema_of("site-document")["$id"], SITE_DOCUMENT_ID);
}

#[test]
fn site_document_schema_describes_serde_reality() {
    let s = schema_of("site-document");
    // Tagged unions are `oneOf` with the tag as a const.
    let node = &s["$defs"]["Node"]["oneOf"];
    assert_eq!(node.as_array().map(Vec::len), Some(6));
    assert_eq!(node[0]["properties"]["type"]["const"], "element");
    assert_eq!(
        s["$defs"]["RoutePattern"]["oneOf"][1]["properties"]["kind"]["const"],
        "dynamic"
    );
    // `#[serde(default)]` fields are not required, except `pages`, which the
    // original hand-written schema required (with `minItems: 1`) and the
    // generated schema keeps; real requireds are.
    assert_eq!(s["required"], json!(["slug", "name", "base_url", "pages"]));
    // Options are nullable, maps are additionalProperties, Value is open.
    assert_eq!(
        s["$defs"]["Page"]["properties"]["description"]["type"],
        json!(["string", "null"])
    );
    assert_eq!(
        s["properties"]["components"]["additionalProperties"]["$ref"],
        "#/$defs/ComponentDef"
    );
    assert_eq!(s["properties"]["data"]["additionalProperties"], json!(true));
    // Strict objects.
    assert_eq!(s["additionalProperties"], json!(false));
    assert_eq!(s["$defs"]["Page"]["additionalProperties"], json!(false));
    // Enums are snake_case strings.
    assert_eq!(s["$defs"]["FormFieldKind"]["enum"][0], "text");
}

#[test]
fn serialized_ir_documents_validate_against_the_generated_schema() {
    let schema = schema_of("site-document");
    let value = site_doc_value();
    assert_eq!(validate(&schema, &value), Vec::<String>::new());
    // A minimal document with only required keys also validates,
    // and so does a legacy-shaped document that omits every defaulted key.
    let minimal = json!({
        "slug": "a", "name": "A", "base_url": "https://example.test",
        "pages": [{"id": "index", "route": {"kind": "static", "path": "/"}, "title": "Home"}]
    });
    assert_eq!(validate(&schema, &minimal), Vec::<String>::new());
    let parsed: SiteDocument = serde_json::from_value(minimal).expect("minimal parses");
    let round = serde_json::to_value(parsed).expect("serializes");
    assert_eq!(validate(&schema, &round), Vec::<String>::new());
}

fn put(doc: &Value, ptr: &str, value: Value) -> Value {
    let mut doc = doc.clone();
    let (parent, key) = ptr.rsplit_once('/').expect("pointer");
    let target = doc.pointer_mut(parent).expect("parent exists");
    match target {
        Value::Object(m) => {
            m.insert(key.to_string(), value);
        }
        Value::Array(a) => a[key.parse::<usize>().expect("index")] = value,
        _ => panic!("cannot edit {ptr}"),
    }
    doc
}

#[test]
fn unknown_fields_bad_tags_and_wrong_shapes_are_rejected_by_the_schema() {
    let schema = schema_of("site-document");
    let base = site_doc_value();
    let bad = [
        put(&base, "/x_ext", json!(1)),
        put(&base, "/tokens/future", json!(1)),
        put(&base, "/pages/0/future", json!(1)),
        put(&base, "/pages/0/route/future", json!(1)),
        put(&base, "/pages/0/body/0/unknown", json!(1)),
        put(&base, "/pages/0/body/0/type", json!("section")),
        put(&base, "/pages/0/body/1/type", json!("Component_Instance")),
        put(&base, "/pages/0/route/kind", json!("regex")),
        put(&base, "/pages/0/body/0/tag", json!(5)),
        put(&base, "/pages/0/body/1/slots", json!({"body": "not nodes"})),
        put(&base, "/schema_version", json!("1")),
        put(&base, "/forms/contact/fields/0/kind", json!("color")),
        put(&base, "/components/Card/props/title/kind", json!("int")),
        put(&base, "/pages", json!({})),
        put(&base, "/slug", json!(null)),
    ];
    for (i, doc) in bad.iter().enumerate() {
        assert!(
            !validate(&schema, doc).is_empty(),
            "mutation {i} was accepted: {doc}"
        );
    }
    // A missing required key, a missing tag and a missing variant field.
    for ptr in [
        "/slug",
        "/pages/0/id",
        "/pages/0/body/0/type",
        "/pages/0/body/0/tag",
    ] {
        let mut doc = base.clone();
        let (parent, key) = ptr.rsplit_once('/').expect("pointer");
        doc.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .map(|m| m.remove(key));
        assert!(
            !validate(&schema, &doc).is_empty(),
            "removing {ptr} was accepted"
        );
    }
}

#[test]
fn parser_is_tolerant_where_the_schema_is_strict() {
    // Policy: strict schema for authoring tools, tolerant parser for build and
    // check. Unknown keys parse (and are dropped); the schema rejects them.
    let schema = schema_of("site-document");
    let with_unknown = json!({
        "slug": "a", "name": "A", "base_url": "https://example.test",
        "x_ext": {"future": true},
        "pages": [{
            "id": "p", "title": "P", "route": {"kind": "static", "path": "/"},
            "future": 1, "body": [{"type": "text", "value": "v", "unknown": 1}]
        }]
    });
    assert!(serde_json::from_value::<SiteDocument>(with_unknown.clone()).is_ok());
    assert!(!validate(&schema, &with_unknown).is_empty());
    // Unknown enum tags are hard errors in both.
    let bad_tag = json!({
        "slug": "a", "name": "A", "base_url": "https://example.test",
        "pages": [{"id": "p", "title": "P", "route": {"kind": "static", "path": "/"},
                   "body": [{"type": "section"}]}]
    });
    assert!(serde_json::from_value::<SiteDocument>(bad_tag.clone()).is_err());
    assert!(!validate(&schema, &bad_tag).is_empty());
}

#[test]
fn every_ir_patch_op_round_trips_through_the_schema() {
    let schema = schema_of("ir-patch");
    let page = full_document().pages[0].clone();
    let patches = vec![
        IrPatch::SetSiteMeta {
            name: Some("n".into()),
            locale: None,
            base_url: None,
        },
        IrPatch::SetColorToken {
            key: "bg".into(),
            hex: "#000000".into(),
        },
        IrPatch::SetFontToken {
            slot: "body".into(),
            stack: "serif".into(),
        },
        IrPatch::UpsertPage { page },
        IrPatch::RemovePage { id: "home".into() },
        IrPatch::SetNodeText {
            page_id: "home".into(),
            node_id: "a".into(),
            text: "t".into(),
        },
        IrPatch::SetNodeAttr {
            page_id: "home".into(),
            node_id: "a".into(),
            attr: "href".into(),
            value: None,
        },
        IrPatch::InsertChildNode {
            page_id: "home".into(),
            parent_id: None,
            index: Some(0),
            node: Node::Text {
                uid: None,
                value: "x".into(),
            },
        },
        IrPatch::RemoveNode {
            page_id: "home".into(),
            node_id: "a".into(),
        },
        IrPatch::UpsertStyleRule {
            rule: IrStyleRule {
                id: "r".into(),
                selector: ".a".into(),
                breakpoint: None,
                pseudo: None,
                declarations: BTreeMap::new(),
            },
        },
        IrPatch::SetDataValue {
            key: "k".into(),
            value: json!({"any": ["thing"]}),
        },
    ];
    assert_eq!(
        schema["oneOf"].as_array().map(Vec::len),
        Some(patches.len())
    );
    for patch in &patches {
        let value = serde_json::to_value(patch).expect("serializes");
        assert_eq!(validate(&schema, &value), Vec::<String>::new(), "{value}");
    }
    for bad in [
        json!({"op": "delete_everything"}),
        json!({"op": "remove_page"}),
        json!({"op": "remove_page", "id": "a", "extra": 1}),
        json!({"op": "set_color_token", "key": "k", "hex": 5}),
        json!({"id": "a"}),
    ] {
        assert!(!validate(&schema, &bad).is_empty(), "{bad}");
    }
}

#[test]
fn declarations_cover_every_root_and_def() {
    let dts = core_artifacts()
        .expect("artifacts")
        .into_iter()
        .find(|a| a.path.ends_with("index.d.ts"))
        .expect("index.d.ts")
        .contents;
    for needle in [
        "export interface SiteDocument {",
        "export interface Page {",
        "export type Node =",
        "export type RoutePattern =",
        "export type IrPatch =",
        "export type PropKind =",
        "components?: Record<string, ComponentDef>;",
        "data?: Record<string, unknown>;",
        "description?: string | null;",
        "type: \"element\";",
        "kind: \"static\";",
        "op: \"set_site_meta\";",
    ] {
        assert!(dts.contains(needle), "index.d.ts is missing `{needle}`");
    }
    let any_type = Regex::new(r"(:|\||&|<|,|=)\s*any\b|\bany\s*(\[\]|[|&;,>])")
        .expect("regex");
    assert!(
        !any_type.is_match(&dts),
        "no `any` type in generated declarations"
    );
}

#[test]
fn package_ships_no_javascript() {
    let dir = repo_root().join(PACKAGE_DIR);
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)
            .expect("package dir readable")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.to_string_lossy().to_string();
                assert!(
                    !(name.ends_with(".js") || name.ends_with(".mjs") || name.ends_with(".cjs")),
                    "executable JS in the types package: {name}"
                );
            }
        }
    }
    let pkg: Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(PACKAGE_DIR).join("package.json"))
            .expect("package.json"),
    )
    .expect("package.json parses");
    assert!(pkg.get("main").is_none());
    assert!(pkg.get("dependencies").is_none());
    assert_eq!(pkg["license"], "MIT OR Apache-2.0");
    assert_eq!(pkg["types"], "./index.d.ts");
}

#[test]
fn validator_basics() {
    let s = json!({
        "type": "object", "required": ["a"], "additionalProperties": false,
        "properties": {"a": {"type": "integer", "minimum": 1, "maximum": 3},
                       "b": {"oneOf": [{"type": "string"}, {"type": "null"}]}}
    });
    assert!(validate(&s, &json!({"a": 2, "b": null})).is_empty());
    assert!(!validate(&s, &json!({"a": 0})).is_empty());
    assert!(!validate(&s, &json!({"a": 1.5})).is_empty());
    assert!(!validate(&s, &json!({"a": 1, "c": 1})).is_empty());
    assert!(!validate(&s, &json!({})).is_empty());
    assert_eq!(
        unsupported_keywords(&json!({"type": "string", "contentEncoding": "x"})).len(),
        1
    );
    assert!(!validate(&json!({"contentEncoding": "x"}), &json!("v")).is_empty());
}

#[test]
fn drift_is_reported_in_check_mode_and_fixed_in_write_mode() {
    let dir = std::env::temp_dir().join(format!("sws-codegen-test-{}", std::process::id()));
    let artifacts = vec![Artifact {
        path: "p/a.json".into(),
        contents: "{}\n".into(),
    }];
    assert_eq!(
        sync(&dir, &artifacts, Mode::Check).expect("check"),
        vec!["p/a.json".to_string()]
    );
    assert_eq!(
        sync(&dir, &artifacts, Mode::Write).expect("write"),
        vec!["p/a.json".to_string()]
    );
    assert!(
        sync(&dir, &artifacts, Mode::Check)
            .expect("check")
            .is_empty()
    );
    std::fs::write(dir.join("p/a.json"), "{ }\n").expect("edit");
    assert_eq!(
        sync(&dir, &artifacts, Mode::Check).expect("check"),
        vec!["p/a.json".to_string()]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_schema_files_are_reported_in_check_mode_and_deleted_in_write_mode() {
    let dir = std::env::temp_dir().join(format!("sws-codegen-stale-{}", std::process::id()));
    let artifacts = vec![Artifact {
        path: "pkg/schema/a.schema.json".into(),
        contents: "{}\n".into(),
    }];
    sync(&dir, &artifacts, Mode::Write).expect("write");
    std::fs::write(dir.join("pkg/schema/old.schema.json"), "{}\n").expect("stale");
    std::fs::write(dir.join("pkg/schema/notes.txt"), "keep").expect("other");
    assert_eq!(
        sync(&dir, &artifacts, Mode::Check).expect("check"),
        vec!["pkg/schema/old.schema.json".to_string()]
    );
    assert!(dir.join("pkg/schema/old.schema.json").exists());
    assert_eq!(
        sync(&dir, &artifacts, Mode::Write).expect("write"),
        vec!["pkg/schema/old.schema.json".to_string()]
    );
    assert!(!dir.join("pkg/schema/old.schema.json").exists());
    assert!(dir.join("pkg/schema/notes.txt").exists());
    assert!(
        sync(&dir, &artifacts, Mode::Check)
            .expect("check")
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn package_ships_both_license_texts() {
    let artifacts = core_artifacts().expect("artifacts");
    for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
        let a = artifacts
            .iter()
            .find(|a| a.path == format!("{PACKAGE_DIR}/{name}"))
            .unwrap_or_else(|| panic!("{name} is not generated"));
        assert_eq!(
            a.contents,
            std::fs::read_to_string(repo_root().join(name)).expect("root license")
        );
        let pkg = artifacts
            .iter()
            .find(|a| a.path.ends_with("package.json"))
            .expect("package.json");
        assert!(pkg.contents.contains(&format!("\"{name}\"")));
    }
}

#[test]
fn unknown_keywords_nested_in_combinators_are_rejected() {
    for wrapper in [
        json!({"not": {"type": "string", "bogus": 1}}),
        json!({"oneOf": [{"type": "string", "bogus": 1}, {"type": "null"}]}),
        json!({"anyOf": [{"type": "string", "bogus": 1}]}),
        json!({"if": {"bogus": 1}, "then": {"type": "string"}}),
    ] {
        let errs = validate(&wrapper, &json!(1));
        assert!(
            errs.iter().any(|e| e.contains("unsupported schema keyword")),
            "{wrapper}: {errs:?}"
        );
        // Also when the instance would otherwise be accepted.
        assert!(!validate(&wrapper, &json!("x")).is_empty(), "{wrapper}");
    }
}

#[test]
fn dangling_refs_and_bad_pattern_properties_regexes_are_errors() {
    assert!(!validate(&json!({"$ref": "#/$defs/missing"}), &json!(1)).is_empty());
    let schema = json!({"type": "object", "patternProperties": {"(": {"type": "string"}}});
    let errs = validate(&schema, &json!({"a": "x"}));
    assert!(errs.iter().any(|e| e.contains("unsupported regex")), "{errs:?}");
}

#[test]
fn declarations_reject_shapes_they_would_render_wrongly() {
    let root = |extra: Value| {
        let mut r = json!({"title": "Root", "type": "object", "properties": {}});
        if let (Value::Object(m), Value::Object(e)) = (&mut r, extra) {
            m.extend(e);
        }
        r
    };
    // additionalProperties: false with no properties.
    let empty_object = json!({
        "title": "Root", "type": "object",
        "properties": {"a": {"type": "object", "additionalProperties": false}}
    });
    assert!(ts::render_declarations(&[empty_object]).is_err());
    // properties together with oneOf.
    let both = root(json!({"oneOf": [{"type": "string"}]}));
    assert!(ts::render_declarations(&[both]).is_err());
    // A definition named like the utility type the renderer emits.
    let shadow = root(json!({"$defs": {"Record": {"type": "string"}}}));
    assert!(ts::render_declarations(&[shadow]).is_err());
    // The same shapes are fine when they are not wrong.
    assert!(ts::render_declarations(&[root(json!({}))]).is_ok());
}
