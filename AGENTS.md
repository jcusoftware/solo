# Solo (Solo Website System) — Project Rules

This repository is the open-source Solo (Solo Website System, SWS) workspace. The stack is Rust on Cloudflare Workers, with static assets, D1, and Turnstile, designed for $0 free-tier hosting. The user-facing CLI binary is `solo` (`crates/sws-cli`).

## 1. Directory zones & IP isolation (hard rule)

| Path | Zone | Contents | License |
| --- | --- | --- | --- |
| `crates/`, `core/` | A (public) | Reusable SWS core engine (`sws-ir`, `sws-css`, `sws-compiler`, `sws-plugin`, `sws-security`, `sws-cli`, `sws-wasm`, `sws-worker-site`), D1 migrations, tenant-agnostic tools (`core/tools`), rendered demo output (`core/public`) | `MIT OR Apache-2.0` |
| `packs/hospitality/` | A (public) | Open-source reference hospitality pack (schema, crate, VA ABC lint, and fictional `packs/hospitality/demo/venue.json`) | `MIT OR Apache-2.0` |
| `sites/` | A (public) | Static landing (`sws-landing`) and docs (`sws-docs`) generator and output | `MIT OR Apache-2.0` |
| `integrations/` | A (public, tracked) | Standalone integrations (Devvit subreddit release bot) | Per-package: `integrations/devvit-release-bot` is `BSD-3-Clause` |
| `docs/` | A (public) | Architecture documentation (`ARCHITECTURE.md`) | `MIT OR Apache-2.0` |
| `private/` | **B** (gitignored) | Private client assets, prospect dossiers, credentials (`*.env`, `.dev.vars`) | Never commit |

- **No tenant-specific or vertical-specific logic in `crates/` or `core/tools`.** Tools take an input path argument. `core/public/*` and `sites/*/public/*` are rendered output only: regenerate them with `npm run render` / `npm run sites:build` and never hand-edit them.
- **Code dependencies are one-way.** `packs/` may depend on `crates/`; `crates/` must not depend on any vertical pack (only optional feature wiring in `crates/sws-cli`).
- **Two-zone trademark firewall.** Zone A (core, reference pack, demo fixture, tests, docs, screenshots) must never contain real third-party venue names, logos, photos, menus or trademarks.
  - The public showcase uses only the fictional `shirlington-hearth-demo` (`packs/hospitality/demo/venue.json`). Its address, phone and domain are fictional on purpose: Hearthstone Lane, 555-01xx, `.example`.
  - Real-client or prospect material goes in `private/` (gitignored) or a separate private studio repo, never in Zone A.

## 2. License hygiene

`cargo deny check` must pass. The allowlist is MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-DFS-2016 and CC0-1.0. That blocks GPL, AGPL, LGPL, MPL and any source-available license (SSPL, BUSL, Elastic).

The only exceptions are narrow per-crate `Unicode-3.0` entries for ICU/unicode-ident, documented in `deny.toml`. Never broaden the global list; add a scoped exception with a justification and ask the user first. npm devDependencies are tooling only and never ship in the Worker.

Per-package licensing (not cargo dependencies): tracked integration packages under `integrations/` carry their own license — `integrations/devvit-release-bot` is `BSD-3-Clause` (a permissive allowlisted license, never a Rust crate dependency, so `cargo deny check` is unaffected).

## 3. Rust / Wasm constraints

- **Target and build.** Compile to `wasm32-unknown-unknown` with `workers-rs` (`worker` 0.9) and `worker-build`. Use `worker::Fetch` for outbound HTTP.
- **Banned crates.** No native OpenSSL, `ring` with C assembly, OS threads, `tokio`, `reqwest` default features, `std::fs`, or blocking I/O.
- **Memory and size.** Stay well under the 128 MB isolate limit: stream or limit bodies (inquiry bodies are capped at 16 KB), avoid large in-memory caches, and keep the release profile size-optimized.
- **Host testability.** Keep pure logic in host-testable modules (`crates/sws-security/src/headers.rs`, `packs/hospitality/crate/src/abc.rs`, `validate.rs`, `routes.rs`). Code that only works on wasm sits behind `#[cfg(target_arch = "wasm32")]`.
- **Error handling.** Never call `unwrap()` on user input. All D1 queries use bound parameters.

## 4. Dual security-header mandate

Cloudflare `_headers` applies **only to static assets**. Worker responses, meaning every `/api/*` route through `run_worker_first`, need the identical set from `crates/sws-security/src/headers.rs::with_security_headers`. That covers HSTS preload, nosniff, `X-Frame-Options: DENY`, `strict-origin-when-cross-origin`, `Permissions-Policy`, COOP, and a strict CSP with `frame-ancestors 'none'` that allowlists `https://challenges.cloudflare.com` for Turnstile.

Change a header in **both** files in the same commit (and regenerate `sites/*/public/_headers` with `npm run sites:build`). Parity is enforced by `cargo test` (`sws-security` parses `_headers`), by `npm run check:headers` and `npm run sites:check:headers` (static), by `npm run check:headers:live` (live), and by `tests/headers.spec.mjs`.

## 5. Gate 1 — 15-hour Rust/Wasm fallback

If cumulative time spent debugging the Rust/Wasm toolchain or runtime (worker-build, wasm-bindgen, workers-rs API gaps, D1 bindings) goes over **15 hours** in Phase 0/1, stop. Log the hours in `docs/ARCHITECTURE.md` under "Gate 1 log" and fall back to **Astro (static) + TypeScript/Hono on Workers**.

The fallback keeps the same IR/pack schema contracts, D1 schema, `_headers`, tests and CI gates. Don't sink more time into Rust past the ceiling; raise it with the user.

## 6. Accessibility — WCAG 2.2 Level AA (native semantics only)

- **Headings.** Exactly one `<h1>` per page, with sequential headings and no skipped levels.
- **Structure.** Use landmarks (`header`, `nav`, `main`, `footer`), a skip link and `lang`.
- **Contrast.** Text contrast is at least 4.5:1, or 3:1 for large text and UI components. The demo palette `#241207` on `#E9E2D2` is 13.97:1, which meets AAA.
- **Interaction.** Focus is always visible (`:focus-visible`) and never obscured. Targets are at least 24×24 CSS px.
- **Forms.** Use `<label>` and `aria-describedby` for errors and an `aria-live` status region. Forms must work without JS.
- **No accessibility overlay widgets.** That means no accessiBe, UserWay, AudioEye or similar.
- **Menus are semantic HTML plus `Menu`/`MenuSection`/`MenuItem` JSON-LD.** Never ship a PDF-only menu.

Gates are `npm run test:a11y` (axe, wcag22aa) and `npm run lhci` (accessibility = 1.0).

## 7. Virginia ABC advertising guardrails (`packs/hospitality`)

The linters in `packs/hospitality/tools/abc-lint.mjs` (Node) and `packs/hospitality/crate/src/abc.rs` (Rust) are advisory. They flag:

- "bottomless"
- "2-for-1" (and "2 for 1" or "2for1")
- "two for one" (and "two-for-one")
- "unlimited drinks"
- any happy-hour start or end time from 21:00 through 05:59 (the overnight window, midnight wrap included)

This covers 3 VAC 5-20-10 to -100 and 3 VAC 5-50-160. Re-verify the rules against current Virginia ABC regulations before client use. **A human signs off on every drink-related copy change.** This is not legal advice.

## 8. Other compliance

- **CAN-SPAM.** The newsletter needs an explicit, unchecked consent checkbox. `/api/newsletter` stores `consent_timestamp` and `source_route`. Never re-subscribe anyone marked `unsubscribed`.
- **Turnstile.** It is required on every public form. The test keys in `wrangler.jsonc` and `.dev.vars.example` are always-pass keys for dev only.
- **Secrets.** Keep them in `.dev.vars` (gitignored), `private/*.env` (gitignored, mode 600) or `wrangler secret put`. Never commit them and never print them.

## 9. Common commands

| Task | Command |
| --- | --- |
| Render demo fixture → `core/public` | `npm run render` (`npm run render:check` in CI) |
| Build static landing & docs sites | `npm run sites:build` (`npm run sites:check` in CI) |
| VA ABC lint | `npm run lint:abc` |
| Header parity (static / live) | `npm run check:headers` / `npm run check:headers:live` |
| Negative ABC fixture check | `npm run lint:abc:fixture-negative` (also inside `npm run verify`) |
| Local D1 migrate | `npm run db:migrate:local` |
| Local dev server (builds Worker) | `npm run dev` → http://127.0.0.1:8787 |
| Rust tests / wasm clippy | `cargo test --workspace --locked` / `cargo clippy --locked --target wasm32-unknown-unknown -p sws-security -p sws-worker-site -p sws-wasm -- -D warnings` |
| License gate | `cargo deny check` |
| a11y + header E2E | `npm run test:a11y` (set `BASE_URL` to reuse a running server) |
| Lighthouse | `npm run lhci` (needs a running server) |
| All fast gates | `npm run verify` |

**Gate scoping.** `npm run verify` is the fast contributor subset: it never needs a running server. The server-dependent gates — `check:headers:live`, `test:a11y` and `lhci` — run unconditionally in CI (the `web` job starts the dev server first) and are CI-only for contributors; run them locally against a dev server only when you want to.

Make sure `~/.cargo/bin` is on `PATH` so `wrangler dev` can find `worker-build`.

## 10. Git & GitHub workflow

- **Branches and commits.** Work on one isolated feature branch per change (`feat/…`, `fix/…`, `chore/…`). Use conventional commits that reference issues, e.g. `feat(sws-compiler): enforce heading hierarchy (fixes #12)`. All commits and release tags must be cryptographically signed (`git commit -S` / `commit.gpgsign=true`, verified with `git log --show-signature -1`).
- **Pull requests.** PRs include the verification log: `npm run verify`, plus `test:a11y` and `lhci` when UI changed.
- **Ask first.** `git push`, `wrangler deploy`, secrets and remote D1 writes need user confirmation.
- **Before committing,** run `npm run verify` and `git diff --stat`, and verify no real-client/Zone B data or secrets are staged.
