# Contributing to Solo Website System (SWS)

Thank you for contributing to **Solo Website System (SWS)**. SWS is a deterministic, accessibility-first website compiler, edge runtime, and visual studio built on a dual-licensed (`MIT OR Apache-2.0`) Rust + WebAssembly core and TypeScript workspace.

## Monorepo Structure

SWS is organized as a Cargo + pnpm monorepo:

```text
├── crates/       # Open-source core Rust engine & tooling (MIT OR Apache-2.0, industry-agnostic)
├── apps/         # Studio, visual builder, canvas, dashboard, and Worker control-plane apps
├── packages/     # Shared TypeScript packages (e.g., SDK, UI components)
└── packs/        # Industry vertical packs (e.g., packs/hospitality)
```

### Dependency & Separation Rules

- **Core engine crates (`crates/*`) are strictly industry-agnostic.** Domain-specific schemas, copy, integrations, and advisory checks belong in `packs/*` (such as `packs/hospitality`), never in `crates/*`.
- **One-way dependencies:** `apps/`, `packages/`, and `packs/` may depend on `crates/`; `crates/` must never depend on vertical packs or application code.
- **Host testability:** Keep pure logic in host-testable Rust modules (`cargo test`). Browser/Workers Wasm bindings must sit behind `#[cfg(target_arch = "wasm32")]`.

---

## Prerequisites & Local Setup

### Option A: Dev Container (Recommended)

Open the repository in a compatible editor (VS Code / Dev Containers CLI) using `.devcontainer/devcontainer.json`. The Dev Container pre-provisions Rust, `wasm32-unknown-unknown`, `clippy`, `cargo-deny`, `worker-build`, Node LTS, `pnpm`, and Playwright Chromium.

### Option B: Local Workstation

1. **Node.js LTS & pnpm**
   Use `nvm` to select the active Node.js LTS release and enable `pnpm`:
   ```bash
   export NVM_DIR="$HOME/.nvm"
   [ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"
   nvm use --lts
   corepack enable
   pnpm --version
   ```

2. **Rust Toolchain (`wasm32-unknown-unknown` & `clippy`)**
   Install stable Rust via `rustup` and add the Wasm target and Clippy:
   ```bash
   rustup toolchain install stable
   rustup default stable
   rustup target add wasm32-unknown-unknown
   rustup component add clippy
   ```

3. **Cargo Tooling (`worker-build` & `cargo-deny`)**
   Ensure `~/.cargo/bin` is on your `PATH`, then install:
   ```bash
   export PATH="$HOME/.cargo/bin:$PATH"
   cargo install worker-build@^0.9 --locked
   cargo install cargo-deny --locked
   ```

4. **Workspace Dependencies & Playwright Chromium**
   Use `npm ci` with the committed `package-lock.json` (`pnpm` is optional for future workspace apps):
   ```bash
   npm ci
   npx playwright install --with-deps chromium
   ```

---

## Quality Gates & Standards

Every pull request must pass all automated and manual quality gates listed below.

### 1. License & Dependency Hygiene (`cargo deny check`)

SWS core crates are dual-licensed under `MIT OR Apache-2.0`. Run:

```bash
cargo deny check
```

Per `deny.toml`, only strictly permissive OSI-approved licenses are permitted (`MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-DFS-2016`, `CC0-1.0`, plus narrowly scoped `Unicode-3.0` exceptions documented in `deny.toml`).

- **Prohibited licenses:** GPL, AGPL, LGPL, MPL, SSPL, BUSL, Elastic, and any non-OSI/source-available license.
- **Never add `lightningcss` or `cssparser`** (or any other MPL-2.0 / copyleft crate) to the dependency graph.

### 2. Rust Host Tests & Wasm Clippy

Run all host unit/integration tests and lint the `wasm32-unknown-unknown` target for worker/wasm crates:

```bash
cargo test --workspace --locked
cargo clippy --locked --target wasm32-unknown-unknown -p sws-security -p sws-worker-site -p sws-wasm -- -D warnings
```

Worker and Wasm constraints:
- No `tokio`, native OpenSSL, `ring` C-assembly, OS threads, `std::fs`, or blocking I/O in Worker/Wasm crates.
- Never call `unwrap()` on untrusted input.
- Use bound parameters for all D1 SQL queries and enforce a 16 KB streaming request body cap on public endpoints.

### 3. Accessibility — WCAG 2.2 Level AA (Native Semantics Only)

All rendered HTML and UI components must satisfy WCAG 2.2 Level AA using native semantics:

- **Headings:** Exactly one `<h1>` per page with sequential heading levels (no skipped levels).
- **Landmarks & Structure:** Include `<header>`, `<nav>`, `<main>`, `<footer>`, a skip link, and `lang` on `<html>`.
- **Contrast:** At least `4.5:1` for normal text and `3:1` for large text and UI components.
- **Interaction:** Visible `:focus-visible` outlines (never obscured) and interactive targets at least `24x24` CSS px.
- **Forms:** Progressive-enhancement forms using `<label>`, `aria-describedby` for field errors, and an `aria-live` status region. Forms must function without client-side JavaScript.
- **No overlay widgets:** Third-party accessibility overlay scripts/widgets are strictly prohibited.

Verify locally with `test:a11y` (axe-core `wcag22aa`) and `lhci` (Lighthouse Accessibility score = `1.0`).

### 4. Dual Security-Header Parity

Static hosting (`_headers`) and dynamic Worker `/api/*` responses (Rust `SECURITY_HEADERS` via `with_security_headers`) must remain **byte-identical**. Whenever you modify a security header, update both locations in the same commit and verify parity via `cargo test` and the header parity checks.

### 5. Two-Zone IP & Trademark Firewall (Zone A vs. Zone B)

SWS enforces a strict separation between public/open-source assets and private client material:

- **Zone A (Public / Open-Source):** `crates/`, `apps/`, `packages/`, `packs/`, docs, tests, CI workflows, and demo fixtures.
  - Must contain **zero** real third-party business names, logos, menus, addresses, phone numbers, or trademarks.
  - Demo data must be 100% fictional (invented street names, `555-01xx` phone numbers, `.example` domains).
- **Zone B (Private, Never Committed):** `private/`, `.dev.vars`, `*.env`, and real client assets or secrets.
  - Never read, copy, or commit Zone B files or secrets into Zone A.

### 6. Git Workflow & Conventional Commits

- Create an isolated feature branch per change (`feat/...`, `fix/...`, `docs/...`, `chore/...`).
- Use [Conventional Commits](https://www.conventionalcommits.org/) referencing the related issue when applicable:
  - `feat(sws-compiler): enforce heading hierarchy validation (fixes #12)`
  - `fix(sws-css): preserve media query order during minification`
- Include verification command results in your pull request description.
