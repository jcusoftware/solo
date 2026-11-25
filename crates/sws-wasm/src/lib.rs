//! Browser Wasm bridge (`wasm-bindgen`) for Solo Website System (SWS).
//!
//! Pure logic lives in the host-testable `*_inner` functions so `cargo test`
//! exercises compilation, IR/diagnostic validation, and WCAG contrast checks
//! natively while `#[wasm_bindgen]` exports delegate cleanly for browser use.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use sws_compiler::{CompileOptions, CompilerDiagnostic, DiagnosticLevel};
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
    let raw_value = serde_json::from_str::<Value>(ir_json).ok();
    let ir_errors: Vec<SerializableIrError> = validate_ir(&doc)
        .iter()
        .map(SerializableIrError::from)
        .collect();
    let options = CompileOptions::default();
    let diagnostics =
        sws_compiler::check_site_diagnostics(&doc, raw_value.as_ref(), None, &options);
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

/// Wasm export for [`check_contrast_inner`].
#[wasm_bindgen]
pub fn check_contrast(fg: &str, bg: &str) -> Result<f64, JsValue> {
    check_contrast_inner(fg, bg).map_err(|msg| JsValue::from_str(&msg))
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
                id: None,
                tag: "h3".to_string(),
                attrs: BTreeMap::new(),
                classes: Vec::new(),
                style_refs: Vec::new(),
                children: vec![sws_ir::Node::Text {
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
}
