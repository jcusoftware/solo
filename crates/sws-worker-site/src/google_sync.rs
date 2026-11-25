//! Google Calendar snapshot import for the public events calendar (issue #43
//! port; config-gated — inert without an enabled `events-config.json`).
//!
//! Scheduled runs fetch a complete 180-day snapshot (at most ten pages) and
//! atomically update and prune the importer's own rows in one D1 batch.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sws_security::validate::iso8601_utc;
use worker::{Date, Env, Error, Headers, Method, Result, Url, wasm_bindgen::JsValue};

use super::events::wasm::{EventsSource, config};
use super::google_auth::wasm::{access_token, fetch_json};
use super::timestamp::google_datetime_utc;

const READ_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events.readonly";
const WINDOW_MS: u64 = 180 * 24 * 60 * 60 * 1_000;
const MAX_PAGES: usize = 10;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    kind: String,
    #[serde(default)]
    items: Vec<GoogleEvent>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct GoogleEvent {
    id: String,
    visibility: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    start: Option<GoogleTime>,
    end: Option<GoogleTime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleTime {
    date_time: Option<String>,
}

#[derive(Serialize)]
struct Imported {
    external_uid: String,
    title: String,
    description: Option<String>,
    location: Option<String>,
    start_utc: String,
    end_utc: String,
}

fn sync_error() -> Error {
    Error::RustError("Google Calendar snapshot was not imported".into())
}

fn imported(event: GoogleEvent, external_uid: String) -> Result<Option<Imported>> {
    if event.visibility.as_deref() != Some("public") || event.status.as_deref() == Some("cancelled")
    {
        return Ok(None);
    }
    let (Some(start), Some(end)) = (
        event.start.and_then(|t| t.date_time),
        event.end.and_then(|t| t.date_time),
    ) else {
        // All-day events have date-only bounds; do not guess their time zone.
        return Ok(None);
    };
    let normalize = |s: &str| google_datetime_utc(s).ok_or_else(sync_error);
    let (start_utc, end_utc) = (normalize(&start)?, normalize(&end)?);
    if end_utc <= start_utc {
        return Err(sync_error());
    }
    Ok(Some(Imported {
        external_uid,
        title: event
            .summary
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Public event".into())
            .chars()
            .take(200)
            .collect(),
        description: event.description.map(|s| s.chars().take(1_000).collect()),
        location: event.location.map(|s| s.chars().take(300).collect()),
        start_utc,
        end_utc,
    }))
}

pub async fn sync(env: &Env, schedule_ms: f64) -> Result<()> {
    // The Assets binding resolves the path within this deployment, not over HTTP.
    let Some(config) = config(env, "https://assets.example").await? else {
        return Ok(());
    };
    if !config.enabled || config.source != EventsSource::GoogleCalendarSync {
        return Ok(());
    }
    if config.tenant.is_empty() || !schedule_ms.is_normal() || schedule_ms <= 0.0 {
        return Err(sync_error());
    }
    let setting = |name: &str| {
        env.secret(name)
            .ok()
            .map(|v| v.to_string().trim().to_owned())
            .filter(|v| !v.is_empty())
            .ok_or_else(sync_error)
    };
    let calendar = setting("GOOGLE_SYNC_CALENDAR_ID")?;
    let subject = setting("GOOGLE_SYNC_SUBJECT")?;
    if calendar.len() > 320 || matches!(calendar.as_str(), "." | "..") {
        return Err(sync_error());
    }
    let bearer = access_token(env, &config.tenant, &subject, READ_SCOPE).await?;
    let mut base = Url::parse("https://www.googleapis.com/calendar/v3/calendars/")
        .map_err(|_| sync_error())?;
    base.path_segments_mut()
        .map_err(|_| sync_error())?
        .pop_if_empty()
        .push(&calendar)
        .push("events");
    let now = Date::now().as_millis();
    base.query_pairs_mut()
        .append_pair("timeMin", &iso8601_utc(now))
        .append_pair("timeMax", &iso8601_utc(now.saturating_add(WINDOW_MS)))
        .append_pair("singleEvents", "true")
        .append_pair("showDeleted", "false")
        .append_pair("eventTypes", "default")
        .append_pair("maxResults", "100")
        .append_pair("fields", "kind,nextPageToken,items(id,visibility,status,summary,description,location,start(dateTime),end(dateTime))");

    let mut next = None::<String>;
    let mut seen_tokens = HashSet::new();
    let mut rows = HashMap::new();
    for _ in 0..MAX_PAGES {
        let mut url = base.clone();
        if let Some(token) = next.take() {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }
        let headers = Headers::new();
        headers.set("Authorization", &format!("Bearer {bearer}"))?;
        let page: Page = fetch_json(url.as_str(), Method::Get, headers, None, 256 * 1_024).await?;
        if page.kind != "calendar#events" {
            return Err(sync_error());
        }
        for event in page.items {
            if event.id.is_empty() || event.id.len() > 1_024 {
                return Err(sync_error());
            }
            let uid = format!("gcal:{}:{}:{}", calendar.len(), calendar, event.id);
            // Conflicting duplicates make the snapshot ambiguous; publish neither version.
            if rows.insert(uid.clone(), imported(event, uid)?).is_some() {
                return Err(sync_error());
            }
        }
        match page.next_page_token {
            None => return apply(env, schedule_ms, rows.into_values().flatten().collect()).await,
            Some(token)
                if !token.is_empty()
                    && token.len() <= 4_096
                    && seen_tokens.insert(token.clone()) =>
            {
                next = Some(token)
            }
            Some(_) => return Err(sync_error()),
        }
    }
    Err(sync_error())
}

async fn apply(env: &Env, schedule_ms: f64, rows: Vec<Imported>) -> Result<()> {
    let snapshot = serde_json::to_string(&rows).map_err(|_| sync_error())?;
    if snapshot.len() > 1_000_000 {
        return Err(sync_error());
    }
    let db = env.d1("DB")?;
    let args = [
        JsValue::from_str(&snapshot),
        JsValue::from_f64(schedule_ms.floor()),
    ];
    // D1 batches are transactional. A stale or retried Cron run cannot replace a newer snapshot.
    // The `gcal:` namespace is reserved for this importer; manual rows are never selected for deletion.
    db.batch(vec![
        db.prepare("INSERT INTO events (slug, external_uid, title, description, location, starts_at, ends_at, start_utc, end_utc, is_free, updated_at)
            SELECT 'gcal-' || lower(hex(j.value ->> '$.external_uid')), j.value ->> '$.external_uid',
                j.value ->> '$.title', j.value ->> '$.description', j.value ->> '$.location',
                j.value ->> '$.start_utc', j.value ->> '$.end_utc', j.value ->> '$.start_utc', j.value ->> '$.end_utc', 1, CURRENT_TIMESTAMP
            FROM json_each(?1) j WHERE (SELECT last_schedule_ms FROM event_sync_state WHERE id = 1) < ?2
            ON CONFLICT(external_uid) DO UPDATE SET title = excluded.title, description = excluded.description,
                location = excluded.location, starts_at = excluded.starts_at, ends_at = excluded.ends_at,
                start_utc = excluded.start_utc, end_utc = excluded.end_utc, is_free = 1, is_ticketed = 0,
                updated_at = CURRENT_TIMESTAMP").bind(&args)?,
        db.prepare("DELETE FROM events WHERE external_uid GLOB 'gcal:*'
            AND (SELECT last_schedule_ms FROM event_sync_state WHERE id = 1) < ?2
            AND external_uid NOT IN (SELECT value ->> '$.external_uid' FROM json_each(?1))").bind(&args)?,
        db.prepare("UPDATE event_sync_state SET last_schedule_ms = ?1 WHERE id = 1 AND last_schedule_ms < ?1")
            .bind(&args[1..])?,
    ]).await.map_err(|_| sync_error())?;
    Ok(())
}
