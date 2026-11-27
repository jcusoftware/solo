# Changelog

All notable changes to Solo Website System (SWS) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Roadmap and architecture phase table now cover the v0.3.x Builder additions and milestones v0.5.0 through v1.0.0, plus post-1.0 packs and declared non-goals (refs #58).

### Added

- Scaffolded the optional Studio Worker `crates/sws-worker-studio` (#61): D1 workspace registry migration (`0001_registry.sql`), tenant-scoped repository SQL, strict request bounds, `GET|HEAD /api/health`, and a separate `wrangler.studio.jsonc` with its own local D1 state (`npm run studio:dev`, `studio:db:migrate:local`, `test:studio`).
- Bounded Studio asset stores (#63): new `sws-assets` crate (limits, quota reservations, validation, reconciliation, native-only local store), D1 migration `0002_asset_store.sql`, authenticated tenant-scoped asset routes with cardless D1 BLOB chunks by default and opt-in R2, `solo assets` for the local store, and the closed `auth` boundary (real auth is #65). Commit liveness uses a fresh clock read plus a safety margin that the orphan sweep outlasts, R2 objects are per-upload keys with tombstones, R2 downloads and eligible uploads stream, and the local store is lock-protected. Limits are provisional; application caps are not provider-side hard stops. New gates: `test:studio:assets` (CI only) and `check:studio:no-dev-auth`.
- Renamed `STUDIO_ORIGIN` to `STUDIO_PUBLIC_ORIGIN` in the Studio example config, which is now cardless by default (D1 + vars; R2, Queues and Durable Objects are commented opt-ins).

## [0.1.2] - 2026-11-25

Everything since the v0.1.1 tag lands here: the milestone-1 review remediation
wave, the milestone-2 feature ports into the hospitality pack, and the release
hardening.

### Added

- Events calendar for the hospitality pack: `events` venue schema with validation, a rendered `/events` page with `Event`/`ItemList` JSON-LD, and the worker events surface with a D1 migration (#43, #46).
- Sandbox Stripe checkout demo in the hospitality pack, config-gated: `checkout` schema, rendered checkout page, `/api/checkout/*` worker routes, and browser specs scoped like the rest of the pack (#45, #48).
- Call & booking scheduling for the hospitality pack, config-gated and ported from the pre-refactor feature line: `bookings` schema with availability validation, a `/book-a-call` page with keyboard-accessible slot selection, `/api/bookings`, Zoom meeting creation, and Google Calendar sync/auth routes with a scheduling D1 migration (#44, #50).
- Deterministic cross-process two-run byte-diff test guarding compiler output (#30).
- `solo preview` verb serving compiled output locally (#38).
- Deterministic CSS-text-to-AST parser in `sws-css` (#39).
- Accessible `<noscript>` notice inside every Turnstile-protected form, before the widget (#40).
- ABI version handshake and plugin contributions in `sws-plugin` (#41).
- `sws-wasm` surface for `sws-css` plus a wasm export smoke test (#42).
- Repository git hooks (`pre-commit` signing/Zone-B checks; `pre-push` tag/version sync, `npm run verify`, and the server-dependent browser specs) documented in CONTRIBUTING §6, with the same version-sync checks mirrored as CI steps in both `release.yml` tag jobs (#47, #51).

### Changed

- Performance: expand page bodies once and remove redundant input copies (#37).
- Synced AGENTS.md, ARCHITECTURE.md and README with implemented reality (#31); recorded the no-JS vs Turnstile boundary in AGENTS.md (#35).

### Fixed

- Fail closed on Turnstile TEST secrets and `DEV_MODE` bypasses (#27).
- Execute the emitted export bundle and unify route validation (#28).
- Give the Playwright webServer its DB and secret prerequisites so a bare run passes (#29).
- Make `npm run verify` pass on a fresh clone (#26).
- Surface silent compiler drops and enforce contrast coverage (#33).
- Cover the newsletter unsubscribed-re-subscription guard with a test (#34).
- Put cargo-deny on PATH for the CI verify job (#36).
- Untrack the accidental `node_modules` symlink and close the `.gitignore` gap (#49).
- **Release integrity:** the v0.1.1 binaries reported `solo --version` as 0.1.0 because the workspace version was not bumped at tag time. The workspace version now moves with the release, and mismatched release tags are rejected by the pre-push hook and CI.

## [0.1.1] - 2026-11-25

### Fixed

- Remediated medium and low findings from the 5-track code review (#3).

## [0.1.0] - 2026-11-24

### Changed

- Pivoted the repository into **Solo Website System (SWS)**, an industry-agnostic, deterministic website compiler and visual studio workspace organized as a Cargo + pnpm monorepo (`crates/`, `apps/`, `packages/`, `packs/`).
- Decoupled core engine crates (`crates/*`) from industry-specific vertical logic, isolating vertical-specific features, templates, and rules under `packs/` (including `packs/hospitality`).
- Replaced `main`-branch `release-plz` automation with a strictly tag-only GitHub Actions release pipeline (`.github/workflows/release.yml` triggered on `v*` and `pack-hospitality-v*` tags, with optional tag-gated Cloudflare Worker deployment).

### Fixed

- Aligned `deploy/cloudflare/wrangler.studio.example.jsonc` paths with `crates/sws-worker-studio`.
- Fixed shell operator precedence for `corepack` fallback in `.devcontainer/devcontainer.json`.
- Updated `README.md`, `CONTRIBUTING.md`, `docs/ARCHITECTURE.md`, `ROADMAP.md`, and `.github/PULL_REQUEST_TEMPLATE.md` for tag-only releases, `npm ci` onboarding, and Rust-first builder/export milestones.

### Added

- Static Landing (`sites/landing`, `sws-landing`, custom domain `solo.jcusoftware.com`) and Documentation (`sites/docs`, `sws-docs`, custom domain `docs.jcusoftware.com`) sites generated deterministically by `sites/build.mjs`, including release CLI download links, Getting Started, User Guide, CLI & MCP Reference, Packs, Live Showcase, Accessibility & Security, Deployment, FAQ & Troubleshooting, and Roadmap & Licensing pages.
- Converted the Cloudflare deploy step in `.github/workflows/release.yml` to build and deploy both static Workers Assets sites (`sws-landing` and `sws-docs`) on `v*` tags with exact release download URLs and security-header parity checks.
- Dual open-source licensing (`MIT OR Apache-2.0`) via `LICENSE-MIT` and `LICENSE-APACHE`.
- Community and governance documentation: `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, and `ROADMAP.md`.
- GitHub issue and pull request templates in `.github/ISSUE_TEMPLATE/` and `.github/PULL_REQUEST_TEMPLATE.md`.
