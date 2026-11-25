//! Solo Website System (SWS) Plugin ABI and deterministic registry.
//!
//! Vertical packs and community extensions implement [`SwsPlugin`] and register
//! with [`PluginRegistry`] using `BTreeMap` storage for deterministic iteration.
//!
//! # Stable plugin ABI
//!
//! Plugins are in-process trait objects: a plugin is compiled against one
//! `sws-plugin` version and registered into a registry built from another. To
//! make accidental incompatibilities mechanically detectable, the crate
//! publishes [`PLUGIN_ABI_VERSION`] and every plugin reports the ABI it was
//! built against via [`SwsPlugin::abi_version`].
//!
//! **ABI v1 promises:**
//!
//! - The [`SwsPlugin`] trait surface: existing method names, signatures, and
//!   semantics never change within an ABI version. Adding new methods with
//!   default implementations is allowed.
//! - Serde compatibility of the plugin-facing types in this crate
//!   ([`PluginDiagnostic`], [`DiagnosticSeverity`], [`PluginComponent`],
//!   [`PluginFormSchema`], [`PluginError`]): fields are only ever added behind
//!   `#[serde(default)]`, never removed or renamed. The same holds for the
//!   `sws_ir` payloads crossing the API ([`sws_ir::SiteDocument`] produced by
//!   `transform_input`, and the [`sws_ir::ComponentDef`] /
//!   [`sws_ir::FormSchema`] values inside contributions); IR *document*
//!   payloads additionally carry their own `schema_version` in `sws_ir`.
//!
//! **ABI bumps:** any breaking change to the trait surface or to the serde
//! format of the types above bumps [`PLUGIN_ABI_VERSION`]. Because the trait's
//! `abi_version` default returns the constant from the crate the plugin was
//! actually compiled against, a plugin built against a different ABI reports a
//! stale (or newer) version, and [`PluginRegistry::register`] rejects it with
//! an error instructing the author to rebuild against the current
//! `sws-plugin`. This trades a small amount of ceremony at bump time for a
//! mechanical detection mechanism: an incompatible plugin can never be
//! silently registered and misbehave at runtime.
//!
//! # Plugin contributions
//!
//! Plugins may contribute reusable components ([`SwsPlugin::components`]) and
//! form schemas ([`SwsPlugin::form_schemas`]).
//! [`PluginRegistry::apply_to_document`] merges those contributions into a
//! [`sws_ir::SiteDocument`] (keyed by `ComponentDef::name` and
//! `FormSchema::id`) in deterministic registration order, then runs
//! `transform_ir` for every plugin. The compile pipeline invokes
//! `apply_to_document` before template expansion and rendering, so contributed
//! components and form schemas are usable without any manual merging. A
//! contribution whose key already exists — in the document or contributed by an
//! earlier plugin — is a hard error, never a silent overwrite or skip.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The plugin ABI version this crate implements.
///
/// Plugins report the ABI they were built against via
/// [`SwsPlugin::abi_version`], whose default implementation returns this
/// constant, and [`PluginRegistry::register`] rejects any plugin whose
/// reported version differs. Bump this constant on every breaking change to
/// the [`SwsPlugin`] trait surface or to the serde format of the types that
/// cross the plugin API (see the crate-level "Stable plugin ABI" section).
pub const PLUGIN_ABI_VERSION: u32 = 1;

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

    /// The plugin ABI version this plugin was compiled against.
    ///
    /// The default returns [`PLUGIN_ABI_VERSION`] as seen by the crate the
    /// plugin was compiled against, so plugins built against the current
    /// `sws-plugin` are compatible without any extra code. Override this only
    /// to deliberately pin an older ABI; such a plugin is rejected at
    /// registration once the registry's ABI version differs.
    fn abi_version(&self) -> u32 {
        PLUGIN_ABI_VERSION
    }

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
    ///
    /// This is the ABI handshake point: the plugin's
    /// [`SwsPlugin::abi_version`] must equal [`PLUGIN_ABI_VERSION`], otherwise
    /// the plugin is rejected (not inserted) with an error naming both
    /// versions.
    pub fn register(&mut self, plugin: Box<dyn SwsPlugin>) -> Result<(), PluginError> {
        let plugin_id = plugin.id();
        let reported = plugin.abi_version();
        if reported != PLUGIN_ABI_VERSION {
            return Err(PluginError::new(
                plugin_id,
                format!(
                    "unsupported plugin ABI v{reported}: this registry was built against plugin ABI v{PLUGIN_ABI_VERSION}; rebuild the plugin against the current sws-plugin"
                ),
            ));
        }
        self.plugins.insert(plugin_id.to_string(), plugin);
        Ok(())
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

    /// Apply the registry to `doc`: merge every plugin-contributed component
    /// and form schema into the document, then run `transform_ir` across all
    /// registered plugins — everything in deterministic registration (ID)
    /// order.
    ///
    /// Components are keyed by `ComponentDef::name` and form schemas by
    /// `FormSchema::id`. A contribution whose key already exists in the
    /// document, or that was already contributed by an earlier plugin, is a
    /// hard error naming the plugin and the colliding key; contributions are
    /// never silently overwritten or skipped. The compile pipeline calls this
    /// before template expansion and rendering so contributed components are
    /// renderable without manual merging.
    pub fn apply_to_document(&self, doc: &mut sws_ir::SiteDocument) -> Result<(), PluginError> {
        for plugin in self.plugins.values() {
            for component in plugin.components() {
                let name = component.definition.name.clone();
                if doc.components.contains_key(&name) {
                    return Err(PluginError::new(
                        plugin.id(),
                        format!(
                            "contributed component '{name}' collides with an existing component of the same name"
                        ),
                    ));
                }
                doc.components.insert(name, component.definition);
            }
            for schema in plugin.form_schemas() {
                let id = schema.schema.id.clone();
                if doc.forms.contains_key(&id) {
                    return Err(PluginError::new(
                        plugin.id(),
                        format!(
                            "contributed form schema '{id}' collides with an existing form schema of the same id"
                        ),
                    ));
                }
                doc.forms.insert(id, schema.schema);
            }
        }
        self.apply_transforms(doc)
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
        registry.register(Box::new(MockCatalogPlugin)).unwrap();
        registry.register(Box::new(StubPlugin)).unwrap();

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

    /// Plugin that deliberately pins an ABI version via `abi_version`.
    struct PinnedAbiPlugin(u32);

    impl SwsPlugin for PinnedAbiPlugin {
        fn id(&self) -> &'static str {
            "pinned-abi"
        }

        fn name(&self) -> &'static str {
            "Pinned ABI Plugin"
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }

        fn abi_version(&self) -> u32 {
            self.0
        }
    }

    /// Minimal `ComponentDef` with the given name for merge tests.
    fn simple_component(name: &str) -> ComponentDef {
        ComponentDef {
            name: name.to_string(),
            props: BTreeMap::new(),
            slots: Vec::new(),
            template: vec![Node::Element {
                id: None,
                tag: "div".to_string(),
                attrs: BTreeMap::new(),
                classes: vec![name.to_string()],
                style_refs: Vec::new(),
                children: Vec::new(),
            }],
        }
    }

    /// Minimal `FormSchema` with the given id for merge tests.
    fn simple_form_schema(id: &str) -> FormSchema {
        FormSchema {
            id: id.to_string(),
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
        }
    }

    /// Configurable test plugin contributing one component and one form schema.
    struct CatalogContributor {
        id: &'static str,
        component_name: &'static str,
        schema_id: &'static str,
    }

    impl SwsPlugin for CatalogContributor {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            "Catalog Contributor"
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }

        fn components(&self) -> Vec<PluginComponent> {
            vec![PluginComponent {
                name: self.component_name.to_string(),
                description: "Contributed component".to_string(),
                definition: simple_component(self.component_name),
            }]
        }

        fn form_schemas(&self) -> Vec<PluginFormSchema> {
            vec![PluginFormSchema {
                id: self.schema_id.to_string(),
                schema: simple_form_schema(self.schema_id),
            }]
        }
    }

    #[test]
    fn register_rejects_incompatible_plugin_abi() {
        let mut registry = PluginRegistry::new();

        // Older ABI than the registry's.
        let err = registry
            .register(Box::new(PinnedAbiPlugin(PLUGIN_ABI_VERSION - 1)))
            .unwrap_err();
        assert_eq!(err.plugin_id, "pinned-abi");
        assert!(
            err.message.contains("unsupported plugin ABI"),
            "unexpected message: {}",
            err.message
        );
        assert!(
            err.message.contains(&format!("v{}", PLUGIN_ABI_VERSION - 1))
                && err.message.contains(&format!("v{PLUGIN_ABI_VERSION}")),
            "message must name both versions: {}",
            err.message
        );
        // The rejected plugin is not registered.
        assert!(registry.get("pinned-abi").is_none());
        assert!(registry.list().is_empty());

        // A newer ABI than the registry's is equally incompatible.
        let err = registry
            .register(Box::new(PinnedAbiPlugin(PLUGIN_ABI_VERSION + 1)))
            .unwrap_err();
        assert_eq!(err.plugin_id, "pinned-abi");
        assert!(err.message.contains("unsupported plugin ABI"));
        assert!(registry.list().is_empty());

        // The default `abi_version` keeps current plugins compatible.
        registry.register(Box::new(StubPlugin)).unwrap();
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn apply_to_document_merges_contributions_then_transforms() {
        let mut registry = PluginRegistry::new();
        registry
            .register(Box::new(CatalogContributor {
                id: "a-contributor",
                component_name: "a-card",
                schema_id: "a-request",
            }))
            .unwrap();
        registry.register(Box::new(MockCatalogPlugin)).unwrap();
        registry
            .register(Box::new(CatalogContributor {
                id: "z-contributor",
                component_name: "z-card",
                schema_id: "z-request",
            }))
            .unwrap();

        let mut doc = SiteDocument::default();
        registry.apply_to_document(&mut doc).unwrap();

        // Contributions from every plugin were merged.
        assert!(doc.components.contains_key("a-card"));
        assert!(doc.components.contains_key("z-card"));
        assert!(doc.forms.contains_key("a-request"));
        assert!(doc.forms.contains_key("z-request"));

        // `transform_ir` still runs after the merge, in registration order.
        assert_eq!(
            doc.metadata.get("enriched_by"),
            Some(&json!("mock-catalog"))
        );
    }

    #[test]
    fn apply_to_document_rejects_colliding_contributions() {
        // Collision against a component already present in the document: hard
        // error, and the document keeps its original definition untouched.
        let mut registry = PluginRegistry::new();
        registry
            .register(Box::new(CatalogContributor {
                id: "a-contributor",
                component_name: "catalog-grid",
                schema_id: "a-request",
            }))
            .unwrap();
        let mut doc = SiteDocument::default();
        doc.components
            .insert("catalog-grid".to_string(), simple_component("catalog-grid"));
        let err = registry.apply_to_document(&mut doc).unwrap_err();
        assert_eq!(err.plugin_id, "a-contributor");
        assert!(
            err.message
                .contains("contributed component 'catalog-grid' collides"),
            "unexpected message: {}",
            err.message
        );
        assert_eq!(doc.components.len(), 1);

        // Collision between two plugins: the later plugin in registration
        // (ID) order is the one named by the error.
        let mut registry = PluginRegistry::new();
        registry
            .register(Box::new(CatalogContributor {
                id: "a-contributor",
                component_name: "shared-card",
                schema_id: "shared-form",
            }))
            .unwrap();
        registry
            .register(Box::new(CatalogContributor {
                id: "b-contributor",
                component_name: "shared-card",
                schema_id: "shared-form",
            }))
            .unwrap();
        let mut doc = SiteDocument::default();
        let err = registry.apply_to_document(&mut doc).unwrap_err();
        assert_eq!(err.plugin_id, "b-contributor");
        assert!(
            err.message.contains("'shared-card'"),
            "unexpected message: {}",
            err.message
        );

        // Collision against a form schema already present in the document.
        let mut registry = PluginRegistry::new();
        registry
            .register(Box::new(CatalogContributor {
                id: "a-contributor",
                component_name: "a-card",
                schema_id: "existing-form",
            }))
            .unwrap();
        let mut doc = SiteDocument::default();
        doc.forms
            .insert("existing-form".to_string(), simple_form_schema("existing-form"));
        let err = registry.apply_to_document(&mut doc).unwrap_err();
        assert_eq!(err.plugin_id, "a-contributor");
        assert!(
            err.message
                .contains("contributed form schema 'existing-form' collides"),
            "unexpected message: {}",
            err.message
        );
        assert_eq!(doc.forms.len(), 1);
    }
}
