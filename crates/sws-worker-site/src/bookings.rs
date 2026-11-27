//! Call booking: pure slot math (host-testable) and the `POST /api/bookings`
//! + `GET /api/bookings/slots` Worker handlers.
//!
//! Ported from the pre-refactor `core/src/scheduling/bookings.rs` (issue #44).
//! The pure logic stays outside `#[cfg(target_arch = "wasm32")]` per AGENTS.md
//! §3; all D1 queries use bound parameters and never `unwrap()` user input.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    pub start: i64,
    pub end: i64,
}

pub fn overlaps(a: Interval, b: Interval, gap_seconds: i64) -> bool {
    a.start < b.end + gap_seconds && b.start < a.end + gap_seconds
}

pub fn slot_at<E>(
    wall_start: i64,
    duration: i64,
    offset: i64,
    now: i64,
    wall: impl Fn(i64) -> Result<i64, E>,
) -> Result<Option<Interval>, E> {
    let start = wall_start - offset;
    let end = start + duration;
    // A wall-clock label can identify no instant or two instants; round-trip both endpoints.
    Ok(
        (start > now && wall(start)? == wall_start && wall(end)? == wall_start + duration)
            .then_some(Interval { start, end }),
    )
}

/// Workers rate-limit binding guarding `GET /api/bookings/slots`.
pub const SLOTS_LIMIT_BINDING: &str = "BOOKING_SLOTS_LIMIT";

/// Maps the slot-listing rate-limit check to a rejection (`status`, error code),
/// or `None` when the request may proceed. `check` is `None` when the binding is
/// missing or the limiter call failed: fail closed rather than serve unlimited.
pub fn slots_limit_rejection(check: Option<bool>) -> Option<(u16, &'static str)> {
    match check {
        Some(true) => None,
        Some(false) => Some((429, "rate_limited")),
        None => Some((503, "rate_limit_unavailable")),
    }
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    use serde::Deserialize;
    use serde_json::{Value, json};
    use sws_security::response::error_body;
    use sws_security::validate::{self, iso8601_utc};
    use sws_security::worker_utils::{read_fields, verify_turnstile};
    use worker::{
        D1Database, Env, Error, Headers, Method, Request, Response, Result, Url,
        js_sys::{Array, Date as JsDate, Function, Reflect, global},
        wasm_bindgen::{JsCast, JsValue},
        web_sys::WorkerGlobalScope,
    };

    use super::{Interval, SLOTS_LIMIT_BINDING, overlaps, slot_at, slots_limit_rejection};
    use crate::google_auth::wasm::{access_token, fetch_json};
    use crate::timestamp::google_datetime_seconds;
    use crate::zoom::wasm as zoom;

    const SCOPES: &str = "https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/calendar.freebusy";
    const DAY: i64 = 86_400;

    #[derive(Deserialize)]
    struct Config {
        enabled: bool,
        tenant: String,
        timezone: String,
        slot_duration_minutes: u16,
        buffer_minutes: u16,
        working_hours: BTreeMap<Weekday, [String; 2]>,
        auto_zoom: bool,
    }

    #[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord)]
    #[serde(rename_all = "lowercase")]
    enum Weekday {
        Sun,
        Mon,
        Tue,
        Wed,
        Thu,
        Fri,
        Sat,
    }

    impl Config {
        fn buffer(&self) -> i64 {
            i64::from(self.buffer_minutes) * 60
        }
        fn duration(&self) -> i64 {
            i64::from(self.slot_duration_minutes) * 60
        }
    }

    fn unavailable() -> Error {
        Error::RustError("Booking service unavailable".into())
    }

    fn timestamp(sec: i64) -> String {
        iso8601_utc(sec as u64 * 1000)
    }

    fn response(status: u16, body: &Value) -> Result<Response> {
        let resp = Response::from_json(body)?.with_status(status);
        resp.headers().set("Cache-Control", "no-store")?;
        Ok(resp)
    }

    fn reject(status: u16, code: &str) -> Result<Response> {
        response(status, &error_body(code))
    }

    async fn config(env: &Env, url: &Url) -> Result<Option<Config>> {
        let asset = format!("{}/booking-config.json", url.origin().ascii_serialization());
        let mut response = env
            .assets("ASSETS")?
            .fetch_request(Request::new(&asset, Method::Get)?)
            .await?;
        if response.status_code() != 200 {
            return Ok(None);
        }
        let cfg: Config = response.json().await.map_err(|_| unavailable())?;
        if !cfg.enabled {
            return Ok(None);
        }
        if cfg.tenant.is_empty()
            || !(5..=240).contains(&cfg.slot_duration_minutes)
            || cfg.buffer_minutes > 1440
            || cfg.working_hours.is_empty()
        {
            return Err(unavailable());
        }
        Ok(Some(cfg))
    }

    /// Use the runtime's IANA time-zone rules, not an assumed numeric offset.
    struct LocalClock {
        formatter: JsValue,
        parts: Function,
    }

    impl LocalClock {
        fn new(zone: &str) -> Result<Self> {
            let get = |v: &JsValue, key| {
                Reflect::get(v, &JsValue::from_str(key)).map_err(|_| unavailable())
            };
            let intl = get(&global(), "Intl")?;
            let ctor: Function = get(&intl, "DateTimeFormat")?
                .dyn_into()
                .map_err(|_| unavailable())?;
            let opts = worker::js_sys::Object::new();
            for (key, val) in [
                ("timeZone", zone),
                ("year", "numeric"),
                ("month", "2-digit"),
                ("day", "2-digit"),
                ("hour", "2-digit"),
                ("minute", "2-digit"),
                ("second", "2-digit"),
                ("hourCycle", "h23"),
            ] {
                Reflect::set(&opts, &key.into(), &val.into()).map_err(|_| unavailable())?;
            }
            let formatter = Reflect::construct(&ctor, &Array::of2(&"en-US".into(), &opts))
                .map_err(|_| unavailable())?;
            let parts = get(&formatter, "formatToParts")?
                .dyn_into()
                .map_err(|_| unavailable())?;
            Ok(Self { formatter, parts })
        }

        /// Wall clock represented on a UTC-shaped numeric axis, not a UTC instant.
        fn wall(&self, utc: i64) -> Result<i64> {
            let date = JsDate::new(&JsValue::from_f64((utc * 1000) as f64));
            let parts: Array = self
                .parts
                .call1(&self.formatter, date.as_ref())
                .map_err(|_| unavailable())?
                .dyn_into()
                .map_err(|_| unavailable())?;
            let mut values = [
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ];
            for part in parts.iter() {
                let get = |k| {
                    Reflect::get(&part, &JsValue::from_str(k))
                        .ok()
                        .and_then(|v| v.as_string())
                };
                if let Some(i) = ["year", "month", "day", "hour", "minute", "second"]
                    .iter()
                    .position(|k| Some(*k) == get("type").as_deref())
                {
                    values[i] = get("value").ok_or_else(unavailable)?;
                }
            }
            let [y, mo, d, h, mi, s] = values;
            google_datetime_seconds(&format!("{y}-{mo}-{d}T{h}:{mi}:{s}Z")).ok_or_else(unavailable)
        }
    }

    fn window(
        cfg: &Config,
        local: &LocalClock,
        day: i64,
        now: i64,
    ) -> Result<Option<Vec<Interval>>> {
        let today = local.wall(now)?.div_euclid(DAY) * DAY;
        if !(today..today + 60 * DAY).contains(&day) {
            return Ok(None);
        }
        let weekdays = [
            Weekday::Sun,
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
        ];
        let weekday = &weekdays[(day.div_euclid(DAY) + 4).rem_euclid(7) as usize];
        let Some([open, close]) = cfg.working_hours.get(weekday) else {
            return Ok(Some(Vec::new()));
        };
        let parse = |time: &str| {
            google_datetime_seconds(&format!("2000-01-01T{time}:00Z"))
                .filter(|_| time.len() == 5)
                .map(|sec| sec.rem_euclid(DAY))
                .ok_or_else(unavailable)
        };
        let (mut from, to) = (day + parse(open)?, day + parse(close)?);
        if to <= from {
            return Err(unavailable());
        }
        let mut offsets = BTreeSet::new();
        for h in (-24..=48).step_by(3) {
            let sample = day + h * 3600;
            offsets.insert(local.wall(sample)? - sample);
        }
        let mut slots = Vec::new();
        while from + cfg.duration() <= to {
            for &offset in &offsets {
                if let Some(slot) = slot_at(from, cfg.duration(), offset, now, |t| local.wall(t))? {
                    slots.push(slot);
                }
            }
            from += cfg.duration() + cfg.buffer();
        }
        slots.sort_by_key(|slot| slot.start);
        Ok(Some(slots))
    }

    #[derive(Deserialize)]
    struct Conflict {
        start: i64,
        end: i64,
        buffer_minutes: i64,
    }

    async fn conflicts(
        db: &D1Database,
        tenant: &str,
        range: Interval,
        buffer: i64,
    ) -> Result<Vec<Conflict>> {
        let rows = db.prepare("SELECT unixepoch(start_utc) AS start, unixepoch(end_utc) AS end, buffer_minutes FROM bookings \
            WHERE tenant_slug = ?1 AND status IN ('pending','uncertain','confirmed') \
            AND unixepoch(start_utc) < ?3 + max(?4, 60 * buffer_minutes) \
            AND ?2 < unixepoch(end_utc) + max(?4, 60 * buffer_minutes) LIMIT 501")
            .bind(&[tenant.into(), (range.start as f64).into(), (range.end as f64).into(), (buffer as f64).into()])?
            .all().await?.results::<Conflict>()?;
        if rows.len() > 500 {
            return Err(unavailable());
        }
        Ok(rows)
    }

    #[derive(Deserialize)]
    struct BusyResponse {
        calendars: HashMap<String, CalendarBusy>,
    }
    #[derive(Deserialize)]
    struct CalendarBusy {
        #[serde(default)]
        errors: Vec<Value>,
        busy: Vec<BusyPeriod>,
    }
    #[derive(Deserialize)]
    struct BusyPeriod {
        start: String,
        end: String,
    }

    fn calendar(env: &Env) -> Result<String> {
        let calendar = env
            .secret("GOOGLE_BOOKING_CALENDAR_ID")
            .map_err(|_| unavailable())?
            .to_string()
            .trim()
            .to_owned();
        if !validate::is_valid_email(&calendar) {
            return Err(unavailable());
        }
        Ok(calendar)
    }

    fn google_headers(bearer: &str) -> Result<Headers> {
        let headers = Headers::new();
        headers.set("Authorization", &format!("Bearer {bearer}"))?;
        headers.set("Content-Type", "application/json")?;
        Ok(headers)
    }

    async fn busy(
        calendar: &str,
        bearer: &str,
        range: Interval,
        buffer: i64,
    ) -> Result<Vec<Interval>> {
        let body = json!({ "timeMin": timestamp(range.start - buffer), "timeMax": timestamp(range.end + buffer),
            "items": [{ "id": calendar }] }).to_string();
        let mut response: BusyResponse = fetch_json(
            "https://www.googleapis.com/calendar/v3/freeBusy",
            Method::Post,
            google_headers(bearer)?,
            Some(&body),
            128 * 1024,
        )
        .await?;
        let cal = response
            .calendars
            .remove(calendar)
            .ok_or_else(unavailable)?;
        if !cal.errors.is_empty() {
            return Err(unavailable());
        }
        cal.busy
            .into_iter()
            .map(|b| {
                let start = google_datetime_seconds(&b.start).ok_or_else(unavailable)?;
                let end = google_datetime_seconds(&b.end).ok_or_else(unavailable)?;
                let fractional = b.end.split_once('.').is_some_and(|(_, s)| {
                    s.bytes().take_while(u8::is_ascii_digit).any(|b| b != b'0')
                });
                let end = end + i64::from(fractional);
                if start >= end {
                    return Err(unavailable());
                }
                Ok(Interval { start, end })
            })
            .collect()
    }

    async fn slots(
        cfg: &Config,
        env: &Env,
        url: &Url,
        clock: &LocalClock,
        now: i64,
    ) -> Result<Response> {
        let dates: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "date")
            .take(2)
            .collect();
        let [(_, date)] = dates.as_slice() else {
            return reject(400, "invalid_date");
        };
        if !validate::is_valid_date(date) {
            return reject(400, "invalid_date");
        }
        let day = google_datetime_seconds(&format!("{date}T00:00:00Z")).ok_or_else(unavailable)?;
        let Some(mut slots) = window(cfg, clock, day, now)? else {
            return reject(400, "date_outside_booking_horizon");
        };
        if let (Some(first), Some(last)) = (slots.first(), slots.last()) {
            let range = Interval {
                start: first.start,
                end: last.end,
            };
            let db = env.d1("DB")?;
            let claims = conflicts(&db, &cfg.tenant, range, cfg.buffer()).await?;
            let calendar = calendar(env)?;
            let bearer = access_token(env, &cfg.tenant, &calendar, SCOPES).await?;
            let busy = busy(&calendar, &bearer, range, cfg.buffer()).await?;
            slots.retain(|slot| {
                claims.iter().all(|c| {
                    !overlaps(
                        *slot,
                        Interval {
                            start: c.start,
                            end: c.end,
                        },
                        cfg.buffer().max(c.buffer_minutes * 60),
                    )
                }) && busy.iter().all(|b| !overlaps(*slot, *b, cfg.buffer()))
            });
        }
        let slots: Vec<_> = slots
            .into_iter()
            .map(|slot| json!({"start_utc":timestamp(slot.start), "end_utc":timestamp(slot.end)}))
            .collect();
        response(200, &json!({"slots":slots}))
    }

    async fn update(db: &D1Database, sql: &str, args: &[JsValue]) -> Result<()> {
        let result = db.prepare(sql).bind(args)?.run().await?;
        if !result.success() || result.meta()?.and_then(|meta| meta.changes) != Some(1) {
            return Err(unavailable());
        }
        Ok(())
    }

    #[derive(Deserialize)]
    struct CalendarInsert {
        id: String,
    }

    async fn write_calendar(
        calendar: &str,
        bearer: &str,
        reference: &str,
        email: &str,
        dates: [&str; 2],
        link: Option<&str>,
    ) -> Result<()> {
        let mut url = Url::parse("https://www.googleapis.com/calendar/v3/calendars/")
            .map_err(|_| unavailable())?;
        url.path_segments_mut()
            .map_err(|_| unavailable())?
            .pop_if_empty()
            .push(calendar)
            .push("events");
        url.query_pairs_mut()
            .append_pair("sendUpdates", "all")
            .append_pair("fields", "id");
        let [start, end] = dates;
        let mut body = json!({ "id": reference, "summary": "Venue call", "visibility": "private",
            "start": { "dateTime": start }, "end": { "dateTime": end }, "attendees": [{ "email": email }] });
        if let Some(link) = link {
            body["location"] = json!(link);
            body["description"] = json!(format!("Join the scheduled call: {link}"));
        }
        let inserted: CalendarInsert = fetch_json(
            url.as_str(),
            Method::Post,
            google_headers(bearer)?,
            Some(&body.to_string()),
            64 * 1024,
        )
        .await?;
        if inserted.id != reference {
            return Err(unavailable());
        }
        Ok(())
    }

    async fn book(
        cfg: &Config,
        env: &Env,
        req: &mut Request,
        clock: &LocalClock,
        now: i64,
    ) -> Result<Response> {
        let fields = match read_fields(req).await? {
            Ok(fields) => fields,
            Err(resp) => return Ok(resp),
        };
        let field = |key| fields.get(key).map_or("", |s| s.trim());
        let (name, email, notes, start) = (
            field("guest_name"),
            field("guest_email"),
            field("guest_notes"),
            field("start_utc"),
        );
        let invalid = |text: &str, max, multiline| {
            text.chars().count() > max
                || text
                    .chars()
                    .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
        };
        if name.is_empty()
            || invalid(name, 100, false)
            || !validate::is_valid_email(email)
            || invalid(notes, 4000, true)
        {
            return reject(400, "invalid_guest_details");
        }
        let Some(start_sec) = google_datetime_seconds(start).filter(|s| timestamp(*s) == start)
        else {
            return reject(400, "invalid_slot");
        };
        if start_sec <= now || start_sec > now + 61 * DAY {
            return reject(409, "slot_unavailable");
        }
        let day = clock.wall(start_sec)?.div_euclid(DAY) * DAY;
        let Some(slot) = window(cfg, clock, day, now)?
            .and_then(|slots| slots.into_iter().find(|slot| slot.start == start_sec))
        else {
            return reject(409, "slot_unavailable");
        };
        let token = fields
            .get("cf-turnstile-response")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && s.len() <= 2048);
        if let Some(resp) = verify_turnstile(env, req, token, "booking").await? {
            resp.headers().set("Cache-Control", "no-store")?;
            return Ok(resp);
        }
        let calendar = calendar(env)?;
        let bearer = access_token(env, &cfg.tenant, &calendar, SCOPES).await?;
        let zoom_host = if cfg.auto_zoom {
            Some((
                zoom::meetings_url(env)?,
                zoom::access_token(env, &cfg.tenant).await?,
            ))
        } else {
            None
        };
        if busy(&calendar, &bearer, slot, cfg.buffer())
            .await?
            .iter()
            .any(|b| overlaps(slot, *b, cfg.buffer()))
            || slot.start <= (worker::Date::now().as_millis() / 1000) as i64
        {
            return reject(409, "slot_unavailable");
        }
        let scope: WorkerGlobalScope = global().unchecked_into();
        let reference = format!(
            "b{}",
            scope
                .crypto()
                .map_err(|_| unavailable())?
                .random_uuid()
                .replace('-', "")
        );
        let end = timestamp(slot.end);
        let db = env.d1("DB")?;
        // This single INSERT and the D1 trigger use the same half-open overlap rule atomically.
        let id = db.prepare("INSERT INTO bookings (tenant_slug,guest_name,guest_email,guest_notes,start_utc,end_utc,buffer_minutes,gcal_event_id) \
            SELECT ?1,?2,?3,?4,?5,?6,?7,?8 WHERE NOT EXISTS (SELECT 1 FROM bookings \
                WHERE tenant_slug = ?1 AND status IN ('pending','uncertain','confirmed') \
                AND unixepoch(?5) < unixepoch(end_utc) + 60 * max(?7,buffer_minutes) \
                AND unixepoch(start_utc) < unixepoch(?6) + 60 * max(?7,buffer_minutes)) RETURNING id")
            .bind(&[cfg.tenant.as_str().into(), name.into(), email.into(), if notes.is_empty() { JsValue::NULL } else { notes.into() },
                start.into(), end.as_str().into(), f64::from(cfg.buffer_minutes).into(), reference.as_str().into()])?.first::<f64>(Some("id")).await?;
        let Some(id) = id else {
            return reject(409, "slot_unavailable");
        };
        let id = JsValue::from_f64(id);

        let external = async {
            let mut zoom_link = None;
            if let Some((url, token)) = zoom_host {
                let meeting = zoom::create(&url, &token, &reference, start, cfg.slot_duration_minutes).await?;
                update(&db, "UPDATE bookings SET zoom_meeting_id=?2, zoom_join_url=?3 WHERE id=?1 AND status='pending'",
                    &[id.clone(), meeting.id.to_string().into(), meeting.join_url.as_str().into()]).await?;
                zoom_link = Some(meeting.join_url);
            }
            write_calendar(&calendar, &bearer, &reference, email, [start, &end], zoom_link.as_deref()).await?;
            update(
                &db,
                "UPDATE bookings SET status='confirmed' WHERE id=?1 AND status='pending'",
                std::slice::from_ref(&id),
            )
            .await
        }.await;
        if external.is_err() {
            // A crash or failed status update leaves a blocking pending claim; never retry provider writes automatically.
            let _ = update(
                &db,
                "UPDATE bookings SET status='uncertain' WHERE id=?1 AND status='pending'",
                &[id],
            )
            .await;
            worker::console_error!("Booking requires reconciliation: {reference}");
            return response(
                503,
                &json!({ "ok":false, "error":"booking_pending_review", "reference":reference }),
            );
        }
        response(
            201,
            &json!({ "ok":true, "status":"confirmed", "start_utc":start, "end_utc":end, "calendar_invitation":true }),
        )
    }

    pub async fn handle(req: &mut Request, env: &Env, is_slots: bool) -> Result<Response> {
        let result = async {
            let url = req.url()?;
            let Some(cfg) = config(env, &url).await? else {
                return reject(404, "not_found");
            };
            let (method, allow) = if is_slots {
                (Method::Get, "GET")
            } else {
                (Method::Post, "POST")
            };
            if req.method() != method {
                let resp = reject(405, "method_not_allowed")?;
                resp.headers().set("Allow", allow)?;
                return Ok(resp);
            }
            let clock = LocalClock::new(&cfg.timezone)?;
            let now = (worker::Date::now().as_millis() / 1000) as i64;
            if is_slots {
                let ip = req.headers().get("CF-Connecting-IP")?;
                let key = format!("{}:{}", cfg.tenant, ip.as_deref().unwrap_or("unknown"));
                let check = match env.rate_limiter(SLOTS_LIMIT_BINDING) {
                    Ok(limiter) => limiter.limit(key).await.ok().map(|outcome| outcome.success),
                    Err(_) => None,
                };
                if let Some((status, code)) = slots_limit_rejection(check) {
                    let response = reject(status, code)?;
                    if check.is_none() {
                        worker::console_error!("{SLOTS_LIMIT_BINDING} rate limiter unavailable");
                    } else {
                        response.headers().set("Retry-After", "60")?;
                    }
                    return Ok(response);
                }
                slots(&cfg, env, &url, &clock, now).await
            } else {
                book(&cfg, env, req, &clock, now).await
            }
        }
        .await;
        match result {
            Ok(resp) => Ok(resp),
            Err(_) => {
                worker::console_error!("Booking request failed before confirmation");
                reject(503, "booking_unavailable")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_slots_skip_the_gap_and_keep_both_fold_occurrences() {
        let spring = |utc| {
            Ok::<_, ()>(
                utc + if utc >= 7 * 3600 {
                    -4 * 3600
                } else {
                    -5 * 3600
                },
            )
        };
        let autumn = |utc| {
            Ok::<_, ()>(
                utc + if utc >= 6 * 3600 {
                    -5 * 3600
                } else {
                    -4 * 3600
                },
            )
        };
        for offset in [-4 * 3600, -5 * 3600] {
            assert_eq!(slot_at(2 * 3600, 900, offset, 0, spring), Ok(None));
            assert_eq!(
                slot_at(3600, 900, offset, 0, autumn),
                Ok(Some(Interval {
                    start: 3600 - offset,
                    end: 4500 - offset,
                }))
            );
        }
        assert_eq!(slot_at(6_300, 900, -5 * 3600, 0, spring), Ok(None));
    }

    #[test]
    fn overlap_is_symmetric_and_buffer_is_half_open() {
        let a = Interval {
            start: 0,
            end: 1800,
        };
        let b = Interval {
            start: 1800,
            end: 3600,
        };
        assert!(!overlaps(a, b, 0));
        assert!(overlaps(a, b, 900));
        assert!(overlaps(b, a, 900));
        assert!(overlaps(
            a,
            Interval {
                start: 2699,
                end: 4499
            },
            900
        ));
        assert!(!overlaps(
            a,
            Interval {
                start: 2700,
                end: 4500
            },
            900
        ));
        assert!(overlaps(
            a,
            Interval {
                start: 600,
                end: 1200
            },
            0
        ));
    }

    #[test]
    fn slots_limit_fails_closed_when_the_binding_is_unavailable() {
        assert_eq!(slots_limit_rejection(Some(true)), None);
        assert_eq!(
            slots_limit_rejection(Some(false)),
            Some((429, "rate_limited"))
        );
        assert_eq!(
            slots_limit_rejection(None),
            Some((503, "rate_limit_unavailable"))
        );
    }

    #[test]
    fn wrangler_configs_declare_the_slots_limit_binding() {
        let needle = format!("\"name\": \"{SLOTS_LIMIT_BINDING}\"");
        for (path, config) in [
            ("wrangler.jsonc", include_str!("../../../wrangler.jsonc")),
            (
                "deploy/cloudflare/wrangler.site.example.jsonc",
                include_str!("../../../deploy/cloudflare/wrangler.site.example.jsonc"),
            ),
        ] {
            assert!(
                config.contains("\"ratelimits\"") && config.contains(&needle),
                "{path} must declare the {SLOTS_LIMIT_BINDING} ratelimits binding"
            );
        }
    }
}
