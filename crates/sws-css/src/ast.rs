//! Typed CSS AST and deterministic stylesheet emitter.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

use crate::sanitize::{
    is_safe_css_value, is_safe_media_query, is_safe_property, is_safe_selector,
};

/// Error returned when a stylesheet or rule fails CSS safety or structural validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssError {
    pub message: String,
}

impl CssError {
    /// Creates a new [`CssError`] with the provided message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CssError {}

/// A single CSS rule targeting an optional breakpoint ID and optional pseudo-class/element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CssRule {
    pub selector: String,
    pub breakpoint: Option<String>,
    pub pseudo: Option<String>,
    pub declarations: BTreeMap<String, String>,
}

/// Deterministic CSS stylesheet AST with `:root` variables, `@media` breakpoints, and rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stylesheet {
    pub comment_header: Option<String>,
    pub root_vars: BTreeMap<String, String>,
    pub media_queries: BTreeMap<String, String>,
    pub rules: Vec<CssRule>,
}

impl Stylesheet {
    /// Creates an empty [`Stylesheet`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a `:root` CSS custom property, automatically prefixing `--` when absent.
    pub fn set_var(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let prefixed = if name.starts_with("--") {
            name
        } else {
            format!("--{name}")
        };
        self.root_vars.insert(prefixed, value.into());
    }

    /// Registers or updates a named `@media` breakpoint query.
    pub fn define_breakpoint(&mut self, id: impl Into<String>, media_query: impl Into<String>) {
        self.media_queries.insert(id.into(), media_query.into());
    }

    /// Appends a [`CssRule`] to the stylesheet.
    pub fn add_rule(&mut self, rule: CssRule) {
        self.rules.push(rule);
    }

    /// Appends an unconditional rule with no breakpoint or pseudo suffix.
    pub fn add_simple_rule(
        &mut self,
        selector: impl Into<String>,
        declarations: BTreeMap<String, String>,
    ) {
        self.rules.push(CssRule {
            selector: selector.into(),
            breakpoint: None,
            pseudo: None,
            declarations,
        });
    }

    /// Adds standard `@media (prefers-reduced-motion: ...)` safety rules.
    pub fn include_reduced_motion_defaults(&mut self) {
        self.define_breakpoint(
            "reduced-motion-no-preference",
            "(prefers-reduced-motion: no-preference)",
        );
        self.define_breakpoint(
            "reduced-motion-reduce",
            "(prefers-reduced-motion: reduce)",
        );

        let mut smooth = BTreeMap::new();
        smooth.insert("scroll-behavior".to_string(), "smooth".to_string());
        self.add_rule(CssRule {
            selector: "html".to_string(),
            breakpoint: Some("reduced-motion-no-preference".to_string()),
            pseudo: None,
            declarations: smooth,
        });

        let mut reduce = BTreeMap::new();
        reduce.insert("animation".to_string(), "none !important".to_string());
        reduce.insert("scroll-behavior".to_string(), "auto !important".to_string());
        reduce.insert("transition".to_string(), "none !important".to_string());
        self.add_rule(CssRule {
            selector: "*, *::before, *::after".to_string(),
            breakpoint: Some("reduced-motion-reduce".to_string()),
            pseudo: None,
            declarations: reduce,
        });
    }

    /// Validates all stylesheet components and emits deterministic CSS.
    pub fn emit(&self) -> Result<String, CssError> {
        let mut out = String::new();

        if let Some(header) = &self.comment_header {
            let trimmed = header.trim();
            if trimmed.contains("/*")
                || trimmed.contains("*/")
                || trimmed.contains('\0')
                || trimmed.to_ascii_lowercase().contains("</style")
            {
                return Err(CssError::new(
                    "comment_header contains unsafe CSS comment breakout tokens",
                ));
            }
            if !trimmed.is_empty() {
                let _ = writeln!(out, "/* {trimmed} */");
            }
        }

        if !self.root_vars.is_empty() {
            out.push_str(":root {\n");
            for (prop, val) in &self.root_vars {
                if !prop.starts_with("--") || !is_safe_property(prop) {
                    return Err(CssError::new(format!(
                        "unsafe CSS custom property name: {prop}"
                    )));
                }
                if !is_safe_css_value(val) {
                    return Err(CssError::new(format!(
                        "unsafe CSS custom property value for {prop}"
                    )));
                }
                let _ = writeln!(out, "  {prop}: {};", val.trim());
            }
            out.push_str("}\n");
        }

        let rules_css = Self::emit_rule_list(&self.rules, &self.media_queries)?;
        out.push_str(&rules_css);
        Ok(out)
    }

    /// Validates and emits a slice of [`CssRule`] values using the supplied `media_queries` map.
    pub fn emit_rule_list(
        rules: &[CssRule],
        media_queries: &BTreeMap<String, String>,
    ) -> Result<String, CssError> {
        let mut normalized_mqs: BTreeMap<String, String> = BTreeMap::new();
        for (id, mq) in media_queries {
            if id.trim().is_empty() {
                return Err(CssError::new("breakpoint id must not be empty"));
            }
            normalized_mqs.insert(id.clone(), normalize_media_query(mq)?);
        }

        let mut base_rules = RuleBucket::default();
        let mut bp_rules: BTreeMap<String, RuleBucket> = BTreeMap::new();

        for rule in rules {
            let effective_selector =
                combine_selector(&rule.selector, rule.pseudo.as_deref())?;

            for (prop, val) in &rule.declarations {
                if !is_safe_property(prop) {
                    return Err(CssError::new(format!("unsafe CSS property: {prop}")));
                }
                if !is_safe_css_value(val) {
                    return Err(CssError::new(format!(
                        "unsafe CSS value for property {prop}"
                    )));
                }
            }

            let bucket = match &rule.breakpoint {
                None => &mut base_rules,
                Some(bp_id) => {
                    if !normalized_mqs.contains_key(bp_id) {
                        return Err(CssError::new(format!(
                            "unknown breakpoint id: {bp_id}"
                        )));
                    }
                    bp_rules.entry(bp_id.clone()).or_default()
                }
            };

            if !rule.declarations.is_empty() {
                bucket.merge_rule(effective_selector, &rule.declarations);
            }
        }

        let mut out = String::new();
        for (selector, decls) in &base_rules.entries {
            write_rule(&mut out, selector, decls, "");
        }

        for (bp_id, bucket) in &bp_rules {
            if bucket.entries.is_empty() {
                continue;
            }
            if let Some(mq) = normalized_mqs.get(bp_id) {
                let _ = writeln!(out, "{mq} {{");
                for (selector, decls) in &bucket.entries {
                    write_rule(&mut out, selector, decls, "  ");
                }
                out.push_str("}\n");
            }
        }

        Ok(out)
    }
}

#[derive(Default)]
struct RuleBucket {
    index: BTreeMap<String, usize>,
    entries: Vec<(String, BTreeMap<String, String>)>,
}

impl RuleBucket {
    fn merge_rule(&mut self, selector: String, declarations: &BTreeMap<String, String>) {
        if let Some(&idx) = self.index.get(&selector) {
            for (prop, val) in declarations {
                self.entries[idx]
                    .1
                    .insert(prop.clone(), val.trim().to_string());
            }
        } else {
            let idx = self.entries.len();
            let mut normalized = BTreeMap::new();
            for (prop, val) in declarations {
                normalized.insert(prop.clone(), val.trim().to_string());
            }
            self.index.insert(selector.clone(), idx);
            self.entries.push((selector, normalized));
        }
    }
}

fn write_rule(
    out: &mut String,
    selector: &str,
    declarations: &BTreeMap<String, String>,
    indent: &str,
) {
    let _ = write!(out, "{indent}{selector} {{ ");
    for (idx, (prop, val)) in declarations.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{prop}: {val};");
    }
    out.push_str(" }\n");
}

fn normalize_media_query(mq: &str) -> Result<String, CssError> {
    if !is_safe_media_query(mq) {
        return Err(CssError::new(format!("unsafe media query: {mq}")));
    }
    let trimmed = mq.trim();
    if let Some(rest) = trimmed.strip_prefix("@media") {
        let cond = rest.trim();
        if cond.is_empty() {
            return Err(CssError::new("empty @media condition"));
        }
        Ok(format!("@media {cond}"))
    } else {
        Ok(format!("@media {trimmed}"))
    }
}

fn combine_selector(selector: &str, pseudo: Option<&str>) -> Result<String, CssError> {
    if !is_safe_selector(selector) {
        return Err(CssError::new(format!("unsafe CSS selector: {selector}")));
    }
    let trimmed_sel = selector.trim();

    let Some(raw_pseudo) = pseudo else {
        return Ok(trimmed_sel.to_string());
    };

    let trimmed_pseudo = raw_pseudo.trim();
    if trimmed_pseudo.is_empty() || !is_safe_selector(trimmed_pseudo) {
        return Err(CssError::new(format!(
            "unsafe CSS pseudo-selector: {raw_pseudo}"
        )));
    }
    let suffix = if trimmed_pseudo.starts_with(':') {
        trimmed_pseudo.to_string()
    } else {
        format!(":{trimmed_pseudo}")
    };

    let parts = split_selector_list(trimmed_sel);
    let mut combined_parts = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            return Err(CssError::new(format!(
                "invalid empty selector segment in: {selector}"
            )));
        }
        combined_parts.push(format!("{part}{suffix}"));
    }

    let combined = combined_parts.join(", ");
    if !is_safe_selector(&combined) {
        return Err(CssError::new(format!(
            "unsafe combined CSS selector: {combined}"
        )));
    }
    Ok(combined)
}

fn split_selector_list(selector: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut paren_depth: usize = 0;
    let mut bracket_depth: usize = 0;

    for (idx, ch) in selector.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '(' => paren_depth += 1,
                ')' => paren_depth = paren_depth.saturating_sub(1),
                '[' => bracket_depth += 1,
                ']' => bracket_depth = bracket_depth.saturating_sub(1),
                ',' if paren_depth == 0 && bracket_depth == 0 => {
                    parts.push(&selector[start..idx]);
                    start = idx + 1;
                }
                _ => {}
            },
        }
    }
    parts.push(&selector[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn emits_deterministic_root_vars_and_header() {
        let mut sheet_a = Stylesheet::new();
        sheet_a.comment_header = Some("Generated by SWS. Do not edit.".to_string());
        sheet_a.set_var("bg", "#E9E2D2");
        sheet_a.set_var("--text", "#241207");
        sheet_a.add_simple_rule(
            "body",
            decls(&[("background", "var(--bg)"), ("color", "var(--text)")]),
        );

        let mut sheet_b = Stylesheet::new();
        sheet_b.comment_header = Some("Generated by SWS. Do not edit.".to_string());
        sheet_b.set_var("--text", "#241207");
        sheet_b.set_var("bg", "#E9E2D2");
        sheet_b.add_simple_rule(
            "body",
            decls(&[("color", "var(--text)"), ("background", "var(--bg)")]),
        );

        let css_a = sheet_a.emit().expect("valid stylesheet");
        let css_b = sheet_b.emit().expect("valid stylesheet");
        assert_eq!(css_a, css_b);
        assert_eq!(
            css_a,
            "/* Generated by SWS. Do not edit. */\n:root {\n  --bg: #E9E2D2;\n  --text: #241207;\n}\nbody { background: var(--bg); color: var(--text); }\n"
        );
    }

    #[test]
    fn deduplicates_identical_rules_and_merges_same_selector() {
        let mut sheet = Stylesheet::new();
        sheet.add_simple_rule("a", decls(&[("color", "var(--accent)")]));
        sheet.add_simple_rule("a", decls(&[("color", "var(--accent)")]));
        sheet.add_simple_rule(
            "a",
            decls(&[
                ("text-decoration-thickness", ".08em"),
                ("text-underline-offset", ".18em"),
            ]),
        );

        let css = sheet.emit().expect("valid stylesheet");
        assert_eq!(
            css,
            "a { color: var(--accent); text-decoration-thickness: .08em; text-underline-offset: .18em; }\n"
        );
    }

    #[test]
    fn supports_pseudo_classes_and_comma_separated_selectors() {
        let mut sheet = Stylesheet::new();
        sheet.add_rule(CssRule {
            selector: "a".to_string(),
            breakpoint: None,
            pseudo: Some(":hover".to_string()),
            declarations: decls(&[("text-decoration-thickness", ".16em")]),
        });
        sheet.add_rule(CssRule {
            selector: ".skip-link".to_string(),
            breakpoint: None,
            pseudo: Some("focus-visible".to_string()),
            declarations: decls(&[("outline", "3px solid var(--bg)")]),
        });
        sheet.add_rule(CssRule {
            selector: "a, button".to_string(),
            breakpoint: None,
            pseudo: Some(":focus-visible".to_string()),
            declarations: decls(&[("outline-offset", "3px")]),
        });

        let css = sheet.emit().expect("valid stylesheet");
        assert!(css.contains("a:hover { text-decoration-thickness: .16em; }"));
        assert!(css.contains(".skip-link:focus-visible { outline: 3px solid var(--bg); }"));
        assert!(css.contains("a:focus-visible, button:focus-visible { outline-offset: 3px; }"));
    }

    #[test]
    fn supports_media_breakpoints_and_reduced_motion_defaults() {
        let mut sheet = Stylesheet::new();
        sheet.define_breakpoint("sm", "(min-width: 40rem)");
        sheet.define_breakpoint("md", "@media (min-width: 48rem)");
        sheet.include_reduced_motion_defaults();
        sheet.add_simple_rule("body", decls(&[("margin", "0")]));
        sheet.add_rule(CssRule {
            selector: ".grid".to_string(),
            breakpoint: Some("sm".to_string()),
            pseudo: None,
            declarations: decls(&[("grid-template-columns", "1fr 1fr")]),
        });
        sheet.add_rule(CssRule {
            selector: ".footer-grid".to_string(),
            breakpoint: Some("md".to_string()),
            pseudo: None,
            declarations: decls(&[("grid-template-columns", "3fr 2fr")]),
        });

        let css = sheet.emit().expect("valid stylesheet");
        assert!(css.contains("body { margin: 0; }"));
        assert!(css.contains("@media (min-width: 40rem) {\n  .grid { grid-template-columns: 1fr 1fr; }\n}"));
        assert!(css.contains("@media (min-width: 48rem) {\n  .footer-grid { grid-template-columns: 3fr 2fr; }\n}"));
        assert!(css.contains("@media (prefers-reduced-motion: no-preference) {\n  html { scroll-behavior: smooth; }\n}"));
        assert!(css.contains("@media (prefers-reduced-motion: reduce) {\n  *, *::before, *::after { animation: none !important; scroll-behavior: auto !important; transition: none !important; }\n}"));
    }

    #[test]
    fn rejects_invalid_selectors_properties_values_headers_and_breakpoints() {
        let mut bad_bp = Stylesheet::new();
        bad_bp.add_rule(CssRule {
            selector: "body".to_string(),
            breakpoint: Some("missing".to_string()),
            pseudo: None,
            declarations: decls(&[("margin", "0")]),
        });
        let err = bad_bp.emit().expect_err("unknown breakpoint");
        assert!(err.to_string().contains("unknown breakpoint"));

        let mut bad_sel = Stylesheet::new();
        bad_sel.add_simple_rule("body { color: red }", decls(&[("margin", "0")]));
        assert!(bad_sel.emit().is_err());

        let mut bad_prop = Stylesheet::new();
        bad_prop.add_simple_rule("body", decls(&[("color;x", "red")]));
        assert!(bad_prop.emit().is_err());

        let mut bad_val = Stylesheet::new();
        bad_val.add_simple_rule("body", decls(&[("color", "red; background: blue")]));
        assert!(bad_val.emit().is_err());

        let mut bad_var = Stylesheet::new();
        bad_var.set_var("font", "Arial; color: red");
        assert!(bad_var.emit().is_err());

        let mut bad_header = Stylesheet::new();
        bad_header.comment_header = Some("ok */ body { display: none; } /*".to_string());
        assert!(bad_header.emit().is_err());

        let mut bad_mq = Stylesheet::new();
        bad_mq.define_breakpoint("evil", "(min-width: 1px) { body { display: none }");
        assert!(bad_mq.emit().is_err());
    }

    #[test]
    fn round_trips_through_serde_json() {
        let mut sheet = Stylesheet::new();
        sheet.set_var("bg", "#FAF8F5");
        sheet.define_breakpoint("sm", "(min-width: 40rem)");
        sheet.add_rule(CssRule {
            selector: ".card".to_string(),
            breakpoint: Some("sm".to_string()),
            pseudo: Some(":hover".to_string()),
            declarations: decls(&[("background", "var(--bg)")]),
        });

        let json = serde_json::to_string(&sheet).expect("serialize");
        let parsed: Stylesheet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(sheet, parsed);
        assert_eq!(sheet.emit().unwrap(), parsed.emit().unwrap());
    }
}
