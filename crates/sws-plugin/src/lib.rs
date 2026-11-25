//! Solo Website System (SWS) Plugin ABI and deterministic registry.
//!
//! Vertical packs and community extensions implement [`SwsPlugin`] and register
//! with [`PluginRegistry`] using `BTreeMap` storage for deterministic iteration.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Severity level for a diagnostic emitted by a [`SwsPlugin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Advisory,
}

/// Structured lint/validation diagnostic returned by [`SwsPlugin::lint`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDiagnostic {
    pub plugin_id: String,
    pub rule: String,
    pub severity: DiagnosticSeverity,
    pub path: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation: Option<String>,
}

/// Reusable component contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginComponent {
    pub name: String,
    pub description: String,
    pub definition: sws_ir::ComponentDef,
}

/// Form schema contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginFormSchema {
    pub id: String,
    pub schema: sws_ir::FormSchema,
}

/// Error returned by plugin operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    pub plugin_id: String,
    pub message: String,
}

impl PluginError {
    #[must_use]
    pub fn new(plugin_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.plugin_id, self.message)
    }
}

impl std::error::Error for PluginError {}

/// Domain-agnostic extension trait for SWS input adapters, IR transforms,
/// runtime/build data resolvers, components, form schemas, and linters.
pub trait SwsPlugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;

    fn components(&self) -> Vec<PluginComponent> {
        Vec::new()
    }

    fn form_schemas(&self) -> Vec<PluginFormSchema> {
        Vec::new()
    }

    fn can_transform_input(&self, _input: &Value) -> bool {
        false
    }

    fn transform_input(&self, _input: &Value) -> Result<sws_ir::SiteDocument, PluginError> {
        Err(PluginError::new(
            self.id(),
            "input transformation is not supported by this plugin",
        ))
    }

    fn transform_ir(&self, _doc: &mut sws_ir::SiteDocument) -> Result<(), PluginError> {
        Ok(())
    }

    fn resolve_data(
        &self,
        _doc: &sws_ir::SiteDocument,
        _route: &str,
    ) -> Result<BTreeMap<String, Value>, PluginError> {
        Ok(BTreeMap::new())
    }

    fn lint(&self, _doc: &sws_ir::SiteDocument, _raw_input: Option<&Value>) -> Vec<PluginDiagnostic> {
        Vec::new()
    }
}

/// Deterministic registry of [`SwsPlugin`] instances keyed by plugin ID.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, Box<dyn SwsPlugin>>,
}

impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<&str> = self.plugins.keys().map(String::as_str).collect();
        f.debug_struct("PluginRegistry")
            .field("plugins", &ids)
            .finish()
    }
}

impl PluginRegistry {
    /// Create an empty [`PluginRegistry`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin, replacing any existing plugin with the same `id()`.
    pub fn register(&mut self, plugin: Box<dyn SwsPlugin>) {
        self.plugins.insert(plugin.id().to_string(), plugin);
    }

    /// Look up a registered plugin by its identifier.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&dyn SwsPlugin> {
        self.plugins.get(id).map(Box::as_ref)
    }

    /// Return all registered plugins in deterministic ID order.
    #[must_use]
    pub fn list(&self) -> Vec<&dyn SwsPlugin> {
        self.plugins.values().map(Box::as_ref).collect()
    }

    /// Transform raw JSON input into a [`sws_ir::SiteDocument`] using either an
    /// explicit `plugin_id` or the first plugin returning `can_transform_input(input) == true`.
    pub fn transform_input_with(
        &self,
        plugin_id: Option<&str>,
        input: &Value,
    ) -> Result<sws_ir::SiteDocument, PluginError> {
        match plugin_id {
            Some(id) => {
                let plugin = self
                    .get(id)
                    .ok_or_else(|| PluginError::new(id, "plugin is not registered"))?;
                plugin.transform_input(input)
            }
            None => {
                let plugin = self
                    .plugins
                    .values()
                    .find(|p| p.can_transform_input(input))
                    .ok_or_else(|| {
                        PluginError::new(
                            "registry",
                            "no registered plugin can transform the supplied input",
                        )
                    })?;
                plugin.transform_input(input)
            }
        }
    }

    /// Run `transform_ir` across all registered plugins in deterministic ID order.
    pub fn apply_transforms(&self, doc: &mut sws_ir::SiteDocument) -> Result<(), PluginError> {
        for plugin in self.plugins.values() {
            plugin.transform_ir(doc)?;
        }
        Ok(())
    }

    /// Collect lint diagnostics from all registered plugins in deterministic ID order.
    #[must_use]
    pub fn collect_diagnostics(
        &self,
        doc: &sws_ir::SiteDocument,
        raw_input: Option<&Value>,
    ) -> Vec<PluginDiagnostic> {
        let mut diagnostics = Vec::new();
        for plugin in self.plugins.values() {
            diagnostics.extend(plugin.lint(doc, raw_input));
        }
        diagnostics
    }

    /// Collect all components contributed by registered plugins in deterministic ID order.
    #[must_use]
    pub fn all_components(&self) -> Vec<PluginComponent> {
        let mut components = Vec::new();
        for plugin in self.plugins.values() {
            components.extend(plugin.components());
        }
        components
    }

    /// Collect all form schemas contributed by registered plugins in deterministic ID order.
    #[must_use]
    pub fn all_form_schemas(&self) -> Vec<PluginFormSchema> {
        let mut schemas = Vec::new();
        for plugin in self.plugins.values() {
            schemas.extend(plugin.form_schemas());
        }
        schemas
    }

    /// Resolve and merge route data from all registered plugins in deterministic ID order.
    pub fn resolve_all_data(
        &self,
        doc: &sws_ir::SiteDocument,
        route: &str,
    ) -> Result<BTreeMap<String, Value>, PluginError> {
        let mut merged = BTreeMap::new();
        for plugin in self.plugins.values() {
            merged.extend(plugin.resolve_data(doc, route)?);
        }
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sws_ir::{
        ComponentDef, FormFieldKind, FormFieldSchema, FormSchema, Node, Page, PropKind, PropSpec,
        RoutePattern, SiteDocument,
    };

    use super::*;

    struct MockCatalogPlugin;

    impl SwsPlugin for MockCatalogPlugin {
        fn id(&self) -> &'static str {
            "mock-catalog"
        }

        fn name(&self) -> &'static str {
            "Mock Catalog Pack"
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }

        fn components(&self) -> Vec<PluginComponent> {
            vec![PluginComponent {
                name: "catalog-grid".to_string(),
                description: "Displays catalog entries".to_string(),
                definition: ComponentDef {
                    name: "catalog-grid".to_string(),
                    props: BTreeMap::from([(
                        "title".to_string(),
                        PropSpec {
                            kind: PropKind::String,
                            required: true,
                            default: None,
                            description: None,
                        },
                    )]),
                    slots: Vec::new(),
                    template: vec![Node::Element {
                        id: None,
                        tag: "div".to_string(),
                        attrs: BTreeMap::new(),
                        classes: vec!["catalog-grid".to_string()],
                        style_refs: Vec::new(),
                        children: Vec::new(),
                    }],
                },
            }]
        }

        fn form_schemas(&self) -> Vec<PluginFormSchema> {
            vec![PluginFormSchema {
                id: "catalog-request".to_string(),
                schema: FormSchema {
                    id: "catalog-request".to_string(),
                    action: "/api/inquire".to_string(),
                    method: "POST".to_string(),
                    turnstile_action: "inquire".to_string(),
                    fields: vec![FormFieldSchema {
                        name: "email".to_string(),
                        label: "Email".to_string(),
                        kind: FormFieldKind::Email,
                        required: true,
                        placeholder: None,
                        help_text: None,
                        default_value: None,
                        options: Vec::new(),
                        max_chars: Some(254),
                        min: None,
                        max: None,
                        autocomplete: Some("email".to_string()),
                    }],
                },
            }]
        }

        fn can_transform_input(&self, input: &Value) -> bool {
            input.get("kind").and_then(Value::as_str) == Some("mock_catalog")
        }

        fn transform_input(&self, input: &Value) -> Result<SiteDocument, PluginError> {
            let slug = input
                .get("slug")
                .and_then(Value::as_str)
                .ok_or_else(|| PluginError::new(self.id(), "missing slug"))?;
            let name = input
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Mock Site");
            Ok(SiteDocument {
                slug: slug.to_string(),
                name: name.to_string(),
                base_url: format!("https://{slug}.example"),
                pages: vec![Page {
                    id: "home".to_string(),
                    route: RoutePattern::Static {
                        path: "/".to_string(),
                    },
                    title: name.to_string(),
                    description: None,
                    canonical: None,
                    noindex: false,
                    is_not_found: false,
                    json_ld: Vec::new(),
                    scripts: Vec::new(),
                    body: Vec::new(),
                }],
                ..SiteDocument::default()
            })
        }

        fn transform_ir(&self, doc: &mut SiteDocument) -> Result<(), PluginError> {
            doc.metadata
                .insert("enriched_by".to_string(), json!(self.id()));
            Ok(())
        }

        fn resolve_data(
            &self,
            _doc: &SiteDocument,
            route: &str,
        ) -> Result<BTreeMap<String, Value>, PluginError> {
            let mut data = BTreeMap::new();
            data.insert("active_route".to_string(), json!(route));
            Ok(data)
        }

        fn lint(&self, doc: &SiteDocument, raw_input: Option<&Value>) -> Vec<PluginDiagnostic> {
            let mut out = Vec::new();
            if doc.seo.default_description.is_none() {
                out.push(PluginDiagnostic {
                    plugin_id: self.id().to_string(),
                    rule: "seo-default-description".to_string(),
                    severity: DiagnosticSeverity::Advisory,
                    path: "seo.default_description".to_string(),
                    message: "Consider setting seo.default_description".to_string(),
                    citation: Some("SWS-SEO-01".to_string()),
                });
            }
            if raw_input.and_then(|v| v.get("flagged")).is_some() {
                out.push(PluginDiagnostic {
                    plugin_id: self.id().to_string(),
                    rule: "flagged-input".to_string(),
                    severity: DiagnosticSeverity::Warning,
                    path: "flagged".to_string(),
                    message: "Input contained flagged marker".to_string(),
                    citation: None,
                });
            }
            out
        }
    }

    struct StubPlugin;

    impl SwsPlugin for StubPlugin {
        fn id(&self) -> &'static str {
            "a-stub"
        }

        fn name(&self) -> &'static str {
            "Stub Plugin"
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }
    }

    #[test]
    fn registry_lifecycle_and_default_methods() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(MockCatalogPlugin));
        registry.register(Box::new(StubPlugin));

        // Deterministic ID ordering ("a-stub" before "mock-catalog")
        let ids: Vec<&str> = registry.list().iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["a-stub", "mock-catalog"]);
        assert!(registry.get("mock-catalog").is_some());
        assert!(registry.get("missing").is_none());

        // Stub default transform_input returns unsupported error
        let stub_err = registry
            .transform_input_with(Some("a-stub"), &json!({}))
            .unwrap_err();
        assert_eq!(stub_err.plugin_id, "a-stub");
        assert!(stub_err.to_string().contains("[a-stub]"));

        // Auto-detect input via can_transform_input
        let raw_input = json!({
            "kind": "mock_catalog",
            "slug": "north-star-demo",
            "name": "North Star",
            "flagged": true
        });
        let mut doc = registry.transform_input_with(None, &raw_input).unwrap();
        assert_eq!(doc.slug, "north-star-demo");
        assert_eq!(doc.base_url, "https://north-star-demo.example");

        // Explicit plugin transform_input
        let doc_explicit = registry
            .transform_input_with(Some("mock-catalog"), &raw_input)
            .unwrap();
        assert_eq!(doc, doc_explicit);

        // Missing plugin and unmatched auto-detect errors
        assert!(
            registry
                .transform_input_with(Some("unknown"), &raw_input)
                .is_err()
        );
        assert!(
            registry
                .transform_input_with(None, &json!({"kind": "other"}))
                .is_err()
        );

        // apply_transforms
        registry.apply_transforms(&mut doc).unwrap();
        assert_eq!(doc.metadata.get("enriched_by"), Some(&json!("mock-catalog")));

        // resolve_data & resolve_all_data
        let plugin_data = registry
            .get("mock-catalog")
            .unwrap()
            .resolve_data(&doc, "/catalog")
            .unwrap();
        assert_eq!(plugin_data["active_route"], json!("/catalog"));
        let all_data = registry.resolve_all_data(&doc, "/catalog").unwrap();
        assert_eq!(all_data["active_route"], json!("/catalog"));

        // lint
        let diags = registry.collect_diagnostics(&doc, Some(&raw_input));
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].severity, DiagnosticSeverity::Advisory);
        assert_eq!(diags[1].severity, DiagnosticSeverity::Warning);

        // components & form schemas
        let components = registry.all_components();
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].name, "catalog-grid");

        let schemas = registry.all_form_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].id, "catalog-request");
    }
}
