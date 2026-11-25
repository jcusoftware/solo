//! Deterministic CSS-text-to-[`Stylesheet`] parser.
//!
//! Parses a deliberately small, sanitizer-compatible subset of CSS:
//!
//! - `/* ... */` comments. The first comment, when it precedes all other
//!   content, is captured as [`Stylesheet::comment_header`] so round-trips
//!   preserve it; every other comment is skipped.
//! - one `:root { --custom-prop: value; }`-style block whose declarations
//!   populate [`Stylesheet::root_vars`] (declarations there must be
//!   `--`-prefixed custom properties);
//! - top-level rules (`selector { property: value; ... }`);
//! - one level of `@media (query) { ... }` blocks grouping rules by a
//!   breakpoint ID derived from the query text itself. Nested at-rules are
//!   rejected.
//!
//! ## Security model
//!
//! The parser is strict: it only accepts constructs the existing
//! sanitizer-gated emitter ([`Stylesheet::emit`]) accepts. Selector,
//! property, value, media-query, and comment-header safety are validated at
//! parse time with the very same `crate::sanitize` predicates the emitter
//! uses, so a successfully parsed [`Stylesheet`] is guaranteed to emit
//! through [`Stylesheet::emit`] without introducing unsafe output. The
//! emitter keeps its own validation as defense in depth.
//!
//! ## Determinism
//!
//! Parse order never changes emitted output: duplicate custom properties and
//! duplicate declarations resolve last-wins, root vars and declarations are
//! keyed maps (emitted sorted), and a `@media` breakpoint ID is the query
//! text itself, so two blocks with the same query deterministically merge.
//!
//! ## Robustness
//!
//! Parser input is untrusted: there is no recursion anywhere in the parser,
//! no `unwrap`/`expect`/`panic!` reachable from input, and malformed input
//! always yields a [`CssError`] with a line/column diagnostic. Input size,
//! rule count, and declaration count are capped ([`MAX_INPUT_BYTES`],
//! [`MAX_RULES`], [`MAX_DECLARATIONS`]), mirroring the workspace's defensive
//! cap style (cf. `sws_security::validate::MAX_BODY_BYTES`).

use std::collections::BTreeMap;

use crate::ast::{CssError, CssRule, Stylesheet};
use crate::sanitize::{
    is_safe_css_value, is_safe_media_query, is_safe_property, is_safe_selector,
};

/// Maximum accepted CSS input size (1 MiB).
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

/// Maximum number of parsed rules (top-level and inside `@media` blocks).
pub const MAX_RULES: usize = 10_000;

/// Maximum number of parsed declarations, including `:root` custom properties.
pub const MAX_DECLARATIONS: usize = 100_000;

/// Parses CSS text into a [`Stylesheet`].
///
/// Accepts the subset documented on [`crate::parser`]: comments, `:root`
/// custom properties, top-level rules, and one level of `@media` blocks.
/// Empty (or whitespace/comment-only) input yields an empty [`Stylesheet`].
///
/// Safety-relevant tokens are validated at parse time with the same
/// predicates the sanitized emitter uses, so an `Ok` result is guaranteed to
/// emit successfully through [`Stylesheet::emit`].
///
/// # Errors
///
/// Returns a [`CssError`] with a line/column diagnostic for malformed input
/// (unclosed blocks, stray `}`, unterminated comments, unsafe selectors,
/// properties, values, or media queries) and for inputs exceeding
/// [`MAX_INPUT_BYTES`], [`MAX_RULES`], or [`MAX_DECLARATIONS`].
pub fn parse_stylesheet(input: &str) -> Result<Stylesheet, CssError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(CssError::new(format!(
            "CSS input of {} bytes exceeds the maximum supported size of {MAX_INPUT_BYTES} bytes",
            input.len()
        )));
    }
    Cursor::new(input).parse()
}

struct Cursor<'a> {
    input: &'a str,
    pos: usize,
    rules: usize,
    declarations: usize,
}

impl<'a> Cursor<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            rules: 0,
            declarations: 0,
        }
    }

    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.rest().starts_with(prefix)
    }

    fn error(&self, message: impl AsRef<str>) -> CssError {
        let (line, column) = self.location();
        CssError::new(format!(
            "CSS parse error at line {line}, column {column}: {}",
            message.as_ref()
        ))
    }

    /// 1-based `(line, column)` of the current position.
    fn location(&self) -> (usize, usize) {
        let consumed = &self.input[..self.pos];
        match consumed.rfind('\n') {
            Some(last_newline) => (consumed.matches('\n').count() + 1, consumed.len() - last_newline),
            None => (1, consumed.len() + 1),
        }
    }

    /// Skips whitespace and `/* ... */` comments.
    fn skip_trivia(&mut self) -> Result<(), CssError> {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\n' | '\r' | '\u{000C}') => self.bump(),
                Some('/') if self.starts_with("/*") => {
                    self.read_comment()?;
                }
                _ => return Ok(()),
            }
        }
    }

    /// Consumes a `/* ... */` comment starting at the cursor and returns its
    /// inner text.
    fn read_comment(&mut self) -> Result<String, CssError> {
        self.pos += 2; // "/*" — guaranteed ASCII, so a char boundary
        let rest = &self.input[self.pos..];
        match rest.find("*/") {
            Some(end) => {
                let text = rest[..end].to_string();
                self.pos += end + 2;
                Ok(text)
            }
            None => Err(self.error("unterminated comment: missing '*/'")),
        }
    }

    /// Reads text until an ASCII character in `stop` is reached. The stop
    /// character is not consumed; comments are replaced by a single space.
    fn read_until_stop(&mut self, stop: &str, eof_message: &str) -> Result<String, CssError> {
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error(eof_message)),
                Some(c) if stop.contains(c) => return Ok(text),
                Some('/') if self.starts_with("/*") => {
                    self.read_comment()?;
                    text.push(' ');
                }
                Some(c) => {
                    self.bump();
                    text.push(c);
                }
            }
        }
    }

    /// Reads text up to and consumes the `{` opening a block.
    fn read_block_start(&mut self, what: &str) -> Result<String, CssError> {
        let mut text = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(self.error(format!(
                        "unclosed {what}: expected '{{' before end of input"
                    )))
                }
                Some('{') => {
                    self.bump();
                    return Ok(text);
                }
                Some('}') => {
                    return Err(self.error(format!(
                        "unexpected '}}' before the '{{' opening the {what}"
                    )))
                }
                Some('/') if self.starts_with("/*") => {
                    self.read_comment()?;
                    text.push(' ');
                }
                Some(c) => {
                    self.bump();
                    text.push(c);
                }
            }
        }
    }

    fn parse(&mut self) -> Result<Stylesheet, CssError> {
        let mut sheet = Stylesheet::new();
        let mut header_done = false;
        loop {
            // Whitespace only: comments are handled below so the first one
            // can be captured as the stylesheet header.
            self.skip_whitespace();
            match self.peek() {
                None => return Ok(sheet),
                Some('/') if self.starts_with("/*") => {
                    let text = self.read_comment()?;
                    if !header_done {
                        header_done = true;
                        validate_header(self, &text)?;
                        // Store the trimmed header exactly as the emitter
                        // writes it, so round-trips are byte-stable.
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            sheet.comment_header = Some(trimmed.to_string());
                        }
                    }
                }
                Some(';') => {
                    self.bump();
                    header_done = true;
                }
                Some('}') => {
                    return Err(self.error("unexpected '}' without a matching '{'"));
                }
                Some('@') => {
                    self.parse_at_rule(&mut sheet)?;
                    header_done = true;
                }
                Some(_) if self.at_root_block() => {
                    header_done = true;
                    self.parse_root_block(&mut sheet)?;
                }
                Some(_) => {
                    header_done = true;
                    self.parse_rule(&mut sheet, None)?;
                }
            }
        }
    }

    /// Skips whitespace (but not comments).
    fn skip_whitespace(&mut self) {
        while matches!(
            self.peek(),
            Some(' ' | '\t' | '\n' | '\r' | '\u{000C}')
        ) {
            self.bump();
        }
    }

    /// Whether the cursor sits at a `:root` block start rather than at a
    /// selector that merely begins with `:root` (e.g. `:root:hover`).
    fn at_root_block(&self) -> bool {
        if !self.starts_with(":root") {
            return false;
        }
        let Some(after) = self.input.get(self.pos + 5..) else {
            return false;
        };
        let Some(first) = after.chars().next() else {
            return false;
        };
        matches!(first, '{' | ' ' | '\t' | '\n' | '\r' | '\u{000C}' | '/')
    }

    fn parse_root_block(&mut self, sheet: &mut Stylesheet) -> Result<(), CssError> {
        self.pos += 5; // ":root" — guaranteed ASCII, so a char boundary
        self.skip_trivia()?;
        match self.peek() {
            Some('{') => self.bump(),
            None => {
                return Err(
                    self.error("unclosed :root block: expected '{' before end of input")
                )
            }
            Some(c) => {
                return Err(self.error(format!(
                    "expected '{{' to open the :root block, found {c:?}"
                )))
            }
        }
        let declarations = self.parse_declarations(true)?;
        for (prop, value) in declarations {
            // Duplicate custom properties resolve last-wins.
            sheet.root_vars.insert(prop, value);
        }
        Ok(())
    }

    fn parse_at_rule(&mut self, sheet: &mut Stylesheet) -> Result<(), CssError> {
        self.bump(); // '@'
        let mut keyword = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '-' {
                self.bump();
                keyword.push(c);
            } else {
                break;
            }
        }
        if keyword == "media" {
            self.parse_media_block(sheet)
        } else {
            Err(self.error(format!(
                "unsupported at-rule '@{keyword}': only '@media' blocks are supported"
            )))
        }
    }

    fn parse_media_block(&mut self, sheet: &mut Stylesheet) -> Result<(), CssError> {
        let raw = self.read_block_start("@media condition")?;
        let mut query = raw.trim().to_string();
        // Match the emitter's normalize_media_query: a redundant "@media"
        // prefix on the condition is tolerated and stripped.
        if let Some(rest) = query.strip_prefix("@media") {
            query = rest.trim().to_string();
        }
        if query.is_empty() {
            return Err(self.error("empty @media condition"));
        }
        if !is_safe_media_query(&query) {
            return Err(self.error(format!("unsafe media query: {query}")));
        }
        // The breakpoint ID is the query text itself, so identical queries
        // deterministically share one breakpoint across blocks.
        let id = query.clone();
        sheet.media_queries.entry(id.clone()).or_insert(query);
        loop {
            self.skip_trivia()?;
            match self.peek() {
                None => {
                    return Err(self.error(
                        "unclosed @media block: missing '}' at end of input",
                    ))
                }
                Some('}') => {
                    self.bump();
                    return Ok(());
                }
                Some('{') => {
                    return Err(self.error("unexpected '{' inside @media block"))
                }
                Some('@') => {
                    return Err(self.error(
                        "at-rules are not supported inside @media blocks",
                    ))
                }
                Some(';') => self.bump(),
                Some(_) => self.parse_rule(sheet, Some(id.as_str()))?,
            }
        }
    }

    fn parse_rule(
        &mut self,
        sheet: &mut Stylesheet,
        breakpoint: Option<&str>,
    ) -> Result<(), CssError> {
        let raw = self.read_block_start("rule block")?;
        let selector = raw.trim();
        if selector.is_empty() {
            return Err(self.error("empty rule selector"));
        }
        if !is_safe_selector(selector) {
            return Err(self.error(format!("unsafe CSS selector: {selector}")));
        }
        let declarations = self.parse_declarations(false)?;
        self.rules += 1;
        if self.rules > MAX_RULES {
            return Err(self.error(format!(
                "stylesheet exceeds the maximum of {MAX_RULES} rules"
            )));
        }
        sheet.rules.push(CssRule {
            selector: selector.to_string(),
            breakpoint: breakpoint.map(str::to_string),
            pseudo: None,
            declarations,
        });
        Ok(())
    }

    fn parse_declarations(
        &mut self,
        root: bool,
    ) -> Result<BTreeMap<String, String>, CssError> {
        let mut declarations = BTreeMap::new();
        loop {
            self.skip_trivia()?;
            match self.peek() {
                None => {
                    return Err(self.error(
                        "unclosed declaration block: missing '}' at end of input",
                    ))
                }
                Some('}') => {
                    self.bump();
                    return Ok(declarations);
                }
                Some('{') => {
                    return Err(self.error("unexpected '{' inside declaration block"))
                }
                Some('@') => {
                    return Err(self.error(
                        "at-rules are not supported inside declaration blocks",
                    ))
                }
                Some(';') => self.bump(),
                Some(_) => {
                    let (prop, value) = self.parse_declaration(root)?;
                    // Duplicate declarations resolve last-wins.
                    declarations.insert(prop, value);
                }
            }
        }
    }

    fn parse_declaration(&mut self, root: bool) -> Result<(String, String), CssError> {
        let unclosed = "unclosed declaration block: missing '}' at end of input";
        let raw_prop = self.read_until_stop(":;{}", unclosed)?;
        let prop = raw_prop.trim().to_string();
        if prop.is_empty() {
            return Err(self.error("empty property name in declaration"));
        }
        match self.peek() {
            Some(':') => self.bump(),
            Some(';') | Some('}') => {
                return Err(self.error(format!(
                    "expected ':' after property name '{prop}'"
                )));
            }
            Some('{') => {
                return Err(self.error("unexpected '{' inside declaration block"))
            }
            Some(_) | None => return Err(self.error(unclosed)),
        }
        let raw_value = self.read_until_stop(";}{", unclosed)?;
        let value = raw_value.trim().to_string();
        match self.peek() {
            Some(';') => self.bump(),
            // The declaration loop consumes the closing brace.
            Some('}') => {}
            Some('{') => {
                return Err(self.error("unexpected '{' inside declaration value"))
            }
            Some(_) | None => return Err(self.error(unclosed)),
        }

        if root && !prop.starts_with("--") {
            return Err(self.error(format!(
                "declarations in a :root block must be custom properties starting with '--', found '{prop}'"
            )));
        }
        if !is_safe_property(&prop) {
            return Err(self.error(format!("unsafe CSS property: {prop}")));
        }
        if value.is_empty() {
            return Err(self.error(format!("empty value for property '{prop}'")));
        }
        if !is_safe_css_value(&value) {
            return Err(self.error(format!(
                "unsafe CSS value for property '{prop}': {value}"
            )));
        }
        self.declarations += 1;
        if self.declarations > MAX_DECLARATIONS {
            return Err(self.error(format!(
                "stylesheet exceeds the maximum of {MAX_DECLARATIONS} declarations"
            )));
        }
        Ok((prop, value))
    }
}

/// Validates a captured comment header with the same checks the emitter
/// applies to [`Stylesheet::comment_header`].
fn validate_header(cursor: &Cursor<'_>, header: &str) -> Result<(), CssError> {
    let trimmed = header.trim();
    if trimmed.contains("/*")
        || trimmed.contains("*/")
        || trimmed.contains('\0')
        || trimmed.to_ascii_lowercase().contains("</style")
    {
        return Err(cursor.error(
            "comment header contains unsafe CSS comment breakout tokens",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(input: &str) -> Stylesheet {
        let sheet = parse_stylesheet(input).expect("parse should succeed");
        // Model invariant: anything parsed must emit through the sanitized
        // emitter without error.
        sheet.emit().expect("parsed stylesheet must emit cleanly");
        sheet
    }

    fn parse_err(input: &str) -> String {
        parse_stylesheet(input)
            .expect_err("parse should fail")
            .to_string()
    }

    #[test]
    fn parses_root_vars_rules_media_comments_and_header() {
        let sheet = parse_ok(
            "/* Generated by SWS. Do not edit. */\n\
             :root {\n\
             \x20 --bg: #E9E2D2;\n\
             \x20 --text: #241207;\n\
             }\n\n\
             /* palette; skipped */\n\
             body {\n\
             \x20 color: var(--text);\n\
             }\n\
             @media (min-width: 40rem) {\n\
             \x20 .grid { grid-template-columns: 1fr 1fr; }\n\
             }\n",
        );
        assert_eq!(
            sheet.comment_header.as_deref(),
            Some("Generated by SWS. Do not edit.")
        );
        assert_eq!(sheet.root_vars.get("--bg").map(String::as_str), Some("#E9E2D2"));
        assert_eq!(sheet.root_vars.get("--text").map(String::as_str), Some("#241207"));
        assert_eq!(sheet.media_queries.len(), 1);
        assert_eq!(
            sheet.media_queries.get("(min-width: 40rem)").map(String::as_str),
            Some("(min-width: 40rem)")
        );
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].selector, "body");
        assert_eq!(sheet.rules[0].breakpoint, None);
        assert_eq!(sheet.rules[0].pseudo, None);
        assert_eq!(
            sheet.rules[1].breakpoint.as_deref(),
            Some("(min-width: 40rem)")
        );
        assert_eq!(
            sheet.emit().expect("emit"),
            "/* Generated by SWS. Do not edit. */\n\
             :root {\n\
             \x20 --bg: #E9E2D2;\n\
             \x20 --text: #241207;\n\
             }\n\
             body { color: var(--text); }\n\
             @media (min-width: 40rem) {\n\
             \x20 .grid { grid-template-columns: 1fr 1fr; }\n\
             }\n"
        );
    }

    #[test]
    fn empty_and_whitespace_only_input_yields_empty_stylesheet() {
        for input in ["", "   ", "\n\t\r  ", "/* */", "   /* */  "] {
            let sheet = parse_ok(input);
            assert_eq!(sheet, Stylesheet::new(), "input: {input:?}");
            assert_eq!(sheet.emit().expect("emit"), "");
        }
        // A non-empty comment is the first content, so it becomes the header.
        let sheet = parse_ok("/* only a comment */");
        assert_eq!(sheet.comment_header.as_deref(), Some("only a comment"));
        assert!(sheet.rules.is_empty() && sheet.root_vars.is_empty());
    }

    #[test]
    fn round_trip_is_byte_stable() {
        let fixtures = [
            "",
            "/* Generated by SWS. Do not edit. */\n:root {\n  --bg: #E9E2D2;\n  --text: #241207;\n}\nbody { background: var(--bg); color: var(--text); }\n",
            "body { color: #241207; }",
            ":root { --bg: red; }",
            "a, button { text-decoration-thickness: .08em; text-underline-offset: .18em; }",
            "a:hover { text-decoration-thickness: .16em; }\n.skip-link:focus-visible { outline: 3px solid var(--bg); }",
            "a { color: red; }\na { margin: 0; }",
            "@media (min-width: 40rem) {\n  .grid { grid-template-columns: 1fr 1fr; }\n}\n@media (prefers-reduced-motion: reduce) {\n  *, *::before, *::after { animation: none !important; }\n}",
            ":root {\n  --x: 1;\n}\n\n\n\tbody\t{\n\t\tmargin:\t0 auto;\n\t}\n",
        ];
        for css in fixtures {
            let sheet1 = parse_ok(css);
            let out1 = sheet1.emit().expect("first emit");
            let sheet2 = parse_ok(&out1);
            let out2 = sheet2.emit().expect("second emit");
            assert_eq!(out1, out2, "round-trip drifted for input {css:?}");
        }
    }

    #[test]
    fn captures_leading_comment_as_header_and_skips_later_comments() {
        let sheet = parse_ok("/* header */ /* mid */ body { color: red; } /* tail */");
        assert_eq!(sheet.comment_header.as_deref(), Some("header"));
        let out = sheet.emit().expect("emit");
        assert_eq!(out, "/* header */\nbody { color: red; }\n");
        // Re-parsing the emitted output keeps the header stable.
        let reparsed = parse_ok(&out);
        assert_eq!(reparsed.comment_header.as_deref(), Some("header"));
    }

    #[test]
    fn duplicate_vars_and_declarations_resolve_last_wins() {
        let sheet = parse_ok(":root { --bg: red; --bg: blue; } :root { --bg: green; }");
        assert_eq!(sheet.root_vars.get("--bg").map(String::as_str), Some("green"));

        let sheet = parse_ok("body { color: red; color: blue; } body { color: teal; }");
        assert_eq!(sheet.rules[0].declarations.get("color").map(String::as_str), Some("blue"));
        assert_eq!(sheet.rules[1].declarations.get("color").map(String::as_str), Some("teal"));
    }

    #[test]
    fn same_media_query_shares_one_breakpoint() {
        let sheet = parse_ok(
            "@media (min-width: 40rem) { .a { color: red; } }\n\
             @media (min-width: 40rem) { .a { margin: 0; } }\n",
        );
        assert_eq!(sheet.media_queries.len(), 1);
        assert_eq!(sheet.rules.len(), 2);
        let out = sheet.emit().expect("emit");
        assert!(out.contains("@media (min-width: 40rem) {\n  .a { color: red; margin: 0; }\n}"));
    }

    #[test]
    fn unusual_but_safe_whitespace_is_tolerated() {
        let sheet = parse_ok(
            "\t\r\n :root\r{\t--bg\t:\t#FFF\t;\t}\r\nbody\n{\ncolor\t:\n\tvar(--bg)\n;\n}\n",
        );
        assert_eq!(sheet.root_vars.get("--bg").map(String::as_str), Some("#FFF"));
        assert_eq!(sheet.rules[0].selector, "body");
        assert_eq!(sheet.rules[0].declarations.get("color").map(String::as_str), Some("var(--bg)"));
    }

    #[test]
    fn malformed_input_yields_diagnostics_not_panics() {
        let cases = [
            ("body { color: red", "unclosed declaration block"),
            ("body color: red }", "unexpected '}'"),
            ("}", "unexpected '}'"),
            ("}}}}", "unexpected '}'"),
            ("body { color }", "expected ':' after property name"),
            ("body { color: }", "empty value for property 'color'"),
            ("@media (min-width: 40rem) { .a { color: red }", "unclosed @media block"),
            ("@media { .a { color: red } }", "empty @media condition"),
            (
                "@media (min-width: 40rem) { @media (x) { .a { color: red } } }",
                "at-rules are not supported",
            ),
            ("body { @media (x) { } color: red }", "at-rules are not supported"),
            ("/* unterminated", "unterminated comment"),
            ("a { b: c { } }", "unexpected '{' inside declaration value"),
        ];
        for (input, expected) in cases {
            let err = parse_err(input);
            assert!(
                err.contains(expected),
                "diagnostic for {input:?} should contain {expected:?}, got: {err}"
            );
            assert!(
                err.contains("line "),
                "diagnostic for {input:?} should carry a position, got: {err}"
            );
        }
    }

    #[test]
    fn unsupported_at_rules_are_rejected() {
        for input in ["@import \"x.css\";", "@ { }", "@font-face { font-family: x; }"] {
            let err = parse_err(input);
            assert!(err.contains("unsupported at-rule"), "input: {input:?}: {err}");
        }
    }

    #[test]
    fn unsafe_tokens_are_rejected_at_parse_time() {
        let cases = [
            ("body { color: url(https://evil.example/track.png); }", "unsafe CSS value"),
            ("body { background: expression(alert(1)); }", "unsafe CSS value"),
            ("body { color: \"unclosed; }", "unsafe CSS value"),
            ("body\n.x { color: red; }", "unsafe CSS selector"),
            ("body { color;x: red; }", "CSS parse error"),
            (":root { font-size: 16px; }", "must be custom properties"),
            (":root { --: red; }", "unsafe CSS property"),
            ("/* a /* b */ body { color: red; }", "comment header contains unsafe"),
            ("body { color: red </style> }", "unsafe CSS value"),
        ];
        for (input, expected) in cases {
            let err = parse_err(input);
            assert!(
                err.contains(expected),
                "diagnostic for {input:?} should contain {expected:?}, got: {err}"
            );
        }
    }

    #[test]
    fn oversized_input_is_rejected() {
        let huge = "a".repeat(MAX_INPUT_BYTES + 1);
        let err = parse_err(&huge);
        assert!(err.contains("exceeds the maximum supported size"), "{err}");
    }

    #[test]
    fn rule_and_declaration_caps_are_enforced() {
        let many_rules: String = (0..=MAX_RULES)
            .map(|i| format!(".c{i} {{ color: red; }}"))
            .collect();
        let err = parse_err(&many_rules);
        assert!(err.contains("maximum of 10000 rules"), "{err}");

        // Kept under MAX_INPUT_BYTES so the declaration cap is what trips.
        let many_decls: String = (0..=MAX_DECLARATIONS)
            .map(|i| format!("x{i}:1;"))
            .collect();
        let err = parse_err(&format!("body {{ {many_decls} }}"));
        assert!(err.contains("maximum of 100000 declarations"), "{err}");
    }

    #[test]
    fn adversarial_input_terminates_without_panicking() {
        // The parser is fully iterative: nested-looking or pathological input
        // must terminate with an Ok or a diagnostic, never recurse or panic.
        let adversarial = [
            "{".repeat(50_000),
            "}".repeat(50_000),
            "a{".repeat(50_000),
            "a { b: c".repeat(50_000),
            "*".repeat(400_000),
            "/*x*/".repeat(200_000),
            "@media (a) { ".repeat(60_000),
            format!("body {{ color: {} }}", "x".repeat(MAX_INPUT_BYTES)),
        ];
        for input in adversarial {
            // Also feeds the result through emit when parsing succeeds, to
            // double-check the no-unsafe-output invariant on odd input.
            if let Ok(sheet) = parse_stylesheet(&input) {
                let _ = sheet.emit();
            }
        }
    }

    #[test]
    fn parsed_stylesheets_always_emit() {
        let accepted = [
            "body { color: red; }",
            ":root { --a: 1; --b: 2; }",
            "a:hover { color: blue; }",
            "@media (min-width: 40rem) { .a { color: red; } }",
            "body { margin: 0; padding: 0; border: none; }",
        ];
        for input in accepted {
            let sheet = parse_stylesheet(input).expect("parse should succeed");
            sheet.emit().expect("parsed stylesheet must emit");
        }
    }
}
