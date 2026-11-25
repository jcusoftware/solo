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

/// Return `true` if `url` is a valid `https://` URL without whitespace, control chars,
/// backslashes, or embedded credentials.
#[must_use]
pub fn is_valid_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if rest.is_empty()
        || url
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || b == b'\\')
    {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let (host, port_opt) = if let Some(after_open) = authority.strip_prefix('[') {
        let Some((ipv6, tail)) = after_open.split_once(']') else {
            return false;
        };
        if ipv6.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        let port = if tail.is_empty() {
            None
        } else if let Some(p) = tail.strip_prefix(':') {
            Some(p)
        } else {
            return false;
        };
        (ipv6, port)
    } else {
        match authority.split_once(':') {
            Some((h, p)) => {
                if p.contains(':') {
                    return false;
                }
                (h, Some(p))
            }
            None => (authority, None),
        }
    };

    if let Some(port) = port_opt {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if !port.parse::<u16>().is_ok_and(|n| n > 0) {
            return false;
        }
    }

    if authority.starts_with('[') {
        return true;
    }

    if host.is_empty() || host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn is_safe_static_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg != "."
        && seg != ".."
        && seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Return `true` if `path` is a safe static route (starts with `/`, no traversal, no query/fragment).
#[must_use]
pub fn is_valid_static_route(path: &str) -> bool {
    if !path.starts_with('/')
        || path.len() > 200
        || path.contains("//")
        || path
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || matches!(b, b'\\' | b'?' | b'#'))
    {
        return false;
    }
    if path == "/" {
        return true;
    }
    let trimmed = path.strip_prefix('/').unwrap_or("");
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    !trimmed.is_empty() && trimmed.split('/').all(is_safe_static_segment)
}

/// Return `true` if `pattern` is a safe dynamic route pattern (segments are static or `{param}`).
#[must_use]
pub fn is_valid_dynamic_route(pattern: &str) -> bool {
    if !pattern.starts_with('/')
        || pattern.len() > 200
        || pattern.contains("//")
        || pattern
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || matches!(b, b'\\' | b'?' | b'#'))
    {
        return false;
    }
    let trimmed = pattern.strip_prefix('/').unwrap_or("");
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    !trimmed.is_empty()
        && trimmed.split('/').all(|seg| {
            if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                !inner.is_empty()
                    && inner
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            } else {
                is_safe_static_segment(seg)
            }
        })
}

const MAX_IR_NODE_DEPTH: usize = 128;

fn canonical_route_key(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_valid_robots_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 200
        && !path.bytes().any(|b| b <= 0x20 || b == 0x7f)
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
                if !is_valid_static_route(path) {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.path"),
                        format!(
                            "static route '{path}' must start with '/' and contain only safe path segments"
                        ),
                    ));
                } else if !seen_routes.insert(canonical_route_key(path)) {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.path"),
                        format!("duplicate route '{path}'"),
                    ));
                }
            }
            RoutePattern::Dynamic { pattern, params } => {
                if !is_valid_dynamic_route(pattern) {
                    errors.push(IrValidationError::new(
                        format!("pages[{idx}].route.pattern"),
                        format!(
                            "dynamic route pattern '{pattern}' must start with '/' and contain only safe segments"
                        ),
                    ));
                } else if !seen_routes.insert(canonical_route_key(pattern)) {
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

        if let Some(canonical) = &page.canonical
            && !canonical.trim().is_empty()
            && !is_valid_https_url(canonical)
        {
            errors.push(IrValidationError::new(
                format!("pages[{idx}].canonical"),
                format!("canonical URL '{canonical}' must be a valid https:// URL"),
            ));
        }

        validate_nodes(
            doc,
            &page.body,
            &format!("pages[{idx}].body"),
            0,
            &mut errors,
        );
    }

    for (idx, allow) in doc.seo.robots_allow.iter().enumerate() {
        if !is_valid_robots_path(allow) {
            errors.push(IrValidationError::new(
                format!("seo.robots_allow[{idx}]"),
                format!("robots_allow entry '{allow}' must start with '/' and contain no whitespace or control characters"),
            ));
        }
    }
    for (idx, disallow) in doc.seo.robots_disallow.iter().enumerate() {
        if !is_valid_robots_path(disallow) {
            errors.push(IrValidationError::new(
                format!("seo.robots_disallow[{idx}]"),
                format!("robots_disallow entry '{disallow}' must start with '/' and contain no whitespace or control characters"),
            ));
        }
    }
    for (idx, route) in doc.seo.sitemap_routes.iter().enumerate() {
        if !is_valid_static_route(route) {
            errors.push(IrValidationError::new(
                format!("seo.sitemap_routes[{idx}]"),
                format!("sitemap route '{route}' must be a safe static route starting with '/'"),
            ));
        }
    }

    for (comp_name, comp_def) in &doc.components {
        validate_nodes(
            doc,
            &comp_def.template,
            &format!("components.{comp_name}.template"),
            0,
            &mut errors,
        );
    }

    errors
}

fn validate_nodes(
    doc: &SiteDocument,
    nodes: &[Node],
    base_path: &str,
    depth: usize,
    errors: &mut Vec<IrValidationError>,
) {
    if depth >= MAX_IR_NODE_DEPTH {
        errors.push(IrValidationError::new(
            base_path,
            format!("node nesting depth exceeds maximum limit ({MAX_IR_NODE_DEPTH})"),
        ));
        return;
    }
    for (idx, node) in nodes.iter().enumerate() {
        let node_path = format!("{base_path}[{idx}]");
        match node {
            Node::Element { children, .. } => {
                validate_nodes(
                    doc,
                    children,
                    &format!("{node_path}.children"),
                    depth + 1,
                    errors,
                );
            }
            Node::Text { .. } => {}
            Node::SlotRef { fallback, .. } => {
                validate_nodes(
                    doc,
                    fallback,
                    &format!("{node_path}.fallback"),
                    depth + 1,
                    errors,
                );
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
                        depth + 1,
                        errors,
                    );
                }
            }
            Node::Conditional {
                then_nodes,
                else_nodes,
                ..
            } => {
                validate_nodes(
                    doc,
                    then_nodes,
                    &format!("{node_path}.then_nodes"),
                    depth + 1,
                    errors,
                );
                validate_nodes(
                    doc,
                    else_nodes,
                    &format!("{node_path}.else_nodes"),
                    depth + 1,
                    errors,
                );
            }
            Node::Loop { body, .. } => {
                validate_nodes(
                    doc,
                    body,
                    &format!("{node_path}.body"),
                    depth + 1,
                    errors,
                );
            }
        }
    }
}
