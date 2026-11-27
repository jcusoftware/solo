#!/usr/bin/env node
// Live smoke check for the Studio Worker (zero dependencies).
//
// Hits a running `npm run studio:dev` server and asserts, on REAL responses, the
// status, the Allow header for 405, the full `studio` security-header profile
// (STUDIO_HEADERS in crates/sws-security/src/headers.rs), and `Cache-Control: no-store`:
//   GET/HEAD /api/health -> 200, GET /nope -> 404, POST /api/health -> 405 + Allow,
//   oversized POST -> 413, non-JSON POST -> 415, over-long URL -> 414.
//
// Usage: node scripts/check-studio-live.mjs [--url http://127.0.0.1:8788]
//        (or BASE_URL=...). Server-dependent: CI only, not part of `npm run verify`.
// Exit: 0 = OK, 1 = mismatch, 2 = usage / network error.

import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { compare, opt, parseRustConst } from '../core/tools/check-headers.mjs';

const MAX_BODY_BYTES = 64 * 1024;
const MAX_URL_BYTES = 2048;

export async function checkStudio(base, expected) {
  const problems = [];
  const cases = [
    { name: 'GET /api/health', path: '/api/health', status: 200, json: (b) => b?.status === 'ok' && b?.service === 'sws-worker-studio' },
    { name: 'HEAD /api/health', path: '/api/health', method: 'HEAD', status: 200, empty: true },
    { name: 'GET /nope', path: '/nope', status: 404, json: (b) => b?.error === 'not_found' },
    { name: 'POST /api/health', path: '/api/health', method: 'POST', status: 405, allow: 'GET, HEAD', body: '{}', type: 'application/json', json: (b) => b?.error === 'method_not_allowed' },
    { name: 'oversized POST', path: '/api/health', method: 'POST', status: 413, body: 'x'.repeat(MAX_BODY_BYTES + 1), type: 'application/json', json: (b) => b?.error === 'payload_too_large' },
    { name: 'non-JSON POST', path: '/api/health', method: 'POST', status: 415, body: 'hi', type: 'text/plain', json: (b) => b?.error === 'unsupported_media_type' },
    { name: 'over-long URL', path: `/api/health?q=${'a'.repeat(MAX_URL_BYTES)}`, status: 414, json: (b) => b?.error === 'uri_too_long' },
  ];
  for (const c of cases) {
    const url = new URL(c.path, base).href;
    const res = await fetch(url, {
      method: c.method ?? 'GET',
      headers: c.type ? { 'Content-Type': c.type } : {},
      body: c.body,
      redirect: 'manual',
      signal: AbortSignal.timeout(10_000),
    });
    const label = `${c.name} (HTTP ${res.status})`;
    if (res.status !== c.status) problems.push(`${label}: expected HTTP ${c.status}`);
    const actual = expected.map(([n]) => [n, res.headers.get(n)]).filter(([, v]) => v !== null);
    problems.push(...compare(expected, actual, label));
    const cc = (res.headers.get('cache-control') || '').toLowerCase();
    if (!cc.includes('no-store')) problems.push(`${label}: expected Cache-Control no-store, got ${JSON.stringify(cc)}`);
    if (c.allow !== undefined && res.headers.get('allow') !== c.allow) {
      problems.push(`${label}: expected Allow ${JSON.stringify(c.allow)}, got ${JSON.stringify(res.headers.get('allow'))}`);
    }
    const text = await res.text();
    if (c.empty && text !== '') problems.push(`${label}: expected an empty body`);
    if (c.json) {
      let body = null;
      try {
        body = JSON.parse(text);
      } catch {
        // reported below
      }
      if (!c.json(body)) problems.push(`${label}: unexpected body ${JSON.stringify(text.slice(0, 200))}`);
    }
  }
  return problems;
}

async function main(argv) {
  const base = opt(argv, '--url', process.env.BASE_URL || 'http://127.0.0.1:8788');
  if (!base) {
    console.error('usage: check-studio-live.mjs [--url <base>]');
    return 2;
  }
  let expected;
  try {
    expected = parseRustConst(await readFile('crates/sws-security/src/headers.rs', 'utf8'), 'STUDIO_HEADERS');
  } catch (e) {
    console.error(`check-studio-live: ${e.message}`);
    return 2;
  }
  let problems;
  try {
    problems = await checkStudio(base, expected);
  } catch (e) {
    console.error(`check-studio-live: cannot reach ${base}: ${e.message}`);
    return 2;
  }
  if (problems.length) {
    for (const p of problems) console.error(`FAIL ${p}`);
    return 1;
  }
  console.log(`check-studio-live: OK at ${base} (200/HEAD/404/405/413/415/414 with ${expected.length} studio-profile headers + no-store)`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  process.exit(await main(process.argv.slice(2)));
}
