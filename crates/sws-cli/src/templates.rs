//! Starter templates and export scaffolds for the `solo` CLI.

use std::collections::BTreeMap;

use serde_json::json;
use sws_ir::{
    ContrastPair, DesignTokens, FontTokens, FormFieldKind, FormFieldSchema, FormSchema,
    IrStyleRule, Node, Page, RoutePattern, ScriptRef, SeoMeta, SiteDocument,
};
#[cfg(feature = "pack-hospitality")]
use sws_pack_hospitality::venue::{
    Address, Brand, ContrastConfig, ContrastPair as VenueContrastPair, DayHours, Fonts, HappyHour,
    Integrations, Menu, MenuItem, MenuSection, Nap, NewsletterConfig, SeoConfig, TurnstileConfig,
    VenueManifest, contrast_ratio,
};
use sws_security::headers::{API_CACHE_CONTROL, SECURITY_HEADERS};

/// Embedded domain-agnostic Cloudflare D1 schema emitted by `solo export`.
pub const MIGRATION_0001_INIT_SQL: &str =
    include_str!("../../sws-worker-site/migrations/0001_init.sql");

/// Build a minimal, accessible, domain-agnostic [`SiteDocument`] starter.
#[must_use]
pub fn starter_site_document() -> SiteDocument {
    let mut colors = BTreeMap::new();
    colors.insert("background".to_string(), "#FFFFFF".to_string());
    colors.insert("text".to_string(), "#1A1A1A".to_string());
    colors.insert("surface".to_string(), "#F4F4F6".to_string());
    colors.insert("accent".to_string(), "#0B4F9C".to_string());
    colors.insert("on_accent".to_string(), "#FFFFFF".to_string());
    colors.insert("muted_text".to_string(), "#4A4A4A".to_string());
    colors.insert("footer_background".to_string(), "#1A1A1A".to_string());
    colors.insert("footer_text".to_string(), "#FFFFFF".to_string());

    let mut spacing = BTreeMap::new();
    spacing.insert("sm".to_string(), "0.5rem".to_string());
    spacing.insert("md".to_string(), "1rem".to_string());
    spacing.insert("lg".to_string(), "2rem".to_string());

    let mut radii = BTreeMap::new();
    radii.insert("card".to_string(), "0.5rem".to_string());

    let contrast_pairs = vec![
        ContrastPair {
            foreground: "text".to_string(),
            background: "background".to_string(),
            min_ratio: 4.5,
            usage: Some("body text".to_string()),
        },
        ContrastPair {
            foreground: "text".to_string(),
            background: "surface".to_string(),
            min_ratio: 4.5,
            usage: Some("card body text".to_string()),
        },
        ContrastPair {
            foreground: "accent".to_string(),
            background: "background".to_string(),
            min_ratio: 4.5,
            usage: Some("links and headings".to_string()),
        },
        ContrastPair {
            foreground: "on_accent".to_string(),
            background: "accent".to_string(),
            min_ratio: 4.5,
            usage: Some("primary buttons".to_string()),
        },
        ContrastPair {
            foreground: "muted_text".to_string(),
            background: "background".to_string(),
            min_ratio: 4.5,
            usage: Some("secondary text".to_string()),
        },
        ContrastPair {
            foreground: "footer_text".to_string(),
            background: "footer_background".to_string(),
            min_ratio: 4.5,
            usage: Some("footer text".to_string()),
        },
    ];

    let mut forms = BTreeMap::new();
    forms.insert(
        "contact".to_string(),
        FormSchema {
            id: "contact".to_string(),
            action: "/api/forms/contact".to_string(),
            method: "POST".to_string(),
            turnstile_action: "contact".to_string(),
            fields: vec![
                FormFieldSchema {
                    name: "name".to_string(),
                    label: "Name".to_string(),
                    kind: FormFieldKind::Text,
                    required: true,
                    placeholder: Some("Alex Rivera".to_string()),
                    help_text: None,
                    default_value: None,
                    options: Vec::new(),
                    max_chars: Some(120),
                    min: None,
                    max: None,
                    autocomplete: Some("name".to_string()),
                },
                FormFieldSchema {
                    name: "email".to_string(),
                    label: "Email".to_string(),
                    kind: FormFieldKind::Email,
                    required: true,
                    placeholder: Some("you@example.test".to_string()),
                    help_text: None,
                    default_value: None,
                    options: Vec::new(),
                    max_chars: Some(254),
                    min: None,
                    max: None,
                    autocomplete: Some("email".to_string()),
                },
                FormFieldSchema {
                    name: "message".to_string(),
                    label: "Message".to_string(),
                    kind: FormFieldKind::Textarea,
                    required: true,
                    placeholder: Some("How can we help?".to_string()),
                    help_text: None,
                    default_value: None,
                    options: Vec::new(),
                    max_chars: Some(2000),
                    min: None,
                    max: None,
                    autocomplete: None,
                },
                FormFieldSchema {
                    name: "source_route".to_string(),
                    label: "Source route".to_string(),
                    kind: FormFieldKind::Route,
                    required: false,
                    placeholder: None,
                    help_text: None,
                    default_value: Some("/".to_string()),
                    options: Vec::new(),
                    max_chars: Some(200),
                    min: None,
                    max: None,
                    autocomplete: None,
                },
            ],
        },
    );

    let home_page = Page {
        id: "home".to_string(),
        route: RoutePattern::Static {
            path: "/".to_string(),
        },
        title: "Starter Site".to_string(),
        description: Some("Minimal accessible site built with Solo Website System.".to_string()),
        canonical: Some("https://starter-site.example/".to_string()),
        noindex: false,
        is_not_found: false,
        json_ld: Vec::new(),
        scripts: Vec::new(),
        body: vec![
            el(
                Some("skip-link"),
                "a",
                [("href", "#main")],
                &["skip-link"],
                vec![text("Skip to main content")],
            ),
            el(
                Some("site-header"),
                "header",
                [],
                &["site-header"],
                vec![
                    el(
                        Some("site-brand"),
                        "p",
                        [],
                        &["site-brand"],
                        vec![text("Starter Site")],
                    ),
                    el(
                        Some("primary-nav"),
                        "nav",
                        [("aria-label", "Primary")],
                        &["primary-nav"],
                        vec![
                            el(None, "a", [("href", "#overview")], &[], vec![text("Overview")]),
                            el(None, "a", [("href", "#contact")], &[], vec![text("Contact")]),
                        ],
                    ),
                ],
            ),
            el(
                Some("main"),
                "main",
                [("tabindex", "-1")],
                &["site-main"],
                vec![
                    el(
                        Some("hero-title"),
                        "h1",
                        [],
                        &["hero-title"],
                        vec![text("Starter Site")],
                    ),
                    el(
                        Some("overview"),
                        "section",
                        [("aria-labelledby", "overview-heading")],
                        &["card"],
                        vec![
                            el(
                                Some("overview-heading"),
                                "h2",
                                [],
                                &[],
                                vec![text("Overview")],
                            ),
                            el(
                                Some("overview-copy"),
                                "p",
                                [],
                                &[],
                                vec![text(
                                    "Accessible static and edge website generated by Solo Website System (SWS).",
                                )],
                            ),
                        ],
                    ),
                    el(
                        Some("contact"),
                        "section",
                        [("aria-labelledby", "contact-heading")],
                        &["card"],
                        vec![
                            el(
                                Some("contact-heading"),
                                "h2",
                                [],
                                &[],
                                vec![text("Contact")],
                            ),
                            el(
                                Some("contact-form"),
                                "form",
                                [
                                    ("method", "post"),
                                    ("action", "/api/forms/contact"),
                                    ("data-sws-form", "contact"),
                                ],
                                &["js-api-form", "sws-form"],
                                vec![
                                    form_input_group(
                                        "contact-name",
                                        "name",
                                        "Name",
                                        "text",
                                        "name",
                                        true,
                                    ),
                                    form_input_group(
                                        "contact-email",
                                        "email",
                                        "Email",
                                        "email",
                                        "email",
                                        true,
                                    ),
                                    el(
                                        None,
                                        "div",
                                        [],
                                        &["field"],
                                        vec![
                                            el(
                                                None,
                                                "label",
                                                [("for", "contact-message")],
                                                &[],
                                                vec![text("Message")],
                                            ),
                                            el(
                                                Some("contact-message"),
                                                "textarea",
                                                [
                                                    ("name", "message"),
                                                    ("rows", "4"),
                                                    ("required", "required"),
                                                ],
                                                &[],
                                                Vec::new(),
                                            ),
                                        ],
                                    ),
                                    el(
                                        None,
                                        "input",
                                        [
                                            ("type", "hidden"),
                                            ("name", "source_route"),
                                            ("value", "/"),
                                        ],
                                        &[],
                                        Vec::new(),
                                    ),
                                    el(
                                        None,
                                        "div",
                                        [("aria-hidden", "true")],
                                        &["hp"],
                                        vec![
                                            el(
                                                None,
                                                "label",
                                                [("for", "contact-website")],
                                                &[],
                                                vec![text("Leave this field blank")],
                                            ),
                                            el(
                                                Some("contact-website"),
                                                "input",
                                                [
                                                    ("type", "text"),
                                                    ("name", "website"),
                                                    ("tabindex", "-1"),
                                                    ("autocomplete", "off"),
                                                ],
                                                &[],
                                                Vec::new(),
                                            ),
                                        ],
                                    ),
                                    el(
                                        Some("contact-turnstile"),
                                        "div",
                                        [
                                            ("data-sitekey", "1x00000000000000000000AA"),
                                            ("data-action", "contact"),
                                        ],
                                        &["cf-turnstile"],
                                        Vec::new(),
                                    ),
                                    el(
                                        Some("contact-submit"),
                                        "button",
                                        [("type", "submit")],
                                        &["btn"],
                                        vec![text("Send inquiry")],
                                    ),
                                    el(
                                        Some("contact-status"),
                                        "div",
                                        [("role", "status"), ("aria-live", "polite")],
                                        &["form-status"],
                                        Vec::new(),
                                    ),
                                ],
                            ),
                        ],
                    ),
                ],
            ),
            el(
                Some("site-footer"),
                "footer",
                [],
                &["site-footer"],
                vec![el(
                    None,
                    "p",
                    [],
                    &[],
                    vec![text("Starter Site — built with Solo Website System.")],
                )],
            ),
        ],
    };

    let not_found_page = Page {
        id: "not-found".to_string(),
        route: RoutePattern::Static {
            path: "/404".to_string(),
        },
        title: "Page Not Found".to_string(),
        description: Some("The requested page could not be found.".to_string()),
        canonical: None,
        noindex: true,
        is_not_found: true,
        json_ld: Vec::new(),
        scripts: Vec::new(),
        body: vec![
            el(
                None,
                "a",
                [("href", "#main")],
                &["skip-link"],
                vec![text("Skip to main content")],
            ),
            el(
                None,
                "header",
                [],
                &["site-header"],
                vec![el(None, "p", [], &["site-brand"], vec![text("Starter Site")])],
            ),
            el(
                Some("main"),
                "main",
                [("tabindex", "-1")],
                &["site-main"],
                vec![
                    el(None, "h1", [], &[], vec![text("Page Not Found")]),
                    el(
                        None,
                        "p",
                        [],
                        &[],
                        vec![text("The requested page could not be found.")],
                    ),
                    el(
                        None,
                        "p",
                        [],
                        &[],
                        vec![el(None, "a", [("href", "/")], &[], vec![text("Return home")])],
                    ),
                ],
            ),
            el(
                None,
                "footer",
                [],
                &["site-footer"],
                vec![el(
                    None,
                    "p",
                    [],
                    &[],
                    vec![text("Starter Site — built with Solo Website System.")],
                )],
            ),
        ],
    };

    SiteDocument {
        schema_version: 1,
        slug: "starter-site-demo".to_string(),
        name: "Starter Site".to_string(),
        locale: "en-US".to_string(),
        base_url: "https://starter-site.example".to_string(),
        tokens: DesignTokens {
            colors,
            fonts: FontTokens {
                body: "system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif".to_string(),
                heading: Some("Georgia, \"Times New Roman\", serif".to_string()),
                mono: None,
            },
            spacing,
            radii,
            contrast_pairs,
        },
        breakpoints: Vec::new(),
        styles: starter_styles(),
        components: BTreeMap::new(),
        forms,
        pages: vec![home_page, not_found_page],
        seo: SeoMeta {
            title_template: Some("%s | Starter Site".to_string()),
            default_description: Some(
                "Minimal accessible site built with Solo Website System.".to_string(),
            ),
            robots_allow: vec!["/".to_string()],
            robots_disallow: vec!["/api/".to_string()],
            sitemap_routes: vec!["/".to_string()],
        },
        scripts: vec![ScriptRef {
            src: "/js/forms.js".to_string(),
            async_load: false,
            defer: true,
        }],
        data: BTreeMap::new(),
        metadata: BTreeMap::new(),
    }
}

/// Build a fictional hospitality [`VenueManifest`] starter with `.example` domain and `555-01xx` phone.
#[cfg(feature = "pack-hospitality")]
#[must_use]
pub fn starter_hospitality_venue() -> VenueManifest {
    let mut hours = BTreeMap::new();
    for day in ["monday", "tuesday", "wednesday", "thursday", "friday"] {
        hours.insert(
            day.to_string(),
            DayHours {
                opens: Some("07:00".to_string()),
                closes: Some("19:00".to_string()),
                closed: None,
            },
        );
    }
    for day in ["saturday", "sunday"] {
        hours.insert(
            day.to_string(),
            DayHours {
                opens: Some("08:00".to_string()),
                closes: Some("18:00".to_string()),
                closed: None,
            },
        );
    }

    let mut palette = BTreeMap::new();
    palette.insert("background".to_string(), "#E9E2D2".to_string());
    palette.insert("text".to_string(), "#241207".to_string());
    palette.insert("surface".to_string(), "#F6F1E7".to_string());
    palette.insert("accent".to_string(), "#7A1F2B".to_string());
    palette.insert("accent_secondary".to_string(), "#2F5233".to_string());
    palette.insert("muted_text".to_string(), "#5C3A1E".to_string());
    palette.insert("on_accent".to_string(), "#F6F1E7".to_string());
    palette.insert("footer_background".to_string(), "#241207".to_string());
    palette.insert("footer_text".to_string(), "#E9E2D2".to_string());

    let pair_specs = [
        ("text", "background", "body text"),
        ("text", "surface", "body text on cards"),
        ("accent", "background", "links and headings"),
        ("accent", "surface", "prices and links on cards"),
        ("accent_secondary", "background", "dietary tags"),
        ("accent_secondary", "surface", "dietary tags on cards"),
        ("muted_text", "background", "secondary text"),
        ("muted_text", "surface", "item descriptions"),
        ("on_accent", "accent", "button text"),
        ("footer_text", "footer_background", "footer text and links"),
    ];
    let pairs = pair_specs
        .into_iter()
        .map(|(fg, bg, usage)| {
            let ratio = contrast_ratio(&palette[fg], &palette[bg]).unwrap_or(0.0);
            let rounded = (ratio * 100.0).round() / 100.0;
            VenueContrastPair {
                foreground: fg.to_string(),
                background: bg.to_string(),
                ratio: rounded,
                usage: Some(usage.to_string()),
            }
        })
        .collect();

    VenueManifest {
        schema: None,
        schema_version: 1,
        slug: "cedar-and-rye-demo".to_string(),
        name: "Cedar & Rye Café".to_string(),
        legal_disclaimer: Some("Fictional demonstration starter venue".to_string()),
        fictional: Some(true),
        tagline: Some("Neighborhood bakery and coffee bar.".to_string()),
        description: Some(
            "Cedar & Rye Café is a fictional neighborhood bakery and coffee bar starter manifest for Solo Website System."
                .to_string(),
        ),
        category_label: Some("Café & Bakery".to_string()),
        business_types: vec!["CafeOrCoffeeShop".to_string()],
        cuisine: vec!["Coffee".to_string(), "Pastries".to_string()],
        price_range: Some("$$".to_string()),
        locale: Some("en-US".to_string()),
        timezone: Some("America/New_York".to_string()),
        nap: Nap {
            name: "Cedar & Rye Café".to_string(),
            address: Address {
                street: "1200 Maplewood Lane".to_string(),
                locality: "Arlington".to_string(),
                region: "VA".to_string(),
                postal_code: "22201".to_string(),
                country: "US".to_string(),
            },
            phone: "+1-703-555-0188".to_string(),
            email: "hello@cedar-and-rye.example".to_string(),
            website: "https://cedar-and-rye.example".to_string(),
        },
        geo: None,
        hours,
        holiday_overrides: Vec::new(),
        booking_url: None,
        venue_accessibility: vec![
            "Step-free entrance from Maplewood Lane.".to_string(),
            "Accessible restroom on the main floor.".to_string(),
        ],
        brand: Brand {
            palette,
            contrast: ContrastConfig {
                wcag_target: Some(
                    "AA text 4.5:1 for every foreground/background pair actually rendered"
                        .to_string(),
                ),
                pairs,
            },
            fonts: Fonts {
                body: "system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif".to_string(),
                heading: Some("Georgia, \"Times New Roman\", serif".to_string()),
            },
        },
        modules: BTreeMap::new(),
        integrations: Integrations {
            toast: None,
            seating_policy: "walk_in_only".to_string(),
            opentable: None,
            resy: None,
            inkind: None,
            turnstile: Some(TurnstileConfig {
                site_key: "1x00000000000000000000AA".to_string(),
                test_key: Some(true),
            }),
        },
        newsletter: Some(NewsletterConfig {
            enabled: true,
            blurb: Some("Occasional seasonal menu updates.".to_string()),
            consent_text: "Yes, email me Cedar & Rye Café updates. I can unsubscribe at any time."
                .to_string(),
            privacy_note: Some(
                "We never sell your email. Every message includes an unsubscribe link.".to_string(),
            ),
        }),
        happy_hour: Some(HappyHour {
            days: vec![
                "monday".to_string(),
                "tuesday".to_string(),
                "wednesday".to_string(),
                "thursday".to_string(),
                "friday".to_string(),
            ],
            start: "15:00".to_string(),
            end: "18:00".to_string(),
            offers: vec![
                "$2 off pour-over flights".to_string(),
                "Half-price house pastries after 4pm".to_string(),
            ],
            notes: Some("Offers subject to availability.".to_string()),
        }),
        menu: Menu {
            name: "Daytime Menu".to_string(),
            currency: "USD".to_string(),
            sections: vec![
                MenuSection {
                    id: "coffee".to_string(),
                    name: "Coffee & Tea".to_string(),
                    description: None,
                    items: vec![
                        MenuItem {
                            name: "Espresso".to_string(),
                            description: Some("Double shot of our seasonal espresso roast.".to_string()),
                            price: Some("3.50".to_string()),
                            price_note: None,
                            contains_alcohol: Some(false),
                            dietary: vec!["vegan".to_string(), "gluten-free".to_string()],
                        },
                        MenuItem {
                            name: "Oat Latte".to_string(),
                            description: Some("Espresso with steamed oat milk.".to_string()),
                            price: Some("5.25".to_string()),
                            price_note: None,
                            contains_alcohol: Some(false),
                            dietary: vec!["vegan".to_string()],
                        },
                    ],
                },
                MenuSection {
                    id: "pastries".to_string(),
                    name: "Baked Goods".to_string(),
                    description: None,
                    items: vec![MenuItem {
                        name: "Rye Croissant".to_string(),
                        description: Some("Laminated with stone-ground rye flour.".to_string()),
                        price: Some("4.50".to_string()),
                        price_note: None,
                        contains_alcohol: Some(false),
                        dietary: vec!["vegetarian".to_string()],
                    }],
                },
            ],
        },
        seo: Some(SeoConfig {
            title: Some("Cedar & Rye Café — Fictional Starter Venue".to_string()),
            description: Some(
                "Fictional demonstration café starter manifest for Solo Website System."
                    .to_string(),
            ),
            routes: vec!["/".to_string()],
        }),
    }
}

/// Starter README.md for `solo init`.
#[must_use]
pub fn starter_readme(manifest_file: &str) -> String {
    format!(
        "# Solo Project\n\n\
         Starter manifest: `{manifest_file}`\n\n\
         ## Common commands\n\n\
         ```sh\n\
         solo check {manifest_file}\n\
         solo build {manifest_file} public\n\
         solo build {manifest_file} public --check\n\
         solo export {manifest_file} dist\n\
         ```\n"
    )
}

/// Example local `.dev.vars` template emitted by `solo export` (never commit `.dev.vars`).
pub const EXPORT_DEV_VARS_EXAMPLE: &str = "\
# Local-only secrets for `wrangler dev` (copy to .dev.vars; never commit .dev.vars).\n\
# Cloudflare Turnstile always-pass TEST secret for local development:\n\
TURNSTILE_SECRET_KEY=1x0000000000000000000000000000000AA\n\
# Uncomment ONLY for offline local dev without a Turnstile secret: skips verification.\n\
# DEV_MODE=true\n";

fn style_rule(id: &str, selector: &str, pseudo: Option<&str>, decls: &[(&str, &str)]) -> IrStyleRule {
    let mut declarations = BTreeMap::new();
    for (k, v) in decls {
        declarations.insert((*k).to_string(), (*v).to_string());
    }
    IrStyleRule {
        id: id.to_string(),
        selector: selector.to_string(),
        breakpoint: None,
        pseudo: pseudo.map(str::to_string),
        declarations,
    }
}

fn starter_styles() -> Vec<IrStyleRule> {
    vec![
        style_rule(
            "body",
            "body",
            None,
            &[
                ("margin", "0"),
                ("font-family", "var(--font-body)"),
                ("background-color", "var(--background)"),
                ("color", "var(--text)"),
                ("line-height", "1.6"),
            ],
        ),
        style_rule(
            "skip-link",
            ".skip-link",
            None,
            &[
                ("position", "absolute"),
                ("left", "-9999px"),
                ("top", "0"),
                ("padding", "0.65rem 1rem"),
                ("background-color", "var(--text)"),
                ("color", "var(--background)"),
                ("font-weight", "700"),
                ("text-decoration", "none"),
                ("border-radius", "var(--radius-card)"),
            ],
        ),
        style_rule(
            "skip-link-focus",
            ".skip-link",
            Some(":focus"),
            &[
                ("position", "fixed"),
                ("left", "0.75rem"),
                ("top", "0.75rem"),
                ("z-index", "1000"),
            ],
        ),
        style_rule(
            "focus-visible",
            ":focus-visible",
            None,
            &[
                ("outline", "3px solid var(--accent)"),
                ("outline-offset", "2px"),
            ],
        ),
        style_rule(
            "site-header",
            ".site-header",
            None,
            &[
                ("display", "flex"),
                ("flex-wrap", "wrap"),
                ("align-items", "center"),
                ("justify-content", "space-between"),
                ("gap", "var(--space-md)"),
                ("padding", "var(--space-md) var(--space-lg)"),
                ("border-bottom", "1px solid var(--surface)"),
            ],
        ),
        style_rule(
            "site-brand",
            ".site-brand",
            None,
            &[("margin", "0"), ("font-weight", "700"), ("font-size", "1.125rem")],
        ),
        style_rule(
            "primary-nav",
            ".primary-nav",
            None,
            &[("display", "flex"), ("flex-wrap", "wrap"), ("gap", "var(--space-md)")],
        ),
        style_rule(
            "primary-nav-link",
            ".primary-nav a",
            None,
            &[
                ("color", "var(--accent)"),
                ("font-weight", "600"),
                ("min-height", "44px"),
                ("display", "inline-flex"),
                ("align-items", "center"),
            ],
        ),
        style_rule(
            "site-main",
            ".site-main",
            None,
            &[
                ("max-width", "52rem"),
                ("margin", "0 auto"),
                ("padding", "var(--space-lg)"),
            ],
        ),
        style_rule(
            "card",
            ".card",
            None,
            &[
                ("background-color", "var(--surface)"),
                ("padding", "var(--space-lg)"),
                ("border-radius", "var(--radius-card)"),
                ("margin-bottom", "var(--space-lg)"),
            ],
        ),
        style_rule(
            "field",
            ".field",
            None,
            &[
                ("display", "flex"),
                ("flex-direction", "column"),
                ("gap", "0.35rem"),
                ("margin-bottom", "var(--space-md)"),
            ],
        ),
        style_rule(
            "field-control",
            ".field input, .field textarea",
            None,
            &[
                ("font", "inherit"),
                ("padding", "0.6rem 0.75rem"),
                ("border", "1px solid var(--muted_text)"),
                ("border-radius", "0.25rem"),
                ("background-color", "var(--background)"),
                ("color", "var(--text)"),
                ("min-height", "44px"),
            ],
        ),
        style_rule(
            "btn",
            ".btn",
            None,
            &[
                ("font", "inherit"),
                ("font-weight", "700"),
                ("padding", "0.65rem 1.25rem"),
                ("border", "0"),
                ("border-radius", "0.375rem"),
                ("background-color", "var(--accent)"),
                ("color", "var(--on_accent)"),
                ("min-height", "44px"),
                ("cursor", "pointer"),
            ],
        ),
        style_rule(
            "honeypot",
            ".hp",
            None,
            &[
                ("position", "absolute"),
                ("left", "-9999px"),
                ("width", "1px"),
                ("height", "1px"),
                ("overflow", "hidden"),
            ],
        ),
        style_rule(
            "site-footer",
            ".site-footer",
            None,
            &[
                ("background-color", "var(--footer_background)"),
                ("color", "var(--footer_text)"),
                ("padding", "var(--space-lg)"),
                ("margin-top", "var(--space-lg)"),
            ],
        ),
    ]
}

/// Render a Cloudflare Workers `wrangler.jsonc` configuration for `solo export`.
#[must_use]
pub fn render_export_wrangler_jsonc(slug: &str, base_url: &str) -> String {
    let worker_name = sanitize_worker_name(slug);
    let host = extract_host(base_url).unwrap_or("site.example");
    let config = json!({
        "name": worker_name,
        "main": "build/index.js",
        "compatibility_date": "2026-11-01",
        "workers_dev": true,
        "build": {
            "command": "worker-build --release",
            "watch_dir": "src"
        },
        "assets": {
            "directory": "./public",
            "binding": "ASSETS",
            "run_worker_first": ["/api/*"],
            "not_found_handling": "404-page",
            "html_handling": "auto-trailing-slash"
        },
        "d1_databases": [
            {
                "binding": "DB",
                "database_name": format!("{worker_name}-db"),
                "migrations_dir": "migrations",
                "database_id": "00000000-0000-0000-0000-000000000000"
            }
        ],
        "vars": {
            "BUILD_VERSION": "0.1.0",
            "TURNSTILE_SITE_KEY": "1x00000000000000000000AA",
            "RESEND_FROM": format!("noreply@{host}"),
            "FORM_TO": format!("hello@{host}"),
            "INQUIRY_TO": format!("hello@{host}")
        },
        "observability": {
            "enabled": true
        }
    });
    let pretty = serde_json::to_string_pretty(&config).unwrap_or_else(|_| "{}".to_string());
    format!(
        "// Cloudflare Workers + Static Assets + D1 export generated by `solo export`.\n\
         // Requires: `cargo install worker-build@^0.9 --locked` (Rust wasm32-unknown-unknown target).\n\
         // Replace `database_id` and `TURNSTILE_SITE_KEY` before production deploy.\n\
         {pretty}\n"
    )
}

/// Render a standalone Rust Cloudflare Worker `Cargo.toml` for `solo export`.
#[must_use]
pub fn render_export_cargo_toml(slug: &str) -> String {
    let pkg_name = sanitize_worker_name(slug);
    format!(
        r#"[package]
name = "{pkg_name}"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
serde_json = "1"

[target.'cfg(target_arch = "wasm32")'.dependencies]
worker = {{ version = "0.9", features = ["d1"] }}
console_error_panic_hook = {{ version = "0.1", optional = true }}

[features]
default = ["console_error_panic_hook"]

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = "debuginfo"
panic = "abort"
"#
    )
}

/// Render an exported project `README.md` with build, D1 migration, and deploy instructions.
#[must_use]
pub fn render_export_readme(slug: &str) -> String {
    let worker_name = sanitize_worker_name(slug);
    format!(
        "# {worker_name}\n\n\
         Standalone Rust Cloudflare Worker + Static Assets + D1 bundle exported by `solo export`.\n\n\
         ## Local development\n\n\
         ```sh\n\
         rustup target add wasm32-unknown-unknown\n\
         cargo install worker-build@^0.9 --locked\n\
         cp .dev.vars.example .dev.vars\n\
         npx wrangler d1 migrations apply DB --local\n\
         npx wrangler dev --port 8787\n\
         ```\n\n\
         ## Production deploy\n\n\
         1. Run `npx wrangler d1 create {worker_name}-db` and place the returned `database_id` into `wrangler.jsonc`.\n\
         2. Set your Turnstile secret (`npx wrangler secret put TURNSTILE_SECRET_KEY`) and replace `TURNSTILE_SITE_KEY` in `wrangler.jsonc`.\n\
         3. Apply remote migrations (`npx wrangler d1 migrations apply DB --remote`) and run `npx wrangler deploy`.\n"
    )
}

fn escape_rust_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

/// Render a self-contained `src/lib.rs` Cloudflare Worker enforcing dual security headers,
/// bounded form/newsletter validation, Turnstile verification, and D1 persistence.
#[must_use]
pub fn render_export_worker_lib() -> String {
    let mut headers_entries = String::new();
    for (name, value) in SECURITY_HEADERS {
        headers_entries.push_str(&format!(
            "    (\"{}\", \"{}\"),\n",
            escape_rust_str(name),
            escape_rust_str(value),
        ));
    }

    format!(
        r#"//! Standalone Cloudflare Worker + Static Assets entrypoint generated by `solo export`.
//! Keeps pure routing, validation, and header logic host-testable (`cargo test`).

use std::collections::BTreeMap;

use serde_json::{{Value, json}};

pub const API_CACHE_CONTROL: &str = "{API_CACHE_CONTROL}";
pub const MAX_BODY_BYTES: usize = 16 * 1024;
pub const MAX_TURNSTILE_TOKEN: usize = 2048;
pub const HONEYPOT_FIELD: &str = "website";
pub const TURNSTILE_FIELD: &str = "cf-turnstile-response";

pub const SECURITY_HEADERS: &[(&str, &str)] = &[
{headers_entries}];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {{
    Health,
    HealthHead,
    FormSubmit(String),
    InquireCompat,
    Newsletter,
    MethodNotAllowed(&'static str),
    NotFound,
    Asset,
}}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {{
    Json,
    FormUrlEncoded,
}}

#[must_use]
pub fn is_valid_form_id(id: &str) -> bool {{
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9')
        && bytes[1..]
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}}

#[must_use]
pub fn route(method: &str, path: &str) -> Route {{
    let m = method.to_ascii_uppercase();
    match path {{
        "/api/health" => match m.as_str() {{
            "GET" => Route::Health,
            "HEAD" => Route::HealthHead,
            _ => Route::MethodNotAllowed("GET, HEAD"),
        }},
        "/api/inquire" => match m.as_str() {{
            "POST" => Route::InquireCompat,
            _ => Route::MethodNotAllowed("POST"),
        }},
        "/api/newsletter" => match m.as_str() {{
            "POST" => Route::Newsletter,
            _ => Route::MethodNotAllowed("POST"),
        }},
        p if p.starts_with("/api/forms/") => {{
            let id = &p["/api/forms/".len()..];
            if is_valid_form_id(id) {{
                match m.as_str() {{
                    "POST" => Route::FormSubmit(id.to_string()),
                    _ => Route::MethodNotAllowed("POST"),
                }}
            }} else {{
                Route::NotFound
            }}
        }}
        p if p == "/api" || p.starts_with("/api/") => Route::NotFound,
        _ => Route::Asset,
    }}
}}

#[must_use]
pub fn health_body(version: &str) -> Value {{
    json!({{
        "status": "ok",
        "service": env!("CARGO_PKG_NAME"),
        "version": version,
    }})
}}

#[must_use]
pub fn has_disallowed_control(s: &str, allow_newlines: bool) -> bool {{
    s.chars()
        .any(|c| c.is_control() && !(allow_newlines && matches!(c, '\n' | '\r' | '\t')))
}}

#[must_use]
pub fn is_valid_email(raw: &str) -> bool {{
    let email = raw.trim();
    if email.len() < 3 || email.len() > 254 {{
        return false;
    }}
    let Some((local, domain)) = email.split_once('@') else {{
        return false;
    }};
    if local.is_empty() || local.len() > 64 || domain.contains('@') {{
        return false;
    }}
    let local_ok = local.bytes().all(|b| {{
        b.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{{|}}~.-".contains(&b)
    }}) && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..");
    let labels: Vec<&str> = domain.split('.').collect();
    let tld_ok = labels
        .last()
        .is_some_and(|tld| tld.len() >= 2 && tld.bytes().any(|b| b.is_ascii_alphabetic()));
    let domain_ok = labels.len() >= 2
        && tld_ok
        && labels.iter().all(|l| {{
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }});
    local_ok && domain_ok
}}

#[must_use]
pub fn is_valid_route(raw: &str) -> bool {{
    let route = raw.trim();
    route.starts_with('/')
        && route.len() <= 200
        && !route.contains("//")
        && !route.split('/').any(|seg| seg == "..")
        && route
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
}}

#[must_use]
pub fn is_turnstile_test_secret(secret: &str) -> bool {{
    matches!(
        secret.trim(),
        "1x0000000000000000000000000000000AA"
            | "2x0000000000000000000000000000000AA"
            | "3x0000000000000000000000000000000AA"
    )
}}

#[must_use]
pub fn is_valid_phone(raw: &str) -> bool {{
    let p = raw.trim();
    if p.is_empty() || p.len() > 32 {{
        return false;
    }}
    let mut digits = 0usize;
    for b in p.bytes() {{
        if b.is_ascii_digit() {{
            digits += 1;
        }} else if !matches!(b, b' ' | b'+' | b'-' | b'(' | b')' | b'.' | b'x' | b'X') {{
            return false;
        }}
    }}
    digits >= 7
}}

#[must_use]
pub fn is_valid_date(raw: &str) -> bool {{
    let b = raw.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {{
        return false;
    }}
    let Ok(year) = raw[0..4].parse::<u32>() else {{
        return false;
    }};
    let Ok(month) = raw[5..7].parse::<u32>() else {{
        return false;
    }};
    let Ok(day) = raw[8..10].parse::<u32>() else {{
        return false;
    }};
    if !(1900..=2100).contains(&year) || !(1..=12).contains(&month) || day == 0 {{
        return false;
    }}
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let max_day = match month {{
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    }};
    day <= max_day
}}

#[must_use]
pub fn parse_content_type(content_type: Option<&str>) -> Option<MediaType> {{
    let mut parts = content_type?.split(';');
    let mime = parts.next()?.trim().to_ascii_lowercase();
    for param in parts {{
        if let Some((k, v)) = param.split_once('=')
            && k.trim().eq_ignore_ascii_case("charset")
        {{
            let cs = v.trim().trim_matches('"').trim_matches('\'');
            if !cs.eq_ignore_ascii_case("utf-8") && !cs.eq_ignore_ascii_case("utf8") {{
                return None;
            }}
        }}
    }}
    match mime.as_str() {{
        "application/json" => Some(MediaType::Json),
        "application/x-www-form-urlencoded" => Some(MediaType::FormUrlEncoded),
        _ => None,
    }}
}}

pub fn parse_fields(content_type: Option<&str>, body: &[u8]) -> Result<BTreeMap<String, String>, &'static str> {{
    let media_type = parse_content_type(content_type).ok_or("unsupported_media_type")?;
    if body.len() > MAX_BODY_BYTES {{
        return Err("payload_too_large");
    }}
    let text = std::str::from_utf8(body).map_err(|_| "malformed_body")?;
    let mut out = BTreeMap::new();
    match media_type {{
        MediaType::Json => {{
            let val: Value = serde_json::from_str(text).map_err(|_| "malformed_body")?;
            let obj = val.as_object().ok_or("malformed_body")?;
            for (k, v) in obj {{
                let s = match v {{
                    Value::Null => continue,
                    Value::String(s) => s.clone(),
                    Value::Bool(b) => b.to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => return Err("malformed_body"),
                }};
                out.insert(k.clone(), s);
            }}
            Ok(out)
        }}
        MediaType::FormUrlEncoded => {{
            for pair in text.split('&') {{
                if pair.is_empty() {{
                    continue;
                }}
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                out.insert(decode_form_component(k)?, decode_form_component(v)?);
            }}
            Ok(out)
        }}
    }}
}}

#[must_use]
pub fn form_encode(pairs: &[(&str, &str)]) -> String {{
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {{
        if i > 0 {{
            out.push('&');
        }}
        for (j, part) in [k, v].iter().enumerate() {{
            if j == 1 {{
                out.push('=');
            }}
            for b in part.bytes() {{
                match b {{
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {{
                        out.push(b as char);
                    }}
                    _ => out.push_str(&format!("%{{b:02X}}")),
                }}
            }}
        }}
    }}
    out
}}

#[must_use]
pub fn iso8601_utc(millis: u64) -> String {{
    let secs = millis / 1000;
    let ms = millis % 1000;
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 {{ mp + 3 }} else {{ mp - 9 }};
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{{y:04}}-{{m:02}}-{{d:02}}T{{:02}}:{{:02}}:{{:02}}.{{ms:03}}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}}

fn decode_form_component(raw: &str) -> Result<String, &'static str> {{
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {{
        match bytes[i] {{
            b'+' => {{
                decoded.push(b' ');
                i += 1;
            }}
            b'%' => {{
                if i + 2 >= bytes.len() {{
                    return Err("malformed_body");
                }}
                let hi = (bytes[i + 1] as char).to_digit(16).ok_or("malformed_body")? as u8;
                let lo = (bytes[i + 2] as char).to_digit(16).ok_or("malformed_body")? as u8;
                decoded.push((hi << 4) | lo);
                i += 3;
            }}
            b => {{
                decoded.push(b);
                i += 1;
            }}
        }}
    }}
    String::from_utf8(decoded).map_err(|_| "malformed_body")
}}

#[cfg(target_arch = "wasm32")]
mod wasm {{
    use std::future::Future;
    use std::task::Poll;
    use std::time::Duration;

    use serde_json::json;
    use worker::wasm_bindgen::{{JsCast, JsValue}};
    use worker::wasm_bindgen_futures::JsFuture;
    use worker::{{
        AbortSignal, Context, Delay, Env, Fetch, Headers, Method, Request, RequestInit, Response,
        Result, console_error, console_warn, event, js_sys, web_sys,
    }};

    use super::{{
        API_CACHE_CONTROL, HONEYPOT_FIELD, MAX_BODY_BYTES, MAX_TURNSTILE_TOKEN, Route,
        SECURITY_HEADERS, TURNSTILE_FIELD, form_encode, has_disallowed_control, health_body,
        is_turnstile_test_secret, is_valid_date, is_valid_email, is_valid_phone, is_valid_route,
        iso8601_utc, parse_content_type, parse_fields, route,
    }};

    const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
    const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);

    #[event(start)]
    fn start() {{
        #[cfg(feature = "console_error_panic_hook")]
        console_error_panic_hook::set_once();
    }}

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {{
        let resp = match handle(req, &env).await {{
            Ok(r) => r,
            Err(e) => {{
                console_error!("unhandled error: {{e}}");
                json_response(500, &json!({{ "ok": false, "error": "internal_error" }}))?
            }}
        }};
        with_security_headers(resp)
    }}

    fn with_security_headers(resp: Response) -> Result<Response> {{
        let headers = Headers::new();
        for (name, value) in resp.headers().entries() {{
            headers.append(&name, &value)?;
        }}
        for (name, value) in SECURITY_HEADERS {{
            headers.set(name, value)?;
        }}
        Ok(resp.with_headers(headers))
    }}

    fn json_response(status: u16, body: &serde_json::Value) -> Result<Response> {{
        let resp = Response::from_json(body)?.with_status(status);
        resp.headers().set("Cache-Control", API_CACHE_CONTROL)?;
        Ok(resp)
    }}

    fn var(env: &Env, name: &str) -> Option<String> {{
        env.var(name)
            .ok()
            .map(|v| v.to_string().trim().to_string())
            .filter(|s| !s.is_empty())
    }}

    fn secret(env: &Env, name: &str) -> Option<String> {{
        env.secret(name)
            .ok()
            .map(|v| v.to_string().trim().to_string())
            .filter(|s| !s.is_empty())
    }}

    async fn with_timeout<F, T>(dur: Duration, fut: F) -> std::result::Result<T, ()>
    where
        F: Future<Output = T>,
    {{
        let mut fut = std::pin::pin!(fut);
        let mut delay = std::pin::pin!(Delay::from(dur));
        std::future::poll_fn(|cx| {{
            if let Poll::Ready(v) = fut.as_mut().poll(cx) {{
                return Poll::Ready(Ok(v));
            }}
            if delay.as_mut().poll(cx).is_ready() {{
                return Poll::Ready(Err(()));
            }}
            Poll::Pending
        }})
        .await
    }}

    async fn handle(mut req: Request, env: &Env) -> Result<Response> {{
        let method = req.method();
        let path = req.path();
        match route(method.as_ref(), &path) {{
            Route::Health => {{
                let version = var(env, "BUILD_VERSION")
                    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
                json_response(200, &health_body(&version))
            }}
            Route::HealthHead => {{
                let resp = Response::empty()?.with_status(200);
                resp.headers().set("Content-Type", "application/json")?;
                resp.headers().set("Cache-Control", API_CACHE_CONTROL)?;
                Ok(resp)
            }}
            Route::FormSubmit(form_id) => submit_form(&mut req, env, &form_id, false).await,
            Route::InquireCompat => submit_form(&mut req, env, "inquire", true).await,
            Route::Newsletter => submit_newsletter(&mut req, env).await,
            Route::MethodNotAllowed(allow) => {{
                let resp = json_response(405, &json!({{ "ok": false, "error": "method_not_allowed" }}))?;
                resp.headers().set("Allow", allow)?;
                Ok(resp)
            }}
            Route::NotFound => json_response(404, &json!({{ "ok": false, "error": "not_found" }})),
            Route::Asset => env.assets("ASSETS")?.fetch_request(req).await,
        }}
    }}

    async fn read_body_fields(
        req: &mut Request,
    ) -> Result<std::result::Result<std::collections::BTreeMap<String, String>, Response>> {{
        let ct = req.headers().get("Content-Type")?;
        if parse_content_type(ct.as_deref()).is_none() {{
            return Ok(Err(json_response(
                415,
                &json!({{ "ok": false, "error": "unsupported_media_type" }}),
            )?));
        }}
        if let Some(len) = req
            .headers()
            .get("Content-Length")?
            .and_then(|v| v.trim().parse::<usize>().ok())
            && len > MAX_BODY_BYTES
        {{
            return Ok(Err(json_response(
                413,
                &json!({{ "ok": false, "error": "payload_too_large" }}),
            )?));
        }}
        let bytes = match req.inner().body() {{
            None => Vec::new(),
            Some(stream) => {{
                let Ok(reader) = stream
                    .get_reader()
                    .dyn_into::<web_sys::ReadableStreamDefaultReader>()
                else {{
                    return Ok(Err(json_response(
                        400,
                        &json!({{ "ok": false, "error": "malformed_body" }}),
                    )?));
                }};
                let mut buf = Vec::new();
                loop {{
                    let chunk_js = match JsFuture::from(reader.read()).await {{
                        Ok(v) => v,
                        Err(_) => {{
                            reader.release_lock();
                            return Ok(Err(json_response(
                                400,
                                &json!({{ "ok": false, "error": "malformed_body" }}),
                            )?));
                        }}
                    }};
                    let done = js_sys::Reflect::get(&chunk_js, &JsValue::from_str("done"))
                        .ok()
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if done {{
                        break;
                    }}
                    let Ok(val) = js_sys::Reflect::get(&chunk_js, &JsValue::from_str("value")) else {{
                        reader.release_lock();
                        return Ok(Err(json_response(
                            400,
                            &json!({{ "ok": false, "error": "malformed_body" }}),
                        )?));
                    }};
                    if val.is_undefined() || val.is_null() {{
                        continue;
                    }}
                    let Ok(arr) = val.dyn_into::<js_sys::Uint8Array>() else {{
                        reader.release_lock();
                        return Ok(Err(json_response(
                            400,
                            &json!({{ "ok": false, "error": "malformed_body" }}),
                        )?));
                    }};
                    let chunk_len = arr.length() as usize;
                    if buf.len().saturating_add(chunk_len) > MAX_BODY_BYTES {{
                        let _ = reader.cancel();
                        reader.release_lock();
                        return Ok(Err(json_response(
                            413,
                            &json!({{ "ok": false, "error": "payload_too_large" }}),
                        )?));
                    }}
                    let old_len = buf.len();
                    buf.resize(old_len + chunk_len, 0);
                    arr.copy_to(&mut buf[old_len..]);
                }}
                reader.release_lock();
                buf
            }}
        }};
        match parse_fields(ct.as_deref(), &bytes) {{
            Ok(fields) => Ok(Ok(fields)),
            Err("unsupported_media_type") => Ok(Err(json_response(
                415,
                &json!({{ "ok": false, "error": "unsupported_media_type" }}),
            )?)),
            Err("payload_too_large") => Ok(Err(json_response(
                413,
                &json!({{ "ok": false, "error": "payload_too_large" }}),
            )?)),
            Err(code) => Ok(Err(json_response(
                400,
                &json!({{ "ok": false, "error": code }}),
            )?)),
        }}
    }}

    async fn verify_turnstile(
        env: &Env,
        req: &Request,
        token: Option<&str>,
        expected_action: &str,
    ) -> Result<Option<Response>> {{
        let dev_mode = var(env, "DEV_MODE").as_deref() == Some("true");
        let Some(secret_key) = secret(env, "TURNSTILE_SECRET_KEY") else {{
            if dev_mode {{
                console_warn!("TURNSTILE_SECRET_KEY missing; DEV_MODE=true skips verification");
                return Ok(None);
            }}
            return Ok(Some(json_response(
                503,
                &json!({{ "ok": false, "error": "turnstile_unconfigured" }}),
            )?));
        }};
        let relax_dev_checks = dev_mode && is_turnstile_test_secret(&secret_key);
        let Some(token) = token.map(str::trim).filter(|s| !s.is_empty()) else {{
            return Ok(Some(json_response(
                400,
                &json!({{ "ok": false, "error": "turnstile_required" }}),
            )?));
        }};
        if token.len() > MAX_TURNSTILE_TOKEN {{
            return Ok(Some(json_response(
                400,
                &json!({{
                    "ok": false,
                    "error": "validation_failed",
                    "fields": {{ TURNSTILE_FIELD: "too_long" }}
                }}),
            )?));
        }}
        let ip = req.headers().get("CF-Connecting-IP")?.unwrap_or_default();
        let mut pairs = vec![("secret", secret_key.as_str()), ("response", token)];
        if !ip.is_empty() {{
            pairs.push(("remoteip", ip.as_str()));
        }}
        let headers = Headers::new();
        headers.set("Content-Type", "application/x-www-form-urlencoded")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(JsValue::from_str(&form_encode(&pairs))));
        let outbound = Request::new_with_init(SITEVERIFY_URL, &init)?;
        let controller = web_sys::AbortController::new()?;
        let signal = AbortSignal::from(controller.signal());
        let verdict = match with_timeout(OUTBOUND_TIMEOUT, async {{
            match Fetch::Request(outbound).send_with_signal(&signal).await {{
                Ok(mut resp) => {{
                    let status = resp.status_code();
                    if !(200..300).contains(&status) {{
                        console_error!("turnstile siteverify HTTP {{status}}");
                        return None;
                    }}
                    resp.json::<serde_json::Value>().await.ok()
                }}
                Err(e) => {{
                    console_error!("turnstile siteverify fetch failed: {{e}}");
                    None
                }}
            }}
        }})
        .await
        {{
            Ok(v) => v,
            Err(()) => {{
                controller.abort();
                console_error!("turnstile siteverify timed out");
                None
            }}
        }};
        let Some(verdict) = verdict else {{
            return Ok(Some(json_response(
                503,
                &json!({{ "ok": false, "error": "turnstile_unavailable" }}),
            )?));
        }};
        if verdict["success"].as_bool() == Some(true) {{
            if !relax_dev_checks {{
                let action = verdict["action"].as_str().unwrap_or("");
                if action != expected_action {{
                    console_warn!("turnstile action mismatch: expected {{expected_action}}, got {{action:?}}");
                    return Ok(Some(json_response(
                        403,
                        &json!({{ "ok": false, "error": "turnstile_failed" }}),
                    )?));
                }}
                if let Some(raw_hosts) = var(env, "ALLOWED_HOSTNAMES") {{
                    let hostname = verdict["hostname"].as_str().unwrap_or("").trim();
                    let allowed: Vec<&str> = raw_hosts
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !allowed.is_empty()
                        && !allowed.iter().any(|h| h.eq_ignore_ascii_case(hostname))
                    {{
                        console_warn!("turnstile hostname rejected: {{hostname:?}}");
                        return Ok(Some(json_response(
                            403,
                            &json!({{ "ok": false, "error": "turnstile_failed" }}),
                        )?));
                    }}
                }}
            }}
            Ok(None)
        }} else {{
            Ok(Some(json_response(
                403,
                &json!({{ "ok": false, "error": "turnstile_failed" }}),
            )?))
        }}
    }}

    async fn submit_form(
        req: &mut Request,
        env: &Env,
        form_id: &str,
        compat_inquire: bool,
    ) -> Result<Response> {{
        let fields = match read_body_fields(req).await? {{
            Ok(f) => f,
            Err(r) => return Ok(r),
        }};
        if fields.get(HONEYPOT_FIELD).is_some_and(|v| !v.trim().is_empty()) {{
            return json_response(400, &json!({{ "ok": false, "error": "submission_rejected" }}));
        }}

        let name = fields.get("name").map(|s| s.trim()).unwrap_or("");
        let email = fields.get("email").map(|s| s.trim()).unwrap_or("");
        let phone = fields.get("phone").map(|s| s.trim()).filter(|s| !s.is_empty());
        let message = fields.get("message").map(|s| s.trim()).unwrap_or("");
        let source_route = fields.get("source_route").map(|s| s.trim()).unwrap_or("/");
        let kind = fields.get("kind").map(|s| s.trim()).filter(|s| !s.is_empty());
        let event_date = fields.get("event_date").map(|s| s.trim()).filter(|s| !s.is_empty());
        let guest_count_raw = fields.get("guest_count").map(|s| s.trim()).filter(|s| !s.is_empty());
        let token = fields.get(TURNSTILE_FIELD).map(|s| s.trim()).filter(|s| !s.is_empty());

        let mut errs = serde_json::Map::new();
        if name.is_empty() {{
            errs.insert("name".into(), json!("required"));
        }} else if name.chars().count() > 100 {{
            errs.insert("name".into(), json!("too_long"));
        }} else if has_disallowed_control(name, false) {{
            errs.insert("name".into(), json!("invalid_characters"));
        }}
        if !is_valid_email(email) {{
            errs.insert("email".into(), json!(if email.is_empty() {{ "required" }} else {{ "invalid" }}));
        }}
        if let Some(p) = phone
            && !is_valid_phone(p)
        {{
            errs.insert("phone".into(), json!("invalid"));
        }}
        if message.is_empty() {{
            errs.insert("message".into(), json!("required"));
        }} else if message.chars().count() > 4000 {{
            errs.insert("message".into(), json!("too_long"));
        }} else if has_disallowed_control(message, true) {{
            errs.insert("message".into(), json!("invalid_characters"));
        }}
        if !is_valid_route(source_route) {{
            errs.insert("source_route".into(), json!("invalid"));
        }}
        let mut guest_count_num: Option<u32> = None;
        if compat_inquire {{
            if let Some(k) = kind
                && !matches!(k, "private_event" | "contact")
            {{
                errs.insert("kind".into(), json!("invalid"));
            }}
            if let Some(d) = event_date
                && !is_valid_date(d)
            {{
                errs.insert("event_date".into(), json!("invalid"));
            }}
            if let Some(gc) = guest_count_raw {{
                match gc.parse::<u32>() {{
                    Ok(n) if (1..=1000).contains(&n) => guest_count_num = Some(n),
                    _ => {{
                        errs.insert("guest_count".into(), json!("invalid"));
                    }}
                }}
            }}
        }}
        if token.is_some_and(|t| t.len() > MAX_TURNSTILE_TOKEN) {{
            errs.insert(TURNSTILE_FIELD.into(), json!("too_long"));
        }}
        if !errs.is_empty() {{
            return json_response(
                400,
                &json!({{ "ok": false, "error": "validation_failed", "fields": errs }}),
            );
        }}

        let expected_action = if compat_inquire {{ "inquire" }} else {{ form_id }};
        if let Some(resp) = verify_turnstile(env, req, token, expected_action).await? {{
            return Ok(resp);
        }}

        let mut payload_obj = serde_json::Map::new();
        payload_obj.insert("name".into(), json!(name));
        payload_obj.insert("email".into(), json!(email.to_ascii_lowercase()));
        if let Some(p) = phone {{
            payload_obj.insert("phone".into(), json!(p));
        }}
        payload_obj.insert("message".into(), json!(message));
        if compat_inquire {{
            if let Some(k) = kind {{
                payload_obj.insert("kind".into(), json!(k));
            }}
            if let Some(d) = event_date {{
                payload_obj.insert("event_date".into(), json!(d));
            }}
            if let Some(n) = guest_count_num {{
                payload_obj.insert("guest_count".into(), json!(n));
            }}
        }}
        let payload_json = serde_json::Value::Object(payload_obj).to_string();

        let stmt = env
            .d1("DB")?
            .prepare(
                "INSERT INTO form_submissions (form_id, payload_json, source_route) \
                 VALUES (?1, ?2, ?3) RETURNING id",
            )
            .bind(&[
                JsValue::from_str(form_id),
                JsValue::from_str(&payload_json),
                JsValue::from_str(source_route),
            ])?;
        let id = match stmt.first::<f64>(Some("id")).await {{
            Ok(Some(id)) => id as i64,
            Ok(None) => {{
                console_error!("form_submissions insert returned no id");
                return json_response(500, &json!({{ "ok": false, "error": "storage_failed" }}));
            }}
            Err(e) => {{
                console_error!("form_submissions insert failed: {{e}}");
                return json_response(500, &json!({{ "ok": false, "error": "storage_failed" }}));
            }}
        }};
        if compat_inquire {{
            json_response(201, &json!({{ "ok": true, "id": id }}))
        }} else {{
            json_response(201, &json!({{ "ok": true, "id": id, "form_id": form_id }}))
        }}
    }}

    async fn submit_newsletter(req: &mut Request, env: &Env) -> Result<Response> {{
        let fields = match read_body_fields(req).await? {{
            Ok(f) => f,
            Err(r) => return Ok(r),
        }};
        if fields.get(HONEYPOT_FIELD).is_some_and(|v| !v.trim().is_empty()) {{
            return json_response(400, &json!({{ "ok": false, "error": "submission_rejected" }}));
        }}

        let email = fields.get("email").map(|s| s.trim()).unwrap_or("");
        let consent_ok = fields
            .get("consent")
            .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"));
        let source_route = fields.get("source_route").map(|s| s.trim()).unwrap_or("/");
        let consent_text = fields
            .get("consent_text")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let token = fields.get(TURNSTILE_FIELD).map(|s| s.trim()).filter(|s| !s.is_empty());

        let mut errs = serde_json::Map::new();
        if !is_valid_email(email) {{
            errs.insert("email".into(), json!(if email.is_empty() {{ "required" }} else {{ "invalid" }}));
        }}
        if !consent_ok {{
            errs.insert("consent".into(), json!("required"));
        }}
        if !is_valid_route(source_route) {{
            errs.insert("source_route".into(), json!("invalid"));
        }}
        if let Some(ct) = consent_text {{
            if ct.chars().count() > 500 {{
                errs.insert("consent_text".into(), json!("too_long"));
            }} else if has_disallowed_control(ct, false) {{
                errs.insert("consent_text".into(), json!("invalid_characters"));
            }}
        }}
        if token.is_some_and(|t| t.len() > MAX_TURNSTILE_TOKEN) {{
            errs.insert(TURNSTILE_FIELD.into(), json!("too_long"));
        }}
        if !errs.is_empty() {{
            return json_response(
                400,
                &json!({{ "ok": false, "error": "validation_failed", "fields": errs }}),
            );
        }}

        if let Some(resp) = verify_turnstile(env, req, token, "newsletter").await? {{
            return Ok(resp);
        }}

        let now = iso8601_utc(worker::Date::now().as_millis());
        let consent_js = consent_text.map(JsValue::from_str).unwrap_or(JsValue::NULL);
        if let Err(e) = env
            .d1("DB")?
            .prepare(
                "INSERT INTO newsletter_subscribers (email, status, consent_timestamp, source_route, consent_text) \
                 VALUES (?1, 'subscribed', ?2, ?3, ?4) \
                 ON CONFLICT(email) DO NOTHING",
            )
            .bind(&[
                JsValue::from_str(&email.to_ascii_lowercase()),
                JsValue::from_str(&now),
                JsValue::from_str(source_route),
                consent_js,
            ])?
            .run()
            .await
        {{
            console_error!("newsletter insert failed: {{e}}");
            return json_response(500, &json!({{ "ok": false, "error": "storage_failed" }}));
        }}

        json_response(
            201,
            &json!({{ "ok": true, "status": "received", "consent_timestamp": now }}),
        )
    }}
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn routes_and_validation_work() {{
        assert_eq!(route("GET", "/api/health"), Route::Health);
        assert_eq!(
            route("POST", "/api/forms/contact"),
            Route::FormSubmit("contact".to_string())
        );
        assert_eq!(route("POST", "/api/inquire"), Route::InquireCompat);
        assert_eq!(route("POST", "/api/newsletter"), Route::Newsletter);
        assert_eq!(route("GET", "/"), Route::Asset);
        assert!(is_valid_email("guest@example.test"));
        assert!(!is_valid_email("bad@email"));
        assert!(!is_valid_email("bad\u{{0007}}@example.test"));
        assert!(is_valid_route("/contact"));
        assert!(!is_valid_route("//evil.example"));
        assert!(!is_valid_route("/a?b=1"));
        assert!(!is_valid_route("/../etc/passwd"));
        assert!(is_valid_phone("+1 (703) 555-0100"));
        assert!(!is_valid_phone("123"));
        assert!(is_valid_date("2028-02-29"));
        assert!(!is_valid_date("2027-02-29"));
        assert_eq!(
            form_encode(&[("secret", "live"), ("response", "tok&secret=test")]),
            "secret=live&response=tok%26secret%3Dtest"
        );
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert!(parse_content_type(Some("application/json; charset=utf-8")).is_some());
        assert!(parse_content_type(Some("application/json; charset=utf-16")).is_none());
        assert!(is_turnstile_test_secret("1x0000000000000000000000000000000AA"));
        assert!(!is_turnstile_test_secret("0x4AAAAAALiveSecret"));
        assert_eq!(SECURITY_HEADERS.len(), 7);
    }}
}}
"#
    )
}

fn sanitize_worker_name(slug: &str) -> String {
    let cleaned: String = slug
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "sws-site".to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_host(base_url: &str) -> Option<&str> {
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))?;
    let host_port = rest.split(['/', '?', '#']).next()?;
    let host = host_port.split(':').next()?;
    if host.is_empty() { None } else { Some(host) }
}

fn el<'a>(
    id: Option<&str>,
    tag: &str,
    attrs: impl IntoIterator<Item = (&'a str, &'a str)>,
    classes: &[&str],
    children: Vec<Node>,
) -> Node {
    let mut attr_map = BTreeMap::new();
    for (k, v) in attrs {
        attr_map.insert(k.to_string(), v.to_string());
    }
    Node::Element {
        id: id.map(str::to_string),
        tag: tag.to_string(),
        attrs: attr_map,
        classes: classes.iter().map(|s| (*s).to_string()).collect(),
        style_refs: Vec::new(),
        children,
    }
}

fn text(value: &str) -> Node {
    Node::Text {
        value: value.to_string(),
    }
}

fn form_input_group(
    input_id: &str,
    name: &str,
    label: &str,
    input_type: &str,
    autocomplete: &str,
    required: bool,
) -> Node {
    let mut attrs = vec![
        ("type", input_type),
        ("name", name),
        ("autocomplete", autocomplete),
    ];
    if required {
        attrs.push(("required", "required"));
    }
    el(
        None,
        "div",
        [],
        &["field"],
        vec![
            el(
                None,
                "label",
                [("for", input_id)],
                &[],
                vec![text(label)],
            ),
            el(Some(input_id), "input", attrs, &[], Vec::new()),
        ],
    )
}
