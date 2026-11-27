# Solo Website System (SWS)

**Solo Website System (SWS)** is a standalone, **Cloudflare-native, Rust-powered open-source visual website builder** (`MIT OR Apache-2.0`). It compiles a declarative intermediate representation (IR) into accessible, zero-runtime-overhead static sites served via **Cloudflare Workers with Static Assets**, backed by **D1** (SQLite), **Turnstile**, and lean progressive-enhancement JS islands.

SWS core is industry-agnostic and accessible by construction (**WCAG 2.2 AA**). Domain-specific verticals live in external or workspace packs — starting with **`packs/hospitality`** as the first open-source reference vertical pack (`MIT OR Apache-2.0`: menus, hours, Toast/OpenTable/Resy integrations, advisory Virginia ABC copy lint, and the fictional `shirlington-hearth-demo` fixture).

## Prerequisites (all free-tier compatible)

| Tool | Install / Setup |
| --- | --- |
| Node.js LTS (via nvm) | `nvm use --lts` (Node >= 20 LTS) |
| pnpm | `corepack enable && corepack prepare pnpm@latest --activate` (used across target monorepo workspaces) |
| Rust stable + wasm target | `rustup target add wasm32-unknown-unknown` (on Arch, the `rust` package ships the target without rustup) |
| `worker-build` | `cargo install worker-build@^0.9 --locked` (or `npm run setup:rust`) |
| `cargo-deny` | `cargo install cargo-deny --locked` |
| Playwright Chromium | `npx playwright install chromium` |

Make sure `~/.cargo/bin` is on your `PATH`; `wrangler dev` invokes `worker-build`.

## Quick start

```bash
nvm use --lts
npm ci
cp .dev.vars.example .dev.vars          # Turnstile always-pass TEST secret; Resend stubbed
npm run db:migrate:local                # applies crates/sws-worker-site/migrations to local D1 (.wrangler/)
npm run render                          # renders demo fixture -> core/public/{index.html,styles.css,...}
npm run dev                             # builds the Worker, serves http://127.0.0.1:8787
```

Try the edge Worker API locally:

```bash
curl -s http://127.0.0.1:8787/api/health
# {"service":"sws-worker-site","status":"ok","version":"0.1.0-dev"}

curl -s -H 'content-type: application/json' \
  -d '{"email":"guest@example.com","consent":true,"source_route":"/","cf-turnstile-response":"XXXX.DUMMY.TOKEN.XXXX"}' \
  http://127.0.0.1:8787/api/newsletter                   # 201 (400 without "consent": true)

curl -s --data-urlencode kind=private_event --data-urlencode name='Test Guest' \
  --data-urlencode email=guest@example.com --data-urlencode event_date=2027-02-14 \
  --data-urlencode guest_count=20 --data-urlencode message='Inquiry test' \
  --data-urlencode source_route=/book-an-event --data-urlencode cf-turnstile-response=XXXX.DUMMY.TOKEN.XXXX \
  http://127.0.0.1:8787/api/inquire                      # 201 {"ok":true,"id":1}

npx wrangler d1 execute DB --local --command "SELECT email,status,consent_timestamp,source_route FROM newsletter_subscribers"
```

## Quality gates

```bash
npm run verify               # render:check + pack lint + header parity + cargo test + cargo deny
cargo check --target wasm32-unknown-unknown -p sws-worker-site
BASE_URL=http://127.0.0.1:8787 npm run test:a11y     # axe WCAG 2.2 AA + headings/labels/overlays + header E2E
npm run check:headers:live   # static "/" vs Worker "/api/health" header parity
npm run lhci                 # Lighthouse CI (accessibility must be 1.0)
```

`npm run verify:full` additionally runs the agent tests and the Devvit bot tests (`verify:full (studio)`). **Node >= 23.6 is required for `verify:full`'s devvit tests**, which run `.ts` sources directly via `node --test` (unflagged type stripping); `npm run verify` itself works on Node >= 20.

CI (`.github/workflows/ci.yml`) runs the verification suite on every push and PR.

## Releasing (tag-only)

Merges to `main` run CI verification only and never trigger releases or production deploys. Releases and optional Cloudflare static-site deployments (`.github/workflows/release.yml`) run exclusively on pushed annotated tags:

```bash
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

For independent hospitality pack releases, push a `pack-hospitality-v*` tag (for example, `pack-hospitality-v0.1.0`). If `CLOUDFLARE_API_TOKEN` is configured in repository secrets, `v*` tag releases build and deploy the two static Workers Assets sites — `sws-landing` (`sites/landing/wrangler.jsonc`, custom domain `solo.jcusoftware.com`) and `sws-docs` (`sites/docs/wrangler.jsonc`, custom domain `docs.jcusoftware.com`) — with download links targeting the exact release tag. Both Wrangler configs declare active `custom_domain` `routes` on the `jcusoftware.com` Cloudflare zone alongside `workers_dev: true`; for forks or `*.workers.dev`-only preview deployments, remove or comment out `routes` in both `wrangler.jsonc` files and override the canonical URLs via optional GitHub repository variables (`SWS_LANDING_URL`, `SWS_DOCS_URL`, `SWS_SHOWCASE_URL`). If `CLOUDFLARE_API_TOKEN` is unset, Cloudflare deploys are skipped while the GitHub Release is still created.

## Target monorepo layout

SWS is structured around an open-source core engine (`crates/`, plus rendered output and tools under `core/`) and modular vertical packs (`packs/`); the planned v0.2/v0.3 monorepo layout adds `apps/`, `packages/` and `sws-worker-studio/`:

```
crates/                       Open-source SWS Rust engine (MIT OR Apache-2.0)
  sws-ir/                     Core page/component/token IR & schema validation
  sws-css/                    Pure-Rust token & layout CSS emitter
  sws-compiler/               Deterministic HTML + JSON-LD + asset compiler
  sws-plugin/                 Pack/plugin trait & manifest ABI
  sws-wasm/                   Browser Wasm bindings for live visual canvas & preview
  sws-cli/                    `solo` CLI & MCP server for local/CI workflows
  sws-worker-site/            Published-site edge Worker (/api/*, Turnstile, D1, security headers)
  sws-worker-studio/          planned v0.2.0 — optional Studio control-plane Worker (Durable Objects, D1; R2 opt-in)

apps/                         planned v0.3.0 — thin TypeScript Builder shell and sandboxed canvas (MIT OR Apache-2.0)
  builder/                    Visual website builder shell (properties, tokens, publish)
  canvas/                     Browser-based visual canvas powered by sws-wasm

packages/                     planned v0.2.0-alpha.1 onward — generated types and shared TypeScript packages (@solo-website-system/*, MIT OR Apache-2.0)

packs/                        Vertical plugins & industry schemas (open-source or external packs)
  hospitality/                Open-source reference pack (`MIT OR Apache-2.0`): M1–M10 modules,
                              semantic menus & hours, Toast/OpenTable/Resy integrations,
                              advisory VA ABC lint, and fictional `shirlington-hearth-demo` fixture

integrations/                 Standalone integrations (per-package licensing)
  devvit-release-bot/         Devvit subreddit release bot (BSD-3-Clause)

core/                         Static asset output & renderer CLI utilities
  public/                     Static assets (rendered output) + _headers
  tools/                      CLI rendering, ABC lint & header-check utilities
sites/                        Public static sites deployed on v* tags (MIT OR Apache-2.0)
  build.mjs & src/            Zero-dependency deterministic static site builder
  landing/                    `sws-landing` (solo.jcusoftware.com): landing page & CLI downloads
  docs/                       `sws-docs` (docs.jcusoftware.com): user & pack documentation
docs/                         ARCHITECTURE.md
tests/                        Playwright: a11y.spec.mjs, headers.spec.mjs
private/                      Gitignored Zone B (prospect dossiers, client exports, never committed)
```

## $0 free-tier Cloudflare architecture notes

- **Cloudflare free tier:** Workers (100k requests/day), Workers Static Assets, D1 (5 GB SQLite), Turnstile, DNS, and DMARC Management. (The optional v0.2.0 Studio control plane adds SQLite Durable Objects and is cardless by default; R2 is opt-in. Quota values are assumptions until verified; see `ROADMAP.md`.)
- **Resend:** optional free tier for transactional form notifications. Without `RESEND_API_KEY`, email dispatch is stubbed and logged.
- **Tooling:** GitHub Actions, Playwright, `axe-core`, and Lighthouse CI are open-source and free to run.

## Deploying (requires confirmation)

1. Run `npx wrangler d1 create sws-demo` and put the returned `database_id` into `wrangler.jsonc`.
2. Run `npx wrangler secret put TURNSTILE_SECRET_KEY` (a production key from the Cloudflare Turnstile dashboard), and `RESEND_API_KEY` if email forwarding is enabled.
3. Replace `TURNSTILE_SITE_KEY` with the real site key, then run `npx wrangler d1 migrations apply DB --remote` and `npx wrangler deploy`.

## Licensing

- **Dual open-source core, sites & reference pack (`MIT OR Apache-2.0`):** Core SWS Rust crates (`crates/*`, `core/`), builder applications (`apps/*`), `@solo-website-system/*` packages (`packages/*`), static sites (`sites/*`), and the reference hospitality pack (`packs/hospitality`) are dual-licensed under **MIT OR Apache-2.0**.
- **Integrations (`integrations/`, per-package licensing):** `integrations/devvit-release-bot` is licensed under **BSD-3-Clause** — tracked public code, but outside the core dual license (and never a Rust crate dependency, so `cargo deny check` is unaffected).
- **Rust dependency gate:** `cargo deny check` (`deny.toml`) allows only permissive OSI licenses (`MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-DFS-2016`, `CC0-1.0`) plus narrowly scoped `Unicode-3.0` exceptions. Copyleft and source-available licenses are rejected.
- **External packs & private workspace:** Third-party or external vertical packs may choose their own licenses. Private client/studio assets (`private/`, `.dev.vars`, `*.env`) are gitignored and never committed.
