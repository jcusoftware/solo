//! Reducer, history, upgrade and lossless-save tests (host-only, deterministic).

use serde_json::{Value, json};

use super::*;
use crate::types::{Node, SiteDocument};

fn legacy_json() -> Value {
    json!({
        "slug": "edit-fixture",
        "name": "Edit Fixture",
        "base_url": "https://fixture.example",
        "tokens": {
            "colors": { "ink": "#111111", "paper": "#FFFFFF", "accent": "#0B5FFF" },
            "fonts": { "body": "system-ui, sans-serif", "heading": "Georgia, serif" },
            "contrast_pairs": [
                { "foreground": "ink", "background": "paper", "min_ratio": 4.5 }
            ]
        },
        "styles": [
            { "id": "s-a", "selector": ".a", "declarations": { "color": "#111111" } },
            { "id": "s-b", "selector": ".b", "declarations": { "color": "#0B5FFF" } },
            { "id": "s-c", "selector": ".c", "declarations": { "color": "#FFFFFF" } }
        ],
        "components": {
            "card": {
                "name": "card",
                "slots": ["default"],
                "template": [{
                    "type": "element", "tag": "section", "classes": ["card"],
                    "children": [{
                        "type": "slot_ref", "name": "default",
                        "fallback": [{ "type": "text", "value": "fallback" }]
                    }]
                }]
            }
        },
        "pages": [
            {
                "id": "home",
                "route": { "kind": "static", "path": "/" },
                "title": "Home",
                "body": [{
                    "type": "element", "tag": "main", "attrs": { "id": "main" },
                    "children": [
                        { "type": "element", "tag": "section", "attrs": { "id": "intro" },
                          "children": [{ "type": "element", "tag": "h1",
                              "children": [{ "type": "text", "value": "Hello" }] }] },
                        { "type": "element", "tag": "section", "attrs": { "id": "menu" },
                          "children": [{ "type": "text", "value": "Menu" }] },
                        { "type": "element", "tag": "section", "attrs": { "id": "menu" },
                          "children": [] },
                        { "type": "component_instance", "component": "card",
                          "slots": { "default": [{ "type": "text", "value": "in card" }] } },
                        { "type": "conditional", "expr": "true",
                          "then_nodes": [{ "type": "text", "value": "yes" }], "else_nodes": [] },
                        { "type": "loop", "item_var": "x", "collection_expr": "data.items",
                          "body": [{ "type": "text", "value": "{{x}}" }] }
                    ]
                }]
            },
            {
                "id": "about",
                "route": { "kind": "static", "path": "/about" },
                "title": "About",
                "body": [{
                    "type": "element", "tag": "main", "attrs": { "id": "main" },
                    "children": [{ "type": "text", "value": "About us" }]
                }]
            }
        ],
        "data": { "items": ["a", "b"] },
        "metadata": { "note": { "z": 1, "a": 2 } }
    })
}

fn legacy_doc() -> SiteDocument {
    serde_json::from_value(legacy_json()).expect("fixture parses")
}

fn upgraded() -> SiteDocument {
    assign_stable_ids(&legacy_doc()).expect("upgrade").0
}

fn bytes(doc: &SiteDocument) -> String {
    canonical_json(doc)
}

fn uid_of(doc: &SiteDocument, path: &str) -> String {
    let mut found = None;
    crate::edit::ids::for_each_node(doc, &mut |n, p| {
        if p == path {
            found = n.uid().map(str::to_string);
        }
    });
    found.unwrap_or_else(|| panic!("no node at {path}"))
}

fn batch(commands: Vec<Command>) -> CommandBatch {
    CommandBatch::new(commands)
}

fn text(uid: &str, value: &str) -> Node {
    Node::Text {
        uid: Some(uid.to_string()),
        value: value.to_string(),
    }
}

fn element(uid: &str, tag: &str, children: Vec<Node>) -> Node {
    Node::Element {
        uid: Some(uid.to_string()),
        id: None,
        tag: tag.to_string(),
        attrs: Default::default(),
        classes: Vec::new(),
        style_refs: Vec::new(),
        children,
    }
}

fn strip_uids(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("uid");
            map.values_mut().for_each(strip_uids);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_uids),
        _ => {}
    }
}

#[test]
fn legacy_documents_serialize_without_uid_keys() {
    let text = serde_json::to_string(&legacy_doc()).unwrap();
    assert!(!text.contains("\"uid\""), "None uid must be skipped");
    let upgraded_text = serde_json::to_string(&upgraded()).unwrap();
    assert!(upgraded_text.contains("\"uid\":\"home.main\""));
}

// ---- upgrade ---------------------------------------------------------------

#[test]
fn upgrade_is_additive_deterministic_and_idempotent() {
    let legacy = legacy_doc();
    let (first, report) = assign_stable_ids(&legacy).expect("upgrade");
    let (second, _) = assign_stable_ids(&legacy).expect("upgrade again");
    assert_eq!(bytes(&first), bytes(&second), "deterministic");

    let (again, again_report) = assign_stable_ids(&first).expect("re-upgrade");
    assert_eq!(bytes(&first), bytes(&again), "idempotent");
    assert!(again_report.assigned.is_empty());
    assert!(!again_report.marker_written);
    assert_eq!(again_report.kept, report.assigned.len());

    check_uids(&first).expect("all uids valid and unique");
    assert_eq!(
        first.metadata.get(MARKER_KEY),
        Some(&json!({ "uid_scheme": 1 }))
    );

    // Everything except `uid` and the marker is untouched (no reorder, no rewrite).
    let mut before = serde_json::to_value(&legacy).unwrap();
    let mut after = serde_json::to_value(&first).unwrap();
    after["metadata"]
        .as_object_mut()
        .unwrap()
        .remove(MARKER_KEY);
    strip_uids(&mut before);
    strip_uids(&mut after);
    assert_eq!(before, after);
    assert!(legacy.schema_version == first.schema_version);
}

#[test]
fn upgrade_names_follow_the_documented_scheme() {
    let doc = upgraded();
    assert_eq!(uid_of(&doc, "pages[0].body[0]"), "home.main");
    assert_eq!(uid_of(&doc, "pages[1].body[0]"), "about.main");
    assert_eq!(uid_of(&doc, "pages[0].body[0].children[0]"), "home.intro");
    // Same HTML id twice: first keeps the bare name, second gets -2.
    assert_eq!(uid_of(&doc, "pages[0].body[0].children[1]"), "home.menu");
    assert_eq!(uid_of(&doc, "pages[0].body[0].children[2]"), "home.menu-2");
    assert_eq!(
        uid_of(&doc, "components.card.template[0]"),
        "component-card.card"
    );
}

#[test]
fn upgrade_reassigns_duplicate_and_invalid_uids_and_keeps_good_ones() {
    let mut doc = legacy_doc();
    if let Node::Element { children, .. } = &mut doc.pages[0].body[0] {
        children[0].set_uid(Some("keep-me".to_string()));
        children[1].set_uid(Some("keep-me".to_string()));
        children[2].set_uid(Some("Bad Uid!".to_string()));
    }
    let (out, report) = assign_stable_ids(&doc).expect("upgrade");
    check_uids(&out).expect("unique");
    assert_eq!(uid_of(&out, "pages[0].body[0].children[0]"), "keep-me");
    let reasons: Vec<&str> = report.assigned.iter().map(|a| a.reason.as_str()).collect();
    assert!(reasons.contains(&"duplicate_uid"));
    assert!(reasons.contains(&"invalid_uid"));
}

#[test]
fn upgrade_rejects_unknown_marker_scheme() {
    let mut doc = legacy_doc();
    doc.metadata
        .insert(MARKER_KEY.to_string(), json!({ "uid_scheme": 2 }));
    let err = assign_stable_ids(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedUidScheme);
}

#[test]
fn upgrade_keeps_other_keys_inside_an_existing_marker_object() {
    let mut doc = legacy_doc();
    doc.metadata.insert(
        MARKER_KEY.to_string(),
        json!({ "uid_scheme": 1, "editor": { "layout": "wide" } }),
    );
    doc.metadata
        .insert("other".to_string(), json!({ "keep": true }));
    let (out, report) = assign_stable_ids(&doc).unwrap();
    assert_eq!(
        out.metadata[MARKER_KEY],
        json!({ "uid_scheme": 1, "editor": { "layout": "wide" } })
    );
    assert_eq!(out.metadata["other"], json!({ "keep": true }));
    assert!(!report.marker_written, "scheme was already 1");

    // A marker object without a scheme gets the scheme added in place.
    let mut bare = legacy_doc();
    bare.metadata
        .insert(MARKER_KEY.to_string(), json!({ "editor": 1 }));
    let (out, report) = assign_stable_ids(&bare).unwrap();
    assert_eq!(
        out.metadata[MARKER_KEY],
        json!({ "editor": 1, "uid_scheme": 1 })
    );
    assert!(report.marker_written);
}

#[test]
fn upgrade_refuses_a_marker_that_is_not_an_object() {
    for bad in [json!("yes"), json!(1), json!(null), json!([1])] {
        let mut doc = legacy_doc();
        doc.metadata.insert(MARKER_KEY.to_string(), bad);
        let err = assign_stable_ids(&doc).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedUidScheme);
        assert!(err.message.contains("must be an object"), "{err}");
    }
}

// ---- editability gate ------------------------------------------------------

#[test]
fn legacy_documents_are_refused_for_editing() {
    let err = History::new(legacy_doc()).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotUpgraded);
    let err = apply_batch(
        &legacy_doc(),
        &batch(vec![Command::SetSiteMeta {
            name: Some("x".to_string()),
            locale: None,
            base_url: None,
        }]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotUpgraded);
}

#[test]
fn marked_document_missing_uids_is_refused_until_reupgraded() {
    let mut doc = upgraded();
    if let Node::Element { children, .. } = &mut doc.pages[0].body[0] {
        children[3].set_uid(None);
    }
    let err = check_editable(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingUid);
    assert_eq!(err.paths, vec!["pages[0].body[0].children[3]".to_string()]);
    // Re-upgrading repairs it.
    let (fixed, report) = assign_stable_ids(&doc).unwrap();
    assert_eq!(report.assigned.len(), 1);
    check_editable(&fixed).expect("editable after re-upgrade");
}

#[test]
fn duplicate_and_invalid_uids_are_rejected() {
    let mut dup = upgraded();
    if let Node::Element { children, .. } = &mut dup.pages[0].body[0] {
        children[1].set_uid(Some("home.intro".to_string()));
    }
    assert_eq!(
        check_editable(&dup).unwrap_err().code,
        ErrorCode::DuplicateUid
    );

    let mut invalid = upgraded();
    invalid.pages[1].body[0].set_uid(Some("UPPER".to_string()));
    assert_eq!(
        check_editable(&invalid).unwrap_err().code,
        ErrorCode::InvalidUid
    );

    let mut dup_style = upgraded();
    let rule = dup_style.styles[0].clone();
    dup_style.styles.push(rule);
    assert_eq!(
        check_editable(&dup_style).unwrap_err().code,
        ErrorCode::DuplicateId
    );
}

#[test]
fn uid_grammar() {
    for ok in ["a", "0", "home.main", "a_b-c.9", &"a".repeat(64)] {
        assert!(is_valid_uid(ok), "{ok}");
    }
    for bad in ["", "A", "-a", ".a", "a b", "a/b", &"a".repeat(65), "é"] {
        assert!(!is_valid_uid(bad), "{bad}");
    }
}

// ---- command semantics -----------------------------------------------------

fn each_command_case(doc: &SiteDocument) -> Vec<(&'static str, Command)> {
    let intro = uid_of(doc, "pages[0].body[0].children[0]");
    let menu = uid_of(doc, "pages[0].body[0].children[1]");
    let main = uid_of(doc, "pages[0].body[0]");
    let h1_text = uid_of(doc, "pages[0].body[0].children[0].children[0].children[0]");
    let instance = uid_of(doc, "pages[0].body[0].children[3]");
    let cond = uid_of(doc, "pages[0].body[0].children[4]");
    let looped = uid_of(doc, "pages[0].body[0].children[5]");
    let slot_ref = uid_of(doc, "components.card.template[0].children[0]");
    let into = |uid: &str, slot: &str| ContainerRef::Node {
        uid: uid.to_string(),
        slot: Some(slot.to_string()),
    };
    let new_rule = |id: &str| crate::types::IrStyleRule {
        id: id.to_string(),
        selector: ".n".to_string(),
        breakpoint: None,
        pseudo: None,
        declarations: Default::default(),
    };
    let page = crate::types::Page {
        id: "extra".to_string(),
        route: crate::types::RoutePattern::Static {
            path: "/extra".to_string(),
        },
        title: "Extra".to_string(),
        description: None,
        canonical: None,
        noindex: false,
        is_not_found: false,
        json_ld: Vec::new(),
        scripts: Vec::new(),
        body: vec![element("extra.main", "main", vec![text("extra.t", "x")])],
    };
    vec![
        (
            "set_site_meta",
            Command::SetSiteMeta {
                name: Some("Renamed".to_string()),
                locale: Some("en-GB".to_string()),
                base_url: None,
            },
        ),
        (
            "set_color_token existing",
            Command::SetColorToken {
                key: "accent".into(),
                hex: "#112233".into(),
            },
        ),
        (
            "set_color_token new",
            Command::SetColorToken {
                key: "brand".into(),
                hex: "#445566".into(),
            },
        ),
        (
            "remove_color_token",
            Command::RemoveColorToken {
                key: "accent".into(),
            },
        ),
        (
            "set_font_token body",
            Command::SetFontToken {
                slot: "body".into(),
                stack: "serif".into(),
            },
        ),
        (
            "set_font_token heading (existing)",
            Command::SetFontToken {
                slot: "heading".into(),
                stack: "Fraunces, serif".into(),
            },
        ),
        (
            "set_font_token mono (new)",
            Command::SetFontToken {
                slot: "mono".into(),
                stack: "ui-monospace, monospace".into(),
            },
        ),
        (
            "clear_font_token heading",
            Command::ClearFontToken {
                slot: "heading".into(),
            },
        ),
        (
            "set_data_value existing",
            Command::SetDataValue {
                key: "items".into(),
                value: json!([1, 2, 3]),
            },
        ),
        (
            "set_data_value new",
            Command::SetDataValue {
                key: "fresh".into(),
                value: json!({ "k": 1 }),
            },
        ),
        (
            "remove_data_value",
            Command::RemoveDataValue {
                key: "items".into(),
            },
        ),
        (
            "upsert_style_rule replace",
            Command::UpsertStyleRule {
                rule: {
                    let mut r = new_rule("s-b");
                    r.declarations.insert("color".into(), "#000000".into());
                    r
                },
            },
        ),
        (
            "upsert_style_rule new",
            Command::UpsertStyleRule {
                rule: new_rule("s-new"),
            },
        ),
        (
            "insert_style_rule",
            Command::InsertStyleRule {
                rule: new_rule("s-mid"),
                index: Some(1),
            },
        ),
        (
            "remove_style_rule",
            Command::RemoveStyleRule { id: "s-b".into() },
        ),
        (
            "insert_page",
            Command::InsertPage {
                page: page.clone(),
                index: Some(1),
            },
        ),
        (
            "upsert_page new",
            Command::UpsertPage { page: page.clone() },
        ),
        (
            "upsert_page replacing an existing page",
            Command::UpsertPage {
                page: {
                    let mut replacement = page.clone();
                    replacement.id = "about".to_string();
                    replacement.route = crate::types::RoutePattern::Static {
                        path: "/about".to_string(),
                    };
                    replacement
                },
            },
        ),
        ("remove_page", Command::RemovePage { id: "home".into() }),
        (
            "move_page",
            Command::MovePage {
                id: "about".into(),
                index: 0,
            },
        ),
        (
            "insert_node",
            Command::InsertNode {
                container: ContainerRef::Node {
                    uid: main.clone(),
                    slot: None,
                },
                index: Some(1),
                node: element("new.sec", "section", vec![text("new.t", "New")]),
            },
        ),
        (
            "insert_node into then_nodes",
            Command::InsertNode {
                container: into(&cond, "then_nodes"),
                index: Some(0),
                node: text("cond.then.new", "t"),
            },
        ),
        (
            "insert_node into else_nodes",
            Command::InsertNode {
                container: into(&cond, "else_nodes"),
                index: None,
                node: element("cond.else.new", "p", vec![text("cond.else.t", "e")]),
            },
        ),
        (
            "insert_node into a component slot",
            Command::InsertNode {
                container: into(&instance, "slots.default"),
                index: Some(1),
                node: text("inst.slot.new", "s"),
            },
        ),
        (
            "insert_node element into a component slot at 0",
            Command::InsertNode {
                container: into(&instance, "slots.default"),
                index: Some(0),
                node: element("inst.slot.el", "b", Vec::new()),
            },
        ),
        (
            "insert_node into fallback",
            Command::InsertNode {
                container: ContainerRef::Node {
                    uid: slot_ref.clone(),
                    slot: None,
                },
                index: None,
                node: text("fb.new", "f"),
            },
        ),
        (
            "insert_node into loop body",
            Command::InsertNode {
                container: ContainerRef::Node {
                    uid: looped.clone(),
                    slot: None,
                },
                index: Some(0),
                node: text("loop.new", "l"),
            },
        ),
        (
            "move_node into then_nodes",
            Command::MoveNode {
                uid: menu.clone(),
                container: into(&cond, "then_nodes"),
                index: Some(1),
            },
        ),
        (
            "move_node into else_nodes",
            Command::MoveNode {
                uid: intro.clone(),
                container: into(&cond, "else_nodes"),
                index: None,
            },
        ),
        (
            "move_node into a component slot",
            Command::MoveNode {
                uid: menu.clone(),
                container: into(&instance, "slots.default"),
                index: Some(0),
            },
        ),
        (
            "move_node into fallback",
            Command::MoveNode {
                uid: menu.clone(),
                container: into(&slot_ref, "fallback"),
                index: None,
            },
        ),
        (
            "move_node into loop body",
            Command::MoveNode {
                uid: intro.clone(),
                container: into(&looped, "body"),
                index: Some(0),
            },
        ),
        (
            "move_node out of a loop body",
            Command::MoveNode {
                uid: uid_of(doc, "pages[0].body[0].children[5].body[0]"),
                container: ContainerRef::PageBody {
                    page_id: "about".into(),
                },
                index: None,
            },
        ),
        (
            "insert_node into component template",
            Command::InsertNode {
                container: ContainerRef::ComponentTemplate {
                    name: "card".into(),
                },
                index: None,
                node: text("card.extra", "tail"),
            },
        ),
        (
            "remove_node subtree",
            Command::RemoveNode { uid: menu.clone() },
        ),
        (
            "move_node same container",
            Command::MoveNode {
                uid: intro.clone(),
                container: ContainerRef::Node {
                    uid: main.clone(),
                    slot: None,
                },
                index: Some(3),
            },
        ),
        (
            "move_node across pages",
            Command::MoveNode {
                uid: menu.clone(),
                container: ContainerRef::PageBody {
                    page_id: "about".into(),
                },
                index: Some(0),
            },
        ),
        (
            "set_attr new",
            Command::SetAttr {
                uid: menu.clone(),
                attr: "class".into(),
                value: Some("x".into()),
            },
        ),
        (
            "set_attr existing",
            Command::SetAttr {
                uid: menu.clone(),
                attr: "id".into(),
                value: Some("menu-2".into()),
            },
        ),
        (
            "set_attr remove",
            Command::SetAttr {
                uid: menu.clone(),
                attr: "id".into(),
                value: None,
            },
        ),
        (
            "set_text",
            Command::SetText {
                uid: h1_text,
                value: "Hi".into(),
            },
        ),
        (
            "set_tag",
            Command::SetTag {
                uid: intro.clone(),
                tag: "article".into(),
            },
        ),
        (
            "set_classes",
            Command::SetClasses {
                uid: intro.clone(),
                classes: vec!["a".into(), "b".into()],
            },
        ),
        (
            "set_style_refs",
            Command::SetStyleRefs {
                uid: intro,
                style_refs: vec!["s-a".into()],
            },
        ),
    ]
}

#[test]
fn every_command_round_trips_through_its_inverse_on_serialized_bytes() {
    let doc = upgraded();
    let original = bytes(&doc);
    for (name, command) in each_command_case(&doc) {
        let (next, tx) = apply_batch(&doc, &batch(vec![command.clone()]))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_ne!(bytes(&next), original, "{name} changed nothing");
        check_uids(&next).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!tx.touched.is_empty(), "{name}: no touched fields");
        assert_eq!(tx.forward, vec![command]);

        let (restored, redo_tx) = apply_batch(&next, &tx.inverse_batch())
            .unwrap_or_else(|e| panic!("{name} inverse: {e}"));
        assert_eq!(
            bytes(&restored),
            original,
            "{name}: apply∘inverse != identity"
        );

        let (replayed, _) = apply_batch(&restored, &tx.forward_batch()).unwrap();
        assert_eq!(bytes(&replayed), bytes(&next), "{name}: redo differs");
        // The inverse's own inverse is the forward batch (symmetry).
        let (back, _) = apply_batch(&restored, &redo_tx.inverse_batch()).unwrap();
        assert_eq!(bytes(&back), bytes(&next), "{name}: double inverse");
    }
}

#[test]
fn history_undo_redo_is_identity_for_a_long_mixed_sequence() {
    let doc = upgraded();
    let original = bytes(&doc);
    let mut history = History::new(doc.clone()).unwrap();
    let mut checkpoints = vec![original.clone()];
    for (name, command) in each_command_case(&doc) {
        // Some cases conflict with earlier ones in a shared sequence; skip those.
        if history.apply(&batch(vec![command])).is_ok() {
            checkpoints.push(bytes(history.document()));
        } else {
            eprintln!("skipped in sequence: {name}");
        }
    }
    assert!(checkpoints.len() > 12);
    let final_bytes = bytes(history.document());
    for expected in checkpoints.iter().rev().skip(1) {
        assert!(history.undo().unwrap().is_some());
        assert_eq!(&bytes(history.document()), expected);
    }
    assert!(history.undo().unwrap().is_none());
    for expected in checkpoints.iter().skip(1) {
        assert!(history.redo().unwrap().is_some());
        assert_eq!(&bytes(history.document()), expected);
    }
    assert_eq!(bytes(history.document()), final_bytes);
    assert!(!history.can_redo());
}

fn random_page(id: &str) -> crate::types::Page {
    crate::types::Page {
        id: id.to_string(),
        route: crate::types::RoutePattern::Static {
            path: format!("/{id}"),
        },
        title: id.to_string(),
        description: None,
        canonical: None,
        noindex: false,
        is_not_found: false,
        json_ld: Vec::new(),
        scripts: Vec::new(),
        body: vec![element(
            &format!("{id}.main"),
            "main",
            vec![text(&format!("{id}.t"), "x")],
        )],
    }
}

#[test]
fn deterministic_pseudo_random_sequences_keep_every_invariant() {
    // Tiny LCG: no RNG dependency, identical on every platform.
    let mut seed: u64 = 0x5EED_1234;
    let mut next = move |bound: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % bound.max(1)
    };
    let mut history = History::with_limit(upgraded(), 10_000).unwrap();
    // `states[i]` is the document after undo step `i` (coalesced batches
    // replace the last entry instead of adding one).
    let mut states = vec![bytes(history.document())];
    let mut counter = 0usize;
    let mut kinds_seen = std::collections::BTreeSet::new();
    let mut merged_steps = 0usize;
    for _ in 0..600 {
        // (uid, child-list names, is_text) of every node.
        let mut nodes: Vec<(String, Vec<String>, bool)> = Vec::new();
        crate::edit::ids::for_each_node(history.document(), &mut |n, _| {
            let slots = crate::edit::ids::child_lists(n)
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            nodes.push((
                n.uid().unwrap().to_string(),
                slots,
                matches!(n, Node::Text { .. }),
            ));
        });
        let uids: Vec<String> = nodes.iter().map(|n| n.0.clone()).collect();
        let pick = |n: &mut dyn FnMut(usize) -> usize, v: &[String]| v[n(v.len())].clone();
        // A container drawn from the real child lists (conditional branches,
        // component slots, fallbacks, loop bodies, page bodies, templates).
        let container = |n: &mut dyn FnMut(usize) -> usize| -> ContainerRef {
            let (uid, slots, _) = &nodes[n(nodes.len())];
            match n(8) {
                0 => ContainerRef::PageBody {
                    page_id: "about".into(),
                },
                1 => ContainerRef::ComponentTemplate {
                    name: "card".into(),
                },
                _ if slots.is_empty() => ContainerRef::Node {
                    uid: uid.clone(),
                    slot: None,
                },
                _ => ContainerRef::Node {
                    uid: uid.clone(),
                    slot: Some(slots[n(slots.len())].clone()),
                },
            }
        };
        counter += 1;
        let fresh = format!("gen{counter}");
        let selector = if uids.len() < 8 { 2 } else { next(22) };
        let command = match selector {
            0 => Command::RemoveNode {
                uid: pick(&mut next, &uids),
            },
            1 => Command::MoveNode {
                uid: pick(&mut next, &uids),
                container: container(&mut next),
                index: if next(2) == 0 { None } else { Some(next(4)) },
            },
            2 => Command::InsertNode {
                container: container(&mut next),
                index: if next(2) == 0 { None } else { Some(next(3)) },
                node: element(&fresh, "div", vec![text(&format!("{fresh}.t"), "t")]),
            },
            3 => Command::SetAttr {
                uid: pick(&mut next, &uids),
                attr: "data-k".into(),
                value: if next(4) == 0 {
                    None
                } else {
                    Some(format!("v{counter}"))
                },
            },
            4 => Command::SetColorToken {
                key: format!("c{}", next(3)),
                hex: "#0A0B0C".into(),
            },
            5 => Command::MovePage {
                id: "home".into(),
                index: next(2),
            },
            6 => Command::SetText {
                uid: pick(&mut next, &uids),
                value: format!("text{counter}"),
            },
            7 => Command::SetTag {
                uid: pick(&mut next, &uids),
                tag: ["div", "clipPath", "section", "linearGradient"][next(4)].into(),
            },
            8 => Command::SetClasses {
                uid: pick(&mut next, &uids),
                classes: (0..next(3)).map(|i| format!("k{i}")).collect(),
            },
            9 => Command::SetStyleRefs {
                uid: pick(&mut next, &uids),
                style_refs: vec!["s-a".into()],
            },
            10 => Command::SetSiteMeta {
                name: Some(format!("Site {}", next(3))),
                locale: None,
                base_url: None,
            },
            11 => Command::SetFontToken {
                slot: ["body", "heading", "mono"][next(3)].into(),
                stack: format!("font{}, serif", next(3)),
            },
            12 => Command::ClearFontToken {
                slot: ["heading", "mono"][next(2)].into(),
            },
            13 => {
                if next(2) == 0 {
                    Command::SetDataValue {
                        key: format!("d{}", next(3)),
                        value: json!({ "n": next(5) }),
                    }
                } else {
                    Command::RemoveDataValue {
                        key: format!("d{}", next(3)),
                    }
                }
            }
            14 => Command::UpsertStyleRule {
                rule: crate::types::IrStyleRule {
                    id: format!("r{}", next(3)),
                    selector: ".r".into(),
                    breakpoint: None,
                    pseudo: None,
                    declarations: Default::default(),
                },
            },
            15 => Command::InsertStyleRule {
                rule: crate::types::IrStyleRule {
                    id: format!("r{}", next(4)),
                    selector: ".i".into(),
                    breakpoint: None,
                    pseudo: None,
                    declarations: Default::default(),
                },
                index: Some(next(3)),
            },
            16 => Command::RemoveStyleRule {
                id: format!("r{}", next(4)),
            },
            17 => Command::InsertPage {
                page: random_page(&format!("p{}", next(4))),
                index: Some(next(2)),
            },
            18 => Command::UpsertPage {
                page: random_page(&format!("p{}", next(4))),
            },
            19 => Command::RemovePage {
                id: format!("p{}", next(4)),
            },
            20 => Command::RemoveColorToken {
                key: format!("c{}", next(3)),
            },
            _ => Command::SetSiteMeta {
                name: None,
                locale: Some(["en-US", "en-GB"][next(2)].into()),
                base_url: None,
            },
        };
        let mut commands = vec![command];
        if next(5) == 0 {
            commands.push(Command::SetDataValue {
                key: "second".into(),
                value: json!(counter),
            });
        }
        let mut b = batch(commands);
        if next(2) == 0 {
            b.coalesce_key = Some(format!("grp{}", counter / 6));
        }
        let before = bytes(history.document());
        let depth_before = history.undo_depth();
        match history.apply(&b) {
            Ok(tx) => {
                kinds_seen.insert(
                    serde_json::to_value(&tx.forward[0]).unwrap()["op"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                );
                check_uids(history.document()).expect("uids stay unique");
                check_editable(history.document()).expect("document stays editable");
                if history.undo_depth() > depth_before {
                    states.push(bytes(history.document()));
                } else {
                    merged_steps += 1;
                    *states.last_mut().unwrap() = bytes(history.document());
                }
            }
            Err(_) => assert_eq!(
                bytes(history.document()),
                before,
                "failed apply must not mutate"
            ),
        }
    }
    assert!(states.len() > 80, "too few valid steps: {}", states.len());
    assert!(
        merged_steps > 20,
        "coalescing barely exercised: {merged_steps}"
    );
    assert!(
        kinds_seen.len() >= 18,
        "too few command kinds applied: {kinds_seen:?}"
    );
    for expected in states.iter().rev().skip(1) {
        history.undo().unwrap().unwrap();
        assert_eq!(&bytes(history.document()), expected);
    }
    assert!(!history.can_undo());
    for expected in states.iter().skip(1) {
        history.redo().unwrap().unwrap();
        assert_eq!(&bytes(history.document()), expected);
    }
}

#[test]
fn move_keeps_uids_and_author_order_of_everything_else() {
    let doc = upgraded();
    let intro = uid_of(&doc, "pages[0].body[0].children[0]");
    let main = uid_of(&doc, "pages[0].body[0]");
    let before_uids = {
        let mut v = Vec::new();
        crate::edit::ids::for_each_node(&doc, &mut |n, _| v.push(n.uid().unwrap().to_string()));
        v.sort();
        v
    };
    let (moved, tx) = apply_batch(
        &doc,
        &batch(vec![Command::MoveNode {
            uid: intro.clone(),
            container: ContainerRef::Node {
                uid: main.clone(),
                slot: Some("children".into()),
            },
            index: None,
        }]),
    )
    .unwrap();
    let mut after_uids = Vec::new();
    crate::edit::ids::for_each_node(&moved, &mut |n, _| {
        after_uids.push(n.uid().unwrap().to_string())
    });
    after_uids.sort();
    assert_eq!(before_uids, after_uids, "move creates/destroys no ids");
    let Node::Element { children, .. } = &moved.pages[0].body[0] else {
        panic!()
    };
    let order: Vec<_> = children
        .iter()
        .map(|n| n.uid().unwrap().to_string())
        .collect();
    assert_eq!(order[0], "home.menu");
    assert_eq!(order[1], "home.menu-2");
    assert_eq!(order.last().unwrap(), &intro);
    // Untouched arrays keep their order.
    assert_eq!(moved.styles, doc.styles);
    assert_eq!(
        moved.pages.iter().map(|p| &p.id).collect::<Vec<_>>(),
        vec!["home", "about"]
    );
    assert!(tx.touched.contains(&format!("node:{intro}")));
    assert!(tx.touched.contains(&format!("node:{main}/children")));
}

#[test]
fn moving_a_node_into_its_own_subtree_is_rejected() {
    let doc = upgraded();
    let main = uid_of(&doc, "pages[0].body[0]");
    let intro = uid_of(&doc, "pages[0].body[0].children[0]");
    let err = apply_batch(
        &doc,
        &batch(vec![Command::MoveNode {
            uid: main,
            container: ContainerRef::Node {
                uid: intro,
                slot: None,
            },
            index: None,
        }]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::MoveIntoSelf);
    assert_eq!(err.command_index, Some(0));
}

// ---- atomicity and validation ----------------------------------------------

#[test]
fn failing_command_leaves_the_document_byte_identical() {
    let doc = upgraded();
    let original = bytes(&doc);
    let mut working = doc.clone();
    let main = uid_of(&doc, "pages[0].body[0]");
    let bad_batches: Vec<(Vec<Command>, ErrorCode, usize)> = vec![
        (
            vec![
                Command::SetSiteMeta {
                    name: Some("Changed".into()),
                    locale: None,
                    base_url: None,
                },
                Command::SetColorToken {
                    key: "accent".into(),
                    hex: "#123456".into(),
                },
                Command::RemoveNode {
                    uid: "does.not.exist".into(),
                },
            ],
            ErrorCode::NotFound,
            2,
        ),
        (
            vec![
                Command::SetColorToken {
                    key: "k".into(),
                    hex: "#123456".into(),
                },
                Command::SetColorToken {
                    key: "k".into(),
                    hex: "not-a-color".into(),
                },
            ],
            ErrorCode::InvalidValue,
            1,
        ),
        (
            vec![
                Command::InsertNode {
                    container: ContainerRef::Node {
                        uid: main.clone(),
                        slot: None,
                    },
                    index: None,
                    node: text("dup.t", "a"),
                },
                Command::InsertNode {
                    container: ContainerRef::Node {
                        uid: main.clone(),
                        slot: None,
                    },
                    index: None,
                    node: text("dup.t", "b"),
                },
            ],
            ErrorCode::DuplicateUid,
            1,
        ),
        (
            vec![Command::InsertNode {
                container: ContainerRef::Node {
                    uid: main.clone(),
                    slot: None,
                },
                index: Some(99),
                node: text("x.1", "a"),
            }],
            ErrorCode::InvalidIndex,
            0,
        ),
        (
            vec![Command::InsertNode {
                container: ContainerRef::Node {
                    uid: main.clone(),
                    slot: None,
                },
                index: None,
                node: Node::Text {
                    uid: None,
                    value: "no uid".into(),
                },
            }],
            ErrorCode::MissingUid,
            0,
        ),
        (
            vec![Command::InsertNode {
                container: ContainerRef::Node {
                    uid: main.clone(),
                    slot: None,
                },
                index: None,
                node: text("home.intro", "reused id"),
            }],
            ErrorCode::DuplicateUid,
            0,
        ),
        (
            vec![Command::InsertNode {
                container: ContainerRef::Node {
                    uid: main.clone(),
                    slot: Some("nope".into()),
                },
                index: None,
                node: text("x.2", "a"),
            }],
            ErrorCode::InvalidContainer,
            0,
        ),
        (
            vec![Command::SetText {
                uid: main.clone(),
                value: "x".into(),
            }],
            ErrorCode::WrongNodeKind,
            0,
        ),
        (
            vec![Command::SetTag {
                uid: main.clone(),
                tag: "Bad Tag".into(),
            }],
            ErrorCode::InvalidValue,
            0,
        ),
        (
            vec![
                Command::RemovePage { id: "about".into() },
                Command::RemovePage { id: "about".into() },
            ],
            ErrorCode::NotFound,
            1,
        ),
        (
            vec![Command::InsertPage {
                page: working.pages[1].clone(),
                index: None,
            }],
            ErrorCode::AlreadyExists,
            0,
        ),
    ];
    for (commands, code, index) in bad_batches {
        let err = apply_batch(&doc, &batch(commands.clone())).unwrap_err();
        assert_eq!(err.code, code, "{commands:?}");
        assert_eq!(err.command_index, Some(index), "{commands:?}");
        assert_eq!(bytes(&doc), original);
        // The in-place variant must not mutate either.
        let tx = apply_batch_in_place(&mut working, &batch(commands));
        assert!(tx.is_err());
        assert_eq!(bytes(&working), original);
    }
}

#[test]
fn batch_that_breaks_document_validity_is_rejected_whole() {
    let doc = upgraded();
    let err = apply_batch(
        &doc,
        &batch(vec![Command::SetSiteMeta {
            name: None,
            locale: None,
            base_url: Some("http://insecure.example".into()),
        }]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidValue);

    // Removing a token still referenced by a contrast pair fails validate_ir.
    let err = apply_batch(
        &doc,
        &batch(vec![Command::RemoveColorToken { key: "ink".into() }]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidDocument);
}

#[test]
fn batch_envelope_is_versioned_strict_and_bounded() {
    let doc = upgraded();
    let mut b = batch(vec![Command::SetSiteMeta {
        name: Some("x".into()),
        locale: None,
        base_url: None,
    }]);
    b.version = 2;
    assert_eq!(
        apply_batch(&doc, &b).unwrap_err().code,
        ErrorCode::UnsupportedVersion
    );
    assert_eq!(
        apply_batch(&doc, &batch(vec![])).unwrap_err().code,
        ErrorCode::EmptyBatch
    );
    let big = batch(vec![
        Command::SetDataValue {
            key: "k".into(),
            value: json!(1)
        };
        MAX_BATCH_COMMANDS + 1
    ]);
    assert_eq!(
        apply_batch(&doc, &big).unwrap_err().code,
        ErrorCode::BatchTooLarge
    );

    let typo = r##"{"version":1,"commands":[{"op":"set_color_token","key":"a","hex":"#000000","hexx":1}]}"##;
    assert_eq!(parse_batch(typo).unwrap_err().code, ErrorCode::InvalidJson);
    let unknown_top = r#"{"version":1,"commands":[],"extra":true}"#;
    assert_eq!(
        parse_batch(unknown_top).unwrap_err().code,
        ErrorCode::InvalidJson
    );
    let ok = r##"{"version":1,"label":"x","commands":[{"op":"set_color_token","key":"a","hex":"#000000"}]}"##;
    assert_eq!(parse_batch(ok).unwrap().commands.len(), 1);
}

// ---- coalescing, touched fields -------------------------------------------

fn set_accent(hex: &str) -> CommandBatch {
    let mut b = batch(vec![Command::SetColorToken {
        key: "accent".into(),
        hex: hex.into(),
    }]);
    b.coalesce_key = Some("accent-drag".into());
    b
}

#[test]
fn coalescing_merges_consecutive_same_key_batches_into_one_undo_step() {
    let mut history = History::new(upgraded()).unwrap();
    let original = bytes(history.document());
    for hex in ["#111111", "#222222", "#333333"] {
        history.apply(&set_accent(hex)).unwrap();
    }
    assert_eq!(history.undo_depth(), 1, "three drags are one step");
    assert_eq!(history.document().tokens.colors["accent"], "#333333");
    history.undo().unwrap();
    assert_eq!(bytes(history.document()), original);
    history.redo().unwrap();
    assert_eq!(history.document().tokens.colors["accent"], "#333333");
}

#[test]
fn coalescing_does_not_cross_other_batches_or_undo() {
    let mut history = History::new(upgraded()).unwrap();
    history.apply(&set_accent("#111111")).unwrap();
    history
        .apply(&batch(vec![Command::SetSiteMeta {
            name: Some("N".into()),
            locale: None,
            base_url: None,
        }]))
        .unwrap();
    history.apply(&set_accent("#222222")).unwrap();
    assert_eq!(history.undo_depth(), 3);

    history.undo().unwrap();
    history.apply(&set_accent("#444444")).unwrap();
    assert_eq!(history.undo_depth(), 3, "undo seals the previous step");
    assert!(!history.can_redo(), "new work clears redo");
}

#[test]
fn coalescing_non_overwrite_commands_chains_inverses() {
    let doc = upgraded();
    let main = uid_of(&doc, "pages[0].body[0]");
    let mut history = History::new(doc).unwrap();
    let original = bytes(history.document());
    for n in 0..3 {
        let mut b = batch(vec![Command::InsertNode {
            container: ContainerRef::Node {
                uid: main.clone(),
                slot: None,
            },
            index: None,
            node: text(&format!("typed.{n}"), "x"),
        }]);
        b.coalesce_key = Some("typing".into());
        history.apply(&b).unwrap();
    }
    assert_eq!(history.undo_depth(), 1);
    history.undo().unwrap();
    assert_eq!(bytes(history.document()), original);
    let after_typing = {
        let mut replay = History::new(upgraded()).unwrap();
        for n in 0..3 {
            replay
                .apply(&batch(vec![Command::InsertNode {
                    container: ContainerRef::Node {
                        uid: uid_of(replay.document(), "pages[0].body[0]"),
                        slot: None,
                    },
                    index: None,
                    node: text(&format!("typed.{n}"), "x"),
                }]))
                .unwrap();
        }
        bytes(replay.document())
    };
    history.redo().unwrap();
    assert_eq!(
        bytes(history.document()),
        after_typing,
        "redo restores the full typing sequence byte for byte"
    );
}

#[test]
fn history_limit_drops_oldest_steps() {
    let mut history = History::with_limit(upgraded(), 2).unwrap();
    for n in 0..5 {
        history
            .apply(&batch(vec![Command::SetDataValue {
                key: "n".into(),
                value: json!(n),
            }]))
            .unwrap();
    }
    assert_eq!(history.undo_depth(), 2);
}

#[test]
fn touched_fields_are_id_addressed_and_conflict_aware() {
    let doc = upgraded();
    let menu = uid_of(&doc, "pages[0].body[0].children[1]");
    let (_, tx) = apply_batch(
        &doc,
        &batch(vec![
            Command::SetAttr {
                uid: menu.clone(),
                attr: "class".into(),
                value: Some("x".into()),
            },
            Command::SetColorToken {
                key: "accent".into(),
                hex: "#000000".into(),
            },
            Command::MovePage {
                id: "about".into(),
                index: 0,
            },
        ]),
    )
    .unwrap();
    assert_eq!(
        tx.touched,
        vec![
            "node:home.menu/attrs/class".to_string(),
            "pages".to_string(),
            "tokens/colors/accent".to_string(),
        ]
    );
    let mut sorted = tx.touched.clone();
    sorted.sort();
    assert_eq!(tx.touched, sorted);

    let other_attr = vec!["node:home.menu/attrs/id".to_string()];
    let whole_node = vec!["node:home.menu".to_string()];
    let sibling = vec!["node:home.menu-2".to_string()];
    assert!(!touched_conflict(&tx.touched, &other_attr));
    assert!(touched_conflict(&tx.touched, &whole_node));
    assert!(!touched_conflict(&tx.touched, &sibling));
    assert!(touched_conflict(&tx.touched, &["pages".to_string()]));
}

// ---- lossless save ---------------------------------------------------------

#[test]
fn lossy_sources_are_refused_with_paths_and_extensions_survive() {
    let mut raw = serde_json::to_value(upgraded()).unwrap();
    raw["x_ext"] = json!({ "kept": false });
    raw["pages"][0]["future"] = json!(1);
    raw["pages"][0]["body"][0]["unknown"] = json!("?");
    let source = serde_json::to_string(&raw).unwrap();
    let b = r#"{"version":1,"commands":[{"op":"set_data_value","key":"k","value":1}]}"#;
    let err = apply_batch_json(&source, b, EditOptions::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::LossyDocument);
    assert_eq!(
        err.paths,
        vec!["$.pages[0].body[0].unknown", "$.pages[0].future", "$.x_ext"]
    );
    let upgraded_err = upgrade_json(&source, EditOptions::default()).unwrap_err();
    assert_eq!(upgraded_err.code, ErrorCode::LossyDocument);

    let out = apply_batch_json(&source, b, EditOptions { allow_lossy: true }).unwrap();
    assert_eq!(out.dropped_paths.len(), 3);

    // Supported extension containers round trip without being reported.
    let clean = serde_json::to_string(&upgraded()).unwrap();
    let out = apply_batch_json(&clean, b, EditOptions::default()).unwrap();
    let saved: Value = serde_json::from_str(&out.document).unwrap();
    assert_eq!(saved["metadata"]["note"], json!({ "a": 2, "z": 1 }));
    assert_eq!(saved["data"]["k"], json!(1));
    assert!(out.dropped_paths.is_empty());
}

#[test]
fn lossless_check_treats_null_and_number_forms_as_equal() {
    let original = json!({
        "schema_version": 1, "slug": "s", "name": "n", "base_url": "https://s.example",
        "pages": [], "data": { "n": 1.0, "e": 1e3, "nil": null }
    });
    let doc: SiteDocument = serde_json::from_value(original.clone()).unwrap();
    check_lossless(&original, &doc).expect("equivalent numbers and nulls are lossless");
}

#[test]
fn json_entry_points_report_typed_parse_errors() {
    let err = upgrade_json("{not json", EditOptions::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidJson);
    let err = apply_batch_json("{}", "{}", EditOptions::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidJson);
}

#[test]
fn batch_and_transaction_json_round_trip() {
    let doc = upgraded();
    let main = uid_of(&doc, "pages[0].body[0]");
    let b = batch(vec![Command::InsertNode {
        container: ContainerRef::Node {
            uid: main,
            slot: None,
        },
        index: Some(0),
        node: element("rt.a", "p", vec![text("rt.b", "hi")]),
    }]);
    let text = serde_json::to_string(&b).unwrap();
    assert_eq!(parse_batch(&text).unwrap(), b);
    let (_, tx) = apply_batch(&doc, &b).unwrap();
    let tx2: Transaction = serde_json::from_str(&serde_json::to_string(&tx).unwrap()).unwrap();
    assert_eq!(tx, tx2);
}

// ---- review hardening ------------------------------------------------------

fn nested_inserts(parent: &str, levels: usize) -> CommandBatch {
    let mut commands = Vec::new();
    let mut parent = parent.to_string();
    for i in 0..levels {
        let uid = format!("deep.{i}");
        commands.push(Command::InsertNode {
            container: ContainerRef::Node {
                uid: parent.clone(),
                slot: None,
            },
            index: None,
            node: element(&uid, "div", Vec::new()),
        });
        parent = uid;
    }
    batch(commands)
}

#[test]
fn coalesced_steps_never_exceed_the_batch_cap_and_undo_redo_stay_exact() {
    let doc = upgraded();
    let main = uid_of(&doc, "pages[0].body[0]");
    let intro = uid_of(&doc, "pages[0].body[0].children[0]");
    let mut history = History::new(doc).unwrap();
    let original = bytes(history.document());
    let total = MAX_BATCH_COMMANDS + 203;
    for n in 0..total {
        let mut b = batch(vec![Command::MoveNode {
            uid: intro.clone(),
            container: ContainerRef::Node {
                uid: main.clone(),
                slot: None,
            },
            index: Some(n % 3),
        }]);
        b.coalesce_key = Some("drag".into());
        history.apply(&b).unwrap();
    }
    assert_eq!(history.undo_depth(), 2, "1000 merged + the overflow step");
    let final_bytes = bytes(history.document());
    history.undo().unwrap().expect("undo overflow step");
    history.undo().unwrap().expect("undo capped step");
    assert_eq!(bytes(history.document()), original);
    assert!(!history.can_undo());
    history.redo().unwrap().expect("redo capped step");
    history.redo().unwrap().expect("redo overflow step");
    assert_eq!(bytes(history.document()), final_bytes);
}

#[test]
fn editing_refuses_documents_that_could_not_be_parsed_again() {
    // serde_json refuses input nested deeper than 127 containers.
    let nest = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    assert!(serde_json::from_str::<Value>(&nest(MAX_EDIT_JSON_DEPTH)).is_ok());
    assert!(serde_json::from_str::<Value>(&nest(MAX_EDIT_JSON_DEPTH + 1)).is_err());

    let doc = upgraded();
    let main = uid_of(&doc, "pages[0].body[0]");
    let source = bytes(&doc);
    // validate_ir alone would accept 120 nested elements; the saved file would
    // not parse. The whole batch is refused with a typed error.
    let deep = nested_inserts(&main, 120);
    let err = apply_batch(&doc, &deep).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidDocument);
    assert!(err.message.contains("JSON levels"), "{err}");
    let deep_json = serde_json::to_string(&deep).unwrap();
    let err = apply_batch_json(&source, &deep_json, EditOptions::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidDocument);

    // Find the real boundary: the deepest accepted result re-parses, the next is refused.
    let mut accepted = 0;
    for levels in 1..=120 {
        let json = serde_json::to_string(&nested_inserts(&main, levels)).unwrap();
        match apply_batch_json(&source, &json, EditOptions::default()) {
            Ok(out) => {
                accepted = levels;
                serde_json::from_str::<SiteDocument>(&out.document)
                    .expect("accepted output must re-parse");
            }
            Err(e) => {
                assert_eq!(e.code, ErrorCode::InvalidDocument, "{e}");
                break;
            }
        }
    }
    assert!((40..120).contains(&accepted), "boundary at {accepted}");

    // A document nested exactly at the limit is editable; one level deeper is not.
    let mut at_limit = upgraded();
    let mut value = json!(0);
    for _ in 0..(MAX_EDIT_JSON_DEPTH - 2) {
        value = json!([value]);
    }
    at_limit.data.insert("k".into(), value.clone());
    check_editable(&at_limit).expect("at the limit");
    at_limit.data.insert("k".into(), json!([value]));
    assert_eq!(
        check_editable(&at_limit).unwrap_err().code,
        ErrorCode::InvalidDocument
    );
}

fn legacy_shaped_doc() -> SiteDocument {
    let mut doc = upgraded();
    doc.tokens.fonts.heading = Some(String::new());
    doc.tokens.fonts.mono = Some("   ".to_string());
    if let Node::Element { tag, .. } = &mut doc.pages[0].body[0] {
        *tag = "svg:main".to_string();
    }
    doc
}

#[test]
fn inverses_restore_values_the_forward_grammar_would_not_accept() {
    let doc = legacy_shaped_doc();
    check_editable(&doc).expect("legacy-shaped values pass the editing gate");
    let original = bytes(&doc);
    let main = uid_of(&doc, "pages[0].body[0]");
    let intro = uid_of(&doc, "pages[0].body[0].children[0]");
    let cases = vec![
        (
            "set heading over an empty stack",
            Command::SetFontToken {
                slot: "heading".into(),
                stack: "Georgia, serif".into(),
            },
        ),
        (
            "set mono over a blank stack",
            Command::SetFontToken {
                slot: "mono".into(),
                stack: "ui-monospace".into(),
            },
        ),
        (
            "clear an empty heading stack",
            Command::ClearFontToken {
                slot: "heading".into(),
            },
        ),
        (
            "clear a blank mono stack",
            Command::ClearFontToken {
                slot: "mono".into(),
            },
        ),
        (
            "retag a tag outside the new-input grammar",
            Command::SetTag {
                uid: main.clone(),
                tag: "main".into(),
            },
        ),
        (
            "svg tag with an uppercase letter",
            Command::SetTag {
                uid: intro.clone(),
                tag: "clipPath".into(),
            },
        ),
    ];
    for (name, command) in cases {
        let (next, tx) =
            apply_batch(&doc, &batch(vec![command])).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (restored, _) = apply_batch_restore(&next, &tx.inverse_batch())
            .unwrap_or_else(|e| panic!("{name} restore: {e}"));
        assert_eq!(bytes(&restored), original, "{name}: restore != identity");

        let mut history = History::new(doc.clone()).unwrap();
        history.apply(&batch(vec![tx.forward[0].clone()])).unwrap();
        history.undo().unwrap().unwrap_or_else(|| panic!("{name}"));
        assert_eq!(bytes(history.document()), original, "{name}: undo");
        history.redo().unwrap().unwrap();
        assert_eq!(bytes(history.document()), bytes(&next), "{name}: redo");
    }
    // Forward input is still validated strictly.
    for command in [
        Command::SetFontToken {
            slot: "heading".into(),
            stack: " ".into(),
        },
        Command::SetTag {
            uid: main,
            tag: "bad tag".into(),
        },
    ] {
        assert_eq!(
            apply_batch(&doc, &batch(vec![command])).unwrap_err().code,
            ErrorCode::InvalidValue
        );
    }
}

#[test]
fn set_tag_accepts_what_the_compiler_accepts() {
    let doc = upgraded();
    let intro = uid_of(&doc, "pages[0].body[0].children[0]");
    for tag in ["clipPath", "linearGradient", "my_el", "h1", "x-y"] {
        let (next, tx) = apply_batch(
            &doc,
            &batch(vec![Command::SetTag {
                uid: intro.clone(),
                tag: tag.into(),
            }]),
        )
        .unwrap_or_else(|e| panic!("{tag}: {e}"));
        let (back, _) = apply_batch(&next, &tx.inverse_batch()).unwrap();
        assert_eq!(bytes(&back), bytes(&doc));
    }
    for tag in ["", "1a", "a b", "a<b", "-x"] {
        assert!(
            apply_batch(
                &doc,
                &batch(vec![Command::SetTag {
                    uid: intro.clone(),
                    tag: tag.into(),
                }])
            )
            .is_err(),
            "{tag:?} must be refused"
        );
    }
}

#[test]
fn embedded_payloads_are_strict_too() {
    let doc = upgraded();
    let main = uid_of(&doc, "pages[0].body[0]");
    let base = batch(vec![
        Command::InsertNode {
            container: ContainerRef::Node {
                uid: main,
                slot: None,
            },
            index: None,
            node: element("p.a", "p", vec![text("p.b", "x")]),
        },
        Command::UpsertPage {
            page: random_page("fresh"),
        },
        Command::UpsertStyleRule {
            rule: crate::types::IrStyleRule {
                id: "r".into(),
                selector: ".r".into(),
                breakpoint: None,
                pseudo: None,
                declarations: Default::default(),
            },
        },
    ]);
    let mut raw = serde_json::to_value(&base).unwrap();
    parse_batch(&raw.to_string()).expect("the clean batch parses");

    raw["commands"][0]["node"]["chidren"] = json!([]);
    raw["commands"][0]["node"]["children"][0]["extra"] = json!(null);
    raw["commands"][1]["page"]["bogus"] = json!(1);
    raw["commands"][2]["rule"]["seletor"] = json!(".x");
    let err = parse_batch(&raw.to_string()).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidJson);
    assert_eq!(
        err.paths,
        vec![
            "$.commands[0].node.chidren",
            "$.commands[0].node.children[0].extra",
            "$.commands[1].page.bogus",
            "$.commands[2].rule.seletor",
        ]
    );
    // Through the JSON entry point nothing is applied.
    let err = apply_batch_json(&bytes(&doc), &raw.to_string(), EditOptions::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidJson);

    // Known optional fields may be explicit nulls.
    let mut nulls = serde_json::to_value(&base).unwrap();
    nulls["commands"][1]["page"]["description"] = json!(null);
    nulls["label"] = json!(null);
    parse_batch(&nulls.to_string()).expect("known optional nulls are fine");
}

#[test]
fn legacy_patch_ops_keep_working_on_unmarked_documents_and_are_guarded_on_marked_ones() {
    use crate::patch::{IrError, IrPatch, apply_patch};

    let with_id = |mut doc: SiteDocument| {
        if let Node::Element { children, .. } = &mut doc.pages[0].body[0]
            && let Node::Element { id, .. } = &mut children[1]
        {
            *id = Some("hero".to_string());
        }
        doc
    };
    let insert = IrPatch::InsertChildNode {
        page_id: "home".into(),
        parent_id: Some("hero".into()),
        index: None,
        node: Node::Text {
            uid: None,
            value: "added".into(),
        },
    };
    let set_text = IrPatch::SetNodeText {
        page_id: "home".into(),
        node_id: "hero".into(),
        text: "replaced".into(),
    };
    let remove = IrPatch::RemoveNode {
        page_id: "home".into(),
        node_id: "hero".into(),
    };

    // Unmarked: exactly the historic behaviour.
    let mut unmarked = with_id(legacy_doc());
    apply_patch(&mut unmarked, &insert).unwrap();
    apply_patch(&mut unmarked, &set_text).unwrap();
    let json = serde_json::to_value(&unmarked).unwrap();
    assert_eq!(
        json["pages"][0]["body"][0]["children"][1]["children"],
        json!([{ "type": "text", "value": "replaced" }])
    );
    apply_patch(&mut unmarked, &remove).unwrap();
    assert!(
        !serde_json::to_string(&unmarked)
            .unwrap()
            .contains("\"uid\"")
    );

    // Marked: node-level ops that add or destroy nodes are refused, untouched.
    let mut marked = with_id(upgraded());
    let before = bytes(&marked);
    for patch in [&insert, &set_text, &remove] {
        match apply_patch(&mut marked, patch).unwrap_err() {
            IrError::InvalidValue { message, .. } => {
                assert!(message.contains("edit command batch"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let uidless_page = IrPatch::UpsertPage {
        page: {
            let mut page = random_page("legacy");
            page.body = vec![Node::Text {
                uid: None,
                value: "no uid".into(),
            }];
            page
        },
    };
    assert!(apply_patch(&mut marked, &uidless_page).is_err());
    assert_eq!(bytes(&marked), before, "refused ops change nothing");

    // Fully identified but colliding or malformed uids are refused and rolled back,
    // both when replacing an existing page and when adding a new one.
    let reuse_uid = |page_id: &str, uid: String| IrPatch::UpsertPage {
        page: {
            let mut page = random_page(page_id);
            page.body = vec![Node::Text {
                uid: Some(uid),
                value: "x".into(),
            }];
            page
        },
    };
    let taken = marked.pages[0].body[0]
        .uid()
        .expect("upgraded doc has uids")
        .to_string();
    for patch in [
        reuse_uid("fresh", taken.clone()),
        reuse_uid("fresh", "Bad UID!".into()),
        reuse_uid(
            &marked.pages[1 % marked.pages.len()].id.clone(),
            taken.clone(),
        ),
    ] {
        match apply_patch(&mut marked, &patch).unwrap_err() {
            IrError::InvalidValue { message, .. } => {
                assert!(message.contains("stable node IDs"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(bytes(&marked), before, "rejected upsert rolls back");
    }

    // Ops that cannot damage uids keep working and the document stays editable.
    apply_patch(
        &mut marked,
        &IrPatch::SetNodeAttr {
            page_id: "home".into(),
            node_id: "hero".into(),
            attr: "data-x".into(),
            value: Some("1".into()),
        },
    )
    .unwrap();
    apply_patch(
        &mut marked,
        &IrPatch::UpsertPage {
            page: random_page("with-uids"),
        },
    )
    .unwrap();
    apply_patch(
        &mut marked,
        &IrPatch::SetColorToken {
            key: "accent".into(),
            hex: "#010203".into(),
        },
    )
    .unwrap();
    apply_patch(
        &mut marked,
        &IrPatch::RemovePage {
            id: "with-uids".into(),
        },
    )
    .unwrap();
    check_editable(&marked).expect("still editable");
}

#[test]
fn lossless_check_reports_what_both_parses_agree_on() {
    let clean = serde_json::to_string(&upgraded()).unwrap();
    let with_data = |data: &str| {
        clean.replacen(
            "\"data\":{\"items\":[\"a\",\"b\"]}",
            &format!("\"data\":{data}"),
            1,
        )
    };
    let paths_for = |text: &str| -> Vec<String> {
        let err = upgrade_json(text, EditOptions::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::LossyDocument, "{err}");
        err.paths
    };

    // Integers beyond i64/u64 and over-precise floats.
    let paths = paths_for(&with_data(r#"{"big":123456789012345678901234567890}"#));
    assert_eq!(
        paths,
        vec!["$.data.big (number 123456789012345678901234567890 loses precision)"]
    );
    let paths = paths_for(&with_data(r#"{"p":0.1000000000000000055511151231257827}"#));
    assert!(paths[0].starts_with("$.data.p (number"), "{paths:?}");
    assert!(!paths_for(&with_data(r#"{"neg":-9223372036854775809}"#)).is_empty());
    // Numbers that are exact in some other spelling are fine.
    for ok in [
        "1.0",
        "1e3",
        "0.10",
        "18446744073709551615",
        "-9223372036854775808",
        "2.5E-3",
    ] {
        upgrade_json(
            &with_data(&format!("{{\"n\":{ok}}}")),
            EditOptions::default(),
        )
        .unwrap_or_else(|e| panic!("{ok}: {e}"));
    }

    // Duplicate keys: the last value would silently win.
    let paths = paths_for(&with_data(r#"{"k":1,"k":2}"#));
    assert_eq!(paths, vec!["$.data.k (duplicate key)"]);

    // Unknown keys holding null are dropped on save, so they are reported;
    // known optional nulls are not.
    let mut raw = serde_json::to_value(upgraded()).unwrap();
    raw["pages"][0]["future_null"] = json!(null);
    let paths = paths_for(&raw.to_string());
    assert_eq!(paths, vec!["$.pages[0].future_null"]);

    let mut known = serde_json::to_value(upgraded()).unwrap();
    known["pages"][0]["body"][0]["uid"] = json!(null);
    known["pages"][0]["description"] = json!(null);
    known["pages"][0]["canonical"] = json!(null);
    let doc: SiteDocument = serde_json::from_value(known.clone()).unwrap();
    assert!(lossy_paths(&known, &doc).is_empty());

    // --allow-lossy reports them instead of failing.
    let out = upgrade_json(
        &with_data(r#"{"k":1,"k":2}"#),
        EditOptions { allow_lossy: true },
    )
    .unwrap();
    assert_eq!(out.dropped_paths, vec!["$.data.k (duplicate key)"]);

    // The same scan guards command batches (data values, node payloads).
    let dup =
        r#"{"version":1,"commands":[{"op":"set_data_value","key":"k","value":{"a":1,"a":2}}]}"#;
    assert_eq!(parse_batch(dup).unwrap_err().code, ErrorCode::InvalidJson);
    let big = r#"{"version":1,"commands":[{"op":"set_data_value","key":"k","value":340282366920938463463374607431768211456}]}"#;
    assert_eq!(parse_batch(big).unwrap_err().code, ErrorCode::InvalidJson);
    let unknown_null =
        r#"{"version":1,"commands":[{"op":"set_data_value","key":"k","value":1}],"x":null}"#;
    assert_eq!(
        parse_batch(unknown_null).unwrap_err().code,
        ErrorCode::InvalidJson
    );
}

#[test]
fn next_id_is_the_smallest_free_numbered_id() {
    let mut doc = upgraded();
    assert_eq!(next_id(&doc, "x"), "x-1");
    assert_eq!(next_id(&doc, "home.menu"), "home.menu-1");
    let main = uid_of(&doc, "pages[0].body[0]");
    for uid in ["x-1", "x-3"] {
        let (next, _) = apply_batch(
            &doc,
            &batch(vec![Command::InsertNode {
                container: ContainerRef::Node {
                    uid: main.clone(),
                    slot: None,
                },
                index: None,
                node: text(uid, "t"),
            }]),
        )
        .unwrap();
        doc = next;
    }
    assert_eq!(next_id(&doc, "x"), "x-2", "gaps are reused, smallest first");
    assert_eq!(next_id(&doc, "x"), next_id(&doc.clone(), "x"));
    // Page ids, style ids and component names count as used too.
    doc.pages[1].id = "p-1".to_string();
    assert_eq!(next_id(&doc, "p"), "p-2");
    doc.styles[0].id = "s-1".to_string();
    assert_eq!(next_id(&doc, "s"), "s-2");
    assert_eq!(next_id(&doc, "card"), "card-1");
    // The result is accepted by insert_node when the prefix is a valid uid.
    let fresh = next_id(&doc, "x");
    apply_batch(
        &doc,
        &batch(vec![Command::InsertNode {
            container: ContainerRef::Node {
                uid: main,
                slot: None,
            },
            index: None,
            node: text(&fresh, "t"),
        }]),
    )
    .unwrap();
}

// ---- lossless null / number soundness -----------------------------------------

#[test]
fn batch_with_many_explicit_nulls_is_accepted() {
    let uid = uid_of(&upgraded(), "pages[0].body[0]");
    let cmds: Vec<Value> = (0..300)
        .map(|i| json!({ "op": "set_attr", "uid": uid, "attr": format!("a{i}"), "value": null }))
        .collect();
    let text = json!({ "version": 1, "commands": cmds }).to_string();
    parse_batch(&text).expect("300 explicit nulls on a known optional field");

    // A genuinely lost null after the same shape is still reported.
    let mut cmds: Vec<Value> = (0..300)
        .map(|i| json!({ "op": "set_attr", "uid": uid, "attr": format!("a{i}"), "value": null }))
        .collect();
    cmds[299]["bogus"] = json!(null);
    let text = json!({ "version": 1, "commands": cmds }).to_string();
    assert_eq!(parse_batch(&text).unwrap_err().code, ErrorCode::InvalidJson);
}

#[test]
fn document_with_many_uid_nulls_upgrades_and_unknown_null_is_lossy() {
    let mut raw = serde_json::to_value(legacy_doc()).unwrap();
    let nodes: Vec<Value> = (0..300)
        .map(|i| json!({ "type": "text", "value": format!("t{i}"), "uid": null }))
        .collect();
    raw["pages"][0]["body"] = Value::Array(nodes);
    upgrade_json(&raw.to_string(), EditOptions::default()).expect("300 uid nulls upgrade");

    raw["pages"][0]["body"][299]["future"] = json!(null);
    let err = upgrade_json(&raw.to_string(), EditOptions::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::LossyDocument);
    assert_eq!(err.paths, vec!["$.pages[0].body[299].future"]);
}

#[test]
fn integers_must_round_trip_exactly_through_f64_fields() {
    let pair = |ratio: &str| {
        let mut raw = serde_json::to_value(upgraded()).unwrap();
        raw["tokens"]["contrast_pairs"] = json!([{ "foreground": "a", "background": "b" }]);
        let text = serde_json::to_string(&raw).unwrap().replace(
            "\"background\":\"b\"",
            &format!("\"background\":\"b\",\"min_ratio\":{ratio}"),
        );
        upgrade_json(&text, EditOptions::default())
    };
    let err = pair("9007199254740993").unwrap_err();
    assert_eq!(err.code, ErrorCode::LossyDocument, "{err}");
    pair("4.5").expect("ordinary floats are fine");
    pair("7").expect("small integers round-trip");
    pair("9007199254740992").expect("2^53 is exactly representable");
}
