#!/usr/bin/env node
// Deterministic static-site generator for SWS Landing (`sws-landing`) and Docs (`sws-docs`).
// Zero third-party dependencies.
//
// Usage:
//   node sites/build.mjs [--check] [--version <tag>] [--has-cli true|false]
//     [--repo <owner/repo>] [--landing-url <url>] [--docs-url <url>]
//     [--showcase-url <url>] [--out-landing <dir>] [--out-docs <dir>]

import { mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { parseRustConst } from '../core/tools/check-headers.mjs';
import { normalizeConfig, parseCliArgs, validateOutDir } from './src/config.mjs';
import { DOC_PAGES, renderDocs404, renderDocsPages } from './src/docs.mjs';
import { renderLanding404, renderLandingIndex } from './src/landing.mjs';
import {
  renderCss,
  renderHeadersFile,
  renderRobotsTxt,
  renderSitemapXml,
} from './src/layout.mjs';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_RUST_HEADERS = path.join(REPO_ROOT, 'crates/sws-security/src/headers.rs');

const USAGE = `Usage: node sites/build.mjs [options]

Options:
  --check                 Verify existing output directories byte-for-byte without writing
  --version <tag>         Release version/tag (env: SWS_RELEASE_VERSION, default: dev)
  --has-cli <true|false>  Whether prebuilt sws-cli archives exist (env: SWS_HAS_CLI, default: false)
  --repo <owner/repo>     GitHub repository slug (env: SWS_GITHUB_REPO, default: jcusoftware/solo)
  --landing-url <url>     Landing canonical URL (env: SWS_LANDING_URL, default: https://solo.jcusoftware.com)
  --docs-url <url>        Docs canonical URL (env: SWS_DOCS_URL, default: https://docs.jcusoftware.com)
  --showcase-url <url>    Optional live showcase URL (env: SWS_SHOWCASE_URL, default: unset)
  --out-landing <dir>     Landing output directory (default: sites/landing/public)
  --out-docs <dir>        Docs output directory (default: sites/docs/public)
  -h, --help              Show this help message
`;

export async function loadSecurityHeaders(rustHeadersPath = DEFAULT_RUST_HEADERS) {
  const src = await readFile(rustHeadersPath, 'utf8');
  const parsed = parseRustConst(src, 'SECURITY_HEADERS');
  if (!parsed.length) {
    throw new Error(`No SECURITY_HEADERS entries parsed from ${rustHeadersPath}`);
  }
  return parsed;
}

export function buildSiteMaps(rawConfig, securityHeaders) {
  const config = normalizeConfig(rawConfig);
  const css = renderCss();
  const headers = renderHeadersFile(securityHeaders);

  const landing = new Map([
    ['index.html', renderLandingIndex(config)],
    ['404.html', renderLanding404(config)],
    ['styles.css', css],
    ['_headers', headers],
    ['robots.txt', renderRobotsTxt(config.landingUrl)],
    ['sitemap.xml', renderSitemapXml(config.landingUrl, ['/'])],
  ]);

  const docs = renderDocsPages(config);
  docs.set('404.html', renderDocs404(config));
  docs.set('styles.css', css);
  docs.set('_headers', headers);
  docs.set('robots.txt', renderRobotsTxt(config.docsUrl));
  docs.set(
    'sitemap.xml',
    renderSitemapXml(
      config.docsUrl,
      DOC_PAGES.map((p) => p.href),
    ),
  );

  return { config, landing, docs };
}

async function listRelativeFiles(rootDir, currentDir = rootDir) {
  let entries;
  try {
    entries = await readdir(currentDir, { withFileTypes: true });
  } catch (err) {
    if (err && err.code === 'ENOENT') return [];
    throw err;
  }
  const files = [];
  for (const entry of entries) {
    const full = path.join(currentDir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await listRelativeFiles(rootDir, full)));
    } else if (entry.isFile()) {
      files.push(path.relative(rootDir, full).split(path.sep).join('/'));
    }
  }
  return files.sort();
}

async function pruneEmptyDirs(rootDir, currentDir = rootDir) {
  let entries;
  try {
    entries = await readdir(currentDir, { withFileTypes: true });
  } catch (err) {
    if (err && err.code === 'ENOENT') return;
    throw err;
  }
  for (const entry of entries) {
    if (entry.isDirectory()) {
      await pruneEmptyDirs(rootDir, path.join(currentDir, entry.name));
    }
  }
  if (currentDir !== rootDir) {
    const remaining = await readdir(currentDir);
    if (remaining.length === 0) {
      await rm(currentDir, { recursive: true, force: true });
    }
  }
}

export async function syncDirectory(outDir, expectedFiles, check) {
  const safeOutDir = validateOutDir(outDir, 'outDir');
  const resolvedOut = path.resolve(safeOutDir);
  const existingFiles = await listRelativeFiles(resolvedOut);
  const problems = [];

  if (check) {
    for (const rel of existingFiles) {
      if (!expectedFiles.has(rel)) {
        problems.push(`${outDir}: unexpected extra file ${rel}`);
      }
    }
    for (const [rel, expected] of expectedFiles) {
      const target = path.join(resolvedOut, rel);
      let actual;
      try {
        actual = await readFile(target, 'utf8');
      } catch (err) {
        if (err && err.code === 'ENOENT') {
          problems.push(`${outDir}: missing ${rel}`);
          continue;
        }
        throw err;
      }
      if (actual !== expected) {
        problems.push(`${outDir}: drifted ${rel}`);
      }
    }
    return { problems, fileCount: expectedFiles.size, totalBytes: countBytes(expectedFiles) };
  }

  await mkdir(resolvedOut, { recursive: true });
  for (const rel of existingFiles) {
    if (!expectedFiles.has(rel)) {
      await rm(path.join(resolvedOut, rel), { force: true });
    }
  }
  await pruneEmptyDirs(resolvedOut);
  for (const [rel, content] of expectedFiles) {
    const target = path.join(resolvedOut, rel);
    await mkdir(path.dirname(target), { recursive: true });
    await writeFile(target, content, 'utf8');
  }
  return { problems, fileCount: expectedFiles.size, totalBytes: countBytes(expectedFiles) };
}

function countBytes(fileMap) {
  let total = 0;
  for (const content of fileMap.values()) {
    total += Buffer.byteLength(content, 'utf8');
  }
  return total;
}

export async function main(argv, env = process.env) {
  let config;
  try {
    config = parseCliArgs(argv, env);
  } catch (err) {
    console.error(`sites/build: ${err.message}`);
    console.error(USAGE.trimEnd());
    return 2;
  }

  if (config.help) {
    console.log(USAGE.trimEnd());
    return 0;
  }

  let securityHeaders;
  try {
    securityHeaders = await loadSecurityHeaders();
  } catch (err) {
    console.error(`sites/build: ${err.message}`);
    return 2;
  }

  const { landing, docs } = buildSiteMaps(config, securityHeaders);
  const landingResult = await syncDirectory(config.outLanding, landing, config.check);
  const docsResult = await syncDirectory(config.outDocs, docs, config.check);
  const problems = [...landingResult.problems, ...docsResult.problems];

  if (problems.length > 0) {
    for (const problem of problems) {
      console.error(`sites/build: ${problem}`);
    }
    console.error('sites/build: run `npm run sites:build` to regenerate static site output.');
    return 1;
  }

  const mode = config.check ? 'check' : 'wrote';
  console.log(
    `sites/build: ${mode} landing (${landingResult.fileCount} files, ${landingResult.totalBytes} B) and docs (${docsResult.fileCount} files, ${docsResult.totalBytes} B) [version=${config.version}, hasCli=${config.hasCli}]`,
  );
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = await main(process.argv.slice(2), process.env);
}
