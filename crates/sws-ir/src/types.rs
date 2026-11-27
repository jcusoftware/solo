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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "jsonschema", schemars(extend("required" = ["slug", "name", "base_url", "pages"])))]
#[cfg_attr(
    feature = "jsonschema",
    schemars(
        description = "Deterministic intermediate representation for a Solo Website System (SWS) site."
    )
)]
pub struct SiteDocument {
    #[serde(default = "default_schema_version")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("minimum" = 1)))]
    pub schema_version: u32,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9]+(?:-[a-z0-9]+)*$")))]
    pub slug: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub name: String,
    #[serde(default = "default_locale")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub locale: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("format" = "uri", "pattern" = "^https://[^\\s/]+(/.*)?$")))]
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
    #[cfg_attr(feature = "jsonschema", schemars(extend("minItems" = 1)))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct DesignTokens {
    #[serde(default)]
    #[cfg_attr(feature = "jsonschema", schemars(extend("additionalProperties" = {"type": "string", "pattern": "^#[0-9a-fA-F]{6}$"})))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct FontTokens {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ContrastPair {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub foreground: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub background: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("exclusiveMinimum" = 0)))]
    pub min_ratio: f64,
    #[serde(default)]
    pub usage: Option<String>,
}

/// Responsive media-query breakpoint definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Breakpoint {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub id: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub media_query: String,
    #[serde(default)]
    pub min_width: Option<String>,
}

/// Deterministic CSS rule emitted by the IR compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct IrStyleRule {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub id: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ComponentDef {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct FormSchema {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct FormFieldSchema {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct FormFieldOption {
    pub value: String,
    pub label: String,
}

/// Individual static or parameterized page in a [`SiteDocument`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Page {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutePattern {
    Static {
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^/")))]
        path: String,
    },
    #[cfg_attr(feature = "jsonschema", schemars(extend("required" = ["kind", "pattern", "params"])))]
    Dynamic {
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^/")))]
        pattern: String,
        #[serde(default)]
        #[cfg_attr(feature = "jsonschema", schemars(extend("minItems" = 1, "items" = {"type": "string", "minLength": 1})))]
        params: Vec<String>,
    },
}

/// Structured JSON-LD script block attached to a [`Page`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct JsonLdBlock {
    #[serde(default)]
    pub id: Option<String>,
    pub data: Value,
}

/// Site-wide SEO, robots.txt, and sitemap configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ScriptRef {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub src: String,
    #[serde(default)]
    pub async_load: bool,
    #[serde(default)]
    pub defer: bool,
}

/// Renderable DOM/template node inside a [`Page`] or [`ComponentDef`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Element {
        /// Stable editor identity; never rendered, unique across the document.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9][a-z0-9._-]{0,63}$")))]
        uid: Option<String>,
        #[serde(default)]
        id: Option<String>,
        #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
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
        /// Stable editor identity; never rendered, unique across the document.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9][a-z0-9._-]{0,63}$")))]
        uid: Option<String>,
        value: String,
    },
    SlotRef {
        /// Stable editor identity; never rendered, unique across the document.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9][a-z0-9._-]{0,63}$")))]
        uid: Option<String>,
        #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
        name: String,
        #[serde(default)]
        fallback: Vec<Node>,
    },
    ComponentInstance {
        /// Stable editor identity; never rendered, unique across the document.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9][a-z0-9._-]{0,63}$")))]
        uid: Option<String>,
        #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
        component: String,
        #[serde(default)]
        props: BTreeMap<String, Value>,
        #[serde(default)]
        slots: BTreeMap<String, Vec<Node>>,
    },
    Conditional {
        /// Stable editor identity; never rendered, unique across the document.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9][a-z0-9._-]{0,63}$")))]
        uid: Option<String>,
        #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
        expr: String,
        #[serde(default)]
        then_nodes: Vec<Node>,
        #[serde(default)]
        else_nodes: Vec<Node>,
    },
    Loop {
        /// Stable editor identity; never rendered, unique across the document.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9][a-z0-9._-]{0,63}$")))]
        uid: Option<String>,
        #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
        item_var: String,
        #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
        collection_expr: String,
        #[serde(default)]
        body: Vec<Node>,
    },
}
