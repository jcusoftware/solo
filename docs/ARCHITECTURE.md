# Solo Website System (SWS) Architecture

**Solo Website System (SWS)** is a Cloudflare-native, Rust-powered open-source visual website builder (`MIT OR Apache-2.0`). SWS compiles a declarative, industry-agnostic intermediate representation (`sws-ir`) into deterministic, WCAG 2.2 AA static websites deployed on **Cloudflare Workers with Static Assets**, backed by **D1**, **Turnstile**, and optional control-plane services.

Domain verticals are isolated in pluggable packs (`packs/*`). The first open-source reference vertical pack (`MIT OR Apache-2.0`) is **`packs/hospitality`**, which provides hospitality modules (menus, hours, Toast/OpenTable/Resy integrations, advisory Virginia ABC copy linting, and the fictional `shirlington-hearth-demo` showcase fixture).

---

## 1. Cloudflare-native three-plane architecture

```
====================================================================================================
 PLANE 1: BUILD / AUTHORING PLANE (Browser Wasm + Local CLI / MCP)
====================================================================================================
  Planned (v0.3.0): thin TypeScript Builder shell + script-free sandboxed canvas
       │  edits go through an opaque Rust/Wasm session (sws-wasm); no canonical IR in JS
       ▼
  Core Rust Engine (crates/, MIT OR Apache-2.0)
    ├── sws-ir        : Declarative page, component, token & form intermediate representation
    ├── sws-css       : Pure-Rust design-token & layout CSS emitter (zero GPL/MPL dependencies)
    ├── sws-compiler  : Deterministic HTML, JSON-LD, sitemap & asset compiler
    └── sws-plugin    : Pack ABI & extension hooks (loads packs/hospitality, etc.)
       ▲
       │  local authoring, CI builds & AI-agent automation
  `solo` CLI & MCP Server (sws-cli)

====================================================================================================
 PLANE 2: CONTROL PLANE (planned v0.2.0 Studio — optional, Cloudflare Workers)
====================================================================================================
  `sws-worker-studio` (Rust → wasm32; scaffold present: D1 registry + request bounds + /api/health; DOs, R2, Queues and auth are later issues)
    ├── SQLite Durable Objects (hibernating, one command session per site)
    │     └── per-site snapshot + monotonic command log + expiring soft presence
    │         (command log, not CRDT; see §10)
    ├── D1 (SQLite)         : Workspace/site/user, token, asset & deployment metadata
    ├── Asset storage       : size-capped D1 blobs by default (cardless); R2 is opt-in
    └── Cloudflare Queues   : Async publish jobs (sub-1 KB pointer to an immutable revision)
                              — only if the #59 spike proves Free-plan CPU headroom;
                              otherwise the CLI compiles and uploads (#68)

====================================================================================================
 PLANE 3: EDGE PUBLISHED-SITE PLANE (Cloudflare Free-Tier Edge)
====================================================================================================
  Visitor Browser
       │  HTML + CSS + lean progressive-enhancement JS islands (forms.js)
       ▼
  Cloudflare Workers Static Assets (_headers → static security headers)
    ├── path matches static asset & not /api/*
    │     └── index.html, styles.css, js/forms.js, robots.txt, sitemap.xml, 404.html
    └── run_worker_first: ["/api/*"]
          ▼
     `sws-worker-site` (Rust → wasm32, workers-rs; `crates/sws-worker-site`)
       ├── with_security_headers(...)   ◄── byte-identical header set as _headers
       ├── GET  /api/health             → {status, service, version}
       ├── POST /api/inquire            → Turnstile siteverify → D1 form_submissions → Resend (opt.)
       ├── POST /api/newsletter         → Turnstile siteverify → D1 newsletter_subscribers
       ├── GET  /api/bookings/slots     → D1 claims ∩ Google Calendar free/busy (config-gated)
       ├── POST /api/bookings           → Turnstile siteverify → D1 `bookings` claim → Calendar
       │                                  invitation (+ optional Zoom meeting; issue #44)
       └── POST /api/checkout           → sandbox-only hosted Stripe Checkout handoff (303; no storage)
             │                              │
             ▼                              ▼
        D1 (SQLite) binding DB       challenges.cloudflare.com / www.googleapis.com /
                                     api.zoom.us / api.resend.com / api.stripe.com
```

- **Deterministic static rendering.** Published pages are pre-rendered static HTML and CSS compiled from `sws-ir`, yielding zero runtime rendering cost and top Lighthouse performance. Edge Workers execute only on `/api/*`.
- **Progressive enhancement.** Client JS islands (`forms.js`) enhance form submission with `fetch` and `aria-live` status regions. Published pages render and navigate with zero JS; public form submission is the documented exception — the mandatory Turnstile verification (§8 of AGENTS.md) requires JavaScript, so a no-JS submission fails closed with `400 turnstile_required`.
- **Memory and size discipline.** Request bodies are capped at 16 KB, nothing is cached unboundedly in isolate memory, and no native C/OpenSSL or threaded crates are used—staying well within Cloudflare Workers' 128 MB isolate limit.

---

## 2. Target monorepo layout & multi-track release model

SWS separates its MIT OR Apache-2.0 core engine from frontend builder surfaces, TypeScript SDKs, and pluggable vertical packs:

```
crates/                       Open-source Rust workspace (MIT OR Apache-2.0)
  sws-ir/                     Core IR types, schema validation, token & route graph
  sws-css/                    Pure-Rust CSS compiler
  sws-compiler/               Deterministic static HTML/JSON-LD/asset emitter
  sws-plugin/                 Pack trait & plugin registry ABI
  sws-wasm/                   wasm-bindgen target for browser visual canvas
  sws-cli/                    `solo` CLI + MCP server
  sws-worker-site/            Published-site edge Worker (/api/*, Turnstile, D1, security headers)
  sws-worker-studio/          optional studio control-plane Worker — scaffold only (D1 registry, request bounds; Durable Objects, Queues and R2 opt-in/conditional, later issues)

apps/                         planned v0.3.0 — thin TypeScript Builder shell and canvas (MIT OR Apache-2.0); directory names are an open assumption
  builder/                    Editor shell (inspector, tree, diagnostics, save/publish/export dialogs)
  canvas/                     Sandboxed, script-free canvas fed by `sws-wasm` deltas

packages/                     published JS/TS packages (`@solo-website-system/*`, MIT OR Apache-2.0)
  @solo-website-system/ir-types/  planned v0.2.0-alpha.1 (#56) — generated `.d.ts` + Draft 2020-12 JSON Schema, no JS runtime

packs/                        Industry verticals & domain plugins
  hospitality/                Open-source reference hospitality pack (`pack-hospitality`, MIT OR Apache-2.0)

integrations/                 Standalone integrations (per-package licensing)
  devvit-release-bot/         Devvit subreddit release bot (BSD-3-Clause)

core/                         Tenant-agnostic rendered demo output & CLI tools (the Rust engine lives in `crates/`)
  public/                     Deterministic rendered static output + _headers
  tools/                      CLI utilities (header parity check)

D1 migrations live at `crates/sws-worker-site/migrations/` (`0001_init.sql`, `0002_public_events.sql`, `0003_event_sync.sql`, `0004_scheduling_bookings.sql`). The Studio registry has its own migration directory, `crates/sws-worker-studio/migrations/` (`0001_registry.sql`), applied to a separate `STUDIO_DB` database via `wrangler.studio.jsonc`.
```

Entries above marked *planned* are targets, not present directories: `packages/@solo-website-system/ir-types` arrives in v0.2.0-alpha.1 (#56), and `apps/` (builder, canvas) in v0.3.0. Today's repository contains `crates/`, `packs/`, `integrations/`, `core/` (output + tools only), `sites/`, `docs/` and `tests/`.

### Multi-track release model

| Track | Artifact / Registry | Cadence & Tooling | License |
| --- | --- | --- | --- |
| Core releases (`v*` tags) | GitHub Releases (`solo` CLI binaries when present) + optional Cloudflare static-site deploys (`sws-landing` at `solo.jcusoftware.com` and `sws-docs` at `docs.jcusoftware.com` when `CLOUDFLARE_API_TOKEN` is configured) | Tag-only GitHub Actions (`.github/workflows/release.yml`, triggered strictly on pushed `v*` tags; merges to `main` run CI only and never trigger releases or deploys) | `MIT OR Apache-2.0` |
| Vertical packs (`pack-hospitality-v*` tags) | `pack-hospitality` (open-source vertical pack bundle & schemas) | Independent tag-only GitHub Actions track (`.github/workflows/release.yml` on `pack-hospitality-v*` tags) | `MIT OR Apache-2.0` |

Release tags are created only from the verified final squash commit on `main` (see §10 and CONTRIBUTING §7). Versioned npm packages (planned, #82) ship alongside signed-tag CLI artifacts and checksums; the hospitality pack tag workflow stays separate.

---

## 3. Core edge data model (D1)

The optional [Stripe pickup demo](STRIPE_DEMO.md) is separate from D1: its static `/takeout` page posts to `/api/checkout`, which requires Turnstile and can only redirect to a verified sandbox Stripe Checkout Session. It records no orders, handles no webhooks, and does not fulfill anything.

The published-site edge Worker (`sws-worker-site`, using `crates/sws-worker-site/migrations/0001_init.sql`) uses bound SQLite parameters in Cloudflare D1:

- **`form_submissions` (Phase 0/1 table: `inquiries`).** Stores structured form payloads (`kind` such as `contact` or pack-defined kinds like `private_event`), contact fields, `message`, `source_route`, `status` (`new` | `responded` | `closed` | `spam`), and `created_at`.
- **`newsletter_subscribers`.** Stores `email` (unique, `NOCASE`), `status` (`subscribed` | `unsubscribed` | `bounced`), `consent_timestamp` (ISO-8601 UTC stamped by the Worker), `source_route`, `consent_text`, `created_at`, `updated_at`, and `unsubscribed_at`.
  - Public web forms never flip `unsubscribed` or `bounced` rows back to `subscribed` (re-subscription requires explicit double opt-in).
- **Pack-extended tables (e.g., `events` and `bookings` in `packs/hospitality`).** Optional schema tables are enabled when the corresponding pack feature is active: `events` (`slug`, `title`, `description`, legacy `starts_at`/`ends_at`/`is_ticketed`, public-event columns `external_uid`, `location`, `start_utc`, `end_utc`, `is_free`, `updated_at`) in `migrations/0002_public_events.sql`, the one-row `event_sync_state` snapshot marker (with the reserved `identity_fingerprint` column carried over from the old `0004_event_snapshot_identity.sql`) in `migrations/0003_event_sync.sql`, and the call-booking claims table `bookings` (with pending/uncertain/confirmed/cancelled status and half-open overlap-guard triggers) in `migrations/0004_scheduling_bookings.sql`. All queries bind parameters.

---

## 4. Reference vertical pack: `packs/hospitality`

All hospitality-specific schemas, UI templates, third-party restaurant integrations, and compliance linters live in `packs/hospitality` rather than SWS core. Its showcase fixture is the 100% fictional **`shirlington-hearth-demo`** (`packs/hospitality/demo/venue.json`).

### Hospitality module map (M1–M11)

| Module | Name | Routes | Data | Default flag | Status |
| --- | --- | --- | --- | --- | --- |
| M1 | Core Shell & NAP | `/` (header, footer, NAP, hours), `LocalBusiness` JSON-LD | `venue.json` | on (required) | Phase 0 ✅ |
| M2 | Semantic Menus | `/menu`, `/happy-hour` | `venue.json` `menu`, `happy_hour` | on (required) | Phase 0 on `/`; Phase 1 routes |
| M3 | Private Events Inquiry | `/book-an-event` → `POST /api/inquire` (`kind=private_event`) | D1 `inquiries` / `form_submissions` | on | API ✅, page Phase 1 |
| M4 | Contact | `/contact` → `POST /api/inquire` (`kind=contact`) | D1 `inquiries` / `form_submissions` | on | API ✅, page Phase 1 |
| M5 | Newsletter + CAN-SPAM Consent | Footer form → `POST /api/newsletter` | D1 `newsletter_subscribers` | on | Phase 0 ✅ |
| M6 | Events Calendar | `/events`, `Event` JSON-LD | `scheduling.events`, D1 `events` + `event_sync_state` | on in demo (`d1_or_cms`) | API + render ✅ (port #43) |
| M7 | Call Booking | `/book-a-call`, `GET /api/bookings/slots`, `POST /api/bookings` | `scheduling.bookings`, D1 `bookings`, Google Calendar free/busy + invitations, optional Zoom | off (on in demo) | API + render ✅ (port #44); Zoom/Google require setup |
| M8 | Visit | `/visit` (hours, holiday overrides, directions, venue accessibility) | `venue.json` | on | Phase 1 |
| M9 | About / Story / Gallery | `/about` | `venue.json` + optimized images | off in demo until Phase 1 | Phase 1 |
| M10 | SEO & Discovery | `sitemap.xml`, `robots.txt`, NAP drift audit | `venue.json` | on (required) | Phase 0 ✅ |
| M11 | Security, a11y & Observability Gate | `_headers` + `with_security_headers`, `/api/health`, CI gates | — | on (required) | Phase 0 ✅ |

The Phase 1 hospitality showcase exposes 11 routes: `/`, `/menu`, `/happy-hour`, `/events`, `/book-a-call`, `/book-an-event`, `/visit`, `/about`, `/contact`, `/accessibility`, `/privacy`. `/events` and `/book-a-call` are independently opt-in through their manifest flags.

### Hospitality pluggable integrations (`venue.json` `integrations`)

| Key | Values | Default | Notes |
| --- | --- | --- | --- |
| `toast` | `{enabled, restaurant_guid}` | `enabled: false` | Online ordering is link-out only; no Toast API keys in Phase 0/1 |
| `seating_policy` | `"walk_in_only"` \| `"opentable"` \| `"resy"` | `"walk_in_only"` | Controls the reservation CTA. A plain link is preferred over an embed (CSP and a11y) |
| `opentable` / `resy` | `{restaurant_id}` / `{venue_id}` | `null` | Used only when matching `seating_policy` |
| `inkind` | `{enabled}` | `false` | House-account / inKind promo link, off by default |
| `turnstile` | `{site_key, test_key}` | test key | Production Turnstile key per tenant |

### Public events calendar (M6 — `scheduling.events`)

Venues with `scheduling.events.enabled: true` in `venue.json` render a deterministic `/events` page (semantic list hydrated by `/js/events-client.js`, `noscript` link to the JSON API, per-event `.ics` downloads) and emit `events-config.json` plus `js/events-client.js` as build assets. The Worker routes `GET /api/events` and `GET /api/events/:id/ics` stay inert (404) unless the enabled marker asset is present, so zero-backend sites are unaffected. An optional Google Calendar import (`source: "google_calendar_sync"`) runs only on a Cron trigger and only with the `GOOGLE_SA_EMAIL`, `GOOGLE_SA_PRIVATE_KEY`, `GOOGLE_SYNC_SUBJECT`, and `GOOGLE_SYNC_CALENDAR_ID` secrets provisioned in the Worker environment; it atomically replaces the importer's own `gcal:` rows and is fully inert without configuration. Setup details: `docs/EVENTS_SETUP.md`.

### Call booking (M7 — `scheduling.bookings`)

Venues with `scheduling.bookings.enabled: true` render a deterministic `/book-a-call` page (native date input, keyboard-selectable time slots hydrated by `/js/booking-client.js`, hidden form with a `noscript` phone fallback because Turnstile verification is mandatory) and emit `booking-config.json` plus `js/booking-client.js`. The Worker routes `GET /api/bookings/slots` (rate-limited by the `BOOKING_SLOTS_LIMIT` binding) and `POST /api/bookings` stay inert (404) without the marker. Slots derive from `scheduling.bookings.working_hours` in `scheduling.events.timezone`; the free-slot listing intersects D1 claims (`bookings` table, atomically overlap-guarded by triggers) with Google Calendar free/busy, and the POST claims the buffered slot in D1 before writing the Calendar invitation and — when `auto_zoom` is on and the `ZOOM_*` secrets are provisioned — a Zoom meeting. Everything is config-gated: without the enabled marker or the provisioning secrets the endpoints answer 404/503 fail-closed. Setup, the API contract, and the pending/uncertain reconciliation runbook: `docs/SCHEDULING_SETUP.md`.

### Advisory Virginia ABC copy lint (`packs/hospitality`)

`packs/hospitality` includes an advisory copy linter (implemented in Node and Rust) covering 3 VAC 5-20-10 to -100 and 3 VAC 5-50-160. It flags phrases such as `"bottomless"`, `"2-for-1"` / `"2 for 1"` / `"2for1"`, `"two for one"` / `"two-for-one"`, `"unlimited drinks"`, and happy-hour start or end times from `21:00` through `05:59` (the overnight window, midnight wrap included). It is advisory only (not legal advice) and requires human review on drink-related copy.

---

## 5. Dual security headers

Cloudflare `_headers` (`core/public/_headers`) applies to **static assets only**. Every edge Worker response from `sws-worker-site` (including 4xx and 5xx errors on `/api/*`) must emit the byte-identical header set from `with_security_headers` (`crates/sws-security/src/headers.rs::with_security_headers`).

The shared header set enforces:
- `Strict-Transport-Security`: 2 years, `includeSubDomains`, `preload`
- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: DENY`
- `Referrer-Policy: strict-origin-when-cross-origin`
- Restrictive `Permissions-Policy`
- `Cross-Origin-Opener-Policy: same-origin`
- Strict `Content-Security-Policy` with `default-src 'self'`, Cloudflare Turnstile allowlisted on `https://challenges.cloudflare.com` (`script-src`, `frame-src`, `connect-src`), `frame-ancestors 'none'`, `object-src 'none'`, and `upgrade-insecure-requests`

Header parity is enforced by:
1. `cargo test` (parses `_headers` and asserts exact parity with `with_security_headers`)
2. `npm run check:headers` (static)
3. `npm run check:headers:live` (live `/` vs `/api/health`)
4. `tests/headers.spec.mjs`

See the `cloudflare-rust-headers` skill.

---

## 6. Gate 1 — Rust/Wasm fallback criteria

**Ceiling:** 15 cumulative hours of *toolchain or runtime* debugging in Phase 0/1. That covers `worker-build` and `wasm-bindgen` breakage, `workers-rs` API gaps, D1 binding issues, wasm size or startup limits, and Rust-only CI failures. Feature work and normal bugs do not count.

**Triggers:**
- the ceiling is exceeded
- a blocking `workers-rs` limitation has no workaround within 2 hours
- the wasm bundle can't stay inside the Workers free-plan size limit
- cold-start or CPU limits are exceeded on the free plan

**Fallback target:** Astro (static output) for pages, and TypeScript + Hono on Workers for `/api/*`.
- **What carries over unchanged:** the IR/pack schema contracts and JSON Schema, the D1 migrations, `_headers`, the Playwright, axe and Lighthouse tests, pack linters (Node), the header parity check, and CI.
- **What gets rewritten:** `crates/sws-security` and `crates/sws-worker-site` become TypeScript/Hono modules under `core/api/src/*.ts`, with the same routes, JSON bodies, validation rules and middleware (`app.use('*', securityHeaders)`).
- **Layout:**
  - `core/web/` holds Astro, reading tenant/site configuration.
  - `core/api/` holds Hono.
  - `wrangler.jsonc` keeps the same `assets` and `run_worker_first`.

**Decision rule:** when a trigger fires, stop, log it below, and get explicit sign-off from the user before switching.

### Gate 1 log
| Date | Hours | Cumulative | Issue | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-24 | 0 | 0 | Phase 0 scaffold: `cargo test`, `cargo check --target wasm32`, `worker-build` and `wrangler dev` all working first time | — |
| 2026-11-27 | 0 | 0 | #61 Studio Worker scaffold: wasm32 clippy, `worker-build --release`, local D1 migration and `wrangler dev` all worked first time; no toolchain debugging | — |

---

## 7. Two-zone IP & trademark firewall

- **Zone A, public or publishable:**
  - Open-source SWS core (`crates/`, `apps/`, `packages/`, `core/`, `sites/`), licensed under `MIT OR Apache-2.0`
  - Open-source reference pack `packs/hospitality/` (`MIT OR Apache-2.0`), including its fictional showcase fixture `packs/hospitality/demo/venue.json`
  - Fixtures, tests, docs, and screenshots or portfolio material
- **Zone B, private:** `private/` (gitignored), real client tenants (kept in a private repo or under NDA), prospect dossiers, client exports such as zone files and subscriber CSVs, and credentials (`*.env`, `.dev.vars`).

**Rules:**
1. Zone A never contains real third-party venue names, logos, photos, menus, colors copied from a real brand, addresses or phone numbers. Never include real client or prospect trademarks in demos, fixtures, tests, docs, screenshots or commit messages.
2. Demo data uses fictional conventions: invented street names, the `555-01xx` phone range, and `.example` domains.
3. Material never flows from Zone B into Zone A. A pattern learned from a client engagement may be generalized into SWS core or a pack only after removing every client-identifying detail.
4. SWS core (`crates/`, `core/`) must stay tenant-agnostic and industry-agnostic: tools take paths or IR inputs, there are no hard-coded tenant names, and rendered output (`core/public`) is build output only.
5. Pre-commit check: keep any real-client denylist regex in a gitignored local pre-commit hook or `private/` so Zone A never stores real business names.

---

## 8. Licensing plan for SWS core

- **Core licenses:** Dual **`MIT OR Apache-2.0`** across `crates/*`, `apps/*`, `packages/*`, `core/`, `sites/*`, and `packs/hospitality/`.
- **Integrations:** Tracked integration packages under `integrations/` carry their own license; `integrations/devvit-release-bot` is **`BSD-3-Clause`** (permissive, never a Rust crate dependency, so the `cargo deny` gate is unaffected).
- **Dependency gate:** `cargo deny check` (`deny.toml`) allows only `MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-DFS-2016`, and `CC0-1.0`, plus narrowly scoped per-crate `Unicode-3.0` exceptions for ICU/`unicode-ident` crates. Copyleft (GPL/AGPL/LGPL/MPL) and source-available (SSPL/BUSL) licenses are rejected. (Notably, `sws-css` is pure Rust without `lightningcss` or `cssparser`.)
- **External vertical packs:** Third-party or external industry packs are decoupled behind `sws-plugin` and may be distributed on independent release tracks under their own licenses.

---

## 9. Compliance summary

- **WCAG 2.2 AA (SWS Core):**
  - Native semantic HTML, exactly one `<h1>` per page, sequential headings with no skipped levels, landmark regions (`header`, `nav`, `main`, `footer`), skip links, `lang` attribute.
  - Contrast >= 4.5:1 for normal text (>= 3:1 for large text/UI), visible `:focus-visible` outlines, targets >= 24x24 CSS px, `<label>` + `aria-describedby` + `aria-live` status regions on progressive-enhancement forms.
  - Zero accessibility overlay widgets (no accessiBe, UserWay, AudioEye, etc.).
  - Automated gates: `npm run test:a11y` (axe `wcag22aa`) and `npm run lhci` (Lighthouse accessibility = `1.0`).
- **CAN-SPAM (SWS Core `/api/newsletter`):**
  - Explicit unchecked consent checkbox, server-stamped `consent_timestamp` and `source_route` in D1, unsubscribes/bounces never silently re-subscribed, and physical mailing address in outbound emails.
- **Semantic Menus & Virginia ABC (`packs/hospitality`):**
  - Menus render as semantic HTML plus `Menu`/`MenuSection`/`MenuItem` JSON-LD (never PDF-only).
  - Advisory VA ABC linter (Node + Rust) flags prohibited drink promo language and happy-hour start or end times inside the 21:00–05:59 overnight window per 3 VAC 5-20 and 3 VAC 5-50-160; human sign-off is required before client publication (not legal advice).

---

## 10. Studio and Builder design alignment (v0.2 onward)

This section records the decisions stated by the Studio and Builder release issues (#54 to #88). The design document those issues cite is not in this repository. Where an issue does not decide something, it is labelled **assumption** or **open question** below and in `ROADMAP.md` ("Open assumptions and questions"). Nothing here weakens §5 (dual security headers), §6 (Gate 1) or §7 (two-zone firewall).

### Rust core, thin TypeScript shell

- Canonical IR, the command reducer, undo/redo, validation, contrast diagnostics and compilation stay in Rust (`sws-ir`, `sws-compiler`, `sws-css`). One versioned command batch with an atomic Rust apply path is shared by the CLI, MCP, browser and Studio (#55), keyed by stable node IDs (#54).
- The browser parses IR once into an opaque Rust-owned Wasm session and never mirrors canonical IR or validation in JavaScript (#70). The TypeScript layer is limited to generated types and schemas (#56), the `StudioEngine` wrapper (#70), client transport (#75) and the editor shell (#77, Preact and Signals). The earlier "pure-Rust Studio frontend" wording is retired.
- The canvas is script-free: compiler-produced markup in an opaque sandboxed iframe with a validated `MessageChannel` bridge, and no user scripts or third-party widgets (#72).
- Pack frontends stay out of Rust and hospitality rules stay out of the generic UI (#74).

### Command log instead of CRDT

Each site's Durable Object session keeps a snapshot, a monotonic command log and expiring soft presence, and checkpoints every 50 commands or on save/publish (#62). Stale batches are rebased only for explicitly tested independent field updates. Stale structural commands and incompatible undo are rejected with actionable conflicts. Collaborative undo is a new inverse command, and operation IDs are idempotent. The former `ProjectRoom` CRDT/patch wording is retired. Whether the class keeps that name is open.

### Cardless default, opt-in R2

- Default asset storage is size-capped D1 blobs when R2 is unbound. R2 is an explicit opt-in that streams uploads with concurrency-safe quota reservations and counts actual bytes (#63). Local filesystem storage stays out of Wasm.
- Authentication is self-hosted GitHub OAuth with one bootstrapped owner and explicit invitations. Cloudflare Access, a card or an auth SaaS is never a default dependency (#60, #65). Edit-only credentials cannot publish or activate production.
- Provisional cardless envelope (#63, to be tested with #69): up to 20 pages, 1 MiB per uploaded asset, 50 MiB of stored assets per site.
- Application caps and alerts reduce billing risk but are not provider-side hard stops.

### Cloud quota values are assumptions

All Cloudflare Workers, D1, Durable Object, Queue and R2 limits, quotas and prices are **assumptions until verified** against current Cloudflare documentation and evidence from a fresh Workers Free account (#59, #63, #69). This document intentionally states no authoritative platform numbers; the sub-1 KB queue pointer (#64), the 50-command checkpoint (#62) and the 20-page/1 MiB/50 MiB envelope are project targets, not platform facts.

### Publish path

- Publishing compiles against an immutable revision, off HTTP handlers, through an asynchronous job pipeline (#64). Studio uploads compiled static assets to the standalone edge Worker through Cloudflare Direct Upload Assets, preserving `run_worker_first: ["/api/*"]` and the existing D1/Turnstile bindings (#66). Visitors are served with Studio unavailable.
- Server-side compilation is the default only if #59 shows repeatable Free-plan headroom (queue wall time is not CPU time). Otherwise the documented cardless path is CLI compile, local shared Rust verification, then upload of a revision-pinned artifact manifest with a publish-scoped credential (#59, #64, #68). CLI-built artifacts are labelled as such and never presented as independently server-built.
- Draft previews are served from a separate, non-indexed origin (#67). Rollback is a new audited action on a retained verified deployment and never rewinds editable IR (#66).
- Static and Worker security-header parity (§5) must hold for published and exported sites (#69, #82).

### Saves, sync and export

- File saves are explicit. `Saved to file` appears only after a successful write, `Synced to Studio` only after the revision is acknowledged, and queued edits are neither (#75, #77). Automatic browser draft recovery is deferred until tested.
- Local IR open and edit work without a Studio account (#75). `solo export` output runs with no Studio dependency (#79). GitOps uses a verified downloadable export (#80). GitHub PR creation, `repository_dispatch` and automated production deployment are out of scope for v0.3.
- Third-party runtime pack loading is deferred (#73). Statically linked Rust packs, including `packs/hospitality`, cover launch (#74, #78).

### Phase exits

| Milestone | Phase exit |
| --- | --- |
| `v0.2.0-alpha.1` IR Command & Schema Foundation | Local native/Wasm editing slice passes before Studio: stable IDs, explicit legacy upgrade, undo/redo, lossless save/reopen, byte-identical production output, schema drift check, reproducible Wasm tooling |
| `v0.2.0-beta.1` Studio Control Plane Core | Hibernating multi-client sessions, conservative conflicts, revocation, bounded cardless and opt-in R2 storage, async publish pipeline with an evidence-justified default compile path |
| `v0.2.0` Publish Bridge, Auth & CLI Sync | Small-site publish on a fresh no-payment-method account with the CLI and a reference sync client; R2 tested separately; failure recovery, audited rollback, header parity, independent visitor traffic |
| `v0.3.0-alpha.1` Opaque Wasm Session & Isolated Canvas | Script-free sandboxed canvas, p95 input-to-preview under 16 ms in a controlled token scrub, verified focus, scroll, instrumentation and origin isolation |
| `v0.3.0-beta.1` Builder, Inspector & Pack Hooks | Accessible editing of fictional demo menus, hours and tokens with the existing Rust pack, explicit file saves, distinct sync status, live diagnostics, lossless CLI/MCP round-trips |
| `v0.3.0` Standalone Export & Release Hardening | Verified downloadable standalone export, independent Builder/canvas/export checks, Wasm and licensing budgets, signed tag-only releases from verified squash commits |
| `v0.3.1` Templates, Fonts & Media Library | Starter template exports pass the WCAG 2.2 AA gates; images resized at build time with no third-party font or image CDN |
| `v0.3.2` Pages, Navigation & SEO | Sitemap and redirects verified in CI; Lighthouse SEO score does not regress |
| `v0.3.3` Visual Form Builder | Generated forms pass the WCAG 2.2 AA gates, keep native semantics and fail closed without Turnstile |
| `v0.4.0` Import Static Sites | Two-page static fixture imports, edits, saves, reopens and exports deterministically with an inert preview |
| `v0.5.0-alpha.1` Collections & Content Model | Collections round-trip losslessly, native and Wasm output stay byte-identical, legacy files build without writes |
| `v0.5.0-beta.1` Blog, Search & Media Content | Demo gains a blog and gallery passing the a11y and header gates |
| `v0.5.0` Multilingual, Scheduling & Restore Points | Scheduled post publishes on time on a cardless account; restore returns to an earlier revision with an audit trail |
| `v0.6.0-alpha.1` Inbox, Contacts & Webhooks | Tenant-scoped, exportable, log-redacted submissions; signed, safely retried webhooks |
| `v0.6.0-beta.1` Analytics & Consent | Default site sets no cookies and ships no third-party scripts |
| `v0.6.0` Campaigns & Engagement | Campaign sends through a BYO provider and never re-subscribes an unsubscribed contact |
| `v0.7.0-alpha.1` Visitor Identity & Catalog | Visitor sign-in on a cardless account (OAuth-only default, opt-in magic link); catalog passes a11y gates |
| `v0.7.0-beta.1` Cart, Payments & Fulfilment | Test-mode order completes with webhook-verified state and atomic inventory decrement |
| `v0.7.0` Orders, Subscriptions & Growth | Refunds and subscriptions verified in Stripe test mode; store passes a11y and header gates |
| `v0.8.0-beta.1` Core Bookings | Hospitality pack runs on the core bookings API with no regressions |
| `v0.8.0` Paid Bookings & Reminders | Deposit booking completes in Stripe test mode; reminders respect consent |
| `v0.9.0-beta.1` Gated Content & Memberships | Gated content never served to unauthorised visitors |
| `v0.9.0` Community, Courses & Loyalty | Moderation, rate limits and a11y gates pass for member-generated content |
| `v0.10.0-beta.1` Builder Import Adapters | Fixture exports from WordPress, Squarespace, Shopify and Wix import deterministically with a migration report |
| `v0.10.0` URL Mapping & GitHub Sync | Old URLs keep working through generated redirects; repo sync round-trips without data loss |
| `v0.11.0-beta.1` Setup Wizard & Domains | Non-technical tester publishes to a custom domain on a fresh cardless account using only the wizard |
| `v0.11.0` Operations & Support | Backup and restore verified end to end; PWA Studio passes the a11y gates |
| `v1.0.0-rc.1` Audits & API Freeze | Audit findings resolved or documented; pack API and IR compatibility policy published |
| `v1.0.0` Initial Release | All prior milestone exits hold on the release commit |

The separate `v0.2 — Hospitality pack feature ports` GitHub milestone (#43, #44, #45) is not part of this table.

### Names and baseline

- `workers-rs` baseline: `worker` 0.9 (`Cargo.toml`, `Cargo.lock`) with `worker-build` 0.9. It is not downgraded.
- The GitHub milestone names above are canonical. `sws-worker-studio` is the Studio Worker crate (scaffolded in #61: D1 registry, request bounds, `/api/health`; configured separately via `wrangler.studio.jsonc`); `sws-worker-site` remains the visitor Worker.
- Related runbook: `docs/WASM_TOOLCHAIN.md` (#57).

### Merge and release discipline

All pull requests into `main` are squash-merged with a conventional PR title. A PR uses a closing reference (`Closes #N`) only for issues it fully completes and `Refs #N` otherwise, and it includes its verification results. Release tags are signed and created from the verified final squash commit on `main`, never from feature-branch commits. See CONTRIBUTING §6 and §7.
