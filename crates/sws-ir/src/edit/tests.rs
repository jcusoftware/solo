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
            "fonts": { "body": "system-ui, sans-serif" },
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
fn upgrade_rejects_unknown_marker_scheme_and_flags_stale_manifest() {
    let mut doc = legacy_doc();
    doc.metadata
        .insert(MARKER_KEY.to_string(), json!({ "uid_scheme": 2 }));
    let err = assign_stable_ids(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedUidScheme);

    let mut with_manifest = legacy_doc();
    with_manifest
        .metadata
        .insert("venue_manifest".to_string(), json!({ "nap": {} }));
    let (_, report) = assign_stable_ids(&with_manifest).unwrap();
    assert!(report.venue_manifest_stale);
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
            "set_font_token heading (new)",
            Command::SetFontToken {
                slot: "heading".into(),
                stack: "Georgia, serif".into(),
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
    let mut states = vec![bytes(history.document())];
    let mut counter = 0;
    for _ in 0..300 {
        let mut uids = Vec::new();
        crate::edit::ids::for_each_node(history.document(), &mut |n, _| {
            uids.push(n.uid().unwrap().to_string());
        });
        let pick = |n: &mut dyn FnMut(usize) -> usize, v: &[String]| v[n(v.len())].clone();
        counter += 1;
        let selector = if uids.len() < 6 { 4 + next(2) } else { next(6) };
        let command = match selector {
            0 => Command::RemoveNode {
                uid: pick(&mut next, &uids),
            },
            1 => Command::MoveNode {
                uid: pick(&mut next, &uids),
                container: ContainerRef::Node {
                    uid: pick(&mut next, &uids),
                    slot: None,
                },
                index: if next(2) == 0 { None } else { Some(next(4)) },
            },
            2 => Command::InsertNode {
                container: ContainerRef::Node {
                    uid: pick(&mut next, &uids),
                    slot: None,
                },
                index: if next(2) == 0 { None } else { Some(next(3)) },
                node: element(
                    &format!("gen.{counter}"),
                    "div",
                    vec![text(&format!("gen.{counter}.t"), "t")],
                ),
            },
            3 => Command::SetAttr {
                uid: pick(&mut next, &uids),
                attr: "data-k".into(),
                value: Some(format!("v{counter}")),
            },
            4 => Command::SetColorToken {
                key: format!("c{}", next(3)),
                hex: "#0A0B0C".into(),
            },
            _ => Command::MovePage {
                id: "home".into(),
                index: next(2),
            },
        };
        let before = bytes(history.document());
        match history.apply(&batch(vec![command])) {
            Ok(_) => {
                check_uids(history.document()).expect("uids stay unique");
                states.push(bytes(history.document()));
            }
            Err(_) => assert_eq!(
                bytes(history.document()),
                before,
                "failed apply must not mutate"
            ),
        }
    }
    assert!(states.len() > 50, "too few valid steps: {}", states.len());
    for expected in states.iter().rev().skip(1) {
        history.undo().unwrap().unwrap();
        assert_eq!(&bytes(history.document()), expected);
    }
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
    history.redo().unwrap();
    assert_eq!(history.document().pages[0].body.len(), 1);
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
