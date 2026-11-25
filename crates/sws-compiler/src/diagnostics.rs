//! Compile-time WCAG 2.2 AA, security, and IR diagnostics.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sws_ir::{RoutePattern, SiteDocument, validate_ir};
use sws_plugin::{DiagnosticSeverity, PluginRegistry};

use crate::expand::{ExpandedNode, expand_page_body};

const BANNED_OVERLAY_PATTERNS: &[&str] = &[
    "accessibe",
    "acsbapp",
    "userway",
    "audioeye",
    "equalweb",
    "maxaccess",
    "purpose-ui",
    "purposeui",
    "accessibilityassistant",
];

/// Severity of a compile-time diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Error,
    Warning,
}

/// Structured compile-time diagnostic emitted by `sws-compiler`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerDiagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub path: String,
    pub message: String,
}

impl CompilerDiagnostic {
    fn error(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level: DiagnosticLevel::Error,
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }

    fn warning(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level: DiagnosticLevel::Warning,
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

pub(crate) fn contains_banned_overlay(value: &str) -> Option<&'static str> {
    let lower = value.to_ascii_lowercase();
    BANNED_OVERLAY_PATTERNS
        .iter()
        .copied()
        .find(|pattern| lower.contains(pattern))
}

struct ControlRecord {
    tag: String,
    id: Option<String>,
    in_label: bool,
    has_aria_label: bool,
    has_aria_labelledby: bool,
    path: String,
}

#[derive(Default)]
struct PageDomStats {
    headings: Vec<(u8, String)>,
    header_count: usize,
    footer_count: usize,
    main_count: usize,
    valid_main_count: usize,
    has_skip_link: bool,
    label_for_ids: BTreeSet<String>,
    controls: Vec<ControlRecord>,
    overlay_hits: Vec<(String, &'static str)>,
}

fn heading_level(tag: &str) -> Option<u8> {
    match tag.to_ascii_lowercase().as_str() {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn collect_dom_stats(
    nodes: &[ExpandedNode],
    base_path: &str,
    in_aria_hidden: bool,
    in_label: bool,
    stats: &mut PageDomStats,
) {
    for (idx, node) in nodes.iter().enumerate() {
        let path = format!("{base_path}[{idx}]");
        let ExpandedNode::Element {
            tag,
            attrs,
            classes,
            children,
        } = node
        else {
            continue;
        };

        let tag_lower = tag.to_ascii_lowercase();
        let self_aria_hidden = attrs
            .get("aria-hidden")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let hidden = in_aria_hidden || self_aria_hidden;
        let next_in_label = in_label || tag_lower == "label";

        for (attr_k, attr_v) in attrs {
            if let Some(pat) = contains_banned_overlay(attr_v) {
                stats.overlay_hits.push((format!("{path}@{attr_k}"), pat));
            }
        }
        for cls in classes {
            if let Some(pat) = contains_banned_overlay(cls) {
                stats.overlay_hits.push((format!("{path}.class"), pat));
            }
        }
        if tag_lower == "script" {
            for child in children {
                if let ExpandedNode::Text(script_text) = child
                    && let Some(pat) = contains_banned_overlay(script_text)
                {
                    stats.overlay_hits.push((path.clone(), pat));
                }
            }
        }

        if let Some(level) = heading_level(&tag_lower) {
            stats.headings.push((level, path.clone()));
        }

        match tag_lower.as_str() {
            "header" => stats.header_count += 1,
            "footer" => stats.footer_count += 1,
            "main" => {
                stats.main_count += 1;
                let has_id = attrs.get("id").is_some_and(|v| v == "main");
                let has_tabindex = attrs.get("tabindex").is_some_and(|v| v == "-1");
                if has_id && has_tabindex {
                    stats.valid_main_count += 1;
                }
            }
            "a" => {
                let is_skip_class = classes.iter().any(|c| c == "skip-link");
                let points_to_main = attrs.get("href").is_some_and(|h| h == "#main");
                if is_skip_class && points_to_main {
                    stats.has_skip_link = true;
                }
            }
            "label" => {
                if let Some(for_id) = attrs.get("for")
                    && !for_id.trim().is_empty()
                {
                    stats.label_for_ids.insert(for_id.clone());
                }
            }
            "input" | "select" | "textarea" if !hidden => {
                let skip_input = tag_lower == "input"
                    && attrs.get("type").is_some_and(|t| {
                        let tl = t.to_ascii_lowercase();
                        tl == "hidden" || tl == "submit" || tl == "button"
                    });
                if !skip_input {
                    let has_aria_label = attrs
                        .get("aria-label")
                        .is_some_and(|v| !v.trim().is_empty());
                    let has_aria_labelledby = attrs
                        .get("aria-labelledby")
                        .is_some_and(|v| !v.trim().is_empty());
                    let id = attrs
                        .get("id")
                        .filter(|v| !v.trim().is_empty())
                        .cloned();
                    stats.controls.push(ControlRecord {
                        tag: tag_lower.clone(),
                        id,
                        in_label: next_in_label,
                        has_aria_label,
                        has_aria_labelledby,
                        path: path.clone(),
                    });
                }
            }
            _ => {}
        }

        collect_dom_stats(children, &path, hidden, next_in_label, stats);
    }
}

/// Run IR, contrast, font-safety, WCAG 2.2 AA, security, and plugin lint checks
/// on a [`SiteDocument`].
#[must_use]
pub fn run_diagnostics(
    doc: &SiteDocument,
    raw_input: Option<&Value>,
    registry: Option<&PluginRegistry>,
) -> Vec<CompilerDiagnostic> {
    let mut diags = Vec::new();

    // 1. Structural IR validation
    for err in validate_ir(doc) {
        diags.push(CompilerDiagnostic::error(
            "ir_invalid",
            err.path,
            err.message,
        ));
    }

    // 2. Font stack safety
    if !sws_css::is_safe_font_stack(&doc.tokens.fonts.body) {
        diags.push(CompilerDiagnostic::error(
            "unsafe_font_stack",
            "tokens.fonts.body",
            "body font stack is empty or contains unsafe CSS tokens",
        ));
    }
    if let Some(heading) = &doc.tokens.fonts.heading
        && !sws_css::is_safe_font_stack(heading)
    {
        diags.push(CompilerDiagnostic::error(
            "unsafe_font_stack",
            "tokens.fonts.heading",
            "heading font stack is empty or contains unsafe CSS tokens",
        ));
    }
    if let Some(mono) = &doc.tokens.fonts.mono
        && !sws_css::is_safe_font_stack(mono)
    {
        diags.push(CompilerDiagnostic::error(
            "unsafe_font_stack",
            "tokens.fonts.mono",
            "mono font stack is empty or contains unsafe CSS tokens",
        ));
    }

    // 3. Color hex + WCAG AA contrast ratio (>= 4.5:1)
    for (key, hex) in &doc.tokens.colors {
        if !sws_css::is_valid_hex_color(hex) {
            diags.push(CompilerDiagnostic::error(
                "invalid_hex_color",
                format!("tokens.colors.{key}"),
                format!("color '{hex}' must be a #RRGGBB hex literal"),
            ));
        }
    }

    for (idx, pair) in doc.tokens.contrast_pairs.iter().enumerate() {
        let Some(fg_hex) = doc.tokens.colors.get(&pair.foreground) else {
            continue;
        };
        let Some(bg_hex) = doc.tokens.colors.get(&pair.background) else {
            continue;
        };
        if let Some(ratio) = sws_css::contrast_ratio(fg_hex, bg_hex) {
            let required = pair.min_ratio.max(4.5);
            if ratio < required {
                diags.push(CompilerDiagnostic::error(
                    "contrast_ratio_too_low",
                    format!("tokens.contrast_pairs[{idx}]"),
                    format!(
                        "contrast ratio {ratio:.2}:1 for {}/{} is below required {required:.2}:1",
                        pair.foreground, pair.background
                    ),
                ));
            }
        }
    }

    // 4. Global script overlay check
    for (idx, script) in doc.scripts.iter().enumerate() {
        if let Some(pat) = contains_banned_overlay(&script.src) {
            diags.push(CompilerDiagnostic::error(
                "banned_accessibility_overlay",
                format!("scripts[{idx}].src"),
                format!("banned accessibility overlay reference '{pat}' in script src"),
            ));
        }
    }

    // 5. Per-page semantic HTML, WCAG 2.2 AA, and overlay checks
    for (p_idx, page) in doc.pages.iter().enumerate() {
        let page_path = format!("pages[{p_idx}]");

        for (s_idx, script) in page.scripts.iter().enumerate() {
            if let Some(pat) = contains_banned_overlay(&script.src) {
                diags.push(CompilerDiagnostic::error(
                    "banned_accessibility_overlay",
                    format!("{page_path}.scripts[{s_idx}].src"),
                    format!("banned accessibility overlay reference '{pat}' in script src"),
                ));
            }
        }

        let expanded = expand_page_body(doc, &page.body);
        let mut stats = PageDomStats::default();
        collect_dom_stats(&expanded, &format!("{page_path}.body"), false, false, &mut stats);

        for (hit_path, pat) in stats.overlay_hits {
            diags.push(CompilerDiagnostic::error(
                "banned_accessibility_overlay",
                hit_path,
                format!("banned accessibility overlay pattern '{pat}' detected"),
            ));
        }

        // Dynamic pages with an empty body act as route metadata and have no rendered HTML.
        let check_page_semantics = matches!(page.route, RoutePattern::Static { .. })
            || !page.body.is_empty();
        if !check_page_semantics {
            continue;
        }

        // Headings: exactly one <h1>, starts with h1, no skipped levels
        let h1_count = stats.headings.iter().filter(|(lvl, _)| *lvl == 1).count();
        if h1_count == 0 {
            diags.push(CompilerDiagnostic::error(
                "missing_h1",
                page_path.clone(),
                format!("page '{}' must contain exactly one <h1>", page.id),
            ));
        } else if h1_count > 1 {
            diags.push(CompilerDiagnostic::error(
                "multiple_h1",
                page_path.clone(),
                format!(
                    "page '{}' contains {h1_count} <h1> elements; expected exactly 1",
                    page.id
                ),
            ));
        }

        if let Some((first_lvl, first_path)) = stats.headings.first()
            && *first_lvl != 1
        {
            diags.push(CompilerDiagnostic::error(
                "first_heading_not_h1",
                first_path.clone(),
                format!("first heading on page '{}' must be <h1>, found <h{first_lvl}>", page.id),
            ));
        }

        for pair in stats.headings.windows(2) {
            let (prev_lvl, _) = &pair[0];
            let (next_lvl, next_path) = &pair[1];
            if *next_lvl > *prev_lvl && (*next_lvl - *prev_lvl) > 1 {
                diags.push(CompilerDiagnostic::error(
                    "skipped_heading_level",
                    next_path.clone(),
                    format!("heading level jumped from <h{prev_lvl}> to <h{next_lvl}>"),
                ));
            }
        }

        // Landmarks
        if stats.header_count == 0 {
            diags.push(CompilerDiagnostic::error(
                "missing_header_landmark",
                page_path.clone(),
                format!("page '{}' is missing a <header> landmark", page.id),
            ));
        }
        if stats.main_count == 0 {
            diags.push(CompilerDiagnostic::error(
                "missing_main_landmark",
                page_path.clone(),
                format!("page '{}' is missing <main id=\"main\" tabindex=\"-1\">", page.id),
            ));
        } else if stats.main_count > 1 {
            diags.push(CompilerDiagnostic::error(
                "multiple_main_landmarks",
                page_path.clone(),
                format!("page '{}' has multiple <main> landmarks", page.id),
            ));
        } else if stats.valid_main_count == 0 {
            diags.push(CompilerDiagnostic::error(
                "invalid_main_landmark",
                page_path.clone(),
                format!(
                    "<main> on page '{}' must have id=\"main\" and tabindex=\"-1\"",
                    page.id
                ),
            ));
        }
        if stats.footer_count == 0 {
            diags.push(CompilerDiagnostic::error(
                "missing_footer_landmark",
                page_path.clone(),
                format!("page '{}' is missing a <footer> landmark", page.id),
            ));
        }
        if !stats.has_skip_link {
            diags.push(CompilerDiagnostic::error(
                "missing_skip_link",
                page_path.clone(),
                format!(
                    "page '{}' is missing <a class=\"skip-link\" href=\"#main\">",
                    page.id
                ),
            ));
        }

        // Form controls outside aria-hidden="true" must have an accessible label
        for ctrl in stats.controls {
            let has_for = ctrl
                .id
                .as_deref()
                .is_some_and(|id| stats.label_for_ids.contains(id));
            if !(ctrl.in_label || ctrl.has_aria_label || ctrl.has_aria_labelledby || has_for) {
                diags.push(CompilerDiagnostic::error(
                    "unlabeled_form_control",
                    ctrl.path,
                    format!("<{}> control is missing an accessible label", ctrl.tag),
                ));
            }
        }
    }

    // 6. Plugin lint diagnostics
    if let Some(reg) = registry {
        for pd in reg.collect_diagnostics(doc, raw_input) {
            let code = format!("plugin:{}:{}", pd.plugin_id, pd.rule);
            let msg = match pd.citation {
                Some(cit) if !cit.is_empty() => format!("{} ({cit})", pd.message),
                _ => pd.message,
            };
            let diag = match pd.severity {
                DiagnosticSeverity::Error => CompilerDiagnostic::error(code, pd.path, msg),
                DiagnosticSeverity::Warning | DiagnosticSeverity::Advisory => {
                    CompilerDiagnostic::warning(code, pd.path, msg)
                }
            };
            diags.push(diag);
        }
    }

    diags
}
