// Local integration test for the Studio asset store (issue #63). Server-dependent:
// NOT part of `npm run verify`; CI runs it in the `web` job (`npm run test:studio:assets`).
//
// It builds the Studio Worker WITH the test-only `insecure-dev-auth` feature into a
// SEPARATE output directory (crates/sws-worker-studio/build-insecure-dev-auth/), then
// starts three local `wrangler dev` instances on their own ports and their own
// `--persist-to` state (nothing is shared with `npm run studio:dev`, and nothing
// touches Cloudflare: local D1/R2 simulation only, no --remote):
//   A  :8791  cardless default (D1 blobs, no R2 binding)
//   B  :8792  opt-in R2 (a LOCAL simulated MEDIA_BUCKET, STUDIO_ASSET_BACKEND=r2)
//   C  :8793  STUDIO_ASSET_BACKEND=r2 WITHOUT a binding (diagnostic)
// The R2 binding exists only in B's generated test config.
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { Readable } from 'node:stream';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import net from 'node:net';
import { resolve, join } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');
const WRANGLER = join(ROOT, 'node_modules', '.bin', 'wrangler');
const STATE = join(ROOT, '.wrangler', 'studio-assets-test');
const CRATE = join(ROOT, 'crates', 'sws-worker-studio');
const MIB = 1024 * 1024;
const PORTS = { a: 8791, b: 8792, c: 8793 };
const INSPECTOR = { a: 9391, b: 9392, c: 9393 };
const SHA = (buf) => createHash('sha256').update(buf).digest('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// `X-Sws-Dev-Now` (dev build only) is an OFFSET in seconds from the Worker's real clock.
const PAST_TTL = 700; // past the 600 s reservation TTL / orphan minimum age

const PNG_SIG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
/** A distinct PNG-signed payload of exactly `len` bytes. */
const png = (len) => Buffer.concat([PNG_SIG, randomBytes(Math.max(0, len - PNG_SIG.length))]).subarray(0, len);

const servers = [];

function writeConfig(name, { r2 }) {
  const dir = join(STATE, name);
  mkdirSync(dir, { recursive: true });
  const cfg = join(dir, 'wrangler.jsonc');
  writeFileSync(
    cfg,
    JSON.stringify({
      name: `sws-studio-assets-test-${name}`,
      main: join(CRATE, 'build-insecure-dev-auth', 'index.js'),
      compatibility_date: '2026-11-01',
      d1_databases: [
        {
          binding: 'STUDIO_DB',
          database_name: 'sws-studio-assets-test',
          migrations_dir: join(CRATE, 'migrations'),
          database_id: '00000000-0000-0000-0000-000000000063',
        },
      ],
      ...(r2 ? { r2_buckets: [{ binding: 'MEDIA_BUCKET', bucket_name: 'sws-studio-assets-test-media' }] } : {}),
      vars: { STUDIO_VERSION: 'assets-test' },
    }),
  );
  return { dir, cfg, persist: join(dir, 'state') };
}

function wr(args, env = {}) {
  const r = spawnSync(WRANGLER, args, {
    cwd: ROOT,
    encoding: 'utf8',
    env: { ...process.env, CI: '1', WRANGLER_SEND_METRICS: 'false', ...env },
  });
  assert.equal(r.status, 0, `wrangler ${args.join(' ')} failed:\n${r.stdout}\n${r.stderr}`);
  return r.stdout;
}

const SEED = `
INSERT INTO workspaces (id, name) VALUES ('ws-a', 'Workspace A'), ('ws-b', 'Workspace B'), ('ws-c', 'Workspace C');
INSERT INTO users (github_id, github_login) VALUES (1001, 'alice'), (2002, 'bob'), (3003, 'carol');
INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a', 1, 'owner'), ('ws-b', 2, 'owner'), ('ws-a', 3, 'editor'), ('ws-c', 1, 'owner');
INSERT INTO sites (workspace_id, id, slug, name) VALUES
  ('ws-a', 'site-a1', 'one', 'One'), ('ws-a', 'site-a2', 'two', 'Two'), ('ws-a', 'site-a3', 'three', 'Three'),
  ('ws-a', 'site-a4', 'four', 'Four'), ('ws-b', 'site-b1', 'bee', 'Bee'), ('ws-c', 'site-c1', 'see', 'See');
-- Workspace cap: three live 50 MiB reservations fill ws-d's 150 MiB; site-d4 is empty.
INSERT INTO workspaces (id, name) VALUES ('ws-d', 'Workspace D'), ('ws-e', 'Workspace E');
INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-d', 1, 'owner'), ('ws-e', 1, 'owner');
INSERT INTO sites (workspace_id, id, slug, name) VALUES
  ('ws-d', 'site-d1', 'd1', 'D1'), ('ws-d', 'site-d2', 'd2', 'D2'), ('ws-d', 'site-d3', 'd3', 'D3'), ('ws-d', 'site-d4', 'd4', 'D4'),
  ('ws-e', 'site-e1', 'e1', 'E1');
INSERT INTO asset_reservations (workspace_id, site_id, id, kind, byte_size, created_at, expires_at) VALUES
  ('ws-d', 'site-d1', 'cap1', 'asset', 52428800, '2020-01-01T00:00:00Z', '2099-01-01T00:00:00Z'),
  ('ws-d', 'site-d2', 'cap2', 'asset', 52428800, '2020-01-01T00:00:00Z', '2099-01-01T00:00:00Z'),
  ('ws-d', 'site-d3', 'cap3', 'asset', 52428800, '2020-01-01T00:00:00Z', '2099-01-01T00:00:00Z');
-- Per-site count cap: 500 live 1-byte reservations fill site-e1's 500 files.
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500)
INSERT INTO asset_reservations (workspace_id, site_id, id, kind, byte_size, created_at, expires_at)
SELECT 'ws-e', 'site-e1', 'cnt' || i, 'asset', 1, '2020-01-01T00:00:00Z', '2099-01-01T00:00:00Z' FROM n;
-- Stale state a crashed/aborted run would leave behind, for the reconcile test (site-c1):
INSERT INTO asset_reservations (workspace_id, site_id, id, kind, byte_size, created_at, expires_at) VALUES
  ('ws-c', 'site-c1', 'stale1', 'asset', 1048576, '2020-01-01T00:00:00Z', '2020-01-01T00:10:00Z'),
  ('ws-c', 'site-c1', 'stale2', 'preview', 1048576, '2020-01-01T00:00:00Z', '2020-01-01T00:10:00Z'),
  ('ws-c', 'site-c1', 'live1', 'asset', 1024, '2020-01-01T00:00:00Z', '2099-01-01T00:00:00Z');
INSERT INTO assets (workspace_id, site_id, id, sha256, byte_size, content_type, storage_backend) VALUES
  ('ws-c', 'site-c1', 'a-incomplete', '${'1'.repeat(64)}', 10, 'image/png', 'd1');
INSERT INTO blob_chunks (workspace_id, blob_id, seq, byte_size, data) VALUES
  ('ws-c', 'gone', 0, 2, x'0102'), ('ws-c', 'gone', 1, 2, x'0304');
INSERT INTO generated_artifacts (workspace_id, site_id, id, kind, sha256, byte_size, storage_backend, created_at, expires_at) VALUES
  ('ws-c', 'site-c1', 'g-expired', 'preview', '${'2'.repeat(64)}', 100, 'd1', '2020-01-01T00:00:00Z', '2020-01-02T00:00:00Z'),
  ('ws-c', 'site-c1', 'g-p1', 'preview', '${'3'.repeat(64)}', 100, 'd1', '2099-01-01T01:00:00Z', '2099-02-01T00:00:00Z'),
  ('ws-c', 'site-c1', 'g-p2', 'preview', '${'4'.repeat(64)}', 100, 'd1', '2099-01-01T02:00:00Z', '2099-02-01T00:00:00Z'),
  ('ws-c', 'site-c1', 'g-p3', 'preview', '${'5'.repeat(64)}', 100, 'd1', '2099-01-01T03:00:00Z', '2099-02-01T00:00:00Z'),
  ('ws-c', 'site-c1', 'g-p4', 'preview', '${'6'.repeat(64)}', 100, 'd1', '2099-01-01T04:00:00Z', '2099-02-01T00:00:00Z'),
  ('ws-c', 'site-c1', 'g-p5', 'preview', '${'7'.repeat(64)}', 100, 'd1', '2099-01-01T05:00:00Z', '2099-02-01T00:00:00Z');
`;

async function startServer(key, { r2, vars = [], seed = true }) {
  const { cfg, persist, dir } = writeConfig(key, { r2 });
  wr(['d1', 'migrations', 'apply', 'STUDIO_DB', '--local', '-c', cfg, '--persist-to', persist]);
  if (seed) {
    const file = join(dir, 'seed.sql');
    writeFileSync(file, SEED);
    wr(['d1', 'execute', 'STUDIO_DB', '--local', '-c', cfg, '--persist-to', persist, '--file', file]);
  }
  const port = PORTS[key];
  const args = ['dev', '-c', cfg, '--port', String(port), '--ip', '127.0.0.1', '--inspector-port', String(INSPECTOR[key]), '--persist-to', persist];
  for (const v of vars) args.push('--var', v);
  const child = spawn(WRANGLER, args, {
    cwd: ROOT,
    detached: true, // own process group so the whole wrangler/workerd tree can be stopped
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, CI: '1', WRANGLER_SEND_METRICS: 'false' },
  });
  let log = '';
  child.stdout.on('data', (d) => (log += d));
  child.stderr.on('data', (d) => (log += d));
  const server = { key, port, base: `http://127.0.0.1:${port}`, child, persist, cfg, log: () => log };
  servers.push(server);
  for (let i = 0; i < 90; i++) {
    try {
      const r = await fetch(`${server.base}/api/health`, { signal: AbortSignal.timeout(2000) });
      if (r.ok) return server;
    } catch {
      /* not ready */
    }
    await sleep(1000);
  }
  throw new Error(`server ${key} did not start:\n${log}`);
}

function stopAll() {
  for (const s of servers) {
    try {
      process.kill(-s.child.pid, 'SIGTERM');
    } catch {
      /* already gone */
    }
  }
}

process.on('exit', stopAll);

/** Authenticated request helper. `user` is the dev-auth user id (undefined = anonymous). */
function api(server) {
  const call = async (method, path, { user, body, headers = {}, now } = {}) => {
    const h = { ...headers };
    if (user !== undefined) h['X-Sws-Dev-User'] = String(user);
    if (now !== undefined) h['X-Sws-Dev-Now'] = String(now);
    const res = await fetch(`${server.base}${path}`, { method, headers: h, body, duplex: body && body.getReader ? 'half' : undefined });
    const buf = Buffer.from(await res.arrayBuffer());
    let json = null;
    if ((res.headers.get('content-type') || '').includes('application/json')) json = JSON.parse(buf.toString('utf8'));
    return { status: res.status, headers: res.headers, buf, json };
  };
  const assetPath = (ws, site, sha) => `/api/sites/${ws}/${site}/assets/${sha}`;
  /** Upload with retry on 503 upload_busy (the per-isolate memory guard). */
  const put = async (ws, site, buf, opts = {}) => {
    const { user = 1, type = 'image/png', sha = SHA(buf), now, headers = {} } = opts;
    for (let attempt = 0; attempt < 400; attempt++) {
      const res = await call('PUT', assetPath(ws, site, sha), {
        user,
        now,
        headers: { 'Content-Type': type, ...headers },
        body: buf,
      });
      if (res.status === 503 && res.json?.error === 'upload_busy') {
        await sleep(25 + Math.random() * 50);
        continue;
      }
      return res;
    }
    throw new Error('upload stayed busy');
  };
  const list = (ws, site, user = 1) => call('GET', `/api/sites/${ws}/${site}/assets`, { user });
  const reconcile = (ws, site, { cursor, ...opts } = {}) =>
    call('POST', `/api/sites/${ws}/${site}/assets/reconcile${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''}`, { user: 1, ...opts });
  return { call, put, list, reconcile, assetPath };
}

/** Chunked upload (no Content-Length): a ReadableStream body. */
function streamBody(total, piece = 64 * 1024) {
  let sent = 0;
  return new ReadableStream({
    pull(ctl) {
      if (sent >= total) return ctl.close();
      const n = Math.min(piece, total - sent);
      const b = Buffer.alloc(n, 7);
      if (sent === 0) PNG_SIG.copy(b);
      sent += n;
      ctl.enqueue(b);
    },
  });
}

/** Raw HTTP/1.1 over a socket so Content-Length can lie. Resolves the first response. */
function rawPut(port, path, headers, bodyBytes, { closeAfterSend = false } = {}) {
  return new Promise((resolveP, reject) => {
    const sock = net.connect(port, '127.0.0.1');
    let data = '';
    sock.on('data', (d) => {
      data += d.toString('latin1');
      const m = data.match(/^HTTP\/1\.1 (\d{3})[\s\S]*?\r\n\r\n([\s\S]*)$/);
      if (m && /\}$/.test(m[2].trim())) {
        sock.destroy();
        resolveP({ status: Number(m[1]), body: m[2].trim() });
      }
    });
    sock.on('error', reject);
    sock.on('close', () => resolveP({ status: Number((data.match(/^HTTP\/1\.1 (\d{3})/) || [])[1] || 0), body: data }));
    const head = [`PUT ${path} HTTP/1.1`, `Host: 127.0.0.1:${port}`, ...Object.entries(headers).map(([k, v]) => `${k}: ${v}`), '', ''].join('\r\n');
    sock.write(head);
    sock.write(bodyBytes);
    if (closeAfterSend) setTimeout(() => sock.destroy(), 200);
  });
}

describe('studio asset store (local wrangler dev, insecure-dev-auth build)', { concurrency: false }, () => {
  let A, B, C;

  before(async () => {
    rmSync(STATE, { recursive: true, force: true });
    const build = spawnSync('node', ['scripts/build-wasm.mjs', '--profile', 'release', '--crate', 'crates/sws-worker-studio', '--features', 'insecure-dev-auth'], {
      cwd: ROOT,
      encoding: 'utf8',
    });
    assert.equal(build.status, 0, `${build.stdout}\n${build.stderr}`);
    A = await startServer('a', { r2: false });
    B = await startServer('b', { r2: true, vars: ['STUDIO_ASSET_BACKEND:r2'] });
    C = await startServer('c', { r2: false, vars: ['STUDIO_ASSET_BACKEND:r2'] });
  });

  after(() => stopAll());

  describe('authentication and tenant boundary', () => {
    it('is closed without credentials (401), even though the test build is running', async () => {
      const a = api(A);
      const buf = png(100);
      for (const [m, p, body] of [
        ['PUT', a.assetPath('ws-a', 'site-a1', SHA(buf)), buf],
        ['GET', a.assetPath('ws-a', 'site-a1', SHA(buf))],
        ['DELETE', a.assetPath('ws-a', 'site-a1', SHA(buf))],
        ['GET', '/api/sites/ws-a/site-a1/assets'],
        ['POST', '/api/sites/ws-a/site-a1/assets/reconcile'],
      ]) {
        const r = await a.call(m, p, { body, headers: body ? { 'Content-Type': 'image/png' } : {} });
        assert.equal(r.status, 401, `${m} ${p}`);
        assert.equal(r.json.error, 'unauthenticated');
      }
      const bad = await a.call('GET', '/api/sites/ws-a/site-a1/assets', { user: 'abc' });
      assert.equal(bad.status, 401);
      assert.equal((await a.call('GET', '/api/health')).status, 200, 'health stays public');
    });

    it('denies non-members and other workspaces (403), with no existence oracle', async () => {
      const a = api(A);
      const buf = png(200);
      assert.equal((await a.put('ws-a', 'site-a1', buf, { user: 1 })).status, 201);
      const sha = SHA(buf);
      // user 2 (bob) is only in ws-b; user 99 does not exist; alice is not in ws-b.
      for (const [user, ws, site] of [[2, 'ws-a', 'site-a1'], [99, 'ws-a', 'site-a1'], [1, 'ws-b', 'site-b1'], [1, 'ws-nope', 'site-a1']]) {
        const label = `user ${user} ${ws}/${site}`;
        assert.equal((await a.put(ws, site, png(100), { user })).status, 403, `PUT ${label}`);
        assert.equal((await a.call('GET', a.assetPath(ws, site, sha), { user })).status, 403, `GET ${label}`);
        assert.equal((await a.call('DELETE', a.assetPath(ws, site, sha), { user })).status, 403, `DELETE ${label}`);
        assert.equal((await a.list(ws, site, user)).status, 403, `LIST ${label}`);
        assert.equal((await a.reconcile(ws, site, { user })).status, 403, `RECONCILE ${label}`);
      }
      // The same hash uploaded to ws-b by bob is a different tenant's asset.
      assert.equal((await a.put('ws-b', 'site-b1', buf, { user: 2 })).status, 201);
      assert.equal((await a.call('DELETE', a.assetPath('ws-b', 'site-b1', sha), { user: 2 })).status, 200);
      assert.equal((await a.call('GET', a.assetPath('ws-a', 'site-a1', sha), { user: 1 })).status, 200, 'ws-a copy untouched');
      // A site id from another workspace is "not found", not a leak.
      assert.equal((await a.call('GET', a.assetPath('ws-a', 'site-b1', sha), { user: 1 })).json.error, 'site_not_found');
    });

    it('rejects malformed ids and hashes', async () => {
      const a = api(A);
      assert.equal((await a.call('GET', '/api/sites/ws-a/site-a1/assets/xyz', { user: 1 })).json.error, 'invalid_sha256');
      assert.equal((await a.call('GET', `/api/sites/ws-a/site-a1/assets/${'A'.repeat(64)}`, { user: 1 })).json.error, 'invalid_sha256');
      for (const p of ['/api/sites/ws-a/..%2Fx/assets', '/api/sites/WS-A/site-a1/assets', '/api/sites/ws-a/site-a1/assets/a/b']) {
        assert.equal((await a.call('GET', p, { user: 1 })).status, 404, p);
      }
      const r = await a.call('PATCH', a.assetPath('ws-a', 'site-a1', 'a'.repeat(64)), { user: 1, body: '{}', headers: { 'Content-Type': 'application/json' } });
      assert.equal(r.status, 405);
      assert.equal(r.headers.get('allow'), 'GET, PUT, DELETE');
    });
  });

  describe('D1 default (cardless)', () => {
    it('round-trips a multi-chunk asset with safe headers and dedupes', async () => {
      const a = api(A);
      const buf = png(700 * 1024); // 3 chunks of 256 KiB
      const up = await a.put('ws-a', 'site-a1', buf);
      assert.equal(up.status, 201, up.buf.toString());
      assert.deepEqual({ ...up.json, ok: undefined }, { ok: undefined, created: true, sha256: SHA(buf), byte_size: buf.length, content_type: 'image/png', storage_backend: 'd1' });
      const dl = await a.call('GET', a.assetPath('ws-a', 'site-a1', SHA(buf)), { user: 1 });
      assert.equal(dl.status, 200);
      assert.ok(dl.buf.equals(buf), 'downloaded bytes are identical');
      assert.equal(dl.headers.get('content-type'), 'image/png');
      assert.equal(dl.headers.get('x-content-type-options'), 'nosniff');
      assert.equal(dl.headers.get('cache-control'), 'no-store');
      assert.match(dl.headers.get('content-disposition'), /^attachment;/);
      assert.match(dl.headers.get('content-security-policy'), /frame-ancestors 'none'/);
      const again = await a.put('ws-a', 'site-a1', buf);
      assert.equal(again.status, 200);
      assert.equal(again.json.created, false);
      assert.equal((await a.call('GET', a.assetPath('ws-a', 'site-a1', 'b'.repeat(64)), { user: 1 })).json.error, 'asset_not_found');
    });

    it('stores exactly 1 MiB and refuses 1 MiB + 1 on ACTUAL streamed bytes', async () => {
      const a = api(A);
      const exact = png(MIB);
      assert.equal((await a.put('ws-a', 'site-a1', exact)).status, 201);
      assert.ok((await a.call('GET', a.assetPath('ws-a', 'site-a1', SHA(exact)), { user: 1 })).buf.equals(exact));

      const before = (await a.list('ws-a', 'site-a1')).json.usage;
      // Declared Content-Length over the cap: refused before the body is read.
      const declared = await a.put('ws-a', 'site-a1', png(MIB + 1));
      assert.equal(declared.status, 413);
      assert.equal(declared.json.error, 'asset_too_large');
      assert.equal(declared.json.details.max_asset_bytes, MIB);
      // No Content-Length (chunked): the stream itself is cut at the cap.
      const sha = 'c'.repeat(64);
      const chunked = await a.call('PUT', a.assetPath('ws-a', 'site-a1', sha), { user: 1, headers: { 'Content-Type': 'image/png' }, body: streamBody(MIB + 1) });
      assert.equal(chunked.status, 413, chunked.buf.toString());
      assert.equal(chunked.json.error, 'asset_too_large');
      // Reservations were released: usage did not move.
      assert.deepEqual((await a.list('ws-a', 'site-a1')).json.usage, before);
    });

    it('reads only the declared Content-Length from the wire, refuses a hash mismatch, and releases on a truncated body', async () => {
      const a = api(A);
      const real = png(5000);
      const head = real.subarray(0, 100);
      // Declared 100, 5000 bytes on the wire: only the declared 100 bytes are the body.
      const lie = await rawPut(A.port, a.assetPath('ws-a', 'site-a4', SHA(head)), { 'X-Sws-Dev-User': '1', 'Content-Type': 'image/png', 'Content-Length': '100', Connection: 'close' }, real);
      assert.equal(lie.status, 201, lie.body);
      const stored = await a.call('GET', a.assetPath('ws-a', 'site-a4', SHA(head)), { user: 1 });
      assert.equal(stored.buf.length, 100, 'extra wire bytes were never stored');
      // The bytes do not hash to the address: refused, nothing stored.
      const mismatch = await rawPut(A.port, a.assetPath('ws-a', 'site-a4', SHA(real)), { 'X-Sws-Dev-User': '1', 'Content-Type': 'image/png', 'Content-Length': '100', Connection: 'close' }, real);
      assert.equal(mismatch.status, 422, mismatch.body);
      // Declared 4000 but the client hangs up after 200 bytes: nothing stored, space not leaked.
      const short = png(4000);
      await rawPut(A.port, a.assetPath('ws-a', 'site-a4', SHA(short)), { 'X-Sws-Dev-User': '1', 'Content-Type': 'image/png', 'Content-Length': '4000', Connection: 'close' }, short.subarray(0, 200), { closeAfterSend: true }).catch(() => {});
      await sleep(500);
      assert.equal((await a.call('GET', a.assetPath('ws-a', 'site-a4', SHA(short)), { user: 1 })).status, 404);
      // Whether the Worker released it itself or it must expire, a reconcile after the TTL clears it.
      assert.equal((await a.reconcile('ws-a', 'site-a4', { now: PAST_TTL })).status, 200);
      const u = (await a.list('ws-a', 'site-a4', 1)).json.usage;
      assert.equal(u.site_bytes, 100, 'only the 100-byte asset remains accounted');
    });

    it('refuses a commit that happens after the reservation expired: nothing stored, reservation released', async () => {
      const a = api(A);
      const before = (await a.list('ws-a', 'site-a4')).json.usage;
      const buf = png(2000);
      // The request STARTS inside the TTL, but the commit clock is read 700 s later
      // (X-Sws-Dev-Commit-Skew simulates a body that took longer than the reservation).
      const slow = await a.put('ws-a', 'site-a4', buf, { headers: { 'X-Sws-Dev-Commit-Skew': String(PAST_TTL) } });
      assert.equal(slow.status, 409, slow.buf.toString());
      assert.equal(slow.json.error, 'reservation_expired');
      assert.equal((await a.call('GET', a.assetPath('ws-a', 'site-a4', SHA(buf)), { user: 1 })).status, 404, 'nothing stored');
      assert.deepEqual((await a.list('ws-a', 'site-a4')).json.usage, before, 'reservation released, no chunks left');
      const fast = await a.put('ws-a', 'site-a4', buf);
      assert.equal(fast.status, 201, 'the same bytes commit fine when the clock has not moved');
      await a.call('DELETE', a.assetPath('ws-a', 'site-a4', SHA(buf)), { user: 1 });
    });

    it('refuses an upload that would exceed the workspace cap or the per-site file count', async () => {
      const a = api(A);
      const ws = await a.put('ws-d', 'site-d4', png(100));
      assert.equal(ws.status, 409, ws.buf.toString());
      assert.equal(ws.json.error, 'workspace_quota_exceeded');
      assert.ok(ws.json.message.length > 10, 'actionable hint');
      assert.equal(ws.json.details.usage.workspace_bytes, 150 * MIB);
      const count = await a.put('ws-e', 'site-e1', png(100));
      assert.equal(count.status, 409, count.buf.toString());
      assert.equal(count.json.error, 'site_asset_count_exceeded');
      assert.equal(count.json.details.usage.site_assets, 500);
      // Refusals reserved nothing.
      assert.equal((await a.list('ws-d', 'site-d4')).json.usage.site_bytes, 0);
    });

    it('validates content type, sniffed bytes, hash and framing with clear codes', async () => {
      const a = api(A);
      const cases = [
        ['svg is not allowlisted', () => a.put('ws-a', 'site-a1', png(100), { type: 'image/svg+xml' }), 415, 'unsupported_content_type'],
        ['html bytes declared png', () => a.put('ws-a', 'site-a1', Buffer.from('<script>alert(1)</script>'), { type: 'image/png' }), 415, 'content_type_mismatch'],
        ['png bytes declared jpeg', () => a.put('ws-a', 'site-a1', png(100), { type: 'image/jpeg' }), 415, 'content_type_mismatch'],
        ['hash does not match', () => a.put('ws-a', 'site-a1', png(100), { sha: 'd'.repeat(64) }), 422, 'sha256_mismatch'],
        ['empty body', () => a.put('ws-a', 'site-a1', Buffer.alloc(0), { sha: SHA(Buffer.alloc(0)) }), 400, 'empty_asset'],
      ];
      for (const [name, f, status, code] of cases) {
        const r = await f();
        assert.equal(r.status, status, `${name}: ${r.buf.toString()}`);
        assert.equal(r.json.error, code, name);
        assert.ok(r.json.message.length > 10, `${name}: actionable message`);
      }
    });

    it('identical parallel uploads store one asset and count it once', async () => {
      const a = api(A);
      const buf = png(300 * 1024);
      const before = (await a.list('ws-a', 'site-a2')).json.usage.site_bytes;
      const results = await Promise.all(Array.from({ length: 8 }, () => a.put('ws-a', 'site-a2', buf)));
      assert.ok(results.every((r) => r.status === 200 || r.status === 201));
      assert.equal(results.filter((r) => r.status === 201).length, 1, 'exactly one creator');
      const l = (await a.list('ws-a', 'site-a2')).json;
      assert.equal(l.usage.site_bytes - before, buf.length);
      assert.equal(l.assets.filter((x) => x.sha256 === SHA(buf)).length, 1);
    });

    it('PARALLEL uploads can never over-reserve the 50 MiB site quota', async () => {
      const a = api(A);
      // site-a2 starts with ~300 KiB; use site-a4's sibling-free budget instead: a fresh site.
      const start = (await a.list('ws-a', 'site-a2')).json.usage.site_bytes;
      const room = 50 * MIB - start;
      const fit = Math.floor(room / MIB);
      const total = fit + 12;
      const payloads = Array.from({ length: total }, () => png(MIB));
      const results = await Promise.all(payloads.map((p) => a.put('ws-a', 'site-a2', p)));
      const created = results.filter((r) => r.status === 201).length;
      const refused = results.filter((r) => r.status === 409);
      assert.equal(created, fit, `exactly ${fit} 1 MiB assets fit; statuses: ${results.map((r) => r.status).join(',')}`);
      assert.equal(refused.length, total - fit);
      assert.ok(refused.every((r) => r.json.error === 'site_quota_exceeded' && r.json.details.limits.max_site_bytes === 50 * MIB));
      const l = (await a.list('ws-a', 'site-a2')).json;
      const stored = l.assets.reduce((n, x) => n + x.byte_size, 0);
      assert.ok(stored <= 50 * MIB, 'stored bytes never exceed the cap');
      assert.equal(l.usage.site_bytes, stored, 'no reservation left behind');
      // Free one asset; a new upload fits again.
      const victim = results.find((r) => r.status === 201).json.sha256;
      assert.equal((await a.call('DELETE', a.assetPath('ws-a', 'site-a2', victim), { user: 1 })).status, 200);
      assert.equal((await a.put('ws-a', 'site-a2', png(MIB))).status, 201);
      assert.equal((await a.put('ws-a', 'site-a2', png(MIB))).json.error, 'site_quota_exceeded');
    });

    it('delete frees space, is idempotent-404, and downloads of damaged assets fail closed', async () => {
      const a = api(A);
      const buf = png(1000);
      await a.put('ws-a', 'site-a1', buf);
      const used = (await a.list('ws-a', 'site-a1')).json.usage.site_bytes;
      assert.equal((await a.call('DELETE', a.assetPath('ws-a', 'site-a1', SHA(buf)), { user: 1 })).status, 200);
      assert.equal((await a.call('GET', a.assetPath('ws-a', 'site-a1', SHA(buf)), { user: 1 })).status, 404);
      assert.equal((await a.call('DELETE', a.assetPath('ws-a', 'site-a1', SHA(buf)), { user: 1 })).status, 404);
      assert.equal((await a.list('ws-a', 'site-a1')).json.usage.site_bytes, used - 1000);
      // Seeded asset row on ws-c/site-c1 has no chunk rows: never served.
      const bad = await a.call('GET', a.assetPath('ws-c', 'site-c1', '1'.repeat(64)), { user: 1 });
      assert.equal(bad.status, 502);
      assert.equal(bad.json.error, 'store_failed');
    });

    it('reports cardless diagnostics and the provisional envelope', async () => {
      const l = (await api(A).list('ws-a', 'site-a1')).json;
      assert.equal(l.backend, 'd1');
      assert.equal(l.r2_bound, false);
      assert.equal(l.provisional, true);
      assert.deepEqual(l.limits, { max_asset_bytes: MIB, max_site_bytes: 50 * MIB, max_workspace_bytes: 150 * MIB, max_site_assets: 500, max_database_blob_bytes: 400 * MIB });
      assert.deepEqual(l.generated.policy.map((p) => p.kind), ['preview', 'export']);
    });

    it('reconciles stale reservations, orphan chunks, incomplete assets and generated retention', async () => {
      const a = api(A);
      const before = (await a.list('ws-c', 'site-c1')).json;
      // Expired reservations never counted; the live one does.
      assert.equal(before.usage.site_bytes, 10 + 1024);
      const r = await a.reconcile('ws-c', 'site-c1');
      assert.equal(r.status, 200, r.buf.toString());
      assert.deepEqual(r.json.reconciled, {
        expired_reservations: 2,
        expired_generated: 1,
        excess_generated: 2, // 5 live previews, keep_latest = 3
        incomplete_assets: 1,
        orphan_chunks: 2,
        tombstoned_objects: 0,
        orphan_objects: 0,
      });
      assert.equal(r.json.more, false);
      const after = (await a.list('ws-c', 'site-c1')).json;
      assert.equal(after.usage.site_bytes, 1024, 'the incomplete asset is gone; the live reservation stays');
      assert.equal(after.generated.usage.find((g) => g.kind === 'preview').artifacts, 3);
      // Idempotent.
      const again = await a.reconcile('ws-c', 'site-c1');
      assert.ok(Object.values(again.json.reconciled).every((n) => n === 0), JSON.stringify(again.json));
      // Past the live reservation's expiry (dev clock) it is swept too.
      const far = await a.reconcile('ws-c', 'site-c1', { now: 100 * 365 * 24 * 3600 });
      assert.equal(far.json.reconciled.expired_reservations, 1);
    });
  });

  describe('opt-in R2 (local simulated bucket)', () => {
    it('reports R2 mode and stores/serves bytes from R2, above the D1 cap', async () => {
      const b = api(B);
      const l = (await b.list('ws-a', 'site-a1')).json;
      assert.equal(l.backend, 'r2');
      assert.equal(l.r2_bound, true);
      assert.equal(l.limits.max_asset_bytes, 5 * MIB);
      assert.equal(l.limits.max_database_blob_bytes, null, 'R2 bytes are not in D1');
      const buf = png(3 * MIB); // refused by the 1 MiB D1 cap, fine for opt-in R2
      const up = await b.put('ws-a', 'site-a1', buf);
      assert.equal(up.status, 201, up.buf.toString());
      assert.equal(up.json.storage_backend, 'r2');
      const dl = await b.call('GET', b.assetPath('ws-a', 'site-a1', SHA(buf)), { user: 1 });
      assert.ok(dl.buf.equals(buf));
      assert.equal(dl.headers.get('x-content-type-options'), 'nosniff');
      // Per-asset cap on ACTUAL bytes, with and without Content-Length.
      assert.equal((await b.put('ws-a', 'site-a1', png(5 * MIB + 1))).json.error, 'asset_too_large');
      const chunked = await b.call('PUT', b.assetPath('ws-a', 'site-a1', 'e'.repeat(64)), { user: 1, headers: { 'Content-Type': 'image/png' }, body: streamBody(5 * MIB + 1) });
      assert.equal(chunked.status, 413);
      assert.equal((await b.list('ws-a', 'site-a1')).json.usage.site_bytes, buf.length, 'failed uploads reserved nothing lasting');
      // Tenant denial applies to R2 too.
      assert.equal((await b.call('GET', b.assetPath('ws-b', 'site-b1', SHA(buf)), { user: 1 })).status, 403);
    });

    it('PARALLEL R2 uploads: distinct ones all land, identical ones store once, no reservation leaks', async () => {
      const b = api(B);
      const distinct = Array.from({ length: 10 }, () => png(2 * MIB));
      const same = png(2 * MIB);
      const before = (await b.list('ws-a', 'site-a2')).json.usage.site_bytes;
      const results = await Promise.all([...distinct.map((p) => b.put('ws-a', 'site-a2', p)), ...Array.from({ length: 6 }, () => b.put('ws-a', 'site-a2', same))]);
      assert.ok(results.every((r) => r.status === 200 || r.status === 201), results.map((r) => r.status).join());
      assert.equal(results.slice(0, 10).filter((r) => r.status === 201).length, 10);
      assert.equal(results.slice(10).filter((r) => r.status === 201).length, 1);
      const l = (await b.list('ws-a', 'site-a2')).json;
      assert.equal(l.usage.site_bytes - before, 11 * 2 * MIB, 'accounted exactly once each, nothing reserved');
      for (const p of [...distinct.slice(0, 3), same]) {
        const dl = await b.call('GET', b.assetPath('ws-a', 'site-a2', SHA(p)), { user: 1 });
        assert.ok(dl.buf.equals(p));
      }
    });

    it('cleans up: delete removes the object, reconcile sweeps only OLD unreferenced objects', async () => {
      const b = api(B);
      const keep = png(1000);
      const gone = png(1100);
      await b.put('ws-a', 'site-a3', keep);
      await b.put('ws-a', 'site-a3', gone);
      assert.equal((await b.call('DELETE', b.assetPath('ws-a', 'site-a3', SHA(gone)), { user: 1 })).status, 200);
      // An orphaned object: put straight into the local bucket with no asset row.
      const orphan = png(1200);
      const file = join(STATE, 'orphan.bin');
      writeFileSync(file, orphan);
      wr(['r2', 'object', 'put', `sws-studio-assets-test-media/ws-a/site-a3/${SHA(orphan)}/orphan1`, '--file', file, '--local', '-c', B.cfg, '--persist-to', B.persist]);
      // Young: could be an in-flight upload, so it is kept.
      const young = await b.reconcile('ws-a', 'site-a3');
      assert.equal(young.json.reconciled.orphan_objects, 0, JSON.stringify(young.json));
      // Past the reservation TTL it is swept; the referenced object is not.
      const old = await b.reconcile('ws-a', 'site-a3', { now: PAST_TTL });
      assert.equal(old.json.reconciled.orphan_objects, 1, JSON.stringify(old.json));
      assert.equal((await b.reconcile('ws-a', 'site-a3', { now: PAST_TTL })).json.reconciled.orphan_objects, 0);
      assert.ok((await b.call('GET', b.assetPath('ws-a', 'site-a3', SHA(keep)), { user: 1 })).buf.equals(keep));
    });
  });

  describe('opt-in R2: verification, expiry, races and paging', () => {
    it('streams an R2 upload with a Content-Length, and keeps nothing when verification fails', async () => {
      const b = api(B);
      const good = png(2 * MIB);
      assert.equal((await b.put('ws-a', 'site-a4', good)).status, 201, 'streamed (Content-Length known)');
      assert.ok((await b.call('GET', b.assetPath('ws-a', 'site-a4', SHA(good)), { user: 1 })).buf.equals(good));
      // Hash mismatch: the object is written while streaming and must be deleted again.
      const wrong = await b.put('ws-a', 'site-a4', png(3000), { sha: 'f'.repeat(64) });
      assert.equal(wrong.status, 422, wrong.buf.toString());
      assert.equal(wrong.json.error, 'sha256_mismatch');
      // Wrong type is caught from the first 12 bytes, while streaming.
      const jpeg = await b.put('ws-a', 'site-a4', png(3000), { type: 'image/jpeg' });
      assert.equal(jpeg.json.error, 'content_type_mismatch');
      // No object may be left behind: nothing orphaned, even long after the TTL.
      const sweep = await b.reconcile('ws-a', 'site-a4', { now: PAST_TTL });
      assert.equal(sweep.json.reconciled.orphan_objects, 0, JSON.stringify(sweep.json));
      assert.ok((await b.call('GET', b.assetPath('ws-a', 'site-a4', SHA(good)), { user: 1 })).buf.equals(good), 'the referenced object survives');
      await b.call('DELETE', b.assetPath('ws-a', 'site-a4', SHA(good)), { user: 1 });
    });

    it('refuses an R2 commit after the reservation expired and removes the object it wrote', async () => {
      const b = api(B);
      const before = (await b.list('ws-a', 'site-a4')).json.usage;
      const buf = png(100 * 1024);
      const skew = { headers: { 'X-Sws-Dev-Commit-Skew': String(PAST_TTL) } };
      const streamed = await b.put('ws-a', 'site-a4', buf, skew);
      assert.equal(streamed.status, 409, streamed.buf.toString());
      assert.equal(streamed.json.error, 'reservation_expired');
      // Buffered path (chunked, no Content-Length) too.
      const sha = SHA(buf);
      const buffered = await b.call('PUT', b.assetPath('ws-a', 'site-a4', sha), { user: 1, headers: { 'Content-Type': 'image/png', ...skew.headers }, body: Readable.toWeb(Readable.from([buf])) });
      assert.equal(buffered.status, 409, buffered.buf.toString());
      assert.equal(buffered.json.error, 'reservation_expired');
      assert.equal((await b.call('GET', b.assetPath('ws-a', 'site-a4', sha), { user: 1 })).status, 404);
      assert.deepEqual((await b.list('ws-a', 'site-a4')).json.usage, before, 'reservations released');
      // Had the refused objects been left behind, an old-enough sweep would count them.
      assert.equal((await b.reconcile('ws-a', 'site-a4', { now: PAST_TTL })).json.reconciled.orphan_objects, 0);
    });

    it('releases the reservation when the R2 put itself fails (streamed and buffered)', async () => {
      const b = api(B);
      const before = (await b.list('ws-a', 'site-a4')).json.usage;
      const fail = { 'X-Sws-Dev-Fail-Put': '1' };
      const streamed = await b.put('ws-a', 'site-a4', png(50 * 1024), { headers: fail });
      assert.equal(streamed.status, 500);
      const buf = png(50 * 1024);
      const buffered = await b.call('PUT', b.assetPath('ws-a', 'site-a4', SHA(buf)), { user: 1, headers: { 'Content-Type': 'image/png', ...fail }, body: Readable.toWeb(Readable.from([buf])) });
      assert.equal(buffered.status, 500);
      assert.deepEqual((await b.list('ws-a', 'site-a4')).json.usage, before, 'nothing reserved or stored');
    });

    it('a delete racing a re-upload of identical bytes never removes the live object', async () => {
      const b = api(B);
      for (let i = 0; i < 8; i++) {
        const buf = png(20 * 1024);
        const sha = SHA(buf);
        assert.equal((await b.put('ws-a', 'site-a4', buf)).status, 201);
        const [del, up] = await Promise.all([b.call('DELETE', b.assetPath('ws-a', 'site-a4', sha), { user: 1 }), b.put('ws-a', 'site-a4', buf)]);
        assert.ok([200, 404].includes(del.status), `delete ${del.status}`);
        assert.ok([200, 201].includes(up.status), `upload ${up.status}`);
        const dl = await b.call('GET', b.assetPath('ws-a', 'site-a4', sha), { user: 1 });
        // Either the delete ran last (gone) or the re-upload did (intact): never a dangling row.
        assert.ok(dl.status === 404 || (dl.status === 200 && dl.buf.equals(buf)), `download ${dl.status}: ${dl.buf.toString().slice(0, 120)}`);
        if (dl.status === 200) {
          await b.reconcile('ws-a', 'site-a4', { now: PAST_TTL });
          assert.ok((await b.call('GET', b.assetPath('ws-a', 'site-a4', sha), { user: 1 })).buf.equals(buf), 'a sweep never removes a referenced object');
          await b.call('DELETE', b.assetPath('ws-a', 'site-a4', sha), { user: 1 });
        }
      }
    });

    it('pages the orphan sweep with a cursor, so orphans past the first page are removed', async () => {
      const b = api(B);
      const keep = png(2000);
      assert.equal((await b.put('ws-a', 'site-a4', keep)).status, 201);
      for (let i = 0; i < 5; i++) {
        const orphan = png(300 + i);
        const file = join(STATE, `orphan-${i}.bin`);
        writeFileSync(file, orphan);
        wr(['r2', 'object', 'put', `sws-studio-assets-test-media/ws-a/site-a4/${SHA(orphan)}/orph${i}`, '--file', file, '--local', '-c', B.cfg, '--persist-to', B.persist]);
      }
      let cursor;
      let swept = 0;
      let pages = 0;
      do {
        const r = await b.reconcile('ws-a', 'site-a4', { now: PAST_TTL, cursor, headers: { 'X-Sws-Dev-Batch': '2' } });
        assert.equal(r.status, 200, r.buf.toString());
        swept += r.json.reconciled.orphan_objects;
        cursor = r.json.next_cursor ?? undefined;
        pages++;
        assert.ok(pages < 20, 'cursor must advance');
      } while (cursor);
      assert.equal(swept, 5, 'every orphan, across pages of 2');
      assert.ok(pages >= 3, `pages ${pages}`);
      assert.ok((await b.call('GET', b.assetPath('ws-a', 'site-a4', SHA(keep)), { user: 1 })).buf.equals(keep), 'the referenced object was kept');
      await b.call('DELETE', b.assetPath('ws-a', 'site-a4', SHA(keep)), { user: 1 });
    });
  });

  describe('R2 requested but not bound', () => {
    it('answers r2_not_configured with an actionable hint and never falls back silently', async () => {
      const c = api(C);
      const up = await c.put('ws-a', 'site-a1', png(100));
      assert.equal(up.status, 503);
      assert.equal(up.json.error, 'r2_not_configured');
      assert.match(up.json.message, /MEDIA_BUCKET/);
      const l = (await c.list('ws-a', 'site-a1')).json;
      assert.equal(l.backend, null);
      assert.equal(l.backend_error.error, 'r2_not_configured');
      assert.equal(l.r2_bound, false);
    });
  });
});
