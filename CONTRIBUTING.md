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

- **Zone A (Public / Open-Source):** `crates/`, `apps/`, `packages/`, `packs/`, `integrations/`, docs, tests, CI workflows, and demo fixtures.
  - Must contain **zero** real third-party business names, logos, menus, addresses, phone numbers, or trademarks.
  - Demo data must be 100% fictional (invented street names, `555-01xx` phone numbers, `.example` domains).
- **Zone B (Private, Never Committed):** `private/`, `.dev.vars`, `*.env`, and real client assets or secrets.
  - Never read, copy, or commit Zone B files or secrets into Zone A.

### 6. Git Workflow, Signed Commits & Conventional Commits

- Create an isolated feature branch per change (`feat/...`, `fix/...`, `docs/...`, `chore/...`).
- **Signed commits & tags required:** All commits pushed to branches and pull requests must be cryptographically signed (SSH or GPG, verified on GitHub), and release tags should be signed too. To configure Git with an SSH key:
  ```bash
  git config --global gpg.format ssh
  git config --global user.signingkey ~/.ssh/id_ed25519.pub
  git config --global commit.gpgsign true
  git config --global tag.gpgsign true
  ```
  Then add the same public key as a **Signing Key** under **GitHub → Settings → SSH and GPG keys**. GPG signing (`git config --global commit.gpgsign true` with `user.signingkey`) is equally accepted.
- **Git hooks (release + hygiene guards):** install once per clone with `npm run setup:hooks` (equivalent to `git config core.hooksPath .githooks`). The committed hooks enforce:
  - `pre-commit` — blocks unsigned-intent commits (`commit.gpgsign` disabled; bypass `SWS_SKIP_SIGN_CHECK=1`), blocks staging Zone B material (`private/`, `.dev.vars*`, env files, key material — `*.example`/`*.template` templates allowed), rejects unparseable staged JSON, and warns when rendered output (`core/public`, `sites/*/public`) is staged (confirm it came from `npm run render` / `npm run sites:build`). Deliberately fast; full tests run pre-push and in CI.
  - `post-commit` — best-effort check that the just-created HEAD actually carries a signature; an unsigned HEAD gets a fix-it message (`git commit --amend -S --no-edit`). Always exits 0.
  - `pre-push` — rejects `v*` / `pack-hospitality-v*` tags whose version does not match the `[workspace.package]` version at the tagged commit, and rejects lightweight or unsigned release tags; branch pushes run `npm run verify` **and then the server-dependent browser specs** (`npx playwright test` — it spins up its own dev server; skip with `SWS_SKIP_WEBTESTS=1`) before the code leaves the machine (`SWS_SKIP_VERIFY=1` skips the verify half; tags-only pushes skip both because `release.yml` verifies tags). The same version check also runs as CI steps in both `release.yml` tag jobs, so a release cannot ship mismatched even when hooks are not installed.
  - Worktrees don't see a relative `core.hooksPath` unless `.githooks` is linked there (`ln -s <repo>/.githooks <worktree>/.githooks`).
- Use [Conventional Commits](https://www.conventionalcommits.org/) referencing the related issue when applicable:
  - `feat(sws-compiler): enforce heading hierarchy validation (fixes #12)`
  - `fix(sws-css): preserve media query order during minification`
- Include verification command results in your pull request description.

### 7. Release Runbook (tag-only pipeline)

Releases are tag-triggered only (`.github/workflows/release.yml`). There are two tag families, and **both must match the workspace version in `Cargo.toml`** (enforced by the pre-push hook and by CI):

1. **Prepare the release via PR.** On an isolated `chore/release-...` branch:
   - Bump `[workspace.package] version` in `Cargo.toml` (all crates inherit via `version.workspace = true`); run `cargo update --workspace --precise <no-op>`/`cargo check` so `Cargo.lock` picks up the new version.
   - Move the relevant `CHANGELOG.md` `[Unreleased]` entries under a new dated `## [x.y.z]` heading.
   - Open the PR, get it merged to `main`.
2. **Tag `main`** at the release commit — annotated and signed (with `tag.gpgsign true`, `git tag -a -m "..." vX.Y.Z` signs automatically):
   - `git tag -a -m "Solo vX.Y.Z" vX.Y.Z` — full release: workspace verify → CLI binaries (5 targets) → GitHub Release with SHA256SUMS → landing/docs Cloudflare deploy (if `CLOUDFLARE_API_TOKEN` is configured).
   - `git tag -a -m "SWS Hospitality Pack vX.Y.Z" pack-hospitality-vX.Y.Z` — pack release: pack tests/clippy/deny → `packs/hospitality` tarball → GitHub Release.
3. **Push the tags** (pushing tags needs maintainer confirmation): `git push origin vX.Y.Z pack-hospitality-vX.Y.Z`.
4. Watch the `release` workflow; confirm the GitHub Release assets and `solo --version` output match the tag.
