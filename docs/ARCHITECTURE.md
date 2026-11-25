# Solo Website System (SWS) Architecture

**Solo Website System (SWS)** is a Cloudflare-native, Rust-powered open-source visual website builder (`MIT OR Apache-2.0`). SWS compiles a declarative, industry-agnostic intermediate representation (`sws-ir`) into deterministic, WCAG 2.2 AA static websites deployed on **Cloudflare Workers with Static Assets**, backed by **D1**, **Turnstile**, and optional control-plane services.

Domain verticals are isolated in pluggable packs (`packs/*`). The first open-source reference vertical pack (`MIT OR Apache-2.0`) is **`packs/hospitality`**, which provides hospitality modules (menus, hours, Toast/OpenTable/Resy integrations, advisory Virginia ABC copy linting, and the fictional `shirlington-hearth-demo` showcase fixture).

---

## 1. Cloudflare-native three-plane architecture

```
====================================================================================================
 PLANE 1: BUILD / AUTHORING PLANE (Browser Wasm + Local CLI / MCP)
====================================================================================================
  Visual Builder (apps/builder) + Visual Canvas (apps/canvas)
       │  real-time editing & live preview via Browser Wasm (sws-wasm)
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
 PLANE 2: CONTROL PLANE (v0.2.0 Studio Backend — Cloudflare Workers)
====================================================================================================
  `sws-worker-studio` (Rust → wasm32)
    ├── SQLite Durable Objects
    │     ├── ProjectRoom   : Authoritative collaborative project state & optimistic IR sync
    │     └── RateLimiter   : Per-tenant / per-IP token-bucket rate limiting
    ├── D1 (SQLite)         : Tenants, projects, publish history, pack entitlements
    ├── R2 Object Storage   : Media assets, immutable build snapshots & published site bundles
    └── Cloudflare Queues   : Async publish pipelines, image optimization & webhook dispatch

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
       └── POST /api/newsletter         → Turnstile siteverify → D1 newsletter_subscribers
             │                              │
             ▼                              ▼
        D1 (SQLite) binding DB       challenges.cloudflare.com / api.resend.com
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
  sws-worker-studio/          planned v0.2.0 — studio control-plane Worker (Durable Objects, D1, R2, Queues)

apps/                         planned v0.3.0 — visual authoring web apps (MIT OR Apache-2.0)
  builder/                    Studio shell (inspector, theme tokens, publish controls)
  canvas/                     Live interactive canvas backed by `sws-wasm`

packages/                     planned v0.3.0 — published JS/TS packages (`@solo-website-system/*`, MIT OR Apache-2.0)

packs/                        Industry verticals & domain plugins
  hospitality/                Open-source reference hospitality pack (`pack-hospitality`, MIT OR Apache-2.0)

integrations/                 Standalone integrations (per-package licensing)
  devvit-release-bot/         Devvit subreddit release bot (BSD-3-Clause)

core/                         Tenant-agnostic rendered demo output & CLI tools (the Rust engine lives in `crates/`)
  public/                     Deterministic rendered static output + _headers
  tools/                      CLI utilities (header parity check)

D1 migrations live at `crates/sws-worker-site/migrations/0001_init.sql`.
```

Entries above marked *v0.2.0*/*v0.3.0* are planned targets, not present directories: `sws-worker-studio/` is planned for v0.2.0, and `apps/` (builder, canvas) and `packages/` (`@solo-website-system/*`) are planned for v0.3.0. Today's repository contains `crates/`, `packs/`, `integrations/`, `core/` (output + tools only), `sites/`, `docs/` and `tests/`.

### Multi-track release model

| Track | Artifact / Registry | Cadence & Tooling | License |
| --- | --- | --- | --- |
| Core releases (`v*` tags) | GitHub Releases (`solo` CLI binaries when present) + optional Cloudflare static-site deploys (`sws-landing` at `solo.jcusoftware.com` and `sws-docs` at `docs.jcusoftware.com` when `CLOUDFLARE_API_TOKEN` is configured) | Tag-only GitHub Actions (`.github/workflows/release.yml`, triggered strictly on pushed `v*` tags; merges to `main` run CI only and never trigger releases or deploys) | `MIT OR Apache-2.0` |
| Vertical packs (`pack-hospitality-v*` tags) | `pack-hospitality` (open-source vertical pack bundle & schemas) | Independent tag-only GitHub Actions track (`.github/workflows/release.yml` on `pack-hospitality-v*` tags) | `MIT OR Apache-2.0` |

---

## 3. Core edge data model (D1)

The published-site edge Worker (`sws-worker-site`, using `crates/sws-worker-site/migrations/0001_init.sql`) uses bound SQLite parameters in Cloudflare D1:

- **`form_submissions` (Phase 0/1 table: `inquiries`).** Stores structured form payloads (`kind` such as `contact` or pack-defined kinds like `private_event`), contact fields, `message`, `source_route`, `status` (`new` | `responded` | `closed` | `spam`), and `created_at`.
- **`newsletter_subscribers`.** Stores `email` (unique, `NOCASE`), `status` (`subscribed` | `unsubscribed` | `bounced`), `consent_timestamp` (ISO-8601 UTC stamped by the Worker), `source_route`, `consent_text`, `created_at`, `updated_at`, and `unsubscribed_at`.
  - Public web forms never flip `unsubscribed` or `bounced` rows back to `subscribed` (re-subscription requires explicit double opt-in).
- **Pack-extended tables (e.g., `events` in `packs/hospitality`).** Optional schema tables such as scheduled `events` (`slug`, `title`, `description`, `starts_at`, `ends_at`, `is_ticketed`, `ticket_url`) are enabled when the corresponding pack module is active.

---

## 4. Reference vertical pack: `packs/hospitality`

All hospitality-specific schemas, UI templates, third-party restaurant integrations, and compliance linters live in `packs/hospitality` rather than SWS core. Its showcase fixture is the 100% fictional **`shirlington-hearth-demo`** (`packs/hospitality/demo/venue.json`).

### Hospitality module map (M1–M10)

| Module | Name | Routes | Data | Default flag | Status |
| --- | --- | --- | --- | --- | --- |
| M1 | Core Shell & NAP | `/` (header, footer, NAP, hours), `LocalBusiness` JSON-LD | `venue.json` | on (required) | Phase 0 ✅ |
| M2 | Semantic Menus | `/menu`, `/happy-hour` | `venue.json` `menu`, `happy_hour` | on (required) | Phase 0 on `/`; Phase 1 routes |
| M3 | Private Events Inquiry | `/book-an-event` → `POST /api/inquire` (`kind=private_event`) | D1 `inquiries` / `form_submissions` | on | API ✅, page Phase 1 |
| M4 | Contact | `/contact` → `POST /api/inquire` (`kind=contact`) | D1 `inquiries` / `form_submissions` | on | API ✅, page Phase 1 |
| M5 | Newsletter + CAN-SPAM Consent | Footer form → `POST /api/newsletter` | D1 `newsletter_subscribers` | on | Phase 0 ✅ |
| M6 | Events Calendar | `/events`, `Event` JSON-LD | D1 `events` | off in demo until Phase 1 | schema ✅ |
| M7 | Visit | `/visit` (hours, holiday overrides, directions, venue accessibility) | `venue.json` | on | Phase 1 |
| M8 | About / Story / Gallery | `/about` | `venue.json` + optimized images | off in demo until Phase 1 | Phase 1 |
| M9 | SEO & Discovery | `sitemap.xml`, `robots.txt`, NAP drift audit | `venue.json` | on (required) | Phase 0 ✅ |
| M10 | Security, a11y & Observability Gate | `_headers` + `with_security_headers`, `/api/health`, CI gates | — | on (required) | Phase 0 ✅ |

The Phase 1 hospitality showcase exposes 10 routes: `/`, `/menu`, `/happy-hour`, `/events`, `/book-an-event`, `/visit`, `/about`, `/contact`, `/accessibility`, `/privacy`.

### Hospitality pluggable integrations (`venue.json` `integrations`)

| Key | Values | Default | Notes |
| --- | --- | --- | --- |
| `toast` | `{enabled, restaurant_guid}` | `enabled: false` | Online ordering is link-out only; no Toast API keys in Phase 0/1 |
| `seating_policy` | `"walk_in_only"` \| `"opentable"` \| `"resy"` | `"walk_in_only"` | Controls the reservation CTA. A plain link is preferred over an embed (CSP and a11y) |
| `opentable` / `resy` | `{restaurant_id}` / `{venue_id}` | `null` | Used only when matching `seating_policy` |
| `inkind` | `{enabled}` | `false` | House-account / inKind promo link, off by default |
| `turnstile` | `{site_key, test_key}` | test key | Production Turnstile key per tenant |

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
