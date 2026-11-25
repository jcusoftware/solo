# Events Calendar (`scheduling.events`)

The public events calendar runs in the published-site Worker (`sws-worker-site`) and the pack-crate renderer (`packs/hospitality`). It does not add a visual-builder block or change the module map. Native call booking and Zoom (wave B, issue #44) are a separate feature layered on the same `scheduling` section. Private event inquiries remain a form-based flow (`/api/inquire`).

## Enabling the events page

Set `scheduling.events.enabled` to `true` in the venue manifest and rebuild the assets (`npm run render`, or `solo build <manifest> <outdir>`). This adds `/events`, the navigation and sitemap entries, and the `events-config.json` marker asset that enables `GET /api/events` and `GET /api/events/:id/ics`. Rebuild to remove those assets when disabling the feature — sites without the marker stay fully inert (the API routes answer 404 and nothing else changes).

`integrations`-style settings live in `scheduling.events`:

- `source` — `d1_or_cms` (default) or `google_calendar_sync`.
- `google_calendar_id` — declarative only (required in the manifest for `google_calendar_sync`); the Worker reads the real calendar from its `GOOGLE_SYNC_CALENDAR_ID` secret at sync time. Calendar IDs and credentials are never included in the rendered assets.
- `timezone` — IANA zone (e.g. `America/New_York`) used to format times on the page; the API and calendar downloads always use UTC.

Times on the page are formatted in that time zone by `js/events-client.js`, which loads semantic event entries and `Event` JSON-LD from the API; without JavaScript the page links to the JSON listing.

## D1 storage

Apply `npm run db:migrate:local` before testing locally. `crates/sws-worker-site/migrations/0002_public_events.sql` creates the `events` table (with the legacy `starts_at`/`ends_at`/`is_ticketed` columns preserved for the ported SQL contract plus the public-event columns `external_uid`, `location`, `start_utc`, `end_utc`, `is_free`, `updated_at`), and `0003_event_sync.sql` creates the one-row `event_sync_state` snapshot marker. All queries use bound parameters.

Populate D1 with public data only. The listing returns up to 50 upcoming free, non-ticketed events; the ICS endpoint applies the same visibility rules. Provide `slug`, `title`, and `starts_at` (UTC timestamps in `YYYY-MM-DDTHH:MM:SSZ` format). Note that manual rows are only listed while a fresh gcal snapshot condition does not hide them — manual rows (`external_uid` `NULL`) are always visible when the marker is enabled.

## Optional Google Calendar import

1. Enable the Google Calendar API. Have a Workspace super admin grant the service account's numeric OAuth client ID domain-wide delegation for `https://www.googleapis.com/auth/calendar.events.readonly` only.
2. Give the delegated Workspace user mailbox read access to a dedicated calendar for free public events. Mark eligible events explicitly **Public**. The importer excludes default, private, confidential, all-day, and cancelled events.
3. Set `scheduling.events.source` to `google_calendar_sync`, set `google_calendar_id`, rebuild the assets, and apply the D1 migrations. In the intended Worker environment, provision `GOOGLE_SA_EMAIL`, `GOOGLE_SA_PRIVATE_KEY` (raw multiline PKCS#8 PEM), `GOOGLE_SYNC_SUBJECT` (the Workspace user mailbox), and `GOOGLE_SYNC_CALENDAR_ID` (the dedicated calendar) as secrets only after approval — `wrangler secret put` in production, `.dev.vars` locally. Never commit or print them.
4. Only after setup and approval, uncomment `triggers.crons` (`*/30 * * * *`) in `wrangler.jsonc`. The repository does not enable it by default.

The importer fetches a complete 180-day snapshot (at most ten pages) before atomically updating and pruning its own rows in one transactional D1 batch. Use a separate D1 database per published site; reserve `gcal:` external IDs and `gcal-` slugs for the importer. Failed or incomplete runs retain the stored snapshot and report a sanitized scheduled failure. Monitor sync errors and `event_sync_state.last_schedule_ms`.

Both public endpoints hide imported rows when sync is disabled, when the current calendar (from `GOOGLE_SYNC_CALENDAR_ID`) does not match a row's import source, or when the last committed snapshot is older than 45 minutes (30-minute schedule + 15-minute grace). An imported event's opaque ICS UID stays stable across deletion and reimport from the same calendar. Visibility changes and deletions are not reflected immediately; for urgent removal, disable `scheduling.events.enabled` and rebuild, or have an authorized operator remove the affected row.
