//! Cross-process determinism test (issue #11).
//!
//! The in-process double-compile equality test (`sws-compiler` tests) and the
//! committed-fixture `--check` runs both compare outputs produced *within a
//! single process*. Environment-driven nondeterminism (locale, thread count,
//! or a future `HashMap` leak — `std::collections::HashMap` iteration order is
//! randomly seeded per process) would not be caught by either.
//!
//! This test builds the same multi-page IR fixture via two *fresh* `solo`
//! processes into two separate output directories and asserts identical file
//! sets and byte-identical contents. It is intentionally feature-independent:
//! a plain `solo build` on a generic IR fixture does not need the
//! hospitality pack.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Best-effort cleanup for the shared tempdir.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Ordered list of (relative path, bytes) for every file under `root`.
fn snapshot(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .expect("entry path is under root")
            .to_path_buf();
        if path.is_dir() {
            snapshot(root, &path, out);
        } else if let Ok(bytes) = fs::read(&path) {
            out.push((rel, bytes));
        }
    }
    out.sort();
}

/// Multi-page IR fixture that exercises ordering-sensitive output:
///
/// - 3 pages (home, about, contact)
/// - a component with a `Loop` over a multi-element array prop
/// - a page-level `Loop` over `data.aardvark_items`
/// - a `Conditional` driven by `data.show_banner`
/// - JSON-LD blocks on every page
/// - design-token maps whose JSON insertion order differs from alphabetical
///   (e.g. `colors.paper` before `colors.ink`) so any future `BTreeMap` →
///   `HashMap` regression would surface as cross-process byte drift.
fn fixture_json() -> String {
    let tokens = r##"{
        "colors": {
            "paper": "#F7F5F0",
            "ink": "#241207",
            "accent": "#7A2E0D"
        },
        "fonts": {
            "body": "system-ui, sans-serif",
            "heading": "Georgia, serif"
        },
        "spacing": {
            "lg": "2rem",
            "md": "1rem"
        },
        "radii": {
            "card": "0.5rem"
        },
        "contrast_pairs": [
            {
                "foreground": "ink",
                "background": "paper",
                "min_ratio": 4.5,
                "usage": "body"
            }
        ]
    }"##;

    let styles = r##"[
        {
            "id": "body",
            "selector": "body",
            "declarations": {
                "color": "var(--ink)",
                "background": "var(--paper)"
            }
        },
        {
            "id": "card",
            "selector": ".card",
            "declarations": {
                "padding": "var(--space-md)",
                "border-radius": "var(--radius-card)"
            }
        }
    ]"##;

    let components = r##"{
        "feature-card": {
            "name": "feature-card",
            "props": {
                "heading": { "kind": "string", "required": true },
                "items": { "kind": "array", "required": true }
            },
            "slots": [],
            "template": [
                {
                    "type": "element",
                    "tag": "section",
                    "classes": ["card"],
                    "children": [
                        {
                            "type": "element",
                            "tag": "h2",
                            "children": [
                                { "type": "text", "value": "{{heading}}" }
                            ]
                        },
                        {
                            "type": "element",
                            "tag": "ul",
                            "children": [
                                {
                                    "type": "loop",
                                    "item_var": "entry",
                                    "collection_expr": "items",
                                    "body": [
                                        {
                                            "type": "element",
                                            "tag": "li",
                                            "children": [
                                                { "type": "text", "value": "{{entry.name}} ({{entry.price}})" }
                                            ]
                                        }
                                    ]
                                }
                            ]
                        },
                        {
                            "type": "conditional",
                            "expr": "show_note",
                            "then_nodes": [
                                {
                                    "type": "element",
                                    "tag": "p",
                                    "children": [{ "type": "text", "value": "Note visible" }]
                                }
                            ],
                            "else_nodes": [
                                {
                                    "type": "element",
                                    "tag": "p",
                                    "children": [{ "type": "text", "value": "Note hidden" }]
                                }
                            ]
                        }
                    ]
                }
            ]
        }
    }"##;

    let page_skeleton = |h1: &str, main_children: &str| -> String {
        format!(
            r##"
            {{
                "type": "element",
                "tag": "a",
                "attrs": {{ "href": "#main" }},
                "classes": ["skip-link"],
                "children": [{{ "type": "text", "value": "Skip to main content" }}]
            }},
            {{
                "type": "element",
                "tag": "header",
                "children": [{{ "type": "text", "value": "Header" }}]
            }},
            {{
                "type": "element",
                "tag": "main",
                "attrs": {{ "id": "main", "tabindex": "-1" }},
                "children": [
                    {{
                        "type": "element",
                        "tag": "h1",
                        "children": [{{ "type": "text", "value": "{h1}" }}]
                    }},
                    {main_children}
                ]
            }},
            {{
                "type": "element",
                "tag": "footer",
                "children": [{{ "type": "text", "value": "Footer" }}]
            }}
        "##
        )
    };

    let home_main = r##"
        {
            "type": "element",
            "tag": "ul",
            "children": [
                {
                    "type": "loop",
                    "item_var": "item",
                    "collection_expr": "aardvark_items",
                    "body": [
                        {
                            "type": "element",
                            "tag": "li",
                            "children": [{ "type": "text", "value": "{{item.label}}" }]
                        }
                    ]
                }
            ]
        },
        {
            "type": "component_instance",
            "component": "feature-card",
            "props": {
                "heading": "Featured",
                "items": [
                    { "name": "Alpha", "price": "$1" },
                    { "name": "Beta", "price": "$2" },
                    { "name": "Gamma", "price": "$3" }
                ]
            }
        },
        {
            "type": "conditional",
            "expr": "show_banner",
            "then_nodes": [
                {
                    "type": "element",
                    "tag": "p",
                    "children": [{ "type": "text", "value": "Banner shown" }]
                }
            ],
            "else_nodes": []
        }"##;

    let about_main = r##"
        {
            "type": "element",
            "tag": "p",
            "children": [{ "type": "text", "value": "About page" }]
        }"##;

    let contact_main = r##"
        {
            "type": "element",
            "tag": "p",
            "children": [{ "type": "text", "value": "Contact page" }]
        }"##;

    // `data` keys are deliberately inserted in non-alphabetical order
    // (zulu_note before aardvark_items before show_note before show_banner)
    // to catch any future BTreeMap/sorting regression in expression lookup
    // or serialization.
    let data = r##"{
        "zulu_note": "zulu note",
        "aardvark_items": [
            { "label": "Aardvark One" },
            { "label": "Aardvark Two" },
            { "label": "Aardvark Three" }
        ],
        "show_note": true,
        "show_banner": true
    }"##;

    let metadata = r##"{
        "zulu_meta": "zulu",
        "aardvark_meta": "aardvark"
    }"##;

    format!(
        r##"{{
        "schema_version": 1,
        "slug": "determinism-probe",
        "name": "Determinism Probe",
        "locale": "en-US",
        "base_url": "https://probe.example",
        "tokens": {tokens},
        "breakpoints": [
            {{ "id": "tablet", "media_query": "(min-width: 48em)", "min_width": "48em" }}
        ],
        "styles": {styles},
        "components": {components},
        "forms": {{}},
        "pages": [
            {{
                "id": "home",
                "route": {{ "kind": "static", "path": "/" }},
                "title": "Determinism Probe",
                "description": "Cross-process determinism probe",
                "canonical": "https://probe.example/",
                "noindex": false,
                "is_not_found": false,
                "json_ld": [
                    {{
                        "id": "probe",
                        "data": {{
                            "zulu": 1,
                            "alpha": [2, 1],
                            "@context": "https://schema.org"
                        }}
                    }}
                ],
                "scripts": [],
                "body": [{skeleton_home}]
            }},
            {{
                "id": "about",
                "route": {{ "kind": "static", "path": "/about" }},
                "title": "About",
                "description": "About the probe",
                "canonical": "https://probe.example/about",
                "noindex": false,
                "is_not_found": false,
                "json_ld": [
                    {{
                        "data": {{
                            "zulu": 1,
                            "alpha": 2,
                            "@type": "AboutPage"
                        }}
                    }}
                ],
                "scripts": [],
                "body": [{skeleton_about}]
            }},
            {{
                "id": "contact",
                "route": {{ "kind": "static", "path": "/contact" }},
                "title": "Contact",
                "description": "Contact the probe",
                "canonical": "https://probe.example/contact",
                "noindex": false,
                "is_not_found": false,
                "json_ld": [],
                "scripts": [],
                "body": [{skeleton_contact}]
            }}
        ],
        "seo": {{
            "title_template": "%s — Determinism Probe",
            "default_description": "Determinism probe site",
            "robots_allow": ["/"],
            "robots_disallow": ["/api/"],
            "sitemap_routes": ["/", "/about", "/contact"]
        }},
        "scripts": [
            {{ "src": "/js/forms.js", "async_load": false, "defer": true }}
        ],
        "data": {data},
        "metadata": {metadata}
    }}"##,
        tokens = tokens,
        styles = styles,
        components = components,
        skeleton_home = page_skeleton("Determinism Probe", home_main),
        skeleton_about = page_skeleton("About", about_main),
        skeleton_contact = page_skeleton("Contact", contact_main),
        data = data,
        metadata = metadata,
    )
}

#[test]
fn solo_build_is_byte_identical_across_two_fresh_processes() {
    let json = fixture_json();

    // Sanity-check the fixture is valid JSON before invoking the binary.
    let _value: serde_json::Value =
        serde_json::from_str(&json).expect("determinism fixture must be valid JSON");

    // Unique subdir per test run (pid + nanos) so parallel runs never collide.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!(
        "sws-determinism-{}-{nanos}",
        std::process::id()
    ));
    let _guard = TempDirGuard(base.clone());
    fs::create_dir_all(&base).expect("failed to create unique tempdir");

    let fixture_path = base.join("site.ir.json");
    fs::write(&fixture_path, &json).expect("failed to write fixture");

    let out_a = base.join("out-a");
    let out_b = base.join("out-b");

    // Two fresh `solo` processes, identical flags.
    let bin = env!("CARGO_BIN_EXE_solo");
    for out in [&out_a, &out_b] {
        let status = Command::new(bin)
            .arg("build")
            .arg(&fixture_path)
            .arg(out)
            .status()
            .expect("failed to spawn solo");
        assert!(
            status.success(),
            "solo build failed for {out:?}: {status:?}"
        );
    }

    let mut files_a = Vec::new();
    let mut files_b = Vec::new();
    snapshot(&out_a, &out_a, &mut files_a);
    snapshot(&out_b, &out_b, &mut files_b);

    // Guard against a degenerate "both runs failed silently" pass: the
    // fixture must produce the expected deterministic bundle files.
    let names: Vec<String> = files_a
        .iter()
        .map(|(rel, _)| rel.to_string_lossy().replace('\\', "/"))
        .collect();
    for expected in [
        "index.html",
        "about/index.html",
        "contact/index.html",
        "styles.css",
        "robots.txt",
        "sitemap.xml",
        "js/forms.js",
        "_headers",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "fixture output is missing {expected}; found {names:?}"
        );
    }

    // 1. Identical file sets.
    let rel_paths: std::collections::BTreeSet<PathBuf> =
        files_a.iter().map(|(rel, _)| rel.clone()).collect();
    let rel_paths_b: std::collections::BTreeSet<PathBuf> =
        files_b.iter().map(|(rel, _)| rel.clone()).collect();
    assert_eq!(
        rel_paths, rel_paths_b,
        "file sets differ between the two fresh-process runs"
    );

    // 2. Byte-identical contents for every file.
    let mut mismatches = Vec::new();
    for ((rel_a, bytes_a), (rel_b, bytes_b)) in files_a.iter().zip(files_b.iter()) {
        if rel_a != rel_b || bytes_a != bytes_b {
            mismatches.push(format!(
                "{rel_a:?} ({} B) != {rel_b:?} ({} B)",
                bytes_a.len(),
                bytes_b.len()
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "cross-process output is not byte-identical:\n{}",
        mismatches.join("\n")
    );

    // Ordering-sensitive content checks: tokens and data keys were written
    // in non-alphabetical insertion order in the fixture, so they must have
    // been sorted deterministically on every run.
    let index = files_a
        .iter()
        .find(|(rel, _)| rel == Path::new("index.html"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        .expect("index.html must exist");
    assert!(
        index.contains("<li>Aardvark One</li>")
            && index.contains("<li>Aardvark Two</li>")
            && index.contains("<li>Aardvark Three</li>"),
        "page-level loop did not render all items in order"
    );
    assert!(
        index.contains("Alpha ($1)") && index.contains("Beta ($2") && index.contains("Gamma ($3"),
        "component loop over multi-element array prop did not render"
    );
    assert!(
        index.contains("Banner shown"),
        "conditional node did not render the taken branch"
    );
}
