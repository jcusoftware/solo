// Documentation site content generator (sws-docs -> https://docs.jcusoftware.com).

import { CLI_TARGETS, releaseTagUrl } from './config.mjs';
import { escapeHtml, renderDocsShell } from './layout.mjs';

export const DOC_PAGES = [
  {
    slug: 'overview',
    file: 'index.html',
    href: '/',
    navTitle: 'Overview',
    title: 'Solo Website System (SWS) Documentation',
    description:
      'Overview of Solo Website System (SWS): a Rust-powered, Cloudflare-native website builder with WCAG 2.2 AA accessibility and pluggable vertical packs.',
    renderBody: (config) => {
      const showcaseCta = config.showcaseUrl
        ? `<a class="btn btn-secondary" href="${escapeHtml(config.showcaseUrl)}">Open Live Showcase</a>`
        : `<a class="btn btn-secondary" href="/showcase/">Explore the Showcase</a>`;
      return `      <h1>Solo Website System Documentation</h1>
      <p class="docs-lead">Solo Website System (SWS) is an open-source, Rust-powered website builder and deterministic compiler built for <strong>Cloudflare Workers with Static Assets</strong>, <strong>D1</strong>, and <strong>Turnstile</strong>.</p>
      <div class="cta-row">
        <a class="btn btn-primary" href="/getting-started/">Get Started</a>
        <a class="btn btn-secondary" href="${escapeHtml(`${config.landingUrl}/#download`)}">Download solo CLI</a>
        ${showcaseCta}
      </div>

      <h2>Core Philosophy</h2>
      <p>SWS is designed to be a <strong>Rust app that makes Rust apps</strong>. Instead of running a heavy server-side CMS on every visitor request, SWS compiles a strongly typed site intermediate representation (<code>sws-ir</code>) into deterministic static HTML, CSS, and JSON-LD paired with a small Rust/Wasm edge Worker that only runs on <code>/api/*</code> routes.</p>
      <ul>
        <li><strong>Accessible by construction:</strong> Native WCAG 2.2 Level AA semantics enforced during compilation and verified with <code>axe-core</code> and Lighthouse CI — never third-party accessibility overlay widgets.</li>
        <li><strong>Dual security-header parity:</strong> Static assets (<code>_headers</code>) and Worker API responses (<code>with_security_headers</code>) emit byte-identical security headers.</li>
        <li><strong>$0 free-tier edge hosting:</strong> Built from day one to run cleanly on Cloudflare Workers free tier, D1 SQLite, and Turnstile without paid SaaS lock-in.</li>
        <li><strong>Clean vertical separation:</strong> Industry-agnostic core crates are dual-licensed under <code>MIT OR Apache-2.0</code>, while domain packs plug in through <code>sws-plugin</code>.</li>
      </ul>

      <h2>Three-Plane Architecture</h2>
      <pre tabindex="0"><code>PLANE 1: AUTHORING &amp; COMPILER (Local CLI / MCP + Browser Wasm)
  sws-ir       -&gt; Declarative pages, sections, tokens, routes, forms
  sws-css      -&gt; Pure-Rust design-token &amp; layout CSS emitter (no MPL/GPL)
  sws-compiler -&gt; Deterministic HTML, JSON-LD, sitemap &amp; WCAG checks
  sws-plugin   -&gt; Vertical pack ABI (e.g., packs/hospitality)
  sws-cli      -&gt; Unified CLI &amp; stdio MCP server

PLANE 2: CONTROL PLANE (v0.2.0 — Optional Studio Backend)
  sws-worker-studio -&gt; Cloudflare Workers + D1 + R2 + Queues + SQLite Durable Objects

PLANE 3: PUBLISHED EDGE SITE (Cloudflare Free-Tier Edge)
  Workers Static Assets -&gt; index.html, styles.css, robots.txt, sitemap.xml, _headers
  run_worker_first: ["/api/*"]
    -&gt; sws-worker-site (Rust -&gt; wasm32)
       ├── GET  /api/health
       ├── POST /api/inquire    (Turnstile -&gt; D1 -&gt; optional Resend)
       └── POST /api/newsletter (Turnstile -&gt; D1 with CAN-SPAM consent)</code></pre>

      <h2>Documentation Map</h2>
      <div class="grid grid-2">
        <div class="card">
          <h3><a href="/getting-started/">Getting Started</a></h3>
          <p>Install prerequisites, download or build SWS, run local D1 migrations, and preview your first site.</p>
        </div>
        <div class="card">
          <h3><a href="/user-guide/">User Guide</a></h3>
          <p>Author site data, work with deterministic rendering, progressive-enhancement forms, and quality gates.</p>
        </div>
        <div class="card">
          <h3><a href="/cli-and-mcp/">CLI &amp; MCP Reference</a></h3>
          <p>Command reference for <code>solo</code> (<code>init</code>, <code>build</code>, <code>check</code>, <code>export</code>, <code>mcp</code>) and local workspace tools.</p>
        </div>
        <div class="card">
          <h3><a href="/packs/">Vertical Packs</a></h3>
          <p>Understand the pack plugin model and explore <code>packs/hospitality</code>.</p>
        </div>
        <div class="card">
          <h3><a href="/showcase/">Live Showcase</a></h3>
          <p>Inspect the fictional <code>shirlington-hearth-demo</code> reference build and its accessibility and header guarantees.</p>
        </div>
        <div class="card">
          <h3><a href="/accessibility-and-security/">Accessibility &amp; Security</a></h3>
          <p>WCAG 2.2 AA requirements, dual security headers, Turnstile, D1 safety, and Rust/Wasm constraints.</p>
        </div>
        <div class="card">
          <h3><a href="/deployment/">Deployment</a></h3>
          <p>Deploy published sites and static docs/landing Workers to Cloudflare with tag-only releases and custom domains.</p>
        </div>
        <div class="card">
          <h3><a href="/faq/">FAQ &amp; Troubleshooting</a></h3>
          <p>Answers to common questions and fixes for local tooling, headers, Turnstile, and CI checks.</p>
        </div>
        <div class="card">
          <h3><a href="/roadmap-and-licensing/">Roadmap &amp; Licensing</a></h3>
          <p>Planned release milestones (<code>v0.1.0</code>–<code>v0.3.0</code>), dual <code>MIT OR Apache-2.0</code> core licensing, and dependency policy.</p>
        </div>
      </div>`;
    },
  },
  {
    slug: 'getting-started',
    file: 'getting-started/index.html',
    href: '/getting-started/',
    navTitle: 'Getting Started',
    title: 'Getting Started — SWS Documentation',
    description:
      'Prerequisites, downloading prebuilt solo CLI binaries, running the local Cloudflare Worker & D1 workspace, and verifying quality gates.',
    renderBody: (config) => `      <h1>Getting Started</h1>
      <p class="docs-lead">Get Solo Website System running locally in minutes using free-tier tools and zero paid cloud dependencies.</p>

      <h2>Prerequisites</h2>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">Tool</th>
              <th scope="col">Requirement</th>
              <th scope="col">Install / Setup</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Node.js LTS</td>
              <td><code>&gt;= 20</code></td>
              <td><code>nvm use --lts</code></td>
            </tr>
            <tr>
              <td>Rust stable + Wasm target</td>
              <td><code>wasm32-unknown-unknown</code></td>
              <td><code>rustup target add wasm32-unknown-unknown</code></td>
            </tr>
            <tr>
              <td><code>worker-build</code></td>
              <td><code>^0.9</code></td>
              <td><code>cargo install worker-build@^0.9 --locked</code></td>
            </tr>
            <tr>
              <td><code>cargo-deny</code></td>
              <td>Latest stable</td>
              <td><code>cargo install cargo-deny --locked</code></td>
            </tr>
            <tr>
              <td>Playwright Chromium</td>
              <td>For E2E &amp; axe gates</td>
              <td><code>npx playwright install chromium</code></td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="callout">
        <p class="callout-title">Ensure <code>~/.cargo/bin</code> is on your <code>PATH</code></p>
        <p><code>wrangler dev</code> invokes <code>worker-build</code> to compile the Rust Worker to WebAssembly. Add <code>export PATH="$HOME/.cargo/bin:$PATH"</code> to your shell profile if <code>worker-build</code> is not found.</p>
      </div>

      <h2>Option 1: Download a Prebuilt <code>solo</code> Binary</h2>
      <p>Tagged <code>v*</code> releases publish cross-compiled binaries for five targets (${CLI_TARGETS.map((t) => escapeHtml(t.label)).join(', ')}) along with a <code>SHA256SUMS.txt</code> manifest. Visit the <a href="${escapeHtml(`${config.landingUrl}/#download`)}">Download section</a> or <a href="${escapeHtml(releaseTagUrl(config))}">GitHub Releases</a>.</p>
      <pre tabindex="0"><code># Verify integrity before running a downloaded archive
sha256sum -c --ignore-missing SHA256SUMS.txt
tar -xzf solo-&lt;version&gt;-&lt;target&gt;.tar.gz
./solo --help</code></pre>

      <h2>Option 2: Run the Workspace from Source</h2>
      <pre tabindex="0"><code>git clone ${escapeHtml(config.githubUrl)}.git
cd ${escapeHtml(config.repoName)}
npm ci
cp .dev.vars.example .dev.vars
npm run db:migrate:local
npm run render
npm run dev</code></pre>
      <p>Once <code>wrangler dev</code> finishes building the Rust/Wasm Worker, open <code>http://127.0.0.1:8787</code> in your browser.</p>

      <h2>Smoke-Test the Local Edge API</h2>
      <p>The example <code>.dev.vars.example</code> uses Cloudflare&apos;s documented always-pass Turnstile test key for local development and stubs outbound Resend email delivery.</p>
      <pre tabindex="0"><code># 1. Health check
curl -s http://127.0.0.1:8787/api/health
# {"service":"sws-worker-site","status":"ok","version":"0.1.0-dev"}

# 2. Newsletter signup (requires explicit consent: true)
curl -s -H 'content-type: application/json' \\
  -d '{"email":"guest@example.com","consent":true,"source_route":"/","cf-turnstile-response":"XXXX.DUMMY.TOKEN.XXXX"}' \\
  http://127.0.0.1:8787/api/newsletter

# 3. Inspect local D1 rows
npx wrangler d1 execute DB --local \\
  --command "SELECT email, status, consent_timestamp, source_route FROM newsletter_subscribers"</code></pre>

      <h2>Run All Fast Quality Gates</h2>
      <pre tabindex="0"><code>npm run verify
cargo check --target wasm32-unknown-unknown -p sws-worker-site</code></pre>
      <p>With <code>npm run dev</code> running in another terminal, you can also run live header parity, Playwright axe-core WCAG 2.2 AA checks, and Lighthouse CI:</p>
      <pre tabindex="0"><code>npm run check:headers:live
BASE_URL=http://127.0.0.1:8787 npm run test:a11y
npm run lhci</code></pre>`,
  },
  {
    slug: 'user-guide',
    file: 'user-guide/index.html',
    href: '/user-guide/',
    navTitle: 'User Guide',
    title: 'User Guide — SWS Documentation',
    description:
      'Day-to-day workflow for authoring declarative site data, compiling deterministic static assets, managing progressive-enhancement forms, and running CI verification.',
    renderBody: () => `      <h1>User Guide</h1>
      <p class="docs-lead">Learn the end-to-end authoring, compilation, form-handling, and verification workflow for building sites with SWS.</p>

      <h2>Declarative Site Authoring</h2>
      <p>SWS separates structured content and design tokens from generated output. Never hand-edit generated files in <code>core/public/</code> or <code>sites/*/public/</code>; instead, update the structured source data and re-run the deterministic compiler.</p>
      <ul>
        <li><strong>Structured inputs:</strong> Core sites compile from declarative JSON IR (<code>sws-ir</code>), and vertical packs supply validated domain schemas (for example, <code>venue.json</code> in <code>packs/hospitality</code>).</li>
        <li><strong>Deterministic output:</strong> Running the renderer with identical inputs always produces byte-identical HTML, CSS, <code>robots.txt</code>, <code>sitemap.xml</code>, and <code>404.html</code>.</li>
        <li><strong>Drift detection in CI:</strong> Use <code>npm run render:check</code> (and <code>npm run sites:check</code> for the landing/docs sites) to assert that committed static output matches its source inputs without rewriting files.</li>
      </ul>

      <h2>Design Tokens &amp; Contrast Safety</h2>
      <p>SWS generates lean, self-hosted stylesheets with zero external font or CSS CDN dependencies. Every foreground/background pair is checked against WCAG 2.2 Level AA thresholds (at least <code>4.5:1</code> for body copy and <code>3:1</code> for large text and interactive UI borders).</p>
      <p>To keep static sites under strict Content Security Policy (<code>style-src 'self'</code>), avoid inline <code>style</code> attributes and inline <code>&lt;script&gt;</code> blocks.</p>

      <h2>Progressive-Enhancement Forms</h2>
      <p>Forms in SWS published sites keep native HTML form semantics without JavaScript, but successful submission requires it: the mandatory Cloudflare Turnstile verification makes the Worker fail closed with <code>400 turnstile_required</code> when JS is disabled.</p>
      <ol>
        <li><strong>Baseline HTML:</strong> Every form submits standard <code>application/x-www-form-urlencoded</code> POST requests to <code>/api/inquire</code> or <code>/api/newsletter</code>.</li>
        <li><strong>Accessible semantics:</strong> Inputs pair explicit <code>&lt;label for="..."&gt;</code> elements, <code>aria-describedby</code> error hints, and an <code>aria-live</code> status region for feedback.</li>
        <li><strong>Progressive JS island:</strong> A lightweight script enhances submissions with <code>fetch</code> and inline status updates without replacing native browser form semantics.</li>
        <li><strong>Server-side enforcement:</strong> The Rust edge Worker enforces a 16 KB streaming request body cap, honeypot screening, Cloudflare Turnstile verification, and bound-parameter D1 inserts.</li>
      </ol>

      <h3>CAN-SPAM Newsletter Consent</h3>
      <p>Newsletter forms render an explicit, unchecked consent checkbox by default. The Worker stamps <code>consent_timestamp</code> (ISO-8601 UTC) and <code>source_route</code> in D1, and refuses to silently re-subscribe addresses marked <code>unsubscribed</code> or <code>bounced</code>.</p>

      <h2>Workspace Scripts Reference</h2>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">Command</th>
              <th scope="col">Purpose</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>npm run render</code></td>
              <td>Render the showcase fixture into <code>core/public/</code></td>
            </tr>
            <tr>
              <td><code>npm run render:check</code></td>
              <td>Verify <code>core/public/</code> matches source inputs without writing</td>
            </tr>
            <tr>
              <td><code>npm run sites:build</code></td>
              <td>Build both static sites (<code>sites/landing/public</code> and <code>sites/docs/public</code>)</td>
            </tr>
            <tr>
              <td><code>npm run sites:check</code></td>
              <td>Verify static site output is up to date</td>
            </tr>
            <tr>
              <td><code>npm run check:headers</code></td>
              <td>Assert static parity between <code>_headers</code> and Rust <code>SECURITY_HEADERS</code></td>
            </tr>
            <tr>
              <td><code>npm run test:unit</code></td>
              <td>Run Node unit tests (<code>tests/unit/*.test.mjs</code>)</td>
            </tr>
            <tr>
              <td><code>npm run verify</code></td>
              <td>Run all fast checks (render, sites, lint, header parity, Rust tests, <code>cargo deny</code>)</td>
            </tr>
            <tr>
              <td><code>npm run test:a11y</code></td>
              <td>Run Playwright WCAG 2.2 AA (axe-core) and header E2E specs</td>
            </tr>
            <tr>
              <td><code>npm run lhci</code></td>
              <td>Run Lighthouse CI (requires accessibility score <code>1.0</code>)</td>
            </tr>
          </tbody>
        </table>
      </div>`,
  },
  {
    slug: 'cli-and-mcp',
    file: 'cli-and-mcp/index.html',
    href: '/cli-and-mcp/',
    navTitle: 'CLI & MCP Reference',
    title: 'CLI & MCP Reference — SWS Documentation',
    description:
      'Command-line reference for the Rust solo binary (init, build, check, export, mcp) and its stdio JSON-RPC 2.0 MCP server.',
    renderBody: () => `      <h1>CLI &amp; MCP Reference</h1>
      <p class="docs-lead">Solo provides a unified Rust CLI (<code>solo</code>) and local <code>stdio</code> Model Context Protocol (MCP) server for deterministic builds, validation, export, and AI-assisted editing.</p>

      <div class="callout">
        <p class="callout-title">Workspace tooling note</p>
        <p>The pure-Rust <code>sws-cli</code> crate builds the <code>solo</code> binary (<code>cargo run -p sws-cli -- ...</code>). Equivalent workspace npm scripts are also available via <code>npm run render</code>, <code>npm run check:headers</code>, <code>npm run lint:abc</code>, and <code>npm run sites:build</code>.</p>
      </div>

      <h2><code>solo</code> Commands</h2>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">Command</th>
              <th scope="col">Description</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>solo init &lt;dir&gt; [--pack &lt;name&gt;]</code></td>
              <td>Scaffold a starter Solo site project directory, optionally preconfigured for a vertical pack such as <code>hospitality</code>.</td>
            </tr>
            <tr>
              <td><code>solo build &lt;input&gt; &lt;outdir&gt; [--check]</code></td>
              <td>Compile a <code>SiteDocument</code> IR or pack manifest into deterministic static HTML, CSS, JSON-LD, <code>sitemap.xml</code>, <code>robots.txt</code>, and <code>_headers</code>. Pass <code>--check</code> in CI to verify files byte-for-byte without writing.</td>
            </tr>
            <tr>
              <td><code>solo check &lt;input&gt; [--headers-file &lt;path&gt;] [--json]</code></td>
              <td>Validate IR/pack schemas, run compiler WCAG 2.2 AA and plugin lint passes, and optionally check <code>_headers</code> parity against Rust <code>SECURITY_HEADERS</code>.</td>
            </tr>
            <tr>
              <td><code>solo export &lt;input&gt; &lt;outdir&gt;</code></td>
              <td>Export a deployable Cloudflare Workers + Static Assets + D1 bundle (<code>public/</code>, <code>wrangler.jsonc</code>, and D1 SQL migrations).</td>
            </tr>
            <tr>
              <td><code>solo mcp</code></td>
              <td>Start the local stdio JSON-RPC 2.0 MCP server for editor and AI-assistant integrations.</td>
            </tr>
          </tbody>
        </table>
      </div>

      <h3>Example CLI Workflow</h3>
      <pre tabindex="0"><code># Initialize a starter site
solo init ./my-site

# Validate IR, accessibility rules, and header parity (JSON report for CI)
solo check ./my-site/site.json --headers-file ./my-site/public/_headers --json

# Compile deterministic static output
solo build ./my-site/site.json ./my-site/public

# Export a Cloudflare-ready bundle
solo export ./my-site/site.json ./dist-bundle</code></pre>

      <h2>Stdio MCP Server (<code>solo mcp</code>)</h2>
      <p>The built-in MCP server communicates over standard input/output using JSON-RPC 2.0 so coding agents can inspect and patch site IR deterministically without hand-editing rendered HTML.</p>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">MCP Tool</th>
              <th scope="col">Purpose</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>read_site_ir</code></td>
              <td>Load and validate a site IR or pack JSON file and return its normalized structure.</td>
            </tr>
            <tr>
              <td><code>apply_ir_patch</code></td>
              <td>Apply structured patches to site IR while enforcing schema invariants.</td>
            </tr>
            <tr>
              <td><code>compile_preview</code></td>
              <td>Compile the current IR in memory and return preview diagnostics and artifact summaries.</td>
            </tr>
            <tr>
              <td><code>run_a11y_and_plugin_checks</code></td>
              <td>Run compiler accessibility checks and active vertical pack linters on the site IR.</td>
            </tr>
          </tbody>
        </table>
      </div>

      <h3>Local MCP Client Configuration</h3>
      <pre tabindex="0"><code>{
  "mcpServers": {
    "solo": {
      "command": "solo",
      "args": ["mcp"]
    }
  }
}</code></pre>`,
  },
  {
    slug: 'packs',
    file: 'packs/index.html',
    href: '/packs/',
    navTitle: 'Packs',
    title: 'Vertical Packs — SWS Documentation',
    description:
      'How SWS vertical packs extend the open-source core engine via sws-plugin, with a deep dive on packs/hospitality.',
    renderBody: () => `      <h1>Vertical Packs</h1>
      <p class="docs-lead">SWS core stays strictly industry-agnostic under <code>MIT OR Apache-2.0</code>. Domain schemas, templates, third-party integrations, and compliance linters live in modular packs under <code>packs/*</code>.</p>

      <h2>How the Pack Architecture Works</h2>
      <ul>
        <li><strong>One-way dependency rule:</strong> Packs may depend on SWS core crates (<code>sws-ir</code>, <code>sws-compiler</code>, <code>sws-plugin</code>), while core crates never depend on any vertical pack.</li>
        <li><strong>Clean plugin ABI:</strong> Packs register IR transformers, domain validators, semantic HTML/JSON-LD renderers, and post-compile advisory linters via <code>sws-plugin</code>.</li>
        <li><strong>Independent release tracks:</strong> Core releases publish on <code>v*</code> tags; vertical packs can publish independently (for example, <code>pack-hospitality-v*</code>).</li>
        <li><strong>Flexible licensing:</strong> Core engine crates are always dual-licensed <code>MIT OR Apache-2.0</code>, while individual vertical packs may be open-source or commercially licensed.</li>
      </ul>

      <h2>First Vertical Pack: <code>packs/hospitality</code></h2>
      <p><code>packs/hospitality</code> is the open-source reference vertical pack (<code>MIT OR Apache-2.0</code>) built for independent restaurants, pubs, and hospitality venues.</p>

      <h3>Module Map (M1–M10)</h3>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">ID</th>
              <th scope="col">Module</th>
              <th scope="col">Key Capabilities</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>M1</td>
              <td>Core Shell &amp; NAP</td>
              <td>Consistent Name/Address/Phone, hours, <code>LocalBusiness</code> JSON-LD</td>
            </tr>
            <tr>
              <td>M2</td>
              <td>Semantic Menus</td>
              <td>Accessible HTML menus + <code>Menu</code> / <code>MenuSection</code> / <code>MenuItem</code> JSON-LD (never PDF-only)</td>
            </tr>
            <tr>
              <td>M3</td>
              <td>Private Events Inquiry</td>
              <td>Structured inquiry form to <code>POST /api/inquire</code> stored in D1</td>
            </tr>
            <tr>
              <td>M4</td>
              <td>Contact</td>
              <td>General contact form backed by Turnstile + D1</td>
            </tr>
            <tr>
              <td>M5</td>
              <td>Newsletter + Consent</td>
              <td>CAN-SPAM explicit unchecked opt-in stored with UTC consent timestamp</td>
            </tr>
            <tr>
              <td>M6</td>
              <td>Events Calendar</td>
              <td>Scheduled events schema and <code>Event</code> JSON-LD</td>
            </tr>
            <tr>
              <td>M7</td>
              <td>Visit</td>
              <td>Regular hours, holiday overrides, directions, and venue accessibility details</td>
            </tr>
            <tr>
              <td>M8</td>
              <td>About / Story</td>
              <td>Editorial story sections with optimized responsive media</td>
            </tr>
            <tr>
              <td>M9</td>
              <td>SEO &amp; Discovery</td>
              <td><code>sitemap.xml</code>, <code>robots.txt</code>, and NAP consistency checks</td>
            </tr>
            <tr>
              <td>M10</td>
              <td>Security &amp; A11y Gate</td>
              <td>Dual security headers, <code>/api/health</code>, axe-core and Lighthouse CI gates</td>
            </tr>
          </tbody>
        </table>
      </div>

      <h3>Pluggable Hospitality Integrations</h3>
      <p>Hospitality integrations prefer plain outbound links over heavy third-party script embeds to preserve CSP strictness, page performance, and keyboard accessibility:</p>
      <ul>
        <li><strong>Online ordering:</strong> Optional Toast link-out (no third-party API secrets required for static link-outs).</li>
        <li><strong>Seating &amp; reservations:</strong> Configurable <code>walk_in_only</code>, <code>opentable</code>, or <code>resy</code> reservation CTAs.</li>
        <li><strong>Turnstile:</strong> Per-venue site key with Cloudflare&apos;s dummy always-pass key reserved strictly for local development.</li>
      </ul>

      <h3>Advisory Virginia ABC Copy Linter</h3>
      <p><code>packs/hospitality</code> includes an advisory copy linter (implemented in both Rust and Node with parity tests) covering Virginia ABC advertising guidelines (3 VAC 5-20-10 through -100 and 3 VAC 5-50-160). It flags restricted phrases such as <code>"bottomless"</code>, <code>"2-for-1"</code>, <code>"two for one"</code>, <code>"unlimited drinks"</code>, and happy-hour start or end times from <code>21:00</code> through <code>05:59</code> (the overnight window, midnight wrap included).</p>
      <div class="callout">
        <p class="callout-title">Advisory only — not legal advice</p>
        <p>Automated copy linting assists with catching common issues early, but a human must always review and sign off on alcohol-related copy against current local regulations before publication.</p>
      </div>

      <h2>Building Your Own Pack</h2>
      <p>To create a new vertical pack for another industry (such as clinics, studios, nonprofits, or retail):</p>
      <ol>
        <li>Create a dedicated directory under <code>packs/&lt;your-pack&gt;/</code> with its own JSON Schema and Rust crate.</li>
        <li>Keep all domain logic host-testable under <code>cargo test</code> and strictly compliant with <code>cargo deny check</code>.</li>
        <li>Use 100% fictional showcase fixtures (invented street names, <code>555-01xx</code> phone numbers, and <code>.example</code> domains) so public repos never contain real third-party trademarks.</li>
      </ol>`,
  },
  {
    slug: 'showcase',
    file: 'showcase/index.html',
    href: '/showcase/',
    navTitle: 'Live Showcase',
    title: 'Live Showcase — SWS Documentation',
    description:
      'Explore the fictional shirlington-hearth-demo showcase built with Solo Website System and packs/hospitality.',
    renderBody: (config) => {
      const liveBlock = config.showcaseUrl
        ? `<div class="callout">
        <p class="callout-title"><span class="badge-ok">Live Demo Online</span> Inspect a live deployed SWS showcase</p>
        <p>Explore the deployed fictional <code>shirlington-hearth-demo</code> site running on Cloudflare Workers with Static Assets and D1.</p>
        <div class="cta-row">
          <a class="btn btn-primary" href="${escapeHtml(config.showcaseUrl)}">Open Live Showcase</a>
          <a class="btn btn-secondary" href="${escapeHtml(config.githubUrl)}">Inspect Source on GitHub</a>
        </div>
      </div>`
        : `<div class="callout">
        <p class="callout-title"><span class="badge-note">Local &amp; Deployable Demo</span> Live showcase URL not yet configured</p>
        <p>You can run the full interactive <code>shirlington-hearth-demo</code> showcase locally in under two minutes using the commands below, or set the <code>SWS_SHOWCASE_URL</code> repository variable once a public showcase Worker is deployed so this page links directly to it.</p>
      </div>`;

      return `      <h1>Live Showcase: <code>shirlington-hearth-demo</code></h1>
      <p class="docs-lead">SWS ships a complete, 100% fictional reference showcase demonstrating the core Rust edge Worker, deterministic static HTML/CSS compilation, and the hospitality vertical pack.</p>

      ${liveBlock}

      <h2>Why the Showcase Is 100% Fictional</h2>
      <p>Public open-source fixtures in SWS strictly follow a two-zone IP and trademark firewall. <code>shirlington-hearth-demo</code> uses invented street addresses (Hearthstone Lane), RFC-style <code>555-01xx</code> phone numbers, and <code>.example</code> domains so zero real third-party business names, logos, menus, or trademarks appear in the public repository.</p>

      <h2>What to Inspect in the Showcase</h2>
      <div class="grid grid-2">
        <div class="card">
          <h3>WCAG 2.2 AA &amp; AAA Contrast</h3>
          <p>The demo palette delivers <code>13.97:1</code> body text contrast (exceeding WCAG AAA), a single <code>&lt;h1&gt;</code>, sequential headings, keyboard-reachable skip links, and zero accessibility overlay widgets.</p>
        </div>
        <div class="card">
          <h3>Semantic HTML Menus &amp; JSON-LD</h3>
          <p>Menu items render as real semantic HTML paired with <code>Menu</code>, <code>MenuSection</code>, and <code>MenuItem</code> structured data — never trapped inside an inaccessible PDF.</p>
        </div>
        <div class="card">
          <h3>Progressive-Enhancement Forms</h3>
          <p>Newsletter and inquiry forms include explicit <code>&lt;label&gt;</code> bindings, <code>aria-describedby</code> errors, an unchecked CAN-SPAM consent box, and full server-side D1 persistence.</p>
        </div>
        <div class="card">
          <h3>Live Dual Security Headers</h3>
          <p>Compare HTTP response headers on <code>/</code> (Cloudflare Static Assets via <code>_headers</code>) and <code>/api/health</code> (Rust Worker via <code>with_security_headers</code>) — they are byte-identical.</p>
        </div>
      </div>

      <h2>Run the Showcase Locally</h2>
      <pre tabindex="0"><code>git clone ${escapeHtml(config.githubUrl)}.git
cd ${escapeHtml(config.repoName)}
npm ci
cp .dev.vars.example .dev.vars
npm run db:migrate:local
npm run render
npm run dev</code></pre>
      <p>Then open <code>http://127.0.0.1:8787</code> and run the live verification checks in another terminal:</p>
      <pre tabindex="0"><code>npm run check:headers:live
BASE_URL=http://127.0.0.1:8787 npm run test:a11y
npm run lhci</code></pre>`;
    },
  },
  {
    slug: 'accessibility-and-security',
    file: 'accessibility-and-security/index.html',
    href: '/accessibility-and-security/',
    navTitle: 'Accessibility & Security',
    title: 'Accessibility & Security — SWS Documentation',
    description:
      'WCAG 2.2 Level AA native semantic standards, dual security-header parity, Turnstile and D1 safeguards, and Rust/Wasm engineering constraints.',
    renderBody: () => `      <h1>Accessibility &amp; Security</h1>
      <p class="docs-lead">Accessibility and edge security are first-class compiler and CI gates in Solo Website System — not optional plugins bolted on after launch.</p>

      <h2>WCAG 2.2 Level AA (Native Semantics Only)</h2>
      <ul>
        <li><strong>Heading structure:</strong> Exactly one <code>&lt;h1&gt;</code> per page, starting the outline, with sequential heading levels and no skipped levels.</li>
        <li><strong>Landmarks &amp; navigation:</strong> Every page includes <code>&lt;html lang="..."&gt;</code>, <code>&lt;header&gt;</code>, <code>&lt;nav&gt;</code>, <code>&lt;main id="main" tabindex="-1"&gt;</code>, <code>&lt;footer&gt;</code>, and a keyboard-visible skip link.</li>
        <li><strong>Color contrast:</strong> Minimum <code>4.5:1</code> for normal text and <code>3:1</code> for large text and UI components.</li>
        <li><strong>Focus &amp; targets:</strong> Unobscured <code>:focus-visible</code> outlines and interactive targets at least <code>24×24</code> CSS pixels.</li>
        <li><strong>Forms:</strong> Explicit <code>&lt;label&gt;</code> associations, <code>aria-describedby</code> validation messages, and <code>aria-live</code> status announcements. All content and navigation works with zero JS; public form submission is the exception — the mandatory Turnstile verification requires JavaScript, so a no-JS submission fails closed with <code>400 turnstile_required</code>.</li>
        <li><strong>Zero overlay widgets:</strong> Third-party accessibility overlays are explicitly banned and detected by automated Playwright checks.</li>
      </ul>

      <h2>Dual Security-Header Contract</h2>
      <p>Cloudflare&apos;s <code>_headers</code> file applies <strong>only to static assets</strong>. Every dynamic <code>/api/*</code> response handled by the Rust edge Worker must apply the exact same header set via <code>with_security_headers</code>.</p>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">Header</th>
              <th scope="col">Enforced Value</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>Strict-Transport-Security</code></td>
              <td><code>max-age=63072000; includeSubDomains; preload</code></td>
            </tr>
            <tr>
              <td><code>X-Content-Type-Options</code></td>
              <td><code>nosniff</code></td>
            </tr>
            <tr>
              <td><code>X-Frame-Options</code></td>
              <td><code>DENY</code></td>
            </tr>
            <tr>
              <td><code>Referrer-Policy</code></td>
              <td><code>strict-origin-when-cross-origin</code></td>
            </tr>
            <tr>
              <td><code>Permissions-Policy</code></td>
              <td>Disables accelerometer, camera, geolocation, gyroscope, magnetometer, microphone, payment, usb, browsing-topics</td>
            </tr>
            <tr>
              <td><code>Cross-Origin-Opener-Policy</code></td>
              <td><code>same-origin</code></td>
            </tr>
            <tr>
              <td><code>Content-Security-Policy</code></td>
              <td><code>default-src 'self'</code>, Turnstile allowlisted on <code>https://challenges.cloudflare.com</code>, <code>style-src 'self'</code>, <code>object-src 'none'</code>, <code>frame-ancestors 'none'</code>, <code>upgrade-insecure-requests</code></td>
            </tr>
          </tbody>
        </table>
      </div>
      <p>Four independent checks enforce header parity: Rust unit tests (<code>cargo test</code>), static Node parity (<code>npm run check:headers</code>), live parity (<code>npm run check:headers:live</code>), and Playwright E2E (<code>tests/headers.spec.mjs</code>).</p>

      <h2>Edge Worker &amp; Data Safety</h2>
      <ul>
        <li><strong>Request body limits:</strong> Streaming body reads capped at 16 KB to protect Worker memory and stay far under the 128 MB isolate limit.</li>
        <li><strong>Bound D1 SQL parameters:</strong> Every Cloudflare D1 query uses bound parameters; user input is never interpolated into SQL strings.</li>
        <li><strong>No panics on untrusted input:</strong> Worker handlers avoid <code>unwrap()</code> on request payloads and return structured JSON error responses.</li>
        <li><strong>Cloudflare Turnstile:</strong> Required on public form endpoints. Production secrets live strictly in Cloudflare Worker secrets (<code>wrangler secret put</code>) and never in Git.</li>
        <li><strong>Wasm-safe Rust dependencies:</strong> No native OpenSSL, C-assembly <code>ring</code>, OS threads, <code>tokio</code>, or blocking filesystem I/O in Wasm crates.</li>
      </ul>

      <h2>Reporting Security Issues</h2>
      <p>Please report potential security vulnerabilities privately according to <code>SECURITY.md</code> in the repository rather than opening a public GitHub issue.</p>`,
  },
  {
    slug: 'deployment',
    file: 'deployment/index.html',
    href: '/deployment/',
    navTitle: 'Deployment',
    title: 'Deployment — SWS Documentation',
    description:
      'Deploy SWS sites and the sws-landing and sws-docs static Workers to Cloudflare free tier with tag-only releases and custom domains.',
    renderBody: (config) => `      <h1>Deployment</h1>
      <p class="docs-lead">SWS targets Cloudflare&apos;s free tier for both published dynamic-form sites and pure static Workers such as the landing and documentation sites.</p>

      <h2>Cloudflare Free-Tier Footprint</h2>
      <ul>
        <li><strong>Cloudflare Workers &amp; Static Assets:</strong> Free tier covers static asset delivery and up to 100,000 Worker requests/day.</li>
        <li><strong>Cloudflare D1:</strong> Free tier provides 5 GB SQLite storage for form submissions and newsletter subscribers.</li>
        <li><strong>Cloudflare Turnstile &amp; DNS:</strong> Free bot protection, DNS hosting, and DMARC management.</li>
        <li><strong>Resend (optional):</strong> Free tier for transactional form email notifications; when <code>RESEND_API_KEY</code> is unset, email dispatch logs cleanly as a stub.</li>
      </ul>

      <h2>Deploying a Published SWS Site (Worker + D1)</h2>
      <ol>
        <li>Create your D1 database and copy the returned <code>database_id</code> into <code>wrangler.jsonc</code>:
          <pre tabindex="0"><code>npx wrangler d1 create my-site-db</code></pre>
        </li>
        <li>Configure production secrets (never commit secrets or <code>.dev.vars</code>):
          <pre tabindex="0"><code>npx wrangler secret put TURNSTILE_SECRET_KEY
# Optional email forwarding:
npx wrangler secret put RESEND_API_KEY</code></pre>
        </li>
        <li>Replace the test <code>TURNSTILE_SITE_KEY</code> in <code>wrangler.jsonc</code> with your production Turnstile site key.</li>
        <li>Apply remote D1 migrations and deploy:
          <pre tabindex="0"><code>npx wrangler d1 migrations apply DB --remote
npx wrangler deploy</code></pre>
        </li>
      </ol>

      <h2>Deploying the Landing &amp; Docs Static Sites</h2>
      <p>This repository ships two zero-Worker-code Cloudflare Static Assets sites:</p>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">Site</th>
              <th scope="col">Worker Name</th>
              <th scope="col">Wrangler Config</th>
              <th scope="col">Custom Domain</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Landing &amp; Downloads</td>
              <td><code>sws-landing</code></td>
              <td><code>sites/landing/wrangler.jsonc</code></td>
              <td><code>solo.jcusoftware.com</code></td>
            </tr>
            <tr>
              <td>Documentation</td>
              <td><code>sws-docs</code></td>
              <td><code>sites/docs/wrangler.jsonc</code></td>
              <td><code>docs.jcusoftware.com</code></td>
            </tr>
          </tbody>
        </table>
      </div>

      <h3>Tag-Only Automated Releases</h3>
      <p>Merges to <code>main</code> run CI verification only and never publish releases or deploy to Cloudflare. Releases run exclusively when you push an annotated Git tag:</p>
      <pre tabindex="0"><code>git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0</code></pre>
      <p>On every <code>v*</code> tag, <code>.github/workflows/release.yml</code> verifies Rust tests, <code>cargo deny</code>, Wasm clippy, static sites, and header parity, builds CLI binaries (when <code>crates/sws-cli</code> is present), creates the GitHub Release, and — when <code>CLOUDFLARE_API_TOKEN</code> is configured — builds and deploys both <code>sws-landing</code> and <code>sws-docs</code> with links pointing directly to the tagged release.</p>

      <h3>GitHub Repository Secrets &amp; Variables</h3>
      <div class="table-wrap" tabindex="0">
        <table>
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Kind</th>
              <th scope="col">Default / Purpose</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>CLOUDFLARE_API_TOKEN</code></td>
              <td>Secret</td>
              <td>Enables Cloudflare deploys on <code>v*</code> tags (skipped safely if unset)</td>
            </tr>
            <tr>
              <td><code>CLOUDFLARE_ACCOUNT_ID</code></td>
              <td>Secret</td>
              <td>Target Cloudflare account ID for Wrangler deploys</td>
            </tr>
            <tr>
              <td><code>SWS_LANDING_URL</code></td>
              <td>Variable (optional)</td>
              <td>Defaults to <code>https://solo.jcusoftware.com</code>; override for forks or <code>*.workers.dev</code> preview deployments</td>
            </tr>
            <tr>
              <td><code>SWS_DOCS_URL</code></td>
              <td>Variable (optional)</td>
              <td>Defaults to <code>https://docs.jcusoftware.com</code>; override for forks or <code>*.workers.dev</code> preview deployments</td>
            </tr>
            <tr>
              <td><code>SWS_SHOWCASE_URL</code></td>
              <td>Variable (optional)</td>
              <td>When set, adds direct "Open Live Showcase" links across both sites</td>
            </tr>
          </tbody>
        </table>
      </div>

      <h3>Custom Domains &amp; Cloudflare DNS</h3>
      <p>Both <code>sites/landing/wrangler.jsonc</code> and <code>sites/docs/wrangler.jsonc</code> declare Cloudflare Custom Domain <code>routes</code> (<code>solo.jcusoftware.com</code> and <code>docs.jcusoftware.com</code>) alongside <code>workers_dev: true</code>:</p>
      <ol>
        <li>Ensure <code>jcusoftware.com</code> is an active zone in your target Cloudflare account so Wrangler can bind <code>solo.jcusoftware.com</code> (<code>sws-landing</code>) and <code>docs.jcusoftware.com</code> (<code>sws-docs</code>) and provision DNS + TLS automatically on deploy.</li>
        <li>Grant your <code>CLOUDFLARE_API_TOKEN</code> Workers Scripts Edit plus Zone DNS / Workers Routes permissions on <code>jcusoftware.com</code>.</li>
        <li>For forks or preview deployments without a custom domain yet, remove or comment out the <code>routes</code> blocks in both <code>wrangler.jsonc</code> files and set <code>SWS_LANDING_URL</code> / <code>SWS_DOCS_URL</code> to your <code>*.workers.dev</code> URLs.</li>
      </ol>
      <p>Current build URLs: landing <code>${escapeHtml(config.landingUrl)}</code>, docs <code>${escapeHtml(config.docsUrl)}</code>.</p>`,
  },
  {
    slug: 'faq',
    file: 'faq/index.html',
    href: '/faq/',
    navTitle: 'FAQ & Troubleshooting',
    title: 'FAQ & Troubleshooting — SWS Documentation',
    description:
      'Frequently asked questions and troubleshooting solutions for Rust/Wasm builds, Wrangler, cargo-deny, Turnstile, security headers, and releases.',
    renderBody: (config) => `      <h1>FAQ &amp; Troubleshooting</h1>
      <p class="docs-lead">Common questions about Solo Website System and quick fixes for local development, CI, and deployment issues.</p>

      <h2>Frequently Asked Questions</h2>

      <h3>Why build a website builder in Rust for Cloudflare Workers?</h3>
      <p>Compiling site IR in Rust produces deterministic, sub-millisecond static builds, allows the same compiler crates (<code>sws-ir</code>, <code>sws-css</code>, <code>sws-compiler</code>) to run natively in the CLI and inside browsers via WebAssembly, and compiles edge form handlers to tiny, memory-safe <code>wasm32</code> Cloudflare Workers.</p>

      <h3>How much does hosting an SWS site cost?</h3>
      <p>Small and medium sites run for <strong>$0/month</strong> on Cloudflare&apos;s free tier (Workers Static Assets, 100k Worker requests/day, 5 GB D1 SQLite, Turnstile, and DNS). Paid Cloudflare Workers plans are only needed if your traffic or storage exceeds free-tier limits.</p>

      <h3>Can I use SWS without buying a custom domain yet?</h3>
      <p>Yes. Every Wrangler configuration enables <code>workers_dev: true</code> so sites deploy immediately to <code>*.workers.dev</code>. The SWS landing and docs configs bind <code>solo.jcusoftware.com</code> and <code>docs.jcusoftware.com</code> by default; for forks or preview environments without a custom domain, comment out the <code>routes</code> blocks in both <code>wrangler.jsonc</code> files and set <code>SWS_LANDING_URL</code> and <code>SWS_DOCS_URL</code> GitHub repository variables to your <code>*.workers.dev</code> URLs.</p>

      <h3>Why are accessibility overlay widgets banned?</h3>
      <p>Third-party overlay scripts do not repair broken native HTML semantics, often introduce keyboard or screen-reader conflicts, and expand your CSP attack surface. SWS enforces WCAG 2.2 Level AA natively in HTML/CSS and verifies it automatically with <code>axe-core</code> and Lighthouse.</p>

      <h3>Why aren&apos;t prebuilt CLI binaries attached to an early release tag?</h3>
      <p>The tag release workflow checks whether <code>crates/sws-cli/Cargo.toml</code> exists on that tag. Tags cut before <code>sws-cli</code> lands on <code>main</code> publish as verified source releases without broken download links; once <code>sws-cli</code> is present on a tagged commit, all five cross-compiled platform archives and <code>SHA256SUMS.txt</code> attach automatically.</p>

      <h2>Troubleshooting</h2>

      <h3><code>worker-build</code> not found during <code>npm run dev</code> or <code>npm run build</code></h3>
      <p>Install <code>worker-build</code> and make sure Cargo&apos;s bin directory is in your shell <code>PATH</code>:</p>
      <pre tabindex="0"><code>cargo install worker-build@^0.9 --locked
export PATH="$HOME/.cargo/bin:$PATH"
which worker-build</code></pre>

      <h3><code>POST /api/newsletter</code> or <code>/api/inquire</code> returns HTTP 503 locally</h3>
      <p>The Worker refuses to process public form submissions when <code>TURNSTILE_SECRET_KEY</code> is missing unless <code>DEV_MODE="true"</code> is set. Copy the example local vars file before running <code>npm run dev</code>:</p>
      <pre tabindex="0"><code>cp .dev.vars.example .dev.vars</code></pre>

      <h3><code>cargo deny check</code> fails on a new Rust dependency</h3>
      <p>SWS enforces a strict permissive OSI license allowlist (<code>MIT</code>, <code>Apache-2.0</code>, <code>BSD-2-Clause</code>, <code>BSD-3-Clause</code>, <code>ISC</code>, <code>Unicode-DFS-2016</code>, <code>CC0-1.0</code>). Copyleft and source-available crates (including MPL-2.0 crates such as <code>lightningcss</code> or <code>cssparser</code>) are rejected intentionally. Replace the dependency with a permissively licensed alternative.</p>

      <h3><code>npm run check:headers</code> or <code>cargo test</code> fails with a header mismatch</h3>
      <p>Any security-header update must be applied to <code>crates/sws-security/src/headers.rs</code> and <code>core/public/_headers</code> in the same commit, and static sites must be regenerated so <code>sites/*/public/_headers</code> stay synchronized:</p>
      <pre tabindex="0"><code>npm run sites:build
npm run check:headers
npm run sites:check:headers
cargo test --workspace</code></pre>

      <h3><code>npm run render:check</code> or <code>npm run sites:check</code> reports drift in CI</h3>
      <p>Source data or templates changed without regenerating the committed static output. Regenerate locally and commit the updated files:</p>
      <pre tabindex="0"><code>npm run render
npm run sites:build
npm run verify</code></pre>

      <h3>Playwright fails because Chromium is missing</h3>
      <pre tabindex="0"><code>npx playwright install --with-deps chromium</code></pre>

      <h3>Still stuck?</h3>
      <p>Open a bug report or question on <a href="${escapeHtml(`${config.githubUrl}/issues`)}">GitHub Issues</a> with your OS, Rust/Node versions, and the exact command output.</p>`,
  },
  {
    slug: 'roadmap-and-licensing',
    file: 'roadmap-and-licensing/index.html',
    href: '/roadmap-and-licensing/',
    navTitle: 'Roadmap & Licensing',
    title: 'Roadmap & Licensing — SWS Documentation',
    description:
      'Solo Website System release milestones (v0.1.0, v0.2.0, v0.3.0), dual MIT OR Apache-2.0 core licensing, and dependency policy.',
    renderBody: (config) => `      <h1>Roadmap &amp; Licensing</h1>
      <p class="docs-lead">Planned milestones for Solo Website System and the permissive open-source licensing model that governs the core engine.</p>

      <h2>Release Roadmap</h2>

      <h3><code>v0.1.0</code> — Core Rust Engine, CLI/MCP &amp; Vertical Pack Foundation</h3>
      <ul>
        <li><code>sws-ir</code>: Strongly typed, industry-agnostic Site IR (pages, sections, blocks, design tokens, routes, JSON-LD).</li>
        <li><code>sws-css</code>: Pure-Rust, permissively licensed CSS AST, validator, contrast checker, and emitter.</li>
        <li><code>sws-compiler</code>: Deterministic HTML/CSS compiler with built-in WCAG 2.2 Level AA structural enforcement.</li>
        <li><code>sws-plugin</code>: Stable plugin ABI for vertical IR transforms, renderers, and post-compile lint passes.</li>
        <li><code>sws-cli</code> &amp; <code>sws-wasm</code>: Unified CLI, local stdio MCP server, and browser Wasm compiler bridge.</li>
        <li><code>packs/hospitality</code>: First vertical pack cleanly isolated from open-source core crates.</li>
      </ul>

      <h3><code>v0.2.0</code> — Cloudflare Control Plane &amp; Real-Time Sync</h3>
      <ul>
        <li><code>sws-worker-studio</code>: Multi-tenant Cloudflare Workers control plane with dual security-header parity and Turnstile protection.</li>
        <li><strong>D1 + R2 + Queues:</strong> Relational project/session metadata, immutable build &amp; media storage, and async publish pipelines.</li>
        <li><strong>SQLite Durable Objects:</strong> <code>ProjectRoom</code> for collaborative real-time editing and <code>RateLimiter</code> for edge rate limiting.</li>
      </ul>

      <h3><code>v0.3.0</code> — Pure-Rust Visual Builder &amp; Standalone Rust App Export</h3>
      <ul>
        <li><strong>Pure-Rust Studio Frontend:</strong> Browser-based Rust/Wasm visual builder shell, live canvas, and workspace console compiling directly against <code>sws-ir</code>, <code>sws-css</code>, and <code>sws-compiler</code>.</li>
        <li><strong>Standalone Rust App Export (<code>solo export</code>):</strong> Emits self-contained Rust Cloudflare Worker applications (<code>Cargo.toml</code>, typed handlers, D1 migrations, static assets, and <code>wrangler.jsonc</code>).</li>
      </ul>

      <h2>Licensing</h2>
      <ul>
        <li><strong>Open-source core, sites &amp; reference pack (<code>MIT OR Apache-2.0</code>):</strong> Core Rust crates (<code>crates/*</code>, <code>core/</code>), builder applications (<code>apps/*</code>), core packages (<code>packages/*</code>), static sites (<code>sites/*</code>), and the reference hospitality pack (<code>packs/hospitality</code>) are dual-licensed under the MIT License and Apache License 2.0.</li>
        <li><strong>Dependency hygiene:</strong> <code>cargo deny check</code> enforces a strict permissive OSI license allowlist (<code>MIT</code>, <code>Apache-2.0</code>, <code>BSD-2-Clause</code>, <code>BSD-3-Clause</code>, <code>ISC</code>, <code>Unicode-DFS-2016</code>, <code>CC0-1.0</code>, plus narrowly scoped <code>Unicode-3.0</code> entries for ICU/unicode crates). GPL, AGPL, LGPL, MPL, and source-available licenses are rejected.</li>
        <li><strong>External vertical packs:</strong> Third-party or external industry packs are decoupled behind <code>sws-plugin</code> and may be distributed on independent release tracks under their own licenses.</li>
        <li><strong>Integrations (<code>integrations/</code>, per-package licensing):</strong> <code>integrations/devvit-release-bot</code> is licensed under <code>BSD-3-Clause</code> — tracked public code, but outside the core dual license (and never a Rust crate dependency, so <code>cargo deny check</code> is unaffected).</li>
      </ul>

      <h2>Contributing</h2>
      <p>Contributions to the open-source core are welcome! Review <code>CONTRIBUTING.md</code> and <code>CODE_OF_CONDUCT.md</code> on <a href="${escapeHtml(config.githubUrl)}">GitHub</a> before opening a pull request.</p>`,
  },
];

export function renderDocsPages(config) {
  const navItems = DOC_PAGES.map(({ slug, href, navTitle }) => ({ slug, href, navTitle }));
  const files = new Map();

  for (let i = 0; i < DOC_PAGES.length; i += 1) {
    const page = DOC_PAGES[i];
    const prev = i > 0 ? navItems[i - 1] : null;
    const next = i + 1 < DOC_PAGES.length ? navItems[i + 1] : null;

    const jsonLd = {
      '@context': 'https://schema.org',
      '@type': 'TechArticle',
      headline: page.title,
      description: page.description,
      url: `${config.docsUrl}${page.href}`,
      isPartOf: {
        '@type': 'WebSite',
        name: 'Solo Website System Documentation',
        url: `${config.docsUrl}/`,
      },
    };

    const html = renderDocsShell({
      title: page.title,
      description: page.description,
      canonicalPath: page.href,
      activeSlug: page.slug,
      navItems,
      prev,
      next,
      config,
      jsonLd,
      body: page.renderBody(config),
    });
    files.set(page.file, html);
  }

  return files;
}

export function renderDocs404(config) {
  const navItems = DOC_PAGES.map(({ slug, href, navTitle }) => ({ slug, href, navTitle }));
  const body = `      <h1>Page Not Found</h1>
      <p class="docs-lead">The documentation page you requested could not be found. Choose a guide from the sidebar or return to the Documentation Overview.</p>
      <div class="cta-row">
        <a class="btn btn-primary" href="/">Documentation Overview</a>
        <a class="btn btn-secondary" href="/getting-started/">Getting Started</a>
        <a class="btn btn-secondary" href="/faq/">FAQ &amp; Troubleshooting</a>
        <a class="btn btn-secondary" href="${escapeHtml(`${config.landingUrl}/`)}">Back to Landing Page</a>
      </div>`;

  return renderDocsShell({
    title: 'Page Not Found — SWS Documentation',
    description: 'The requested documentation page was not found.',
    activeSlug: '',
    navItems,
    noindex: true,
    config,
    body,
  });
}
