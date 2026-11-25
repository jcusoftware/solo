//! CSS injection guards for selectors, properties, values, and font stacks.

fn has_common_css_injection(val: &str) -> bool {
    if val.contains([';', '{', '}', '\n', '\r', '\u{000C}', '\\', '\0']) {
        return true;
    }
    if val.chars().any(|c| c.is_control() && c != '\t') {
        return true;
    }
    if val.contains("/*") || val.contains("*/") {
        return true;
    }
    val.to_ascii_lowercase().contains("</style")
}

fn is_safe_css_url_arg(raw_arg: &str) -> bool {
    let mut inner = raw_arg.trim();
    if (inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2)
        || (inner.starts_with('\'') && inner.ends_with('\'') && inner.len() >= 2)
    {
        inner = inner[1..inner.len() - 1].trim();
    }
    if inner.is_empty() || inner.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let lower = inner.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("vbscript:") {
        return false;
    }
    if let Some(rest) = lower.strip_prefix("data:") {
        const SAFE_RASTER_DATA_MIMES: &[&str] = &[
            "image/png",
            "image/jpeg",
            "image/webp",
            "image/gif",
            "image/avif",
        ];
        return SAFE_RASTER_DATA_MIMES.iter().any(|mime| {
            rest.strip_prefix(mime)
                .is_some_and(|after| after.starts_with(';') || after.starts_with(','))
        });
    }
    true
}

fn are_safe_css_urls(val: &str) -> bool {
    let lower = val.to_ascii_lowercase();
    let mut offset = 0usize;
    while let Some(rel) = lower[offset..].find("url(") {
        let arg_start = offset + rel + 4;
        let bytes = val.as_bytes();
        let mut idx = arg_start;
        let mut quote: Option<u8> = None;
        let mut found_end = None;
        while idx < bytes.len() {
            let b = bytes[idx];
            match quote {
                Some(q) if b == q => quote = None,
                Some(_) => {}
                None if b == b'"' || b == b'\'' => quote = Some(b),
                None if b == b')' => {
                    found_end = Some(idx);
                    break;
                }
                None => {}
            }
            idx += 1;
        }
        let Some(arg_end) = found_end else {
            return false;
        };
        if !is_safe_css_url_arg(&val[arg_start..arg_end]) {
            return false;
        }
        offset = arg_end + 1;
    }
    true
}

/// Returns `true` if single and double quotes in `val` are properly paired.
///
/// Quotes of a different kind inside an active quoted string (such as `'` inside `"..."`)
/// are treated as literal characters, matching CSS string tokenization.
#[must_use]
pub fn has_balanced_quotes(val: &str) -> bool {
    let mut quote: Option<char> = None;
    for ch in val.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None => {}
        }
    }
    quote.is_none()
}

/// Validates a CSS `font-family` stack against CSS breakout and injection payloads.
#[must_use]
pub fn is_safe_font_stack(val: &str) -> bool {
    if val.trim().is_empty() || has_common_css_injection(val) || !has_balanced_quotes(val) {
        return false;
    }
    let lower = val.to_ascii_lowercase();
    !lower.contains("url(") && !lower.contains("expression(")
}

/// Validates a CSS declaration value, rejecting injection tokens, `expression(`, and `url(`.
#[must_use]
pub fn is_safe_css_value(val: &str) -> bool {
    is_safe_css_value_with_url(val, false)
}

/// Validates a CSS declaration value with an explicit opt-in for safe `url(...)` values.
///
/// When `allow_safe_url` is `true`, `url(...)` is permitted only if it does not reference
/// `javascript:`, `vbscript:`, or `data:text/html`.
#[must_use]
pub fn is_safe_css_value_with_url(val: &str, allow_safe_url: bool) -> bool {
    if val.trim().is_empty() || has_common_css_injection(val) || !has_balanced_quotes(val) {
        return false;
    }
    let lower = val.to_ascii_lowercase();
    if lower.contains("expression(") {
        return false;
    }
    if lower.contains("url(") {
        if !allow_safe_url {
            return false;
        }
        if !are_safe_css_urls(val) {
            return false;
        }
    }
    true
}

/// Validates a CSS selector against rule-block breakout, comment injection, and `</style` tags.
#[must_use]
pub fn is_safe_selector(sel: &str) -> bool {
    !sel.trim().is_empty() && !has_common_css_injection(sel) && has_balanced_quotes(sel)
}

/// Validates a CSS property name (ASCII `[a-zA-Z0-9_-]`, starting with an ASCII letter or `--`).
#[must_use]
pub fn is_safe_property(prop: &str) -> bool {
    let bytes = prop.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let valid_start =
        bytes[0].is_ascii_alphabetic() || (bytes.len() > 2 && bytes[0] == b'-' && bytes[1] == b'-');
    valid_start
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

pub(crate) fn is_safe_media_query(mq: &str) -> bool {
    let trimmed = mq.trim();
    if trimmed.is_empty() || has_common_css_injection(trimmed) || !has_balanced_quotes(trimmed) {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("url(") || lower.contains("expression(") {
        return false;
    }
    let mut depth: i32 = 0;
    for ch in trimmed.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_balanced_quotes() {
        assert!(has_balanced_quotes(""));
        assert!(has_balanced_quotes(r#""Source Serif 4", Georgia, serif"#));
        assert!(has_balanced_quotes("'DM Sans', sans-serif"));
        assert!(has_balanced_quotes(r#""O'Malley Sans", Georgia, serif"#));

        assert!(!has_balanced_quotes(r#""Arial"#));
        assert!(!has_balanced_quotes(r#"Arial""#));
        assert!(!has_balanced_quotes("'DM Sans"));
        assert!(!has_balanced_quotes("DM Sans'"));
        assert!(!has_balanced_quotes(r#""DM Sans'"#));
        assert!(!has_balanced_quotes(r#""Source Serif 4"""#));
    }

    #[test]
    fn validates_font_stacks_matching_renderer_tests() {
        let bad_fonts = [
            "",
            "   ",
            "Arial; color: red",
            "Arial { color: red }",
            "Arial, url(https://evil.example/a.css)",
            "Arial, URL(https://evil.example/a.css)",
            "Arial /* comment */",
            "Arial */",
            "Arial\nbody{display:none}",
            "Arial\rbody{display:none}",
            "Arial\\",
            "Arial\\; color: red",
            r#""Arial"#,
            r#"Arial""#,
            "'DM Sans",
            "DM Sans'",
            r#""DM Sans'"#,
            r#""Source Serif 4"""#,
            "Arial </style><script>alert(1)</script>",
        ];
        for bad in bad_fonts {
            assert!(!is_safe_font_stack(bad), "expected rejection for {bad:?}");
        }

        let good_fonts = [
            "system-ui, sans-serif",
            "Georgia, serif",
            r#""Source Serif 4", Georgia, serif"#,
            "'DM Sans', sans-serif",
            r#""O'Malley Sans", Georgia, serif"#,
        ];
        for good in good_fonts {
            assert!(is_safe_font_stack(good), "expected acceptance for {good:?}");
        }
    }

    #[test]
    fn validates_css_values() {
        assert!(is_safe_css_value("1.0625rem"));
        assert!(is_safe_css_value("clamp(2rem, 5vw, 3rem)"));
        assert!(is_safe_css_value("none !important"));
        assert!(is_safe_css_value("var(--accent)"));
        assert!(is_safe_css_value("inset 0 0 0 2px currentColor"));

        for bad in [
            "",
            "   ",
            "red; background: blue",
            "red { color: blue }",
            "red\ncolor: blue",
            "red\rcolor: blue",
            "red\\",
            "red /* comment */",
            "red */",
            "expression(alert(1))",
            "EXPRESSION(alert(1))",
            "url(https://evil.example/track.png)",
            "URL(https://evil.example/track.png)",
            r#""unclosed"#,
            "1px </style><script>",
        ] {
            assert!(!is_safe_css_value(bad), "expected rejection for {bad:?}");
        }

        assert!(is_safe_css_value_with_url(
            "url(https://assets.example/bg.webp)",
            true
        ));
        assert!(is_safe_css_value_with_url(
            "url('data:image/png,iVBORw0KGgo=')",
            true
        ));
        assert!(!is_safe_css_value("red\u{000C}background:blue"));
        assert!(!is_safe_css_value_with_url(
            "url(javascript:alert(1))",
            true
        ));
        assert!(!is_safe_css_value_with_url(
            "url(data:text/html,<script>)",
            true
        ));
        assert!(!is_safe_css_value_with_url(
            "url(data:image/svg+xml,<svg/>)",
            true
        ));
        assert!(!is_safe_css_value_with_url(
            "url(\"data:application/xhtml+xml,<html/>\")",
            true
        ));
        assert!(!is_safe_css_value_with_url(
            "url(data:text/javascript,alert(1))",
            true
        ));
    }

    #[test]
    fn validates_selectors() {
        assert!(is_safe_selector("body"));
        assert!(is_safe_selector("*, *::before, *::after"));
        assert!(is_safe_selector(r#"input[aria-invalid="true"], select[aria-invalid="true"]"#));
        assert!(is_safe_selector(".skip-link:focus-visible"));

        for bad in [
            "",
            "   ",
            "body { color: red }",
            "body; color: red",
            "body\n.x",
            "body\r.x",
            "body /* comment */",
            "body */",
            "body </style><script>",
            "body </STYLE>",
            r#"input[name="unclosed]"#,
        ] {
            assert!(!is_safe_selector(bad), "expected rejection for {bad:?}");
        }
    }

    #[test]
    fn validates_properties() {
        assert!(is_safe_property("color"));
        assert!(is_safe_property("background-color"));
        assert!(is_safe_property("--bg"));
        assert!(is_safe_property("--accent-2"));
        assert!(is_safe_property("--font_body"));

        for bad in [
            "",
            "--",
            "-webkit-text-size-adjust",
            "_private",
            "123abc",
            "color:red",
            "color;x",
            "color name",
            "color\n",
            "cölör",
        ] {
            assert!(!is_safe_property(bad), "expected rejection for {bad:?}");
        }
    }
}
