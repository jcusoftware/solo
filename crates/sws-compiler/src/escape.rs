//! HTML5 and `<script type="application/ld+json">` escaping helpers.

use serde_json::Value;

/// Escape HTML special characters (`&`, `<`, `>`, `"`, `'`) for safe text and attribute output.
#[must_use]
pub fn esc_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Serialize a JSON-LD [`Value`] for safe embedding inside `<script type="application/ld+json">`.
///
/// Neutralizes `<`, `>`, `&`, and Unicode line/paragraph separators (`\u{2028}`, `\u{2029}`)
/// so `</script>` breakouts and JS line-terminator issues cannot occur.
#[must_use]
pub fn esc_json_ld(val: &Value) -> String {
    let raw = serde_json::to_string(val).unwrap_or_else(|_| "{}".to_string());
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '<' => out.push_str(r"\u003c"),
            '>' => out.push_str(r"\u003e"),
            '&' => out.push_str(r"\u0026"),
            '\u{2028}' => out.push_str(r"\u2028"),
            '\u{2029}' => out.push_str(r"\u2029"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn escapes_html_metacharacters() {
        assert_eq!(
            esc_html(r#"<a href="x" title='y'>Tom & Jerry</a>"#),
            "&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;Tom &amp; Jerry&lt;/a&gt;"
        );
    }

    #[test]
    fn escapes_json_ld_script_breakouts_and_separators() {
        let val = json!({
            "name": "</script><script>alert('xss') & \u{2028}\u{2029}"
        });
        let encoded = esc_json_ld(&val);
        assert!(!encoded.contains('<'));
        assert!(!encoded.contains('>'));
        assert!(!encoded.contains('&'));
        assert!(!encoded.contains('\u{2028}'));
        assert!(!encoded.contains('\u{2029}'));
        assert!(encoded.contains(r"\u003c/script\u003e"));
        assert!(encoded.contains(r"\u0026"));
        assert!(encoded.contains(r"\u2028\u2029"));
    }
}
