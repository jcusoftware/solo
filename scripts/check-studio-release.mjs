#!/usr/bin/env node
// Guard: the Studio release build must never enable `insecure-dev-auth` (#62, #65).
//
// That Cargo feature lets the X-Sws-Dev-User header stand in for authentication and compiles in
// the asset-store test hooks (X-Sws-Dev-Now, -Commit-Skew, -Batch, -Fail-Put,
// -Fail-Batch-After-Commit, -Hold-Slot; #63) and the /api/dev/ session test routes, so it exists
// only for the local integration tests (scripts/test-studio-sessions.mjs with
// wrangler.studio-sessions.jsonc, tests/integration/studio-assets.test.mjs and
// tests/integration/studio-publish.test.mjs, whose /api/dev/publish-drive hook injects
// publish-job faults; #64), each building
// into a SEPARATE build-* directory. This one script is the union of the former #63 guard
// (scripts/check-studio-no-dev-auth.mjs) and the #62 guard. Static checks (part of
// `npm run verify`, no build needed):
//   1. the crate's default features do not include it, and no other manifest turns it on;
//   2. package.json scripts and CI steps mention it only in the allowlisted test/clippy places;
//   3. production configs (wrangler.studio.jsonc, deploy/cloudflare/*) never reference it, and
//      wrangler.studio.jsonc builds the default `build/` directory without `--features`.
//   4. the dev-auth test config (wrangler.studio-sessions.jsonc) never opens a workers.dev route
//      (`"workers_dev": false`), and the runtime fence var (SWS_INSECURE_DEV_AUTH) that the dev-auth
//      build additionally requires is set by no committed config or npm script, only by the local
//      test runners' `--var`. A dev-auth Worker deployed by mistake therefore still answers 401.
// Artifact check (needs `npm run studio:build`; CI passes --require-artifact):
//   5. crates/sws-worker-studio/build/index_bg.wasm contains none of the dev-auth markers (every
//      X-Sws-Dev-* header name, the feature name, the fence var name, the /api/dev/ test routes).
//      The Worker reads those headers only under the feature, so a release Wasm never carries them.
//
// Usage: node scripts/check-studio-release.mjs [--require-artifact | --require-wasm]
// Exit: 0 = OK, 1 = violation.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const FEATURE = 'insecure-dev-auth';
export const HEADER = 'X-Sws-Dev-User';
/** Var the dev-auth build requires at runtime; only the local test script passes it. */
export const FENCE_VAR = 'SWS_INSECURE_DEV_AUTH';
/** Every test-only header the dev-auth build reads (#62 auth, #63 asset-store hooks). */
export const DEV_HEADERS = [
  HEADER,
  'X-Sws-Dev-Now',
  'X-Sws-Dev-Commit-Skew',
  'X-Sws-Dev-Batch',
  'X-Sws-Dev-Fail-Put',
  'X-Sws-Dev-Fail-Batch-After-Commit',
  'X-Sws-Dev-Hold-Slot',
];
/** package.json scripts allowed to mention the feature (test and lint only). */
export const ALLOWED_SCRIPTS = new Set(['test:studio:sessions', 'test:studio:assets', 'lint:clippy']);
/** Local runners that start a dev-auth Worker; each must open the runtime fence itself. */
export const DEV_RUNNERS = ['scripts/test-studio-sessions.mjs', 'tests/integration/studio-assets.test.mjs', 'tests/integration/studio-publish.test.mjs'];

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

/** `[features]` table as { name: [members] } (multi-line arrays and trailing comments handled). */
export function featureTable(manifest) {
  const sect = manifest.match(/^\[features\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m);
  const table = {};
  if (!sect) return table;
  const body = sect[1].replace(/#[^\n]*/g, '');
  for (const m of body.matchAll(/^\s*([A-Za-z0-9_-]+)\s*=\s*\[([^\]]*)\]/gm)) {
    table[m[1]] = [...m[2].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
  }
  return table;
}

/** Every feature transitively enabled by the default set, so an alias such as
 *  `default = ["dev"]` with `dev = ["insecure-dev-auth"]` cannot hide the feature. */
export function enabledByDefault(manifest) {
  const table = featureTable(manifest);
  const out = new Set();
  const stack = [...defaultFeatures(manifest)];
  while (stack.length) {
    const f = stack.pop();
    // `dep:x`, `x/feat` and `x?/feat` members name dependencies; keep the raw text and the feature part.
    for (const name of [f, f.split('/').pop()]) {
      if (out.has(name)) continue;
      out.add(name);
      stack.push(...(table[name] ?? []));
    }
  }
  return [...out];
}

/**
 * True when a manifest turns the feature on for a dependency, by either route:
 *   - a `features = [ ... ]` array (inline table or `[dependencies.x]` table; the array may span
 *     several lines) that lists the feature, or
 *   - a feature-table member naming another crate's feature, `"<crate>/insecure-dev-auth"` or
 *     `"<crate>?/insecure-dev-auth"`, in any array (`dev = ["sws-worker-studio/insecure-dev-auth"]`,
 *     `default = [...]`).
 * The feature's own definition (`insecure-dev-auth = []`) and comments are not flagged.
 */
export function enablesOnDependency(manifest) {
  const text = manifest.replace(/#[^\n]*/g, '');
  for (const m of text.matchAll(/\bfeatures\s*=\s*\[([^\]]*)\]/g)) {
    if (new RegExp(`"${FEATURE}"`).test(m[1])) return true;
  }
  return new RegExp(`"[A-Za-z0-9_-]+\\??/${FEATURE}"`).test(text);
}

export function scanPackageScripts(scripts) {
  return Object.entries(scripts)
    .filter(([name, cmd]) => String(cmd).includes(FEATURE) && !ALLOWED_SCRIPTS.has(name))
    .map(([name]) => `package.json script "${name}" must not mention ${FEATURE}`);
}

/** A CI line that IS a cargo clippy/test invocation (optionally `- run:`/`run:`-prefixed),
 *  not merely a line containing those words. */
const CARGO_LINE = /^\s*(?:-\s+)?(?:run:\s*)?(?:cargo\s+(?:\+\S+\s+)?(?:clippy|test)\b)/;
const OTHER_TOOL = /\b(?:npm|npx|node|wrangler|echo|build-wasm)\b|&&|;|\|\|/;

/** CI: only cargo clippy/test invocations may mention the feature. */
export function scanCi(text, file) {
  return text
    .split('\n')
    .map((l, i) => [l, i + 1])
    .filter(([l]) => l.includes(FEATURE) && !(CARGO_LINE.test(l) && !OTHER_TOOL.test(l)))
    .map(([, n]) => `${file}:${n} mentions ${FEATURE} outside a cargo clippy/test invocation`);
}

/** The dev-auth test config must not open a public workers.dev URL. */
export function checkSessionsConfig(text) {
  const problems = [];
  if (!/"workers_dev"\s*:\s*false/.test(text)) {
    problems.push('wrangler.studio-sessions.jsonc must set "workers_dev": false (the dev-auth build must never be reachable on workers.dev)');
  }
  if (new RegExp(`${FENCE_VAR}`).test(text.replace(/^\s*\/\/.*$/gm, ''))) {
    problems.push(`wrangler.studio-sessions.jsonc must not set ${FENCE_VAR}: only the local test script passes it with --var`);
  }
  return problems;
}

/** Strings only the dev-auth build carries: the dev headers, the feature name, the runtime fence
 *  var and the test-only routes. */
export const DEV_MARKERS = [...DEV_HEADERS, FEATURE, FENCE_VAR, '/api/dev/'];

export function wasmHasDevAuth(bytes) {
  const buf = Buffer.from(bytes);
  return DEV_MARKERS.some((m) => buf.includes(Buffer.from(m)));
}

export function checkStatic(root) {
  const problems = [];
  const crateToml = join(root, 'crates/sws-worker-studio/Cargo.toml');
  if (enabledByDefault(readFileSync(crateToml, 'utf8')).includes(FEATURE)) {
    problems.push(`${FEATURE} must not be enabled by the default features of sws-worker-studio (directly or through an alias)`);
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
    if (existsSync(f) && /insecure-dev-auth|X-Sws-Dev-|build-dev-auth/.test(readFileSync(f, 'utf8'))) {
      problems.push(`${relative(root, f)} must not reference the dev-auth build`);
    }
  }
  const sessionsCfg = join(root, 'wrangler.studio-sessions.jsonc');
  if (existsSync(sessionsCfg)) problems.push(...checkSessionsConfig(readFileSync(sessionsCfg, 'utf8')));
  // The fence var appears in no production config and no npm script.
  const fenceHolders = [
    ...readdirSync(root).filter((n) => /^wrangler.*\.jsonc?$/.test(n) && n !== 'wrangler.studio-sessions.jsonc').map((n) => join(root, n)),
    ...walk(join(root, 'deploy'), (p) => /\.jsonc?$/.test(p)),
  ];
  for (const f of fenceHolders) {
    if (readFileSync(f, 'utf8').includes(FENCE_VAR)) problems.push(`${relative(root, f)} must not set ${FENCE_VAR}`);
  }
  if (readFileSync(join(root, 'package.json'), 'utf8').includes(FENCE_VAR)) {
    problems.push(`package.json must not mention ${FENCE_VAR}; only the local test runners pass it`);
  }
  for (const rel of DEV_RUNNERS) {
    const runner = join(root, rel);
    if (existsSync(runner) && !readFileSync(runner, 'utf8').includes(`${FENCE_VAR}:1`)) {
      problems.push(`${rel} must pass --var ${FENCE_VAR}:1 (the dev-auth build is closed without it)`);
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
    ? [`${relative(root, wasm)} contains a dev-auth marker (${DEV_MARKERS.join(', ')}): the release Wasm was built with ${FEATURE}`]
    : [];
}

function main(argv) {
  const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
  // `--require-wasm` is the former check-studio-no-dev-auth.mjs spelling.
  const require = argv.includes('--require-artifact') || argv.includes('--require-wasm');
  const problems = [...checkStatic(root), ...checkArtifact(root, { require })];
  if (problems.length) {
    for (const p of problems) console.error(`check:studio:release: ${p}`);
    return 1;
  }
  const art = existsSync(join(root, 'crates/sws-worker-studio/build/index_bg.wasm'));
  console.log(`check:studio:release OK (static checks${art ? ' + release Wasm has no dev-auth marker' : '; no release artifact to scan'})`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) process.exit(main(process.argv.slice(2)));
