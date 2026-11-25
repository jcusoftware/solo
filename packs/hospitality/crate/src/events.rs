//! Public events calendar assets for the hospitality pack (issue #43 port).
//!
//! Builds the deterministic extra output files (`events-config.json` marker
//! consumed by the Worker routes and the progressive-enhancement
//! `js/events-client.js` client script) for venues with
//! `scheduling.events.enabled = true`. Sites without the config render
//! unchanged and the `/api/events` routes stay inert (404).

use std::collections::BTreeMap;

use crate::venue::{EventsSource, VenueManifest};

/// Progressive-enhancement client script for the `/events` page. Loads
/// `GET /api/events` into the semantic `#event-list` markup, formats times in
/// the page's declared time zone, attaches per-event `.ics` download links,
/// and emits `Event` JSON-LD. Without JavaScript the page links to the API.
pub const EVENTS_CLIENT_JS: &str = include_str!("events-client.js");

/// The extra static files to emit when events are enabled, keyed by output
/// path. `None` when events are disabled (no marker, no script, no routes).
#[must_use]
pub fn events_extra_files(v: &VenueManifest) -> Option<BTreeMap<String, String>> {
    let events = v.scheduling.as_ref()?.events.as_ref()?;
    if !events.enabled {
        return None;
    }
    let source = match events.source {
        EventsSource::D1OrCms => "d1_or_cms",
        EventsSource::GoogleCalendarSync => "google_calendar_sync",
    };
    let config = format!(
        "{{\"enabled\":true,\"source\":\"{source}\",\"tenant\":\"{}\"}}\n",
        v.slug
    );
    let mut files = BTreeMap::new();
    files.insert("events-config.json".to_string(), config);
    files.insert("js/events-client.js".to_string(), EVENTS_CLIENT_JS.to_string());
    Some(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_script_is_stable_and_self_contained() {
        assert!(EVENTS_CLIENT_JS.contains("fetch('/api/events')"));
        assert!(EVENTS_CLIENT_JS.contains("#event-list"));
        assert!(EVENTS_CLIENT_JS.contains("dataset.timezone"));
        assert!(!EVENTS_CLIENT_JS.contains("http://"));
        assert!(EVENTS_CLIENT_JS.starts_with("const list"));
    }
}
