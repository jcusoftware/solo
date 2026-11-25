// Landing site content generator (sws-landing -> https://solo.jcusoftware.com).

import {
  CLI_TARGETS,
  cliAssetFilename,
  releaseAssetUrl,
  releaseTagUrl,
} from './config.mjs';
import { escapeHtml, renderLandingShell } from './layout.mjs';

function renderDownloadSection(config) {
  const isTaggedRelease = /^v\d/.test(config.version);
  const directBinariesReady = config.hasCli && isTaggedRelease;
  const tagUrl = releaseTagUrl(config);
  const checksumsUrl = directBinariesReady ? releaseAssetUrl(config, 'SHA256SUMS.txt') : tagUrl;

  const statusBanner = directBinariesReady
    ? `<div class="callout">
        <p class="callout-title"><span class="badge-ok">Release ${escapeHtml(config.version)}</span> Prebuilt <code>solo</code> CLI binaries available</p>
        <p>Every archive below is built from a verified Git tag in GitHub Actions and published with a matching <a href="${escapeHtml(checksumsUrl)}"><code>SHA256SUMS.txt</code></a> checksum manifest. See the <a href="${escapeHtml(tagUrl)}">GitHub Release notes</a> for changelog details.</p>
      </div>`
    : `<div class="callout">
        <p class="callout-title"><span class="badge-note">${isTaggedRelease ? `Tag ${escapeHtml(config.version)} · Source Release` : 'Preview Channel'}</span> Prebuilt CLI binaries publish automatically on tagged releases containing <code>sws-cli</code></p>
        <p>Browse published tags and source archives on <a href="${escapeHtml(tagUrl)}">GitHub Releases</a>, or run the workspace locally from source while <code>solo</code> binary builds are finalized for upcoming <code>v*</code> tags.</p>
      </div>`;

  const rows = CLI_TARGETS.map((item) => {
    const filename = isTaggedRelease
      ? cliAssetFilename(config.version, item)
      : `solo-&lt;version&gt;-${item.target}.${item.ext}`;
    const actionCell = directBinariesReady
      ? `<a class="download-link" href="${escapeHtml(releaseAssetUrl(config, cliAssetFilename(config.version, item)))}" aria-label="${escapeHtml(`Download .${item.ext} for ${item.label}`)}">Download .${escapeHtml(item.ext)}</a>`
      : `<a href="${escapeHtml(tagUrl)}" aria-label="${escapeHtml(`View on GitHub Releases for ${item.label}`)}">View on GitHub Releases</a>`;

    return `            <tr>
              <td><strong>${escapeHtml(item.label)}</strong></td>
              <td><code>${escapeHtml(item.target)}</code></td>
              <td><code>${isTaggedRelease ? escapeHtml(filename) : filename}</code></td>
              <td>${actionCell}</td>
            </tr>`;
  }).join('\n');

  const verifySnippet = directBinariesReady
    ? `# Download archive and checksums for ${config.version}
curl -fsSLO "${releaseAssetUrl(config, cliAssetFilename(config.version, CLI_TARGETS[0]))}"
curl -fsSLO "${checksumsUrl}"
sha256sum -c --ignore-missing SHA256SUMS.txt

# Extract and verify the binary
tar -xzf "${cliAssetFilename(config.version, CLI_TARGETS[0])}"
./solo --help`
    : `# Clone and run locally from source (Node >= 20 LTS + Rust stable + wasm32)
git clone ${config.githubUrl}.git
cd ${config.repoName}
npm ci
cp .dev.vars.example .dev.vars
npm run db:migrate:local
npm run render
npm run dev`;

  return `    <section id="download" class="section section-alt">
      <div class="container">
        <h2>Download the <code>solo</code> CLI</h2>
        <p class="section-intro">Cross-compiled, single-binary Rust releases for Linux, macOS, and Windows — built exclusively from signed Git tags with SHA-256 checksums.</p>
        ${statusBanner}
        <div class="table-wrap" tabindex="0">
          <table>
            <thead>
              <tr>
                <th scope="col">Platform</th>
                <th scope="col">Rust Target Triple</th>
                <th scope="col">Release Archive</th>
                <th scope="col">Download</th>
              </tr>
            </thead>
            <tbody>
${rows}
            </tbody>
          </table>
        </div>
        <h3>${directBinariesReady ? 'Verify Checksum &amp; Run' : 'Run Locally from Source'}</h3>
        <pre tabindex="0"><code>${escapeHtml(verifySnippet)}</code></pre>
        <p>Need full installation prerequisites or Cloudflare setup instructions? Read the <a href="${escapeHtml(`${config.docsUrl}/getting-started/`)}">Getting Started guide</a> or browse <a href="${escapeHtml(config.releasesUrl)}">all GitHub Releases</a>.</p>
      </div>
    </section>`;
}

export function renderLandingIndex(config) {
  const showcaseHref = config.showcaseUrl || `${config.docsUrl}/showcase/`;
  const showcaseButtonText = config.showcaseUrl ? 'Open Live Showcase' : 'Explore the Showcase';

  const jsonLd = {
    '@context': 'https://schema.org',
    '@type': 'SoftwareApplication',
    name: 'Solo Website System (SWS)',
    applicationCategory: 'DeveloperApplication',
    operatingSystem: 'Linux, macOS, Windows',
    url: `${config.landingUrl}/`,
    downloadUrl: releaseTagUrl(config),
    softwareVersion: config.version,
    license: 'https://spdx.org/licenses/MIT.html',
    codeRepository: config.githubUrl,
    description:
      'Cloudflare-native, Rust-powered open-source website builder and deterministic compiler with WCAG 2.2 AA enforcement and dual security headers.',
  };

  const body = `    <section class="hero">
      <div class="container">
        <p class="eyebrow">Open Source · MIT OR Apache-2.0 · Rust + Cloudflare Workers</p>
        <h1>A Rust website builder that compiles fast, accessible Rust apps for Cloudflare Workers.</h1>
        <p class="hero-lead">Solo Website System (SWS) turns declarative site IR into deterministic, WCAG 2.2 Level AA static websites and lean Rust/Wasm edge Workers — with byte-identical security headers, progressive-enhancement forms, and $0 free-tier Cloudflare hosting.</p>
        <div class="cta-row">
          <a class="btn btn-primary" href="#download">Download solo CLI</a>
          <a class="btn btn-secondary" href="${escapeHtml(`${config.docsUrl}/getting-started/`)}">Read the Docs</a>
          <a class="btn btn-secondary" href="${escapeHtml(showcaseHref)}">${escapeHtml(showcaseButtonText)}</a>
        </div>
      </div>
    </section>

    <section class="section" aria-label="Core guarantees">
      <div class="container">
        <h2>Engineered for Speed, Accessibility &amp; Ownership</h2>
        <div class="grid grid-4">
          <div class="card">
            <p class="metric-title">100% Rust Core</p>
            <p class="metric-desc">Typed IR, pure-Rust CSS emission (zero copyleft/MPL crates), deterministic HTML compiler, CLI, and <code>wasm32</code> edge Workers.</p>
          </div>
          <div class="card">
            <p class="metric-title">WCAG 2.2 Level AA</p>
            <p class="metric-desc">Native semantics, verified color contrast, single <code>&lt;h1&gt;</code> hierarchy, and zero third-party accessibility overlay widgets.</p>
          </div>
          <div class="card">
            <p class="metric-title">Dual Security Headers</p>
            <p class="metric-desc">Static <code>_headers</code> and Rust Worker middleware stay byte-identical — verified by unit tests, CI, and Playwright.</p>
          </div>
          <div class="card">
            <p class="metric-title">$0 Free-Tier Edge</p>
            <p class="metric-desc">Runs on Cloudflare Workers Static Assets, D1 SQLite, and Turnstile without paid SaaS CMS lock-in.</p>
          </div>
        </div>
      </div>
    </section>

${renderDownloadSection(config)}

    <section id="architecture" class="section">
      <div class="container">
        <h2>Three-Plane Cloudflare-Native Architecture</h2>
        <p class="section-intro">SWS separates authoring, optional collaborative control, and ultra-lean published edge sites so visitor traffic never pays runtime CMS overhead.</p>
        <div class="grid grid-3">
          <article class="card">
            <h3>1. Build &amp; Authoring Plane</h3>
            <p>Author sites declaratively with <code>sws-ir</code>, pure-Rust <code>sws-css</code>, and deterministic <code>sws-compiler</code>. Use the unified <code>solo</code> CLI and local stdio MCP server for CI and AI-assisted editing.</p>
          </article>
          <article class="card">
            <h3>2. Edge Published-Site Plane</h3>
            <p>Published HTML, CSS, and JSON-LD serve straight from Cloudflare Workers Static Assets. Only <code>/api/*</code> routes invoke the Rust/Wasm Worker for Turnstile verification, D1 storage, and optional notifications.</p>
          </article>
          <article class="card">
            <h3>3. Studio &amp; Rust Export</h3>
            <p>Upcoming milestones add <code>sws-worker-studio</code> (D1, R2, Queues, SQLite Durable Objects) and a pure-Rust/Wasm visual canvas with <code>solo export</code> emitting standalone Rust Cloudflare Worker apps.</p>
          </article>
        </div>
        <p class="section-footer-link"><a href="${escapeHtml(`${config.docsUrl}/`)}">Explore the full architecture documentation</a> or check the <a href="${escapeHtml(`${config.docsUrl}/roadmap-and-licensing/`)}">product roadmap</a>.</p>
      </div>
    </section>

    <section id="packs" class="section section-alt">
      <div class="container">
        <h2>Industry-Agnostic Core, Pluggable Vertical Packs</h2>
        <p class="section-intro">Core crates stay clean and domain-neutral under <code>MIT OR Apache-2.0</code>. Domain schemas, semantic templates, integrations, and compliance linters plug in through <code>sws-plugin</code>.</p>
        <div class="grid grid-2">
          <article class="card">
            <h3>First Vertical Pack: <code>packs/hospitality</code></h3>
            <p>Built for restaurants, pubs, and venues with semantic HTML menus + <code>Menu</code> JSON-LD (never PDF-only), structured hours and holiday overrides, private-event and contact inquiry workflows, CAN-SPAM newsletter consent, link-out integrations (Toast, OpenTable, Resy), and an advisory Virginia ABC copy linter.</p>
            <p><a href="${escapeHtml(`${config.docsUrl}/packs/`)}">Read the Packs documentation</a></p>
          </article>
          <article class="card">
            <h3>Live Reference Showcase</h3>
            <p>Inspect the 100% fictional <code>shirlington-hearth-demo</code> showcase demonstrating WCAG 2.2 AA native semantics, <code>13.97:1</code> AAA text contrast, progressive-enhancement forms, and live static-vs-Worker header parity.</p>
            <div class="cta-row">
              <a class="btn btn-primary" href="${escapeHtml(showcaseHref)}">${escapeHtml(showcaseButtonText)}</a>
              ${
                config.showcaseUrl
                  ? `<a class="btn btn-secondary" href="${escapeHtml(`${config.docsUrl}/showcase/`)}">Showcase Guide</a>`
                  : `<a class="btn btn-secondary" href="${escapeHtml(config.githubUrl)}">Inspect Source on GitHub</a>`
              }
            </div>
          </article>
        </div>
      </div>
    </section>

    <section class="section">
      <div class="container">
        <h2>Ready to Build?</h2>
        <p class="section-intro">Start locally in minutes with zero paid services, or browse the guides covering CLI commands, packs, accessibility, security, deployment, and troubleshooting.</p>
        <div class="cta-row">
          <a class="btn btn-primary" href="${escapeHtml(`${config.docsUrl}/getting-started/`)}">Getting Started</a>
          <a class="btn btn-secondary" href="${escapeHtml(`${config.docsUrl}/user-guide/`)}">User Guide</a>
          <a class="btn btn-secondary" href="${escapeHtml(`${config.docsUrl}/cli-and-mcp/`)}">CLI &amp; MCP Reference</a>
          <a class="btn btn-secondary" href="${escapeHtml(`${config.docsUrl}/faq/`)}">FAQ &amp; Troubleshooting</a>
        </div>
      </div>
    </section>`;

  return renderLandingShell({
    title: 'Solo Website System (SWS) — Rust Website Builder for Cloudflare Workers',
    description:
      'Open-source, Rust-powered website builder and deterministic compiler for Cloudflare Workers with WCAG 2.2 AA accessibility and dual security headers.',
    canonicalPath: '/',
    config,
    jsonLd,
    body,
  });
}

export function renderLanding404(config) {
  const body = `    <section class="hero">
      <div class="container">
        <p class="eyebrow">HTTP 404</p>
        <h1>Page Not Found</h1>
        <p class="hero-lead">The page you requested does not exist on Solo Website System. Head back to the landing page or open the documentation.</p>
        <div class="cta-row">
          <a class="btn btn-primary" href="/">Return Home</a>
          <a class="btn btn-secondary" href="${escapeHtml(`${config.docsUrl}/`)}">Open Documentation</a>
          <a class="btn btn-secondary" href="${escapeHtml(`${config.docsUrl}/faq/`)}">FAQ &amp; Troubleshooting</a>
        </div>
      </div>
    </section>`;

  return renderLandingShell({
    title: 'Page Not Found — Solo Website System (SWS)',
    description: 'The requested page was not found on Solo Website System.',
    noindex: true,
    config,
    body,
  });
}
