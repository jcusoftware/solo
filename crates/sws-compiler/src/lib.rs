//! Deterministic, `wasm32`-clean HTML/CSS/SEO/security compiler with built-in
//! WCAG 2.2 AA diagnostics for Solo Website System (SWS).

pub mod diagnostics;
pub mod escape;
mod expand;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sws_ir::{Page, RoutePattern, ScriptRef, SiteDocument};
use sws_plugin::PluginRegistry;

pub use diagnostics::{CompilerDiagnostic, DiagnosticLevel};
pub use escape::{esc_html, esc_json_ld};

/// Default progressive-enhancement client script embedded in the compiler crate.
pub const DEFAULT_FORMS_JS: &str = include_str!("forms.js");

/// Compilation options for [`compile_site`] and [`compile_from_json`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileOptions {
    pub html_css_budget_bytes: usize,
    pub forms_js_source: Option<String>,
    pub include_headers_file: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            html_css_budget_bytes: 50 * 1024,
            forms_js_source: None,
            include_headers_file: true,
        }
    }
}

/// Deterministic output bundle produced by [`compile_site`] or [`compile_from_json`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledSite {
    pub files: BTreeMap<String, String>,
    pub diagnostics: Vec<CompilerDiagnostic>,
    pub index_html_bytes: usize,
    pub styles_css_bytes: usize,
}

/// Fatal compilation error returned when error-level diagnostics or invalid CSS/JSON occur.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileError {
    pub message: String,
    pub diagnostics: Vec<CompilerDiagnostic>,
}

impl CompileError {
    #[must_use]
    pub fn new(message: impl Into<String>, diagnostics: Vec<CompilerDiagnostic>) -> Self {
        Self {
            message: message.into(),
            diagnostics,
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CompileError {}

/// Run all compile-time WCAG 2.2 AA, security, IR, contrast, and plugin diagnostics.
#[must_use]
pub fn check_site_diagnostics(
    doc: &SiteDocument,
    raw_input: Option<&Value>,
    registry: Option<&PluginRegistry>,
    _options: &CompileOptions,
) -> Vec<CompilerDiagnostic> {
    diagnostics::run_diagnostics(doc, raw_input, registry)
}

/// Compile a validated [`SiteDocument`] into deterministic static site files.
pub fn compile_site(
    doc: &SiteDocument,
    registry: Option<&PluginRegistry>,
    options: &CompileOptions,
) -> Result<CompiledSite, CompileError> {
    compile_site_internal(doc, None, registry, options)
}

/// Compile a pre-parsed [`SiteDocument`] along with the original raw JSON value for plugin lint.
pub fn compile_site_with_raw(
    doc: &SiteDocument,
    raw_input: Option<&Value>,
    registry: Option<&PluginRegistry>,
    options: &CompileOptions,
) -> Result<CompiledSite, CompileError> {
    compile_site_internal(doc, raw_input, registry, options)
}

/// Parse raw JSON (either an IR [`SiteDocument`] or plugin-specific manifest),
/// apply registered plugins, and compile to static files.
pub fn compile_from_json(
    input_json: &str,
    registry: &PluginRegistry,
    options: &CompileOptions,
) -> Result<CompiledSite, CompileError> {
    let val: Value = serde_json::from_str(input_json).map_err(|e| {
        CompileError::new(format!("invalid JSON input: {e}"), Vec::new())
    })?;

    let is_ir_doc = val.is_object()
        && val.get("pages").and_then(Value::as_array).is_some()
        && val.get("slug").is_some();
    if is_ir_doc {
        // Deserialize straight from the input string instead of deep-cloning
        // the parsed `Value` (issue #13); the `Value` stays alive unchanged as
        // the plugin-lint `raw_input`.
        let mut doc: SiteDocument = serde_json::from_str(input_json).map_err(|e| {
            CompileError::new(format!("invalid SiteDocument JSON: {e}"), Vec::new())
        })?;
        // Apply plugin contributions and IR transforms before expansion and
        // rendering so contributed components are renderable (issue #18).
        registry
            .apply_to_document(&mut doc)
            .map_err(|e| CompileError::new(e.to_string(), Vec::new()))?;
        return compile_site_internal(&doc, Some(&val), Some(registry), options);
    }

    let mut doc = registry
        .transform_input_with(None, &val)
        .map_err(|e| CompileError::new(e.to_string(), Vec::new()))?;
    registry
        .apply_to_document(&mut doc)
        .map_err(|e| CompileError::new(e.to_string(), Vec::new()))?;

    compile_site_internal(&doc, Some(&val), Some(registry), options)
}

fn compile_site_internal(
    doc: &SiteDocument,
    raw_input: Option<&Value>,
    registry: Option<&PluginRegistry>,
    options: &CompileOptions,
) -> Result<CompiledSite, CompileError> {
    // Expand each page body exactly once; the single `ExpandedBody` per page
    // feeds both the diagnostics pass and the HTML renderer (issue #13).
    let expanded_bodies: Vec<expand::ExpandedBody> = doc
        .pages
        .iter()
        .map(|page| expand::expand_page_body(doc, &page.body))
        .collect();
    let mut diagnostics = diagnostics::run_diagnostics_with_expanded(
        doc,
        raw_input,
        registry,
        &expanded_bodies,
    );
    if let Some(first_err) = diagnostics
        .iter()
        .find(|d| d.level == DiagnosticLevel::Error)
    {
        return Err(CompileError::new(
            format!("compilation failed: [{}] {}", first_err.code, first_err.message),
            diagnostics,
        ));
    }

    let styles_css = emit_stylesheet(doc).map_err(|err_msg| {
        let diag = CompilerDiagnostic {
            level: DiagnosticLevel::Error,
            code: "invalid_css".to_string(),
            path: "styles".to_string(),
            message: err_msg.clone(),
        };
        diagnostics.push(diag);
        CompileError::new(err_msg, diagnostics.clone())
    })?;

    let mut files = BTreeMap::new();
    // Tracks lowercased output paths so case-insensitive filesystems
    // (macOS APFS, Windows NTFS) cannot silently overwrite one page with
    // another (e.g. `About/index.html` vs `about/index.html`).
    let mut lowercased_paths: BTreeMap<String, String> = BTreeMap::new();
    let mut needs_forms_js = doc.scripts.iter().any(|s| s.src == "/js/forms.js");

    for (p_idx, page) in doc.pages.iter().enumerate() {
        let Some(out_path) = output_path_for_page(page) else {
            continue;
        };
        if page.scripts.iter().any(|s| s.src == "/js/forms.js") {
            needs_forms_js = true;
        }
        if files.contains_key(&out_path) {
            let msg = format!("duplicate compiled output path '{out_path}' for page '{}'", page.id);
            diagnostics.push(CompilerDiagnostic::error(
                "output_path_collision",
                out_path,
                msg.clone(),
            ));
            return Err(CompileError::new(msg, diagnostics));
        }
        let lowercased = out_path.to_ascii_lowercase();
        if let Some(previous) = lowercased_paths.get(&lowercased) {
            let msg = format!(
                "output paths '{previous}' and '{out_path}' differ only by case and collide on case-insensitive filesystems; rename one route or use a single casing"
            );
            diagnostics.push(CompilerDiagnostic::error(
                "output_path_collision",
                out_path.clone(),
                msg.clone(),
            ));
            return Err(CompileError::new(msg, diagnostics));
        }
        lowercased_paths.insert(lowercased, out_path.clone());
        let html = render_page_html(doc, page, &expanded_bodies[p_idx]);
        files.insert(out_path, html);
    }

    let index_html_bytes = files.get("index.html").map_or(0, String::len);
    let styles_css_bytes = styles_css.len();

    for (rel_path, html) in &files {
        if !rel_path.ends_with(".html") {
            continue;
        }
        let page_total = html.len() + styles_css_bytes;
        if page_total > options.html_css_budget_bytes {
            let msg = format!(
                "{rel_path} ({} B) + styles.css ({styles_css_bytes} B) = {page_total} B exceeds {} B budget",
                html.len(),
                options.html_css_budget_bytes
            );
            diagnostics.push(CompilerDiagnostic {
                level: DiagnosticLevel::Error,
                code: "budget_exceeded".to_string(),
                path: rel_path.clone(),
                message: msg.clone(),
            });
            return Err(CompileError::new(msg, diagnostics));
        }
    }

    let mut reserved_files: Vec<(&str, String)> = vec![
        ("styles.css", styles_css),
        ("robots.txt", render_robots_txt(doc)),
        ("sitemap.xml", render_sitemap_xml(doc)),
    ];
    if needs_forms_js {
        let js = options
            .forms_js_source
            .as_deref()
            .unwrap_or(DEFAULT_FORMS_JS)
            .to_string();
        reserved_files.push(("js/forms.js", js));
    }
    if options.include_headers_file {
        reserved_files.push(("_headers", sws_security::headers::render_headers_file()));
    }

    for (reserved_path, content) in reserved_files {
        if files.contains_key(reserved_path) {
            let msg = format!("page output collides with reserved file '{reserved_path}'");
            diagnostics.push(CompilerDiagnostic::error(
                "output_path_collision",
                reserved_path,
                msg.clone(),
            ));
            return Err(CompileError::new(msg, diagnostics));
        }
        files.insert(reserved_path.to_string(), content);
    }

    // Ensure no emitted file path is also a directory prefix of another emitted path
    // (e.g. `_headers` vs `_headers/index.html` or `index.html` vs `index.html/index.html`).
    let all_paths: Vec<String> = files.keys().cloned().collect();
    for path in &all_paths {
        let prefix = format!("{path}/");
        if let Some(conflict) = all_paths.iter().find(|other| other.starts_with(&prefix)) {
            let msg = format!(
                "output path '{conflict}' collides with file path '{path}' (file/directory conflict)"
            );
            diagnostics.push(CompilerDiagnostic::error(
                "output_path_collision",
                conflict.clone(),
                msg.clone(),
            ));
            return Err(CompileError::new(msg, diagnostics));
        }
    }

    Ok(CompiledSite {
        files,
        diagnostics,
        index_html_bytes,
        styles_css_bytes,
    })
}

fn emit_stylesheet(doc: &SiteDocument) -> Result<String, String> {
    let mut sheet = sws_css::Stylesheet::new();
    sheet.comment_header = Some(format!(
        "Generated by sws-compiler for {}. Do not edit.",
        doc.slug
    ));
    sheet.include_reduced_motion_defaults();

    for (key, hex) in &doc.tokens.colors {
        sheet.set_var(key, hex);
    }
    sheet.set_var("font-body", &doc.tokens.fonts.body);
    if let Some(heading) = &doc.tokens.fonts.heading {
        sheet.set_var("font-heading", heading);
    }
    if let Some(mono) = &doc.tokens.fonts.mono {
        sheet.set_var("font-mono", mono);
    }
    for (key, val) in &doc.tokens.spacing {
        sheet.set_var(format!("space-{key}"), val);
    }
    for (key, val) in &doc.tokens.radii {
        sheet.set_var(format!("radius-{key}"), val);
    }

    for bp in &doc.breakpoints {
        sheet.define_breakpoint(&bp.id, &bp.media_query);
    }

    for rule in &doc.styles {
        sheet.add_rule(sws_css::CssRule {
            selector: rule.selector.clone(),
            breakpoint: rule.breakpoint.clone(),
            pseudo: rule.pseudo.clone(),
            declarations: rule.declarations.clone(),
        });
    }

    sheet.emit().map_err(|e| format!("CSS emit error: {e}"))
}

fn output_path_for_page(page: &Page) -> Option<String> {
    if page.is_not_found {
        return Some("404.html".to_string());
    }
    match &page.route {
        RoutePattern::Static { path } => {
            if !sws_ir::is_valid_static_route(path) {
                return None;
            }
            let trimmed = path.trim_matches('/');
            if trimmed.is_empty() {
                Some("index.html".to_string())
            } else if trimmed == "404" || trimmed == "404.html" {
                Some("404.html".to_string())
            } else {
                Some(format!("{trimmed}/index.html"))
            }
        }
        RoutePattern::Dynamic { .. } => None,
    }
}

fn render_page_html(doc: &SiteDocument, page: &Page, expanded: &expand::ExpandedBody) -> String {
    let lang = doc
        .locale
        .split('-')
        .next()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("en");

    let effective_title = match &doc.seo.title_template {
        Some(tmpl) if !page.is_not_found && tmpl.contains("%s") => {
            tmpl.replace("%s", &page.title)
        }
        _ => page.title.clone(),
    };

    let description = page
        .description
        .as_deref()
        .or(doc.seo.default_description.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let theme_color = doc
        .tokens
        .colors
        .get("background")
        .or_else(|| doc.tokens.colors.get("bg"))
        .map(String::as_str);

    let mut merged_scripts: Vec<&ScriptRef> = Vec::new();
    let mut seen_srcs = BTreeSet::new();
    for s in doc.scripts.iter().chain(page.scripts.iter()) {
        if seen_srcs.insert(s.src.as_str()) {
            merged_scripts.push(s);
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "<!doctype html>");
    let _ = writeln!(out, "<html lang=\"{}\">", esc_html(lang));
    let _ = writeln!(out, "<head>");
    let _ = writeln!(out, "<meta charset=\"utf-8\">");
    let _ = writeln!(
        out,
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
    );
    let _ = writeln!(out, "<title>{}</title>", esc_html(&effective_title));

    if let Some(desc) = description {
        let _ = writeln!(
            out,
            "<meta name=\"description\" content=\"{}\">",
            esc_html(desc)
        );
    }
    if page.noindex || page.is_not_found {
        let _ = writeln!(out, "<meta name=\"robots\" content=\"noindex\">");
    }
    if let Some(tc) = theme_color {
        let _ = writeln!(
            out,
            "<meta name=\"theme-color\" content=\"{}\">",
            esc_html(tc)
        );
    }
    if let Some(canonical) = &page.canonical
        && !canonical.trim().is_empty()
        && sws_ir::is_valid_https_url(canonical)
    {
        let _ = writeln!(
            out,
            "<link rel=\"canonical\" href=\"{}\">",
            esc_html(canonical)
        );
    }
    let _ = writeln!(out, "<link rel=\"icon\" href=\"data:,\">");
    let _ = writeln!(out, "<link rel=\"stylesheet\" href=\"/styles.css\">");

    for ld in &page.json_ld {
        let _ = writeln!(
            out,
            "<script type=\"application/ld+json\">{}</script>",
            esc_json_ld(&ld.data)
        );
    }

    for s in merged_scripts {
        if !expand::is_safe_script_src(&s.src) {
            continue;
        }
        let async_attr = if s.async_load { " async" } else { "" };
        let defer_attr = if s.defer { " defer" } else { "" };
        let _ = writeln!(
            out,
            "<script src=\"{}\"{async_attr}{defer_attr}></script>",
            esc_html(&s.src)
        );
    }

    let _ = writeln!(out, "</head>");
    let _ = writeln!(out, "<body>");

    // The page body was expanded once in `compile_site_internal` and shared
    // with the diagnostics pass; render it without re-expanding (issue #13).
    out.push_str(&expand::render_expanded_html(&expanded.nodes));
    if !out.ends_with('\n') {
        out.push('\n');
    }

    let _ = writeln!(out, "</body>");
    let _ = writeln!(out, "</html>");
    out
}

fn render_robots_txt(doc: &SiteDocument) -> String {
    let base = doc.base_url.trim_end_matches('/');
    let mut out = String::from("User-agent: *\n");
    if doc.seo.robots_allow.is_empty() {
        out.push_str("Allow: /\n");
    } else {
        for allow in &doc.seo.robots_allow {
            let _ = writeln!(out, "Allow: {allow}");
        }
    }
    for disallow in &doc.seo.robots_disallow {
        let _ = writeln!(out, "Disallow: {disallow}");
    }
    let _ = writeln!(out, "\nSitemap: {base}/sitemap.xml");
    out
}

fn render_sitemap_xml(doc: &SiteDocument) -> String {
    let base = doc.base_url.trim_end_matches('/');
    let routes: Vec<String> = if !doc.seo.sitemap_routes.is_empty() {
        doc.seo.sitemap_routes.clone()
    } else {
        let collected: Vec<String> = doc
            .pages
            .iter()
            .filter(|p| !p.is_not_found && !p.noindex)
            .filter_map(|p| match &p.route {
                RoutePattern::Static { path } => Some(path.clone()),
                RoutePattern::Dynamic { .. } => None,
            })
            .collect();
        if collected.is_empty() {
            vec!["/".to_string()]
        } else {
            collected
        }
    };

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for route in routes {
        let full_url = format!("{base}{route}");
        let _ = writeln!(out, "  <url><loc>{}</loc></url>", esc_html(&full_url));
    }
    out.push_str("</urlset>\n");
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sws_ir::{
        ComponentDef, ContrastPair, DesignTokens, FormFieldKind, FormFieldSchema, FormSchema,
        FontTokens, IrStyleRule, JsonLdBlock, Node, PropKind, PropSpec, SeoMeta,
    };
    use sws_plugin::{
        DiagnosticSeverity, PluginComponent, PluginDiagnostic, PluginError, PluginFormSchema,
        SwsPlugin,
    };

    use super::*;

    fn elem(tag: &str, attrs: BTreeMap<String, String>, children: Vec<Node>) -> Node {
        Node::Element {
            id: None,
            tag: tag.to_string(),
            attrs,
            classes: Vec::new(),
            style_refs: Vec::new(),
            children,
        }
    }

    fn text(val: &str) -> Node {
        Node::Text {
            value: val.to_string(),
        }
    }

    fn valid_page(id: &str, path: &str, is_not_found: bool, main_children: Vec<Node>) -> Page {
        Page {
            id: id.to_string(),
            route: RoutePattern::Static {
                path: path.to_string(),
            },
            title: id.to_string(),
            description: Some("Sample page".to_string()),
            canonical: (!is_not_found).then(|| format!("https://acme.example{path}")),
            noindex: is_not_found,
            is_not_found,
            json_ld: Vec::new(),
            scripts: Vec::new(),
            body: vec![
                Node::Element {
                    id: None,
                    tag: "a".to_string(),
                    attrs: BTreeMap::from([("href".to_string(), "#main".to_string())]),
                    classes: vec!["skip-link".to_string()],
                    style_refs: Vec::new(),
                    children: vec![text("Skip to main content")],
                },
                elem("header", BTreeMap::new(), vec![text("Header")]),
                elem(
                    "main",
                    BTreeMap::from([
                        ("id".to_string(), "main".to_string()),
                        ("tabindex".to_string(), "-1".to_string()),
                    ]),
                    main_children,
                ),
                elem("footer", BTreeMap::new(), vec![text("Footer")]),
            ],
        }
    }

    fn base_doc() -> SiteDocument {
        let mut colors = BTreeMap::new();
        colors.insert("bg".to_string(), "#F7F5F0".to_string());
        colors.insert("text".to_string(), "#1A1A1A".to_string());

        SiteDocument {
            schema_version: 1,
            slug: "acme-demo".to_string(),
            name: "Acme Demo".to_string(),
            locale: "en-US".to_string(),
            base_url: "https://acme.example".to_string(),
            tokens: DesignTokens {
                colors,
                fonts: FontTokens {
                    body: "system-ui, sans-serif".to_string(),
                    heading: Some("Georgia, serif".to_string()),
                    mono: None,
                },
                spacing: BTreeMap::from([("md".to_string(), "1rem".to_string())]),
                radii: BTreeMap::from([("card".to_string(), "0.5rem".to_string())]),
                contrast_pairs: vec![ContrastPair {
                    foreground: "text".to_string(),
                    background: "bg".to_string(),
                    min_ratio: 4.5,
                    usage: Some("body".to_string()),
                }],
            },
            breakpoints: Vec::new(),
            styles: vec![IrStyleRule {
                id: "body".to_string(),
                selector: "body".to_string(),
                breakpoint: None,
                pseudo: None,
                declarations: BTreeMap::from([
                    ("background".to_string(), "var(--bg)".to_string()),
                    ("color".to_string(), "var(--text)".to_string()),
                ]),
            }],
            components: BTreeMap::new(),
            forms: BTreeMap::new(),
            pages: vec![valid_page(
                "home",
                "/",
                false,
                vec![elem("h1", BTreeMap::new(), vec![text("Acme Demo")])],
            )],
            seo: SeoMeta {
                title_template: None,
                default_description: Some("Default description".to_string()),
                robots_allow: vec!["/".to_string()],
                robots_disallow: vec!["/api/".to_string()],
                sitemap_routes: vec!["/".to_string()],
            },
            scripts: Vec::new(),
            data: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn compiles_deterministic_bundle_with_components_loops_conditionals_and_routes() {
        let mut doc = base_doc();
        doc.data.insert("show_extra".to_string(), json!(true));
        doc.data
            .insert("items".to_string(), json!(["Alpha", "Beta"]));
        doc.components.insert(
            "card".to_string(),
            ComponentDef {
                name: "card".to_string(),
                props: BTreeMap::from([(
                    "heading".to_string(),
                    PropSpec {
                        kind: PropKind::String,
                        required: true,
                        default: Some(json!("Card")),
                        description: None,
                    },
                )]),
                slots: vec!["default".to_string()],
                template: vec![elem(
                    "section",
                    BTreeMap::new(),
                    vec![
                        elem("h2", BTreeMap::new(), vec![text("{{heading}}")]),
                        Node::SlotRef {
                            name: "default".to_string(),
                            fallback: vec![text("Fallback")],
                        },
                    ],
                )],
            },
        );
        doc.pages[0].scripts.push(ScriptRef {
            src: "/js/forms.js".to_string(),
            async_load: false,
            defer: true,
        });
        doc.pages[0].json_ld.push(JsonLdBlock {
            id: None,
            data: json!({"@context": "https://schema.org", "name": "</script>"}),
        });
        doc.pages[0].body = vec![
            Node::Element {
                id: None,
                tag: "a".to_string(),
                attrs: BTreeMap::from([("href".to_string(), "#main".to_string())]),
                classes: vec!["skip-link".to_string()],
                style_refs: Vec::new(),
                children: vec![text("Skip")],
            },
            elem("header", BTreeMap::new(), vec![text("Hdr")]),
            elem(
                "main",
                BTreeMap::from([
                    ("id".to_string(), "main".to_string()),
                    ("tabindex".to_string(), "-1".to_string()),
                ]),
                vec![
                    elem("h1", BTreeMap::new(), vec![text("Acme")]),
                    Node::ComponentInstance {
                        component: "card".to_string(),
                        props: BTreeMap::from([("heading".to_string(), json!("Services"))]),
                        slots: BTreeMap::from([(
                            "default".to_string(),
                            vec![Node::Conditional {
                                expr: "show_extra".to_string(),
                                then_nodes: vec![elem(
                                    "ul",
                                    BTreeMap::new(),
                                    vec![Node::Loop {
                                        item_var: "it".to_string(),
                                        collection_expr: "items".to_string(),
                                        body: vec![elem(
                                            "li",
                                            BTreeMap::new(),
                                            vec![text("{{it}}")],
                                        )],
                                    }],
                                )],
                                else_nodes: Vec::new(),
                            }],
                        )]),
                    },
                ],
            ),
            elem("footer", BTreeMap::new(), vec![text("Ftr")]),
        ];
        doc.pages.push(valid_page(
            "about",
            "/about",
            false,
            vec![elem("h1", BTreeMap::new(), vec![text("About")])],
        ));
        doc.pages.push(valid_page(
            "not-found",
            "/404",
            true,
            vec![elem("h1", BTreeMap::new(), vec![text("Not Found")])],
        ));
        doc.pages.push(Page {
            id: "dynamic-item".to_string(),
            route: RoutePattern::Dynamic {
                pattern: "/items/{slug}".to_string(),
                params: vec!["slug".to_string()],
            },
            title: "Item".to_string(),
            description: None,
            canonical: None,
            noindex: false,
            is_not_found: false,
            json_ld: Vec::new(),
            scripts: Vec::new(),
            body: Vec::new(),
        });

        let opts = CompileOptions::default();
        let compiled_a = compile_site(&doc, None, &opts).unwrap();
        let compiled_b = compile_site(&doc, None, &opts).unwrap();
        assert_eq!(compiled_a, compiled_b);

        assert!(compiled_a.files.contains_key("index.html"));
        assert!(compiled_a.files.contains_key("about/index.html"));
        assert!(compiled_a.files.contains_key("404.html"));
        assert!(compiled_a.files.contains_key("styles.css"));
        assert!(compiled_a.files.contains_key("robots.txt"));
        assert!(compiled_a.files.contains_key("sitemap.xml"));
        assert!(compiled_a.files.contains_key("js/forms.js"));
        assert!(compiled_a.files.contains_key("_headers"));

        let index = &compiled_a.files["index.html"];
        assert!(index.contains("<h2>Services</h2>"));
        assert!(index.contains("<li>Alpha</li><li>Beta</li>"));
        assert!(index.contains(r"\u003c/script\u003e"));
        assert!(index.contains(r#"<html lang="en">"#));
        assert!(compiled_a.files["404.html"].contains(r#"<meta name="robots" content="noindex">"#));
        assert!(compiled_a.files["styles.css"].contains("--space-md: 1rem;"));
        assert!(compiled_a.files["styles.css"].contains("--radius-card: 0.5rem;"));
    }

    #[test]
    fn catches_wcag_and_security_errors() {
        let mut doc = base_doc();
        // Skipped heading h1 -> h3, missing footer, unlabeled input, banned overlay, low contrast
        doc.tokens.colors.insert("text".to_string(), "#999999".to_string());
        doc.tokens.fonts.body = "Arial; url(https://evil.example)".to_string();
        doc.pages[0].scripts.push(ScriptRef {
            src: "https://cdn.userway.org/widget.js".to_string(),
            async_load: true,
            defer: false,
        });
        doc.pages[0].body = vec![
            elem(
                "main",
                BTreeMap::from([("id".to_string(), "main".to_string())]), // missing tabindex="-1"
                vec![
                    elem("h1", BTreeMap::new(), vec![text("Title")]),
                    elem("h3", BTreeMap::new(), vec![text("Jump")]),
                    elem(
                        "input",
                        BTreeMap::from([("type".to_string(), "email".to_string())]),
                        Vec::new(),
                    ),
                    elem(
                        "div",
                        BTreeMap::from([("aria-hidden".to_string(), "true".to_string())]),
                        vec![elem(
                            "input",
                            BTreeMap::from([("type".to_string(), "text".to_string())]),
                            Vec::new(),
                        )],
                    ),
                ],
            ),
        ];

        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        let codes: Vec<&str> = err.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert!(codes.contains(&"unsafe_font_stack"), "{codes:?}");
        assert!(codes.contains(&"contrast_ratio_too_low"), "{codes:?}");
        assert!(codes.contains(&"banned_accessibility_overlay"), "{codes:?}");
        assert!(codes.contains(&"skipped_heading_level"), "{codes:?}");
        assert!(codes.contains(&"missing_header_landmark"), "{codes:?}");
        assert!(codes.contains(&"invalid_main_landmark"), "{codes:?}");
        assert!(codes.contains(&"missing_footer_landmark"), "{codes:?}");
        assert!(codes.contains(&"missing_skip_link"), "{codes:?}");
        assert_eq!(
            codes
                .iter()
                .filter(|&&c| c == "unlabeled_form_control")
                .count(),
            1,
            "aria-hidden input should be ignored; visible unlabeled input flagged"
        );
    }

    #[test]
    fn enforces_html_css_byte_budget() {
        let doc = base_doc();
        let err = compile_site(
            &doc,
            None,
            &CompileOptions {
                html_css_budget_bytes: 64,
                ..CompileOptions::default()
            },
        )
        .unwrap_err();
        assert!(err.diagnostics.iter().any(|d| d.code == "budget_exceeded"));
    }

    #[test]
    fn enforces_html_css_budget_on_secondary_pages() {
        let mut doc = base_doc();
        let mut about = doc.pages[0].clone();
        about.id = "about".to_string();
        about.route = RoutePattern::Static {
            path: "/about".to_string(),
        };
        about.title = "About".to_string();
        if let Some(Node::Element { children, .. }) = about
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(elem("p", BTreeMap::new(), vec![text(&"x".repeat(4096))]));
        }
        doc.pages.push(about);

        let err = compile_site(
            &doc,
            None,
            &CompileOptions {
                html_css_budget_bytes: 2048,
                ..CompileOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "budget_exceeded" && d.path == "about/index.html"),
            "expected budget_exceeded on about/index.html: {:?}",
            err.diagnostics
        );
    }

    #[test]
    fn compile_from_json_accepts_minimal_ir_with_defaulted_tokens() {
        let reg = PluginRegistry::new();
        let minimal_json = serde_json::json!({
            "slug": "minimal-ir-site",
            "name": "Minimal IR Site",
            "base_url": "https://minimal.example",
            "pages": [
                {
                    "id": "home",
                    "route": { "kind": "static", "path": "/" },
                    "title": "Home",
                    "body": [
                        {
                            "type": "element",
                            "tag": "a",
                            "classes": ["skip-link"],
                            "attrs": { "href": "#main" },
                            "children": [{ "type": "text", "value": "Skip to main content" }]
                        },
                        {
                            "type": "element",
                            "tag": "header",
                            "children": [{ "type": "text", "value": "Header" }]
                        },
                        {
                            "type": "element",
                            "tag": "main",
                            "attrs": { "id": "main", "tabindex": "-1" },
                            "children": [
                                {
                                    "type": "element",
                                    "tag": "h1",
                                    "children": [{ "type": "text", "value": "Hello" }]
                                }
                            ]
                        },
                        {
                            "type": "element",
                            "tag": "footer",
                            "children": [{ "type": "text", "value": "Footer" }]
                        }
                    ]
                }
            ]
        })
        .to_string();

        let compiled = compile_from_json(&minimal_json, &reg, &CompileOptions::default())
            .expect("minimal IR with omitted tokens should compile");
        assert!(compiled.files.contains_key("index.html"));
    }

    struct DummyPlugin;
    impl SwsPlugin for DummyPlugin {
        fn id(&self) -> &'static str {
            "dummy"
        }
        fn name(&self) -> &'static str {
            "Dummy"
        }
        fn version(&self) -> &'static str {
            "0.1.0"
        }
        fn can_transform_input(&self, input: &Value) -> bool {
            input.get("kind").and_then(Value::as_str) == Some("dummy")
        }
        fn transform_input(&self, _input: &Value) -> Result<SiteDocument, PluginError> {
            Ok(base_doc())
        }
        fn lint(&self, _doc: &SiteDocument, _raw: Option<&Value>) -> Vec<PluginDiagnostic> {
            vec![PluginDiagnostic {
                plugin_id: "dummy".to_string(),
                rule: "advisory-note".to_string(),
                severity: DiagnosticSeverity::Advisory,
                path: "$".to_string(),
                message: "Advisory check".to_string(),
                citation: None,
            }]
        }
    }

    #[test]
    fn compile_from_json_supports_plugins_and_advisory_diagnostics() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(DummyPlugin))
            .expect("DummyPlugin uses the current plugin ABI");

        let compiled = compile_from_json(r#"{"kind":"dummy"}"#, &reg, &CompileOptions::default())
            .expect("advisory lint should not fail compilation");
        assert!(compiled
            .diagnostics
            .iter()
            .any(|d| d.level == DiagnosticLevel::Warning && d.code == "plugin:dummy:advisory-note"));
        assert!(compiled.files.contains_key("index.html"));
    }

    /// Reusable `ComponentDef` for the contribution tests (issue #18).
    fn contributed_card_def() -> ComponentDef {
        ComponentDef {
            name: "contributed-card".to_string(),
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
                tag: "p".to_string(),
                attrs: BTreeMap::new(),
                classes: vec!["contributed-card".to_string()],
                style_refs: Vec::new(),
                children: vec![Node::Text {
                    value: "{{title}}".to_string(),
                }],
            }],
        }
    }

    /// Reusable `FormSchema` for the contribution tests (issue #18).
    fn contributed_form_schema() -> FormSchema {
        FormSchema {
            id: "contributed-contact".to_string(),
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

    /// Test plugin contributing a component and a form schema through the
    /// pack-independent `PluginRegistry` path (issue #18).
    struct ContributingPlugin;

    impl SwsPlugin for ContributingPlugin {
        fn id(&self) -> &'static str {
            "contributor"
        }
        fn name(&self) -> &'static str {
            "Contributing Plugin"
        }
        fn version(&self) -> &'static str {
            "0.1.0"
        }
        fn can_transform_input(&self, input: &Value) -> bool {
            input.get("kind").and_then(Value::as_str) == Some("contributor")
        }
        fn transform_input(&self, _input: &Value) -> Result<SiteDocument, PluginError> {
            let mut doc = base_doc();
            // Reference the contributed component from the page body. Without
            // the registry merge this instance renders nothing and IR
            // validation rejects the unknown component.
            if let Some(Node::Element { children, .. }) = doc.pages[0]
                .body
                .iter_mut()
                .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
            {
                children.push(Node::ComponentInstance {
                    component: "contributed-card".to_string(),
                    props: BTreeMap::from([("title".to_string(), json!("From Plugin"))]),
                    slots: BTreeMap::new(),
                });
            }
            Ok(doc)
        }
        fn components(&self) -> Vec<PluginComponent> {
            vec![PluginComponent {
                name: "contributed-card".to_string(),
                description: "Card contributed by the test plugin".to_string(),
                definition: contributed_card_def(),
            }]
        }
        fn form_schemas(&self) -> Vec<PluginFormSchema> {
            vec![PluginFormSchema {
                id: "contributed-contact".to_string(),
                schema: contributed_form_schema(),
            }]
        }
        fn lint(&self, doc: &SiteDocument, _raw: Option<&Value>) -> Vec<PluginDiagnostic> {
            // Observation channel: `lint` runs on the post-merge document
            // inside the compiler, so these diagnostics prove the
            // contributions reached the compile pipeline unmerged-by-hand.
            let mut out = Vec::new();
            if doc.components.contains_key("contributed-card") {
                out.push(PluginDiagnostic {
                    plugin_id: self.id().to_string(),
                    rule: "contributed-component-present".to_string(),
                    severity: DiagnosticSeverity::Advisory,
                    path: "components.contributed-card".to_string(),
                    message: "contributed component reached the compiler".to_string(),
                    citation: None,
                });
            }
            if doc.forms.contains_key("contributed-contact") {
                out.push(PluginDiagnostic {
                    plugin_id: self.id().to_string(),
                    rule: "contributed-form-schema-present".to_string(),
                    severity: DiagnosticSeverity::Advisory,
                    path: "forms.contributed-contact".to_string(),
                    message: "contributed form schema reached the compiler".to_string(),
                    citation: None,
                });
            }
            out
        }
    }

    #[test]
    fn compile_from_json_plugin_contributions_reach_compiled_output() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(ContributingPlugin))
            .expect("test plugin uses the current plugin ABI");

        let compiled =
            compile_from_json(r#"{"kind":"contributor"}"#, &reg, &CompileOptions::default())
                .expect("plugin contributions must merge before compilation");

        // The contributed component rendered into the compiled HTML.
        let html = compiled
            .files
            .get("index.html")
            .expect("compiled index.html");
        assert!(
            html.contains("contributed-card") && html.contains("From Plugin"),
            "contributed component markup missing from HTML: {html}"
        );

        // The contributed form schema reached the compiled document.
        assert!(compiled
            .diagnostics
            .iter()
            .any(|d| d.code == "plugin:contributor:contributed-component-present"));
        assert!(compiled
            .diagnostics
            .iter()
            .any(|d| d.code == "plugin:contributor:contributed-form-schema-present"));
    }

    #[test]
    fn compile_from_json_ir_input_merges_plugin_contributions() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(ContributingPlugin))
            .expect("test plugin uses the current plugin ABI");

        // Plain SiteDocument IR referencing the contributed component.
        let ir = json!({
            "slug": "acme-demo",
            "name": "Acme Demo",
            "base_url": "https://acme.example",
            "pages": [
                {
                    "id": "home",
                    "route": { "kind": "static", "path": "/" },
                    "title": "Home",
                    "body": [
                        {
                            "type": "element",
                            "tag": "a",
                            "classes": ["skip-link"],
                            "attrs": { "href": "#main" },
                            "children": [{ "type": "text", "value": "Skip to main content" }]
                        },
                        { "type": "element", "tag": "header", "children": [{ "type": "text", "value": "Header" }] },
                        {
                            "type": "element",
                            "tag": "main",
                            "attrs": { "id": "main", "tabindex": "-1" },
                            "children": [
                                { "type": "element", "tag": "h1", "children": [{ "type": "text", "value": "Hello" }] },
                                {
                                    "type": "component_instance",
                                    "component": "contributed-card",
                                    "props": { "title": "From IR Input" }
                                }
                            ]
                        },
                        { "type": "element", "tag": "footer", "children": [{ "type": "text", "value": "Footer" }] }
                    ]
                }
            ]
        })
        .to_string();

        let compiled = compile_from_json(&ir, &reg, &CompileOptions::default())
            .expect("IR input must also merge plugin contributions before compilation");
        let html = compiled
            .files
            .get("index.html")
            .expect("compiled index.html");
        assert!(
            html.contains("contributed-card") && html.contains("From IR Input"),
            "contributed component markup missing from HTML: {html}"
        );
    }

    /// Plugin whose `transform_input` already inserts a component under the
    /// same name it contributes, forcing a merge collision (issue #18).
    struct CollidingPlugin;

    impl SwsPlugin for CollidingPlugin {
        fn id(&self) -> &'static str {
            "collider"
        }
        fn name(&self) -> &'static str {
            "Colliding Plugin"
        }
        fn version(&self) -> &'static str {
            "0.1.0"
        }
        fn can_transform_input(&self, input: &Value) -> bool {
            input.get("kind").and_then(Value::as_str) == Some("collider")
        }
        fn transform_input(&self, _input: &Value) -> Result<SiteDocument, PluginError> {
            let mut doc = base_doc();
            doc.components
                .insert("contributed-card".to_string(), contributed_card_def());
            Ok(doc)
        }
        fn components(&self) -> Vec<PluginComponent> {
            vec![PluginComponent {
                name: "contributed-card".to_string(),
                description: "Colliding contribution".to_string(),
                definition: contributed_card_def(),
            }]
        }
    }

    #[test]
    fn compile_from_json_rejects_colliding_plugin_contribution() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(CollidingPlugin))
            .expect("test plugin uses the current plugin ABI");

        let err = compile_from_json(r#"{"kind":"collider"}"#, &reg, &CompileOptions::default())
            .expect_err("a name collision must be a hard error, not a silent overwrite");
        assert!(
            err.message.contains("contributed-card")
                && err.message.contains("collides with an existing component"),
            "unexpected error message: {}",
            err.message
        );
    }

    #[test]
    fn rejects_xss_elements_event_handlers_unsafe_urls_and_script_srcs() {
        let mut doc = base_doc();
        doc.scripts.push(ScriptRef {
            src: "javascript:alert(1)".to_string(),
            async_load: false,
            defer: true,
        });
        doc.pages[0].scripts.push(ScriptRef {
            src: "http://insecure.example/app.js".to_string(),
            async_load: false,
            defer: true,
        });
        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(elem("script", BTreeMap::new(), vec![text("alert(1)")]));
            children.push(elem(
                "a",
                BTreeMap::from([
                    ("href".to_string(), "javascript:alert(1)".to_string()),
                    ("onclick".to_string(), "alert(2)".to_string()),
                    ("style".to_string(), "color:red".to_string()),
                ]),
                vec![text("Click")],
            ));
            children.push(elem(
                "iframe",
                BTreeMap::from([("srcdoc".to_string(), "<script>alert(3)</script>".to_string())]),
                Vec::new(),
            ));
        }

        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        let codes: Vec<&str> = err.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert!(codes.contains(&"unsafe_script_src"), "{codes:?}");
        assert!(codes.contains(&"unsafe_html_element"), "{codes:?}");
        assert!(codes.contains(&"unsafe_html_attribute"), "{codes:?}");
        assert!(codes.contains(&"unsafe_url_attribute"), "{codes:?}");

        // Defense-in-depth: even rendering expanded nodes strips forbidden tags/attrs/urls.
        let expanded = expand::expand_page_body(&doc, &doc.pages[0].body);
        let rendered = expand::render_expanded_html(&expanded.nodes);
        assert!(!rendered.contains("<script"), "{rendered}");
        assert!(!rendered.contains("<iframe"), "{rendered}");
        assert!(!rendered.contains("onclick"), "{rendered}");
        assert!(!rendered.contains("style="), "{rendered}");
        assert!(!rendered.contains("javascript:"), "{rendered}");
    }

    #[test]
    fn rejects_case_insensitive_output_path_collision() {
        // /about and /About produce distinct routes but collide on
        // case-insensitive filesystems; the compile must fail cleanly.
        let mut doc = base_doc();
        doc.pages.push(valid_page(
            "about",
            "/about",
            false,
            vec![elem("h1", BTreeMap::new(), vec![text("About Lower")])],
        ));
        doc.pages.push(valid_page(
            "about-upper",
            "/About",
            false,
            vec![elem("h1", BTreeMap::new(), vec![text("About Upper")])],
        ));
        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        let collision = err
            .diagnostics
            .iter()
            .find(|d| d.code == "output_path_collision")
            .expect("expected case-insensitive output_path_collision");
        assert!(
            collision.message.contains("about/index.html")
                && collision.message.contains("About/index.html"),
            "diagnostic must name both colliding paths: {}",
            collision.message
        );
    }

    #[test]
    fn still_rejects_exact_duplicate_output_paths() {
        // /404 and /404.html are distinct routes but both emit 404.html.
        let mut doc = base_doc();
        doc.pages.push(valid_page(
            "nf-route",
            "/404",
            false,
            vec![elem("h1", BTreeMap::new(), vec![text("NF Route")])],
        ));
        doc.pages.push(valid_page(
            "alt-404",
            "/404.html",
            false,
            vec![elem("h1", BTreeMap::new(), vec![text("Alt 404")])],
        ));
        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "output_path_collision"
                    && d.message.contains("duplicate compiled output path '404.html'")),
            "{:?}",
            err.diagnostics
        );
    }

    #[test]
    fn warns_on_unresolved_template_expression_and_drops_token() {
        let mut doc = base_doc();
        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
            && let Some(Node::Element { children, .. }) = children
                .iter_mut()
                .find(|n| matches!(n, Node::Element { tag, .. } if tag == "h1"))
        {
            children.push(text("Acme {{heelings}}"));
        }

        let compiled = compile_site(&doc, None, &CompileOptions::default())
            .expect("unresolved expression is a warning, not an error");
        let warning = compiled
            .diagnostics
            .iter()
            .find(|d| d.level == DiagnosticLevel::Warning && d.code == "unresolved_expression")
            .expect("expected unresolved_expression warning");
        assert!(warning.message.contains("heelings"), "{:?}", warning.message);
        assert!(warning.path.starts_with("pages["), "{:?}", warning.path);
        assert!(
            !compiled.files["index.html"].contains("heelings"),
            "unresolved token must still be dropped from output"
        );
        assert!(compiled.files["index.html"].contains("Acme"));
    }

    #[test]
    fn warns_on_unresolvable_loop_collection() {
        let mut doc = base_doc();
        doc.data.remove("items");
        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(Node::Loop {
                item_var: "it".to_string(),
                collection_expr: "missing_items".to_string(),
                body: vec![elem("li", BTreeMap::new(), vec![text("{{it}}")])],
            });
        }

        let compiled = compile_site(&doc, None, &CompileOptions::default())
            .expect("unresolvable loop is a warning, not an error");
        let warning = compiled
            .diagnostics
            .iter()
            .find(
                |d| d.level == DiagnosticLevel::Warning && d.code == "unresolvable_loop_collection",
            )
            .expect("expected unresolvable_loop_collection warning");
        assert!(
            warning.message.contains("missing_items"),
            "{:?}",
            warning.message
        );
        assert!(
            !compiled.files["index.html"].contains("<li>"),
            "loop with unresolvable collection renders nothing"
        );
    }

    #[test]
    fn warns_on_non_array_loop_collection() {
        let mut doc = base_doc();
        doc.data
            .insert("not_an_array".to_string(), json!("just a string"));
        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(Node::Loop {
                item_var: "it".to_string(),
                collection_expr: "not_an_array".to_string(),
                body: vec![elem("li", BTreeMap::new(), vec![text("{{it}}")])],
            });
        }

        let compiled = compile_site(&doc, None, &CompileOptions::default()).unwrap();
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|d| d.code == "unresolvable_loop_collection"
                    && d.message.contains("non-array")),
            "{:?}",
            compiled.diagnostics
        );
    }

    #[test]
    fn requires_contrast_pair_coverage_for_text_and_background_tokens() {
        // 1. text/bg tokens with no covering pair -> compile error.
        let mut doc = base_doc();
        doc.tokens.contrast_pairs.clear();
        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "contrast_pair_uncovered"),
            "{:?}",
            err.diagnostics
        );

        // 2. Restoring a covering pair compiles cleanly.
        doc.tokens.contrast_pairs.push(ContrastPair {
            foreground: "text".to_string(),
            background: "bg".to_string(),
            min_ratio: 4.5,
            usage: Some("body text".to_string()),
        });
        assert!(compile_site(&doc, None, &CompileOptions::default()).is_ok());

        // 3. Coverage is also required for the `foreground`/`background` spelling.
        let mut fg_bg_doc = base_doc();
        fg_bg_doc.tokens.colors.remove("bg");
        fg_bg_doc.tokens.colors.remove("text");
        fg_bg_doc
            .tokens
            .colors
            .insert("background".to_string(), "#FFFFFF".to_string());
        fg_bg_doc
            .tokens
            .colors
            .insert("foreground".to_string(), "#333333".to_string());
        fg_bg_doc.styles = Vec::new();
        let err = compile_site(&fg_bg_doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "contrast_pair_uncovered"
                    && d.message.contains("'foreground'/'background'")),
            "{:?}",
            err.diagnostics
        );
    }

    #[test]
    fn stops_exponential_component_expansion_dos() {
        let mut doc = base_doc();
        // Build a 20-level binary DAG: c0 -> 2*c1 -> 4*c2 -> ... -> 2^20 (>1M) nodes.
        for i in 0..20 {
            let name = format!("c{i}");
            let template = if i == 19 {
                vec![elem("span", BTreeMap::new(), vec![text("leaf")])]
            } else {
                let next = format!("c{}", i + 1);
                vec![
                    Node::ComponentInstance {
                        component: next.clone(),
                        props: BTreeMap::new(),
                        slots: BTreeMap::new(),
                    },
                    Node::ComponentInstance {
                        component: next,
                        props: BTreeMap::new(),
                        slots: BTreeMap::new(),
                    },
                ]
            };
            doc.components.insert(
                name.clone(),
                ComponentDef {
                    name,
                    props: BTreeMap::new(),
                    slots: Vec::new(),
                    template,
                },
            );
        }
        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(Node::ComponentInstance {
                component: "c0".to_string(),
                props: BTreeMap::new(),
                slots: BTreeMap::new(),
            });
        }

        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "expansion_limit_exceeded"),
            "expected expansion_limit_exceeded, got: {:?}",
            err.diagnostics
        );
    }

    #[test]
    fn rejects_traversal_static_route() {
        let mut doc = base_doc();
        doc.pages.push(valid_page(
            "evil",
            "/../../pwn",
            false,
            vec![elem("h1", BTreeMap::new(), vec![text("Evil")])],
        ));
        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics.iter().any(|d| d.code == "ir_invalid"),
            "{:?}",
            err.diagnostics
        );
        assert_eq!(output_path_for_page(&doc.pages[1]), None);
    }

    #[test]
    fn stops_slot_cloning_and_interpolation_billion_laughs_bombs() {
        // 1. Slot-cloning exponential bomb: 25 nested `dup` components doubling a slot -> 2^25 nodes.
        let mut slot_doc = base_doc();
        slot_doc.components.insert(
            "dup".to_string(),
            ComponentDef {
                name: "dup".to_string(),
                props: BTreeMap::new(),
                slots: vec!["default".to_string()],
                template: vec![
                    Node::SlotRef {
                        name: "default".to_string(),
                        fallback: Vec::new(),
                    },
                    Node::SlotRef {
                        name: "default".to_string(),
                        fallback: Vec::new(),
                    },
                ],
            },
        );
        let mut nested = vec![text("leaf")];
        for _ in 0..25 {
            nested = vec![Node::ComponentInstance {
                component: "dup".to_string(),
                props: BTreeMap::new(),
                slots: BTreeMap::from([("default".to_string(), nested)]),
            }];
        }
        if let Some(Node::Element { children, .. }) = slot_doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.extend(nested);
        }
        let err = compile_site(&slot_doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "expansion_limit_exceeded"),
            "expected slot-cloning bomb to trigger expansion_limit_exceeded, got: {:?}",
            err.diagnostics
        );

        // 2. String-interpolation Billion-Laughs bomb: 26-step prop doubling -> >500 MB string.
        let mut str_doc = base_doc();
        str_doc
            .data
            .insert("x".to_string(), json!("AAAAAAAAAAAAAAAA"));
        for i in 0..26 {
            let name = format!("s{i}");
            let template = if i == 25 {
                vec![text("{{x}}")]
            } else {
                vec![Node::ComponentInstance {
                    component: format!("s{}", i + 1),
                    props: BTreeMap::from([("x".to_string(), json!("{{x}}{{x}}"))]),
                    slots: BTreeMap::new(),
                }]
            };
            str_doc.components.insert(
                name.clone(),
                ComponentDef {
                    name,
                    props: BTreeMap::new(),
                    slots: Vec::new(),
                    template,
                },
            );
        }
        if let Some(Node::Element { children, .. }) = str_doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(Node::ComponentInstance {
                component: "s0".to_string(),
                props: BTreeMap::new(),
                slots: BTreeMap::new(),
            });
        }
        let err = compile_site(&str_doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "expansion_limit_exceeded"),
            "expected string-interpolation bomb to trigger expansion_limit_exceeded, got: {:?}",
            err.diagnostics
        );

        // 3. Empty-body huge loop terminates at MAX_EXPANDED_NODES.
        let mut loop_doc = base_doc();
        loop_doc
            .data
            .insert("many".to_string(), Value::Array(vec![json!(1); 60_000]));
        if let Some(Node::Element { children, .. }) = loop_doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(Node::Loop {
                item_var: "it".to_string(),
                collection_expr: "many".to_string(),
                body: Vec::new(),
            });
        }
        let err = compile_site(&loop_doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == "expansion_limit_exceeded"),
            "expected huge empty loop to trigger expansion_limit_exceeded, got: {:?}",
            err.diagnostics
        );
    }

    #[test]
    fn rejects_svg_smil_srcset_invalid_names_and_reserved_route_collisions() {
        let mut doc = base_doc();
        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(elem(
                "animate",
                BTreeMap::from([
                    ("attributeName".to_string(), "href".to_string()),
                    ("to".to_string(), "javascript:alert(1)".to_string()),
                ]),
                Vec::new(),
            ));
            children.push(elem(
                "img",
                BTreeMap::from([
                    ("src".to_string(), "/ok.png".to_string()),
                    ("alt".to_string(), "ok".to_string()),
                    (
                        "srcset".to_string(),
                        "/ok.png 1x, javascript:alert(1) 2x".to_string(),
                    ),
                ]),
                Vec::new(),
            ));
            children.push(elem("div><script>", BTreeMap::new(), Vec::new()));
            children.push(elem(
                "a",
                BTreeMap::from([
                    ("href".to_string(), "\u{0090}https://example.test".to_string()),
                    ("bad attr".to_string(), "1".to_string()),
                ]),
                vec![text("Bad")],
            ));
        }
        let err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        let codes: Vec<&str> = err.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert!(codes.contains(&"unsafe_html_element"), "{codes:?}");
        assert!(codes.contains(&"unsafe_html_attribute"), "{codes:?}");
        assert!(codes.contains(&"unsafe_url_attribute"), "{codes:?}");

        let expanded = expand::expand_page_body(&doc, &doc.pages[0].body);
        let rendered = expand::render_expanded_html(&expanded.nodes);
        assert!(!rendered.contains("<animate"), "{rendered}");
        assert!(!rendered.contains("srcset="), "{rendered}");
        assert!(!rendered.contains("javascript:"), "{rendered}");

        // Reserved output path collision: `/_headers` -> `_headers/index.html` vs `_headers`
        let mut col_doc = base_doc();
        col_doc.pages.push(valid_page(
            "hdr",
            "/_headers",
            false,
            vec![elem("h1", BTreeMap::new(), vec![text("Hdr")])],
        ));
        let col_err = compile_site(&col_doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            col_err
                .diagnostics
                .iter()
                .any(|d| d.code == "output_path_collision"),
            "{:?}",
            col_err.diagnostics
        );
    }

    #[test]
    fn enforces_wcag_large_or_ui_contrast_and_img_alt_diagnostics() {
        // 1. UI contrast pair (~3.26:1 on #F7F5F0) passes with usage="UI border", fails with usage="body text".
        let mut doc = base_doc();
        doc.tokens.colors.insert("ui_mid".to_string(), "#888888".to_string());
        doc.tokens.contrast_pairs.push(ContrastPair {
            foreground: "ui_mid".to_string(),
            background: "bg".to_string(),
            min_ratio: 3.0,
            usage: Some("UI border".to_string()),
        });
        assert!(
            compile_site(&doc, None, &CompileOptions::default()).is_ok(),
            "3.26:1 UI border contrast pair must pass with min_ratio 3.0"
        );

        doc.tokens.contrast_pairs.last_mut().unwrap().usage = Some("body text".to_string());
        let body_err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            body_err.diagnostics.iter().any(|d| d.code == "contrast_ratio_too_low"),
            "{:?}",
            body_err.diagnostics
        );

        doc.tokens.contrast_pairs.pop();

        // 2. Visible <img> missing alt fails; alt="" and aria-hidden="true" pass.
        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
        {
            children.push(elem(
                "img",
                BTreeMap::from([("src".to_string(), "/photo.webp".to_string())]),
                Vec::new(),
            ));
        }
        let img_err = compile_site(&doc, None, &CompileOptions::default()).unwrap_err();
        assert!(
            img_err.diagnostics.iter().any(|d| d.code == "img_missing_alt"),
            "{:?}",
            img_err.diagnostics
        );

        if let Some(Node::Element { children, .. }) = doc.pages[0]
            .body
            .iter_mut()
            .find(|n| matches!(n, Node::Element { tag, .. } if tag == "main"))
            && let Some(Node::Element { attrs, .. }) = children.last_mut()
        {
            attrs.insert("alt".to_string(), String::new());
        }
        assert!(
            compile_site(&doc, None, &CompileOptions::default()).is_ok(),
            "img with alt=\"\" must pass"
        );
    }
}
