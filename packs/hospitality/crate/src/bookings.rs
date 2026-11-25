//! Call booking assets for the hospitality pack (issue #44 port).
//!
//! Builds the deterministic extra output files (`booking-config.json` marker
//! consumed by the Worker routes and the progressive-enhancement
//! `js/booking-client.js` client script) for venues with
//! `scheduling.bookings.enabled = true`. Sites without the config render
//! unchanged and the `/api/bookings` routes stay inert (404).

use std::collections::BTreeMap;

use serde_json::json;

use crate::venue::VenueManifest;

/// Progressive-enhancement client script for the `/book-a-call` page. Loads
/// `GET /api/bookings/slots` into keyboard-selectable radio choices, validates
/// guest details inline, and submits `POST /api/bookings` with the Turnstile
/// token. Without JavaScript the form stays hidden and the noscript notice
/// offers the phone fallback (fail-closed submission).
pub const BOOKING_CLIENT_JS: &str = include_str!("booking-client.js");

/// The extra static files to emit when booking is enabled, keyed by output
/// path. `None` when booking is disabled (no marker, no script, no routes).
#[must_use]
pub fn bookings_extra_files(v: &VenueManifest) -> Option<BTreeMap<String, String>> {
    let events = v.scheduling.as_ref()?.events.as_ref()?;
    let bookings = v.scheduling.as_ref()?.bookings.as_ref()?;
    if !bookings.enabled {
        return None;
    }
    // The worker reads the real calendar from its GOOGLE_BOOKING_CALENDAR_ID
    // secret; the manifest's declarative mailbox ID is never shipped publicly.
    let config = json!({
        "enabled": true,
        "tenant": v.slug,
        "timezone": events.timezone.trim(),
        "slot_duration_minutes": bookings.slot_duration_minutes,
        "buffer_minutes": bookings.buffer_minutes,
        "working_hours": bookings.working_hours,
        "auto_zoom": bookings.auto_zoom,
    })
    .to_string();
    let mut files = BTreeMap::new();
    files.insert("booking-config.json".to_string(), format!("{config}\n"));
    files.insert(
        "js/booking-client.js".to_string(),
        BOOKING_CLIENT_JS.to_string(),
    );
    Some(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_script_is_stable_and_self_contained() {
        assert!(BOOKING_CLIENT_JS.contains("/api/bookings/slots"));
        assert!(BOOKING_CLIENT_JS.contains("cf-turnstile-response"));
        assert!(BOOKING_CLIENT_JS.contains("dataset.timezone"));
        assert!(BOOKING_CLIENT_JS.contains("byId('confirmation')"));
        assert!(!BOOKING_CLIENT_JS.contains("http://"));
        assert!(!BOOKING_CLIENT_JS.contains("https://"));
        assert!(BOOKING_CLIENT_JS.starts_with("const byId"));
    }

    #[test]
    fn config_marker_fictional_and_deterministic() {
        let mut v: VenueManifest =
            serde_json::from_str(include_str!("../../demo/venue.json")).unwrap();
        let mut bookings = v.scheduling.as_ref().unwrap().bookings.clone().unwrap();
        bookings.enabled = true;
        bookings.working_hours.remove("mon");
        bookings.working_hours.insert("sat".into(), ["10:00".into(), "16:00".into()]);
        v.scheduling.as_mut().unwrap().bookings = Some(bookings);
        let files = bookings_extra_files(&v).expect("booking assets");
        let config = files.get("booking-config.json").unwrap();
        let value: serde_json::Value = serde_json::from_str(config).unwrap();
        assert_eq!(value["enabled"], json!(true));
        assert_eq!(value["tenant"], json!("shirlington-hearth-demo"));
        assert_eq!(value["timezone"], json!("America/New_York"));
        assert_eq!(value["slot_duration_minutes"], json!(30));
        assert_eq!(value["working_hours"]["sat"], json!(["10:00", "16:00"]));
        assert!(value.get("google_workspace_calendar_id").is_none());
        assert!(files.contains_key("js/booking-client.js"));

        // Disabling booking removes the marker and script deterministically.
        v.scheduling
            .as_mut()
            .unwrap()
            .bookings
            .as_mut()
            .unwrap()
            .enabled = false;
        assert!(bookings_extra_files(&v).is_none());
    }
}
