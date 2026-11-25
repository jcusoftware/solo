//! Core type definitions for the Solo Website System (SWS) Intermediate Representation.
//!
//! All associative maps use [`std::collections::BTreeMap`] so serialization and
//! compilation remain deterministic across platforms and targets.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const fn default_schema_version() -> u32 {
    1
}

fn default_locale() -> String {
    "en-US".to_string()
}

fn default_form_method() -> String {
    "POST".to_string()
}

/// Complete, domain-agnostic intermediate representation of a static/edge website.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteDocument {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub slug: String,
    pub name: String,
    #[serde(default = "default_locale")]
    pub locale: String,
    pub base_url: String,
    #[serde(default)]
    pub tokens: DesignTokens,
    #[serde(default)]
    pub breakpoints: Vec<Breakpoint>,
    #[serde(default)]
    pub styles: Vec<IrStyleRule>,
    #[serde(default)]
    pub components: BTreeMap<String, ComponentDef>,
    #[serde(default)]
    pub forms: BTreeMap<String, FormSchema>,
    #[serde(default)]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub seo: SeoMeta,
    #[serde(default)]
    pub scripts: Vec<ScriptRef>,
    #[serde(default)]
    pub data: BTreeMap<String, Value>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl Default for SiteDocument {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            slug: String::new(),
            name: String::new(),
            locale: default_locale(),
            base_url: String::new(),
            tokens: DesignTokens::default(),
            breakpoints: Vec::new(),
            styles: Vec::new(),
            components: BTreeMap::new(),
            forms: BTreeMap::new(),
            pages: Vec::new(),
            seo: SeoMeta::default(),
            scripts: Vec::new(),
            data: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }
}

/// Design system tokens for colors, typography, spacing, radii, and contrast checks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesignTokens {
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub fonts: FontTokens,
    #[serde(default)]
    pub spacing: BTreeMap<String, String>,
    #[serde(default)]
    pub radii: BTreeMap<String, String>,
    #[serde(default)]
    pub contrast_pairs: Vec<ContrastPair>,
}

/// Font stack tokens for body, heading, and monospace text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontTokens {
    pub body: String,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub mono: Option<String>,
}

impl Default for FontTokens {
    fn default() -> Self {
        Self {
            body: "system-ui, sans-serif".to_string(),
            heading: None,
            mono: None,
        }
    }
}

/// Declared foreground/background color token pair with minimum WCAG contrast ratio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContrastPair {
    pub foreground: String,
    pub background: String,
    pub min_ratio: f64,
    #[serde(default)]
    pub usage: Option<String>,
}

/// Responsive media-query breakpoint definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breakpoint {
    pub id: String,
    pub media_query: String,
    #[serde(default)]
    pub min_width: Option<String>,
}

/// Deterministic CSS rule emitted by the IR compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrStyleRule {
    pub id: String,
    pub selector: String,
    #[serde(default)]
    pub breakpoint: Option<String>,
    #[serde(default)]
    pub pseudo: Option<String>,
    #[serde(default)]
    pub declarations: BTreeMap<String, String>,
}

/// Reusable template component definition with typed props and named slots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentDef {
    pub name: String,
    #[serde(default)]
    pub props: BTreeMap<String, PropSpec>,
    #[serde(default)]
    pub slots: Vec<String>,
    #[serde(default)]
    pub template: Vec<Node>,
}

/// Specification for a single component prop.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropSpec {
    #[serde(default)]
    pub kind: PropKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Supported value types for component props.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropKind {
    #[default]
    String,
    Number,
    Boolean,
    Array,
    Object,
    Json,
}

/// Progressive-enhancement form definition bound to Turnstile and edge handlers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormSchema {
    pub id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default = "default_form_method")]
    pub method: String,
    #[serde(default)]
    pub turnstile_action: String,
    #[serde(default)]
    pub fields: Vec<FormFieldSchema>,
}

/// Field specification within a [`FormSchema`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormFieldSchema {
    pub name: String,
    #[serde(default)]
    pub label: String,
    pub kind: FormFieldKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub help_text: Option<String>,
    #[serde(default)]
    pub default_value: Option<String>,
    #[serde(default)]
    pub options: Vec<FormFieldOption>,
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    #[serde(default)]
    pub autocomplete: Option<String>,
}

/// Supported form field control types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormFieldKind {
    Text,
    Textarea,
    Email,
    Phone,
    Date,
    Integer,
    Select,
    Checkbox,
    Hidden,
    Route,
}

/// Selectable option for [`FormFieldKind::Select`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormFieldOption {
    pub value: String,
    pub label: String,
}

/// Individual static or parameterized page in a [`SiteDocument`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: String,
    pub route: RoutePattern,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub canonical: Option<String>,
    #[serde(default)]
    pub noindex: bool,
    #[serde(default)]
    pub is_not_found: bool,
    #[serde(default)]
    pub json_ld: Vec<JsonLdBlock>,
    #[serde(default)]
    pub scripts: Vec<ScriptRef>,
    #[serde(default)]
    pub body: Vec<Node>,
}

/// Route pattern for a [`Page`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutePattern {
    Static {
        path: String,
    },
    Dynamic {
        pattern: String,
        #[serde(default)]
        params: Vec<String>,
    },
}

/// Structured JSON-LD script block attached to a [`Page`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonLdBlock {
    #[serde(default)]
    pub id: Option<String>,
    pub data: Value,
}

/// Site-wide SEO, robots.txt, and sitemap configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeoMeta {
    #[serde(default)]
    pub title_template: Option<String>,
    #[serde(default)]
    pub default_description: Option<String>,
    #[serde(default)]
    pub robots_allow: Vec<String>,
    #[serde(default)]
    pub robots_disallow: Vec<String>,
    #[serde(default)]
    pub sitemap_routes: Vec<String>,
}

/// External or bundled script reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptRef {
    pub src: String,
    #[serde(default)]
    pub async_load: bool,
    #[serde(default)]
    pub defer: bool,
}

/// Renderable DOM/template node inside a [`Page`] or [`ComponentDef`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Element {
        #[serde(default)]
        id: Option<String>,
        tag: String,
        #[serde(default)]
        attrs: BTreeMap<String, String>,
        #[serde(default)]
        classes: Vec<String>,
        #[serde(default)]
        style_refs: Vec<String>,
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        value: String,
    },
    SlotRef {
        name: String,
        #[serde(default)]
        fallback: Vec<Node>,
    },
    ComponentInstance {
        component: String,
        #[serde(default)]
        props: BTreeMap<String, Value>,
        #[serde(default)]
        slots: BTreeMap<String, Vec<Node>>,
    },
    Conditional {
        expr: String,
        #[serde(default)]
        then_nodes: Vec<Node>,
        #[serde(default)]
        else_nodes: Vec<Node>,
    },
    Loop {
        item_var: String,
        collection_expr: String,
        #[serde(default)]
        body: Vec<Node>,
    },
}
