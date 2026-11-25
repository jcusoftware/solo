//! Typed CSS AST, design-token emitter, deterministic CSS-text parser,
//! injection guards, and WCAG 2.x contrast utilities for Solo Website System (SWS).

pub mod ast;
pub mod contrast;
pub mod parser;
pub mod sanitize;

pub use ast::{CssError, CssRule, Stylesheet};
pub use contrast::{
    contrast_ratio, is_valid_hex_color, meets_wcag_aa_large, meets_wcag_aa_normal,
    parse_hex_color, relative_luminance,
};
pub use parser::{parse_stylesheet, MAX_DECLARATIONS, MAX_INPUT_BYTES, MAX_RULES};
pub use sanitize::{
    has_balanced_quotes, is_safe_css_value, is_safe_css_value_with_url, is_safe_font_stack,
    is_safe_property, is_safe_selector,
};
