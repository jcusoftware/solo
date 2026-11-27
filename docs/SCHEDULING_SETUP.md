# Call booking (`scheduling.bookings`)

Native call booking runs in the published-site Worker (`sws-worker-site`) and the pack-crate renderer (`packs/hospitality`) on top of the shared `scheduling` section. It does not add a visual-builder block or change the module map. The public events calendar (`scheduling.events`) is documented in `EVENTS_SETUP.md`; booking is independent of it but shares its `timezone`. Private event inquiries remain a form-based flow (`/api/inquire`).

## Enabling the booking page

Set `scheduling.bookings.enabled` to `true` in the venue manifest and rebuild the assets (`npm run render`, or `solo build <manifest> <outdir>`). This adds `/book-a-call`, the navigation and sitemap entries, the `booking-config.json` marker asset that enables `GET /api/bookings/slots` and `POST /api/bookings`, and `js/booking-client.js`. Set the flag to `false` and rebuild to remove all of them — sites without the marker stay fully inert (the API routes answer 404 and nothing else changes).

`integrations`-style settings live in `scheduling.bookings`:

- `google_workspace_calendar_id` — declarative only; the Worker reads the real calendar from its `GOOGLE_BOOKING_CALENDAR_ID` secret at request time. Calendar IDs and credentials are never included in the rendered assets.
- `slot_duration_minutes` — 5–240.
- `buffer_minutes` — 0–1440; both sides of a call get the larger of the two neighbors' buffers.
- `working_hours` — non-empty set of `mon|tue|wed|thu|fri|sat|sun` keys with ordered same-day `HH:MM` pairs in the venue's IANA zone.
- `auto_zoom` — when `true`, confirmed calls also create a Zoom meeting.

Bookings require `scheduling.events.timezone` (the venue's IANA zone, e.g. `America/New_York`) even when public events stay disabled, and `integrations.turnstile.site_key` when enabled. Slots use venue wall time, skip nonexistent and clock-change-crossing intervals, and can return two distinct UTC times for a repeated local time. Both endpoints enforce 60 venue-local calendar days including today. Apply the D1 migrations (`npm run db:migrate:local`) before testing.

The page uses the native date input and keyboard-selectable time choices, with a phone fallback when JavaScript is unavailable (`noscript`). Turnstile verification is mandatory for submission, so with JavaScript disabled a submission fails closed — the form itself stays hidden and the notice directs guests to the phone number.

## API contract

- `GET /api/bookings/slots?date=YYYY-MM-DD` returns `slots` with UTC `start_utc` and `end_utc` fields. A valid closed date returns an empty list. The endpoint is rate-limited by the `BOOKING_SLOTS_LIMIT` Workers rate-limit binding and answers `429 rate_limited` with `Retry-After: 60` when exhausted. If the binding is missing or the limiter call fails, it fails closed with `503 rate_limit_unavailable` instead of serving unlimited requests.
- `POST /api/bookings` accepts JSON or URL-encoded `guest_name`, `guest_email`, optional `guest_notes`, the exact `start_utc` returned by the slots API, and `cf-turnstile-response`. A `201` confirms the booked time and that Calendar accepted the invitation request; it does not guarantee email delivery. A `409` means the slot is no longer available. A `503` with `booking_pending_review` supplies a reconciliation reference; **do not retry**.

## Provisioning

Before enabling public booking, verify the `BOOKING_SLOTS_LIMIT` Workers rate-limit binding is configured for the deployed environment (it allows 20 GET slot requests per tenant/client IP per minute at each Cloudflare location). `wrangler.jsonc` and the `deploy/cloudflare/wrangler.site.example.jsonc` template declare it under `ratelimits`; its `namespace_id` must be unique per Cloudflare account. It is not a global quota: for a public custom domain, also configure an edge rate-limit rule on `/api/bookings/slots` and prevent direct origin bypass. The booking POST always checks live provider availability independently.

Authorize the service account for `https://www.googleapis.com/auth/calendar.events` and `https://www.googleapis.com/auth/calendar.freebusy` (separate from the events importer's `readonly` scope; the importer never requests these). The delegated subject and calendar ID must be the same valid Workspace user mailbox (its primary Calendar), not a group, alias without a mailbox, or resource. Configure `GOOGLE_BOOKING_CALENDAR_ID` to match `google_workspace_calendar_id` from the manifest. Turnstile is required. Google Calendar sends the invitation with `sendUpdates=all`; there is no separate Resend notification.

If `auto_zoom` is enabled, activate a Zoom Server-to-Server OAuth app with `meeting:write:meeting:admin` (legacy `meeting:write:admin`) for Create Meeting on the authorized host. Set `ZOOM_HOST_USER_ID` to that host's opaque Zoom user ID (not an email or `me`). Provision the following only with explicit deployment approval, in addition to the Google service-account and Turnstile secrets:

```sh
npx wrangler secret put GOOGLE_BOOKING_CALENDAR_ID
npx wrangler secret put ZOOM_ACCOUNT_ID
npx wrangler secret put ZOOM_CLIENT_ID
npx wrangler secret put ZOOM_CLIENT_SECRET
npx wrangler secret put ZOOM_HOST_USER_ID
```

Never commit or print secrets; keep them in `.dev.vars` locally or `wrangler secret put` in production. Zoom and Google integrations are inert without their credentials — the endpoints answer `503 booking_unavailable` fail-closed rather than exposing partial flows.

## Reconcile a pending or uncertain claim

The Worker checks free/busy and atomically claims the buffered slot in D1 before creating a meeting or event. D1 protects against concurrent requests through this Worker; outside calendar edits are not atomic with this claim. Use a dedicated booking mailbox and avoid manual inserts into its bookable hours. Do not expire pending or uncertain rows automatically. Review those rows once the request is no longer active, using `gcal_event_id` (the expected Calendar ID and reconciliation reference) and `zoom_meeting_id` if recorded. If Zoom's response was lost, search the host's meetings for `Venue call [<reference>]` at the recorded time. Inspect provider state before retrying any write.

- If the correct Calendar event and any required Zoom meeting exist, verify the attendee and time, retain the provider IDs, then mark the claim confirmed.
- If only one exists, either complete the booking deliberately after checking for conflicts and duplicate invites, or clean up the provider artifacts.
- Only mark the claim cancelled once you have verified no upstream booking remains. If you cannot determine provider state, leave the claim blocking.

Handle guest details in a private operator workflow. Never log OAuth tokens, meeting URLs, standalone passwords, or guest details.

## Local browser checks without provider credentials

```sh
npm run test:a11y -- --grep booking
```

The scheduling specs render the enabled demo pages against the local dev server (the CI `web` job starts one first) and intercept every external request. They exercise the UI, the site's CSP, axe, and a local stub using Cloudflare's always-pass test site key; they do not contact Turnstile, Google, or Zoom. After approved provisioning, separately verify real Turnstile, free/busy, invitation delivery, and optional Zoom meeting creation with an authorized test booking.
