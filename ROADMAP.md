# Solo Website System (SWS) Roadmap

This document outlines the planned milestones for Solo Website System (SWS). Priorities and milestone details may evolve based on contributor feedback and architectural reviews.

---

## `v0.1.0` — Core Engine, CLI & Vertical Pack Foundation

**Goal:** Deliver a deterministic, dual-licensed (`MIT OR Apache-2.0`) Rust/Wasm compilation pipeline, local CLI/MCP tooling, and the first vertical pack.

- **`sws-ir`** — Strongly typed, industry-agnostic Site Intermediate Representation (pages, sections, semantic blocks, design tokens, routes, metadata, and structured JSON-LD).
- **`sws-css`** — Permissively licensed (`MIT OR Apache-2.0`) CSS AST, parser, validator, and emitter without MPL/copyleft dependencies.
- **`sws-compiler`** — Deterministic HTML/CSS compiler with built-in WCAG 2.2 Level AA structural enforcement (single `<h1>`, sequential headings, contrast checks, landmarks, progressive-enhancement forms).
- **`sws-plugin`** — Stable plugin ABI for extending IR transforms, block renderers, and post-compile lint/audit passes.
- **`sws-cli`** — Command-line interface (`solo`) and `stdio` Model Context Protocol (MCP) server for validating, compiling, auditing, and previewing Solo sites locally.
- **`sws-wasm`** — WebAssembly browser bridge exposing `sws-ir`, `sws-css`, and `sws-compiler` to browser-based tooling with zero server round-trips.
- **`packs/hospitality`** — Open-source reference vertical pack (`MIT OR Apache-2.0`) containing hospitality-specific schemas, domain templates, integration descriptors, and optional advisory compliance linters, cleanly isolated from `crates/*`.

---

## Design principles for v0.2 onward

These are the decisions recorded by the Studio and Builder release issues (#54 to #88). The design document those issues cite ("Studio Control Plane and Visual Builder design") is not part of this repository, so anything the issues do not state is listed under [Open assumptions and questions](#open-assumptions-and-questions) instead of being presented as decided.

- **Rust core, thin TypeScript shell.** Canonical IR, command reduction, undo/redo, validation, contrast diagnostics and compilation live in Rust. The browser holds an opaque Wasm editing session and never mirrors canonical IR or validation in JavaScript (#70). TypeScript is limited to generated IR types and schemas (#56), the typed `StudioEngine` wrapper (#70), transport (#75) and the editor shell (#77, Preact and Signals). The earlier "pure-Rust Studio frontend" wording is retired.
- **Command log instead of CRDT.** Collaboration uses one versioned command batch applied atomically by the Rust reducer (#55). Each site has a monotonic command log and snapshots (#62). Stale batches are rebased only for explicitly tested independent field updates, and stale structural commands and incompatible undo are rejected with an actionable conflict. Collaborative undo is a new inverse command, not a shared history rewind. There is no CRDT.
- **Cardless by default, R2 opt-in.** The default path needs no payment method: size-capped D1 blobs when R2 is unbound (#63), GitHub OAuth instead of Cloudflare Access or an auth SaaS (#60), and a cardless-ready config with R2 unbound by default (#68). R2 is an explicit, separately tested opt-in and never substitutes for cardless evidence (#69). The CLI never provisions cloud resources implicitly.
- **Cloud quota values are assumptions until verified.** Every Cloudflare limit, quota or price mentioned in this repository is an assumption until verified against current Cloudflare documentation and a fresh Workers Free account (#59, #63, #69). Application caps and alerts reduce billing risk but are not provider-side hard stops (#63).
- **The Hospitality port milestone stays separate.** The `v0.2 — Hospitality pack feature ports` GitHub milestone (#43, #44, #45) is independent of the `v0.2.0-*` Studio milestones below. The names are close, so always cite the full milestone title.
- **Baseline.** `workers-rs` stays on `worker` 0.9 with `worker-build` 0.9 (see `Cargo.toml` and `Cargo.lock`). Nothing in this roadmap downgrades it.

### Approved launch contracts

| Contract | Source |
| --- | --- |
| The local native/Wasm editing gate must pass before any Studio integration. | #55, #61, #70 |
| Onboarding works on a fresh Workers Free account with no payment method. | #60, #69 |
| Collaboration is narrow: hibernating per-site sessions, conservative conflicts, revocation, soft presence. | #62, #65 |
| The native CLI is the fallback for compile and upload when Free Queue CPU headroom is not proven. | #59, #64, #68 |
| The editing canvas is script-free: compiler-produced markup only, opaque sandboxed iframe, trusted bridge only. | #72, #77 |
| Saves are explicit file saves. `Saved to file` appears only after a successful write, `Synced to Studio` only after the revision is acknowledged, and queued is neither. In-memory edits are never implied to be persisted. | #75, #77 |
| Third-party runtime pack loading is deferred. Only statically linked Rust packs ship at launch. | #73, #74, #78 |
| GitHub PR creation, `repository_dispatch` and automated production deployment are out of scope. GitOps uses a verified downloadable export. | #80 |

---

## `v0.2.0` — Studio Control Plane, Publish Bridge & CLI Sync

**Goal:** Add an optional, cardless-first Studio control plane (Cloudflare Workers) and CLI sync, built on a shared Rust command reducer, without changing the standalone published-site Worker. Three GitHub milestones deliver it in order.

### `v0.2.0-alpha.1` — IR Command & Schema Foundation (Phase 0)

**Exit:** the local native/Wasm editing slice passes before Studio integration: stable IDs, explicit legacy upgrade, undo/redo, lossless save/reopen, byte-identical native and Wasm production output, schema drift check and reproducible Wasm tooling.

- #54 IR compatibility and stable node ID audit. The public format does not change in the audit. Valid v0.1.2 files keep building and checking without writes.
- #55 canonical Rust command reducer and undo/redo, with the automated legacy-demo edit, undo/redo, save, reopen and native-versus-Wasm gate.
- #56 generated TypeScript types and Draft 2020-12 JSON Schema in `packages/@solo-website-system/ir-types`, with no JS runtime and `npm run verify` failing on drift. This is the first `packages/` content, so `packages/` lands in v0.2.0-alpha.1, not v0.3.0.
- #57 pinned, reproducible Wasm toolchain (see `docs/WASM_TOOLCHAIN.md`). Gate 1 criteria stay intact (see ARCHITECTURE §6).
- #58 this documentation alignment.
- #70 pulls forward only the minimal Wasm session work the #55 gate needs. The full wrapper stays in v0.3.0-alpha.1.

### `v0.2.0-beta.1` — Studio Control Plane Core (Phase 1)

**Exit:** hibernating multi-client sessions with conservative conflict handling and revocation, bounded cardless and opt-in R2 storage, and an asynchronous publish pipeline whose default compile path is justified by Free-account evidence.

- #59 time-boxed Workers Free Queue CPU spike. Server compilation is the default only if the supported workload shows repeatable headroom. Otherwise the CLI compile-and-upload path (#68) is the default. Queue wall time is not CPU time. Remote deployment needs approval.
- #60 cardless GitHub OAuth model: self-hosted OAuth app, one bootstrapped owner, explicit collaborator invitations. Magic links, passkeys and Cloudflare Access are not launch dependencies.
- #61 optional wasm32 Studio Worker and D1 workspace registry, configured separately from the visitor Worker. D1 checkpoint writes do not happen on each edit.
- #62 hibernating Durable Object command sessions (`SiteSession`, `docs/STUDIO_SESSIONS.md`). Checkpoint every 50 commands or on save/publish. Sessions stay closed (401) until #65 supplies real authentication.
- #63 bounded cardless, R2 and local asset stores.
- #64 asynchronous publish job pipeline against an immutable revision. Compilation never runs inside HTTP handlers.

### `v0.2.0` — Publish Bridge, Auth & CLI Sync (Phase 2)

**Exit:** small-site publishing is verified on a fresh no-payment-method account using the CLI and a reference sync client (no Builder dependency), with opt-in R2 tested separately. Failure recovery, audited rollback, header parity and independent visitor traffic are demonstrated. A failed cardless run blocks the release.

- #65 cardless authentication, Turnstile on public forms and scoped, rotating, revocable CLI/MCP tokens. Edit-only credentials cannot publish or activate production.
- #66 publish compiled assets to the standalone edge Worker through Direct Upload Assets, preserving `run_worker_first` for `/api/*`, pinning each publish to an immutable revision and providing an audited rollback.
- #67 draft previews on an isolated, non-indexed origin.
- #68 `solo studio` sync, publish and opt-in R2 onboarding in the native CLI.
- #69 end-to-end verification in cardless and R2 modes.

Visitor traffic must keep working when Studio is unavailable. The Studio Worker never replaces `sws-worker-site`.

---

## `v0.3.0` — Builder, Standalone Export & Release Hardening

**Goal:** Ship the browser Builder as a thin TypeScript shell over the Rust/Wasm core, plus hardened standalone Rust Worker export, without a Studio account being required to edit or export. Builder additions (templates, pages, SEO, form builder) follow in the `v0.3.x` releases below.

### `v0.3.0-alpha.1` — Opaque Wasm Session & Isolated Canvas (Phase 3)

**Exit:** script-free sandboxed canvas (trusted bridge only), p95 input-to-preview under 16 ms in a controlled token scrub, and verified focus, scroll, preview instrumentation and origin isolation.

- #70 opaque Wasm editing session and typed `StudioEngine` wrapper.
- #71 incremental token CSS and keyed section HTML deltas. Preview IDs and hooks never reach production or export output.
- #72 isolated canvas bridge (opaque sandbox iframe, validated `MessageChannel` handshake, Idiomorph section morphing).

### `v0.3.0-beta.1` — Builder, Inspector & Pack Hooks (Phase 4)

**Exit:** accessible editing of fictional demo menus, hours and tokens using the existing Rust pack, explicit local file saves, distinct Studio sync status, live diagnostics and lossless CLI/MCP round-trips. Third-party runtime packs are deferred.

- #74 Rust pack ABI extension (schema, declarative UI hints, diagnostics, route and migration descriptors) for statically linked Rust packs.
- #75 Studio client transport, offline adapters, and local IR open with explicit file save without Studio.
- #76 accessible schema-driven inspector kit.
- #77 visual editor shell and accessible selection overlay. The canvas does not simulate live third-party widgets or user scripts.
- #78 hospitality editing and CLI/MCP round-trip validation, with no venue-specific code in the generic UI.

### `v0.3.0` — Standalone Export & Release Hardening (Phase 5)

**Exit:** downloadable verified standalone Rust Worker export, independent Builder, canvas and export checks, Wasm and licensing budgets, and signed tag-only releases from verified squash commits on `main`. No automated GitHub PR or dispatch publishing.

- #79 hardened `solo export` with `--verify` (header parity, static accessibility, wasm32 build, dependency licensing). The export runs with no Studio account or runtime dependency.
- #80 verified, downloadable export artifact for user-controlled GitOps.
- #81 Wasm, frontend, startup and licensing budgets and gates.
- #82 exported and published site verification, versioned npm packages alongside signed-tag CLI artifacts, and the squash-only `main` workflow (see CONTRIBUTING §6 and §7).

---

## `v0.3.x` — Builder additions

**Goal:** Extend the standalone Builder after `v0.3.0`, one release at a time, without requiring a Studio account.

### `v0.3.1` — Templates, Fonts & Media Library

**Exit:** a starter template produces an exported site that passes the WCAG 2.2 AA gates, and images are resized at build time with no third-party font or image CDN.

- #97 starter template gallery (8 to 12 templates, fictional fixtures only).
- #98 self-hosted font picker.
- #99 media library on the #63 asset store with responsive resizing and crop/focal metadata applied at compile.

### `v0.3.2` — Pages, Navigation & SEO

**Exit:** sitemap and redirects are verified in CI and the Lighthouse SEO score does not regress.

- #100 page and navigation management with breakpoint preview.
- #101 SEO panel (title, description, Open Graph).
- #102 sitemap, robots and `_redirects` generation (redirect manager).

### `v0.3.3` — Visual Form Builder

**Exit:** generated forms pass the WCAG 2.2 AA gates, keep native semantics, and fail closed without Turnstile.

- #103 visual form block on the existing Turnstile-protected form pipeline.

---

## `v0.4.0` — Import Static Sites

**Goal:** Import built static HTML, CSS and local assets from a folder or ZIP in the standalone Builder, show a best-effort inert reference, and produce an editable native Solo draft with a migration report (#83 to #88). No Studio account or network is required.

**Exit:** a two-page fixture imports, edits, saves, reopens and exports with working links and images after the source is removed. Identical inputs are deterministic, and the preview cannot execute source code or make external requests. Wix/Squarespace adapters, GitHub integrations, crawling and pixel-perfect conversion are out of scope.

---

## `v0.5.0` — Content & CMS

**Goal:** Schema-driven content in the canonical IR, a blog, search and media content, multilingual sites, scheduling and restore points.

### `v0.5.0-alpha.1` — Collections & Content Model

**Exit:** collections round-trip losslessly through the Rust reducer, native and Wasm output stay byte-identical, and legacy files keep building without writes.

- #104 collections in the canonical IR.
- #105 generated TypeScript types and JSON Schema for collections.
- #106 collection list and detail pages.

### `v0.5.0-beta.1` — Blog, Search & Media Content

**Exit:** the fictional demo gains a blog and gallery that pass the a11y and header gates.

- #107 blog collection (posts, tags, categories, authors) with RSS/Atom.
- #108 galleries with a no-JS fallback, privacy-enhanced click-to-load embeds and downloads.
- #109 static site search index generated at compile.
- #110 generic events promoted from the hospitality pack to core.

### `v0.5.0` — Multilingual, Scheduling & Restore Points

**Exit:** a scheduled post publishes on time on a cardless account, and a restore returns a site to an earlier revision with an audited trail.

- #111 per-locale pages with hreflang and a locale switcher.
- #112 scheduled publishing through Cron Triggers and the #64 pipeline.
- #113 moderated, Turnstile-protected comments.
- #114 site-wide restore points over #62 revisions.
- #115 extended JSON-LD (Article, Event, FAQ).

---

## `v0.6.0` — Growth

**Goal:** Capture and act on visitor interest without breaking the cookie-free default.

- `v0.6.0-alpha.1` — Inbox, Contacts & Webhooks (#117–#119). **Exit:** submissions are tenant-scoped, exportable and redacted in logs; webhooks (using the #65 scoped tokens) are signed and retried safely.
- `v0.6.0-beta.1` — Analytics & Consent (#120–#123). **Exit:** the default site sets no cookies and ships no third-party scripts.
- `v0.6.0` — Campaigns & Engagement (#124–#127). **Exit:** a campaign sends through a bring-your-own provider and never re-subscribes an unsubscribed contact.

---

## `v0.7.0` — Commerce

**Goal:** Full commerce on a shared visitor identity layer (OAuth-only by default, magic-link email opt-in with a bring-your-own provider) that v0.9.0 Members reuses.

- `v0.7.0-alpha.1` — Visitor Identity & Catalog (#128–#131). **Exit:** visitor sign-in works on a cardless account and catalog pages pass the a11y gates.
- `v0.7.0-beta.1` — Cart, Payments & Fulfilment (#132–#138). **Exit:** a test-mode order completes end to end with webhook-verified state and atomic inventory decrement.
- `v0.7.0` — Orders, Subscriptions & Growth (#139–#145). **Exit:** refunds and subscriptions are verified in Stripe test mode and the full store passes the a11y and header gates.

---

## `v0.8.0` — Services & bookings

**Goal:** Promote hospitality scheduling to a generic core bookings feature.

- `v0.8.0-beta.1` — Core Bookings (#146–#148). **Exit:** the hospitality pack runs on the core bookings API with no regressions.
- `v0.8.0` — Paid Bookings & Reminders (#149–#151). **Exit:** a deposit booking completes in Stripe test mode and reminders respect consent.

---

## `v0.9.0` — Members

**Goal:** Member features on the v0.7.0 visitor identity layer.

- `v0.9.0-beta.1` — Gated Content & Memberships (#152–#154). **Exit:** gated content is never served to unauthorised visitors, verified by tests.
- `v0.9.0` — Community, Courses & Loyalty (#155–#157). **Exit:** moderation, rate limits and a11y gates pass for all member-generated content.

---

## `v0.10.0` — Migration

**Goal:** Move existing sites into Solo. Builds on the v0.4.0 import pipeline.

- `v0.10.0-beta.1` — Builder Import Adapters (WordPress WXR, Squarespace, Shopify, Wix) (#158–#160). **Exit:** fictional fixture exports from each source import deterministically into editable drafts with a migration report.
- `v0.10.0` — URL Mapping & GitHub Sync (#161–#162). GitHub repository sync is in scope here by maintainer decision; #80 still keeps GitHub integration out of v0.3.0. **Exit:** an imported site keeps its old URLs working through generated redirects, and repo sync round-trips without data loss.

---

## `v0.11.0` — Onboarding & operations

**Goal:** Make setup and day-two operations approachable. Separate from v0.10.0.

- `v0.11.0-beta.1` — Setup Wizard & Domains (#163–#165). **Exit:** a non-technical tester publishes to a custom domain on a fresh cardless account using only the wizard.
- `v0.11.0` — Operations & Support (#166–#169). **Exit:** backup and restore are verified end to end and the installable PWA Studio passes the a11y gates.

---

## `v1.0.0`

- `v1.0.0-rc.1` — Audits & API Freeze (#170–#172). External security and accessibility audits, a frozen public pack API, and IR upgrade and migration guarantees. **Exit:** audit findings are resolved or documented and the pack API and IR compatibility policy are published.
- `v1.0.0` — Initial Release (#173–#175). User guides, support process and response policy, and a signed tag-only release from a verified squash commit on `main`. **Exit:** all prior milestone exits hold on the release commit.

### Ground rules for v0.5.0 onward

- The cardless default keeps working. Paid services are optional and bring-your-own-key.
- Generic features live in core, vertical features in packs.
- Every public form gets Turnstile and the WCAG 2.2 AA gates.
- Cloudflare quotas stay assumptions until verified.

---

## Post-launch and separate tracks

- #73 evaluate third-party data-only pack loading (not a launch dependency, no milestone). Existing statically linked Rust packs are covered by #74 and #78. Whether executable component plugins are deferred to v0.4+ is part of that evaluation.
- Hospitality pack feature ports (`v0.2 — Hospitality pack feature ports`: #43 events calendar, #44 call and booking scheduling, #45 sandbox Stripe checkout) are tracked separately from the Studio milestones above.
- Post-1.0 industry packs, tracked with the GitHub label `post-1.0 pack`: salon/spa, contractor/trades, nonprofit, professional services, fitness, retail, creator/portfolio, real estate and medical (non-PHI).
- A curated, data-only pack registry follows the #73 evaluation, after 1.0.

### Non-goals

- POS hardware, an ad campaign manager, domain reselling and hosted email mailboxes (Cloudflare Email Routing forwarding is used instead).
- Native mobile apps (an installable PWA Studio is the plan).
- A hosted stock media catalog.
- AI assist is not planned.
- Custom code in the editor: the canvas stays script-free, and custom code is possible only via export.
- One-click third-party code extensions (deferred).

---

## Open assumptions and questions

The Studio and Builder design document is unavailable to this repository. The items below are not determined by the issues or milestones and must not be read as approved decisions.

1. **Cloudflare limits and pricing.** All Workers, D1, Durable Object, Queue, R2 and size or CPU values are unverified assumptions until checked against current Cloudflare documentation and a fresh Free account (#59, #63, #69). This includes the limits that the 2.5 MiB compressed Worker target and 600 KiB gzipped browser Wasm budget (#81) are measured against.
2. **Provisional cardless envelope.** Up to 20 pages, 1 MiB per uploaded asset and 50 MiB of stored assets per site (#63) are provisional. Workspace-wide and preview/export budgets and retention are set only after fresh-account evidence.
3. **Default compile path.** Server-side compilation versus CLI compile-and-upload is open until the #59 spike (it needs approved staging/Free account access).
4. **Studio frontend layout.** The issues fix Preact and Signals for the shell (#77) and `packages/@solo-website-system/ir-types` (#56), but not the exact `apps/` directory names, the npm package list beyond `ir-types`, or the build tool.
5. **Legacy ID and extension handling details** were settled in #54 and are implemented by #55, not decided here.
6. **Hostnames, retention periods and revocation delay.** Studio and preview hostnames, preview/export retention, and the maximum delay for revoked sockets to stop receiving updates (#65 requires documenting and testing it) are unspecified.
7. **Durable Object class names.** The earlier roadmap named `ProjectRoom` and `RateLimiter`. The per-site command session class is now `SiteSession` (#62); `ProjectRoom` is retired and was never CRDT synchronization. A `RateLimiter` class, if one is ever needed, is still open.
