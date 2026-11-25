//! Deterministic structural and semantic validation for [`SiteDocument`].

use std::collections::BTreeSet;

use crate::types::{Node, RoutePattern, SiteDocument};

/// Single validation error with a JSON-pointer-like path and human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrValidationError {
    pub path: String,
    pub message: String,
}

impl IrValidationError {
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Return `true` if `slug` matches `^[a-z0-9]+(?:-[a-z0-9]+)*$`.
#[must_use]
pub fn is_valid_slug(slug: &str) -> bool {
    if slug.is_empty() {
        return false;
    }
    slug.split('-')
        .all(|seg| !seg.is_empty() && seg.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9')))
}

/// Return `true` if `color` is an exact `#RRGGBB` 6-digit hex color string.
#[must_use]
pub fn is_valid_hex_color(color: &str) -> bool {
    let Some(hex) = color.strip_prefix('#') else {
        return false;
    };
    hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Return `true` if `url` is a valid `https://` base URL without whitespace or control chars.
#[must_use]
pub fn is_valid_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if rest.is_empty()
        || url
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return false;
    }
    let host_part = rest.split('/').next().unwrap_or("");
    let host = host_part.split(':').next().unwrap_or("");
    !host.is_empty() && !host.starts_with('.') && !host.ends_with('.')
}

/// Validate a [`SiteDocument`] and return all discovered [`IrValidationError`] items.
#[must_use]
pub fn validate_ir(doc: &SiteDocument) -> Vec<IrValidationError> {
    let mut errors = Vec::new();

    if doc.schema_version != 1 {
        errors.push(IrValidationError::new(
            "schema_version",
            format!("unsupported schema_version {}; expected 1", doc.schema_version),
        ));
    }

    if !is_valid_slug(&doc.slug) {
        errors.push(IrValidationError::new(
            "slug",
            "slug must match ^[a-z0-9]+(?:-[a-z0-9]+)*$",
        ));
    }

    if doc.name.trim().is_empty() {
        errors.push(IrValidationError::new("name", "site name must not be empty"));
    }

    if doc.locale.trim().is_empty() {
        errors.push(IrValidationError::new("locale", "locale must not be empty"));
    }

    if !is_valid_https_url(&doc.base_url) {
        errors.push(IrValidationError::new(
            "base_url",
            "base_url must be a valid https:// URL",
        ));
    }

    for (key, hex) in &doc.tokens.colors {
        if key.trim().is_empty() {
            errors.push(IrValidationError::new(
                "tokens.colors",
                "color token key must not be empty",
            ));
        }
        if !is_valid_hex_color(hex) {
            errors.push(IrValidationError::new(
                format!("tokens.colors.{key}"),
                format!("color '{hex}' must be formatted as #RRGGBB"),
            ));
        }
    }

    if doc.tokens.fonts.body.trim().is_empty() {
        errors.push(IrValidationError::new(
            "tokens.fonts.body",
            "body font stack must not be empty",
        ));
    }

    for (idx, pair) in doc.tokens.contrast_pairs.iter().enumerate() {
        if !doc.tokens.colors.contains_key(&pair.foreground) {
            errors.push(IrValidationError::new(
                format!("tokens.contrast_pairs[{idx}].foreground"),
                format!("color token '{}' does not exist", pair.foreground),
            ));
        }
        if !doc.tokens.colors.contains_key(&pair.background) {
            errors.push(IrValidationError::new(
                format!("tokens.contrast_pairs[{idx}].background"),
                format!("color token '{}' does not exist", pair.background),
            ));
        }
        if !pair.min_ratio.is_finite() || pair.min_ratio <= 0.0 {
            errors.push(IrValidationError::new(
                format!("tokens.contrast_pairs[{idx}].min_ratio"),
                "min_ratio must be a positive finite number",
            ));
        }
    }

    if doc.pages.is_empty() {
        errors.push(IrValidationError::new(
            "pages",
            "site document must contain at least one page",
        ));
    }

    let mut seen_page_ids = BTreeSet::new();
    let mut seen_routes = BTreeSet::new();

    for (idx, page) in doc.pages.iter().enumerate() {
        if page.id.trim().is_empty() {
            errors.push(IrValidationError::new(
                format!("pages[{idx}].id"),
                "page id must not be empty",
            ));
        } else if !seen_page_ids.insert(page.id.clone()) {
            errors.push(IrValidationError::new(
                format!("pages[{idx}].id"),
                format!("duplicate page id '{}'", page.id),
            ));
        }

        match &page.route {
            RoutePattern::Static { path } => {
                if !path.starts_with('/') {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.path"),
                        format!("static route '{path}' must start with '/'"),
                    ));
                } else if !seen_routes.insert(path.clone()) {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.path"),
                        format!("duplicate route '{path}'"),
                    ));
                }
            }
            RoutePattern::Dynamic { pattern, params } => {
                if !pattern.starts_with('/') {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.pattern"),
                        format!("dynamic route pattern '{pattern}' must start with '/'"),
                    ));
                } else if !seen_routes.insert(pattern.clone()) {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.pattern"),
                        format!("duplicate route '{pattern}'"),
                    ));
                }

                if params.is_empty() {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.params"),
                        "dynamic route params must not be empty",
                    ));
                } else {
                    for (p_idx, param) in params.iter().enumerate() {
                        if param.trim().is_empty() {
                            errors.push(IrValidationError::new(
                                format!("pages[{idx}].route.params[{p_idx}]"),
                                "dynamic route parameter name must not be empty",
                            ));
                        }
                    }
                }
            }
        }

        validate_nodes(
            doc,
            &page.body,
            &format!("pages[{idx}].body"),
            &mut errors,
        );
    }

    for (comp_name, comp_def) in &doc.components {
        validate_nodes(
            doc,
            &comp_def.template,
            &format!("components.{comp_name}.template"),
            &mut errors,
        );
    }

    errors
}

fn validate_nodes(
    doc: &SiteDocument,
    nodes: &[Node],
    base_path: &str,
    errors: &mut Vec<IrValidationError>,
) {
    for (idx, node) in nodes.iter().enumerate() {
        let node_path = format!("{base_path}[{idx}]");
        match node {
            Node::Element { children, .. } => {
                validate_nodes(doc, children, &format!("{node_path}.children"), errors);
            }
            Node::Text { .. } => {}
            Node::SlotRef { fallback, .. } => {
                validate_nodes(doc, fallback, &format!("{node_path}.fallback"), errors);
            }
            Node::ComponentInstance {
                component, slots, ..
            } => {
                if !doc.components.contains_key(component) {
                    errors.push(IrValidationError::new(
                        format!("{node_path}.component"),
                        format!("referenced component '{component}' is not defined"),
                    ));
                }
                for (slot_name, slot_nodes) in slots {
                    validate_nodes(
                        doc,
                        slot_nodes,
                        &format!("{node_path}.slots.{slot_name}"),
                        errors,
                    );
                }
            }
            Node::Conditional {
                then_nodes,
                else_nodes,
                ..
            } => {
                validate_nodes(doc, then_nodes, &format!("{node_path}.then_nodes"), errors);
                validate_nodes(doc, else_nodes, &format!("{node_path}.else_nodes"), errors);
            }
            Node::Loop { body, .. } => {
                validate_nodes(doc, body, &format!("{node_path}.body"), errors);
            }
        }
    }
}
