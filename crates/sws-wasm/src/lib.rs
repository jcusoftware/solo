//! Browser Wasm bridge (`wasm-bindgen`) for Solo Website System (SWS).
//!
//! Pure logic lives in the host-testable `*_inner` functions so `cargo test`
//! exercises compilation, IR/diagnostic validation, CSS emission/validation,
//! and WCAG contrast checks natively while `#[wasm_bindgen]` exports delegate
//! cleanly for browser use.
//!
//! Export surface (all exercised by `crates/sws-wasm/tests/smoke.mjs`):
//!
//! - `compile_site(ir_json) -> Result<String, JsValue>`: compile a
//!   [`SiteDocument`] with default options (files + diagnostics report).
//! - `validate_site(ir_json) -> Result<String, JsValue>`: IR validation and
//!   compiler diagnostics (`{ valid, ir_errors, diagnostics }`).
//! - `compile_css(css_json) -> Result<String, JsValue>`: deserialize a
//!   serialized [`sws_css::Stylesheet`] and emit sanitized, deterministic CSS.
//! - `validate_css(css_text) -> Result<String, JsValue>`: parse CSS *text*
//!   with [`sws_css::parse_stylesheet`] (`{ valid, diagnostics }`; parse
//!   failures become error-level `css_parse_error` diagnostics).
//! - `check_contrast(fg, bg) -> Result<f64, JsValue>`: WCAG 2.x contrast
//!   ratio for a `#RRGGBB` pair.
//!
//! - `upgrade_stable_ids(ir_json) -> Result<String, JsValue>`: the explicit,
//!   deterministic stable-ID upgrade (`sws_ir::edit::upgrade_json`); returns
//!   `{ document, report, dropped_paths }` where `document` is the canonical
//!   JSON text of the upgraded IR.
//! - `apply_command_batch(ir_json, batch_json) -> Result<String, JsValue>`: the
//!   canonical editor reducer (`sws_ir::edit::apply_batch_json`); returns
//!   `{ document, transaction, dropped_paths }`. Atomic: on error nothing is
//!   returned and the caller keeps its document.
//! - `apply_undo(ir_json, transaction_json) -> Result<String, JsValue>`: undo a
//!   recorded transaction (the `transaction` returned by `apply_command_batch`)
//!   in restore mode (`sws_ir::edit::apply_undo_json`); same output shape. Plain
//!   `apply_command_batch` of `transaction.inverse` cannot restore values the
//!   forward grammar rejects (for example an empty font stack); use this.
//!
//! Every export maps its host-side `Err(String)` to a `JsValue` string so
//! callers get a plain message (`"<code>: <message>"` for editor errors).

use std::collections::BTreeMap;

use serde::Serialize;
use sws_compiler::{CompileOptions, CompilerDiagnostic, DiagnosticLevel};
use sws_css::{Stylesheet, parse_stylesheet};
use sws_ir::{validate_ir, IrValidationError, SiteDocument};
use wasm_bindgen::prelude::*;

#[derive(Debug, Serialize)]
struct CompileOutput {
    files: BTreeMap<String, String>,
    diagnostics: Vec<CompilerDiagnostic>,
    index_html_bytes: usize,
    styles_css_bytes: usize,
}

#[derive(Debug, Serialize)]
struct SerializableIrError {
    path: String,
    message: String,
}

impl From<&IrValidationError> for SerializableIrError {
    fn from(err: &IrValidationError) -> Self {
        Self {
            path: err.path.clone(),
            message: err.message.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ValidateOutput {
    valid: bool,
    ir_errors: Vec<SerializableIrError>,
    diagnostics: Vec<CompilerDiagnostic>,
}

#[derive(Debug, Serialize)]
struct ValidateCssOutput {
    valid: bool,
    diagnostics: Vec<CompilerDiagnostic>,
}

fn parse_site_document(ir_json: &str) -> Result<SiteDocument, String> {
    serde_json::from_str::<SiteDocument>(ir_json)
        .map_err(|err| format!("invalid SiteDocument JSON: {err}"))
}

fn diagnostic_is_error(diag: &CompilerDiagnostic) -> bool {
    matches!(diag.level, DiagnosticLevel::Error)
}

/// Parse a [`SiteDocument`] JSON string and compile it with default options.
///
/// Returns a serialized JSON object containing `files`, `diagnostics`,
/// `index_html_bytes`, and `styles_css_bytes`.
pub fn compile_site_inner(ir_json: &str) -> Result<String, String> {
    let doc = parse_site_document(ir_json)?;
    let options = CompileOptions::default();
    let compiled = sws_compiler::compile_site(&doc, None, &options)
        .map_err(|err| format!("compilation failed: {err}"))?;

    let output = CompileOutput {
        files: compiled.files,
        diagnostics: compiled.diagnostics,
        index_html_bytes: compiled.index_html_bytes,
        styles_css_bytes: compiled.styles_css_bytes,
    };

    serde_json::to_string(&output)
        .map_err(|err| format!("failed to serialize compile output: {err}"))
}

/// Parse a [`SiteDocument`] JSON string and run structural IR validation and
/// compiler diagnostics.
///
/// Returns a serialized JSON object `{ valid, ir_errors, diagnostics }`.
pub fn validate_site_inner(ir_json: &str) -> Result<String, String> {
    let doc = parse_site_document(ir_json)?;
    let ir_errors: Vec<SerializableIrError> = validate_ir(&doc)
        .iter()
        .map(SerializableIrError::from)
        .collect();
    let options = CompileOptions::default();
    // The plugin registry is `None` on this path, so `raw_input` is never
    // consumed; skip the second parse of `ir_json` (issue #13).
    let diagnostics = sws_compiler::check_site_diagnostics(&doc, None, None, &options);
    let has_error_diagnostic = diagnostics.iter().any(diagnostic_is_error);

    let output = ValidateOutput {
        valid: ir_errors.is_empty() && !has_error_diagnostic,
        ir_errors,
        diagnostics,
    };

    serde_json::to_string(&output)
        .map_err(|err| format!("failed to serialize validation output: {err}"))
}

/// Compute the WCAG 2.x relative-luminance contrast ratio for a `#RRGGBB` pair.
pub fn check_contrast_inner(fg: &str, bg: &str) -> Result<f64, String> {
    sws_css::contrast_ratio(fg, bg)
        .ok_or_else(|| format!("invalid #RRGGBB color pair: fg={fg:?}, bg={bg:?}"))
}

/// Deserialize a serialized [`Stylesheet`] JSON string and emit deterministic,
/// sanitizer-gated CSS.
///
/// This is the same emission path `sws-compiler` uses for `styles.css`
/// (see `sws_compiler::emit_stylesheet`), so the sanitizer gates in
/// [`Stylesheet::emit`] apply unchanged.
pub fn compile_css_inner(css_json: &str) -> Result<String, String> {
    let sheet: Stylesheet = serde_json::from_str(css_json)
        .map_err(|err| format!("invalid Stylesheet JSON: {err}"))?;
    sheet.emit().map_err(|err| format!("CSS emit error: {err}"))
}

/// Parse CSS text with [`parse_stylesheet`] and report the result.
///
/// Returns a serialized JSON object `{ valid, diagnostics }` where
/// `diagnostics` mirrors the [`CompilerDiagnostic`] shape (a parse failure
/// yields a single error-level `css_parse_error` diagnostic carrying the
/// parser's line/column message) rather than rejecting the whole call, so
/// editors and browser tooling can surface diagnostics without round-trips.
pub fn validate_css_inner(css_text: &str) -> Result<String, String> {
    let diagnostics: Vec<CompilerDiagnostic> = match parse_stylesheet(css_text) {
        Ok(_) => Vec::new(),
        Err(err) => vec![CompilerDiagnostic {
            level: DiagnosticLevel::Error,
            code: "css_parse_error".to_string(),
            path: "css".to_string(),
            message: err.to_string(),
        }],
    };

    let output = ValidateCssOutput {
        valid: diagnostics.is_empty(),
        diagnostics,
    };

    serde_json::to_string(&output)
        .map_err(|err| format!("failed to serialize CSS validation output: {err}"))
}

/// Run the explicit stable-ID upgrade (same reducer code the CLI uses).
///
/// Returns `{ document, report, dropped_paths }`; `document` is the canonical
/// pretty JSON text of the upgraded IR. Sources whose unsupported fields would
/// be dropped are refused (`lossy_document`).
pub fn upgrade_stable_ids_inner(ir_json: &str) -> Result<String, String> {
    let out = sws_ir::edit::upgrade_json(ir_json, sws_ir::edit::EditOptions::default())
        .map_err(|err| err.to_string())?;
    serde_json::to_string(&out).map_err(|err| format!("failed to serialize upgrade output: {err}"))
}

/// Apply one serialized command batch atomically through the canonical reducer.
///
/// Returns `{ document, transaction, dropped_paths }`; `transaction.inverse`
/// is the exact undo batch body and `transaction.touched` the conflict fields.
pub fn apply_command_batch_inner(ir_json: &str, batch_json: &str) -> Result<String, String> {
    let out = sws_ir::edit::apply_batch_json(
        ir_json,
        batch_json,
        sws_ir::edit::EditOptions::default(),
    )
    .map_err(|err| err.to_string())?;
    serde_json::to_string(&out).map_err(|err| format!("failed to serialize edit output: {err}"))
}

/// Undo one recorded transaction through the restore-mode reducer.
///
/// `transaction_json` is the `transaction` object returned by
/// [`apply_command_batch_inner`]; it is cross-checked against `ir_json` (the
/// document that edit produced). Returns `{ document, transaction,
/// dropped_paths }` like [`apply_command_batch_inner`].
pub fn apply_undo_inner(ir_json: &str, transaction_json: &str) -> Result<String, String> {
    let out = sws_ir::edit::apply_undo_json(
        ir_json,
        transaction_json,
        sws_ir::edit::EditOptions::default(),
    )
    .map_err(|err| err.to_string())?;
    serde_json::to_string(&out).map_err(|err| format!("failed to serialize edit output: {err}"))
}

/// Wasm export for [`compile_site_inner`].
#[wasm_bindgen]
pub fn compile_site(ir_json: &str) -> Result<String, JsValue> {
    compile_site_inner(ir_json).map_err(|msg| JsValue::from_str(&msg))
}

/// Wasm export for [`validate_site_inner`].
#[wasm_bindgen]
pub fn validate_site(ir_json: &str) -> Result<String, JsValue> {
    validate_site_inner(ir_json).map_err(|msg| JsValue::from_str(&msg))
}

/// Wasm export for [`compile_css_inner`].
#[wasm_bindgen]
pub fn compile_css(css_json: &str) -> Result<String, JsValue> {
    compile_css_inner(css_json).map_err(|msg| JsValue::from_str(&msg))
}

/// Wasm export for [`validate_css_inner`].
#[wasm_bindgen]
pub fn validate_css(css_text: &str) -> Result<String, JsValue> {
    validate_css_inner(css_text).map_err(|msg| JsValue::from_str(&msg))
}

/// Wasm export for [`check_contrast_inner`].
#[wasm_bindgen]
pub fn check_contrast(fg: &str, bg: &str) -> Result<f64, JsValue> {
    check_contrast_inner(fg, bg).map_err(|msg| JsValue::from_str(&msg))
}

/// Wasm export for [`upgrade_stable_ids_inner`].
#[wasm_bindgen]
pub fn upgrade_stable_ids(ir_json: &str) -> Result<String, JsValue> {
    upgrade_stable_ids_inner(ir_json).map_err(|msg| JsValue::from_str(&msg))
}

/// Wasm export for [`apply_command_batch_inner`].
#[wasm_bindgen]
pub fn apply_command_batch(ir_json: &str, batch_json: &str) -> Result<String, JsValue> {
    apply_command_batch_inner(ir_json, batch_json).map_err(|msg| JsValue::from_str(&msg))
}

/// Wasm export for [`apply_undo_inner`].
#[wasm_bindgen]
pub fn apply_undo(ir_json: &str, transaction_json: &str) -> Result<String, JsValue> {
    apply_undo_inner(ir_json, transaction_json).map_err(|msg| JsValue::from_str(&msg))
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn valid_minimal_ir_json() -> String {
        json!({
            "schema_version": 1,
            "slug": "acme-studio-demo",
            "name": "Acme Studio",
            "locale": "en-US",
            "base_url": "https://acme.example",
            "tokens": {
                "colors": {
                    "ink": "#1A1A1A",
                    "paper": "#FFFFFF"
                },
                "fonts": {
                    "body": "system-ui, sans-serif"
                },
                "contrast_pairs": [
                    {
                        "foreground": "ink",
                        "background": "paper",
                        "min_ratio": 4.5,
                        "usage": "body"
                    }
                ]
            },
            "pages": [
                {
                    "id": "home",
                    "route": { "kind": "static", "path": "/" },
                    "title": "Home",
                    "body": [
                        {
                            "type": "element",
                            "tag": "a",
                            "classes": ["skip-link"],
                            "attrs": { "href": "#main" },
                            "children": [
                                { "type": "text", "value": "Skip to main content" }
                            ]
                        },
                        {
                            "type": "element",
                            "tag": "header",
                            "children": [
                                { "type": "text", "value": "Acme Studio" }
                            ]
                        },
                        {
                            "type": "element",
                            "tag": "main",
                            "attrs": { "id": "main", "tabindex": "-1" },
                            "children": [
                                {
                                    "type": "element",
                                    "tag": "h1",
                                    "children": [
                                        { "type": "text", "value": "Acme Studio" }
                                    ]
                                }
                            ]
                        },
                        {
                            "type": "element",
                            "tag": "footer",
                            "children": [
                                { "type": "text", "value": "Acme Studio" }
                            ]
                        }
                    ]
                }
            ]
        })
        .to_string()
    }

    #[test]
    fn compile_site_inner_succeeds_for_valid_minimal_doc() {
        let result = compile_site_inner(&valid_minimal_ir_json()).expect("valid IR compiles");
        let parsed: Value = serde_json::from_str(&result).expect("valid JSON output");

        assert!(parsed["files"].is_object());
        assert!(parsed["files"]["index.html"].as_str().is_some());
        assert!(parsed["diagnostics"].is_array());
        assert!(parsed["index_html_bytes"].as_u64().unwrap_or(0) > 0);
        assert!(parsed["styles_css_bytes"].is_u64());
    }

    #[test]
    fn compile_site_inner_rejects_invalid_json() {
        let err = compile_site_inner("{not valid json").unwrap_err();
        assert!(err.contains("invalid SiteDocument JSON"), "{err}");
    }

    #[test]
    fn validate_site_inner_passes_for_valid_minimal_doc() {
        let result = validate_site_inner(&valid_minimal_ir_json()).expect("valid JSON validates");
        let parsed: Value = serde_json::from_str(&result).expect("valid JSON output");

        assert_eq!(parsed["valid"], json!(true));
        assert_eq!(parsed["ir_errors"], json!([]));
        assert!(parsed["diagnostics"].is_array());
    }

    #[test]
    fn validate_site_inner_reports_ir_errors_and_invalid_json() {
        let json_err = validate_site_inner("[").unwrap_err();
        assert!(json_err.contains("invalid SiteDocument JSON"), "{json_err}");

        let invalid_doc = json!({
            "schema_version": 1,
            "slug": "Bad_Slug",
            "name": "",
            "base_url": "http://insecure.example",
            "pages": []
        })
        .to_string();
        let result =
            validate_site_inner(&invalid_doc).expect("parseable invalid doc returns report");
        let parsed: Value = serde_json::from_str(&result).expect("valid JSON output");

        assert_eq!(parsed["valid"], json!(false));
        let ir_errors = parsed["ir_errors"].as_array().expect("ir_errors array");
        assert!(!ir_errors.is_empty());
    }

    #[test]
    fn validate_site_inner_marks_compiler_errors_invalid() {
        let mut doc: SiteDocument =
            serde_json::from_str(&valid_minimal_ir_json()).expect("parse minimal valid IR");
        // Keep IR structurally valid while introducing a WCAG skipped heading error (h1 -> h3).
        if let Some(sws_ir::Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, sws_ir::Node::Element { tag, .. } if tag == "main"))
        {
            children.push(sws_ir::Node::Element {
                uid: None,
                id: None,
                tag: "h3".to_string(),
                attrs: BTreeMap::new(),
                classes: Vec::new(),
                style_refs: Vec::new(),
                children: vec![sws_ir::Node::Text {
                    uid: None,
                    value: "Skipped Heading".to_string(),
                }],
            });
        }

        let result = validate_site_inner(
            &serde_json::to_string(&doc).expect("serialize modified doc"),
        )
        .expect("validation returns JSON report");
        let parsed: Value = serde_json::from_str(&result).expect("parse validation report");

        assert_eq!(parsed["ir_errors"], json!([]));
        assert_eq!(parsed["valid"], json!(false));
        let diags = parsed["diagnostics"].as_array().expect("diagnostics array");
        assert!(
            diags.iter().any(|d| {
                d["level"] == "error" && d["code"] == "skipped_heading_level"
            }),
            "expected error-level skipped_heading_level diagnostic, got: {diags:?}"
        );
    }

    #[test]
    fn check_contrast_inner_evaluates_aa_and_rejects_invalid_hex() {
        let high = check_contrast_inner("#1A1A1A", "#FFFFFF").expect("valid hex");
        assert!(high >= 4.5, "expected >= 4.5, got {high}");

        let max = check_contrast_inner("#000000", "#FFFFFF").expect("black on white");
        assert!((max - 21.0).abs() < 0.05, "expected ~21.0, got {max}");

        let low = check_contrast_inner("#777777", "#888888").expect("valid low contrast hex");
        assert!(low < 4.5, "expected < 4.5, got {low}");

        for bad in ["#XYZ", "1A1A1A", "#12345", "#1234567", "red", ""] {
            let err = check_contrast_inner(bad, "#FFFFFF").unwrap_err();
            assert!(err.contains("invalid #RRGGBB"), "{bad}: {err}");
        }
    }

    fn sample_stylesheet() -> Stylesheet {
        let mut sheet = Stylesheet::new();
        sheet.comment_header = Some("Generated by SWS. Do not edit.".to_string());
        sheet.set_var("bg", "#E9E2D2");
        sheet.set_var("--text", "#241207");
        sheet.define_breakpoint("sm", "(min-width: 40rem)");
        sheet.add_simple_rule(
            "body",
            BTreeMap::from([
                ("background".to_string(), "var(--bg)".to_string()),
                ("color".to_string(), "var(--text)".to_string()),
            ]),
        );
        sheet.add_rule(sws_css::CssRule {
            selector: ".grid".to_string(),
            breakpoint: Some("sm".to_string()),
            pseudo: Some(":hover".to_string()),
            declarations: BTreeMap::from([(
                "grid-template-columns".to_string(),
                "1fr 1fr".to_string(),
            )]),
        });
        sheet
    }

    #[test]
    fn compile_css_inner_emits_sanitized_deterministic_css() {
        let sheet = sample_stylesheet();
        let css_json =
            serde_json::to_string(&sheet).expect("serialize Stylesheet fixture");

        let first = compile_css_inner(&css_json).expect("valid stylesheet compiles");
        let second = compile_css_inner(&css_json).expect("compilation is deterministic");
        assert_eq!(first, second);
        assert_eq!(first, sheet.emit().expect("direct emission"));
        assert!(first.contains(":root {\n  --bg: #E9E2D2;\n  --text: #241207;\n}"));
        assert!(first.contains("body { background: var(--bg); color: var(--text); }"));
        assert!(first.contains(
            "@media (min-width: 40rem) {\n  .grid:hover { grid-template-columns: 1fr 1fr; }\n}"
        ));
    }

    #[test]
    fn compile_css_inner_rejects_invalid_json_and_unsafe_declarations() {
        let json_err = compile_css_inner("{not valid json").unwrap_err();
        assert!(json_err.contains("invalid Stylesheet JSON"), "{json_err}");

        let unsafe_sheet = json!({
            "comment_header": null,
            "root_vars": {},
            "media_queries": {},
            "rules": [{
                "selector": "body",
                "breakpoint": null,
                "pseudo": null,
                "declarations": { "color": "red; background: blue" }
            }]
        })
        .to_string();
        let emit_err = compile_css_inner(&unsafe_sheet).unwrap_err();
        assert!(emit_err.contains("CSS emit error"), "{emit_err}");
    }

    #[test]
    fn validate_css_inner_accepts_safe_css_and_reports_parse_errors() {
        let css = sample_stylesheet().emit().expect("fixture emits");
        let result = validate_css_inner(&css).expect("valid CSS validates");
        let parsed: Value = serde_json::from_str(&result).expect("valid JSON output");

        assert_eq!(parsed["valid"], json!(true));
        assert_eq!(parsed["diagnostics"], json!([]));

        let invalid = validate_css_inner("body { color: ").expect("report JSON on parse error");
        let parsed: Value = serde_json::from_str(&invalid).expect("valid JSON output");
        assert_eq!(parsed["valid"], json!(false));
        let diagnostics = parsed["diagnostics"].as_array().expect("diagnostics array");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["level"], json!("error"));
        assert_eq!(diagnostics[0]["code"], json!("css_parse_error"));
        assert!(diagnostics[0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("CSS parse error")));
    }

    #[test]
    fn upgrade_and_apply_batch_share_the_native_reducer() {
        let ir = valid_minimal_ir_json();
        let upgraded: Value =
            serde_json::from_str(&upgrade_stable_ids_inner(&ir).expect("upgrade")).unwrap();
        let document = upgraded["document"].as_str().expect("document text");
        assert!(upgraded["report"]["assigned"].as_array().unwrap().len() > 3);
        let again: Value =
            serde_json::from_str(&upgrade_stable_ids_inner(document).unwrap()).unwrap();
        assert_eq!(again["document"], upgraded["document"], "idempotent");

        let batch = json!({
            "version": 1,
            "commands": [{ "op": "set_color_token", "key": "ink", "hex": "#000000" }]
        })
        .to_string();
        let edited: Value =
            serde_json::from_str(&apply_command_batch_inner(document, &batch).expect("apply"))
                .unwrap();
        let edited_doc = edited["document"].as_str().unwrap();
        assert!(edited_doc.contains("#000000"));
        let transaction = edited["transaction"].to_string();
        let restored: Value =
            serde_json::from_str(&apply_undo_inner(edited_doc, &transaction).unwrap()).unwrap();
        assert_eq!(restored["document"].as_str().unwrap(), document);

        let err = apply_command_batch_inner(&ir, &batch).unwrap_err();
        assert!(err.starts_with("not_upgraded:"), "{err}");
    }
}
