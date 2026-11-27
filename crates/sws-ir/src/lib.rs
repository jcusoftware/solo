//! Solo Website System (SWS) Site Intermediate Representation.
//!
//! Domain-agnostic, deterministic (`BTreeMap`-backed), and `wasm32`-clean IR
//! for describing static and edge-rendered sites, design tokens, components,
//! forms, patches, validation, and JSON Schema metadata.

pub mod edit;
pub mod patch;
pub mod schema;
pub mod types;
pub mod validate;

pub use patch::{IrError, IrPatch, apply_patch};
pub use schema::site_document_json_schema;
pub use types::{
    Breakpoint, ComponentDef, ContrastPair, DesignTokens, FontTokens, FormFieldKind,
    FormFieldOption, FormFieldSchema, FormSchema, IrStyleRule, JsonLdBlock, Node, Page, PropKind,
    PropSpec, RoutePattern, ScriptRef, SeoMeta, SiteDocument,
};
pub use validate::{
    IrValidationError, is_valid_dynamic_route, is_valid_hex_color, is_valid_https_url,
    is_valid_slug, is_valid_static_route, validate_ir,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    fn sample_doc() -> SiteDocument {
        let mut colors = BTreeMap::new();
        colors.insert("ink".to_string(), "#1A1A1A".to_string());
        colors.insert("paper".to_string(), "#F7F5F0".to_string());
        colors.insert("accent".to_string(), "#0B5FFF".to_string());

        let mut spacing = BTreeMap::new();
        spacing.insert("sm".to_string(), "0.5rem".to_string());
        spacing.insert("md".to_string(), "1rem".to_string());

        let mut radii = BTreeMap::new();
        radii.insert("card".to_string(), "0.5rem".to_string());

        let mut card_props = BTreeMap::new();
        card_props.insert(
            "heading".to_string(),
            PropSpec {
                kind: PropKind::String,
                required: true,
                default: None,
                description: Some("Section heading".to_string()),
            },
        );

        let mut components = BTreeMap::new();
        components.insert(
            "card".to_string(),
            ComponentDef {
                name: "card".to_string(),
                props: card_props,
                slots: vec!["default".to_string()],
                template: vec![Node::Element {
                    uid: None,
                    id: Some("card-root".to_string()),
                    tag: "section".to_string(),
                    attrs: BTreeMap::new(),
                    classes: vec!["card".to_string()],
                    style_refs: vec!["card-box".to_string()],
                    children: vec![Node::SlotRef {
                        uid: None,
                        name: "default".to_string(),
                        fallback: vec![Node::Text {
                            uid: None,
                            value: "Fallback".to_string(),
                        }],
                    }],
                }],
            },
        );

        let mut forms = BTreeMap::new();
        forms.insert(
            "contact".to_string(),
            FormSchema {
                id: "contact".to_string(),
                action: "/api/contact".to_string(),
                method: "POST".to_string(),
                turnstile_action: "contact".to_string(),
                fields: vec![
                    FormFieldSchema {
                        name: "email".to_string(),
                        label: "Email".to_string(),
                        kind: FormFieldKind::Email,
                        required: true,
                        placeholder: Some("you@example.test".to_string()),
                        help_text: None,
                        default_value: None,
                        options: Vec::new(),
                        max_chars: Some(254),
                        min: None,
                        max: None,
                        autocomplete: Some("email".to_string()),
                    },
                    FormFieldSchema {
                        name: "topic".to_string(),
                        label: "Topic".to_string(),
                        kind: FormFieldKind::Select,
                        required: true,
                        placeholder: None,
                        help_text: None,
                        default_value: Some("general".to_string()),
                        options: vec![FormFieldOption {
                            value: "general".to_string(),
                            label: "General inquiry".to_string(),
                        }],
                        max_chars: None,
                        min: None,
                        max: None,
                        autocomplete: None,
                    },
                ],
            },
        );

        let mut rule_decls = BTreeMap::new();
        rule_decls.insert("padding".to_string(), "var(--space-md)".to_string());

        let mut comp_props = BTreeMap::new();
        comp_props.insert("heading".to_string(), json!("Welcome"));

        let mut comp_slots = BTreeMap::new();
        comp_slots.insert(
            "default".to_string(),
            vec![Node::Element {
                uid: None,
                id: Some("hero-copy".to_string()),
                tag: "p".to_string(),
                attrs: BTreeMap::new(),
                classes: Vec::new(),
                style_refs: Vec::new(),
                children: vec![Node::Text {
                    uid: None,
                    value: "Hello world".to_string(),
                }],
            }],
        );

        let mut data = BTreeMap::new();
        data.insert("show_banner".to_string(), json!(true));
        data.insert("items".to_string(), json!(["alpha", "beta"]));

        let mut metadata = BTreeMap::new();
        metadata.insert("generator".to_string(), json!("sws-ir-test"));

        SiteDocument {
            schema_version: 1,
            slug: "acme-studio-demo".to_string(),
            name: "Acme Studio".to_string(),
            locale: "en-US".to_string(),
            base_url: "https://acme.example".to_string(),
            tokens: DesignTokens {
                colors,
                fonts: FontTokens {
                    body: "Inter, system-ui, sans-serif".to_string(),
                    heading: Some("Fraunces, serif".to_string()),
                    mono: Some("JetBrains Mono, monospace".to_string()),
                },
                spacing,
                radii,
                contrast_pairs: vec![ContrastPair {
                    foreground: "ink".to_string(),
                    background: "paper".to_string(),
                    min_ratio: 4.5,
                    usage: Some("body text".to_string()),
                }],
            },
            breakpoints: vec![Breakpoint {
                id: "md".to_string(),
                media_query: "(min-width: 48rem)".to_string(),
                min_width: Some("48rem".to_string()),
            }],
            styles: vec![IrStyleRule {
                id: "card-box".to_string(),
                selector: ".card".to_string(),
                breakpoint: Some("md".to_string()),
                pseudo: None,
                declarations: rule_decls,
            }],
            components,
            forms,
            pages: vec![
                Page {
                    id: "home".to_string(),
                    route: RoutePattern::Static {
                        path: "/".to_string(),
                    },
                    title: "Home".to_string(),
                    description: Some("Landing page".to_string()),
                    canonical: Some("https://acme.example/".to_string()),
                    noindex: false,
                    is_not_found: false,
                    json_ld: vec![JsonLdBlock {
                        id: Some("org".to_string()),
                        data: json!({"@context": "https://schema.org", "@type": "Organization"}),
                    }],
                    scripts: vec![ScriptRef {
                        src: "/assets/app.js".to_string(),
                        async_load: false,
                        defer: true,
                    }],
                    body: vec![
                        Node::Element {
                            uid: None,
                            id: Some("main-title".to_string()),
                            tag: "h1".to_string(),
                            attrs: BTreeMap::from([(
                                "data-test".to_string(),
                                "hero-heading".to_string(),
                            )]),
                            classes: vec!["title".to_string()],
                            style_refs: Vec::new(),
                            children: vec![Node::Text {
                                uid: None,
                                value: "Acme Studio".to_string(),
                            }],
                        },
                        Node::ComponentInstance {
                            uid: None,
                            component: "card".to_string(),
                            props: comp_props,
                            slots: comp_slots,
                        },
                        Node::Conditional {
                            uid: None,
                            expr: "show_banner".to_string(),
                            then_nodes: vec![Node::Text {
                                uid: None,
                                value: "Open".to_string(),
                            }],
                            else_nodes: vec![Node::Text {
                                uid: None,
                                value: "Closed".to_string(),
                            }],
                        },
                        Node::Loop {
                            uid: None,
                            item_var: "item".to_string(),
                            collection_expr: "items".to_string(),
                            body: vec![Node::Element {
                                uid: None,
                                id: None,
                                tag: "li".to_string(),
                                attrs: BTreeMap::new(),
                                classes: Vec::new(),
                                style_refs: Vec::new(),
                                children: vec![Node::Text {
                                    uid: None,
                                    value: "{{item}}".to_string(),
                                }],
                            }],
                        },
                    ],
                },
                Page {
                    id: "article".to_string(),
                    route: RoutePattern::Dynamic {
                        pattern: "/articles/{slug}".to_string(),
                        params: vec!["slug".to_string()],
                    },
                    title: "Article".to_string(),
                    description: None,
                    canonical: None,
                    noindex: false,
                    is_not_found: false,
                    json_ld: Vec::new(),
                    scripts: Vec::new(),
                    body: Vec::new(),
                },
                Page {
                    id: "not-found".to_string(),
                    route: RoutePattern::Static {
                        path: "/404".to_string(),
                    },
                    title: "Not Found".to_string(),
                    description: None,
                    canonical: None,
                    noindex: true,
                    is_not_found: true,
                    json_ld: Vec::new(),
                    scripts: Vec::new(),
                    body: Vec::new(),
                },
            ],
            seo: SeoMeta {
                title_template: Some("%s | Acme Studio".to_string()),
                default_description: Some("Independent studio".to_string()),
                robots_allow: vec!["/".to_string()],
                robots_disallow: vec!["/api/".to_string()],
                sitemap_routes: vec!["/".to_string()],
            },
            scripts: vec![ScriptRef {
                src: "/assets/global.js".to_string(),
                async_load: true,
                defer: false,
            }],
            data,
            metadata,
        }
    }

    #[test]
    fn serde_round_trip_and_deterministic_json() {
        let doc_a = sample_doc();
        let json_a = serde_json::to_string_pretty(&doc_a).unwrap();
        let decoded: SiteDocument = serde_json::from_str(&json_a).unwrap();
        assert_eq!(doc_a, decoded);

        // Insert BTreeMap keys in reverse order to verify deterministic output.
        let mut doc_b = sample_doc();
        doc_b.tokens.colors.clear();
        doc_b
            .tokens
            .colors
            .insert("paper".to_string(), "#F7F5F0".to_string());
        doc_b
            .tokens
            .colors
            .insert("ink".to_string(), "#1A1A1A".to_string());
        doc_b
            .tokens
            .colors
            .insert("accent".to_string(), "#0B5FFF".to_string());

        let json_b = serde_json::to_string_pretty(&doc_b).unwrap();
        assert_eq!(json_a, json_b);
    }

    #[test]
    fn defaults_populate_when_optional_fields_omitted() {
        let minimal = json!({
            "slug": "minimal-site",
            "name": "Minimal",
            "base_url": "https://minimal.example",
            "pages": [
                {
                    "id": "home",
                    "route": { "kind": "static", "path": "/" },
                    "title": "Home"
                }
            ]
        });
        let doc: SiteDocument = serde_json::from_value(minimal).unwrap();
        assert_eq!(doc.schema_version, 1);
        assert_eq!(doc.locale, "en-US");
        assert!(validate_ir(&doc).is_empty());
    }

    #[test]
    fn apply_all_patch_operations() {
        let mut doc = sample_doc();

        // 1. SetSiteMeta
        apply_patch(
            &mut doc,
            &IrPatch::SetSiteMeta {
                name: Some("Acme Digital".to_string()),
                locale: Some("en-GB".to_string()),
                base_url: Some("https://acme-digital.example".to_string()),
            },
        )
        .unwrap();
        assert_eq!(doc.name, "Acme Digital");
        assert_eq!(doc.locale, "en-GB");
        assert_eq!(doc.base_url, "https://acme-digital.example");

        // 2. SetColorToken
        apply_patch(
            &mut doc,
            &IrPatch::SetColorToken {
                key: "accent".to_string(),
                hex: "#123456".to_string(),
            },
        )
        .unwrap();
        assert_eq!(doc.tokens.colors["accent"], "#123456");

        // 3. SetFontToken
        for (slot, stack) in [
            ("body", "Atkinson Hyperlegible, sans-serif"),
            ("heading", "Newsreader, serif"),
            ("mono", "IBM Plex Mono, monospace"),
        ] {
            apply_patch(
                &mut doc,
                &IrPatch::SetFontToken {
                    slot: slot.to_string(),
                    stack: stack.to_string(),
                },
            )
            .unwrap();
        }
        assert_eq!(doc.tokens.fonts.body, "Atkinson Hyperlegible, sans-serif");
        assert_eq!(
            doc.tokens.fonts.heading.as_deref(),
            Some("Newsreader, serif")
        );
        assert_eq!(
            doc.tokens.fonts.mono.as_deref(),
            Some("IBM Plex Mono, monospace")
        );

        let err = apply_patch(
            &mut doc,
            &IrPatch::SetFontToken {
                slot: "display".to_string(),
                stack: "serif".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            IrError::InvalidFontSlot {
                slot: "display".to_string()
            }
        );

        // 4. UpsertPage (update existing + insert new)
        let about_page = Page {
            id: "about".to_string(),
            route: RoutePattern::Static {
                path: "/about".to_string(),
            },
            title: "About".to_string(),
            description: None,
            canonical: None,
            noindex: false,
            is_not_found: false,
            json_ld: Vec::new(),
            scripts: Vec::new(),
            body: Vec::new(),
        };
        apply_patch(
            &mut doc,
            &IrPatch::UpsertPage {
                page: about_page.clone(),
            },
        )
        .unwrap();
        assert_eq!(doc.pages.len(), 4);

        let mut updated_about = about_page;
        updated_about.title = "About Us".to_string();
        apply_patch(
            &mut doc,
            &IrPatch::UpsertPage {
                page: updated_about,
            },
        )
        .unwrap();
        assert_eq!(doc.pages.len(), 4);
        assert_eq!(
            doc.pages.iter().find(|p| p.id == "about").unwrap().title,
            "About Us"
        );

        // 5. RemovePage
        apply_patch(
            &mut doc,
            &IrPatch::RemovePage {
                id: "about".to_string(),
            },
        )
        .unwrap();
        assert_eq!(doc.pages.len(), 3);
        assert!(matches!(
            apply_patch(
                &mut doc,
                &IrPatch::RemovePage {
                    id: "missing".to_string()
                }
            ),
            Err(IrError::PageNotFound { .. })
        ));

        // 6. SetNodeText (inside top-level element and inside ComponentInstance slot)
        apply_patch(
            &mut doc,
            &IrPatch::SetNodeText {
                page_id: "home".to_string(),
                node_id: "main-title".to_string(),
                text: "Updated Title".to_string(),
            },
        )
        .unwrap();
        apply_patch(
            &mut doc,
            &IrPatch::SetNodeText {
                page_id: "home".to_string(),
                node_id: "hero-copy".to_string(),
                text: "Updated Slot Copy".to_string(),
            },
        )
        .unwrap();

        // 7. SetNodeAttr (insert + remove)
        apply_patch(
            &mut doc,
            &IrPatch::SetNodeAttr {
                page_id: "home".to_string(),
                node_id: "main-title".to_string(),
                attr: "aria-label".to_string(),
                value: Some("Hero".to_string()),
            },
        )
        .unwrap();
        apply_patch(
            &mut doc,
            &IrPatch::SetNodeAttr {
                page_id: "home".to_string(),
                node_id: "main-title".to_string(),
                attr: "data-test".to_string(),
                value: None,
            },
        )
        .unwrap();
        let Node::Element { attrs, .. } = &doc.pages[0].body[0] else {
            panic!("expected element");
        };
        assert_eq!(attrs.get("aria-label").map(String::as_str), Some("Hero"));
        assert!(!attrs.contains_key("data-test"));

        // 8. InsertChildNode (into parent and into page root)
        apply_patch(
            &mut doc,
            &IrPatch::InsertChildNode {
                page_id: "home".to_string(),
                parent_id: Some("main-title".to_string()),
                index: Some(0),
                node: Node::Element {
                    uid: None,
                    id: Some("badge".to_string()),
                    tag: "span".to_string(),
                    attrs: BTreeMap::new(),
                    classes: Vec::new(),
                    style_refs: Vec::new(),
                    children: vec![Node::Text {
                        uid: None,
                        value: "New".to_string(),
                    }],
                },
            },
        )
        .unwrap();
        apply_patch(
            &mut doc,
            &IrPatch::InsertChildNode {
                page_id: "home".to_string(),
                parent_id: None,
                index: None,
                node: Node::Element {
                    uid: None,
                    id: Some("footer-note".to_string()),
                    tag: "p".to_string(),
                    attrs: BTreeMap::new(),
                    classes: Vec::new(),
                    style_refs: Vec::new(),
                    children: Vec::new(),
                },
            },
        )
        .unwrap();

        // Out-of-bounds index check
        assert!(matches!(
            apply_patch(
                &mut doc,
                &IrPatch::InsertChildNode {
                    page_id: "home".to_string(),
                    parent_id: None,
                    index: Some(999),
                    node: Node::Text {
                        uid: None,
                        value: "nope".to_string()
                    }
                }
            ),
            Err(IrError::InvalidIndex { .. })
        ));

        // 9. RemoveNode
        apply_patch(
            &mut doc,
            &IrPatch::RemoveNode {
                page_id: "home".to_string(),
                node_id: "badge".to_string(),
            },
        )
        .unwrap();
        apply_patch(
            &mut doc,
            &IrPatch::RemoveNode {
                page_id: "home".to_string(),
                node_id: "footer-note".to_string(),
            },
        )
        .unwrap();

        // 10. UpsertStyleRule
        let new_rule = IrStyleRule {
            id: "card-box".to_string(),
            selector: ".card".to_string(),
            breakpoint: None,
            pseudo: Some(":hover".to_string()),
            declarations: BTreeMap::from([("opacity".to_string(), "0.95".to_string())]),
        };
        apply_patch(&mut doc, &IrPatch::UpsertStyleRule { rule: new_rule }).unwrap();
        assert_eq!(doc.styles.len(), 1);
        assert_eq!(doc.styles[0].pseudo.as_deref(), Some(":hover"));

        // 11. SetDataValue
        apply_patch(
            &mut doc,
            &IrPatch::SetDataValue {
                key: "tagline".to_string(),
                value: json!("Built cleanly"),
            },
        )
        .unwrap();
        assert_eq!(doc.data["tagline"], json!("Built cleanly"));

        // Serde round-trip for IrPatch
        let patch = IrPatch::SetDataValue {
            key: "k".to_string(),
            value: json!(42),
        };
        let patch_json = serde_json::to_string(&patch).unwrap();
        assert!(patch_json.contains(r#""op":"set_data_value""#));
        let patch_rt: IrPatch = serde_json::from_str(&patch_json).unwrap();
        assert_eq!(patch, patch_rt);
    }

    #[test]
    fn validation_catches_all_invalid_conditions() {
        let valid = sample_doc();
        assert!(validate_ir(&valid).is_empty(), "{:?}", validate_ir(&valid));

        let mut broken = sample_doc();
        broken.slug = "Bad_Slug!".to_string();
        broken.base_url = "http://insecure.example".to_string();
        broken
            .tokens
            .colors
            .insert("bad".to_string(), "#XYZ".to_string());
        broken.tokens.contrast_pairs.push(ContrastPair {
            foreground: "missing_fg".to_string(),
            background: "missing_bg".to_string(),
            min_ratio: 4.5,
            usage: None,
        });
        broken.pages.push(Page {
            id: "home".to_string(), // duplicate id
            route: RoutePattern::Static {
                path: "/".to_string(), // duplicate route
            },
            title: "Duplicate".to_string(),
            description: None,
            canonical: None,
            noindex: false,
            is_not_found: false,
            json_ld: Vec::new(),
            scripts: Vec::new(),
            body: vec![Node::ComponentInstance {
                uid: None,
                component: "nonexistent-comp".to_string(),
                props: BTreeMap::new(),
                slots: BTreeMap::new(),
            }],
        });
        broken.pages.push(Page {
            id: "dyn-empty".to_string(),
            route: RoutePattern::Dynamic {
                pattern: "/items/{id}".to_string(),
                params: Vec::new(),
            },
            title: "Dyn".to_string(),
            description: None,
            canonical: None,
            noindex: false,
            is_not_found: false,
            json_ld: Vec::new(),
            scripts: Vec::new(),
            body: Vec::new(),
        });

        let errs = validate_ir(&broken);
        let paths: Vec<&str> = errs.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"slug"), "{paths:?}");
        assert!(paths.contains(&"base_url"), "{paths:?}");
        assert!(paths.contains(&"tokens.colors.bad"), "{paths:?}");
        assert!(
            paths.contains(&"tokens.contrast_pairs[1].foreground"),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"tokens.contrast_pairs[1].background"),
            "{paths:?}"
        );
        assert!(paths.contains(&"pages[3].id"), "{paths:?}");
        assert!(paths.contains(&"pages[3].route.path"), "{paths:?}");
        assert!(
            paths.contains(&"pages[3].body[0].component"),
            "{paths:?}"
        );
        assert!(paths.contains(&"pages[4].route.params"), "{paths:?}");

        let mut empty_pages = sample_doc();
        empty_pages.pages.clear();
        let errs_empty = validate_ir(&empty_pages);
        assert!(errs_empty.iter().any(|e| e.path == "pages"));
    }

    #[test]
    fn json_schema_is_draft_2020_12_and_deterministic() {
        let s1 = site_document_json_schema();
        let s2 = site_document_json_schema();
        assert_eq!(s1, s2);
        assert_eq!(
            s1["$schema"],
            json!("https://json-schema.org/draft/2020-12/schema")
        );
        assert_eq!(s1["title"], json!("SiteDocument"));
        assert!(s1["$defs"]["Node"]["oneOf"].is_array());
        assert_eq!(s1["$defs"]["Node"]["oneOf"].as_array().unwrap().len(), 6);
        assert!(s1["$defs"]["RoutePattern"]["oneOf"].is_array());
        assert_eq!(
            s1["$defs"]["RoutePattern"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn url_and_route_security_validation() {
        for ok in [
            "https://example.test",
            "https://sub.example.test:8443/path",
            "https://[::1]:8443/ok",
        ] {
            assert!(is_valid_https_url(ok), "expected valid URL: {ok}");
        }
        for bad in [
            "http://example.test",
            "https://",
            "https://?",
            "https://#frag",
            "https://user:pass@example.test",
            "https://example.test\\evil",
            "https://example.test\nSitemap: https://evil.test",
            "https://example.test:0",
            "https://example.test:99999",
            "https://-bad.test",
            "https://bad..test",
            "https://[...]",
            "https://[:::]",
        ] {
            assert!(!is_valid_https_url(bad), "expected invalid URL: {bad}");
        }

        for ok in ["/", "/about", "/about/", "/menu/dinner-2026", "/404.html"] {
            assert!(is_valid_static_route(ok), "expected valid route: {ok}");
        }
        for bad in [
            "",
            "about",
            "//evil",
            "/../../etc/passwd",
            "/a/../b",
            "/./a",
            "/a\\b",
            "/a?x=1",
            "/a#frag",
            "/a\nb",
            "/a b",
        ] {
            assert!(!is_valid_static_route(bad), "expected invalid route: {bad}");
        }

        assert!(is_valid_dynamic_route("/articles/{slug}"));
        assert!(!is_valid_dynamic_route("/../{slug}"));
        assert!(!is_valid_dynamic_route("/articles/{}"));

        // SetSiteMeta rejects invalid base_url atomically without mutating name/locale
        let mut doc = sample_doc();
        let orig_name = doc.name.clone();
        assert!(matches!(
            apply_patch(
                &mut doc,
                &IrPatch::SetSiteMeta {
                    name: Some("Mutated Name".to_string()),
                    locale: None,
                    base_url: Some("https://?evil".to_string()),
                },
            ),
            Err(IrError::InvalidValue { field, .. }) if field == "base_url"
        ));
        assert_eq!(doc.name, orig_name);

        // Trailing-slash route collision ("/about" vs "/about/") is rejected
        let mut dup_doc = sample_doc();
        let mut about1 = dup_doc.pages[0].clone();
        about1.id = "about-a".to_string();
        about1.route = RoutePattern::Static {
            path: "/about".to_string(),
        };
        let mut about2 = dup_doc.pages[0].clone();
        about2.id = "about-b".to_string();
        about2.route = RoutePattern::Static {
            path: "/about/".to_string(),
        };
        dup_doc.pages.push(about1);
        dup_doc.pages.push(about2);
        let dup_errs = validate_ir(&dup_doc);
        assert!(
            dup_errs
                .iter()
                .any(|e| e.message.contains("duplicate route")),
            "{dup_errs:?}"
        );

        // Deeply nested IR trees hit MAX_IR_NODE_DEPTH cleanly without stack overflow
        let mut deep_doc = sample_doc();
        let mut leaf = Node::Text {
            uid: None,
            value: "leaf".to_string(),
        };
        for _ in 0..140 {
            leaf = Node::Element {
                uid: None,
                id: None,
                tag: "div".to_string(),
                attrs: BTreeMap::new(),
                classes: Vec::new(),
                style_refs: Vec::new(),
                children: vec![leaf],
            };
        }
        deep_doc.pages[0].body = vec![leaf];
        let deep_errs = validate_ir(&deep_doc);
        assert!(
            deep_errs.iter().any(|e| e.message.contains("nesting depth")),
            "{deep_errs:?}"
        );

        // validate_ir catches traversal routes, bad canonical, and robots.txt injection
        let mut bad_doc = sample_doc();
        bad_doc.pages[0].route = RoutePattern::Static {
            path: "/../../pwn".to_string(),
        };
        bad_doc.pages[0].canonical = Some("javascript:alert(1)".to_string());
        bad_doc.seo.robots_allow = vec!["/\nDisallow: /".to_string()];
        bad_doc.seo.robots_disallow = vec!["api/".to_string()];
        bad_doc.seo.sitemap_routes = vec!["/../../secret".to_string()];
        let errs = validate_ir(&bad_doc);
        let paths: Vec<&str> = errs.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"pages[0].route.path"), "{paths:?}");
        assert!(paths.contains(&"pages[0].canonical"), "{paths:?}");
        assert!(paths.contains(&"seo.robots_allow[0]"), "{paths:?}");
        assert!(paths.contains(&"seo.robots_disallow[0]"), "{paths:?}");
        assert!(paths.contains(&"seo.sitemap_routes[0]"), "{paths:?}");
    }
}
