#!/usr/bin/env node
// Guard: the Studio release build must never enable `insecure-dev-auth` (#62, #65).
//
// That Cargo feature lets the X-Sws-Dev-User header stand in for authentication, so it exists
// only for the local session integration tests (wrangler.studio-sessions.jsonc, a SEPARATE build
// directory). Static checks (part of `npm run verify`, no build needed):
//   1. the crate's default features do not include it, and no other manifest turns it on;
//   2. package.json scripts and CI steps mention it only in the allowlisted test/clippy places;
//   3. production configs (wrangler.studio.jsonc, deploy/cloudflare/*) never reference it, and
//      wrangler.studio.jsonc builds the default `build/` directory without `--features`.
// Artifact check (needs `npm run studio:build`; CI passes --require-artifact):
//   4. crates/sws-worker-studio/build/index_bg.wasm does not contain the dev header name. The
//      Worker reads that header only under the feature, so a release Wasm never carries it.
//
// Usage: node scripts/check-studio-release.mjs [--require-artifact]
// Exit: 0 = OK, 1 = violation.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const FEATURE = 'insecure-dev-auth';
export const HEADER = 'X-Sws-Dev-User';
/** package.json scripts allowed to mention the feature (test and lint only). */
export const ALLOWED_SCRIPTS = new Set(['test:studio:sessions', 'lint:clippy']);

const WALK_SKIP = new Set(['target', 'node_modules', '.git', 'build', '.wrangler']);

function walk(dir, pred, out = []) {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (e.isDirectory()) {
      if (!WALK_SKIP.has(e.name) && !e.name.startsWith('build-')) walk(join(dir, e.name), pred, out);
    } else if (pred(join(dir, e.name))) {
      out.push(join(dir, e.name));
    }
  }
  return out;
}

/** The `default = [...]` feature list of a Cargo.toml (empty when absent). */
export function defaultFeatures(manifest) {
  const m = manifest.match(/^\s*default\s*=\s*\[([^\]]*)\]/m);
  return m ? [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]) : [];
}

/** True when a manifest line enables the feature on a dependency (`features = [..."insecure-dev-auth"...]`). */
export function enablesOnDependency(manifest) {
  return manifest
    .split('\n')
    .some((l) => l.includes(FEATURE) && /features\s*=/.test(l) && !/^\s*(#|insecure-dev-auth\s*=)/.test(l));
}

export function scanPackageScripts(scripts) {
  return Object.entries(scripts)
    .filter(([name, cmd]) => String(cmd).includes(FEATURE) && !ALLOWED_SCRIPTS.has(name))
    .map(([name]) => `package.json script "${name}" must not mention ${FEATURE}`);
}

/** CI: only cargo clippy/test lines and the session test step may mention the feature. */
export function scanCi(text, file) {
  return text
    .split('\n')
    .map((l, i) => [l, i + 1])
    .filter(([l]) => l.includes(FEATURE) && !/cargo (clippy|test)/.test(l))
    .map(([, n]) => `${file}:${n} mentions ${FEATURE} outside a cargo clippy/test line`);
}

export function wasmHasDevAuth(bytes) {
  return Buffer.from(bytes).includes(Buffer.from(HEADER));
}

export function checkStatic(root) {
  const problems = [];
  const crateToml = join(root, 'crates/sws-worker-studio/Cargo.toml');
  if (defaultFeatures(readFileSync(crateToml, 'utf8')).includes(FEATURE)) {
    problems.push(`${FEATURE} must not be a default feature of sws-worker-studio`);
  }
  for (const f of walk(root, (p) => p.endsWith('Cargo.toml'))) {
    if (enablesOnDependency(readFileSync(f, 'utf8'))) {
      problems.push(`${relative(root, f)} enables ${FEATURE} on a dependency`);
    }
  }
  problems.push(...scanPackageScripts(JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).scripts));
  const wf = join(root, '.github/workflows');
  if (existsSync(wf)) {
    for (const f of readdirSync(wf).filter((n) => /\.ya?ml$/.test(n))) {
      problems.push(...scanCi(readFileSync(join(wf, f), 'utf8'), `.github/workflows/${f}`));
    }
  }
  const prod = [join(root, 'wrangler.studio.jsonc'), ...walk(join(root, 'deploy'), (p) => /\.jsonc?$/.test(p))];
  for (const f of prod) {
    if (existsSync(f) && /insecure-dev-auth|X-Sws-Dev-User|build-dev-auth/.test(readFileSync(f, 'utf8'))) {
      problems.push(`${relative(root, f)} must not reference the dev-auth build`);
    }
  }
  const studio = readFileSync(join(root, 'wrangler.studio.jsonc'), 'utf8');
  if (!studio.includes('"main": "crates/sws-worker-studio/build/index.js"')) {
    problems.push('wrangler.studio.jsonc must run the default crates/sws-worker-studio/build/index.js');
  }
  if (/--features|--out-dir/.test(studio)) {
    problems.push('wrangler.studio.jsonc must build the default feature set into the default build/');
  }
  return problems;
}

export function checkArtifact(root, { require = false } = {}) {
  const wasm = join(root, 'crates/sws-worker-studio/build/index_bg.wasm');
  if (!existsSync(wasm)) {
    return require ? [`${relative(root, wasm)} is missing; run npm run studio:build first`] : [];
  }
  return wasmHasDevAuth(readFileSync(wasm))
    ? [`${relative(root, wasm)} contains "${HEADER}": the release Wasm was built with ${FEATURE}`]
    : [];
}

function main(argv) {
  const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
  const require = argv.includes('--require-artifact');
  const problems = [...checkStatic(root), ...checkArtifact(root, { require })];
  if (problems.length) {
    for (const p of problems) console.error(`check:studio:release: ${p}`);
    return 1;
  }
  const art = existsSync(join(root, 'crates/sws-worker-studio/build/index_bg.wasm'));
  console.log(`check:studio:release OK (static checks${art ? ' + release Wasm has no dev-auth header' : '; no release artifact to scan'})`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) process.exit(main(process.argv.slice(2)));
