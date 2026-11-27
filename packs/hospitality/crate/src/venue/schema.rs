//! JSON Schema (Draft 2020-12) for [`super::VenueManifest`], generated from the
//! Rust types. Only compiled with the `jsonschema` feature (host tooling; never
//! part of the Wasm / Worker builds).
//!
//! The simple constraints (patterns, ranges, lengths, enums) live as
//! `#[schemars(extend(...))]` annotations next to each field in `venue.rs`.
//! The functions here cover the shapes that attributes cannot express:
//! `patternProperties`, nullable-but-constrained strings, `oneOf` objects and
//! tuples. They keep the generated schema exactly as strict as the original
//! hand-written `venue.schema.json` (proved by `tests/venue_schema.rs`).

use schemars::{Schema, SchemaGenerator, json_schema};
use serde_json::Value;

use super::{DayHours, ModuleFlag, VenueManifest};

/// Canonical `$id` of the venue manifest schema (unchanged from the
/// hand-written schema).
pub const VENUE_SCHEMA_ID: &str =
    "https://solo-website-system.example/packs/hospitality/schema/venue.schema.json";

const HHMM: &str = r"^([01][0-9]|2[0-3]):[0-5][0-9]$";
const HEX: &str = r"^#[0-9A-Fa-f]{6}$";
const PRICE: &str = r"^[0-9]+\.[0-9]{2}$";
const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// The generated venue manifest schema as a deterministic JSON value.
///
/// `Option<T>` fields are plain `T` (the manifest omits absent options);
/// fields that really accept `null` say so explicitly with `oneOf`.
#[must_use]
pub fn venue_schema() -> Value {
    sws_ir::jsonschema::root_schema::<VenueManifest>(VENUE_SCHEMA_ID, false)
}

pub fn nullable_string(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "oneOf": [{ "type": "string" }, { "type": "null" }] })
}

pub fn legal_disclaimer(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "oneOf": [{ "type": "string" }, { "type": "null" }],
        "description": "Shown visibly on every page when present."
    })
}

pub fn booking_url(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "oneOf": [{ "type": "string", "pattern": "^https://" }, { "type": "null" }]
    })
}

pub fn events_calendar_id(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "oneOf": [{ "type": "string", "minLength": 1 }, { "type": "null" }]
    })
}

pub fn menu_price(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "oneOf": [{ "type": "string", "pattern": PRICE }, { "type": "null" }]
    })
}

pub fn business_types(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "minItems": 1,
        "items": {
            "type": "string",
            "pattern": "^[A-Z][A-Za-z]+$",
            "description": "schema.org LocalBusiness subtype"
        }
    })
}

pub fn dietary(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "array",
        "uniqueItems": true,
        "items": {
            "enum": ["vegan", "vegetarian", "gluten-free", "dairy-free", "halal", "kosher", "low-salt"]
        }
    })
}

pub fn routes(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "array", "items": { "type": "string", "pattern": "^/" } })
}

pub fn happy_hour_days(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "array", "uniqueItems": true, "items": { "enum": DAYS } })
}

pub fn palette(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "object",
        "required": ["background", "text", "accent"],
        "additionalProperties": { "type": "string", "pattern": HEX }
    })
}

pub fn hours(generator: &mut SchemaGenerator) -> Schema {
    let day = generator.subschema_for::<DayHours>();
    json_schema!({
        "type": "object",
        "required": DAYS,
        "additionalProperties": false,
        "patternProperties": { "^(mon|tues|wednes|thurs|fri|satur|sun)day$": day }
    })
}

pub fn day_hours(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "oneOf": [
            {
                "type": "object",
                "required": ["opens", "closes"],
                "additionalProperties": false,
                "properties": {
                    "opens": { "type": "string", "pattern": HHMM },
                    "closes": { "type": "string", "pattern": HHMM }
                }
            },
            {
                "type": "object",
                "required": ["closed"],
                "additionalProperties": false,
                "properties": { "closed": { "const": true } }
            }
        ]
    })
}

pub fn modules(generator: &mut SchemaGenerator) -> Schema {
    let module = generator.subschema_for::<ModuleFlag>();
    json_schema!({
        "type": "object",
        "additionalProperties": false,
        "patternProperties": { "^M([1-9]|10)$": module }
    })
}

pub fn working_hours(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "object",
        "minProperties": 1,
        "additionalProperties": false,
        "patternProperties": {
            "^(mon|tue|wed|thu|fri|sat|sun)$": {
                "type": "array",
                "minItems": 2,
                "maxItems": 2,
                "items": { "type": "string", "pattern": HHMM }
            }
        }
    })
}
