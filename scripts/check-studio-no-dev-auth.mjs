#!/usr/bin/env node
// Guard: the test-only `insecure-dev-auth` feature of sws-worker-studio must never
// reach a release build. Static checks always run; the built release Wasm is scanned
// when present (CI passes --require-wasm after `npm run studio:build`).
//
//   1. the crate's `default` features do not include it;
//   2. no npm script except `test:studio:assets` mentions it;
//   3. no Wrangler config mentions it;
//   4. workflows mention it only on `clippy` lines (the test build is made by
//      `npm run test:studio:assets`, never by a release/deploy step);
//   5. the release Wasm (crates/sws-worker-studio/build/index_bg.wasm) does not embed
//      the dev header names (they are compiled in only under the feature).
//
// Usage: node scripts/check-studio-no-dev-auth.mjs [--require-wasm] [--root dir]
// Exit: 0 = OK, 1 = violation.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const FEATURE = 'insecure-dev-auth';
const WASM_MARKERS = ['X-Sws-Dev-User', 'X-Sws-Dev-Now', 'X-Sws-Dev-Commit-Skew', 'X-Sws-Dev-Batch', 'X-Sws-Dev-Fail-Put', FEATURE];
const CONFIGS = ['wrangler.studio.jsonc', 'deploy/cloudflare/wrangler.studio.example.jsonc'];

export function check(root, { requireWasm = false } = {}) {
  const problems = [];
  const read = (p) => (existsSync(join(root, p)) ? readFileSync(join(root, p), 'utf8') : null);

  const cargo = read('crates/sws-worker-studio/Cargo.toml') ?? '';
  const def = cargo.match(/^default\s*=\s*\[([^\]]*)\]/m);
  if (def?.[1].includes(FEATURE)) problems.push(`Cargo.toml: ${FEATURE} must not be a default feature`);

  const pkg = JSON.parse(read('package.json') ?? '{}');
  for (const [name, cmd] of Object.entries(pkg.scripts ?? {})) {
    if (String(cmd).includes(FEATURE) && name !== 'test:studio:assets' && !/clippy/.test(String(cmd))) {
      problems.push(`package.json script "${name}" mentions ${FEATURE}`);
    }
  }

  for (const c of CONFIGS) {
    if ((read(c) ?? '').includes(FEATURE)) problems.push(`${c} mentions ${FEATURE}`);
  }

  const wfDir = join(root, '.github', 'workflows');
  if (existsSync(wfDir)) {
    for (const f of readdirSync(wfDir)) {
      read(`.github/workflows/${f}`)
        ?.split('\n')
        .forEach((line, i) => {
          if (line.includes(FEATURE) && !/clippy/.test(line)) problems.push(`.github/workflows/${f}:${i + 1} mentions ${FEATURE} outside a clippy line`);
        });
    }
  }

  const wasm = join(root, 'crates/sws-worker-studio/build/index_bg.wasm');
  if (existsSync(wasm)) {
    const bytes = readFileSync(wasm);
    for (const m of WASM_MARKERS) {
      if (bytes.includes(Buffer.from(m))) problems.push(`release Wasm embeds "${m}": built with ${FEATURE}?`);
    }
  } else if (requireWasm) {
    problems.push('crates/sws-worker-studio/build/index_bg.wasm missing (run `npm run studio:build` first)');
  }
  return problems;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const argv = process.argv.slice(2);
  const rootArg = argv.indexOf('--root');
  const root = rootArg >= 0 ? resolve(argv[rootArg + 1]) : resolve(fileURLToPath(import.meta.url), '..', '..');
  const problems = check(root, { requireWasm: argv.includes('--require-wasm') });
  for (const p of problems) console.error(`FAIL ${p}`);
  if (problems.length) process.exit(1);
  console.log(`OK: ${FEATURE} is not reachable from release builds`);
}
