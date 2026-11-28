#!/usr/bin/env node
// Local integration tests for draft previews on an isolated origin (#67), zero dependencies
// besides the repo's own Playwright devDependency for the browser phase.
//
// Runs the RELEASE Studio Worker (`wrangler dev -c wrangler.studio.jsonc`, the default `build/`,
// no `insecure-dev-auth`) on its own port with its own --persist-to state. The Worker answers the
// preview surface for host `localhost:<port>` and the Studio surface for every other host
// (`127.0.0.1:<port>` here). Browsers scope cookies by host and ignore the port, so two ports on
// one host would NOT be isolated; two loopback names are. Node requests connect to 127.0.0.1 and
// set `Host`.
//   STUDIO_PREVIEW_ORIGIN = http://localhost:<port>
//   STUDIO_PUBLIC_ORIGIN  = http://127.0.0.1:<port+1>, a stand-in Studio shell page this script
//     serves (the page that frames previews). Today's `studio` profile (`default-src 'none'`) lets
//     no Worker page frame anything; the framing shell is Builder work (#72/#77). The configured
//     Studio origin is what CSRF (`Origin`) and `frame-ancestors` compare against, so cookie-borne
//     writes in this suite send that Origin.
//   Third origin           = http://localhost:<port+1> (same stand-in server, other host name).
// Playwright `route.fulfill` is NOT used for the framing pages: Chromium fails every child frame
// of a fulfilled document (chrome-error://), whatever the child's headers.
//
// Credentials are real: browser sessions and edit-scoped CLI tokens are written to local D1 in
// their stored form (SHA-256 digests only) and presented exactly as a browser or CLI would.
// Expiry is simulated by ageing the preview's generated-artifact row in local D1 (no test hook).
//
// Phase 1 (configured): producer authorization (editor, edit token, cookie + Origin, tenant
// denial), revision pinning, hostile bundles refused without storing anything, serving index and
// nested files with the exact `preview` profile + no-store + noindex, HEAD/405/robots.txt, cookies
// and Authorization sent to the preview host ignored and never answered with Set-Cookie, Studio
// routes unreachable on the preview host, preview paths 404 on the Studio host, wrong
// site/revision/token 404, expired 410, revoked 410, keep-newest-3 eviction 410, a multi-chunk
// file and a near-cap bundle served byte-exact.
// Browser phase (Playwright Chromium): a Studio-origin page can frame the preview, a third origin
// cannot (frame-ancestors), the framed preview reads no Studio cookie and cannot fetch a Studio API
// (CSP connect-src 'none'), and same-origin subresources load.
// Phase 2 (server restarted with STUDIO_PREVIEW_ORIGIN on the Studio host): fail closed (503
// preview_origin_not_configured; the would-be preview host is just the Studio).
//
// Usage: node scripts/test-studio-previews.mjs [--port 8796]
// Ports: the Worker on <port> (default 8796) and the stand-in shell on <port+1> (8797).
// Locking (shared machines): run under `flock /tmp/solo-studio-8796.lock` (covers both ports).
// Server-dependent: CI only (not part of `npm run verify`). Exit 0 = all passed.

import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import http from 'node:http';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { parseRustConst } from '../core/tools/check-headers.mjs';

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)));
const CFG = 'wrangler.studio.jsonc';
const STATE = '.wrangler/studio-previews-state';
const argPort = process.argv.indexOf('--port');
const PORT = Number(argPort > 0 ? process.argv[argPort + 1] : process.env.STUDIO_PREVIEWS_PORT || 8796);
const SHELL_PORT = PORT + 1;
const STUDIO = `http://127.0.0.1:${SHELL_PORT}`; // configured STUDIO_PUBLIC_ORIGIN (stand-in shell)
const PREVIEW = `http://localhost:${PORT}`;
const STUDIO_HOST = `127.0.0.1:${PORT}`; // the Worker's Studio surface
const STUDIO_API = `http://${STUDIO_HOST}`;
const PREVIEW_HOST = `localhost:${PORT}`;
const THIRD = `http://localhost:${SHELL_PORT}`;
const BUNDLE_TYPE = 'application/vnd.sws.preview-bundle';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sha256 = (b) => createHash('sha256').update(b).digest('hex');
const hex = (n) => randomBytes(n).toString('hex');

// Header profiles, read from the Rust source of truth.
const HEADERS_RS = readFileSync(join(ROOT, 'crates/sws-security/src/headers.rs'), 'utf8');
const STUDIO_HEADERS = parseRustConst(HEADERS_RS, 'STUDIO_HEADERS');
const PREVIEW_RESOLVED = parseRustConst(HEADERS_RS, 'PREVIEW_HEADERS').map(([k, v]) =>
  k === 'Content-Security-Policy' ? [k, v.replace("frame-ancestors 'none'", `frame-ancestors ${STUDIO}`)] : [k, v],
);

// --- Bundle (mirror of crates/sws-worker-studio/src/preview/bundle.rs) -------------------------

function bundle(files) {
  const sorted = Object.entries(files)
    .map(([p, b]) => [p, Buffer.isBuffer(b) ? b : Buffer.from(b)])
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const head = ['sws-preview-bundle/1', ...sorted.map(([p, b]) => `${sha256(b)} ${b.length} ${p}`), '', ''].join('\n');
  return Buffer.concat([Buffer.from(head), ...sorted.map(([, b]) => b)]);
}
/** A bundle with header lines given verbatim (for hostile inputs). */
function rawBundle(lines, body) {
  return Buffer.concat([Buffer.from(['sws-preview-bundle/1', ...lines, '', ''].join('\n')), Buffer.from(body)]);
}
const line = (path, b) => `${sha256(Buffer.from(b))} ${Buffer.byteLength(b)} ${path}`;

const PNG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]);
const INDEX_HTML = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Draft preview</title>
<link rel="stylesheet" href="assets/site.css"></head><body><main><h1>Draft</h1><p id="probe">hello</p></main></body></html>`;
const SITE_CSS = '#probe { color: rgb(1, 2, 3); }';
const EVENTS_HTML = '<!doctype html><html lang="en"><title>Events</title><h1>Events</h1></html>';
const BIG = Buffer.from(Array.from({ length: 700 * 1024 }, (_, i) => 97 + (i % 26)));
const SITE = {
  'index.html': INDEX_HTML,
  'assets/site.css': SITE_CSS,
  'events/index.html': EVENTS_HTML,
  'img/logo.png': PNG,
  'data/big.txt': BIG,
  '.well-known/security.txt': 'Contact: mailto:security@example.invalid\n',
};

// --- Server and D1 helpers ----------------------------------------------------------------

function portOpen() {
  return new Promise((res) => {
    const s = createConnection({ host: '127.0.0.1', port: PORT });
    s.once('connect', () => (s.destroy(), res(true)));
    s.once('error', () => res(false));
  });
}

async function waitPortFree(ms = 30_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (!(await portOpen())) return true;
    await sleep(300);
  }
  return false;
}

let server = null;
let fullLog = '';
async function startServer(previewOrigin) {
  assert.equal(await portOpen(), false, `port ${PORT} is already in use`);
  const vars = { STUDIO_PUBLIC_ORIGIN: STUDIO, STUDIO_PREVIEW_ORIGIN: previewOrigin, STUDIO_BOOTSTRAP_WORKSPACE: 'studio' };
  const varArgs = Object.entries(vars).flatMap(([k, v]) => ['--var', `${k}:${v}`]);
  // detached: its own process group, so exactly this server tree can be stopped.
  server = spawn('npx', ['wrangler', 'dev', '-c', CFG, '--persist-to', STATE, '--port', String(PORT), '--ip', '127.0.0.1', ...varArgs], {
    cwd: ROOT,
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const onData = (d) => (fullLog += d);
  server.stdout.on('data', onData);
  server.stderr.on('data', onData);
  const end = Date.now() + 300_000;
  while (Date.now() < end) {
    try {
      const r = await request('GET', '/api/health', { host: STUDIO_HOST });
      if (r.status === 200) return;
    } catch {
      // not up yet
    }
    if (server.exitCode !== null) break;
    await sleep(1000);
  }
  throw new Error(`wrangler dev did not become ready\n${fullLog.slice(-3000)}`);
}

async function stopServer() {
  if (!server) return;
  const pid = server.pid;
  try {
    process.kill(-pid, 'SIGTERM');
  } catch {
    // already gone
  }
  server = null;
  if (!(await waitPortFree())) {
    try {
      process.kill(-pid, 'SIGKILL'); // only the process group this script started
    } catch {
      // already gone
    }
    await waitPortFree(10_000);
  }
}

function wrangler(args) {
  const r = spawnSync('npx', ['wrangler', ...args], { cwd: ROOT, encoding: 'utf8', timeout: 180_000 });
  if (r.status !== 0) throw new Error(`wrangler ${args.join(' ')} failed:\n${r.stdout}\n${r.stderr}`);
  return r.stdout;
}

function d1(sql) {
  const out = wrangler(['d1', 'execute', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE, '--json', '--command', sql]);
  return JSON.parse(out.slice(out.indexOf('['))).flatMap((p) => p.results ?? []);
}

function d1File(sql) {
  const dir = mkdtempSync(join(tmpdir(), 'sws-previews-'));
  const file = join(dir, 'q.sql');
  writeFileSync(file, sql);
  try {
    wrangler(['d1', 'execute', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE, '--file', file]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const iso = (msFromNow) => new Date(Date.now() + msFromNow).toISOString().replace(/\.\d{3}Z$/, 'Z');
const DAY_MS = 86_400_000;

// --- Credentials (stored form: SHA-256 digests only) ----------------------------------------

function mintSession(userId) {
  const id = hex(12);
  const value = `${id}.${hex(32)}`;
  return { value, sql: `INSERT INTO auth_sessions (id, user_id, secret_hash, created_at, last_seen_at, expires_at) VALUES ('${id}', ${userId}, '${sha256(value)}', '${iso(-60_000)}', '${iso(-60_000)}', '${iso(13 * DAY_MS)}');` };
}

function mintToken(userId, ws) {
  const id = hex(12);
  const value = `swst_${id}_${hex(32)}`;
  return {
    value,
    sql: `INSERT INTO api_tokens (id, workspace_id, user_id, token_hash, scope, label, created_at, expires_at, chain_started_at) VALUES ('${id}', '${ws}', ${userId}, '${sha256(value)}', 'edit', 'preview test', '${iso(-60_000)}', '${iso(29 * DAY_MS)}', '${iso(-60_000)}');`,
  };
}

const IR = { a: sha256('rev-a1'), a2: sha256('rev-a2'), b: sha256('rev-b1') };
let C; // credentials

function seed() {
  const ownerA = mintSession(1);
  const editorA = mintSession(2);
  const editorTokA = mintToken(2, 'ws-a');
  const editorTokB = mintToken(3, 'ws-b');
  C = { ownerA: ownerA.value, editorA: editorA.value, tokA: editorTokA.value, tokB: editorTokB.value };
  d1File(`INSERT INTO workspaces (id, name) VALUES ('ws-a', 'A'), ('ws-b', 'B');
INSERT INTO users (id, github_id, github_login) VALUES (1, 7001, 'owner-a'), (2, 7002, 'editor-a'), (3, 7003, 'editor-b');
INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a', 1, 'owner'), ('ws-a', 2, 'editor'), ('ws-b', 3, 'editor');
INSERT INTO sites (workspace_id, id, slug, name) VALUES ('ws-a', 'site-1', 's1', 'S1'), ('ws-a', 'site-2', 's2', 'S2'), ('ws-a', 'site-3', 's3', 'S3'), ('ws-b', 'site-b', 'sb', 'SB');
INSERT INTO site_revisions (workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) VALUES
  ('ws-a', 'site-1', 'rev-a1', 1, '${IR.a}', 10, 1),
  ('ws-a', 'site-2', 'rev-a2', 1, '${IR.a2}', 10, 1),
  ('ws-a', 'site-3', 'rev-a3', 1, '${IR.a2}', 10, 1),
  ('ws-b', 'site-b', 'rev-b1', 1, '${IR.b}', 10, 3);
${ownerA.sql}
${editorA.sql}
${editorTokA.sql}
${editorTokB.sql}
`);
}

// --- HTTP ---------------------------------------------------------------------------------

/** One request on a fresh connection to 127.0.0.1 with an explicit Host. Body bytes as Buffer. */
function request(method, path, { host = STUDIO_HOST, headers = {}, body, chunked = false } = {}) {
  return new Promise((res, rej) => {
    const h = { Host: host, ...headers };
    if (body !== undefined && !chunked) h['Content-Length'] = Buffer.byteLength(body);
    const req = http.request({ host: '127.0.0.1', port: PORT, path, method, headers: h, agent: false }, (r) => {
      const parts = [];
      r.on('data', (d) => parts.push(d));
      r.on('end', () => {
        const buf = Buffer.concat(parts);
        let json = null;
        try {
          json = JSON.parse(buf.toString('utf8'));
        } catch {
          // not JSON
        }
        res({
          status: r.statusCode,
          buf,
          text: buf.toString('utf8'),
          json,
          raw: r.headers,
          headers: { get: (n) => (r.headers[n.toLowerCase()] === undefined ? null : [].concat(r.headers[n.toLowerCase()]).join(', ')) },
        });
      });
      r.on('error', rej);
    });
    req.on('error', rej);
    req.setTimeout(60_000, () => req.destroy(new Error('timeout')));
    if (body !== undefined) req.write(body);
    req.end();
  });
}

/** `who`: { token } (Bearer) or { cookie, origin } (session cookie; origin null = none sent). */
function auth(who = {}) {
  const h = {};
  if (who.token) h.Authorization = `Bearer ${who.token}`;
  if (who.cookie) {
    h.Cookie = `__Host-sws_session=${who.cookie}`;
    if (who.origin !== null) h.Origin = who.origin ?? STUDIO;
  }
  return h;
}

function upload(ws, site, body, { who = { token: C.tokA }, rev = 'rev-a1', sha = IR.a, type = BUNDLE_TYPE, query, chunked } = {}) {
  const q = query ?? `revision_id=${rev}&revision_sha256=${sha}`;
  const headers = { ...auth(who), 'Content-Type': type };
  if (chunked) headers['Transfer-Encoding'] = 'chunked';
  return request('POST', `/api/sites/${ws}/${site}/previews${q === '' ? '' : `?${q}`}`, { headers, body, chunked });
}

const previewPath = (url) => new URL(url).pathname;
const getPreview = (path, extra = {}) => request(extra.method ?? 'GET', path, { host: PREVIEW_HOST, headers: extra.headers ?? {} });

/** Every header of `profile` present exactly once with exactly its value, plus no-store. */
function assertProfile(res, profile, label) {
  assert.ok(profile.length >= 6, `profile parsed for ${label}`);
  for (const [k, v] of profile) {
    const raw = res.raw[k.toLowerCase()];
    assert.ok(!Array.isArray(raw), `${k} appears once on ${label}`);
    assert.equal(res.headers.get(k), v, `${k} on ${label}`);
  }
  assert.equal(res.headers.get('cache-control'), 'no-store', `no-store on ${label}`);
}

function assertPreviewResponse(res, label) {
  assertProfile(res, PREVIEW_RESOLVED, label);
  assert.equal(res.headers.get('x-robots-tag'), 'noindex, nofollow', label);
  assert.equal(res.headers.get('x-frame-options'), null, `${label}: framing is CSP-only`);
  assert.equal(res.headers.get('set-cookie'), null, `${label}: never sets a cookie`);
  assert.equal(res.headers.get('cross-origin-resource-policy'), null, `${label}: not the studio profile`);
}

// --- Tests --------------------------------------------------------------------------------

const results = [];
async function test(name, fn) {
  try {
    await fn();
    results.push({ name, ok: true });
    console.log(`  ok   ${name}`);
  } catch (e) {
    results.push({ name, ok: false });
    console.log(`  FAIL ${name}\n       ${String(e.stack ?? e).split('\n').slice(0, 6).join('\n       ')}`);
  }
}

const count = (table) => d1(`SELECT COUNT(*) AS n FROM ${table}`)[0].n;
let main1; // { id, url, path }

async function phase1() {
  console.log('\nphase 1: preview origin configured');

  await test('producer: an editor with an edit-scoped token gets 201 and a token URL on the preview origin', async () => {
    const r = await upload('ws-a', 'site-1', bundle(SITE));
    assert.equal(r.status, 201, r.text);
    assertProfile(r, STUDIO_HEADERS, '201 create');
    const b = r.json;
    assert.match(b.preview_id, /^p[0-9a-f]{28}$/);
    assert.match(b.url, new RegExp(`^${PREVIEW}/p/site-1/rev-a1/[0-9a-f]{32}/$`));
    assert.equal(b.revision_id, 'rev-a1');
    assert.equal(b.file_count, Object.keys(SITE).length);
    assert.equal(b.sha256, sha256(bundle(SITE)));
    const ttl = Date.parse(b.expires_at) - Date.now();
    assert.ok(ttl > 23.9 * 3600_000 && ttl <= 24 * 3600_000 + 5000, `expiry ~24 h, got ${b.expires_at}`);
    main1 = { id: b.preview_id, url: b.url, path: previewPath(b.url) };
    // Only the digest is stored.
    const token = main1.path.split('/')[4];
    const rows = d1(`SELECT token_hash FROM previews WHERE id = '${b.preview_id}'`);
    assert.equal(rows[0].token_hash, sha256(token));
    assert.equal(d1(`SELECT COUNT(*) AS n FROM previews WHERE token_hash = '${token}'`)[0].n, 0);
  });

  await test('producer: a browser session needs the Studio Origin; the owner may create too', async () => {
    const b = bundle({ 'index.html': '<p>session</p>' });
    assert.equal((await upload('ws-a', 'site-2', b, { who: { cookie: C.editorA, origin: null }, rev: 'rev-a2', sha: IR.a2 })).status, 403);
    const evil = await upload('ws-a', 'site-2', b, { who: { cookie: C.editorA, origin: 'http://evil.example' }, rev: 'rev-a2', sha: IR.a2 });
    assert.equal(evil.status, 403);
    assert.equal(evil.json.error, 'forbidden_origin');
    const preview = await upload('ws-a', 'site-2', b, { who: { cookie: C.editorA, origin: PREVIEW }, rev: 'rev-a2', sha: IR.a2 });
    assert.equal(preview.status, 403, 'the preview origin is not the Studio origin');
    assert.equal((await upload('ws-a', 'site-2', b, { who: { cookie: C.editorA }, rev: 'rev-a2', sha: IR.a2 })).status, 201);
    assert.equal((await upload('ws-a', 'site-2', b, { who: { cookie: C.ownerA }, rev: 'rev-a2', sha: IR.a2 })).status, 201);
  });

  await test('producer: no credential 401; other workspaces are refused with a uniform 403', async () => {
    const b = bundle({ 'index.html': '<p>x</p>' });
    const anon = await upload('ws-a', 'site-1', b, { who: {} });
    assert.equal(anon.status, 401);
    assertProfile(anon, STUDIO_HEADERS, '401');
    // ws-b member's token on ws-a, and ws-a's token on ws-b.
    const r1 = await upload('ws-a', 'site-1', b, { who: { token: C.tokB } });
    const r2 = await upload('ws-b', 'site-b', b, { who: { token: C.tokA }, rev: 'rev-b1', sha: IR.b });
    assert.deepEqual([r1.status, r1.json.error], [403, 'forbidden']);
    assert.deepEqual([r2.status, r2.json.error], [403, 'forbidden']);
    // An unknown workspace looks the same.
    const r3 = await upload('ws-zzz', 'site-1', b);
    assert.deepEqual([r3.status, r3.json.error], [403, 'forbidden']);
  });

  await test('producer: the revision pin, framing and content type are enforced', async () => {
    const b = bundle({ 'index.html': '<p>x</p>' });
    const cases = [
      [{ sha: IR.a2 }, 409, 'revision_mismatch'],
      [{ rev: 'rev-a2', sha: IR.a2 }, 404, 'revision_not_found'],
      [{ rev: 'nope' }, 404, 'revision_not_found'],
      [{ query: '' }, 400, 'invalid_revision_pin'],
      [{ query: `revision_id=rev-a1` }, 400, 'invalid_revision_pin'],
      [{ query: `revision_id=rev-a1&revision_sha256=${IR.a}&revision_id=x` }, 400, 'invalid_revision_pin'],
      [{ type: 'application/zip' }, 415, 'unsupported_media_type'],
      [{ type: 'application/json' }, 415, 'unsupported_media_type'],
    ];
    for (const [opts, status, code] of cases) {
      const r = await upload('ws-a', 'site-1', b, opts);
      assert.deepEqual([r.status, r.json?.error], [status, code], JSON.stringify(opts));
      assertProfile(r, STUDIO_HEADERS, code);
    }
    const chunked = await upload('ws-a', 'site-1', b, { chunked: true });
    assert.equal(chunked.status, 411);
    // (wrangler dev buffers request bodies, so the bytes are actually sent.)
    const tooBig = await upload('ws-a', 'site-1', Buffer.alloc(5 * 1024 * 1024 + 1, 'a'));
    assert.deepEqual([tooBig.status, tooBig.json?.error], [413, 'bundle_too_large'], 'over the Preview cap');
    const wrongMethod = await request('PUT', '/api/sites/ws-a/site-1/previews', { headers: auth({ token: C.tokA }) });
    assert.deepEqual([wrongMethod.status, wrongMethod.headers.get('allow')], [405, 'POST']);
  });

  await test('producer: hostile bundles are refused and nothing is stored', async () => {
    const before = [count('generated_artifacts'), count('previews'), count('asset_reservations'), count('blob_chunks')];
    const idx = '<p>x</p>';
    const hostile = [
      [rawBundle([line('../etc/passwd.txt', 'x'), line('index.html', idx)], `x${idx}`), 'invalid_path', 0],
      [rawBundle([line('a%2f..%2fb.txt', 'x'), line('index.html', idx)], `x${idx}`), 'invalid_path', 0],
      [rawBundle([line('a\\b.txt', 'x'), line('index.html', idx)], `x${idx}`), 'invalid_path', 0],
      [rawBundle([line('.env.txt', 'x'), line('index.html', idx)], `x${idx}`), 'invalid_path', 0],
      [bundle({ _headers: '/*\n  X-Frame-Options: ALLOWALL', 'index.html': idx }), 'reserved_path', 0],
      [bundle({ '_worker.js/index.js': 'x', 'index.html': idx }), 'reserved_path', 0],
      [bundle({ 'menu.pdf': '%PDF', 'index.html': idx }), 'unsupported_file_type', 1],
      [bundle({ 'a.txt': 'x', 'A.TXT': 'y', 'index.html': idx }), 'case_duplicate_path', 1],
      [bundle({ 'logo.png': '<svg onload=alert(1)>', 'index.html': idx }), 'content_type_mismatch', 1],
      [rawBundle([line('index.html', idx), line('a.txt', 'x')], `${idx}x`), 'unsorted_or_duplicate_path', 1],
      [rawBundle([line('index.html', 'different')], idx), 'bundle_length_mismatch', null],
      [rawBundle([`${'0'.repeat(64)} 8 index.html`], idx), 'sha256_mismatch', 0],
      [Buffer.concat([bundle({ 'index.html': idx }), Buffer.from('trailing')]), 'bundle_length_mismatch', null],
      [bundle({ 'events/index.html': idx }), 'missing_index_html', null],
      [Buffer.from('PK\x03\x04 not a bundle'), 'bundle_format_unsupported', null],
    ];
    for (const [body, code, fileIndex] of hostile) {
      const r = await upload('ws-a', 'site-1', body);
      assert.deepEqual([r.status, r.json?.error], [400, code], `${code}: ${r.text}`);
      assert.equal(r.json.file_index ?? null, fileIndex, `${code} file_index`);
      assert.ok(!r.text.includes('passwd') && !r.text.includes('..'), 'the hostile path is never echoed');
    }
    const after = [count('generated_artifacts'), count('previews'), count('asset_reservations'), count('blob_chunks')];
    assert.deepEqual(after, before, 'no artifact, row, reservation or chunk was written');
  });

  await test('preview host: index and nested files with the exact preview profile, no-store and noindex', async () => {
    const root = await getPreview(main1.path);
    assert.equal(root.status, 200);
    assert.equal(root.headers.get('content-type'), 'text/html; charset=utf-8');
    assert.equal(root.text, INDEX_HTML);
    assertPreviewResponse(root, 'GET index');
    const csp = root.headers.get('content-security-policy');
    for (const d of [`frame-ancestors ${STUDIO}`, "connect-src 'none'", "form-action 'none'", "frame-src 'none'"]) assert.ok(csp.includes(d), d);
    const files = [
      ['index.html', 'text/html; charset=utf-8', INDEX_HTML],
      ['assets/site.css', 'text/css; charset=utf-8', SITE_CSS],
      ['events/', 'text/html; charset=utf-8', EVENTS_HTML],
      ['img/logo.png', 'image/png', PNG],
      ['.well-known/security.txt', 'text/plain; charset=utf-8', SITE['.well-known/security.txt']],
    ];
    for (const [p, type, want] of files) {
      const r = await getPreview(main1.path + p);
      assert.equal(r.status, 200, p);
      assert.equal(r.headers.get('content-type'), type, p);
      assert.ok(r.buf.equals(Buffer.from(want)), `${p} bytes`);
      assertPreviewResponse(r, `GET ${p}`);
    }
    // A file spanning several 256 KiB D1 chunks, starting mid-chunk.
    const big = await getPreview(`${main1.path}data/big.txt`);
    assert.equal(big.status, 200);
    assert.equal(sha256(big.buf), sha256(BIG), 'multi-chunk file is byte-exact');
  });

  await test('preview host: redirects, HEAD, 405, robots.txt and unknown files', async () => {
    const noSlash = await getPreview(main1.path.slice(0, -1));
    assert.deepEqual([noSlash.status, noSlash.headers.get('location')], [308, main1.path]);
    assertPreviewResponse(noSlash, '308 root');
    const dir = await getPreview(`${main1.path}events`);
    assert.deepEqual([dir.status, dir.headers.get('location')], [308, `${main1.path}events/`]);
    const head = await getPreview(main1.path, { method: 'HEAD' });
    assert.deepEqual([head.status, head.buf.length, head.headers.get('content-type')], [200, 0, 'text/html; charset=utf-8']);
    assertPreviewResponse(head, 'HEAD');
    for (const m of ['POST', 'PUT', 'DELETE', 'PATCH', 'OPTIONS']) {
      const r = await getPreview(main1.path, { method: m });
      assert.deepEqual([r.status, r.headers.get('allow')], [405, 'GET, HEAD'], m);
      assertPreviewResponse(r, `405 ${m}`);
    }
    const robots = await getPreview('/robots.txt');
    assert.deepEqual([robots.status, robots.text], [200, 'User-agent: *\nDisallow: /\n']);
    assertPreviewResponse(robots, 'robots.txt');
    for (const p of ['nope.html', 'img/', 'ASSETS/site.css', '%2e%2e/index.html', 'events/..%2findex.html', 'a/../index.html']) {
      const r = await getPreview(main1.path + p);
      assert.equal(r.status, p === 'a/../index.html' ? 200 : 404, p); // `..` is resolved by the URL parser itself
      assertPreviewResponse(r, `unknown ${p}`);
    }
  });

  await test('preview host: Studio cookies and Authorization are ignored and never answered with Set-Cookie', async () => {
    const plain = await getPreview(main1.path);
    const withCreds = await getPreview(main1.path, {
      headers: { Cookie: `__Host-sws_session=${C.editorA}; other=1`, Authorization: `Bearer ${C.tokA}`, Origin: STUDIO },
    });
    assert.equal(withCreds.status, 200);
    assert.ok(withCreds.buf.equals(plain.buf));
    const strip = (r) => Object.entries(r.raw).filter(([k]) => k !== 'date');
    assert.deepEqual(strip(withCreds), strip(plain), 'identical headers with and without credentials');
    assert.ok(!withCreds.text.includes(C.editorA) && !withCreds.text.includes(C.tokA));
    // Credentials do not unlock anything on the preview host.
    const api = await getPreview('/api/sites/ws-a/site-1/assets', { headers: { Authorization: `Bearer ${C.tokA}` } });
    assert.equal(api.status, 404);
    assertPreviewResponse(api, 'API with credentials on the preview host');
  });

  await test('preview host: no Studio route is reachable there (404/405 with the preview profile)', async () => {
    for (const p of ['/', '/api/health', '/auth/session', '/auth/github/start', '/invite', '/api/sites/ws-a/site-1/assets', '/api/sites/ws-a/site-1/session', '/api/sites/ws-a/site-1/previews', '/index.html']) {
      const r = await getPreview(p, { headers: { Authorization: `Bearer ${C.tokA}`, Upgrade: 'websocket' } });
      assert.equal(r.status, 404, p);
      assert.ok(!r.text.includes('sws-worker-studio') && r.json === null, `${p}: not a Studio JSON body`);
      assertPreviewResponse(r, p);
    }
    const post = await request('POST', '/api/sites/ws-a/site-1/previews?revision_id=rev-a1&revision_sha256=' + IR.a, {
      host: PREVIEW_HOST,
      headers: { ...auth({ token: C.tokA }), 'Content-Type': BUNDLE_TYPE },
      body: bundle({ 'index.html': '<p>x</p>' }),
    });
    assert.equal(post.status, 405);
    assertPreviewResponse(post, 'producer POST on the preview host');
  });

  await test('studio host: preview paths are never served there (404 with the studio profile)', async () => {
    const r = await request('GET', main1.path, { host: STUDIO_HOST });
    assert.deepEqual([r.status, r.json?.error], [404, 'not_found']);
    assert.ok(!r.text.includes('Draft'));
    assertProfile(r, STUDIO_HEADERS, 'preview path on the Studio host');
    const robots = await request('GET', '/robots.txt', { host: STUDIO_HOST });
    assert.equal(robots.status, 404);
  });

  await test('wrong site, revision or token: 404 with the full preview header set', async () => {
    const [, , site, rev, token] = main1.path.split('/');
    const other = hex(16);
    for (const p of [
      `/p/site-2/${rev}/${token}/`,
      `/p/${site}/rev-a2/${token}/`,
      `/p/${site}/${rev}/${other}/`,
      `/p/${site}/${rev}/${token.toUpperCase()}/`,
      `/p/${site}/${rev}/${token.slice(0, -1)}/`,
      `/p/${site}/${rev}/`,
    ]) {
      const r = await getPreview(p);
      assert.equal(r.status, 404, p);
      assert.equal(r.text, 'Not found\n');
      assertPreviewResponse(r, `404 ${p}`);
    }
  });

  await test('expired preview: 410 with the full header set (artifact aged in D1)', async () => {
    const b = await upload('ws-a', 'site-1', bundle({ 'index.html': '<p>soon expired</p>' }));
    assert.equal(b.status, 201, b.text);
    const path = previewPath(b.json.url);
    assert.equal((await getPreview(path)).status, 200);
    d1(`UPDATE generated_artifacts SET created_at = '${iso(-2 * 3600_000)}', expires_at = '${iso(-1000)}' WHERE id = (SELECT artifact_id FROM previews WHERE id = '${b.json.preview_id}')`);
    for (const m of ['GET', 'HEAD']) {
      const r = await getPreview(path, { method: m });
      assert.equal(r.status, 410, m);
      assert.equal(r.text, m === 'GET' ? 'This preview link has expired or was revoked.\n' : '');
      assertPreviewResponse(r, `410 expired ${m}`);
    }
    assert.equal((await getPreview(`${path}index.html`)).status, 410);
  });

  await test('revoked preview: DELETE frees the artifact; the link answers 410', async () => {
    const b = await upload('ws-a', 'site-1', bundle({ 'index.html': '<p>to revoke</p>' }));
    assert.equal(b.status, 201, b.text);
    const path = previewPath(b.json.url);
    const id = b.json.preview_id;
    const artifact = d1(`SELECT artifact_id FROM previews WHERE id = '${id}'`)[0].artifact_id;
    assert.equal((await getPreview(path)).status, 200);
    const denied = await request('DELETE', `/api/sites/ws-a/site-1/previews/${id}`, { headers: auth({ token: C.tokB }) });
    assert.equal(denied.status, 403);
    const noOrigin = await request('DELETE', `/api/sites/ws-a/site-1/previews/${id}`, { headers: auth({ cookie: C.editorA, origin: null }) });
    assert.equal(noOrigin.status, 403);
    const ok = await request('DELETE', `/api/sites/ws-a/site-1/previews/${id}`, { headers: auth({ token: C.tokA }) });
    assert.deepEqual([ok.status, ok.json.revoked], [200, true], ok.text);
    assertProfile(ok, STUDIO_HEADERS, 'DELETE');
    const r = await getPreview(path);
    assert.equal(r.status, 410);
    assertPreviewResponse(r, '410 revoked');
    assert.equal(d1(`SELECT COUNT(*) AS n FROM generated_artifacts WHERE id = '${artifact}'`)[0].n, 0);
    assert.equal(d1(`SELECT COUNT(*) AS n FROM blob_chunks WHERE blob_id = '${artifact}'`)[0].n, 0);
    const again = await request('DELETE', `/api/sites/ws-a/site-1/previews/${id}`, { headers: auth({ cookie: C.editorA }) });
    assert.deepEqual([again.status, again.json.already_revoked], [200, true]);
    const unknown = await request('DELETE', '/api/sites/ws-a/site-1/previews/pnope', { headers: auth({ token: C.tokA }) });
    assert.deepEqual([unknown.status, unknown.json.error], [404, 'preview_not_found']);
    const otherSite = await request('DELETE', `/api/sites/ws-a/site-2/previews/${id}`, { headers: auth({ token: C.tokA }) });
    assert.equal(otherSite.status, 404);
  });

  await test('retention: only the newest 3 previews of a site stay; older links answer 410', async () => {
    const paths = [];
    for (let i = 0; i < 4; i++) {
      const r = await upload('ws-a', 'site-3', bundle({ 'index.html': `<p>v${i}</p>` }), { rev: 'rev-a3', sha: IR.a2 });
      assert.equal(r.status, 201, r.text);
      paths.push(previewPath(r.json.url));
    }
    const statuses = [];
    for (const p of paths) statuses.push((await getPreview(p)).status);
    assert.deepEqual(statuses, [410, 200, 200, 200]);
    assert.equal(d1("SELECT COUNT(*) AS n FROM generated_artifacts WHERE site_id = 'site-3'")[0].n, 3);
  });

  await test('a near-cap bundle (about 4.9 MiB, 20 D1 chunks) round-trips; the per-site budget then refuses more', async () => {
    const big = Buffer.alloc(Math.floor(4.9 * 1024 * 1024), 'z');
    const body = bundle({ 'index.html': '<p>big</p>', 'big.txt': big });
    assert.ok(body.length <= 5 * 1024 * 1024);
    const r = await upload('ws-a', 'site-2', body, { rev: 'rev-a2', sha: IR.a2 });
    assert.equal(r.status, 201, r.text);
    const got = await getPreview(`${previewPath(r.json.url)}big.txt`);
    assert.equal(got.status, 200);
    assert.equal(sha256(got.buf), sha256(big));
    // site-2 now holds ~4.9 MiB + two small previews; a second 4.9 MiB preview would pass
    // 10 MiB only with the first, so add one more and expect the budget refusal.
    const r2 = await upload('ws-a', 'site-2', body, { rev: 'rev-a2', sha: IR.a2 });
    assert.equal(r2.status, 201, r2.text);
    const r3 = await upload('ws-a', 'site-2', body, { rev: 'rev-a2', sha: IR.a2 });
    assert.deepEqual([r3.status, r3.json.error], [409, 'generated_budget_exceeded']);
  });
}

// --- Browser phase --------------------------------------------------------------------------

async function browserPhase() {
  console.log('\nbrowser: framing, cookies and CSP in Chromium');
  let chromium;
  try {
    ({ chromium } = await import('@playwright/test'));
  } catch (e) {
    throw new Error(`Playwright is not installed: ${e.message}`);
  }
  // Stand-in Studio shell (and, by the other host name, the third origin): a page that frames the preview.
  const shell = http.createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    res.end(`<!doctype html><title>host</title><iframe id="f" src="${main1.url}" width="800" height="600"></iframe>`);
  });
  await new Promise((r, j) => shell.once('error', j).listen(SHELL_PORT, '127.0.0.1', r));
  const browser = await chromium.launch();
  try {
    const context = await browser.newContext();
    // A Studio cookie on the Studio host name (cookies ignore the port, so the shell sees it too).
    await context.addCookies([{ name: 'studio_probe', value: 'secret-studio-cookie', url: STUDIO_API }]);

    await test('a Studio-origin page can frame the preview; the frame reads no Studio cookie and cannot fetch a Studio API', async () => {
      const page = await context.newPage();
      await page.goto(`${STUDIO}/shell`);
      assert.equal(await page.evaluate(() => document.cookie), 'studio_probe=secret-studio-cookie', 'the Studio page has its cookie');
      const frame = await (await page.waitForSelector('#f')).contentFrame();
      await frame.waitForLoadState('load');
      assert.equal(frame.url(), main1.url);
      assert.equal(await frame.title(), 'Draft preview');
      assert.equal(await frame.evaluate(() => document.cookie), '', 'no Studio cookie is visible to the draft');
      // The draft's own stylesheet loads ('self'), so the preview renders.
      assert.equal(await frame.evaluate(() => getComputedStyle(document.getElementById('probe')).color), 'rgb(1, 2, 3)');
      const fetchResult = await frame.evaluate(async (studio) => {
        const violations = [];
        document.addEventListener('securitypolicyviolation', (e) => violations.push(e.violatedDirective));
        let outcome;
        try {
          await fetch(`${studio}/api/health`, { credentials: 'include' });
          outcome = 'fetched';
        } catch (e) {
          outcome = `blocked: ${e.name}`;
        }
        await new Promise((r) => setTimeout(r, 200));
        return { outcome, violations };
      }, STUDIO_API);
      assert.match(fetchResult.outcome, /^blocked/, JSON.stringify(fetchResult));
      assert.ok(fetchResult.violations.includes('connect-src'), JSON.stringify(fetchResult));
      // Same for its own origin: drafts cannot call anything.
      const self = await frame.evaluate(async () => {
        try {
          await fetch('/robots.txt');
          return 'fetched';
        } catch {
          return 'blocked';
        }
      });
      assert.equal(self, 'blocked');
      await page.close();
    });

    await test('a third origin cannot frame the preview (CSP frame-ancestors)', async () => {
      const page = await context.newPage();
      const refused = [];
      page.on('console', (m) => /frame-ancestors|Refused to frame/i.test(m.text()) && refused.push(m.text()));
      await page.goto(`${THIRD}/shell`);
      const handle = await page.waitForSelector('#f');
      await page.waitForTimeout(1500);
      const frame = await handle.contentFrame();
      const title = await frame.evaluate(() => document.title).catch(() => null);
      assert.notEqual(title, 'Draft preview', 'the draft must not render inside a third-party page');
      // Same shell markup from the same server as the allowed case above, only the host name
      // (origin) differs, so the refusal is the preview's frame-ancestors.
      assert.equal(frame.url(), 'chrome-error://chromewebdata/', `frame url ${frame.url()}, console ${JSON.stringify(refused)}`);
      if (refused.length) console.log(`       (Chromium: ${refused[0].slice(0, 160)})`);
      await page.close();
    });

    await test('the preview opens top-level with no cookie and the noindex header', async () => {
      const page = await context.newPage();
      const resp = await page.goto(main1.url);
      assert.equal(resp.status(), 200);
      assert.equal(resp.headers()['x-robots-tag'], 'noindex, nofollow');
      assert.equal(await page.evaluate(() => document.cookie), '');
      const cookies = await context.cookies(PREVIEW);
      assert.deepEqual(cookies, [], 'the preview host has no cookie');
      await page.close();
    });
  } finally {
    await browser.close();
    await new Promise((r) => shell.close(r));
  }
}

// --- Phase 2 --------------------------------------------------------------------------------

async function phase2() {
  console.log('\nphase 2: preview origin on the Studio host name, other port (must fail closed)');
  await test('misconfigured preview origin: 503 preview_origin_not_configured; no preview is served anywhere', async () => {
    const create = await upload('ws-a', 'site-1', bundle({ 'index.html': '<p>x</p>' }));
    assert.deepEqual([create.status, create.json?.error], [503, 'preview_origin_not_configured']);
    assertProfile(create, STUDIO_HEADERS, '503 create');
    for (const host of [STUDIO_HOST, PREVIEW_HOST]) {
      const r = await request('GET', main1.path, { host });
      assert.deepEqual([r.status, r.json?.error], [503, 'preview_origin_not_configured'], host);
      assertProfile(r, STUDIO_HEADERS, `503 /p on ${host}`);
      assert.ok(!r.text.includes('Draft'));
      const health = await request('GET', '/api/health', { host });
      assert.equal(health.status, 200, `${host} is just the Studio`);
    }
  });
}

// --- Main -----------------------------------------------------------------------------------

async function main() {
  let code = 1;
  try {
    rmSync(join(ROOT, STATE), { recursive: true, force: true });
    console.log('migrating and seeding the local Studio D1 ...');
    wrangler(['d1', 'migrations', 'apply', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE]);
    seed();
    console.log(`starting wrangler dev (release build) on :${PORT}; Studio origin ${STUDIO} (shell stand-in), preview ${PREVIEW} ...`);
    await startServer(PREVIEW);
    await phase1();
    if (main1) await browserPhase();
    else results.push({ name: 'browser phase skipped: no preview was created', ok: false });
    await stopServer();
    // Preview origin = the Worker's own Studio host on another port than the Studio origin:
    // same host name, so it must be refused.
    await startServer(STUDIO_API);
    await phase2();
    await stopServer();
    const failed = results.filter((r) => !r.ok);
    console.log(`\n${results.length - failed.length}/${results.length} preview tests passed`);
    code = failed.length ? 1 : 0;
    if (failed.length) console.log(`server log tail:\n${fullLog.slice(-2000)}`);
  } catch (e) {
    console.error(`test-studio-previews: ${e.stack ?? e}`);
    console.error(fullLog.slice(-3000));
  } finally {
    await stopServer();
    writeFileSync(join(ROOT, '.wrangler', 'studio-previews-server.log'), fullLog);
  }
  const free = !(await portOpen());
  console.log(free ? `port ${PORT} is free` : `WARNING: port ${PORT} is still in use`);
  return free ? code : code || 1;
}

process.exit(await main());
