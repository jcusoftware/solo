//! Proof that the venue schema generated from the Rust types is exactly as
//! strict as the original hand-written `venue.schema.json`.
//!
//! `fixtures/venue.schema.legacy.json` is a frozen copy of the hand-written
//! schema. The tests below run both schemas over the demo fixture, a rich
//! variant of it, a table of targeted valid/invalid edits for every constraint,
//! and automatic structural mutations (unknown key, removed key, wrong type at
//! every node). Acceptance must be identical.
#![cfg(feature = "jsonschema")]

use std::collections::BTreeSet;

use serde_json::{Value, json};
use sws_codegen::validate::{unsupported_keywords, validate};
use sws_pack_hospitality::compiler::HospitalityPlugin;
use sws_pack_hospitality::venue::schema::venue_schema;
use sws_plugin::SwsPlugin;

const LEGACY: &str = include_str!("fixtures/venue.schema.legacy.json");
const COMMITTED: &str = include_str!("../../schema/venue.schema.json");
const DEMO: &str = include_str!("../../demo/venue.json");
const ABC_BAD: &str = include_str!("../../tools/fixtures/abc-bad.json");

fn legacy() -> Value {
    serde_json::from_str(LEGACY).expect("legacy schema parses")
}

fn demo() -> Value {
    serde_json::from_str(DEMO).expect("demo parses")
}

fn ok(schema: &Value, doc: &Value) -> bool {
    validate(schema, doc).is_empty()
}

/// Demo plus one valid instance of every optional section, so each constraint
/// has something to mutate.
fn rich_base() -> Value {
    let mut doc = demo();
    let root = doc.as_object_mut().expect("object");
    let defaults = [
        (
            "geo",
            json!({"latitude": 38.8, "longitude": -77.1, "approximate": true}),
        ),
        (
            "holiday_overrides",
            json!([{"date": "2026-12-25", "label": "Holiday", "closed": true}]),
        ),
        ("booking_url", json!("https://book.example/table")),
        (
            "newsletter",
            json!({"enabled": true, "consent_text": "Send me news."}),
        ),
        (
            "happy_hour",
            json!({"days": ["monday"], "start": "16:00", "end": "18:00", "offers": ["Half-price small plates"]}),
        ),
        (
            "checkout",
            json!({
                "items": [
                    {"id": "item-a", "name": "A", "cents": 500},
                    {"id": "item-b", "name": "B", "cents": 800}
                ],
                "subscription": {"id": "sub", "name": "S", "cents": 900}
            }),
        ),
        (
            "seo",
            json!({"title": "T", "description": "D", "routes": ["/x"]}),
        ),
    ];
    for (k, v) in defaults {
        root.entry(k).or_insert(v);
    }
    let sched = root.entry("scheduling").or_insert_with(|| json!({}));
    let sched = sched.as_object_mut().expect("scheduling object");
    sched.entry("events").or_insert(json!({
        "enabled": true, "source": "d1_or_cms", "google_calendar_id": null, "timezone": "America/New_York"
    }));
    sched.entry("bookings").or_insert(json!({
        "enabled": true, "google_workspace_calendar_id": "hello@example.test",
        "slot_duration_minutes": 30, "buffer_minutes": 10,
        "working_hours": {"mon": ["09:00", "17:00"]}, "auto_zoom": false
    }));
    let integ = root
        .get_mut("integrations")
        .and_then(Value::as_object_mut)
        .expect("integrations");
    integ
        .entry("toast")
        .or_insert(json!({"enabled": false, "restaurant_guid": null}));
    integ
        .entry("opentable")
        .or_insert(json!({"restaurant_id": null}));
    integ.entry("resy").or_insert(json!({"venue_id": null}));
    integ.entry("inkind").or_insert(json!({"enabled": false}));
    integ
        .entry("turnstile")
        .or_insert(json!({"site_key": "1x00000000000000000000AA", "test_key": true}));
    doc.as_object_mut()
        .expect("object")
        .entry("legal_disclaimer")
        .or_insert(Value::Null);
    doc
}

fn with(base: &Value, ptr: &str, value: Option<Value>) -> Value {
    let mut doc = base.clone();
    let (parent, key) = ptr.rsplit_once('/').expect("pointer has a parent");
    let target = doc
        .pointer_mut(parent)
        .unwrap_or_else(|| panic!("parent of {ptr} missing in base"));
    match (target, value) {
        (Value::Object(m), Some(v)) => {
            m.insert(key.to_string(), v);
        }
        (Value::Object(m), None) => {
            m.remove(key);
        }
        (Value::Array(a), Some(v)) => {
            a[key.parse::<usize>().expect("index")] = v;
        }
        (Value::Array(a), None) => {
            a.remove(key.parse::<usize>().expect("index"));
        }
        _ => panic!("cannot edit {ptr}"),
    }
    doc
}

fn long(n: usize) -> Value {
    Value::String("a".repeat(n))
}

#[test]
fn committed_schema_is_the_generated_schema() {
    let committed: Value = serde_json::from_str(COMMITTED).expect("committed schema parses");
    assert_eq!(committed, venue_schema(), "run `npm run codegen`");
}

#[test]
fn both_schemas_use_only_validator_supported_keywords() {
    assert_eq!(unsupported_keywords(&legacy()), Vec::<String>::new());
    assert_eq!(unsupported_keywords(&venue_schema()), Vec::<String>::new());
}

#[test]
fn schema_keeps_the_original_id_and_draft() {
    let new = venue_schema();
    let old = legacy();
    assert_eq!(new["$id"], old["$id"]);
    assert_eq!(new["$schema"], old["$schema"]);
    assert_eq!(new["title"], old["title"]);
    assert_eq!(new["description"], old["description"]);
}

#[test]
fn demo_and_rich_fixture_are_valid_and_abc_negative_fixture_is_rejected() {
    let (old, new) = (legacy(), venue_schema());
    for (label, doc) in [("demo", demo()), ("rich", rich_base())] {
        assert!(
            ok(&old, &doc),
            "{label}: legacy: {:?}",
            validate(&old, &doc)
        );
        assert!(
            ok(&new, &doc),
            "{label}: generated: {:?}",
            validate(&new, &doc)
        );
    }
    let bad: Value = serde_json::from_str(ABC_BAD).expect("abc-bad parses");
    assert!(!ok(&old, &bad));
    assert!(!ok(&new, &bad));
}

/// (pointer, replacement; `None` removes the key). Every row is checked on
/// both schemas and acceptance must agree; the label is only for messages.
fn targeted() -> Vec<(&'static str, Option<Value>)> {
    let day = |v: Value| Some(v);
    vec![
        ("/$schema", day(json!(5))),
        ("/schema_version", day(json!(2))),
        ("/schema_version", day(json!(0))),
        ("/schema_version", day(json!("1"))),
        ("/schema_version", None),
        ("/slug", day(json!("Bad Slug"))),
        ("/slug", day(json!("-x"))),
        ("/slug", day(json!("a--b"))),
        ("/slug", day(json!("a-b-c1"))),
        ("/slug", None),
        ("/name", day(json!(""))),
        ("/name", day(long(120))),
        ("/name", day(long(121))),
        ("/name", None),
        ("/legal_disclaimer", day(json!(null))),
        ("/legal_disclaimer", day(json!(5))),
        ("/fictional", day(json!("yes"))),
        ("/tagline", day(json!(1))),
        ("/description", day(json!(1))),
        ("/category_label", day(json!(1))),
        ("/business_types", day(json!([]))),
        ("/business_types", day(json!(["lower"]))),
        ("/business_types", day(json!(["A"]))),
        ("/business_types", day(json!(["Ab"]))),
        ("/business_types", day(json!(["Ab1"]))),
        ("/business_types", None),
        ("/cuisine", day(json!([1]))),
        ("/price_range", day(json!("$"))),
        ("/price_range", day(json!("$$$$"))),
        ("/price_range", day(json!("$$$$$"))),
        ("/price_range", day(json!(""))),
        ("/price_range", day(json!("$x"))),
        ("/locale", day(json!(5))),
        ("/timezone", day(json!(5))),
        ("/nap", None),
        ("/nap/name", None),
        ("/nap/phone", day(json!("555"))),
        ("/nap/phone", day(json!("+123456"))),
        ("/nap/phone", day(json!("+1234567"))),
        ("/nap/phone", day(json!("+12345678901234567890"))),
        ("/nap/phone", day(json!("+123456789012345678901"))),
        ("/nap/phone", day(json!("1-555-0100-000"))),
        ("/nap/email", day(json!("nope"))),
        ("/nap/email", day(json!("a@b.example"))),
        ("/nap/website", day(json!("http://x.example"))),
        ("/nap/website", day(json!("https://x.example"))),
        ("/nap/address/region", day(json!("va"))),
        ("/nap/address/region", day(json!("VA"))),
        ("/nap/address/region", day(json!("VAA"))),
        ("/nap/address/country", day(json!("USA"))),
        ("/nap/address/country", day(json!("US"))),
        ("/nap/address/street", None),
        ("/nap/address/postal_code", day(json!(22206))),
        ("/geo/latitude", day(json!(90))),
        ("/geo/latitude", day(json!(90.1))),
        ("/geo/latitude", day(json!(-90.1))),
        ("/geo/longitude", day(json!(180))),
        ("/geo/longitude", day(json!(-181))),
        ("/geo/approximate", day(json!("x"))),
        ("/geo/latitude", None),
        ("/hours/monday", day(json!({"closed": true}))),
        ("/hours/monday", day(json!({"closed": false}))),
        ("/hours/monday", day(json!({"opens": "09:00"}))),
        (
            "/hours/monday",
            day(json!({"opens": "09:00", "closes": "17:00"})),
        ),
        (
            "/hours/monday",
            day(json!({"opens": "24:00", "closes": "01:00"})),
        ),
        (
            "/hours/monday",
            day(json!({"opens": "09:60", "closes": "17:00"})),
        ),
        (
            "/hours/monday",
            day(json!({"opens": "9:00", "closes": "17:00"})),
        ),
        (
            "/hours/monday",
            day(json!({"opens": "09:00", "closes": "17:00", "closed": true})),
        ),
        (
            "/hours/monday",
            day(json!({"opens": "09:00", "closes": "17:00", "closed": false})),
        ),
        (
            "/hours/monday",
            day(json!({"opens": null, "closes": "17:00"})),
        ),
        ("/hours/monday", day(json!({"closed": true, "extra": 1}))),
        ("/hours/monday", day(json!({}))),
        ("/hours/monday", day(json!(null))),
        ("/hours/sunday", None),
        ("/hours/funday", day(json!({"closed": true}))),
        ("/hours/Monday", day(json!({"closed": true}))),
        ("/hours", day(json!([]))),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "closed": true}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "closed": true, "opens": "10:00"}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "closed": true, "closes": "10:00"}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "closed": false}])),
        ),
        (
            "/holiday_overrides",
            day(
                json!([{"date": "2026-12-25", "label": "X", "closed": false, "opens": "10:00", "closes": "12:00"}]),
            ),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "opens": "10:00", "closes": "12:00"}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "opens": "10:00"}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X"}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "12/25/2026", "label": "X", "closed": true}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "closed": true}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "closed": true, "extra": 1}])),
        ),
        (
            "/holiday_overrides",
            day(json!([{"date": "2026-12-25", "label": "X", "opens": "25:00", "closes": "12:00"}])),
        ),
        ("/booking_url", day(json!(null))),
        ("/booking_url", day(json!("http://x.example"))),
        ("/booking_url", day(json!("https://x.example"))),
        ("/booking_url", day(json!(5))),
        ("/booking_url", None),
        ("/venue_accessibility", day(json!(["ramp"]))),
        ("/venue_accessibility", day(json!([1]))),
        (
            "/brand/palette",
            day(json!({"background": "#112233", "text": "#445566"})),
        ),
        (
            "/brand/palette",
            day(
                json!({"background": "#112233", "text": "#445566", "accent": "#778899", "extra": "#AABBCC"}),
            ),
        ),
        (
            "/brand/palette",
            day(json!({"background": "#12345", "text": "#445566", "accent": "#778899"})),
        ),
        (
            "/brand/palette",
            day(json!({"background": "#GGGGGG", "text": "#445566", "accent": "#778899"})),
        ),
        (
            "/brand/palette",
            day(json!({"background": "#aBcDeF", "text": "#445566", "accent": "#778899"})),
        ),
        (
            "/brand/palette",
            day(json!({"background": 5, "text": "#445566", "accent": "#778899"})),
        ),
        ("/brand/contrast/pairs", day(json!([]))),
        ("/brand/contrast/pairs", None),
        (
            "/brand/contrast/pairs",
            day(json!([{"foreground": "text", "background": "background", "ratio": 4.5}])),
        ),
        (
            "/brand/contrast/pairs",
            day(json!([{"foreground": "text", "background": "background", "ratio": 4.49}])),
        ),
        (
            "/brand/contrast/pairs",
            day(json!([{"foreground": "text", "background": "background", "ratio": "5"}])),
        ),
        (
            "/brand/contrast/pairs",
            day(json!([{"foreground": "text", "background": "background"}])),
        ),
        (
            "/brand/contrast/pairs",
            day(json!([{"foreground": "text", "background": "background", "ratio": 5, "x": 1}])),
        ),
        ("/brand/contrast/wcag_target", day(json!(5))),
        ("/brand/fonts/body", None),
        ("/brand/fonts/extra", day(json!("x"))),
        ("/brand/fonts/heading", day(json!(5))),
        ("/brand", None),
        ("/modules", None),
        ("/modules", day(json!({}))),
        ("/modules", day(json!({"M10": {"enabled": true}}))),
        ("/modules", day(json!({"M11": {"enabled": true}}))),
        ("/modules", day(json!({"M0": {"enabled": true}}))),
        ("/modules", day(json!({"m1": {"enabled": true}}))),
        (
            "/modules",
            day(json!({"M1": {"enabled": true, "name": "Menu"}})),
        ),
        ("/modules", day(json!({"M1": {}}))),
        ("/modules", day(json!({"M1": {"enabled": "yes"}}))),
        ("/modules", day(json!({"M1": {"enabled": true, "x": 1}}))),
        ("/modules", day(json!({"M1": {"enabled": true, "name": 5}}))),
        ("/integrations/seating_policy", day(json!("walk_in_only"))),
        ("/integrations/seating_policy", day(json!("resy"))),
        ("/integrations/seating_policy", day(json!("other"))),
        ("/integrations/seating_policy", None),
        (
            "/integrations/toast",
            day(json!({"enabled": true, "restaurant_guid": null})),
        ),
        (
            "/integrations/toast",
            day(json!({"enabled": true, "restaurant_guid": "abc"})),
        ),
        (
            "/integrations/toast",
            day(json!({"enabled": true, "restaurant_guid": 5})),
        ),
        ("/integrations/toast", day(json!({}))),
        ("/integrations/toast", day(json!({"enabled": true}))),
        ("/integrations/opentable", day(json!({}))),
        ("/integrations/opentable", day(json!({"restaurant_id": 5}))),
        (
            "/integrations/opentable",
            day(json!({"restaurant_id": "x"})),
        ),
        ("/integrations/resy", day(json!({"venue_id": null}))),
        ("/integrations/resy", day(json!({"venue_id": 1}))),
        ("/integrations/resy", day(json!({"x": 1}))),
        ("/integrations/inkind", day(json!({"enabled": 1}))),
        ("/integrations/inkind", day(json!({}))),
        ("/integrations/turnstile", day(json!({}))),
        ("/integrations/turnstile", day(json!({"site_key": "k"}))),
        (
            "/integrations/turnstile",
            day(json!({"site_key": "k", "test_key": "x"})),
        ),
        (
            "/integrations/turnstile",
            day(json!({"site_key": "k", "extra": 1})),
        ),
        ("/integrations/extra", day(json!({}))),
        ("/integrations", None),
        ("/newsletter", day(json!({"enabled": true}))),
        ("/newsletter", day(json!({"consent_text": "x"}))),
        (
            "/newsletter",
            day(json!({"enabled": true, "consent_text": "x", "blurb": long(500)})),
        ),
        (
            "/newsletter",
            day(json!({"enabled": true, "consent_text": "x", "blurb": long(501)})),
        ),
        (
            "/newsletter",
            day(json!({"enabled": true, "consent_text": long(500)})),
        ),
        (
            "/newsletter",
            day(json!({"enabled": true, "consent_text": long(501)})),
        ),
        (
            "/newsletter",
            day(json!({"enabled": true, "consent_text": "x", "privacy_note": 5})),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "google_calendar_sync", "google_calendar_id": "cal-id", "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "google_calendar_sync", "google_calendar_id": null, "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "google_calendar_sync", "google_calendar_id": "   ", "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "google_calendar_sync", "google_calendar_id": "", "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": false, "source": "google_calendar_sync", "google_calendar_id": null, "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": false, "source": "google_calendar_sync", "google_calendar_id": "", "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "d1_or_cms", "google_calendar_id": null, "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "d1_or_cms", "google_calendar_id": "x", "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "other", "google_calendar_id": null, "timezone": "America/New_York"}),
            ),
        ),
        (
            "/scheduling/events",
            day(json!({"enabled": true, "source": "d1_or_cms", "timezone": "America/New_York"})),
        ),
        (
            "/scheduling/events",
            day(json!({"enabled": true, "source": "d1_or_cms", "google_calendar_id": null})),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "d1_or_cms", "google_calendar_id": null, "timezone": ""}),
            ),
        ),
        (
            "/scheduling/events",
            day(json!({"source": "d1_or_cms", "google_calendar_id": null, "timezone": "UTC"})),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": "yes", "source": "d1_or_cms", "google_calendar_id": null, "timezone": "UTC"}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "d1_or_cms", "google_calendar_id": null, "timezone": "UTC", "x": 1}),
            ),
        ),
        (
            "/scheduling/events",
            day(
                json!({"enabled": true, "source": "google_calendar_sync", "google_calendar_id": 5, "timezone": "UTC"}),
            ),
        ),
        ("/scheduling/x", day(json!({}))),
        ("/scheduling/bookings/enabled", day(json!("x"))),
        (
            "/scheduling/bookings/google_workspace_calendar_id",
            day(json!("ab")),
        ),
        (
            "/scheduling/bookings/google_workspace_calendar_id",
            day(json!("not-an-email")),
        ),
        (
            "/scheduling/bookings/google_workspace_calendar_id",
            day(json!(5)),
        ),
        (
            "/scheduling/bookings/google_workspace_calendar_id",
            day(json!("a@b.example")),
        ),
        ("/scheduling/bookings/google_workspace_calendar_id", None),
        ("/scheduling/bookings/slot_duration_minutes", day(json!(4))),
        ("/scheduling/bookings/slot_duration_minutes", day(json!(5))),
        (
            "/scheduling/bookings/slot_duration_minutes",
            day(json!(240)),
        ),
        (
            "/scheduling/bookings/slot_duration_minutes",
            day(json!(241)),
        ),
        (
            "/scheduling/bookings/slot_duration_minutes",
            day(json!(5.5)),
        ),
        (
            "/scheduling/bookings/slot_duration_minutes",
            day(json!("30")),
        ),
        ("/scheduling/bookings/buffer_minutes", day(json!(-1))),
        ("/scheduling/bookings/buffer_minutes", day(json!(0))),
        ("/scheduling/bookings/buffer_minutes", day(json!(1440))),
        ("/scheduling/bookings/buffer_minutes", day(json!(1441))),
        ("/scheduling/bookings/working_hours", day(json!({}))),
        (
            "/scheduling/bookings/working_hours",
            day(json!({"mon": ["09:00"]})),
        ),
        (
            "/scheduling/bookings/working_hours",
            day(json!({"mon": ["09:00", "17:00", "18:00"]})),
        ),
        (
            "/scheduling/bookings/working_hours",
            day(json!({"mon": ["9:00", "17:00"]})),
        ),
        (
            "/scheduling/bookings/working_hours",
            day(json!({"xyz": ["09:00", "17:00"]})),
        ),
        (
            "/scheduling/bookings/working_hours",
            day(json!({"sun": ["09:00", "17:00"], "sat": ["10:00", "12:00"]})),
        ),
        (
            "/scheduling/bookings/working_hours",
            day(json!({"monday": ["09:00", "17:00"]})),
        ),
        (
            "/scheduling/bookings/working_hours",
            day(json!({"mon": "09:00"})),
        ),
        ("/scheduling/bookings/auto_zoom", None),
        ("/scheduling/bookings/auto_zoom", day(json!("no"))),
        ("/scheduling/bookings/extra", day(json!(1))),
        ("/happy_hour/days", day(json!(["monday", "monday"]))),
        ("/happy_hour/days", day(json!(["funday"]))),
        ("/happy_hour/days", day(json!([]))),
        ("/happy_hour/days", day(json!(["monday", "tuesday"]))),
        ("/happy_hour/start", day(json!("25:00"))),
        ("/happy_hour/start", day(json!("23:59"))),
        ("/happy_hour/end", day(json!("5pm"))),
        ("/happy_hour/offers", None),
        ("/happy_hour/offers", day(json!([5]))),
        ("/happy_hour/notes", day(json!(5))),
        ("/happy_hour/extra", day(json!(1))),
        (
            "/checkout/items",
            day(json!([{"id": "item-a", "name": "A", "cents": 500}])),
        ),
        ("/checkout/items", day(json!([]))),
        ("/checkout/items/0/id", day(json!("Bad"))),
        ("/checkout/items/0/id", day(json!("1abc"))),
        (
            "/checkout/items/0/id",
            day(json!(format!("a{}", "b".repeat(31)))),
        ),
        (
            "/checkout/items/0/id",
            day(json!(format!("a{}", "b".repeat(32)))),
        ),
        ("/checkout/items/0/name", day(json!(""))),
        ("/checkout/items/0/cents", day(json!(49))),
        ("/checkout/items/0/cents", day(json!(50))),
        ("/checkout/items/0/cents", day(json!(10000))),
        ("/checkout/items/0/cents", day(json!(10001))),
        ("/checkout/items/0/cents", day(json!(50.5))),
        ("/checkout/items/0/cents", day(json!(-5))),
        ("/checkout/items/0/extra", day(json!(1))),
        ("/checkout/subscription", None),
        ("/checkout/subscription/cents", day(json!(10001))),
        ("/menu/name", None),
        ("/menu/currency", day(json!("usd"))),
        ("/menu/currency", day(json!("USDX"))),
        ("/menu/currency", day(json!("EUR"))),
        ("/menu/sections", day(json!([]))),
        ("/menu/sections/0/id", day(json!("Bad Id"))),
        ("/menu/sections/0/id", day(json!("good-id"))),
        ("/menu/sections/0/id", None),
        ("/menu/sections/0/description", day(long(500))),
        ("/menu/sections/0/description", day(long(501))),
        ("/menu/sections/0/items", day(json!([]))),
        ("/menu/sections/0/items/0/name", day(json!(""))),
        ("/menu/sections/0/items/0/name", day(long(120))),
        ("/menu/sections/0/items/0/name", day(long(121))),
        ("/menu/sections/0/items/0/description", day(long(500))),
        ("/menu/sections/0/items/0/description", day(long(501))),
        ("/menu/sections/0/items/0/price", day(json!("12"))),
        ("/menu/sections/0/items/0/price", day(json!("12.5"))),
        ("/menu/sections/0/items/0/price", day(json!("12.50"))),
        ("/menu/sections/0/items/0/price", day(json!(".50"))),
        ("/menu/sections/0/items/0/price", day(json!("$12.50"))),
        ("/menu/sections/0/items/0/price", day(json!(null))),
        ("/menu/sections/0/items/0/price", day(json!(12.5))),
        ("/menu/sections/0/items/0/price", None),
        ("/menu/sections/0/items/0/price_note", day(long(120))),
        ("/menu/sections/0/items/0/price_note", day(long(121))),
        (
            "/menu/sections/0/items/0/contains_alcohol",
            day(json!("no")),
        ),
        (
            "/menu/sections/0/items/0/contains_alcohol",
            day(json!(true)),
        ),
        (
            "/menu/sections/0/items/0/dietary",
            day(json!(["vegan", "vegan"])),
        ),
        (
            "/menu/sections/0/items/0/dietary",
            day(json!(["pescatarian"])),
        ),
        (
            "/menu/sections/0/items/0/dietary",
            day(json!(["vegan", "kosher", "low-salt"])),
        ),
        ("/menu/sections/0/items/0/dietary", day(json!([]))),
        ("/menu/sections/0/items/0/dietary", day(json!("vegan"))),
        ("/menu/sections/0/items/0/extra", day(json!(1))),
        ("/seo/title", day(long(70))),
        ("/seo/title", day(long(71))),
        ("/seo/description", day(long(160))),
        ("/seo/description", day(long(161))),
        ("/seo/routes", day(json!(["x"]))),
        ("/seo/routes", day(json!(["/x", "/y"]))),
        ("/seo/routes", day(json!([5]))),
        ("/seo/extra", day(json!(1))),
        ("/extra", day(json!(1))),
        ("/nap/extra", day(json!(1))),
        ("/nap/address/extra", day(json!(1))),
        ("/geo/extra", day(json!(1))),
        ("/brand/extra", day(json!(1))),
        ("/brand/contrast/extra", day(json!(1))),
        ("/newsletter/extra", day(json!(1))),
        ("/happy_hour/start", None),
        ("/menu/extra", day(json!(1))),
        ("/menu/sections/0/extra", day(json!(1))),
    ]
}

#[test]
fn targeted_constraint_table_agrees_with_the_legacy_schema() {
    let (old, new) = (legacy(), venue_schema());
    let base = rich_base();
    assert!(ok(&old, &base) && ok(&new, &base));
    let (mut accepted, mut rejected) = (0, 0);
    for (ptr, value) in targeted() {
        let doc = with(&base, ptr, value.clone());
        let (a, b) = (ok(&old, &doc), ok(&new, &doc));
        assert_eq!(
            a,
            b,
            "schemas disagree at {ptr} = {value:?}\nlegacy: {:?}\ngenerated: {:?}",
            validate(&old, &doc),
            validate(&new, &doc)
        );
        if a {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    println!("targeted: {accepted} accepted / {rejected} rejected by both schemas");
    // The table must exercise both outcomes heavily to mean anything.
    assert!(
        accepted > 40 && rejected > 150,
        "{accepted} accepted / {rejected} rejected"
    );
}

fn collect_paths(value: &Value, here: &str, out: &mut Vec<String>) {
    out.push(here.to_string());
    match value {
        Value::Object(m) => {
            for (k, v) in m {
                collect_paths(
                    v,
                    &format!("{here}/{}", k.replace('~', "~0").replace('/', "~1")),
                    out,
                );
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                collect_paths(v, &format!("{here}/{i}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn structural_mutations_agree_with_the_legacy_schema() {
    let (old, new) = (legacy(), venue_schema());
    let base = rich_base();
    let mut paths = Vec::new();
    collect_paths(&base, "", &mut paths);
    let swaps = [
        json!(null),
        json!(true),
        json!(-1),
        json!("zz"),
        json!([]),
        json!({}),
    ];
    let (mut checked, mut rejected) = (0, 0);
    for path in paths.iter().filter(|p| !p.is_empty()) {
        // remove the node
        let mut cases = vec![with(&base, path, None)];
        // replace with a value of every other JSON type
        cases.extend(swaps.iter().map(|s| with(&base, path, Some(s.clone()))));
        // unknown key inside an object node
        if base.pointer(path).is_some_and(Value::is_object) {
            cases.push(with(&base, &format!("{path}/zz_unknown"), Some(json!(1))));
        }
        for doc in cases {
            let (a, b) = (ok(&old, &doc), ok(&new, &doc));
            assert_eq!(
                a,
                b,
                "schemas disagree on a mutation at {path}\nlegacy: {:?}\ngenerated: {:?}",
                validate(&old, &doc),
                validate(&new, &doc)
            );
            checked += 1;
            rejected += usize::from(!a);
        }
    }
    // unknown key at the root
    let doc = with(&base, "/zz_unknown", Some(json!(1)));
    assert!(!ok(&old, &doc) && !ok(&new, &doc));
    println!("structural: {checked} mutations ({rejected} rejected) agree");
    assert!(
        checked > 1000 && rejected > 500,
        "{checked} checked / {rejected} rejected"
    );
}

/// Constraint inventory: every distinct (keyword, value) the legacy schema
/// uses must also appear in the generated schema.
fn constraint_inventory(schema: &Value) -> BTreeSet<String> {
    const KEYWORDS: &[&str] = &[
        "pattern",
        "enum",
        "const",
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "uniqueItems",
        "format",
        "minProperties",
        "required",
        "if",
        "then",
        "not",
    ];
    fn walk(v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, child) in m {
                    if KEYWORDS.contains(&k.as_str()) {
                        // `required` / `enum` are compared as sorted sets
                        let canon = match child {
                            Value::Array(a) if k == "required" || k == "enum" => {
                                let mut items: Vec<String> =
                                    a.iter().map(Value::to_string).collect();
                                items.sort();
                                format!("[{}]", items.join(","))
                            }
                            other => other.to_string(),
                        };
                        out.insert(format!("{k}={canon}"));
                    }
                    if k == "additionalProperties" && child == &Value::Bool(false) {
                        out.insert("additionalProperties=false".to_string());
                    }
                    if k == "patternProperties" {
                        for pattern in child.as_object().into_iter().flat_map(|o| o.keys()) {
                            out.insert(format!("patternProperties:{pattern}"));
                        }
                    }
                    if k == "oneOf" {
                        out.insert(format!(
                            "oneOf[{} branches]",
                            child.as_array().map_or(0, Vec::len)
                        ));
                    }
                    walk(child, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(schema, &mut out);
    out
}

/// Differences that are intentional. The set is asserted exactly, so adding a
/// new difference (or removing one) forces a conscious edit here.
const INTENTIONAL_DIFFERENCES: &[&str] = &[];

#[test]
fn every_legacy_constraint_is_present_in_the_generated_schema() {
    let old = constraint_inventory(&legacy());
    let new = constraint_inventory(&venue_schema());
    let missing: Vec<&String> = old.difference(&new).collect();
    let expected: Vec<&str> = INTENTIONAL_DIFFERENCES.to_vec();
    assert_eq!(
        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        expected,
        "legacy constraints missing from the generated schema"
    );
    assert!(
        old.len() > 60,
        "inventory too small to be meaningful: {}",
        old.len()
    );
}

#[test]
fn lowered_demo_ir_validates_against_the_generated_site_document_schema() {
    let ir = HospitalityPlugin
        .transform_input(&demo())
        .expect("demo lowers to IR");
    let value = serde_json::to_value(&ir).expect("IR serializes");
    let schema = sws_codegen::core_roots()
        .into_iter()
        .find(|r| r.file_stem == "site-document")
        .expect("site-document root")
        .schema;
    let errors = validate(&schema, &value);
    assert!(errors.is_empty(), "{errors:?}");
}
