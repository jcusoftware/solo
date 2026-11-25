#!/usr/bin/env node
// Security-header parity check (zero dependencies).
//
// Static (default): the `/*` block of core/public/_headers must equal
// SECURITY_HEADERS in crates/sws-security/src/headers.rs (same names, same exact values).
// Live: --url <base> additionally fetches <base>/ and <base>/api/health and
// asserts every contract header is present with the exact value on both.
//
// Usage: node core/tools/check-headers.mjs [--url http://127.0.0.1:8787]
//        [--headers-file core/public/_headers] [--rust crates/sws-security/src/headers.rs]
// Exit: 0 = parity OK, 1 = mismatch, 2 = usage / read / network error.

import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

export function parseHeadersFile(src, pattern = '/*') {
  const out = [];
  let inBlock = false;
  for (const line of src.split(/\r?\n/)) {
    const t = line.trim();
    if (!t || t.startsWith('#')) continue;
    if (!/^[ \t]/.test(line)) {
      inBlock = t === pattern;
      continue;
    }
    if (!inBlock) continue;
    const i = t.indexOf(':');
    if (i > 0) out.push([t.slice(0, i).trim(), t.slice(i + 1).trim()]);
  }
  return out;
}

export function parseRustConst(src, name = 'SECURITY_HEADERS') {
  const start = src.indexOf(`pub const ${name}`);
  if (start < 0) throw new Error(`const ${name} not found`);
  const open = src.indexOf('= &[', start);
  const close = src.indexOf('];', open);
  if (open < 0 || close < 0) throw new Error(`const ${name} has unexpected shape`);
  const body = src.slice(open, close);
  const str = '"((?:[^"\\\\]|\\\\.)*)"';
  const re = new RegExp(`\\(\\s*${str}\\s*,\\s*${str}\\s*,?\\s*\\)`, 'g');
  const unescape = (s) => s.replace(/\\(.)/g, '$1');
  return [...body.matchAll(re)].map((m) => [unescape(m[1]), unescape(m[2])]);
}

export function compare(expected, actual, label) {
  const problems = [];
  const lower = (s) => s.toLowerCase();
  for (const [name, value] of expected) {
    const found = actual.filter(([n]) => lower(n) === lower(name));
    if (found.length === 0) problems.push(`${label}: missing ${name}`);
    else if (found.length > 1) problems.push(`${label}: duplicate ${name}`);
    else if (found[0][1] !== value) {
      problems.push(`${label}: ${name} differs\n    expected: ${value}\n    actual:   ${found[0][1]}`);
    }
  }
  return problems;
}

export function opt(argv, flag, def = null) {
  const i = argv.indexOf(flag);
  if (i < 0) return def;
  const val = argv[i + 1];
  if (!val || val.startsWith('-')) return undefined;
  return val;
}

export async function live(base, expected) {
  const problems = [];
  const targets = [
    { path: '/', status: 200, api: false },
    { path: '/api/health', status: 200, api: true },
    { path: '/api/does-not-exist', status: 404, api: true },
  ];
  for (const { path, status, api } of targets) {
    const url = new URL(path, base).href;
    const res = await fetch(url, {
      redirect: 'manual',
      signal: AbortSignal.timeout(10_000),
    });
    const actual = expected
      .map(([name]) => [name, res.headers.get(name)])
      .filter(([, v]) => v !== null);
    problems.push(...compare(expected, actual, `${url} (HTTP ${res.status})`));
    if (res.status !== status) {
      problems.push(`${url}: expected HTTP ${status}, got ${res.status}`);
    }
    if (api) {
      const cc = res.headers.get('cache-control') || '';
      if (!cc.toLowerCase().includes('no-store')) {
        problems.push(`${url}: expected Cache-Control to include no-store, got ${JSON.stringify(cc)}`);
      }
    }
    if (path === '/api/health') {
      const body = await res.json().catch(() => null);
      if (!body || body.status !== 'ok') problems.push(`${url}: body.status !== "ok" (${JSON.stringify(body)})`);
    } else {
      await res.arrayBuffer();
    }
  }
  return problems;
}

export async function main(argv) {
  const url = opt(argv, '--url', null);
  if (argv.includes('--url') && !url) {
    console.error('usage: check-headers.mjs [--url <base>] [--headers-file <path>] [--rust <path>]');
    return 2;
  }
  const headersPath = opt(argv, '--headers-file', 'core/public/_headers');
  const rustPath = opt(argv, '--rust', 'crates/sws-security/src/headers.rs');
  if (!headersPath || !rustPath) {
    console.error('usage: check-headers.mjs [--url <base>] [--headers-file <path>] [--rust <path>]');
    return 2;
  }
  let fileHeaders;
  let rustHeaders;
  try {
    fileHeaders = parseHeadersFile(await readFile(headersPath, 'utf8'));
    rustHeaders = parseRustConst(await readFile(rustPath, 'utf8'));
  } catch (e) {
    console.error(`check-headers: ${e.message}`);
    return 2;
  }
  if (!rustHeaders.length) {
    console.error(`check-headers: no headers parsed from ${rustPath}`);
    return 2;
  }
  const problems = [
    ...compare(rustHeaders, fileHeaders, `${headersPath} vs ${rustPath}`),
    ...compare(fileHeaders, rustHeaders, `${rustPath} vs ${headersPath}`),
  ];
  if (!problems.length) console.log(`check-headers: static parity OK (${rustHeaders.length} headers)`);
  if (url) {
    try {
      const liveProblems = await live(url, rustHeaders);
      if (!liveProblems.length) console.log(`check-headers: live OK at ${url} (/, /api/health, and /api/does-not-exist)`);
      problems.push(...liveProblems);
    } catch (e) {
      console.error(`check-headers: cannot reach ${url}: ${e.message}`);
      return 2;
    }
  }
  for (const p of problems) console.error(`check-headers: ${p}`);
  return problems.length ? 1 : 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = await main(process.argv.slice(2));
}
