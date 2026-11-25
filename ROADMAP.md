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

## `v0.2.0` — Cloudflare Control Plane & Real-Time Sync

**Goal:** Add a multi-tenant, edge-native Cloudflare Workers control plane supporting collaborative editing, media storage, and asynchronous publishing.

- **`sws-worker-studio`** — Cloudflare Workers control plane enforcing dual security-header parity, Turnstile verification, and strict request bounds.
- **Cloudflare D1** — Bound-parameter relational persistence for authentication sessions, workspaces, projects, and deployment metadata.
- **Cloudflare R2** — Object storage for uploaded media assets and immutable compiled site bundles.
- **Cloudflare Queues** — Asynchronous publish pipeline, static artifact fan-out, and signed outbound webhooks.
- **SQLite-backed Durable Objects**:
  - `ProjectRoom` — Real-time CRDT / patch synchronization for concurrent project editing.
  - `RateLimiter` — Edge-enforced per-actor and per-endpoint rate limiting.

---

## `v0.3.0` — Rust Visual Builder Canvas, Studio Apps & Rust App Export

**Goal:** Ship an interactive, pure-Rust/Wasm browser-based visual studio (`apps/builder`, `apps/canvas`, `apps/dashboard`) that compiles directly against `sws-ir`, `sws-css`, and `sws-compiler`, connects to `sws-worker-studio`, and supports `solo export` emitting standalone Rust Cloudflare Worker applications.

- **Pure-Rust Studio Frontend (`apps/builder`, `apps/canvas`, `apps/dashboard`)** — Rust/Wasm visual builder UI, deterministic editing canvas, and workspace/project/domain console sharing `sws-ir`, `sws-css`, and `sws-compiler` directly with zero TypeScript/JSON round-trip drift.
- **Standalone Rust App Export (`solo export`)** — Emits self-contained Rust Cloudflare Worker applications (`Cargo.toml`, typed route/form handlers, D1 migrations, compiled static assets, and `wrangler.jsonc`).
- **`sws-wasm`** — Browser WebAssembly bridge and shared UI bindings powering in-browser compilation and live WCAG 2.2 AA diagnostics.
