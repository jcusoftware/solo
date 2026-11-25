//! Public events calendar: ICS generation (host-testable) and the
//! `GET /api/events` + `GET /api/events/:id/ics` Worker handlers.
//!
//! Ported from the pre-refactor `core/src/scheduling/events.rs` (issue #43).
//! Pure logic stays outside `#[cfg(target_arch = "wasm32")]` per AGENTS.md §3;
//! all D1 queries use bound parameters and never `unwrap()` user input.

/// Escape text for an ICS content line (RFC 5545 §3.3.11 TEXT).
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace("\r\n", "\n")
        .replace(['\r', '\n'], "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

/// Fold an ICS content line to 75 octets with a single-space continuation.
fn line(out: &mut String, value: &str) {
    let mut bytes = 0;
    for ch in value.chars() {
        if bytes + ch.len_utf8() > 75 {
            out.push_str("\r\n ");
            bytes = 1;
        }
        out.push(ch);
        bytes += ch.len_utf8();
    }
    out.push_str("\r\n");
}

/// Build a single-event RFC 5545 calendar (`text/calendar`) document.
pub fn calendar(
    id: &str,
    stamp: &str,
    start: &str,
    end: Option<&str>,
    title: &str,
    description: Option<&str>,
    location: Option<&str>,
) -> String {
    let compact = |date: &str| {
        format!(
            "{}Z",
            date.trim_end_matches('Z')
                .split('.')
                .next()
                .unwrap_or_default()
                .replace(['-', ':'], "")
        )
    };
    let mut out = String::new();
    for value in [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Solo Website System//Public Events//EN",
        "BEGIN:VEVENT",
    ] {
        line(&mut out, value);
    }
    line(&mut out, &format!("UID:{}", escape(id)));
    line(&mut out, &format!("DTSTAMP:{}", compact(stamp)));
    line(&mut out, &format!("DTSTART:{}", compact(start)));
    if let Some(end) = end {
        line(&mut out, &format!("DTEND:{}", compact(end)));
    }
    line(&mut out, &format!("SUMMARY:{}", escape(title)));
    if let Some(description) = description {
        line(&mut out, &format!("DESCRIPTION:{}", escape(description)));
    }
    if let Some(location) = location {
        line(&mut out, &format!("LOCATION:{}", escape(location)));
    }
    line(&mut out, "END:VEVENT");
    line(&mut out, "END:VCALENDAR");
    out
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use serde::{Deserialize, Serialize};
    use sws_security::response::error_body;
    use sws_security::validate::iso8601_utc;
    use worker::{Env, Method, Request, Response, Result, wasm_bindgen::JsValue};

    /// Marker file rendered into the site assets; absent files keep the
    /// endpoints inert so zero-backend sites are unaffected (issue #43).
    #[derive(Deserialize)]
    pub struct PublicEventsConfig {
        pub enabled: bool,
        #[serde(default)]
        pub source: EventsSource,
        #[serde(default)]
        pub tenant: String,
    }

    #[derive(Default, Deserialize, PartialEq)]
    #[serde(rename_all = "snake_case")]
    pub enum EventsSource {
        #[default]
        D1OrCms,
        GoogleCalendarSync,
    }

    pub async fn config(env: &Env, origin: &str) -> Result<Option<PublicEventsConfig>> {
        let url = format!("{origin}/events-config.json");
        let mut config = env
            .assets("ASSETS")?
            .fetch_request(Request::new(&url, Method::Get)?)
            .await?;
        Ok(if config.status_code() == 200 {
            config.json().await.ok()
        } else {
            None
        })
    }

    #[derive(Serialize, Deserialize)]
    struct Event {
        id: String,
        title: String,
        description: Option<String>,
        location: Option<String>,
        start_utc: String,
        end_utc: Option<String>,
    }

    const SELECT: &str = "SELECT CAST(id AS TEXT) AS id, substr(title,1,200) AS title, substr(description,1,1000) AS description, substr(location,1,300) AS location, strftime('%Y-%m-%dT%H:%M:%SZ', coalesce(start_utc, starts_at)) AS start_utc, strftime('%Y-%m-%dT%H:%M:%SZ', coalesce(end_utc, ends_at)) AS end_utc FROM events WHERE is_free = 1 AND is_ticketed = 0 AND unixepoch(coalesce(start_utc, starts_at)) >= unixepoch('now')";

    // Bound the age of the last committed full snapshot (30-minute schedule + 15-minute grace).
    // Imported rows are only listed while their source calendar still matches the
    // provisioned secret; the `?1` prefix binds `gcal:<len>:<calendar_id>:`.
    const VISIBILITY: &str = " AND (external_uid IS NULL OR external_uid NOT GLOB 'gcal:*' OR (
        substr(external_uid, 1, length(?1)) = ?1 AND EXISTS (
            SELECT 1 FROM event_sync_state WHERE id = 1 AND last_schedule_ms > 0
            AND last_schedule_ms BETWEEN (unixepoch('now') - 2700) * 1000 AND unixepoch('now') * 1000
        )))";

    pub async fn handle(req: &Request, env: &Env, id: Option<&str>) -> Result<Response> {
        let not_found = || Ok(Response::from_json(&error_body("not_found"))?.with_status(404));
        let url = req.url()?;
        let Some(cfg) = config(env, &url.origin().ascii_serialization())
            .await?
            .filter(|c| c.enabled)
        else {
            return not_found();
        };
        if req.method() != Method::Get {
            let response = Response::from_json(&error_body("method_not_allowed"))?.with_status(405);
            response.headers().set("Allow", "GET")?;
            return Ok(response);
        }
        let db = env.d1("DB")?;
        let prefix = if cfg.source == EventsSource::GoogleCalendarSync && !cfg.tenant.is_empty() {
            env.secret("GOOGLE_SYNC_CALENDAR_ID").ok().and_then(|secret| {
                let value = secret.to_string();
                let calendar = value.trim();
                (!calendar.is_empty() && calendar.len() <= 320 && !matches!(calendar, "." | ".."))
                    .then(|| format!("gcal:{}:{calendar}:", calendar.len()))
            })
        } else {
            None
        };
        let prefix = prefix.as_deref().map_or(JsValue::NULL, JsValue::from_str);
        if let Some(id) = id {
            if !crate::routes::is_valid_event_id(id) {
                return not_found();
            }
            let Some(event) = db
                .prepare(format!("{SELECT}{VISIBILITY} AND id = ?2"))
                .bind(&[prefix, JsValue::from_str(id)])?
                .first::<Event>(None)
                .await?
            else {
                return not_found();
            };
            let uid = format!(
                "event-{}@{}",
                event.id,
                url.host_str().unwrap_or("sws.example")
            );
            let body = super::calendar(
                &uid,
                &iso8601_utc(worker::Date::now().as_millis()),
                &event.start_utc,
                event.end_utc.as_deref(),
                &event.title,
                event.description.as_deref(),
                event.location.as_deref(),
            );
            let response = Response::ok(body)?;
            response.headers().set("Cache-Control", "no-store")?;
            response
                .headers()
                .set("Content-Type", "text/calendar; charset=utf-8")?;
            response
                .headers()
                .set("Content-Disposition", "attachment; filename=event.ics")?;
            return Ok(response);
        }
        let events = db
            .prepare(format!("{SELECT}{VISIBILITY} ORDER BY start_utc, events.id LIMIT 50"))
            .bind(&[prefix])?
            .all()
            .await?
            .results::<Event>()?;
        let response = Response::from_json(&serde_json::json!({ "events": events }))?;
        response.headers().set("Cache-Control", "no-store")?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_text_and_folds_utf8_by_octets() {
        let title = format!("{},;\\\r\nNext", "é".repeat(70));
        let ics = calendar(
            "event-1@example.test",
            "2026-01-01T00:00:00Z",
            "2026-01-02T10:00:00Z",
            None,
            &title,
            None,
            None,
        );
        assert!(ics.split("\r\n").all(|line| line.len() <= 75));
        assert!(ics.contains("DTSTART:20260102T100000Z\r\n"));
        assert!(
            ics.replace("\r\n ", "")
                .contains(&format!("SUMMARY:{}\\,\\;\\\\\\nNext\r\n", "é".repeat(70)))
        );
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
    }
}
