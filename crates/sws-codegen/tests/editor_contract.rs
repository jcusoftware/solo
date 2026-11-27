//! Conformance of the generated editor-contract schemas (issue #55) with the
//! values the real Rust code produces and accepts.

mod common;

use std::collections::BTreeSet;

use common::{rich_base, schema_of};
use serde_json::{Value, json};
use sws_codegen::validate::validate;
use sws_ir::SiteDocument;
use sws_ir::edit::{
    CommandBatch, CommandError, ErrorCode, apply_batch, assign_stable_ids, parse_batch,
};

fn errors(stem: &str, instance: &Value) -> Vec<String> {
    validate(&schema_of(stem), instance)
}

fn assert_valid(stem: &str, instance: &Value) {
    let errs = errors(stem, instance);
    assert!(errs.is_empty(), "{stem}: {errs:?}\n{instance:#}");
}

fn assert_rejected(stem: &str, instance: &Value, why: &str) {
    assert!(
        !errors(stem, instance).is_empty(),
        "{stem} must reject {why}:\n{instance:#}"
    );
}

fn legacy_document() -> SiteDocument {
    serde_json::from_value(rich_base()).expect("rich base parses")
}

fn upgraded() -> (SiteDocument, Value) {
    let (doc, report) = assign_stable_ids(&legacy_document()).expect("upgrade succeeds");
    (
        doc,
        serde_json::to_value(report).expect("report serializes"),
    )
}

macro_rules! to_value {
    ($v:expr) => {
        serde_json::to_value($v).expect("serializes")
    };
}

#[test]
fn upgraded_documents_validate_against_the_site_document_schema() {
    let (doc, report) = upgraded();
    let value = to_value!(&doc);
    assert_valid("site-document", &value);
    assert_eq!(value["metadata"]["sws.ir"], json!({ "uid_scheme": 1 }));
    // every node of the page body carries a uid, and the report matches
    for i in 0..5 {
        let uid = value.pointer(&format!("/pages/0/body/{i}/uid"));
        assert!(
            uid.is_some_and(Value::is_string),
            "body node {i} lacks a uid"
        );
    }
    assert_valid("upgrade-report", &report);
    // the legacy (pre-upgrade) form stays valid too: uid is optional
    assert_valid("site-document", &rich_base());
    // upgrading is idempotent and its output stays valid
    let (again, again_report) = assign_stable_ids(&doc).expect("idempotent");
    assert_eq!(again, doc);
    assert_valid("upgrade-report", &to_value!(&again_report));
}

#[test]
fn upgrade_report_with_reassigned_ids_validates() {
    let mut doc = rich_base();
    doc["metadata"]["sws.ir"] = json!({ "uid_scheme": 1 });
    doc["pages"][0]["body"][0]["uid"] = json!("dup");
    doc["pages"][0]["body"][1]["uid"] = json!("dup");
    doc["pages"][0]["body"][3]["uid"] = json!("Bad Uid");
    // `uid` is a typed `String`, so an invalid one still parses
    let parsed: SiteDocument = serde_json::from_value(doc).expect("parses");
    let (_, report) = assign_stable_ids(&parsed).expect("upgrade succeeds");
    let value = to_value!(&report);
    let reasons: BTreeSet<&str> = value["assigned"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|a| a["reason"].as_str())
        .collect();
    assert!(reasons.contains("duplicate_uid") && reasons.contains("invalid_uid"));
    assert_valid("upgrade-report", &value);
}

fn node(uid: &str, text: &str) -> Value {
    json!({ "type": "text", "uid": uid, "value": text })
}

/// One batch per command; together they cover every `op` of the contract.
/// Each is applied to the document the previous one produced.
fn batches(doc: &Value) -> Vec<Value> {
    let uid = |ptr: &str| {
        doc.pointer(ptr)
            .and_then(Value::as_str)
            .expect("uid")
            .to_string()
    };
    let section = uid("/pages/0/body/0/uid");
    let hello = uid("/pages/0/body/0/children/0/uid");
    let cond = uid("/pages/0/body/3/uid");
    let looped = uid("/pages/0/body/4/uid");
    let instance = uid("/pages/0/body/2/uid");
    let card_text = uid("/pages/0/body/2/slots/body/0/uid");
    let new_page = json!({
        "id": "about", "route": { "kind": "static", "path": "/about" }, "title": "About",
        "body": [ { "type": "text", "uid": "about.intro", "value": "About us" } ]
    });
    let rule = json!({ "id": "r2", "selector": ".d", "declarations": { "color": "blue" } });
    vec![
        json!({ "version": 1, "label": "site", "coalesce_key": "k", "commands": [
            { "op": "set_site_meta", "name": "Renamed", "locale": null, "base_url": "https://example.test/x" },
            { "op": "set_color_token", "key": "bg", "hex": "#eeeeee" },
            { "op": "set_color_token", "key": "extra", "hex": "#123456" },
            { "op": "remove_color_token", "key": "extra" },
            { "op": "set_font_token", "slot": "heading", "stack": "Georgia, serif" },
            { "op": "clear_font_token", "slot": "heading" },
            { "op": "set_data_value", "key": "k2", "value": { "a": [1, null] } },
            { "op": "remove_data_value", "key": "k2" }
        ] }),
        json!({ "version": 1, "commands": [
            { "op": "upsert_style_rule", "rule": rule },
            { "op": "insert_style_rule", "rule": { "id": "r3", "selector": ".e" }, "index": 0 },
            { "op": "remove_style_rule", "id": "r3" }
        ] }),
        json!({ "version": 1, "commands": [
            { "op": "insert_page", "page": new_page, "index": 1 },
            { "op": "move_page", "id": "about", "index": 0 },
            { "op": "upsert_page", "page": {
                "id": "about", "route": { "kind": "static", "path": "/about" }, "title": "About v2",
                "body": [ { "type": "text", "uid": "about.intro", "value": "About us" } ] } },
            { "op": "remove_page", "id": "about" }
        ] }),
        json!({ "version": 1, "commands": [
            { "op": "insert_node", "container": { "kind": "page_body", "page_id": "index" },
              "index": 0, "node": node("index.new", "New") },
            { "op": "insert_node", "container": { "kind": "node", "uid": section, "slot": "children" },
              "node": node("index.child", "Child") },
            { "op": "insert_node", "container": { "kind": "component_template", "name": "card" },
              "node": node("card.extra", "Extra") },
            { "op": "insert_node", "container": { "kind": "node", "uid": cond, "slot": "then_nodes" },
              "node": node("index.then", "Then") },
            { "op": "insert_node", "container": { "kind": "node", "uid": looped },
              "node": node("index.body", "Body") },
            { "op": "insert_node", "container": { "kind": "node", "uid": instance, "slot": "slots.body" },
              "node": node("index.slotted", "Slotted") },
            { "op": "move_node", "uid": "index.then",
              "container": { "kind": "node", "uid": cond, "slot": "else_nodes" }, "index": 0 },
            { "op": "remove_node", "uid": "card.extra" }
        ] }),
        json!({ "version": 1, "commands": [
            { "op": "set_attr", "uid": section, "attr": "data-x", "value": "1" },
            { "op": "set_attr", "uid": section, "attr": "data-x" },
            { "op": "set_text", "uid": hello, "value": "Hi" },
            { "op": "set_text", "uid": card_text, "value": "Card" },
            { "op": "set_tag", "uid": section, "tag": "article" },
            { "op": "set_classes", "uid": section, "classes": ["a", "b"] },
            { "op": "set_style_refs", "uid": section, "style_refs": ["r1", "r2"] }
        ] }),
    ]
}

#[test]
fn batches_transactions_and_inverses_from_the_reducer_validate() {
    let (mut doc, _) = upgraded();
    let seed = to_value!(&doc);
    let mut ops = BTreeSet::new();
    for batch_value in batches(&seed) {
        assert_valid("command-batch", &batch_value);
        let batch: CommandBatch = serde_json::from_value(batch_value.clone()).expect("typed");
        // the strict parser agrees with the schema
        parse_batch(&batch_value.to_string()).expect("strict parse");
        // the typed batch re-serializes into a schema-valid batch
        assert_valid("command-batch", &to_value!(&batch));
        let before = doc.clone();
        let (next, tx) = apply_batch(&doc, &batch).expect("batch applies");
        let tx_value = to_value!(&tx);
        assert_valid("transaction", &tx_value);
        assert_valid("site-document", &to_value!(&next));
        // the recorded inverse is itself a valid batch and undoes the change
        assert_valid("command-batch", &to_value!(&tx.inverse_batch()));
        assert_valid("command-batch", &to_value!(&tx.forward_batch()));
        let (undone, _) = apply_batch(&next, &tx.inverse_batch()).expect("inverse applies");
        assert_eq!(undone, before);
        for c in &batch.commands {
            ops.insert(to_value!(c)["op"].as_str().expect("op").to_string());
        }
        doc = next;
    }
    // every op of the generated schema was exercised by a real batch
    let schema_ops: BTreeSet<String> = schema_of("command-batch")["$defs"]["Command"]["oneOf"]
        .as_array()
        .expect("oneOf")
        .iter()
        .map(|b| {
            b["properties"]["op"]["const"]
                .as_str()
                .expect("op const")
                .to_string()
        })
        .collect();
    assert_eq!(ops, schema_ops, "ops exercised vs ops in the schema");
    assert_eq!(schema_ops.len(), 22);
}

#[test]
fn typed_errors_from_the_reducer_and_the_parser_validate() {
    let (doc, _) = upgraded();
    // reducer error with a command index
    let batch: CommandBatch = serde_json::from_value(json!({
        "version": 1,
        "commands": [
            { "op": "set_color_token", "key": "bg", "hex": "#eeeeee" },
            { "op": "remove_node", "uid": "no.such.node" }
        ]
    }))
    .expect("typed");
    let err = apply_batch(&doc, &batch).expect_err("unknown uid");
    assert_eq!(err.code, ErrorCode::NotFound);
    assert_eq!(err.command_index, Some(1));
    assert_valid("command-error", &to_value!(&err));
    // parser error carrying `paths`
    let err = parse_batch(
        r#"{"version":1,"commands":[{"op":"insert_node","container":{"kind":"page_body","page_id":"index"},"node":{"type":"text","value":"x","chidren":[]}}]}"#,
    )
    .expect_err("unknown field");
    assert_eq!(err.code, ErrorCode::InvalidJson);
    assert!(!err.paths.is_empty());
    assert_valid("command-error", &to_value!(&err));
    // batch-level errors
    let one = json!({ "op": "remove_page", "id": "post" });
    for (batch, code) in [
        (
            json!({ "version": 2, "commands": [] }),
            ErrorCode::UnsupportedVersion,
        ),
        (
            json!({ "version": 1, "commands": [] }),
            ErrorCode::EmptyBatch,
        ),
        (
            json!({ "version": 1, "commands": vec![one; 1001] }),
            ErrorCode::BatchTooLarge,
        ),
    ] {
        let parsed: CommandBatch = serde_json::from_value(batch).expect("typed");
        let err = parsed.validate().expect_err("invalid batch");
        assert_eq!(err.code, code);
        assert_valid("command-error", &to_value!(&err));
    }
    // a legacy document is not editable yet
    let legacy_batch: CommandBatch = serde_json::from_value(json!({
        "version": 1, "commands": [ { "op": "remove_page", "id": "post" } ]
    }))
    .expect("typed");
    let err = apply_batch(&legacy_document(), &legacy_batch).expect_err("not upgraded");
    assert_eq!(err.code, ErrorCode::NotUpgraded);
    assert_valid("command-error", &to_value!(&err));
}

#[test]
fn every_error_code_of_the_schema_is_a_real_wire_code() {
    let schema = schema_of("command-error");
    let codes: Vec<String> = schema["$defs"]["ErrorCode"]["enum"]
        .as_array()
        .expect("enum")
        .iter()
        .map(|c| c.as_str().expect("string").to_string())
        .collect();
    assert_eq!(codes.len(), 20);
    for code in &codes {
        let parsed: ErrorCode = serde_json::from_value(json!(code)).expect("known code");
        assert_eq!(parsed.as_str(), code);
        let err = CommandError::new(parsed, "m");
        assert_valid("command-error", &to_value!(&err));
    }
    assert_rejected(
        "command-error",
        &json!({ "code": "nope", "message": "m" }),
        "unknown code",
    );
}

fn valid_batch() -> Value {
    json!({ "version": 1, "commands": [ { "op": "remove_node", "uid": "index.main" } ] })
}

#[test]
fn invalid_uids_unknown_fields_and_unknown_ops_are_rejected() {
    assert_valid("command-batch", &valid_batch());
    // invalid uid in a command target
    for bad in ["", "Index", "-a", "a b", "a/b", &"a".repeat(65)] {
        let mut b = valid_batch();
        b["commands"][0]["uid"] = json!(bad);
        assert_rejected("command-batch", &b, &format!("command uid {bad:?}"));
    }
    // invalid uid on an inserted node and in a container reference
    let mut b = json!({ "version": 1, "commands": [ { "op": "insert_node",
        "container": { "kind": "page_body", "page_id": "index" },
        "node": { "type": "text", "uid": "Bad", "value": "x" } } ] });
    assert_rejected("command-batch", &b, "invalid uid on an inserted node");
    b["commands"][0]["node"]["uid"] = json!("good");
    assert_valid("command-batch", &b);
    b["commands"][0]["container"] = json!({ "kind": "node", "uid": "Bad" });
    assert_rejected("command-batch", &b, "invalid uid in a container reference");
    // invalid uid in a document
    let mut doc = to_value!(&upgraded().0);
    doc["pages"][0]["body"][0]["uid"] = json!("Not Valid");
    assert_rejected("site-document", &doc, "document node with an invalid uid");

    // unknown command field, container field, batch field, node field
    let mut b = valid_batch();
    b["commands"][0]["extra"] = json!(1);
    assert_rejected("command-batch", &b, "unknown command field");
    let mut b = valid_batch();
    b["extra"] = json!(1);
    assert_rejected("command-batch", &b, "unknown batch field");
    let mut b = json!({ "version": 1, "commands": [ { "op": "move_node", "uid": "a",
        "container": { "kind": "page_body", "page_id": "index", "slot": "x" } } ] });
    assert_rejected("command-batch", &b, "unknown container field");
    b["commands"][0]["container"] = json!({ "kind": "page_body", "page_id": "index" });
    assert_valid("command-batch", &b);
    b["commands"][0]["container"] = json!({ "kind": "elsewhere" });
    assert_rejected("command-batch", &b, "unknown container kind");
    let b = json!({ "version": 1, "commands": [ { "op": "insert_node",
        "container": { "kind": "page_body", "page_id": "index" },
        "node": { "type": "text", "value": "x", "chidren": [] } } ] });
    assert_rejected("command-batch", &b, "unknown field inside an embedded node");

    // unknown op, missing op, wrong field type
    let mut b = valid_batch();
    b["commands"][0]["op"] = json!("explode");
    assert_rejected("command-batch", &b, "unknown op");
    let b = json!({ "version": 1, "commands": [ { "uid": "a" } ] });
    assert_rejected("command-batch", &b, "missing op");
    let b = json!({ "version": 1, "commands": [ { "op": "set_text", "uid": "a", "value": 3 } ] });
    assert_rejected("command-batch", &b, "wrong value type");

    // version and size limits
    let mut b = valid_batch();
    b["version"] = json!(2);
    assert_rejected("command-batch", &b, "unsupported version");
    let mut b = valid_batch();
    b["commands"] = json!([]);
    assert_rejected("command-batch", &b, "empty batch");
    let one = valid_batch()["commands"][0].clone();
    let mut b = valid_batch();
    b["commands"] = Value::Array(vec![one.clone(); 1001]);
    assert_rejected("command-batch", &b, "oversized batch");
    b["commands"] = Value::Array(vec![one; 1000]);
    assert_valid("command-batch", &b);

    // transactions and reports reject unknown fields too
    let (doc, _) = upgraded();
    let batch: CommandBatch = serde_json::from_value(valid_batch_for(&doc)).expect("typed");
    let (_, tx) = apply_batch(&doc, &batch).expect("applies");
    let mut tx_value = to_value!(&tx);
    assert_valid("transaction", &tx_value);
    tx_value["extra"] = json!(1);
    assert_rejected("transaction", &tx_value, "unknown transaction field");
    let mut tx_value = to_value!(&tx);
    tx_value["forward"][0]["op"] = json!("explode");
    assert_rejected("transaction", &tx_value, "unknown op in a transaction");
    let mut report = upgraded().1;
    report["extra"] = json!(1);
    assert_rejected("upgrade-report", &report, "unknown report field");
}

fn valid_batch_for(doc: &SiteDocument) -> Value {
    let uid = doc.pages[0].body[0].clone();
    let uid = to_value!(&uid)["uid"].clone();
    json!({ "version": 1, "commands": [ { "op": "set_classes", "uid": uid, "classes": ["z"] } ] })
}
