//! Hospitality vertical plugin adapter (`SwsPlugin`) converting
//! [`VenueManifest`] JSON into a domain-agnostic [`SiteDocument`] and running
//! advisory Virginia ABC advertising lint.

use std::collections::BTreeMap;

use serde_json::Value;
use sws_ir::{
    Breakpoint, ContrastPair, DesignTokens, FontTokens, IrStyleRule, JsonLdBlock, Node, Page,
    RoutePattern, ScriptRef, SeoMeta, SiteDocument,
};
use sws_plugin::{
    DiagnosticSeverity, PluginDiagnostic, PluginError, SwsPlugin,
};

use crate::abc::{CITATIONS, lint_json_value};
use crate::venue::{
    DAYS, CheckoutConfig, EventsConfig, VenueManifest, build_json_ld, validate_venue,
};

const TURNSTILE_SRC: &str = "https://challenges.cloudflare.com/turnstile/v0/api.js";
const CHECKOUT_NOTICE: &str = "Demo — no real charge or fulfillment";

/// [`SwsPlugin`] implementation for the Solo Website System Hospitality vertical pack.
#[derive(Debug, Clone, Copy, Default)]
pub struct HospitalityPlugin;

impl SwsPlugin for HospitalityPlugin {
    fn id(&self) -> &'static str {
        "hospitality"
    }

    fn name(&self) -> &'static str {
        "SWS Hospitality Pack"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn can_transform_input(&self, input: &Value) -> bool {
        let Some(obj) = input.as_object() else {
            return false;
        };
        obj.contains_key("menu") && obj.contains_key("nap") && obj.contains_key("hours")
    }

    fn transform_input(&self, input: &Value) -> Result<SiteDocument, PluginError> {
        let venue: VenueManifest = serde_json::from_value(input.clone())
            .map_err(|e| PluginError::new(self.id(), format!("invalid venue manifest: {e}")))?;

        let errs = validate_venue(&venue);
        if !errs.is_empty() {
            return Err(PluginError::new(self.id(), errs.join("; ")));
        }

        if !sws_css::is_safe_font_stack(&venue.brand.fonts.body) {
            return Err(PluginError::new(
                self.id(),
                "brand.fonts.body failed CSS font stack safety check",
            ));
        }
        if let Some(heading) = &venue.brand.fonts.heading
            && !sws_css::is_safe_font_stack(heading)
        {
            return Err(PluginError::new(
                self.id(),
                "brand.fonts.heading failed CSS font stack safety check",
            ));
        }

        let mut doc = build_site_document(&venue);
        doc.metadata
            .insert("venue_manifest".to_string(), input.clone());
        Ok(doc)
    }

    fn lint(&self, doc: &SiteDocument, raw_input: Option<&Value>) -> Vec<PluginDiagnostic> {
        let Some(target) = raw_input.or_else(|| doc.metadata.get("venue_manifest")) else {
            return Vec::new();
        };

        lint_json_value(target)
            .into_iter()
            .map(|jf| PluginDiagnostic {
                plugin_id: self.id().to_string(),
                rule: jf.finding.rule.to_string(),
                severity: DiagnosticSeverity::Advisory,
                path: jf.path,
                message: jf.finding.message,
                citation: Some(CITATIONS.to_string()),
            })
            .collect()
    }
}

fn map_palette_token_key(key: &str) -> &str {
    match key {
        "background" => "bg",
        "accent_secondary" => "accent-2",
        "muted_text" => "muted",
        "on_accent" => "on-accent",
        "footer_background" => "footer-bg",
        "footer_text" => "footer-text",
        other => other,
    }
}

fn resolve_palette_tokens(pal: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let bg = pal.get("background").cloned().unwrap_or_default();
    let text = pal.get("text").cloned().unwrap_or_default();
    let accent = pal.get("accent").cloned().unwrap_or_default();

    let mut out = BTreeMap::new();
    out.insert("bg".to_string(), bg.clone());
    out.insert("text".to_string(), text.clone());
    out.insert(
        "surface".to_string(),
        pal.get("surface").cloned().unwrap_or_else(|| bg.clone()),
    );
    out.insert("accent".to_string(), accent.clone());
    out.insert(
        "accent-2".to_string(),
        pal.get("accent_secondary")
            .cloned()
            .unwrap_or_else(|| accent.clone()),
    );
    out.insert(
        "muted".to_string(),
        pal.get("muted_text").cloned().unwrap_or_else(|| text.clone()),
    );
    out.insert(
        "on-accent".to_string(),
        pal.get("on_accent").cloned().unwrap_or_else(|| bg.clone()),
    );
    out.insert(
        "footer-bg".to_string(),
        pal.get("footer_background")
            .cloned()
            .unwrap_or_else(|| text.clone()),
    );
    out.insert(
        "footer-text".to_string(),
        pal.get("footer_text").cloned().unwrap_or(bg),
    );
    out
}

fn build_site_document(v: &VenueManifest) -> SiteDocument {
    let base_url = v.nap.website.trim_end_matches('/').to_string();
    let colors = resolve_palette_tokens(&v.brand.palette);
    let contrast_pairs = v
        .brand
        .contrast
        .pairs
        .iter()
        .map(|p| ContrastPair {
            foreground: map_palette_token_key(&p.foreground).to_string(),
            background: map_palette_token_key(&p.background).to_string(),
            min_ratio: 4.5,
            usage: p.usage.clone(),
        })
        .collect();

    let tokens = DesignTokens {
        colors,
        fonts: FontTokens {
            body: v.brand.fonts.body.clone(),
            heading: Some(
                v.brand
                    .fonts
                    .heading
                    .clone()
                    .unwrap_or_else(|| v.brand.fonts.body.clone()),
            ),
            mono: None,
        },
        spacing: BTreeMap::new(),
        radii: BTreeMap::new(),
        contrast_pairs,
    };

    let breakpoints = vec![
        Breakpoint {
            id: "sm".to_string(),
            media_query: "(min-width: 40rem)".to_string(),
            min_width: Some("40rem".to_string()),
        },
        Breakpoint {
            id: "md".to_string(),
            media_query: "(min-width: 48rem)".to_string(),
            min_width: Some("48rem".to_string()),
        },
        Breakpoint {
            id: "print".to_string(),
            media_query: "print".to_string(),
            min_width: None,
        },
    ];

    let newsletter_enabled = v.newsletter.as_ref().is_some_and(|n| n.enabled);
    let mut index_scripts = Vec::new();
    if newsletter_enabled {
        index_scripts.push(ScriptRef {
            src: "/js/forms.js".to_string(),
            async_load: false,
            defer: true,
        });
        index_scripts.push(ScriptRef {
            src: TURNSTILE_SRC.to_string(),
            async_load: true,
            defer: true,
        });
    }

    // The demo checkout page shares forms.js (its cart-total enhancement is
    // bundled there so the site still works with JS disabled: the form posts
    // natively and Stripe shows the total before confirmation).
    let mut styles = build_style_rules();
    if v.checkout.is_some() {
        styles.extend(checkout_style_rules());
    }
    if bookings_enabled(v) {
        styles.extend(booking_style_rules());
    }

    let index_title = v
        .seo
        .as_ref()
        .and_then(|s| s.title.clone())
        .unwrap_or_else(|| v.name.clone());
    let index_desc = v
        .seo
        .as_ref()
        .and_then(|s| s.description.clone())
        .or_else(|| v.description.clone());

    let not_found_desc = {
        let extra = v
            .seo
            .as_ref()
            .and_then(|s| s.description.as_deref())
            .unwrap_or("");
        format!("Page not found. {extra}").trim().to_string()
    };

    let sitemap_routes = v
        .seo
        .as_ref()
        .map(|s| s.routes.clone())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| vec!["/".to_string()]);

    let events = v.scheduling.as_ref().and_then(|s| s.events.as_ref());
    let events_enabled = events.is_some_and(|e| e.enabled);
    let mut sitemap_routes = sitemap_routes;
    if events_enabled {
        // Deterministic sitemap: /events is published by this build, not the
        // manifest author's route list.
        sitemap_routes.retain(|r| !matches!(r.as_str(), "/events" | "/events/" | "/events.html"));
        sitemap_routes.push("/events".to_string());
    }
    if bookings_enabled(v) {
        sitemap_routes
            .retain(|r| !matches!(r.as_str(), "/book-a-call" | "/book-a-call/" | "/book-a-call.html"));
        sitemap_routes.push("/book-a-call".to_string());
    }

    let mut pages = vec![Page {
        id: "index".to_string(),
        route: RoutePattern::Static {
            path: "/".to_string(),
        },
        title: index_title,
        description: index_desc.clone(),
        canonical: Some(format!("{base_url}/")),
        noindex: false,
        is_not_found: false,
        json_ld: vec![JsonLdBlock {
            id: Some("venue".to_string()),
            data: build_json_ld(v),
        }],
        scripts: index_scripts,
        body: build_index_body(v),
    }];
    if let Some(events) = events.filter(|e| e.enabled) {
        pages.push(events_page(v, &base_url, events));
    }
    if v
        .scheduling
        .as_ref()
        .and_then(|s| s.bookings.as_ref())
        .is_some_and(|b| b.enabled)
    {
        pages.push(booking_page(v, &base_url));
    }    pages.push(Page {
        id: "not-found".to_string(),
        route: RoutePattern::Static {
            path: "/404".to_string(),
        },
        title: format!("Page not found — {}", v.name),
        description: Some(not_found_desc),
        canonical: None,
        noindex: true,
        is_not_found: true,
        json_ld: Vec::new(),
        scripts: Vec::new(),
        body: build_404_body(v),
    });
    if let Some(c) = &v.checkout {
        pages.push(Page {
            id: "takeout".to_string(),
            route: RoutePattern::Static {
                path: "/takeout".to_string(),
            },
            title: format!("Demo checkout — {}", v.name),
            description: Some(CHECKOUT_NOTICE.to_string()),
            canonical: Some(format!("{base_url}/takeout")),
            noindex: false,
            is_not_found: false,
            json_ld: Vec::new(),
            scripts: checkout_scripts(),
            body: build_takeout_body(v, c),
        });
    }

    SiteDocument {
        schema_version: 1,
        slug: v.slug.clone(),
        name: v.name.clone(),
        locale: v.locale.clone().unwrap_or_else(|| "en-US".to_string()),
        base_url: base_url.clone(),
        tokens,
        breakpoints,
        styles,
        components: BTreeMap::new(),
        forms: BTreeMap::new(),
        pages,
        seo: SeoMeta {
            title_template: None,
            default_description: index_desc,
            robots_allow: vec!["/".to_string()],
            robots_disallow: vec!["/api/".to_string()],
            sitemap_routes,
        },
        scripts: Vec::new(),
        data: BTreeMap::new(),
        metadata: BTreeMap::new(),
    }
}

// ---------- CSS Rules ----------

fn style_rule(
    id: &str,
    selector: &str,
    breakpoint: Option<&str>,
    decls: &[(&str, &str)],
) -> IrStyleRule {
    let mut declarations = BTreeMap::new();
    for (k, v) in decls {
        declarations.insert((*k).to_string(), (*v).to_string());
    }
    IrStyleRule {
        id: id.to_string(),
        selector: selector.to_string(),
        breakpoint: breakpoint.map(str::to_string),
        pseudo: None,
        declarations,
    }
}

fn build_style_rules() -> Vec<IrStyleRule> {
    vec![
        style_rule("box-sizing", "*, *::before, *::after", None, &[("box-sizing", "border-box")]),
        style_rule("html", "html", None, &[("text-size-adjust", "100%")]),
        style_rule(
            "body",
            "body",
            None,
            &[
                ("margin", "0"),
                ("background", "var(--bg)"),
                ("color", "var(--text)"),
                ("font-family", "var(--font-body)"),
                ("font-size", "1.0625rem"),
                ("line-height", "1.6"),
            ],
        ),
        style_rule(
            "headings-font",
            "h1, h2, h3, .brand, .menu-title, .footer-title",
            None,
            &[("font-family", "var(--font-heading)"), ("line-height", "1.2")],
        ),
        style_rule(
            "h1",
            "h1",
            None,
            &[("font-size", "clamp(2rem, 5vw, 3rem)"), ("margin", "0 0 .5rem")],
        ),
        style_rule("h2", "h2", None, &[("font-size", "1.6rem"), ("margin", "0 0 1rem")]),
        style_rule("h3", "h3", None, &[("font-size", "1.2rem"), ("margin", "1.5rem 0 .5rem")]),
        style_rule(
            "link",
            "a",
            None,
            &[
                ("color", "var(--accent)"),
                ("text-decoration-thickness", ".08em"),
                ("text-underline-offset", ".18em"),
            ],
        ),
        style_rule(
            "link-hover",
            "a:hover",
            None,
            &[("text-decoration-thickness", ".16em")],
        ),
        style_rule(
            "focus-visible",
            ":focus-visible",
            None,
            &[("outline", "3px solid var(--text)"), ("outline-offset", "3px")],
        ),
        style_rule(
            "footer-focus-visible",
            ".site-footer :focus-visible",
            None,
            &[("outline-color", "var(--footer-text)")],
        ),
        style_rule("main-focus", "main:focus", None, &[("outline", "none")]),
        style_rule(
            "wrap",
            ".wrap",
            None,
            &[("max-width", "60rem"), ("margin", "0 auto"), ("padding", "0 1.25rem")],
        ),
        style_rule(
            "skip-link",
            ".skip-link",
            None,
            &[
                ("position", "absolute"),
                ("left", "1rem"),
                ("top", "-100rem"),
                ("z-index", "10"),
                ("padding", ".75rem 1rem"),
                ("background", "var(--bg)"),
                ("color", "var(--text)"),
                ("border", "2px solid var(--text)"),
                ("font-weight", "700"),
            ],
        ),
        style_rule(
            "skip-link-focus",
            ".skip-link:focus, .skip-link:focus-visible",
            None,
            &[
                ("top", "1rem"),
                ("outline", "3px solid var(--bg)"),
                ("outline-offset", "2px"),
                ("box-shadow", "0 0 0 7px var(--text)"),
            ],
        ),
        style_rule(
            "notice",
            ".notice",
            None,
            &[
                ("margin", "0"),
                ("padding", ".5rem 1.25rem"),
                ("background", "var(--text)"),
                ("color", "var(--bg)"),
                ("text-align", "center"),
                ("font-size", ".95rem"),
            ],
        ),
        style_rule(
            "site-header",
            ".site-header",
            None,
            &[("border-bottom", "1px solid var(--muted)")],
        ),
        style_rule(
            "header-row",
            ".header-row",
            None,
            &[
                ("display", "flex"),
                ("flex-wrap", "wrap"),
                ("align-items", "center"),
                ("justify-content", "space-between"),
                ("gap", ".5rem 1.5rem"),
                ("padding-top", ".75rem"),
                ("padding-bottom", ".75rem"),
            ],
        ),
        style_rule(
            "brand",
            ".brand",
            None,
            &[("margin", "0"), ("font-size", "1.5rem"), ("font-weight", "700")],
        ),
        style_rule(
            "brand-link",
            ".brand a",
            None,
            &[("color", "var(--text)"), ("text-decoration", "none")],
        ),
        style_rule(
            "nav-ul",
            "nav ul",
            None,
            &[
                ("list-style", "none"),
                ("margin", "0"),
                ("padding", "0"),
                ("display", "flex"),
                ("flex-wrap", "wrap"),
                ("gap", ".25rem 1rem"),
            ],
        ),
        style_rule(
            "nav-a",
            "nav a",
            None,
            &[
                ("display", "inline-block"),
                ("min-height", "44px"),
                ("padding", ".6rem .25rem"),
            ],
        ),
        style_rule("intro", ".intro", None, &[("padding", "2.5rem 0 1rem")]),
        style_rule(
            "tagline",
            ".tagline",
            None,
            &[("font-size", "1.25rem"), ("color", "var(--accent)"), ("margin-top", "0")],
        ),
        style_rule(
            "small-hint",
            ".small, .hint",
            None,
            &[("font-size", ".9rem"), ("color", "var(--muted)")],
        ),
        style_rule(
            "card",
            ".card",
            None,
            &[
                ("background", "var(--surface)"),
                ("border", "1px solid var(--muted)"),
                ("border-radius", ".5rem"),
                ("padding", "1.5rem"),
                ("margin", "1.5rem 0"),
            ],
        ),
        style_rule(
            "address",
            "address",
            None,
            &[("font-style", "normal"), ("margin-bottom", "1rem")],
        ),
        style_rule(
            "address-a",
            "address a",
            None,
            &[
                ("display", "inline-block"),
                ("min-height", "24px"),
                ("padding", ".25rem 0"),
            ],
        ),
        style_rule("nap-name", ".nap-name", None, &[("font-weight", "700")]),
        style_rule(
            "hours-table",
            "table.hours",
            None,
            &[
                ("border-collapse", "collapse"),
                ("width", "100%"),
                ("max-width", "28rem"),
            ],
        ),
        style_rule(
            "hours-caption",
            ".hours caption",
            None,
            &[
                ("text-align", "left"),
                ("font-weight", "700"),
                ("padding-bottom", ".5rem"),
            ],
        ),
        style_rule(
            "hours-cells",
            ".hours th, .hours td",
            None,
            &[
                ("text-align", "left"),
                ("padding", ".4rem .5rem"),
                ("border-bottom", "1px solid var(--muted)"),
            ],
        ),
        style_rule(
            "hh-when",
            ".hh-when",
            None,
            &[("font-weight", "700"), ("color", "var(--accent-2)")],
        ),
        style_rule("menu-h2", ".menu > h2", None, &[("margin", "2rem 0 1rem")]),
        style_rule(
            "section-desc",
            ".section-desc",
            None,
            &[("color", "var(--muted)"), ("margin", "0 0 1rem")],
        ),
        style_rule(
            "menu-items",
            ".menu-items",
            None,
            &[
                ("list-style", "none"),
                ("margin", "0"),
                ("padding", "0"),
                ("display", "grid"),
                ("gap", "1rem"),
            ],
        ),
        style_rule(
            "menu-items-sm",
            ".menu-items",
            Some("sm"),
            &[("grid-template-columns", "1fr 1fr")],
        ),
        style_rule("menu-item-p", ".menu-item p", None, &[("margin", "0")]),
        style_rule(
            "item-head",
            ".item-head",
            None,
            &[
                ("display", "flex"),
                ("justify-content", "space-between"),
                ("gap", "1rem"),
                ("font-weight", "700"),
            ],
        ),
        style_rule(
            "price",
            ".price",
            None,
            &[("color", "var(--accent)"), ("white-space", "nowrap")],
        ),
        style_rule("item-desc", ".item-desc", None, &[("color", "var(--muted)")]),
        style_rule(
            "tags",
            ".tags",
            None,
            &[("font-size", ".85rem"), ("color", "var(--accent-2)")],
        ),
        style_rule(
            "site-footer",
            ".site-footer",
            None,
            &[
                ("background", "var(--footer-bg)"),
                ("color", "var(--footer-text)"),
                ("margin-top", "3rem"),
                ("padding", "2.5rem 0"),
            ],
        ),
        style_rule(
            "site-footer-text",
            ".site-footer a, .site-footer .small, .site-footer .hint",
            None,
            &[("color", "var(--footer-text)")],
        ),
        style_rule(
            "footer-grid",
            ".footer-grid",
            None,
            &[("display", "grid"), ("gap", "2rem")],
        ),
        style_rule(
            "footer-grid-md",
            ".footer-grid",
            Some("md"),
            &[("grid-template-columns", "3fr 2fr")],
        ),
        style_rule(
            "footer-title",
            ".footer-title",
            None,
            &[
                ("font-size", "1.3rem"),
                ("font-weight", "700"),
                ("margin", "0 0 .75rem"),
            ],
        ),
        style_rule(
            "newsletter-h2",
            ".newsletter h2",
            None,
            &[("font-size", "1.3rem")],
        ),
        style_rule("field", ".field", None, &[("margin", "0 0 1rem")]),
        style_rule(
            "field-label",
            ".field label",
            None,
            &[
                ("display", "block"),
                ("font-weight", "700"),
                ("margin-bottom", ".25rem"),
            ],
        ),
        style_rule("field-hint", ".field .hint", None, &[("margin", ".25rem 0 0")]),
        style_rule(
            "inputs",
            r#"input[type="email"], input[type="text"], input[type="tel"], input[type="date"], input[type="number"], textarea, select"#,
            None,
            &[
                ("width", "100%"),
                ("max-width", "26rem"),
                ("min-height", "44px"),
                ("padding", ".5rem .75rem"),
                ("font", "inherit"),
                ("color", "var(--text)"),
                ("background", "var(--surface)"),
                ("border", "2px solid var(--muted)"),
                ("border-radius", ".25rem"),
            ],
        ),
        style_rule(
            "inputs-invalid",
            r#"input[aria-invalid="true"], textarea[aria-invalid="true"], select[aria-invalid="true"]"#,
            None,
            &[("box-shadow", "inset 0 0 0 2px currentColor")],
        ),
        style_rule(
            "footer-inputs",
            r#".site-footer input[type="email"], .site-footer input[type="text"], .site-footer input[type="tel"], .site-footer input[type="date"], .site-footer input[type="number"], .site-footer textarea, .site-footer select"#,
            None,
            &[("border-color", "var(--footer-text)")],
        ),
        style_rule(
            "checkbox",
            ".checkbox",
            None,
            &[
                ("display", "flex"),
                ("flex-wrap", "wrap"),
                ("align-items", "flex-start"),
                ("gap", ".75rem"),
            ],
        ),
        style_rule(
            "checkbox-input",
            ".checkbox input",
            None,
            &[
                ("flex", "none"),
                ("width", "24px"),
                ("height", "24px"),
                ("margin", ".15rem 0 0"),
                ("accent-color", "var(--accent)"),
            ],
        ),
        style_rule(
            "footer-checkbox-input",
            ".site-footer .checkbox input",
            None,
            &[("accent-color", "var(--footer-text)")],
        ),
        style_rule(
            "checkbox-label",
            ".checkbox label",
            None,
            &[
                ("flex", "1 1 0"),
                ("min-width", "0"),
                ("font-weight", "400"),
                ("margin", "0"),
            ],
        ),
        style_rule(
            "field-error",
            ".field-error",
            None,
            &[("margin", ".25rem 0 0"), ("font-size", ".9rem"), ("font-weight", "700")],
        ),
        style_rule(
            "checkbox-field-error",
            ".checkbox .field-error",
            None,
            &[("flex", "1 0 100%"), ("margin", "0")],
        ),
        style_rule(
            "button",
            "button",
            None,
            &[
                ("min-height", "44px"),
                ("min-width", "44px"),
                ("padding", ".6rem 1.25rem"),
                ("font", "inherit"),
                ("font-weight", "700"),
                ("color", "var(--on-accent)"),
                ("background", "var(--accent)"),
                ("border", "2px solid var(--accent)"),
                ("border-radius", ".25rem"),
                ("cursor", "pointer"),
            ],
        ),
        style_rule(
            "footer-button",
            ".site-footer button",
            None,
            &[
                ("color", "var(--footer-bg)"),
                ("background", "var(--footer-text)"),
                ("border-color", "var(--footer-text)"),
            ],
        ),
        style_rule(
            "button-disabled",
            "button[disabled]",
            None,
            &[("cursor", "progress")],
        ),
        style_rule(
            "turnstile",
            ".cf-turnstile",
            None,
            &[("margin", "0 0 1rem"), ("min-height", "65px")],
        ),
        style_rule(
            "form-status",
            ".form-status",
            None,
            &[("min-height", "1.6em"), ("font-weight", "700")],
        ),
        style_rule(
            "hp",
            ".hp",
            None,
            &[
                ("position", "absolute"),
                ("left", "-10000px"),
                ("width", "1px"),
                ("height", "1px"),
                ("overflow", "hidden"),
            ],
        ),
        style_rule(
            "print-hide",
            ".site-footer form, nav, .skip-link",
            Some("print"),
            &[("display", "none")],
        ),
        style_rule(
            "print-body",
            "body",
            Some("print"),
            &[("background", "#fff"), ("color", "#000")],
        ),
    ]
}

// ---------- Node Builders ----------

// ---------- Sandbox demo checkout rendering ----------

/// Scripts for the `/takeout` demo checkout page.
fn checkout_scripts() -> Vec<ScriptRef> {
    vec![
        ScriptRef {
            src: "/js/forms.js".to_string(),
            async_load: false,
            defer: true,
        },
        ScriptRef {
            src: TURNSTILE_SRC.to_string(),
            async_load: true,
            defer: true,
        },
    ]
}

fn checkout_style_rules() -> Vec<IrStyleRule> {
    vec![
        style_rule("checkout-main", ".checkout-main", None, &[("padding-top", "2rem")]),
        style_rule(
            "checkout-quantity",
            ".checkout-main input[type=\"number\"]",
            None,
            &[
                ("display", "block"),
                ("width", "5rem"),
                ("min-height", "44px"),
                ("padding", ".25rem .5rem"),
                ("font", "inherit"),
                ("color", "var(--text)"),
                ("background", "var(--surface)"),
                ("border", "2px solid var(--text)"),
                ("border-radius", ".25rem"),
            ],
        ),
        style_rule(
            "checkout-button",
            ".checkout-main button",
            None,
            &[("border-color", "var(--text)")],
        ),
    ]
}

fn usd(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn turnstile_widget(site_key: &str) -> Node {
    el(
        "div",
        &["cf-turnstile"],
        &[("data-sitekey", site_key), ("data-action", "demo_checkout")],
        Vec::new(),
    )
}

fn build_takeout_body(v: &VenueManifest, c: &CheckoutConfig) -> Vec<Node> {
    let site_key = v
        .integrations
        .turnstile
        .as_ref()
        .map(|t| t.site_key.as_str())
        .unwrap_or("");

    let item_fields: Vec<Node> = c
        .items
        .iter()
        .map(|it| {
            el(
                "p",
                &[],
                &[],
                vec![
                    el(
                        "label",
                        &[],
                        &[("for", &format!("qty-{}", it.id))],
                        vec![txt(format!(
                            "{} ({} each) — quantity",
                            it.name,
                            usd(it.cents)
                        ))],
                    ),
                    el(
                        "input",
                        &[],
                        &[
                            ("id", &format!("qty-{}", it.id)),
                            ("name", &format!("qty-{}", it.id)),
                            ("type", "number"),
                            ("min", "0"),
                            ("max", "5"),
                            ("step", "1"),
                            ("value", "0"),
                            ("required", ""),
                            ("data-cents", &it.cents.to_string()),
                        ],
                        Vec::new(),
                    ),
                ],
            )
        })
        .collect();

    let mut payment_children = vec![
        el("input", &[], &[("type", "hidden"), ("name", "mode"), ("value", "payment")], Vec::new()),
    ];
    payment_children.extend(item_fields);
    payment_children.extend([
        el(
            "p",
            &[],
            &[("hidden", "")],
            vec![
                txt("Cart total: "),
                el(
                    "output",
                    &[],
                    &[("data-cart-total", "")],
                    vec![txt("$0.00")],
                ),
                txt(" USD"),
            ],
        ),
        el(
            "p",
            &[],
            &[],
            vec![txt(
                "Without the cart script, Stripe displays the total before you confirm.",
            )],
        ),
        turnstile_widget(site_key),
        el(
            "button",
            &[],
            &[("type", "submit")],
            vec![txt("Continue to Stripe for pickup")],
        ),
    ]);

    let subscription_children = vec![
        el("input", &[], &[("type", "hidden"), ("name", "mode"), ("value", "subscription")], Vec::new()),
        turnstile_widget(site_key),
        el(
            "button",
            &[],
            &[("type", "submit")],
            vec![txt("Continue to Stripe for monthly pickup")],
        ),
    ];

    let mut nodes = site_header_nodes(v);
    nodes.push(el(
        "main",
        &["wrap", "checkout-main"],
        &[("id", "main"), ("tabindex", "-1")],
        vec![
            el("h1", &[], &[], vec![txt("Demo checkout")]),
            el(
                "p",
                &[],
                &[("id", "checkout-instructions")],
                vec![
                    el("strong", &[], &[], vec![txt(format!("{CHECKOUT_NOTICE}."))]),
                    txt(" Use only fictional information and Stripe’s test card "),
                    el("strong", &[], &[], vec![txt("4242 4242 4242 4242")]),
                    txt(", any future expiry and any three-digit CVC. Do not enter real payment or personal information. Checkout opens on Stripe after the security check."),
                ],
            ),
            el(
                "noscript",
                &[],
                &[],
                vec![el(
                    "p",
                    &[],
                    &[],
                    vec![txt(
                        "The required Turnstile security check needs browser JavaScript. Enable it to continue.",
                    )],
                )],
            ),
            el(
                "section",
                &["card"],
                &[("aria-labelledby", "pickup-heading")],
                vec![
                    el("h2", &[], &[("id", "pickup-heading")], vec![txt("Takeout pickup cart")]),
                    el(
                        "p",
                        &[],
                        &[],
                        vec![
                            el("strong", &[], &[], vec![txt(format!("{CHECKOUT_NOTICE}."))]),
                            txt(" USD. Set quantity to zero to remove an item. Limit: 5 per item, 10 items and $100 per cart."),
                        ],
                    ),
                    el(
                        "form",
                        &[],
                        &[
                            ("method", "post"),
                            ("action", "/api/checkout"),
                            ("data-demo-checkout", ""),
                            ("aria-describedby", "checkout-instructions"),
                        ],
                        payment_children,
                    ),
                ],
            ),
            el(
                "section",
                &["card"],
                &[("aria-labelledby", "subscription-heading")],
                vec![
                    el(
                        "h2",
                        &[],
                        &[("id", "subscription-heading")],
                        vec![txt("Monthly pickup subscription")],
                    ),
                    el(
                        "p",
                        &[],
                        &[],
                        vec![
                            el("strong", &[], &[], vec![txt(format!("{CHECKOUT_NOTICE}."))]),
                            txt(format!(
                                " {} — {} USD per month for one pickup. This starts a separate test subscription, not a takeout order.",
                                c.subscription.name,
                                usd(c.subscription.cents)
                            )),
                        ],
                    ),
                    el(
                        "form",
                        &[],
                        &[
                            ("method", "post"),
                            ("action", "/api/checkout"),
                            ("data-demo-checkout", ""),
                            ("aria-describedby", "checkout-instructions"),
                        ],
                        subscription_children,
                    ),
                ],
            ),
            el(
                "section",
                &[],
                &[("id", "returned"), ("aria-labelledby", "returned-heading")],
                vec![
                    el(
                        "h2",
                        &[],
                        &[("id", "returned-heading")],
                        vec![txt("Returning from Stripe?")],
                    ),
                    el(
                        "p",
                        &[],
                        &[],
                        vec![txt(
                            "This demo does not verify or fulfill orders. If you canceled, you can start over above.",
                        )],
                    ),
                ],
            ),
        ],
    ));
    nodes.push(site_footer_node(v, false));
    nodes
}

/// Public, read-only sandbox catalog (item keys and cents only — never Stripe
/// configuration). The Worker validates every posted amount against it.
pub fn checkout_catalog_json(v: &VenueManifest) -> Option<String> {
    let c = v.checkout.as_ref()?;
    let mut items = serde_json::Map::new();
    for it in &c.items {
        items.insert(it.id.clone(), serde_json::json!(it.cents));
    }
    Some(format!(
        "{}\n",
        serde_json::json!({
            "items": Value::Object(items),
            "subscription": {"id": c.subscription.id, "cents": c.subscription.cents},
        })
    ))
}

/// Additional static asset files the pack contributes for this manifest
/// (currently only the sandbox demo checkout catalog). Empty for venues whose
/// manifest has no `checkout` configuration.
pub fn checkout_extra_files(v: &VenueManifest) -> BTreeMap<String, String> {
    checkout_catalog_json(v)
        .map(|json| ("demo-checkout-catalog.json".to_string(), json))
        .into_iter()
        .collect()
}

fn txt(s: impl Into<String>) -> Node {
    Node::Text { value: s.into() }
}

fn el(tag: &str, classes: &[&str], attrs: &[(&str, &str)], children: Vec<Node>) -> Node {
    let mut attr_map = BTreeMap::new();
    for (k, v) in attrs {
        attr_map.insert((*k).to_string(), (*v).to_string());
    }
    Node::Element {
        id: None,
        tag: tag.to_string(),
        attrs: attr_map,
        classes: classes.iter().map(|c| (*c).to_string()).collect(),
        style_refs: Vec::new(),
        children,
    }
}

fn br() -> Node {
    el("br", &[], &[], Vec::new())
}

fn cap_ascii(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn time_12(hhmm: &str) -> String {
    let Some((h_str, m_str)) = hhmm.split_once(':') else {
        return hhmm.to_string();
    };
    let Ok(h) = h_str.parse::<u8>() else {
        return hhmm.to_string();
    };
    let suffix = if h < 12 { "AM" } else { "PM" };
    let h12 = if h.is_multiple_of(12) { 12 } else { h % 12 };
    format!("{h12}:{m_str} {suffix}")
}

fn time_node(hhmm: &str) -> Node {
    el(
        "time",
        &[],
        &[("datetime", hhmm)],
        vec![txt(time_12(hhmm))],
    )
}

fn day_ranges(days: &[String]) -> String {
    let mut indices: Vec<usize> = days
        .iter()
        .filter_map(|d| DAYS.iter().position(|known| known == d))
        .collect();
    indices.sort_unstable();
    indices.dedup();

    let mut out = Vec::new();
    let mut i = 0;
    while i < indices.len() {
        let mut j = i;
        while j + 1 < indices.len() && indices[j + 1] == indices[j] + 1 {
            j += 1;
        }
        if i == j {
            out.push(cap_ascii(DAYS[indices[i]]));
        } else {
            out.push(format!(
                "{}–{}",
                cap_ascii(DAYS[indices[i]]),
                cap_ascii(DAYS[indices[j]])
            ));
        }
        i = j + 1;
    }
    out.join(", ")
}

fn phone_parts(phone: &str) -> (String, String) {
    let digits: String = phone
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    let href = format!("tel:{digits}");

    let parts: Vec<&str> = phone.split('-').collect();
    let display = if parts.len() == 4
        && parts[0] == "+1"
        && parts[1].len() == 3
        && parts[2].len() == 3
        && parts[3].len() == 4
        && parts[1..].iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()))
    {
        format!("({}) {}-{}", parts[1], parts[2], parts[3])
    } else {
        phone.to_string()
    };

    (href, display)
}

fn diet_label(tag: &str) -> &str {
    match tag {
        "vegan" => "Vegan",
        "vegetarian" => "Vegetarian",
        "gluten-free" => "Gluten-free",
        "dairy-free" => "Dairy-free",
        "halal" => "Halal",
        "kosher" => "Kosher",
        "low-salt" => "Low-salt",
        other => other,
    }
}

fn format_price(price: &str, currency: &str) -> String {
    if currency == "USD" {
        format!("${price}")
    } else {
        format!("{price} {currency}")
    }
}

fn address_node(v: &VenueManifest) -> Node {
    let a = &v.nap.address;
    let (tel_href, tel_display) = phone_parts(&v.nap.phone);
    let mailto = format!("mailto:{}", v.nap.email);

    el(
        "address",
        &[],
        &[],
        vec![
            el("span", &["nap-name"], &[], vec![txt(&v.nap.name)]),
            br(),
            txt(&a.street),
            br(),
            txt(format!("{}, {} {}", a.locality, a.region, a.postal_code)),
            br(),
            el("a", &[], &[("href", &tel_href)], vec![txt(tel_display)]),
            br(),
            el("a", &[], &[("href", &mailto)], vec![txt(&v.nap.email)]),
        ],
    )
}

fn site_header_nodes(v: &VenueManifest) -> Vec<Node> {
    let mut header_children = Vec::new();

    if v.fictional == Some(true) {
        let notice_children = if let Some(legal) = &v.legal_disclaimer {
            vec![
                el("strong", &[], &[], vec![txt(format!("{legal}."))]),
                txt(" This site is a design demonstration; the business, address, phone number and menu are invented."),
            ]
        } else {
            vec![txt(
                "This site is a design demonstration; the business, address, phone number and menu are invented.",
            )]
        };
        header_children.push(el("p", &["notice"], &[], notice_children));
    } else if let Some(legal) = &v.legal_disclaimer {
        header_children.push(el("p", &["notice"], &[], vec![txt(legal)]));
    }

    let mut nav_items = vec![el(
        "li",
        &[],
        &[],
        vec![el("a", &[], &[("href", "/#menu")], vec![txt("Menu")])],
    )];
    if v.checkout.is_some() {
        nav_items.push(el(
            "li",
            &[],
            &[],
            vec![el("a", &[], &[("href", "/takeout")], vec![txt("Demo checkout")])],
        ));
    }
    if v.happy_hour.is_some() {
        nav_items.push(el(
            "li",
            &[],
            &[],
            vec![el(
                "a",
                &[],
                &[("href", "/#happy-hour")],
                vec![txt("Happy hour")],
            )],
        ));
    }
    if events_enabled(v) {
        nav_items.push(el(
            "li",
            &[],
            &[],
            vec![el("a", &[], &[("href", "/events")], vec![txt("Events")])],
        ));
    }
    if bookings_enabled(v) {
        nav_items.push(el(
            "li",
            &[],
            &[],
            vec![el("a", &[], &[("href", "/book-a-call")], vec![txt("Book a call")])],
        ));
    }
    nav_items.push(el(
        "li",
        &[],
        &[],
        vec![el(
            "a",
            &[],
            &[("href", "/#visit")],
            vec![txt("Hours & location")],
        )],
    ));
    if v.newsletter.as_ref().is_some_and(|n| n.enabled) {
        nav_items.push(el(
            "li",
            &[],
            &[],
            vec![el(
                "a",
                &[],
                &[("href", "/#newsletter")],
                vec![txt("Newsletter")],
            )],
        ));
    }

    header_children.push(el(
        "div",
        &["wrap", "header-row"],
        &[],
        vec![
            el(
                "p",
                &["brand"],
                &[],
                vec![el("a", &[], &[("href", "/")], vec![txt(&v.name)])],
            ),
            el(
                "nav",
                &[],
                &[("aria-label", "Primary")],
                vec![el("ul", &[], &[], nav_items)],
            ),
        ],
    ));

    vec![
        el(
            "a",
            &["skip-link"],
            &[("href", "#main")],
            vec![txt("Skip to main content")],
        ),
        el("header", &["site-header"], &[], header_children),
    ]
}

fn hours_table_node(v: &VenueManifest) -> Node {
    let rows: Vec<Node> = DAYS
        .iter()
        .filter_map(|d| {
            let h = v.hours.get(*d)?;
            let cell_children = if h.closed == Some(true) {
                vec![txt("Closed")]
            } else {
                vec![
                    time_node(h.opens.as_deref().unwrap_or("")),
                    txt(" – "),
                    time_node(h.closes.as_deref().unwrap_or("")),
                ]
            };
            Some(el(
                "tr",
                &[],
                &[],
                vec![
                    el("th", &[], &[("scope", "row")], vec![txt(cap_ascii(d))]),
                    el("td", &[], &[], cell_children),
                ],
            ))
        })
        .collect();

    el(
        "table",
        &["hours"],
        &[],
        vec![
            el("caption", &[], &[], vec![txt("Regular opening hours")]),
            el(
                "thead",
                &[],
                &[],
                vec![el(
                    "tr",
                    &[],
                    &[],
                    vec![
                        el("th", &[], &[("scope", "col")], vec![txt("Day")]),
                        el("th", &[], &[("scope", "col")], vec![txt("Hours")]),
                    ],
                )],
            ),
            el("tbody", &[], &[], rows),
        ],
    )
}

fn holiday_nodes(v: &VenueManifest) -> Vec<Node> {
    if v.holiday_overrides.is_empty() {
        return Vec::new();
    }
    let items: Vec<Node> = v
        .holiday_overrides
        .iter()
        .map(|o| {
            let mut children = vec![
                el("time", &[], &[("datetime", &o.date)], vec![txt(&o.label)]),
                txt(": "),
            ];
            if o.closed == Some(true) {
                children.push(txt("Closed"));
            } else {
                children.push(time_node(o.opens.as_deref().unwrap_or("")));
                children.push(txt(" – "));
                children.push(time_node(o.closes.as_deref().unwrap_or("")));
            }
            el("li", &[], &[], children)
        })
        .collect();

    vec![
        el("h3", &[], &[], vec![txt("Holiday hours")]),
        el("ul", &[], &[], items),
    ]
}

fn seating_paragraph(v: &VenueManifest) -> Node {
    let children = if v.integrations.seating_policy == "walk_in_only" {
        vec![txt(
            "We are walk-in only. No reservations are needed; just come by.",
        )]
    } else if let Some(url) = &v.booking_url {
        vec![
            el(
                "a",
                &[],
                &[("href", url)],
                vec![txt("Reserve a table online")],
            ),
            txt("."),
        ]
    } else {
        vec![txt("Please call us to reserve a table.")]
    };
    el("p", &[], &[], children)
}

fn happy_hour_node(v: &VenueManifest) -> Option<Node> {
    let hh = v.happy_hour.as_ref()?;
    let offers: Vec<Node> = hh
        .offers
        .iter()
        .map(|o| el("li", &[], &[], vec![txt(o)]))
        .collect();

    let mut children = vec![
        el(
            "h2",
            &[],
            &[("id", "happy-hour-heading")],
            vec![txt("Happy hour")],
        ),
        el(
            "p",
            &["hh-when"],
            &[],
            vec![
                txt(format!("{}, ", day_ranges(&hh.days))),
                time_node(&hh.start),
                txt(" – "),
                time_node(&hh.end),
            ],
        ),
        el("ul", &[], &[], offers),
    ];

    if let Some(notes) = &hh.notes {
        children.push(el("p", &["small"], &[], vec![txt(notes)]));
    }

    Some(el(
        "section",
        &["happy-hour", "card"],
        &[
            ("id", "happy-hour"),
            ("aria-labelledby", "happy-hour-heading"),
        ],
        children,
    ))
}

fn menu_section_nodes(v: &VenueManifest) -> Vec<Node> {
    v.menu
        .sections
        .iter()
        .map(|s| {
            let heading_id = format!("menu-{}", s.id);
            let mut sec_children = vec![el(
                "h3",
                &[],
                &[("id", &heading_id)],
                vec![txt(&s.name)],
            )];
            if let Some(desc) = &s.description {
                sec_children.push(el("p", &["section-desc"], &[], vec![txt(desc)]));
            }

            let items: Vec<Node> = s
                .items
                .iter()
                .map(|it| {
                    let mut head_children = vec![el(
                        "span",
                        &["item-name"],
                        &[],
                        vec![txt(&it.name)],
                    )];

                    let price_text = match (&it.price, &it.price_note) {
                        (Some(p), Some(note)) => {
                            format!("{} ({note})", format_price(p, &v.menu.currency))
                        }
                        (Some(p), None) => format_price(p, &v.menu.currency),
                        (None, Some(note)) => note.clone(),
                        (None, None) => String::new(),
                    };
                    if !price_text.is_empty() {
                        head_children.push(txt(" "));
                        head_children.push(el("span", &["price"], &[], vec![txt(price_text)]));
                    }

                    let mut item_children = vec![el("p", &["item-head"], &[], head_children)];
                    if let Some(desc) = &it.description {
                        item_children.push(el("p", &["item-desc"], &[], vec![txt(desc)]));
                    }
                    if !it.dietary.is_empty() {
                        let tags: Vec<&str> =
                            it.dietary.iter().map(|t| diet_label(t)).collect();
                        item_children.push(el("p", &["tags"], &[], vec![txt(tags.join(", "))]));
                    }
                    el("li", &["menu-item"], &[], item_children)
                })
                .collect();

            sec_children.push(el("ul", &["menu-items"], &[], items));
            el(
                "section",
                &["menu-section", "card"],
                &[("aria-labelledby", &heading_id)],
                sec_children,
            )
        })
        .collect()
}

/// Accessible `<noscript>` notice for a Turnstile-protected form (issue #32):
/// with JavaScript disabled the mandatory Turnstile verification cannot run
/// (submissions fail closed with `400 turnstile_required`), so no-JS visitors
/// get a plain statement plus `mailto:`/`tel:` alternative contact paths.
fn noscript_notice_node(email: Option<&str>, phone: Option<&str>) -> Node {
    let email = email.map(str::trim).filter(|e| !e.is_empty());
    let phone = phone.map(str::trim).filter(|p| !p.is_empty());

    let mut paragraph = vec![txt(
        "This form requires JavaScript for spam verification. Without it, submission is disabled.",
    )];
    match (email, phone) {
        (Some(email), Some(phone)) => {
            let mailto = format!("mailto:{email}");
            let (tel_href, tel_display) = phone_parts(phone);
            paragraph.extend([
                txt(" Email "),
                el("a", &[], &[("href", &mailto)], vec![txt(email)]),
                txt(" or call "),
                el("a", &[], &[("href", &tel_href)], vec![txt(tel_display)]),
                txt(" instead."),
            ]);
        }
        (Some(email), None) => {
            let mailto = format!("mailto:{email}");
            paragraph.extend([
                txt(" Email "),
                el("a", &[], &[("href", &mailto)], vec![txt(email)]),
                txt(" instead."),
            ]);
        }
        (None, Some(phone)) => {
            let (tel_href, tel_display) = phone_parts(phone);
            paragraph.extend([
                txt(" Call "),
                el("a", &[], &[("href", &tel_href)], vec![txt(tel_display)]),
                txt(" instead."),
            ]);
        }
        (None, None) => {}
    }
    el("noscript", &[], &[], vec![el("p", &[], &[], paragraph)])
}

fn newsletter_section(v: &VenueManifest) -> Option<Node> {
    let n = v.newsletter.as_ref().filter(|n| n.enabled)?;
    let site_key = v
        .integrations
        .turnstile
        .as_ref()
        .map(|t| t.site_key.as_str())
        .unwrap_or("");

    let mut form_children = vec![
        el(
            "input",
            &[],
            &[
                ("type", "hidden"),
                ("name", "source_route"),
                ("value", "/"),
            ],
            Vec::new(),
        ),
        el(
            "input",
            &[],
            &[
                ("type", "hidden"),
                ("name", "consent_text"),
                ("value", &n.consent_text),
            ],
            Vec::new(),
        ),
        el(
            "div",
            &["field"],
            &[],
            vec![
                el(
                    "label",
                    &[],
                    &[("for", "nl-email")],
                    vec![txt("Email address (required)")],
                ),
                el(
                    "input",
                    &[],
                    &[
                        ("id", "nl-email"),
                        ("name", "email"),
                        ("type", "email"),
                        ("autocomplete", "email"),
                        ("inputmode", "email"),
                        ("maxlength", "254"),
                        ("required", ""),
                        ("aria-describedby", "nl-email-hint"),
                    ],
                    Vec::new(),
                ),
                el(
                    "p",
                    &["hint"],
                    &[("id", "nl-email-hint")],
                    vec![txt("Example: name@example.com")],
                ),
            ],
        ),
        el(
            "div",
            &["field", "checkbox"],
            &[],
            vec![
                el(
                    "input",
                    &[],
                    &[
                        ("id", "nl-consent"),
                        ("name", "consent"),
                        ("type", "checkbox"),
                        ("value", "yes"),
                        ("required", ""),
                    ],
                    Vec::new(),
                ),
                el(
                    "label",
                    &[],
                    &[("for", "nl-consent")],
                    vec![txt(format!("{} (required)", n.consent_text))],
                ),
            ],
        ),
    ];

    if let Some(priv_note) = &n.privacy_note {
        form_children.push(el("p", &["hint"], &[], vec![txt(priv_note)]));
    }

    form_children.extend([
        el(
            "div",
            &["hp"],
            &[("aria-hidden", "true")],
            vec![
                el(
                    "label",
                    &[],
                    &[("for", "nl-website")],
                    vec![txt("Leave this field empty")],
                ),
                el(
                    "input",
                    &[],
                    &[
                        ("id", "nl-website"),
                        ("name", "website"),
                        ("type", "text"),
                        ("tabindex", "-1"),
                        ("autocomplete", "off"),
                    ],
                    Vec::new(),
                ),
            ],
        ),
        noscript_notice_node(Some(&v.nap.email), Some(&v.nap.phone)),
        el(
            "div",
            &["cf-turnstile"],
            &[("data-sitekey", site_key), ("data-action", "newsletter")],
            Vec::new(),
        ),
        el(
            "button",
            &[],
            &[("type", "submit")],
            vec![txt("Subscribe")],
        ),
        el(
            "p",
            &["form-status"],
            &[("role", "status"), ("aria-live", "polite")],
            Vec::new(),
        ),
    ]);

    let mut sec_children = vec![el(
        "h2",
        &[],
        &[("id", "newsletter-heading")],
        vec![txt("Newsletter")],
    )];
    if let Some(blurb) = &n.blurb
        && !blurb.trim().is_empty()
    {
        sec_children.push(el("p", &[], &[], vec![txt(blurb)]));
    }
    sec_children.push(el(
        "form",
        &["js-api-form"],
        &[
            ("action", "/api/newsletter"),
            ("method", "post"),
            (
                "data-success",
                "Thanks! You are subscribed. Check your inbox for future updates.",
            ),
        ],
        form_children,
    ));

    Some(el(
        "section",
        &["newsletter"],
        &[("id", "newsletter"), ("aria-labelledby", "newsletter-heading")],
        sec_children,
    ))
}

fn site_footer_node(v: &VenueManifest, with_form: bool) -> Node {
    let mut grid_children = Vec::new();
    if with_form && let Some(nl) = newsletter_section(v) {
        grid_children.push(nl);
    }

    let mut nap_children = vec![
        el("p", &["footer-title"], &[], vec![txt("Find us")]),
        address_node(v),
    ];

    if v.fictional == Some(true) {
        let msg = match &v.legal_disclaimer {
            Some(legal) => format!("{legal}. Not a real business."),
            None => "Not a real business.".to_string(),
        };
        nap_children.push(el("p", &["small"], &[], vec![txt(msg)]));
    } else if let Some(legal) = &v.legal_disclaimer {
        nap_children.push(el("p", &["small"], &[], vec![txt(legal)]));
    }

    grid_children.push(el("div", &["footer-nap"], &[], nap_children));

    el(
        "footer",
        &["site-footer"],
        &[],
        vec![el("div", &["wrap", "footer-grid"], &[], grid_children)],
    )
}

/// Whether the public events calendar is enabled (`scheduling.events.enabled`).
fn events_enabled(v: &VenueManifest) -> bool {
    v.scheduling
        .as_ref()
        .and_then(|s| s.events.as_ref())
        .is_some_and(|e| e.enabled)
}

/// The public `/events` page: semantic markup hydrated by `js/events-client.js`
/// from `GET /api/events`; without JavaScript it links to the JSON listing.
/// Deterministic: times are formatted client-side in `scheduling.events.timezone`.
fn events_page(v: &VenueManifest, base_url: &str, events: &EventsConfig) -> Page {
    let timezone = events.timezone.trim();

    let main_children = vec![
        el("h1", &[], &[], vec![txt("Upcoming free events")]),
        el(
            "p",
            &[],
            &[],
            vec![txt(format!(
                "Times are shown in the venue’s local time ({timezone})."
            ))],
        ),
        el(
            "p",
            &["form-status"],
            &[("id", "event-status"), ("role", "status")],
            Vec::new(),
        ),
        el(
            "noscript",
            &[],
            &[],
            vec![el(
                "p",
                &[],
                &[],
                vec![
                    el(
                        "a",
                        &[],
                        &[("href", "/api/events")],
                        vec![txt("View the event data")],
                    ),
                    txt(" or contact the venue for upcoming dates."),
                ],
            )],
        ),
        el(
            "ul",
            &[],
            &[("id", "event-list"), ("data-timezone", timezone)],
            Vec::new(),
        ),
    ];

    let mut nodes = site_header_nodes(v);
    nodes.push(el(
        "main",
        &[],
        &[("id", "main"), ("tabindex", "-1")],
        vec![el("div", &["wrap", "intro"], &[], main_children)],
    ));
    nodes.push(site_footer_node(v, false));

    Page {
        id: "events".to_string(),
        route: RoutePattern::Static {
            path: "/events".to_string(),
        },
        title: format!("Events — {}", v.name),
        description: Some("Upcoming free events.".to_string()),
        canonical: Some(format!("{base_url}/events")),
        noindex: false,
        is_not_found: false,
        json_ld: Vec::new(),
        scripts: vec![ScriptRef {
            src: "/js/events-client.js".to_string(),
            async_load: false,
            defer: true,
        }],
        body: nodes,
    }
}

/// Whether call booking is enabled (`scheduling.bookings.enabled`).
fn bookings_enabled(v: &VenueManifest) -> bool {
    v.scheduling
        .as_ref()
        .and_then(|s| s.bookings.as_ref())
        .is_some_and(|b| b.enabled)
}

/// Scripts for the `/book-a-call` page: the booking client plus the Turnstile
/// widget loader. No forms.js — the booking form manages its own validation
/// and submission.
fn booking_scripts() -> Vec<ScriptRef> {
    vec![
        ScriptRef {
            src: "/js/booking-client.js".to_string(),
            async_load: false,
            defer: true,
        },
        ScriptRef {
            src: TURNSTILE_SRC.to_string(),
            async_load: true,
            defer: true,
        },
    ]
}

/// Port of the old `core/tools/booking.css` into the pack's compiled IR
/// stylesheet (appended only when booking is enabled).
fn booking_style_rules() -> Vec<IrStyleRule> {
    vec![
        style_rule(
            "booking-label",
            ".booking-form label",
            None,
            &[("display", "block"), ("margin-top", "1rem")],
        ),
        style_rule(
            "booking-input",
            ".booking-form input[type=\"date\"], .booking-form textarea",
            None,
            &[
                ("display", "block"),
                ("width", "100%"),
                ("max-width", "26rem"),
                ("min-height", "44px"),
                ("padding", ".5rem .75rem"),
                ("font", "inherit"),
                ("color", "var(--text)"),
                ("background", "var(--surface)"),
                ("border", "2px solid var(--footer-text)"),
                ("border-radius", ".25rem"),
            ],
        ),
        style_rule(
            "booking-button",
            ".booking-form button",
            None,
            &[("margin-top", ".75rem")],
        ),
        style_rule(
            "booking-fieldset",
            ".booking-form fieldset",
            None,
            &[
                ("min-width", "0"),
                ("margin", "1rem 0"),
                ("border", "1px solid var(--muted)"),
            ],
        ),
        style_rule(
            "booking-options-label",
            "#booking-options label",
            None,
            &[
                ("display", "flex"),
                ("align-items", "center"),
                ("gap", ".5rem"),
                ("min-height", "44px"),
                ("margin", "0"),
            ],
        ),
        style_rule(
            "booking-options-input",
            "#booking-options input",
            None,
            &[
                ("flex", "none"),
                ("width", "24px"),
                ("height", "24px"),
                ("accent-color", "var(--accent)"),
            ],
        ),
        style_rule(
            "booking-inline-error",
            ".booking-inline-error",
            None,
            &[("margin", ".25rem 0 .5rem"), ("font-weight", "700")],
        ),
        style_rule(
            "booking-turnstile",
            ".booking-form .cf-turnstile",
            None,
            &[("margin-top", "1rem")],
        ),
    ]
}

/// The `/book-a-call` page: date picker plus keyboard-selectable time slots,
/// hydrated by `js/booking-client.js` from `GET /api/bookings/slots`. The form
/// stays hidden without JavaScript (the mandatory Turnstile check is a
/// no-JS-fail-closed exception) and the noscript notice offers the phone
/// fallback.
fn build_booking_body(v: &VenueManifest) -> Vec<Node> {
    let timezone = v
        .scheduling
        .as_ref()
        .and_then(|s| s.events.as_ref())
        .map(|e| e.timezone.trim())
        .unwrap_or_default();
    let site_key = v
        .integrations
        .turnstile
        .as_ref()
        .map(|t| t.site_key.as_str())
        .unwrap_or("");
    let (href, display) = phone_parts(&v.nap.phone);

    let mut nodes = site_header_nodes(v);
    let mut main_children = vec![
        el("h1", &[], &[], vec![txt("Book a call")]),
        el(
            "p",
            &[],
            &[],
            vec![txt(format!(
                "Choose a date and time in the venue’s time zone ({timezone}). We’ll send a calendar invitation to the email you provide."
            ))],
        ),
        el(
            "noscript",
            &[],
            &[],
            vec![el(
                "p",
                &[],
                &[],
                vec![
                    txt("Online scheduling requires JavaScript. To book by phone, call "),
                    el(
                        "a",
                        &[],
                        &[("href", &href)],
                        vec![txt(display)],
                    ),
                    txt("."),
                ],
            )],
        ),
        el(
            "div",
            &["card"],
            &[
                ("id", "booking-errors"),
                ("tabindex", "-1"),
                ("aria-labelledby", "booking-errors-title"),
                ("hidden", ""),
            ],
            vec![
                el(
                    "h2",
                    &[],
                    &[("id", "booking-errors-title")],
                    vec![txt("Check your booking")],
                ),
                el("ul", &[], &[], Vec::new()),
            ],
        ),
        el(
            "section",
            &["card"],
            &[
                ("id", "booking-confirmation"),
                ("tabindex", "-1"),
                ("aria-labelledby", "booking-confirmation-title"),
                ("hidden", ""),
            ],
            vec![
                el(
                    "h2",
                    &[],
                    &[("id", "booking-confirmation-title")],
                    vec![txt("Your call is booked")],
                ),
                el("p", &[], &[], vec![el("time", &[], &[], Vec::new())]),
                el(
                    "p",
                    &[],
                    &[],
                    vec![txt("Check your email for the calendar invitation.")],
                ),
            ],
        ),
    ];

    let form_children = vec![
        el(
            "label",
            &[],
            &[("for", "booking-date")],
            vec![txt("Date (required)")],
        ),
        el(
            "input",
            &[],
            &[
                ("id", "booking-date"),
                ("type", "date"),
                ("required", ""),
                ("aria-describedby", "booking-date-error"),
            ],
            Vec::new(),
        ),
        el(
            "p",
            &["booking-inline-error"],
            &[("id", "booking-date-error"), ("hidden", "")],
            Vec::new(),
        ),
        el(
            "button",
            &[],
            &[("type", "button"), ("id", "booking-find")],
            vec![txt("Find times")],
        ),
        el(
            "p",
            &[],
            &[("id", "booking-status"), ("role", "status"), ("aria-live", "polite")],
            Vec::new(),
        ),
        el(
            "fieldset",
            &[],
            &[
                ("id", "booking-times"),
                ("aria-describedby", "booking-time-error"),
                ("hidden", ""),
            ],
            vec![
                el(
                    "legend",
                    &[],
                    &[],
                    vec![txt("Available times (required)")],
                ),
                el("div", &[], &[("id", "booking-options")], Vec::new()),
                el(
                    "p",
                    &["booking-inline-error"],
                    &[("id", "booking-time-error"), ("hidden", "")],
                    Vec::new(),
                ),
            ],
        ),
        el(
            "label",
            &[],
            &[("for", "booking-name")],
            vec![txt("Name (required)")],
        ),
        el(
            "input",
            &[],
            &[
                ("id", "booking-name"),
                ("name", "guest_name"),
                ("type", "text"),
                ("autocomplete", "name"),
                ("maxlength", "100"),
                ("required", ""),
                ("aria-describedby", "booking-name-error"),
            ],
            Vec::new(),
        ),
        el(
            "p",
            &["booking-inline-error"],
            &[("id", "booking-name-error"), ("hidden", "")],
            Vec::new(),
        ),
        el(
            "label",
            &[],
            &[("for", "booking-email")],
            vec![txt("Email (required)")],
        ),
        el(
            "input",
            &[],
            &[
                ("id", "booking-email"),
                ("name", "guest_email"),
                ("type", "email"),
                ("autocomplete", "email"),
                ("maxlength", "254"),
                ("required", ""),
                ("aria-describedby", "booking-email-error"),
            ],
            Vec::new(),
        ),
        el(
            "p",
            &["booking-inline-error"],
            &[("id", "booking-email-error"), ("hidden", "")],
            Vec::new(),
        ),
        el(
            "label",
            &[],
            &[("for", "booking-notes")],
            vec![txt("Notes (optional)")],
        ),
        el(
            "textarea",
            &[],
            &[
                ("id", "booking-notes"),
                ("name", "guest_notes"),
                ("maxlength", "4000"),
                ("rows", "3"),
            ],
            Vec::new(),
        ),
        el(
            "div",
            &["cf-turnstile"],
            &[
                ("data-sitekey", site_key),
                ("data-action", "booking"),
            ],
            Vec::new(),
        ),
        el(
            "button",
            &[],
            &[("id", "booking-submit"), ("type", "submit"), ("disabled", "")],
            vec![txt("Book this time")],
        ),
    ];
    main_children.push(el(
        "form",
        &["card", "booking-form"],
        &[
            ("id", "booking-form"),
            ("action", "/api/bookings"),
            ("method", "post"),
            ("data-timezone", timezone),
            ("hidden", ""),
        ],
        form_children,
    ));

    nodes.push(el(
        "main",
        &[],
        &[("id", "main"), ("tabindex", "-1")],
        vec![el("div", &["wrap", "intro"], &[], main_children)],
    ));
    nodes.push(site_footer_node(v, false));
    nodes
}

fn booking_page(v: &VenueManifest, base_url: &str) -> Page {
    Page {
        id: "book-a-call".to_string(),
        route: RoutePattern::Static {
            path: "/book-a-call".to_string(),
        },
        title: format!("Book a call — {}", v.name),
        description: Some("Choose a time for a call with the venue.".to_string()),
        canonical: Some(format!("{base_url}/book-a-call")),
        noindex: false,
        is_not_found: false,
        json_ld: Vec::new(),
        scripts: booking_scripts(),
        body: build_booking_body(v),
    }
}

fn build_index_body(v: &VenueManifest) -> Vec<Node> {
    let mut nodes = site_header_nodes(v);

    let a = &v.nap.address;
    let location_text = match &v.category_label {
        Some(cat) if !cat.trim().is_empty() => {
            format!("{cat} in {}, {}.", a.locality, a.region)
        }
        _ => format!("{}, {}.", a.locality, a.region),
    };

    let mut intro_children = vec![el("h1", &[], &[], vec![txt(&v.name)])];
    if let Some(tagline) = &v.tagline {
        intro_children.push(el("p", &["tagline"], &[], vec![txt(tagline)]));
    }
    if let Some(desc) = &v.description {
        intro_children.push(el("p", &[], &[], vec![txt(desc)]));
    }
    intro_children.push(el("p", &["small"], &[], vec![txt(location_text)]));

    let mut visit_children = vec![
        el(
            "h2",
            &[],
            &[("id", "visit-heading")],
            vec![txt("Hours & location")],
        ),
        address_node(v),
        hours_table_node(v),
    ];
    visit_children.extend(holiday_nodes(v));
    visit_children.push(el("h3", &[], &[], vec![txt("Reservations")]));
    visit_children.push(seating_paragraph(v));

    if !v.venue_accessibility.is_empty() {
        let access_items: Vec<Node> = v
            .venue_accessibility
            .iter()
            .map(|x| el("li", &[], &[], vec![txt(x)]))
            .collect();
        visit_children.push(el("h3", &[], &[], vec![txt("Accessibility")]));
        visit_children.push(el("ul", &[], &[], access_items));
    }

    let mut wrap_children = vec![
        el("div", &["intro"], &[], intro_children),
        el(
            "section",
            &["visit", "card"],
            &[("id", "visit"), ("aria-labelledby", "visit-heading")],
            visit_children,
        ),
    ];

    if let Some(hh) = happy_hour_node(v) {
        wrap_children.push(hh);
    }

    let mut menu_children = vec![el(
        "h2",
        &[],
        &[("id", "menu-heading")],
        vec![txt(&v.menu.name)],
    )];
    menu_children.extend(menu_section_nodes(v));
    wrap_children.push(el(
        "section",
        &["menu"],
        &[("id", "menu"), ("aria-labelledby", "menu-heading")],
        menu_children,
    ));

    nodes.push(el(
        "main",
        &[],
        &[("id", "main"), ("tabindex", "-1")],
        vec![el("div", &["wrap"], &[], wrap_children)],
    ));
    nodes.push(site_footer_node(v, true));
    nodes
}

fn build_404_body(v: &VenueManifest) -> Vec<Node> {
    let mut nodes = site_header_nodes(v);
    nodes.push(el(
        "main",
        &[],
        &[("id", "main"), ("tabindex", "-1")],
        vec![el(
            "div",
            &["wrap", "intro"],
            &[],
            vec![
                el("h1", &[], &[], vec![txt("Page not found")]),
                el(
                    "p",
                    &[],
                    &[],
                    vec![
                        txt("Sorry, we could not find that page. "),
                        el(
                            "a",
                            &[],
                            &[("href", "/")],
                            vec![txt("Return to the home page")],
                        ),
                        txt("."),
                    ],
                ),
            ],
        )],
    ));
    nodes.push(site_footer_node(v, false));
    nodes
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sws_plugin::PluginRegistry;

    use super::*;

    const DEMO_VENUE_JSON: &str = include_str!("../../demo/venue.json");

    #[test]
    fn hospitality_plugin_transforms_and_lints_demo_venue() {
        let val: Value = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        let plugin = HospitalityPlugin;
        assert_eq!(plugin.id(), "hospitality");
        assert!(plugin.can_transform_input(&val));
        assert!(!plugin.can_transform_input(&json!({"slug": "other"})));

        let doc = plugin.transform_input(&val).unwrap();
        assert_eq!(doc.slug, "shirlington-hearth-demo");
        // Demo ships events, booking AND checkout enabled: index + events +
        // book-a-call + takeout + 404.
        assert_eq!(doc.pages.len(), 5);
        assert!(doc.pages.iter().any(|p| p.id == "events"));
        assert!(doc.pages.iter().any(|p| p.id == "book-a-call"));
        assert!(doc.pages.iter().any(|p| p.id == "takeout"));
        assert!(doc.metadata.contains_key("venue_manifest"));
        assert!(sws_ir::validate_ir(&doc).is_empty());
        assert!(plugin.lint(&doc, Some(&val)).is_empty());

        // Verify CSS rules and tokens emit cleanly via sws_css::Stylesheet
        let mut sheet = sws_css::Stylesheet::new();
        sheet.include_reduced_motion_defaults();
        for (k, v) in &doc.tokens.colors {
            sheet.set_var(k, v);
        }
        sheet.set_var("font-body", &doc.tokens.fonts.body);
        if let Some(h) = &doc.tokens.fonts.heading {
            sheet.set_var("font-heading", h);
        }
        for bp in &doc.breakpoints {
            sheet.define_breakpoint(&bp.id, &bp.media_query);
        }
        for r in &doc.styles {
            sheet.add_rule(sws_css::CssRule {
                selector: r.selector.clone(),
                breakpoint: r.breakpoint.clone(),
                pseudo: r.pseudo.clone(),
                declarations: r.declarations.clone(),
            });
        }
        let css = sheet.emit().unwrap();
        assert!(css.contains("--bg: #E9E2D2;"));
        assert!(css.contains("--accent: #7A1F2B;"));
        assert!(css.contains("--footer-bg: #241207;"));
        assert!(css.contains(".skip-link:focus, .skip-link:focus-visible"));
        assert!(css.contains("@media (min-width: 40rem)"));
        assert!(css.contains("@media (min-width: 48rem)"));
        assert!(css.contains("@media print"));
        assert!(css.len() < 10_000, "CSS size {} unexpectedly large", css.len());
    }

    #[test]
    fn hospitality_plugin_rejects_invalid_manifest() {
        let mut val: Value = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        val["slug"] = json!("Invalid_Slug!");
        let err = HospitalityPlugin.transform_input(&val).unwrap_err();
        assert_eq!(err.plugin_id, "hospitality");
        assert!(err.message.contains("slug"));
    }

    #[test]
    fn hospitality_plugin_emits_deterministic_events_page_and_assets() {
        let val: Value = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        let doc = HospitalityPlugin.transform_input(&val).unwrap();

        let events = doc
            .pages
            .iter()
            .find(|p| p.id == "events")
            .expect("demo ships events enabled");
        match &events.route {
            RoutePattern::Static { path } => assert_eq!(path, "/events"),
            other => panic!("unexpected events route: {other:?}"),
        }
        assert_eq!(events.title, "Events — Shirlington Hearth");
        assert!(events.scripts.iter().any(|s| s.src == "/js/events-client.js"));
        let body = format!("{:?}", events.body);
        assert!(body.contains("Upcoming free events"), "{body}");
        assert!(body.contains("event-status"), "{body}");
        assert!(doc.seo.sitemap_routes.contains(&"/events".to_string()));

        let venue: VenueManifest = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        let extra = crate::events::events_extra_files(&venue).expect("events assets");
        assert!(extra.contains_key("events-config.json"));
        assert!(extra.contains_key("js/events-client.js"));

        // Disabling events removes the page and both assets deterministically.
        let mut disabled = venue.clone();
        disabled
            .scheduling
            .as_mut()
            .unwrap()
            .events
            .as_mut()
            .unwrap()
            .enabled = false;
        assert!(crate::events::events_extra_files(&disabled).is_none());
        let doc2 = HospitalityPlugin
            .transform_input(&serde_json::to_value(&disabled).unwrap())
            .unwrap();
        // Events disabled; the sandbox demo checkout and booking pages keep the
        // count at four (index, book-a-call, takeout, 404) instead of the bare two.
        assert_eq!(doc2.pages.len(), 4);
        assert!(doc2.pages.iter().all(|p| p.id != "events"));
        assert!(!doc2.seo.sitemap_routes.contains(&"/events".to_string()));
    }

    #[test]
    fn hospitality_plugin_emits_deterministic_booking_page_and_assets() {
        let val: Value = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        let doc = HospitalityPlugin.transform_input(&val).unwrap();

        let booking = doc
            .pages
            .iter()
            .find(|p| p.id == "book-a-call")
            .expect("demo ships booking enabled");
        match &booking.route {
            RoutePattern::Static { path } => assert_eq!(path, "/book-a-call"),
            other => panic!("unexpected booking route: {other:?}"),
        }
        assert_eq!(booking.title, "Book a call — Shirlington Hearth");
        let srcs: Vec<&str> = booking.scripts.iter().map(|s| s.src.as_str()).collect();
        assert!(srcs.contains(&"/js/booking-client.js"));
        assert!(srcs.iter().any(|s| s.starts_with("https://challenges.cloudflare.com/")));
        let body = format!("{:?}", booking.body);
        assert!(body.contains("Book a call"), "{body}");
        assert!(body.contains("booking-form"), "{body}");
        assert!(body.contains("data-timezone"), "{body}");
        assert!(body.contains("Online scheduling requires JavaScript"), "{body}");
        assert!(body.contains("/api/bookings"), "{body}");
        assert!(doc.seo.sitemap_routes.contains(&"/book-a-call".to_string()));

        // The booking stylesheet compiles through sws_css with the site tokens.
        let mut sheet = sws_css::Stylesheet::new();
        for r in doc.styles.iter().filter(|r| r.id.starts_with("booking-")) {
            sheet
                .add_rule(sws_css::CssRule {
                    selector: r.selector.clone(),
                    breakpoint: r.breakpoint.clone(),
                    pseudo: r.pseudo.clone(),
                    declarations: r.declarations.clone(),
                });
        }
        let css = sheet.emit().unwrap();
        assert!(css.contains("#booking-options input"));
        assert!(css.contains("min-height: 44px"));

        let index = &doc.pages[0];
        let index_body = format!("{index:?}");
        assert!(
            index_body.contains(r#""href": "/book-a-call""#),
            "{index_body}"
        );

        let venue: VenueManifest = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        let extra = crate::bookings::bookings_extra_files(&venue).expect("booking assets");
        assert!(extra.contains_key("booking-config.json"));
        assert!(extra.contains_key("js/booking-client.js"));

        // Disabling booking removes the page, nav link and assets deterministically.
        let mut disabled = venue.clone();
        disabled
            .scheduling
            .as_mut()
            .unwrap()
            .bookings
            .as_mut()
            .unwrap()
            .enabled = false;
        assert!(crate::bookings::bookings_extra_files(&disabled).is_none());
        let doc2 = HospitalityPlugin
            .transform_input(&serde_json::to_value(&disabled).unwrap())
            .unwrap();
        assert_eq!(doc2.pages.len(), 4);
        assert!(doc2.pages.iter().all(|p| p.id != "book-a-call"));
        assert!(!doc2.seo.sitemap_routes.contains(&"/book-a-call".to_string()));
        assert!(!doc2.pages.iter().any(|p| p.id == "index"
            && format!("{:?}", p.body).contains(r#""href": "/book-a-call""#)));
    }

    #[test]
    fn hospitality_plugin_emits_advisory_abc_diagnostics() {
        let mut val: Value = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        val["happy_hour"]["offers"] = json!(["Bottomless mimosas"]);
        val["happy_hour"]["end"] = json!("22:00");

        let mut reg = PluginRegistry::new();
        reg.register(Box::new(HospitalityPlugin))
            .expect("hospitality pack must satisfy the current plugin ABI");

        let doc = reg.transform_input_with(None, &val).unwrap();
        let diags = reg.collect_diagnostics(&doc, None);
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|d| d.severity == DiagnosticSeverity::Advisory));
        assert!(diags.iter().any(|d| d.rule == "bottomless"));
        assert!(diags.iter().any(|d| d.rule == "happy_hour_late_end"));
    }

    /// Issue #32: the Turnstile-protected newsletter form must emit an
    /// accessible `<noscript>` notice with `mailto:`/`tel:` alternative paths.
    #[test]
    fn newsletter_form_emits_accessible_noscript_notice() {
        let val: Value = serde_json::from_str(DEMO_VENUE_JSON).unwrap();
        let doc = HospitalityPlugin.transform_input(&val).unwrap();

        fn find_form<'a>(nodes: &'a [Node], action: &str) -> Option<&'a Node> {
            for node in nodes {
                if let Node::Element { tag, attrs, children, .. } = node {
                    if tag == "form" && attrs.get("action").is_some_and(|a| a == action) {
                        return Some(node);
                    }
                    if let Some(found) = find_form(children, action) {
                        return Some(found);
                    }
                }
            }
            None
        }

        fn find_noscript(nodes: &[Node]) -> Option<&Node> {
            for node in nodes {
                if let Node::Element { tag, children, .. } = node {
                    if tag == "noscript" {
                        return Some(node);
                    }
                    if let Some(found) = find_noscript(children) {
                        return Some(found);
                    }
                }
            }
            None
        }

        fn text_of(node: &Node) -> String {
            match node {
                Node::Text { value } => value.clone(),
                Node::Element { children, .. } => children.iter().map(text_of).collect(),
                _ => String::new(),
            }
        }

        let form = find_form(&doc.pages[0].body, "/api/newsletter").expect("newsletter form");
        let noscript = find_noscript(std::slice::from_ref(form)).expect("noscript notice");

        let Node::Element { tag, children, .. } = noscript else {
            panic!("expected element");
        };
        assert_eq!(tag, "noscript");
        // A single plain paragraph: no heading-level disruption of the ladder.
        assert_eq!(children.len(), 1, "noscript must wrap one paragraph");
        let Node::Element { tag: p_tag, children: p_children, .. } = &children[0] else {
            panic!("expected element");
        };
        assert_eq!(p_tag, "p");

        let text = text_of(noscript);
        assert!(
            text.contains("This form requires JavaScript for spam verification"),
            "statement missing: {text}"
        );

        // mailto: and tel: alternative paths, each with discernible text.
        let links: Vec<(&str, String)> = p_children
            .iter()
            .filter_map(|n| match n {
                Node::Element { tag, attrs, children, .. } if tag == "a" => {
                    Some((attrs.get("href")?.as_str(), text_of(n)))
                }
                _ => None,
            })
            .collect();
        assert!(
            links.iter().any(|(href, label)| href.starts_with("mailto:") && !label.trim().is_empty()),
            "mailto link with discernible text missing: {links:?}"
        );
        assert!(
            links.iter().any(|(href, label)| href.starts_with("tel:") && !label.trim().is_empty()),
            "tel link with discernible text missing: {links:?}"
        );
        assert!(
            !p_children.iter().any(|n| matches!(n, Node::Element { tag, .. } if tag == "h1" || tag == "h2" || tag == "h3" || tag == "h4" || tag == "h5" || tag == "h6")),
            "noscript must not contain headings"
        );
    }
}
