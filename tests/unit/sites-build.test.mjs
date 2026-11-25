import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, stat, unlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { describe, it } from 'node:test';

import { compare, parseHeadersFile } from '../../core/tools/check-headers.mjs';
import {
  buildSiteMaps,
  loadSecurityHeaders,
  main,
  syncDirectory,
} from '../../sites/build.mjs';
import { safeJsonLd } from '../../sites/src/layout.mjs';
import {
  CLI_TARGETS,
  cliAssetFilename,
  DEFAULT_DOCS_URL,
  DEFAULT_GITHUB_REPO,
  DEFAULT_LANDING_URL,
  normalizeConfig,
  parseCliArgs,
} from '../../sites/src/config.mjs';
import { DOC_PAGES } from '../../sites/src/docs.mjs';

function stripJsoncComments(src) {
  return src.replace(/^\s*\/\/.*$/gm, '');
}

function extractHeadings(html) {
  return [...html.matchAll(/<h([1-6])\b/gi)].map((m) => Number(m[1]));
}

describe('sites/src/config validation & CLI parsing', () => {
  it('uses solo.jcusoftware.com and docs.jcusoftware.com as defaults', () => {
    const cfg = normalizeConfig();
    assert.equal(cfg.landingUrl, DEFAULT_LANDING_URL);
    assert.equal(cfg.docsUrl, DEFAULT_DOCS_URL);
    assert.equal(cfg.landingUrl, 'https://solo.jcusoftware.com');
    assert.equal(cfg.docsUrl, 'https://docs.jcusoftware.com');
    assert.equal(cfg.repo, DEFAULT_GITHUB_REPO);
    assert.equal(cfg.repo, 'jcusoftware/solo');
    assert.equal(cfg.hasCli, false);
    assert.equal(cfg.showcaseUrl, '');
  });

  it('normalizes trailing slashes, case, default ports, and parses CLI flags & env vars', () => {
    const cfg = parseCliArgs(
      [
        '--version',
        'v0.1.0',
        '--has-cli',
        'true',
        '--repo',
        'solo-website-system/sws',
        '--landing-url',
        'https://SOLO.JCUSOFTWARE.COM:443///',
        '--docs-url',
        'https://DOCS.JCUSOFTWARE.COM/',
        '--showcase-url',
        'https://demo.jcusoftware.com/',
        '--check',
      ],
      {},
    );
    assert.equal(cfg.version, 'v0.1.0');
    assert.equal(cfg.hasCli, true);
    assert.equal(cfg.repo, 'solo-website-system/sws');
    assert.equal(cfg.landingUrl, 'https://solo.jcusoftware.com');
    assert.equal(cfg.docsUrl, 'https://docs.jcusoftware.com');
    assert.equal(cfg.showcaseUrl, 'https://demo.jcusoftware.com');
    assert.equal(cfg.check, true);
  });

  it('rejects javascript: URLs, credentials, query/hash/path origins, bad paths, bad repos/versions, and unknown flags', () => {
    assert.throws(() => normalizeConfig({ landingUrl: 'javascript:alert(1)' }));
    assert.throws(() => normalizeConfig({ docsUrl: 'https://docs.example\n/evil' }));
    assert.throws(() => normalizeConfig({ landingUrl: 'https://solo.jcusoftware.com?foo=1' }));
    assert.throws(() => normalizeConfig({ landingUrl: 'https://solo.jcusoftware.com?' }));
    assert.throws(() => normalizeConfig({ docsUrl: 'https://docs.jcusoftware.com#top' }));
    assert.throws(() => normalizeConfig({ docsUrl: 'https://docs.jcusoftware.com#' }));
    assert.throws(() => normalizeConfig({ landingUrl: 'https://solo.jcusoftware.com/subpath' }));
    assert.throws(() => normalizeConfig({ docsUrl: 'https://docs.jcusoftware.com/getting-started/' }));
    assert.throws(() => normalizeConfig({ landingUrl: 'https://user:pass@solo.jcusoftware.com' }));
    assert.throws(() => normalizeConfig({ showcaseUrl: 'https://user:pass@demo.jcusoftware.com' }));
    assert.throws(() => normalizeConfig({ showcaseUrl: 'ftp://example.com' }));
    assert.throws(() => normalizeConfig({ outLanding: '' }));
    assert.throws(() => normalizeConfig({ outLanding: '.' }));
    assert.throws(() => normalizeConfig({ outDocs: '/' }));
    assert.throws(() => normalizeConfig({ outLanding: '/tmp/sws-same', outDocs: '/tmp/sws-same' }));
    assert.throws(() => normalizeConfig({ outLanding: '/tmp/sws-parent', outDocs: '/tmp/sws-parent/nested' }));
    assert.throws(() => normalizeConfig({ repo: 'invalid-repo-slug' }));
    assert.throws(() => normalizeConfig({ version: 'v1.0.0; rm -rf /' }));
    assert.throws(() => normalizeConfig({ hasCli: 'maybe' }));
    assert.throws(() => parseCliArgs(['--unknown-flag'], {}));
    assert.throws(() => parseCliArgs(['--version'], {}));
    assert.throws(() => parseCliArgs(['--docs-url', '--check'], {}));
  });

  it('falls back cleanly when env vars are empty or whitespace-only', () => {
    const cfg = parseCliArgs([], {
      SWS_RELEASE_VERSION: '   ',
      SWS_HAS_CLI: '   ',
      SWS_GITHUB_REPO: ' ',
      SWS_LANDING_URL: '   ',
      SWS_DOCS_URL: '\t',
      SWS_SHOWCASE_URL: '   ',
    });
    assert.equal(cfg.version, 'dev');
    assert.equal(cfg.hasCli, false);
    assert.equal(cfg.landingUrl, DEFAULT_LANDING_URL);
    assert.equal(cfg.docsUrl, DEFAULT_DOCS_URL);
    assert.equal(cfg.showcaseUrl, '');
  });
});

describe('sites/build static site generation', async () => {
  const securityHeaders = await loadSecurityHeaders();

  it('renders direct binary download links + SHA256SUMS when hasCli=true on a v* tag', () => {
    const { landing } = buildSiteMaps(
      {
        version: 'v0.1.0',
        hasCli: true,
        repo: 'solo-website-system/sws',
      },
      securityHeaders,
    );
    const html = landing.get('index.html');
    for (const target of CLI_TARGETS) {
      const filename = cliAssetFilename('v0.1.0', target);
      const expectedHref = `https://github.com/solo-website-system/sws/releases/download/v0.1.0/${filename}`;
      assert.ok(html.includes(expectedHref), `Missing download link: ${expectedHref}`);
      assert.ok(
        html.includes(`aria-label="Download .${target.ext} for ${target.label}"`),
        `Missing distinct accessible name for ${target.label}`,
      );
    }
    assert.ok(
      html.includes('https://github.com/solo-website-system/sws/releases/download/v0.1.0/SHA256SUMS.txt'),
      'Missing SHA256SUMS.txt link',
    );
    assert.ok(html.includes('sha256sum -c --ignore-missing SHA256SUMS.txt'));
  });

  it('avoids broken /releases/download/ links when hasCli=false or version=dev and derives repo dir name', () => {
    const preCliTag = buildSiteMaps(
      { version: 'v0.1.0', hasCli: false, repo: 'solo-website-system/sws' },
      securityHeaders,
    );
    const preCliHtml = preCliTag.landing.get('index.html');
    assert.ok(!preCliHtml.includes('/releases/download/'), 'Should not emit direct binary links when hasCli=false');
    assert.ok(preCliHtml.includes('/releases/tag/v0.1.0'), 'Should link to the GitHub Release tag page');
    assert.ok(preCliHtml.includes('cd sws\n'), 'Clone snippet should match repo directory name');
    assert.ok(
      preCliTag.docs.get('getting-started/index.html').includes('cd sws\n'),
      'Docs getting-started clone snippet should match repo directory name',
    );
    assert.ok(
      preCliTag.docs.get('showcase/index.html').includes('cd sws\n'),
      'Docs showcase clone snippet should match repo directory name',
    );
    for (const target of CLI_TARGETS) {
      assert.ok(
        preCliHtml.includes(`aria-label="View on GitHub Releases for ${target.label}"`),
        `Missing distinct fallback accessible name for ${target.label}`,
      );
    }

    const devBuild = buildSiteMaps({ version: 'dev', hasCli: true }, securityHeaders);
    const devHtml = devBuild.landing.get('index.html');
    assert.ok(!devHtml.includes('/releases/download/'), 'Should not emit direct binary links for dev version');
  });

  it('wires optional live showcase URL across landing and docs sites without duplicate adjacent CTAs', () => {
    const withoutLive = buildSiteMaps({ showcaseUrl: '' }, securityHeaders);
    const landingNoLive = withoutLive.landing.get('index.html');
    assert.ok(landingNoLive.includes('https://docs.jcusoftware.com/showcase/'));
    assert.ok(
      !landingNoLive.includes('>Showcase Guide<'),
      'Should not render duplicate Showcase Guide button pointing to same URL when showcaseUrl is unset',
    );
    assert.ok(withoutLive.docs.get('showcase/index.html').includes('SWS_SHOWCASE_URL'));

    const withLive = buildSiteMaps(
      { showcaseUrl: 'https://demo.jcusoftware.com' },
      securityHeaders,
    );
    assert.ok(withLive.landing.get('index.html').includes('href="https://demo.jcusoftware.com"'));
    assert.ok(withLive.landing.get('index.html').includes('>Showcase Guide<'));
    assert.ok(withLive.docs.get('showcase/index.html').includes('href="https://demo.jcusoftware.com"'));
    assert.ok(withLive.docs.get('index.html').includes('href="https://demo.jcusoftware.com"'));
  });

  it('generates all 10 docs pages including Live Showcase and FAQ & Troubleshooting', () => {
    const { landing, docs } = buildSiteMaps({}, securityHeaders);
    const expectedSlugs = [
      'overview',
      'getting-started',
      'user-guide',
      'cli-and-mcp',
      'packs',
      'showcase',
      'accessibility-and-security',
      'deployment',
      'faq',
      'roadmap-and-licensing',
    ];
    assert.deepEqual(
      DOC_PAGES.map((p) => p.slug),
      expectedSlugs,
    );
    assert.ok(
      landing.get('index.html').includes('<link rel="canonical" href="https://solo.jcusoftware.com/">'),
    );
    assert.ok(landing.get('sitemap.xml').includes('<loc>https://solo.jcusoftware.com/</loc>'));
    assert.ok(landing.get('robots.txt').includes('Sitemap: https://solo.jcusoftware.com/sitemap.xml'));
    const overviewHtml = docs.get('index.html');
    const sitemap = docs.get('sitemap.xml');
    for (const page of DOC_PAGES) {
      assert.ok(docs.has(page.file), `Missing docs file ${page.file}`);
      const pageHtml = docs.get(page.file);
      assert.ok(
        pageHtml.includes(`<link rel="canonical" href="https://docs.jcusoftware.com${page.href}">`),
        `Missing canonical link on docs/${page.file}`,
      );
      assert.ok(
        pageHtml.includes(`"url": "https://docs.jcusoftware.com${page.href}"`),
        `Missing JSON-LD url on docs/${page.file}`,
      );
      assert.ok(
        sitemap.includes(`<loc>https://docs.jcusoftware.com${page.href}</loc>`),
        `Missing sitemap entry for ${page.href}`,
      );
      if (page.slug !== 'overview') {
        assert.ok(
          overviewHtml.includes(`<h3><a href="${page.href}">`),
          `Docs Overview Documentation Map is missing card for ${page.href}`,
        );
      }
    }
    for (const [label, html] of [
      ['landing/404.html', landing.get('404.html')],
      ['docs/404.html', docs.get('404.html')],
    ]) {
      assert.ok(html.includes('<meta name="robots" content="noindex">'), `${label}: missing noindex`);
      assert.ok(!html.includes('rel="canonical"'), `${label}: 404 page should not emit canonical`);
    }
    const allowedHosts = new Set([
      'solo.jcusoftware.com',
      'docs.jcusoftware.com',
      'github.com',
      'schema.org',
      'spdx.org',
      'challenges.cloudflare.com',
      'www.sitemaps.org',
      '127.0.0.1',
    ]);
    for (const [label, content] of [...landing.entries(), ...docs.entries()]) {
      for (const match of content.matchAll(/https?:\/\/([A-Za-z0-9.-]+)/g)) {
        assert.ok(
          allowedHosts.has(match[1].toLowerCase()),
          `${label}: unexpected external/legacy host "${match[1]}"`,
        );
      }
    }
    assert.ok(docs.get('robots.txt').includes('Sitemap: https://docs.jcusoftware.com/sitemap.xml'));
  });

  it('enforces WCAG structural invariants, strict CSP hygiene, valid JSON-LD, and zero sensitive leaks', () => {
    const { landing, docs } = buildSiteMaps(
      { version: 'v0.1.0', hasCli: true, showcaseUrl: 'https://demo.jcusoftware.com' },
      securityHeaders,
    );
    const allHtml = [
      ...[...landing.entries()].filter(([name]) => name.endsWith('.html')).map(([n, c]) => [`landing/${n}`, c]),
      ...[...docs.entries()].filter(([name]) => name.endsWith('.html')).map(([n, c]) => [`docs/${n}`, c]),
    ];

    const overlayRe = /accessibe|acsbapp|userway|audioeye|equalweb|maxaccess/i;
    const sensitiveRe = /speculative-dossier|pitch-brief|private\/\*\.env|TURNSTILE_SECRET_KEY\s*=/i;

    for (const [label, html] of allHtml) {
      assert.ok(html.includes('<html lang="en">'), `${label}: missing lang="en"`);
      assert.ok(html.includes('<a class="skip-link" href="#main">'), `${label}: missing skip link`);
      assert.ok(html.includes('<main id="main"'), `${label}: missing main#main`);
      assert.ok(html.includes('tabindex="-1"'), `${label}: main must be focusable via tabindex="-1"`);
      assert.ok(html.includes('<header '), `${label}: missing header`);
      assert.ok(html.includes('<nav '), `${label}: missing nav`);
      assert.ok(html.includes('<footer '), `${label}: missing footer`);
      assert.ok(!/\sstyle="/i.test(html), `${label}: inline style attribute violates strict CSP`);
      assert.ok(
        !/<script(?![^>]*type="application\/ld\+json")/i.test(html),
        `${label}: unexpected executable script tag`,
      );
      assert.ok(!overlayRe.test(html), `${label}: contains banned overlay reference`);
      assert.ok(!sensitiveRe.test(html), `${label}: contains restricted Zone B / internal text`);

      const headings = extractHeadings(html);
      assert.equal(
        headings.filter((h) => h === 1).length,
        1,
        `${label}: expected exactly one <h1>, got ${JSON.stringify(headings)}`,
      );
      assert.equal(headings[0], 1, `${label}: first heading must be <h1>`);
      for (let i = 1; i < headings.length; i += 1) {
        assert.ok(
          headings[i] - headings[i - 1] <= 1,
          `${label}: skipped heading level h${headings[i - 1]} -> h${headings[i]}`,
        );
      }

      for (const match of html.matchAll(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/g)) {
        assert.doesNotThrow(() => JSON.parse(match[1]), `${label}: invalid JSON-LD`);
      }
      assert.ok(html.includes('<link rel="icon" href="data:,">'), `${label}: missing empty data favicon link`);
    }

    const encoded = safeJsonLd({ text: '</script>&>\u2028\u2029' });
    assert.ok(!encoded.includes('<'));
    assert.ok(!encoded.includes('>'));
    assert.ok(!encoded.includes('&'));
    assert.ok(!encoded.includes('\u2028'));
    assert.ok(!encoded.includes('\u2029'));
  });

  it('emits _headers matching crates/sws-security/src/headers.rs SECURITY_HEADERS exactly', () => {
    const { landing, docs } = buildSiteMaps({}, securityHeaders);
    for (const [label, content] of [
      ['landing/_headers', landing.get('_headers')],
      ['docs/_headers', docs.get('_headers')],
    ]) {
      const parsed = parseHeadersFile(content, '/*');
      assert.deepEqual(compare(securityHeaders, parsed, label), []);
      assert.deepEqual(compare(parsed, securityHeaders, label), []);
    }
  });

  it('writes files, detects drift/missing/extra files in --check mode, and cleans stale files', async () => {
    const tmpRoot = await mkdtemp(path.join(os.tmpdir(), 'sws-sites-test-'));
    try {
      const outLanding = path.join(tmpRoot, 'landing');
      const outDocs = path.join(tmpRoot, 'docs');
      const { landing, docs } = buildSiteMaps({ outLanding, outDocs }, securityHeaders);

      const wrote = await syncDirectory(outLanding, landing, false);
      assert.deepEqual(wrote.problems, []);
      assert.equal((await syncDirectory(outLanding, landing, true)).problems.length, 0);

      // Drifted file
      await writeFile(path.join(outLanding, 'index.html'), 'drifted', 'utf8');
      const drifted = await syncDirectory(outLanding, landing, true);
      assert.ok(drifted.problems.some((p) => p.includes('drifted index.html')));

      // Missing file
      await unlink(path.join(outLanding, '404.html'));
      const missing = await syncDirectory(outLanding, landing, true);
      assert.ok(missing.problems.some((p) => p.includes('missing 404.html')));

      // Unexpected extra file inside a stale subdirectory
      const staleDir = path.join(outLanding, 'stale-sub');
      await mkdir(staleDir, { recursive: true });
      await writeFile(path.join(staleDir, 'stale.html'), 'old', 'utf8');
      const extra = await syncDirectory(outLanding, landing, true);
      assert.ok(extra.problems.some((p) => p.includes('unexpected extra file stale-sub/stale.html')));

      // Re-sync restores clean state, removes stale file, and prunes empty stale directory
      await syncDirectory(outLanding, landing, false);
      assert.equal((await syncDirectory(outLanding, landing, true)).problems.length, 0);
      await assert.rejects(() => stat(staleDir), { code: 'ENOENT' });

      // End-to-end CLI main() check
      assert.equal(await main(['--out-landing', outLanding, '--out-docs', outDocs], {}), 0);
      assert.equal(await main(['--check', '--out-landing', outLanding, '--out-docs', outDocs], {}), 0);
      assert.equal((await syncDirectory(outDocs, docs, true)).problems.length, 0);
    } finally {
      await rm(tmpRoot, { recursive: true, force: true });
    }
  });

  it('validates landing and docs wrangler.jsonc configs and release.yml CLI target parity', async () => {
    const landingCfg = JSON.parse(
      stripJsoncComments(await readFile('sites/landing/wrangler.jsonc', 'utf8')),
    );
    const docsCfg = JSON.parse(
      stripJsoncComments(await readFile('sites/docs/wrangler.jsonc', 'utf8')),
    );

    assert.equal(landingCfg.name, 'sws-landing');
    assert.equal(landingCfg.workers_dev, true);
    assert.equal(landingCfg.assets.directory, './public');
    assert.equal(landingCfg.main, undefined);
    assert.deepEqual(landingCfg.routes, [
      { pattern: 'solo.jcusoftware.com', custom_domain: true },
    ]);

    assert.equal(docsCfg.name, 'sws-docs');
    assert.equal(docsCfg.workers_dev, true);
    assert.equal(docsCfg.assets.directory, './public');
    assert.equal(docsCfg.main, undefined);
    assert.deepEqual(docsCfg.routes, [
      { pattern: 'docs.jcusoftware.com', custom_domain: true },
    ]);

    const releaseYml = await readFile('.github/workflows/release.yml', 'utf8');
    for (const item of CLI_TARGETS) {
      assert.ok(
        releaseYml.includes(`target: ${item.target}`),
        `release.yml is missing CLI target ${item.target}`,
      );
    }
    assert.ok(releaseYml.includes("SWS_LANDING_URL: ${{ vars.SWS_LANDING_URL || 'https://solo.jcusoftware.com' }}"));
    assert.ok(releaseYml.includes("SWS_DOCS_URL: ${{ vars.SWS_DOCS_URL || 'https://docs.jcusoftware.com' }}"));
    assert.ok(releaseYml.includes('npx wrangler deploy -c sites/landing/wrangler.jsonc'));
    assert.ok(releaseYml.includes('npx wrangler deploy -c sites/docs/wrangler.jsonc'));
  });

  it('has zero broken internal or cross-site href links or fragment anchors across landing and docs HTML pages', () => {
    const { config, landing, docs } = buildSiteMaps({}, securityHeaders);
    const origins = new Map([
      [config.landingUrl, { name: 'landing', map: landing }],
      [config.docsUrl, { name: 'docs', map: docs }],
    ]);
    const checkLinks = (siteMap, siteName) => {
      let crossSiteCount = 0;
      for (const [file, html] of siteMap.entries()) {
        if (!file.endsWith('.html')) continue;
        for (const match of html.matchAll(/href="((?:\/|#)[^"]*)"/g)) {
          const href = match[1];
          const [rawPath, fragment] = href.split('#');
          const cleanPath = rawPath.split('?')[0];
          const targetFile =
            cleanPath === ''
              ? file
              : cleanPath.endsWith('/')
                ? `${cleanPath.slice(1)}index.html`
                : cleanPath.slice(1);
          assert.ok(
            siteMap.has(targetFile),
            `${siteName}/${file}: broken internal link "${href}" (expected ${targetFile})`,
          );
          if (fragment) {
            const targetHtml = siteMap.get(targetFile);
            assert.ok(
              targetHtml.includes(`id="${fragment}"`),
              `${siteName}/${file}: broken fragment anchor "${href}" (missing id="${fragment}" in ${targetFile})`,
            );
          }
        }
        for (const match of html.matchAll(/href="(https?:\/\/[^"]+)"/g)) {
          const parsed = new URL(match[1]);
          const target = origins.get(parsed.origin);
          if (!target) continue;
          crossSiteCount += 1;
          const targetFile =
            parsed.pathname === '/'
              ? 'index.html'
              : parsed.pathname.endsWith('/')
                ? `${parsed.pathname.slice(1)}index.html`
                : parsed.pathname.slice(1);
          assert.ok(
            target.map.has(targetFile),
            `${siteName}/${file}: broken cross-site link "${match[1]}" (expected ${target.name}/${targetFile})`,
          );
          if (parsed.hash) {
            const fragment = parsed.hash.slice(1);
            const targetHtml = target.map.get(targetFile);
            assert.ok(
              targetHtml.includes(`id="${fragment}"`),
              `${siteName}/${file}: broken cross-site anchor "${match[1]}" (missing id="${fragment}" in ${target.name}/${targetFile})`,
            );
          }
        }
      }
      assert.ok(crossSiteCount > 0, `${siteName}: expected cross-site links to be verified`);
    };
    checkLinks(landing, 'landing');
    checkLinks(docs, 'docs');
  });
});
