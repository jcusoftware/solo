//! Hospitality venue manifest types, validation, and schema.org JSON-LD builder.
//!
//! `packs/hospitality/schema/venue.schema.json` is generated from these types
//! (`jsonschema` feature, see [`schema`]); regenerate it with `npm run codegen`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[cfg(feature = "jsonschema")]
pub mod schema;

pub const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "jsonschema", schemars(title = "SWS venue manifest", description = "Tenant-agnostic hospitality manifest consumed by sws-pack-hospitality and sws-cli.", extend("required" = ["schema_version", "slug", "name", "business_types", "nap", "hours", "brand", "modules", "integrations", "menu"])))]
pub struct VenueManifest {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[cfg_attr(feature = "jsonschema", schemars(extend("const" = 1)))]
    pub schema_version: u32,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9]+(-[a-z0-9]+)*$")))]
    pub slug: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1, "maxLength" = 120)))]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::legal_disclaimer"))]
    pub legal_disclaimer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fictional: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_label: Option<String>,
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::business_types"))]
    pub business_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuisine: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^\\${1,4}$")))]
    pub price_range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub nap: Nap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo: Option<Geo>,
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::hours"))]
    pub hours: BTreeMap<String, DayHours>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holiday_overrides: Vec<HolidayOverride>,
    #[serde(default)]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::booking_url"))]
    pub booking_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub venue_accessibility: Vec<String>,
    pub brand: Brand,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::modules"))]
    pub modules: BTreeMap<String, ModuleFlag>,
    pub integrations: Integrations,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newsletter: Option<NewsletterConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling: Option<SchedulingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub happy_hour: Option<HappyHour>,
    pub menu: Menu,
    /// Optional sandbox demo checkout configuration; absent means the venue
    /// renders no checkout pages, catalog, or nav links at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<CheckoutConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seo: Option<SeoConfig>,
}

/// One fixed-price sandbox checkout catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct CheckoutItem {
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z][a-z0-9-]{0,31}$")))]
    pub id: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub name: String,
    /// USD cents (50..=10000); the demo never takes variable amounts.
    #[cfg_attr(feature = "jsonschema", schemars(extend("minimum" = 50, "maximum" = 10000)))]
    pub cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct CheckoutConfig {
    /// 2–5 one-time pickup items.
    #[cfg_attr(feature = "jsonschema", schemars(extend("minItems" = 2, "maxItems" = 5)))]
    pub items: Vec<CheckoutItem>,
    /// Exactly one fixed monthly subscription item.
    pub subscription: CheckoutItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Nap {
    pub name: String,
    pub address: Address,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^\\+[0-9-]{7,20}$")))]
    pub phone: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("format" = "email")))]
    pub email: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^https://")))]
    pub website: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Address {
    pub street: String,
    pub locality: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[A-Z]{2}$")))]
    pub region: String,
    pub postal_code: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[A-Z]{2}$")))]
    pub country: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Geo {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minimum" = -90, "maximum" = 90)))]
    pub latitude: f64,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minimum" = -180, "maximum" = 180)))]
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approximate: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::day_hours"))]
pub struct DayHours {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "jsonschema", schemars(extend("oneOf" = [{"required": ["closed"], "properties": {"closed": {"const": true}}, "not": {"anyOf": [{"required": ["opens"]}, {"required": ["closes"]}]}}, {"required": ["opens", "closes"], "properties": {"closed": {"const": false}}}])))]
pub struct HolidayOverride {
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[0-9]{4}-[0-9]{2}-[0-9]{2}$")))]
    pub date: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^([01][0-9]|2[0-3]):[0-5][0-9]$")))]
    pub opens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^([01][0-9]|2[0-3]):[0-5][0-9]$")))]
    pub closes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Brand {
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::palette"))]
    pub palette: BTreeMap<String, String>,
    pub contrast: ContrastConfig,
    pub fonts: Fonts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ContrastConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wcag_target: Option<String>,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minItems" = 1)))]
    pub pairs: Vec<ContrastPair>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ContrastPair {
    pub foreground: String,
    pub background: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minimum" = 4.5)))]
    pub ratio: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Fonts {
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ModuleFlag {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Integrations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toast: Option<ToastConfig>,
    #[cfg_attr(feature = "jsonschema", schemars(extend("enum" = ["walk_in_only", "opentable", "resy"])))]
    pub seating_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opentable: Option<OpenTableConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resy: Option<ResyConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inkind: Option<InKindConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turnstile: Option<TurnstileConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ToastConfig {
    pub enabled: bool,
    #[serde(default)]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::nullable_string"))]
    pub restaurant_guid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct OpenTableConfig {
    #[serde(default)]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::nullable_string"))]
    pub restaurant_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct ResyConfig {
    #[serde(default)]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::nullable_string"))]
    pub venue_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct InKindConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct TurnstileConfig {
    pub site_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_key: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct NewsletterConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("maxLength" = 500)))]
    pub blurb: Option<String>,
    #[cfg_attr(feature = "jsonschema", schemars(extend("maxLength" = 500)))]
    pub consent_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_note: Option<String>,
}

/// Event source for the public events calendar (issue #43 port).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EventsSource {
    // Plain comments (not `///`): doc comments on variants make schemars emit
    // `oneOf` of `const`s instead of the flat `enum` the schema has always had.
    //
    // D1OrCms: manual rows / D1-managed records only.
    D1OrCms,
    // GoogleCalendarSync: scheduled Google Calendar snapshot import
    // (config-gated; secrets are provisioned in the Worker environment only,
    // never in the manifest).
    GoogleCalendarSync,
}

/// Public events calendar configuration (`scheduling.events`).
///
/// `google_calendar_id` is declarative only: the Worker reads the actual
/// calendar from its `GOOGLE_SYNC_CALENDAR_ID` secret at sync time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "jsonschema", schemars(extend("required" = ["enabled", "source", "google_calendar_id", "timezone"], "if" = {"properties": {"enabled": {"const": true}, "source": {"const": "google_calendar_sync"}}}, "then" = {"properties": {"google_calendar_id": {"type": "string", "pattern": "\\S"}}})))]
pub struct EventsConfig {
    pub enabled: bool,
    pub source: EventsSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::events_calendar_id"))]
    pub google_calendar_id: Option<String>,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1)))]
    pub timezone: String,
}

/// Scheduling configuration. Native call booking (issue #44) adds its own
/// section under `scheduling` on top of `events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct SchedulingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<EventsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookings: Option<BookingsConfig>,
}

/// Call booking configuration (`scheduling.bookings`).
///
/// Slots are derived from `working_hours` in the venue's IANA zone
/// (`scheduling.events.timezone`, which bookings require even when public
/// events stay disabled). `google_workspace_calendar_id` is declarative only:
/// the Worker reads the actual calendar from its
/// `GOOGLE_BOOKING_CALENDAR_ID` secret at request time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct BookingsConfig {
    pub enabled: bool,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 3, "maxLength" = 254, "format" = "email")))]
    pub google_workspace_calendar_id: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minimum" = 5, "maximum" = 240)))]
    pub slot_duration_minutes: u16,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minimum" = 0, "maximum" = 1440)))]
    pub buffer_minutes: u16,
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::working_hours"))]
    pub working_hours: BTreeMap<String, [String; 2]>,
    pub auto_zoom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct HappyHour {
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::happy_hour_days"))]
    pub days: Vec<String>,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^([01][0-9]|2[0-3]):[0-5][0-9]$")))]
    pub start: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^([01][0-9]|2[0-3]):[0-5][0-9]$")))]
    pub end: String,
    pub offers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct Menu {
    pub name: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[A-Z]{3}$")))]
    pub currency: String,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minItems" = 1)))]
    pub sections: Vec<MenuSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct MenuSection {
    #[cfg_attr(feature = "jsonschema", schemars(extend("pattern" = "^[a-z0-9]+(-[a-z0-9]+)*$")))]
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("maxLength" = 500)))]
    pub description: Option<String>,
    #[cfg_attr(feature = "jsonschema", schemars(extend("minItems" = 1)))]
    pub items: Vec<MenuItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct MenuItem {
    #[cfg_attr(feature = "jsonschema", schemars(extend("minLength" = 1, "maxLength" = 120)))]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("maxLength" = 500)))]
    pub description: Option<String>,
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::menu_price"))]
    pub price: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("maxLength" = 120)))]
    pub price_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains_alcohol: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::dietary"))]
    pub dietary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct SeoConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("maxLength" = 70)))]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "jsonschema", schemars(extend("maxLength" = 160)))]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "jsonschema", schemars(schema_with = "schema::routes"))]
    pub routes: Vec<String>,
}

fn diet_schema_type(tag: &str) -> Option<&'static str> {
    match tag {
        "vegan" => Some("VeganDiet"),
        "vegetarian" => Some("VegetarianDiet"),
        "gluten-free" => Some("GlutenFreeDiet"),
        "dairy-free" => Some("LowLactoseDiet"),
        "halal" => Some("HalalDiet"),
        "kosher" => Some("KosherDiet"),
        "low-salt" => Some("LowSaltDiet"),
        _ => None,
    }
}

fn is_slug(s: &str) -> bool {
    sws_ir::is_valid_slug(s)
}

fn is_hhmm(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return false;
    }
    let Ok(h) = s[0..2].parse::<u8>() else {
        return false;
    };
    let Ok(m) = s[3..5].parse::<u8>() else {
        return false;
    };
    b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && h < 24
        && m < 60
}

fn is_hex(s: &str) -> bool {
    sws_css::is_valid_hex_color(s)
}

fn is_price(s: &str) -> bool {
    let Some((dollars, cents)) = s.split_once('.') else {
        return false;
    };
    !dollars.is_empty()
        && dollars.bytes().all(|b| b.is_ascii_digit())
        && cents.len() == 2
        && cents.bytes().all(|b| b.is_ascii_digit())
}

fn is_date_ymd(s: &str) -> bool {
    sws_security::validate::is_valid_date(s)
}

fn is_https_url(s: &str) -> bool {
    sws_ir::is_valid_https_url(s)
}

fn is_safe_font_stack(s: &str) -> bool {
    sws_css::is_safe_font_stack(s)
}

/// Compute WCAG 2.x contrast ratio between two `#RRGGBB` colors.
pub fn contrast_ratio(a: &str, b: &str) -> Option<f64> {
    sws_css::contrast_ratio(a, b)
}

fn resolve_color<'a>(
    pal: &'a BTreeMap<String, String>,
    key: &str,
    fallback: &'a str,
) -> &'a str {
    pal.get(key).map_or(fallback, String::as_str)
}

fn is_business_type(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase() && s.len() >= 2 && chars.all(|c| c.is_ascii_alphabetic())
}

/// Validate a deserialized [`VenueManifest`], returning human-readable errors.
pub fn validate_venue(v: &VenueManifest) -> Vec<String> {
    let mut errs = Vec::new();
    let mut need = |cond: bool, msg: String| {
        if !cond {
            errs.push(msg);
        }
    };

    need(v.schema_version == 1, "schema_version must be 1".into());
    need(
        is_slug(&v.slug),
        "slug must match ^[a-z0-9]+(-[a-z0-9]+)*$".into(),
    );
    need(
        !v.name.trim().is_empty(),
        "name must be a non-empty string".into(),
    );
    need(
        !v.business_types.is_empty() && v.business_types.iter().all(|t| is_business_type(t)),
        "business_types must be a non-empty array of PascalCase types".into(),
    );

    need(!v.nap.name.trim().is_empty(), "nap.name required".into());
    need(!v.nap.phone.trim().is_empty(), "nap.phone required".into());
    need(!v.nap.email.trim().is_empty(), "nap.email required".into());
    need(
        is_https_url(&v.nap.website),
        "nap.website must be a valid https:// URL".into(),
    );
    let addr = &v.nap.address;
    need(
        !addr.street.trim().is_empty(),
        "nap.address.street required".into(),
    );
    need(
        !addr.locality.trim().is_empty(),
        "nap.address.locality required".into(),
    );
    need(
        !addr.region.trim().is_empty(),
        "nap.address.region required".into(),
    );
    need(
        !addr.postal_code.trim().is_empty(),
        "nap.address.postal_code required".into(),
    );
    need(
        !addr.country.trim().is_empty(),
        "nap.address.country required".into(),
    );

    if let Some(url) = &v.booking_url {
        need(
            is_https_url(url),
            "booking_url must be a valid https:// URL".into(),
        );
    }
    if let Some(seo) = &v.seo {
        for (i, r) in seo.routes.iter().enumerate() {
            need(
                r.starts_with('/'),
                format!("seo.routes[{i}] must start with /"),
            );
        }
    }

    for day in DAYS {
        let valid = v.hours.get(day).is_some_and(|h| {
            let is_closed = h.closed == Some(true) && h.opens.is_none() && h.closes.is_none();
            let is_open = h.closed.is_none()
                && h.opens.as_deref().is_some_and(is_hhmm)
                && h.closes.as_deref().is_some_and(is_hhmm);
            is_closed || is_open
        });
        need(valid, format!("hours.{day} invalid"));
    }
    for key in v.hours.keys() {
        need(
            DAYS.contains(&key.as_str()),
            format!("hours.{key} is not a valid day"),
        );
    }

    for (i, o) in v.holiday_overrides.iter().enumerate() {
        need(
            is_date_ymd(&o.date),
            format!("holiday_overrides[{i}].date invalid"),
        );
        need(
            !o.label.trim().is_empty(),
            format!("holiday_overrides[{i}].label required"),
        );
        let is_closed = o.closed == Some(true) && o.opens.is_none() && o.closes.is_none();
        let is_open = matches!(o.closed, None | Some(false))
            && o.opens.as_deref().is_some_and(is_hhmm)
            && o.closes.as_deref().is_some_and(is_hhmm);
        need(
            is_closed || is_open,
            format!("holiday_overrides[{i}] needs closed: true or opens/closes"),
        );
    }

    need(
        is_safe_font_stack(&v.brand.fonts.body),
        "brand.fonts.body required and must not contain CSS injection tokens".into(),
    );
    if let Some(heading) = &v.brand.fonts.heading {
        need(
            is_safe_font_stack(heading),
            "brand.fonts.heading must be a non-empty string without CSS injection tokens".into(),
        );
    }

    let pal = &v.brand.palette;
    let mut palette_valid = true;
    for (k, hex) in pal {
        let ok = is_hex(hex);
        if !ok {
            palette_valid = false;
        }
        need(ok, format!("brand.palette.{k} must be #RRGGBB"));
    }
    for req in ["background", "text", "accent"] {
        let present = pal.contains_key(req);
        if !present {
            palette_valid = false;
        }
        need(present, format!("brand.palette.{req} required"));
    }

    let pairs = &v.brand.contrast.pairs;
    need(
        !pairs.is_empty(),
        "brand.contrast.pairs must be a non-empty array".into(),
    );
    for (i, p) in pairs.iter().enumerate() {
        let (Some(fg), Some(bg)) = (pal.get(&p.foreground), pal.get(&p.background)) else {
            need(
                false,
                format!(
                    "brand.contrast.pairs[{i}] references unknown or invalid palette key"
                ),
            );
            continue;
        };
        let Some(ratio) = contrast_ratio(fg, bg) else {
            need(
                false,
                format!(
                    "brand.contrast.pairs[{i}] references unknown or invalid palette key"
                ),
            );
            continue;
        };
        let actual = (ratio * 100.0).round() / 100.0;
        need(
            actual >= 4.5,
            format!(
                "contrast {}/{} = {actual} < 4.5",
                p.foreground, p.background
            ),
        );
        need(
            (actual - p.ratio).abs() <= 0.01,
            format!(
                "contrast {}/{} recorded {}, computed {actual}",
                p.foreground, p.background, p.ratio
            ),
        );
    }

    if palette_valid {
        let background = pal["background"].as_str();
        let text = pal["text"].as_str();
        let accent = pal["accent"].as_str();
        let surface = resolve_color(pal, "surface", background);
        let accent_secondary = resolve_color(pal, "accent_secondary", accent);
        let muted_text = resolve_color(pal, "muted_text", text);
        let on_accent = resolve_color(pal, "on_accent", background);
        let footer_background = resolve_color(pal, "footer_background", text);
        let footer_text = resolve_color(pal, "footer_text", background);

        let required_pairs = [
            ("text", text, "background", background),
            ("text", text, "surface", surface),
            ("accent", accent, "background", background),
            ("accent", accent, "surface", surface),
            (
                "accent_secondary",
                accent_secondary,
                "background",
                background,
            ),
            ("accent_secondary", accent_secondary, "surface", surface),
            ("muted_text", muted_text, "background", background),
            ("muted_text", muted_text, "surface", surface),
            ("on_accent", on_accent, "accent", accent),
            ("footer_text", footer_text, "footer_background", footer_background),
        ];
        for (fg_key, fg_hex, bg_key, bg_hex) in required_pairs {
            if let Some(ratio) = contrast_ratio(fg_hex, bg_hex) {
                let actual = (ratio * 100.0).round() / 100.0;
                need(
                    ratio >= 4.5,
                    format!("required contrast {fg_key}/{bg_key} = {actual} < 4.5"),
                );
            }
        }
    }

    need(
        matches!(
            v.integrations.seating_policy.as_str(),
            "walk_in_only" | "opentable" | "resy"
        ),
        "integrations.seating_policy must be walk_in_only, opentable, or resy".into(),
    );

    if let Some(hh) = &v.happy_hour {
        need(
            is_hhmm(&hh.start) && is_hhmm(&hh.end),
            "happy_hour start/end must be HH:MM".into(),
        );
        need(
            hh.days.iter().all(|d| DAYS.contains(&d.as_str())),
            "happy_hour.days invalid".into(),
        );
    }

    need(
        v.menu.currency.len() == 3 && v.menu.currency.bytes().all(|b| b.is_ascii_uppercase()),
        "menu.currency must be a 3-letter ISO currency code".into(),
    );
    need(
        !v.menu.sections.is_empty(),
        "menu.sections must be a non-empty array".into(),
    );
    for s in &v.menu.sections {
        need(
            is_slug(&s.id),
            format!("menu section id invalid: {}", s.id),
        );
        need(
            !s.name.trim().is_empty(),
            format!("menu section name required: {}", s.id),
        );
        need(
            !s.items.is_empty(),
            format!("menu section items must be a non-empty array: {}", s.id),
        );
        for it in &s.items {
            need(
                !it.name.trim().is_empty(),
                "menu item name required".into(),
            );
            need(
                it.price.as_deref().is_none_or(is_price),
                format!("menu item price must be decimal string or null: {}", it.name),
            );
            for t in &it.dietary {
                need(
                    diet_schema_type(t).is_some(),
                    format!("unknown dietary tag {t} on {}", it.name),
                );
            }
        }
    }

    if v.newsletter.as_ref().is_some_and(|n| n.enabled) {
        let has_key = v
            .integrations
            .turnstile
            .as_ref()
            .is_some_and(|t| !t.site_key.trim().is_empty());
        need(
            has_key,
            "integrations.turnstile.site_key required for newsletter".into(),
        );
    }

    if let Some(events) = v.scheduling.as_ref().and_then(|s| s.events.as_ref()) {
        validate_events_config(events, &mut errs);
    }

    if let Some(bookings) = v.scheduling.as_ref().and_then(|s| s.bookings.as_ref()) {
        validate_bookings_config(bookings, v, &mut errs);
    }

    if let Some(c) = &v.checkout {
        validate_checkout_config(c, v, &mut errs);
    }

    errs
}

/// Validates the optional sandbox demo `checkout` catalog.
fn validate_checkout_config(c: &CheckoutConfig, v: &VenueManifest, errs: &mut Vec<String>) {
    let mut need = |cond: bool, msg: String| {
        if !cond {
            errs.push(msg);
        }
    };
    let valid_item = |it: &CheckoutItem| {
        checkout_item_id(&it.id)
            && !it.name.trim().is_empty()
            && (50..=10_000).contains(&it.cents)
    };
    let mut ids: Vec<&str> = c
        .items
        .iter()
        .map(|it| it.id.as_str())
        .chain(std::iter::once(c.subscription.id.as_str()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    need(
        (2..=5).contains(&c.items.len())
            && c.items.iter().all(valid_item)
            && valid_item(&c.subscription)
            && ids.len() == c.items.len() + 1,
        "checkout needs 2-5 items and one subscription with distinct ids and valid USD cents (50-10000)".into(),
    );
    let has_key = v
        .integrations
        .turnstile
        .as_ref()
        .is_some_and(|t| !t.site_key.trim().is_empty());
    need(
        has_key,
        "integrations.turnstile.site_key required for checkout".into(),
    );
}

/// Validates `scheduling.events`. The time zone only gets a structural check
/// here (IANA names are resolved client-side by `Intl.DateTimeFormat`); the
/// `google_calendar_id` is declarative and never shipped to the Worker.
fn validate_events_config(events: &EventsConfig, errs: &mut Vec<String>) {
    let tz = events.timezone.trim();
    let tz_ok = !tz.is_empty()
        && tz.len() <= 64
        && tz
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+' | b'.'))
        && !tz.starts_with('/')
        && !tz.ends_with('/')
        && !tz.contains("//")
        && !tz.contains("..");
    if !tz_ok {
        errs.push("scheduling.events.timezone must be a valid IANA time zone".into());
    }

    match events.google_calendar_id.as_deref() {
        Some(id) => {
            let id = id.trim();
            if id.is_empty() || id.len() > 320 || id.bytes().any(|b| b.is_ascii_control()) {
                errs.push("scheduling.events.google_calendar_id invalid".into());
            }
        }
        None => {
            if events.enabled && events.source == EventsSource::GoogleCalendarSync {
                errs.push(
                    "scheduling.events.google_calendar_id required for google_calendar_sync"
                        .into(),
                );
            }
        }
    }
}

/// Weekday keys for `scheduling.bookings.working_hours` (slots derive from
/// venue-local wall time, so both the key spelling and the ordered HH:MM
/// range are validated here).
const WEEKDAY_KEYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Validates `scheduling.bookings`. Booking is independent of public events
/// but shares the venue time zone: `scheduling.events.timezone` must be
/// present (events may stay disabled). The calendar ID only gets a structural
/// check here (it is a Workspace mailbox in production; the Worker reads the
/// real value from its `GOOGLE_BOOKING_CALENDAR_ID` secret at request time).
fn validate_bookings_config(bookings: &BookingsConfig, v: &VenueManifest, errs: &mut Vec<String>) {
    let mut need = |cond: bool, msg: String| {
        if !cond {
            errs.push(msg);
        }
    };
    let has_timezone = v
        .scheduling
        .as_ref()
        .and_then(|s| s.events.as_ref())
        .map(|e| !e.timezone.trim().is_empty())
        .unwrap_or(false);
    need(
        has_timezone,
        "bookings require scheduling.events.timezone (events may stay disabled)".into(),
    );

    let calendar = bookings.google_workspace_calendar_id.trim();
    need(
        !calendar.is_empty()
            && calendar.len() <= 254
            && !calendar.bytes().any(|b| b.is_ascii_control())
            && sws_security::validate::is_valid_email(calendar),
        "bookings require a Workspace mailbox calendar ID".into(),
    );
    need(
        (5..=240).contains(&bookings.slot_duration_minutes),
        "booking slot duration must be 5-240 minutes".into(),
    );
    need(
        bookings.buffer_minutes <= 1440,
        "booking buffer must be 0-1440 minutes".into(),
    );
    need(
        !bookings.working_hours.is_empty(),
        "booking working hours must be a non-empty object".into(),
    );
    for (day, range) in &bookings.working_hours {
        need(
            WEEKDAY_KEYS.contains(&day.as_str()),
            format!("booking working_hours key must be mon|tue|wed|thu|fri|sat|sun: {day}"),
        );
        let [open, close] = range;
        need(
            is_hhmm(open) && is_hhmm(close) && open < close,
            format!("booking working hours must be ordered same-day HH:MM pairs: {day}"),
        );
    }
    if bookings.enabled {
        let has_key = v
            .integrations
            .turnstile
            .as_ref()
            .is_some_and(|t| !t.site_key.trim().is_empty());
        need(
            has_key,
            "integrations.turnstile.site_key required for booking".into(),
        );
    }
}

/// Catalog item id pattern: `^[a-z][a-z0-9-]{0,31}$`.
fn checkout_item_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn capitalize_ascii(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Build the schema.org JSON-LD graph (`LocalBusiness` / `Menu` / `MenuSection` / `MenuItem`).
pub fn build_json_ld(v: &VenueManifest) -> Value {
    let base = v.nap.website.trim_end_matches('/');
    let a = &v.nap.address;

    let mut ld = Map::new();
    ld.insert("@context".into(), json!("https://schema.org"));
    let type_val = if v.business_types.len() == 1 {
        Value::String(v.business_types[0].clone())
    } else {
        json!(v.business_types)
    };
    ld.insert("@type".into(), type_val);
    ld.insert("@id".into(), json!(format!("{base}/#venue")));
    ld.insert("name".into(), json!(v.name_for_ld()));
    if let Some(desc) = &v.description
        && !desc.is_empty()
    {
        ld.insert("description".into(), json!(desc));
    }
    ld.insert("url".into(), json!(format!("{base}/")));
    ld.insert("telephone".into(), json!(v.nap.phone));
    ld.insert("email".into(), json!(v.nap.email));
    ld.insert(
        "address".into(),
        json!({
            "@type": "PostalAddress",
            "streetAddress": a.street,
            "addressLocality": a.locality,
            "addressRegion": a.region,
            "postalCode": a.postal_code,
            "addressCountry": a.country,
        }),
    );
    if let Some(geo) = &v.geo {
        ld.insert(
            "geo".into(),
            json!({
                "@type": "GeoCoordinates",
                "latitude": geo.latitude,
                "longitude": geo.longitude,
            }),
        );
    }
    if let Some(pr) = &v.price_range
        && !pr.is_empty()
    {
        ld.insert("priceRange".into(), json!(pr));
    }
    if !v.cuisine.is_empty() {
        ld.insert("servesCuisine".into(), json!(v.cuisine));
    }

    let opening_hours: Vec<Value> = DAYS
        .iter()
        .filter_map(|d| {
            let h = v.hours.get(*d)?;
            if h.closed == Some(true) {
                return None;
            }
            Some(json!({
                "@type": "OpeningHoursSpecification",
                "dayOfWeek": format!("https://schema.org/{}", capitalize_ascii(d)),
                "opens": h.opens.as_deref().unwrap_or(""),
                "closes": h.closes.as_deref().unwrap_or(""),
            }))
        })
        .collect();
    ld.insert("openingHoursSpecification".into(), Value::Array(opening_hours));

    if !v.holiday_overrides.is_empty() {
        let specials: Vec<Value> = v
            .holiday_overrides
            .iter()
            .map(|o| {
                let mut spec = Map::new();
                spec.insert("@type".into(), json!("OpeningHoursSpecification"));
                spec.insert("validFrom".into(), json!(o.date));
                spec.insert("validThrough".into(), json!(o.date));
                if o.closed != Some(true) {
                    if let Some(opens) = &o.opens {
                        spec.insert("opens".into(), json!(opens));
                    }
                    if let Some(closes) = &o.closes {
                        spec.insert("closes".into(), json!(closes));
                    }
                }
                Value::Object(spec)
            })
            .collect();
        ld.insert(
            "specialOpeningHoursSpecification".into(),
            Value::Array(specials),
        );
    }

    let accepts_reservations = match (&v.integrations.seating_policy, &v.booking_url) {
        (policy, Some(url)) if policy != "walk_in_only" && !url.is_empty() => {
            Value::String(url.clone())
        }
        _ => Value::Bool(false),
    };
    ld.insert("acceptsReservations".into(), accepts_reservations);

    let sections: Vec<Value> = v
        .menu
        .sections
        .iter()
        .map(|s| {
            let mut sec = Map::new();
            sec.insert("@type".into(), json!("MenuSection"));
            sec.insert("name".into(), json!(s.name));
            if let Some(desc) = &s.description
                && !desc.is_empty()
            {
                sec.insert("description".into(), json!(desc));
            }
            let items: Vec<Value> = s
                .items
                .iter()
                .map(|it| {
                    let mut item = Map::new();
                    item.insert("@type".into(), json!("MenuItem"));
                    item.insert("name".into(), json!(it.name));
                    if let Some(desc) = &it.description
                        && !desc.is_empty()
                    {
                        item.insert("description".into(), json!(desc));
                    }
                    if let Some(price) = &it.price {
                        item.insert(
                            "offers".into(),
                            json!({
                                "@type": "Offer",
                                "price": price,
                                "priceCurrency": v.menu.currency,
                            }),
                        );
                    } else if let Some(note) = &it.price_note
                        && !note.is_empty()
                    {
                        item.insert(
                            "offers".into(),
                            json!({
                                "@type": "Offer",
                                "priceCurrency": v.menu.currency,
                                "description": note,
                            }),
                        );
                    }
                    if !it.dietary.is_empty() {
                        let diets: Vec<Value> = it
                            .dietary
                            .iter()
                            .filter_map(|t| {
                                diet_schema_type(t).map(|dt| json!(format!("https://schema.org/{dt}")))
                            })
                            .collect();
                        item.insert("suitableForDiet".into(), Value::Array(diets));
                    }
                    Value::Object(item)
                })
                .collect();
            sec.insert("hasMenuItem".into(), Value::Array(items));
            Value::Object(sec)
        })
        .collect();

    ld.insert(
        "hasMenu".into(),
        json!({
            "@type": "Menu",
            "name": v.menu.name,
            "hasMenuSection": sections,
        }),
    );

    Value::Object(ld)
}

impl VenueManifest {
    fn name_for_ld(&self) -> &str {
        &self.nap.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_venue() -> VenueManifest {
        serde_json::from_str(include_str!("../../demo/venue.json")).unwrap()
    }

    #[test]
    fn demo_manifest_deserializes_and_validates() {
        let v = demo_venue();
        assert_eq!(v.slug, "shirlington-hearth-demo");
        assert_eq!(v.schema_version, 1);
        assert!(validate_venue(&v).is_empty());
    }

    #[test]
    fn scheduling_events_validation() {
        let v = demo_venue();
        assert!(validate_venue(&v).is_empty());

        let mut v = demo_venue();
        v.scheduling.as_mut().unwrap().events.as_mut().unwrap().source = EventsSource::GoogleCalendarSync;
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("google_calendar_id")), "{errs:?}");

        let mut v = demo_venue();
        v.scheduling.as_mut().unwrap().events.as_mut().unwrap().google_calendar_id =
            Some("calendar@example.test".into());
        assert!(validate_venue(&v).is_empty());

        let mut v = demo_venue();
        v.scheduling.as_mut().unwrap().events.as_mut().unwrap().timezone =
            "Not A Zone\n".into();
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("timezone")), "{errs:?}");
    }

    #[test]
    fn scheduling_bookings_validation() {
        // The demo manifest itself carries the bookings config; replacing it
        // must still validate cleanly.
        let mut v = demo_venue();
        v.scheduling.as_mut().unwrap().bookings = Some(BookingsConfig {
            enabled: false,
            google_workspace_calendar_id: "calls@shirlingtonhearth.example".into(),
            slot_duration_minutes: 30,
            buffer_minutes: 15,
            working_hours: BTreeMap::from([("mon".into(), ["09:00".into(), "17:00".into()])]),
            auto_zoom: true,
        });
        assert!(validate_venue(&v).is_empty());

        // Disabling events must not break booking, but the venue timezone is shared.
        v.scheduling.as_mut().unwrap().events = None;
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("timezone")), "{errs:?}");

        let mut v = demo_venue();
        v.scheduling.as_mut().unwrap().bookings = Some(BookingsConfig {
            enabled: false,
            google_workspace_calendar_id: "calls@shirlingtonhearth.example".into(),
            slot_duration_minutes: 30,
            buffer_minutes: 15,
            working_hours: BTreeMap::new(),
            auto_zoom: false,
        });
        let errs = validate_venue(&v);
        assert!(
            errs.iter()
                .any(|e| e.contains("working hours must be a non-empty object")),
            "{errs:?}"
        );

        let invalid = BookingsConfig {
            enabled: true,
            google_workspace_calendar_id: "not-a-mailbox".into(),
            slot_duration_minutes: 4,
            buffer_minutes: 1441,
            working_hours: BTreeMap::from([("monday".into(), ["17:00".into(), "09:00".into()])]),
            auto_zoom: false,
        };
        let mut v = demo_venue();
        v.scheduling.as_mut().unwrap().bookings = Some(invalid);
        let errs = validate_venue(&v);
        for fragment in [
            "Workspace mailbox",
            "slot duration",
            "booking buffer",
            "working_hours key",
            "ordered same-day HH:MM",
        ] {
            assert!(errs.iter().any(|e| e.contains(fragment)), "{errs:?}");
        }

        // The demo carries the always-pass TEST site key, so an enabled booking
        // config alone satisfies the Turnstile requirement; a missing key fails.
        let mut v = demo_venue();
        v.integrations.turnstile.as_mut().unwrap().site_key.clear();
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("turnstile")), "{errs:?}");
    }

    #[test]
    fn rejects_invalid_slug_https_and_css_injection() {
        let mut v = demo_venue();
        v.slug = "Bad_Slug!".into();
        v.nap.website = "http://shirlington-hearth.example".into();
        v.brand.fonts.body = "Arial; background: url(https://evil.example)".into();
        v.brand.fonts.heading = Some("Georgia, expression(alert(1))".into());
        v.holiday_overrides[0].date = "2027-02-30".into();
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("slug")));
        assert!(errs.iter().any(|e| e.contains("nap.website")));
        assert!(errs.iter().any(|e| e.contains("brand.fonts.body")));
        assert!(errs.iter().any(|e| e.contains("brand.fonts.heading")));
        assert!(errs.iter().any(|e| e.contains("holiday_overrides[0].date")));
    }

    #[test]
    fn enforces_holiday_override_mutual_exclusivity() {
        let mut v = demo_venue();
        v.holiday_overrides.push(HolidayOverride {
            date: "2027-07-04".into(),
            label: "Broken".into(),
            closed: Some(true),
            opens: Some("09:00".into()),
            closes: Some("17:00".into()),
        });
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("holiday_overrides[5]")));
    }

    #[test]
    fn enforces_contrast_pairs_and_palette_hex() {
        let mut v = demo_venue();
        v.brand.palette.insert("accent".into(), "#E9E2D0".into());
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("contrast")));
    }

    #[test]
    fn enforces_menu_price_dietary_and_newsletter_turnstile() {
        let mut v = demo_venue();
        v.menu.sections[0].items[0].price = Some("3.5".into());
        v.menu.sections[0].items[0].dietary = vec!["paleo".into()];
        v.integrations.turnstile = None;
        let errs = validate_venue(&v);
        assert!(errs.iter().any(|e| e.contains("menu item price")));
        assert!(errs.iter().any(|e| e.contains("unknown dietary tag")));
        assert!(errs.iter().any(|e| e.contains("turnstile.site_key")));
    }

    #[test]
    fn builds_expected_json_ld() {
        let v = demo_venue();
        let ld = build_json_ld(&v);
        assert_eq!(ld["@context"], "https://schema.org");
        assert_eq!(ld["@type"], json!(["CafeOrCoffeeShop", "BarOrPub"]));
        assert_eq!(ld["@id"], "https://shirlington-hearth.example/#venue");
        assert_eq!(ld["address"]["@type"], "PostalAddress");
        assert_eq!(ld["geo"]["@type"], "GeoCoordinates");
        assert_eq!(ld["openingHoursSpecification"].as_array().unwrap().len(), 7);
        assert_eq!(
            ld["specialOpeningHoursSpecification"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(ld["acceptsReservations"], false);
        assert_eq!(ld["hasMenu"]["@type"], "Menu");
        assert_eq!(ld["hasMenu"]["name"], "Café & Wine Menu");
        let first_section = &ld["hasMenu"]["hasMenuSection"][0];
        assert_eq!(first_section["@type"], "MenuSection");
        assert_eq!(first_section["hasMenuItem"][0]["@type"], "MenuItem");
        assert_eq!(first_section["hasMenuItem"][0]["offers"]["price"], "3.50");
        assert_eq!(
            first_section["hasMenuItem"][0]["suitableForDiet"],
            json!([
                "https://schema.org/VeganDiet",
                "https://schema.org/GlutenFreeDiet"
            ])
        );
    }
}
