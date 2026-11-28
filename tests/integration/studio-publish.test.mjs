// Local integration test for Studio publish jobs (issue #64). Server-dependent: NOT part
// of `npm run verify`; CI runs it in the `web` job (`npm run test:studio:publish`).
//
// It builds the Studio Worker WITH the test-only `insecure-dev-auth` feature into its own
// output directory (crates/sws-worker-studio/build-insecure-dev-auth/), then starts two
// local `wrangler dev` instances on their own ports and their own `--persist-to` state
// (nothing is shared with `npm run studio:dev`; nothing touches Cloudflare):
//   A  :8798  the cardless default: NO Queue. Started with --test-scheduled, so
//             /__scheduled runs the real Cron Trigger handler that drives jobs.
//   B  :8799  the optional Queue path: a LOCAL simulated PUBLISH_QUEUE producer and
//             consumer (only in this generated test config).
// Every server gets `--var SWS_INSECURE_DEV_AUTH:1` (the dev-auth build's runtime fence;
// no committed config sets it) and is reached on a loopback Host. Each wrangler runs in
// its own process group, which this file stops itself.
//
// Authorization is proved with REAL #65 credentials: browser sessions and scoped CLI
// tokens are minted here (random lookup id + secret) and seeded into local D1 as SHA-256
// digests exactly as the Worker stores them, then presented as `__Host-sws_session` or
// `Authorization: Bearer`. The dev identity (`X-Sws-Dev-User`) is used only for the
// pipeline mechanics after authorization.
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { resolve, join } from 'node:path';
import { parseRustConst } from '../../core/tools/check-headers.mjs';

const ROOT = resolve(import.meta.dirname, '..', '..');
const WRANGLER = join(ROOT, 'node_modules', '.bin', 'wrangler');
const STATE = join(ROOT, '.wrangler', 'studio-publish-test');
const CRATE = join(ROOT, 'crates', 'sws-worker-studio');
const PORTS = { a: 8798, b: 8799 };
const INSPECTOR = { a: 9396, b: 9397 };
const STUDIO_HEADERS = parseRustConst(readFileSync(join(ROOT, 'crates/sws-security/src/headers.rs'), 'utf8'), 'STUDIO_HEADERS');
const LEASE = 120; // publish::job::LEASE_SECS
const BACKOFF_1 = 30; // publish::job::backoff_secs(1)
const SHA = (s) => createHash('sha256').update(s).digest('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const REV_SHA = SHA('revision a1');
/** publish::job_id: the deterministic job id of (workspace, site, request id). */
const jobIdOf = (ws, site, req) => `j${SHA(`publish\0${ws}\0${site}\0${req}`).slice(0, 47)}`;
const REV_SHA_A2 = SHA('revision a2');

const servers = [];

// --- Real #65 credentials (auth::secret formats; only digests reach D1) -----------------
const hex = (n) => randomBytes(n).toString('hex');
const isoAgo = (secs) => new Date(Date.now() - secs * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
function mintSession() {
  const id = hex(12);
  const value = `${id}.${hex(32)}`;
  return { id, value, hash: SHA(value) };
}
function mintToken() {
  const id = hex(12);
  const value = `swst_${id}_${hex(32)}`;
  return { id, value, hash: SHA(value) };
}
/** Credentials per server, minted when the server is seeded (so "fresh" is measured from then). */
function credentialsSql() {
  const c = {
    ownerFresh: { ...mintSession(), user: 1, ageSecs: 60 },
    // Signed in 20 minutes ago: valid, but outside the 15-minute re-auth window (#60).
    ownerStale: { ...mintSession(), user: 1, ageSecs: 20 * 60 },
    editorFresh: { ...mintSession(), user: 3, ageSecs: 60 },
    editToken: { ...mintToken(), user: 4, ws: 'ws-a', scope: 'edit' },
    publishToken: { ...mintToken(), user: 4, ws: 'ws-a', scope: 'publish' },
    activateToken: { ...mintToken(), user: 1, ws: 'ws-a', scope: 'activate' },
    // dave is a publisher in BOTH workspaces; this token is bound to ws-b.
    otherWsToken: { ...mintToken(), user: 4, ws: 'ws-b', scope: 'publish' },
  };
  const rows = [];
  for (const v of Object.values(c)) {
    if (v.ageSecs !== undefined) {
      const created = isoAgo(v.ageSecs);
      rows.push(
        `INSERT INTO auth_sessions (id, user_id, secret_hash, created_at, last_seen_at, expires_at) VALUES ('${v.id}', ${v.user}, '${v.hash}', '${created}', '${isoAgo(30)}', '${isoAgo(v.ageSecs - 86_400)}');`,
      );
    } else {
      const created = isoAgo(60);
      rows.push(
        `INSERT INTO api_tokens (id, workspace_id, user_id, token_hash, scope, label, created_at, expires_at, chain_started_at) VALUES ('${v.id}', '${v.ws}', ${v.user}, '${v.hash}', '${v.scope}', 'publish-test', '${created}', '${isoAgo(60 - 23 * 3600)}', '${created}');`,
      );
    }
  }
  return { creds: c, sql: rows.join('\n') };
}

function writeConfig(name, { queue }) {
  const dir = join(STATE, name);
  mkdirSync(dir, { recursive: true });
  const cfg = join(dir, 'wrangler.jsonc');
  writeFileSync(
    cfg,
    JSON.stringify({
      name: `sws-studio-publish-test-${name}`,
      main: join(CRATE, 'build-insecure-dev-auth', 'index.js'),
      compatibility_date: '2026-11-01',
      d1_databases: [
        {
          binding: 'STUDIO_DB',
          database_name: 'sws-studio-publish-test',
          migrations_dir: join(CRATE, 'migrations'),
          database_id: '00000000-0000-0000-0000-000000000064',
        },
      ],
      ...(queue
        ? {
            queues: {
              producers: [{ binding: 'PUBLISH_QUEUE', queue: 'sws-studio-publish-test' }],
              consumers: [{ queue: 'sws-studio-publish-test', max_batch_size: 10, max_batch_timeout: 1, max_retries: 3 }],
            },
          }
        : {}),
      // Cookie-borne writes must come from this origin (the #65 CSRF gate).
      vars: { STUDIO_VERSION: 'publish-test', STUDIO_PUBLIC_ORIGIN: `http://127.0.0.1:${PORTS[name]}` },
    }),
  );
  return { dir, cfg, persist: join(dir, 'state') };
}

function wr(args) {
  const r = spawnSync(WRANGLER, args, {
    cwd: ROOT,
    encoding: 'utf8',
    env: { ...process.env, CI: '1', WRANGLER_SEND_METRICS: 'false' },
  });
  assert.equal(r.status, 0, `wrangler ${args.join(' ')} failed:\n${r.stdout}\n${r.stderr}`);
  return r.stdout;
}

const OLD = '2020-01-01T00:00:00Z';
const SEED = `
INSERT INTO workspaces (id, name) VALUES ('ws-a', 'Workspace A'), ('ws-b', 'Workspace B');
INSERT INTO users (github_id, github_login) VALUES (1001, 'alice'), (2002, 'bob'), (3003, 'carol'), (4004, 'dave');
INSERT INTO memberships (workspace_id, user_id, role) VALUES
  ('ws-a', 1, 'owner'), ('ws-a', 3, 'editor'), ('ws-a', 4, 'publisher'), ('ws-b', 2, 'owner'), ('ws-b', 4, 'publisher');
INSERT INTO sites (workspace_id, id, slug, name) VALUES
  ('ws-a', 'site-a1', 'one', 'One'), ('ws-a', 'site-a2', 'two', 'Two'), ('ws-a', 'site-a3', 'three', 'Three'),
  ('ws-a', 'site-seed', 'seed', 'Seed'), ('ws-b', 'site-b1', 'bee', 'Bee');
INSERT INTO site_revisions (workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) VALUES
  ('ws-a', 'site-a1', 'rev-a1', 1, '${REV_SHA}', 100, 1),
  ('ws-a', 'site-a2', 'rev-a2', 1, '${REV_SHA_A2}', 100, 1),
  ('ws-a', 'site-a3', 'rev-a3', 1, '${REV_SHA}', 100, 1),
  ('ws-a', 'site-seed', 'rev-seed', 1, '${REV_SHA}', 100, 1),
  ('ws-b', 'site-b1', 'rev-b1', 1, '${REV_SHA}', 100, 2);
-- Rows in the exact states a crashed or exhausted driver leaves behind, and a server-origin job:
INSERT INTO publish_jobs (workspace_id, site_id, id, revision_id, request_id, origin, phase, attempt, next_attempt_at, lease_expires_at, created_by, created_at, updated_at) VALUES
  ('ws-a', 'site-seed', 'seed-crashed', 'rev-seed', 'seed-crashed', 'cli', 'uploading', 1, '${OLD}', '${OLD}', 1, '${OLD}', '${OLD}'),
  ('ws-a', 'site-seed', 'seed-exhausted', 'rev-seed', 'seed-exhausted', 'cli', 'validating', 5, '${OLD}', '${OLD}', 1, '${OLD}', '${OLD}'),
  ('ws-a', 'site-seed', 'seed-server', 'rev-seed', 'seed-server', 'server', 'queued', 0, '${OLD}', NULL, 1, '${OLD}', '${OLD}');
`;

async function startServer(key, { queue }) {
  const { cfg, persist, dir } = writeConfig(key, { queue });
  wr(['d1', 'migrations', 'apply', 'STUDIO_DB', '--local', '-c', cfg, '--persist-to', persist]);
  const file = join(dir, 'seed.sql');
  const { creds, sql: credSql } = credentialsSql();
  writeFileSync(file, `${SEED}\n${credSql}\n`);
  wr(['d1', 'execute', 'STUDIO_DB', '--local', '-c', cfg, '--persist-to', persist, '--file', file]);
  const port = PORTS[key];
  const args = ['dev', '-c', cfg, '--port', String(port), '--ip', '127.0.0.1', '--inspector-port', String(INSPECTOR[key]), '--persist-to', persist, '--test-scheduled'];
  // The dev-auth build's runtime fence (the other half is the loopback Host).
  args.push('--var', 'SWS_INSECURE_DEV_AUTH:1');
  const child = spawn(WRANGLER, args, {
    cwd: ROOT,
    detached: true, // own process group so the whole wrangler/workerd tree can be stopped
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, CI: '1', WRANGLER_SEND_METRICS: 'false' },
  });
  let log = '';
  child.stdout.on('data', (d) => (log += d));
  child.stderr.on('data', (d) => (log += d));
  const server = { key, port, base: `http://127.0.0.1:${port}`, child, cfg, persist, creds, log: () => log };
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

/** One HTTP request on a FRESH connection (`agent: false`), like scripts/test-studio-auth.mjs:
 *  the first request after local wrangler D1/Queue writes can otherwise reuse a pooled
 *  keep-alive socket the server already closed and fail with `fetch failed`. */
function request(port, method, path, headers, body) {
  return new Promise((resolveP, reject) => {
    const h = { ...headers };
    if (body !== undefined && h['Content-Length'] === undefined) h['Content-Length'] = Buffer.byteLength(body);
    const req = http.request({ host: '127.0.0.1', port, path, method, headers: h, agent: false }, (r) => {
      let text = '';
      r.setEncoding('utf8');
      r.on('data', (d) => (text += d));
      r.on('end', () =>
        resolveP({
          status: r.statusCode,
          text,
          headers: {
            get: (n) => {
              const v = r.headers[n.toLowerCase()];
              return v === undefined ? null : Array.isArray(v) ? v.join(', ') : v;
            },
          },
        }),
      );
    });
    req.on('error', reject);
    req.setTimeout(30_000, () => req.destroy(new Error('timeout')));
    if (body !== undefined) req.write(body);
    req.end();
  });
}

/** Request helpers. `user` is the dev-auth user id (`null` sends no dev identity);
 *  `session` sends a real `__Host-sws_session` cookie (with the Studio `Origin` unless
 *  `origin` is given; `origin: null` omits it); `token` sends `Authorization: Bearer`. */
function api(server) {
  const call = async (method, path, { user, session, token, origin, body, headers = {} } = {}) => {
    const h = { ...headers };
    if (user !== undefined && user !== null) h['X-Sws-Dev-User'] = String(user);
    if (token) h.Authorization = `Bearer ${token}`;
    if (session) {
      h.Cookie = `__Host-sws_session=${session}`;
      if (origin !== null) h.Origin = origin ?? server.base;
    }
    if (body !== undefined && !h['Content-Type']) h['Content-Type'] = 'application/json';
    const res = await request(server.port, method, path, h, body);
    let json = null;
    if ((res.headers.get('content-type') || '').includes('application/json')) json = JSON.parse(res.text);
    return { status: res.status, headers: res.headers, text: res.text, json };
  };
  const publish = (ws, site, payload, user = 1, headers = {}, auth = {}) =>
    call('POST', `/api/sites/${ws}/${site}/publish`, { user, ...auth, body: typeof payload === 'string' ? payload : JSON.stringify(payload), headers });
  const status = (ws, site, job, user = 1, auth = {}) => call('GET', `/api/sites/${ws}/${site}/publish/${job}`, { user, ...auth });
  /** The real Cron Trigger handler (`wrangler dev --test-scheduled`). */
  const scheduled = async () => {
    const r = await request(server.port, 'GET', '/__scheduled?cron=*%2F5+*+*+*+*', {});
    assert.equal(r.status, 200, r.text);
  };
  /** Dev-build hook: the same driver with a skewed clock and injected faults. */
  const devDrive = async (query) => {
    const r = await call('POST', `/api/dev/publish-drive?${new URLSearchParams(query)}`, { headers: { 'Content-Length': '0' } });
    assert.equal(r.status, 200, r.text);
    return r.json;
  };
  return { call, publish, status, scheduled, devDrive };
}

function manifest({ requestId = 'req-1', revisionId = 'rev-a1', revisionSha = REV_SHA, files } = {}) {
  const list = files ?? [
    { path: 'index.html', sha256: SHA('<h1>hi</h1>'), size: 11 },
    { path: 'styles/site.css', sha256: SHA('body{}'), size: 6 },
  ];
  return {
    request_id: requestId,
    manifest: {
      format: 'sws-artifact-manifest/1',
      revision_id: revisionId,
      revision_sha256: revisionSha,
      compiler: { name: 'sws-cli', version: '0.2.0' },
      files: list,
      file_count: list.length,
      total_bytes: list.reduce((n, f) => n + f.size, 0),
    },
  };
}

/** A request with an explicit Host header (fetch cannot set one). */
function hostRequest(port, method, path, headers, body) {
  return new Promise((resolveP, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path, method, headers, agent: false }, (r) => {
      let text = '';
      r.on('data', (d) => (text += d));
      r.on('end', () => resolveP({ status: r.statusCode, body: text }));
    });
    req.on('error', reject);
    req.setTimeout(10_000, () => req.destroy(new Error('timeout')));
    req.end(body);
  });
}

function assertStudioHeaders(r, label) {
  for (const [k, v] of STUDIO_HEADERS) assert.equal(r.headers.get(k), v, `${k} on ${label}`);
  assert.equal(r.headers.get('cache-control'), 'no-store', `no-store on ${label}`);
}

describe('studio publish jobs (local wrangler dev, insecure-dev-auth build)', { concurrency: false }, () => {
  let A, B;

  before(async () => {
    rmSync(STATE, { recursive: true, force: true });
    const build = spawnSync('node', ['scripts/build-wasm.mjs', '--profile', 'release', '--crate', 'crates/sws-worker-studio', '--features', 'insecure-dev-auth'], {
      cwd: ROOT,
      encoding: 'utf8',
    });
    assert.equal(build.status, 0, `${build.stdout}\n${build.stderr}`);
    A = await startServer('a', { queue: false });
    B = await startServer('b', { queue: true });
  });

  after(() => stopAll());

  describe('authorization (require_publish) and tenant boundary', () => {
    it('is closed without credentials (401) and behind the runtime fence', async () => {
      const a = api(A);
      const r = await a.publish('ws-a', 'site-a1', manifest(), null);
      assert.equal(r.status, 401);
      assert.equal(r.json.error, 'unauthenticated');
      assertStudioHeaders(r, 'POST 401');
      assert.equal((await a.status('ws-a', 'site-a1', 'jx', null)).status, 401);
      const body = JSON.stringify(manifest({ requestId: 'fence' }));
      const fenced = await hostRequest(A.port, 'POST', '/api/sites/ws-a/site-a1/publish', {
        'X-Sws-Dev-User': '1',
        Host: 'studio.example',
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(body),
      }, body);
      assert.equal(fenced.status, 401, 'a non-loopback Host never reaches the dev identity');
    });

    it('refuses an editor (403) and accepts a publisher (202)', async () => {
      const a = api(A);
      const editor = await a.publish('ws-a', 'site-a3', manifest({ requestId: 'by-editor', revisionId: 'rev-a3' }), 3);
      assert.equal(editor.status, 403);
      assert.equal(editor.json.error, 'forbidden_role');
      const pub = await a.publish('ws-a', 'site-a3', manifest({ requestId: 'by-publisher', revisionId: 'rev-a3' }), 4);
      assert.equal(pub.status, 202, pub.text);
      assert.equal((await a.status('ws-a', 'site-a3', pub.json.job_id, 3)).status, 403, 'editors cannot read publish jobs either');
      assert.equal((await a.status('ws-a', 'site-a3', pub.json.job_id, 4)).status, 200);
    });

    it('refuses other workspaces with one uniform 403 and never leaks a job across tenants', async () => {
      const a = api(A);
      const bobs = await a.publish('ws-b', 'site-b1', manifest({ requestId: 'bob-1', revisionId: 'rev-b1' }), 2);
      assert.equal(bobs.status, 202, bobs.text);
      for (const [user, ws, site] of [[2, 'ws-a', 'site-a1'], [99, 'ws-a', 'site-a1'], [1, 'ws-b', 'site-b1'], [1, 'ws-nope', 'site-a1']]) {
        const label = `user ${user} ${ws}/${site}`;
        const p = await a.publish(ws, site, manifest({ requestId: 'cross' }), user);
        assert.equal(p.status, 403, `POST ${label}`);
        assert.equal(p.json.error, 'forbidden', label);
        assert.equal((await a.status(ws, site, bobs.json.job_id, user)).status, 403, `GET ${label}`);
      }
      // A member of ws-a naming bob's job through ws-a's own site: not found.
      assert.equal((await a.status('ws-a', 'site-a1', bobs.json.job_id, 1)).json.error, 'job_not_found');
      // A site id of another workspace, and a revision of another site/workspace.
      assert.equal((await a.publish('ws-a', 'site-b1', manifest({ requestId: 'x1' }), 1)).json.error, 'site_not_found');
      assert.equal((await a.publish('ws-a', 'site-a1', manifest({ requestId: 'x2', revisionId: 'rev-b1' }), 1)).json.error, 'revision_not_found');
      assert.equal((await a.publish('ws-a', 'site-a1', manifest({ requestId: 'x3', revisionId: 'rev-a2' }), 1)).json.error, 'revision_not_found');
    });
  });

  describe('authorization with real #65 credentials (sessions and scoped tokens)', () => {
    const m = (requestId) => manifest({ requestId, revisionId: 'rev-a3' });
    const NO_DEV = null; // no X-Sws-Dev-User: only the real credential is presented

    it('refuses an edit-only token (403 insufficient_scope) on enqueue and on job status', async () => {
      const a = api(A);
      const { editToken } = A.creds;
      const r = await a.publish('ws-a', 'site-a3', m('tok-edit'), NO_DEV, {}, { token: editToken.value });
      assert.equal(r.status, 403, r.text);
      assert.equal(r.json.error, 'insufficient_scope');
      assertStudioHeaders(r, 'POST 403 edit token');
      assert.equal((await a.status('ws-a', 'site-a3', jobIdOf('ws-a', 'site-a3', 'tok-edit'), NO_DEV, { token: editToken.value })).status, 403);
    });

    it('accepts a publish-scoped token (202) and an activate-scoped token, and lets them read the job', async () => {
      const a = api(A);
      const { publishToken, activateToken } = A.creds;
      const r = await a.publish('ws-a', 'site-a3', m('tok-publish'), NO_DEV, {}, { token: publishToken.value });
      assert.equal(r.status, 202, r.text);
      assert.equal(r.json.origin, 'cli');
      assertStudioHeaders(r, 'POST 202 publish token');
      const s = await a.status('ws-a', 'site-a3', r.json.job_id, NO_DEV, { token: publishToken.value });
      assert.equal(s.status, 200, s.text);
      const act = await a.status('ws-a', 'site-a3', r.json.job_id, NO_DEV, { token: activateToken.value });
      assert.equal(act.status, 200, act.text);
    });

    it('refuses a token bound to another workspace (403 forbidden), even for a publisher of both', async () => {
      const a = api(A);
      const { otherWsToken } = A.creds;
      const r = await a.publish('ws-a', 'site-a3', m('tok-other-ws'), NO_DEV, {}, { token: otherWsToken.value });
      assert.equal(r.status, 403, r.text);
      assert.equal(r.json.error, 'forbidden');
      // The same token works in its own workspace.
      const own = await a.publish('ws-b', 'site-b1', manifest({ requestId: 'tok-own-ws', revisionId: 'rev-b1' }), NO_DEV, {}, { token: otherWsToken.value });
      assert.equal(own.status, 202, own.text);
    });

    it('refuses an editor session (403 forbidden_role) and accepts a fresh owner session from the Studio origin', async () => {
      const a = api(A);
      const { editorFresh, ownerFresh } = A.creds;
      const ed = await a.publish('ws-a', 'site-a3', m('sess-editor'), NO_DEV, {}, { session: editorFresh.value });
      assert.equal(ed.status, 403, ed.text);
      assert.equal(ed.json.error, 'forbidden_role');
      const ok = await a.publish('ws-a', 'site-a3', m('sess-owner'), NO_DEV, {}, { session: ownerFresh.value });
      assert.equal(ok.status, 202, ok.text);
      assert.equal((await a.status('ws-a', 'site-a3', ok.json.job_id, NO_DEV, { session: ownerFresh.value, origin: null })).status, 200);
    });

    it('refuses a cookie-borne POST without the Studio Origin (403 forbidden_origin)', async () => {
      const a = api(A);
      const { ownerFresh } = A.creds;
      for (const origin of [null, 'https://evil.example']) {
        const r = await a.publish('ws-a', 'site-a3', m('sess-no-origin'), NO_DEV, {}, { session: ownerFresh.value, origin });
        assert.equal(r.status, 403, `${origin}: ${r.text}`);
        assert.equal(r.json.error, 'forbidden_origin');
      }
      assert.equal((await a.status('ws-a', 'site-a3', jobIdOf('ws-a', 'site-a3', 'sess-no-origin'))).json.error, 'job_not_found');
    });

    it('refuses a session past the 15-minute re-auth window (401 reauth_required)', async () => {
      const a = api(A);
      const { ownerStale } = A.creds;
      const r = await a.publish('ws-a', 'site-a3', m('sess-stale'), NO_DEV, {}, { session: ownerStale.value });
      assert.equal(r.status, 401, r.text);
      assert.equal(r.json.error, 'reauth_required');
    });

    it('an Authorization header is authoritative: a bad bearer is 401 even with a dev identity', async () => {
      const a = api(A);
      const r = await a.publish('ws-a', 'site-a3', m('tok-bad'), 1, {}, { token: `swst_${hex(12)}_${hex(32)}` });
      assert.equal(r.status, 401, r.text);
      assert.equal(r.json.error, 'unauthenticated');
    });
  });

  describe('validation (no job row is written for a refused request)', () => {
    it('refuses hostile manifests with specific codes', async () => {
      const a = api(A);
      const f = (path, size = 1, sha = SHA(path)) => ({ path, sha256: sha, size });
      const cases = [
        [manifest({ requestId: 'v1', files: [f('../etc/passwd')] }), 400, 'invalid_path'],
        [manifest({ requestId: 'v2', files: [f('a.html'), f('a.html')] }), 400, 'duplicate_path'],
        [manifest({ requestId: 'v3', files: [f('_headers')] }), 400, 'reserved_path'],
        [manifest({ requestId: 'v4', files: [f('a.html', 1, 'XYZ')] }), 400, 'invalid_sha256'],
        [manifest({ requestId: 'v5', files: [f('big.bin', 5 * 1024 * 1024 + 1)] }), 400, 'file_too_large'],
        [manifest({ requestId: 'v6', files: Array.from({ length: 1001 }, (_, i) => f(`f${i}`, 0)) }), 400, 'too_many_files'],
        [{ ...manifest({ requestId: 'v7' }), extra: 1 }, 400, 'manifest_malformed'],
        [manifest({ requestId: 'v8', revisionSha: SHA('not the revision') }), 409, 'revision_mismatch'],
        [manifest({ requestId: 'v9', revisionId: 'rev-missing' }), 404, 'revision_not_found'],
        [manifest({ requestId: 'Bad Id' }), 400, 'invalid_request_id'],
      ];
      const m = manifest({ requestId: 'v10' });
      m.manifest.total_bytes += 1;
      cases.push([m, 400, 'total_bytes_mismatch']);
      for (const [body, status, code] of cases) {
        const r = await a.publish('ws-a', 'site-a1', body);
        assert.equal(r.status, status, `${code}: ${r.text}`);
        assert.equal(r.json.error, code);
        assertStudioHeaders(r, code);
      }
      const r = await a.publish('ws-a', 'site-a1', 'not json');
      assert.equal(r.json.error, 'manifest_malformed');
      assert.equal((await a.publish('ws-a', 'site-a1', '{}', 1, { 'Content-Type': 'text/plain' })).status, 415);
      const huge = JSON.stringify({ ...manifest({ requestId: 'huge' }), pad: 'x'.repeat(256 * 1024) });
      assert.equal((await a.publish('ws-a', 'site-a1', huge)).status, 413);
      const wrongMethod = await a.call('GET', '/api/sites/ws-a/site-a1/publish', { user: 1 });
      assert.equal(wrongMethod.status, 405);
      assert.equal(wrongMethod.headers.get('allow'), 'POST');
      // None of the refused requests left a job row behind.
      for (const id of ['v1', 'v2', 'v3', 'v5', 'v6', 'v8', 'v9', 'v10']) {
        assert.equal((await a.status('ws-a', 'site-a1', jobIdOf('ws-a', 'site-a1', id))).json.error, 'job_not_found', id);
      }
    });
  });

  describe('enqueue, scheduled drive, duplicates and recovery (no Queue: the cardless default)', () => {
    let jobId;

    it('enqueue answers 202 with a queued CLI job and never compiles', async () => {
      const a = api(A);
      const r = await a.publish('ws-a', 'site-a2', manifest({ requestId: 'main-1', revisionId: 'rev-a2', revisionSha: REV_SHA_A2 }));
      assert.equal(r.status, 202, r.text);
      assertStudioHeaders(r, 'POST 202');
      assert.equal(r.json.ok, true);
      assert.equal(r.json.created, true);
      assert.equal(r.json.phase, 'queued');
      assert.equal(r.json.origin, 'cli');
      assert.equal(r.json.server_verified, false);
      assert.equal(r.json.attempt, 0);
      assert.equal(r.json.dispatch, 'scheduled', 'no Queue bound: the Cron Trigger picks it up');
      assert.match(r.json.artifact.manifest_sha256, /^[0-9a-f]{64}$/);
      jobId = r.json.job_id;
      // The D1 row itself, read through the status route...
      const s = await a.status('ws-a', 'site-a2', jobId);
      assert.equal(s.status, 200);
      assertStudioHeaders(s, 'GET 200');
      assert.equal(s.json.phase, 'queued');
      assert.equal(s.json.artifact.file_count, 2);
      assert.equal(s.json.artifact.total_bytes, 17);
      assert.equal(s.json.artifact.revision_sha256, REV_SHA_A2);
    });

    it('a retried enqueue returns the same job; the same key with another manifest is a 409', async () => {
      const a = api(A);
      const again = await a.publish('ws-a', 'site-a2', manifest({ requestId: 'main-1', revisionId: 'rev-a2', revisionSha: REV_SHA_A2 }));
      assert.equal(again.status, 202);
      assert.equal(again.json.created, false);
      assert.equal(again.json.job_id, jobId);
      const other = manifest({ requestId: 'main-1', revisionId: 'rev-a2', revisionSha: REV_SHA_A2, files: [{ path: 'index.html', sha256: SHA('x'), size: 1 }] });
      const conflict = await a.publish('ws-a', 'site-a2', other);
      assert.equal(conflict.status, 409);
      assert.equal(conflict.json.error, 'idempotency_conflict');
      // Idempotency is decided before the revision check: a reused key is a conflict even
      // when the new manifest names a missing revision or the wrong revision digest.
      for (const changed of [
        manifest({ requestId: 'main-1', revisionId: 'rev-missing', revisionSha: REV_SHA_A2 }),
        manifest({ requestId: 'main-1', revisionId: 'rev-a2', revisionSha: SHA('another digest') }),
      ]) {
        const r = await a.publish('ws-a', 'site-a2', changed);
        assert.equal(r.status, 409, r.text);
        assert.equal(r.json.error, 'idempotency_conflict');
      }
    });

    it('the scheduled handler drives it to ready', async () => {
      const a = api(A);
      for (let i = 0; i < 5; i++) {
        await a.scheduled();
        if ((await a.status('ws-a', 'site-a2', jobId)).json.phase === 'ready') break;
      }
      const s = (await a.status('ws-a', 'site-a2', jobId)).json;
      assert.equal(s.phase, 'ready');
      assert.equal(s.attempt, 1);
      assert.equal(s.error, null);
      assert.ok(s.finished_at);
      assert.equal(s.server_verified, false, 'a ready CLI job is still not server-verified');
    });

    it('a duplicate delivery of a finished job is a no-op', async () => {
      const a = api(A);
      const before = (await a.status('ws-a', 'site-a2', jobId)).json;
      for (let i = 0; i < 2; i++) {
        const out = await a.devDrive({ job: `ws-a/site-a2/${jobId}` });
        assert.deepEqual(out.jobs[0], { outcome: 'terminal', phase: 'ready' });
      }
      await a.scheduled();
      assert.deepEqual((await a.status('ws-a', 'site-a2', jobId)).json, before);
    });

    it('a driver that dies between phases is recovered after its lease, from the phase it reached', async () => {
      const a = api(A);
      const r = await a.publish('ws-a', 'site-a2', manifest({ requestId: 'crash-1', revisionId: 'rev-a2', revisionSha: REV_SHA_A2 }));
      const id = r.json.job_id;
      const out = await a.devDrive({ job: `ws-a/site-a2/${id}`, crash_after: 'validating' });
      assert.deepEqual(out.jobs[0], { outcome: 'crashed', phase: 'uploading' });
      let s = (await a.status('ws-a', 'site-a2', id)).json;
      assert.deepEqual([s.phase, s.attempt], ['uploading', 1]);
      // The next scheduled run respects the dead driver's lease...
      await a.scheduled();
      s = (await a.status('ws-a', 'site-a2', id)).json;
      assert.deepEqual([s.phase, s.attempt], ['uploading', 1]);
      // ...and once the lease is over the same driver resumes at `uploading`.
      const rec = await a.devDrive({ job: `ws-a/site-a2/${id}`, now: String(LEASE + 1) });
      assert.deepEqual(rec.jobs[0], { outcome: 'stopped', phase: 'ready' });
      s = (await a.status('ws-a', 'site-a2', id)).json;
      assert.deepEqual([s.phase, s.attempt, s.error], ['ready', 2, null]);
    });

    it('a transient failure backs off, is not retried early, then succeeds', async () => {
      const a = api(A);
      const r = await a.publish('ws-a', 'site-a2', manifest({ requestId: 'flaky-1', revisionId: 'rev-a2', revisionSha: REV_SHA_A2 }));
      const id = r.json.job_id;
      const out = await a.devDrive({ job: `ws-a/site-a2/${id}`, transient: 'validating' });
      assert.deepEqual(out.jobs[0], { outcome: 'stopped', phase: 'validating' });
      let s = (await a.status('ws-a', 'site-a2', id)).json;
      assert.deepEqual([s.phase, s.attempt, s.last_error, s.error], ['validating', 1, 'injected_fault', null]);
      const backoff = (Date.parse(s.next_attempt_at) - Date.parse(s.updated_at)) / 1000;
      assert.equal(backoff, BACKOFF_1);
      await a.scheduled();
      assert.equal((await a.status('ws-a', 'site-a2', id)).json.attempt, 1, 'not due: the sweep left it alone');
      const rec = await a.devDrive({ job: `ws-a/site-a2/${id}`, now: String(BACKOFF_1 + 1) });
      assert.deepEqual(rec.jobs[0], { outcome: 'stopped', phase: 'ready' });
      s = (await a.status('ws-a', 'site-a2', id)).json;
      assert.deepEqual([s.phase, s.attempt, s.last_error], ['ready', 2, 'injected_fault']);
    });

    it('the scheduled sweep recovers crashed rows, exhausts spent ones and fails server compilation', async () => {
      const a = api(A);
      for (let i = 0; i < 5; i++) await a.scheduled();
      const crashed = (await a.status('ws-a', 'site-seed', 'seed-crashed')).json;
      assert.deepEqual([crashed.phase, crashed.attempt], ['ready', 2], 'resumed at uploading by the real cron handler');
      const spent = (await a.status('ws-a', 'site-seed', 'seed-exhausted')).json;
      assert.deepEqual([spent.phase, spent.error, spent.last_error], ['failed', { code: 'retries_exhausted', phase: 'validating' }, 'lease_expired']);
      const server = (await a.status('ws-a', 'site-seed', 'seed-server')).json;
      assert.deepEqual([server.phase, server.origin, server.error], ['failed', 'server', { code: 'server_compile_disabled', phase: 'compiling' }]);
    });

    it('caps unfinished jobs per site (429) without writing a row', async () => {
      const a = api(A);
      // Earlier site-a3 jobs were finished by the sweeps above; no sweep runs here.
      for (const id of ['cap-1', 'cap-2', 'cap-3']) {
        const r = await a.publish('ws-a', 'site-a3', manifest({ requestId: id, revisionId: 'rev-a3' }));
        assert.equal(r.status, 202, r.text);
        assert.equal(r.json.job_id, jobIdOf('ws-a', 'site-a3', id));
      }
      const over = await a.publish('ws-a', 'site-a3', manifest({ requestId: 'cap-4', revisionId: 'rev-a3' }));
      assert.equal(over.status, 429);
      assert.equal(over.json.error, 'publish_busy');
      assert.equal(over.headers.get('retry-after'), '60');
      assertStudioHeaders(over, '429');
      assert.equal((await a.status('ws-a', 'site-a3', jobIdOf('ws-a', 'site-a3', 'cap-4'))).json.error, 'job_not_found');
    });
  });

  describe('optional Queue consumer (local simulated Queue)', () => {
    it('enqueue sends a pointer and the consumer drives the job with the same driver', async () => {
      const b = api(B);
      const r = await b.publish('ws-a', 'site-a1', manifest({ requestId: 'queued-1' }));
      assert.equal(r.status, 202, r.text);
      assert.equal(r.json.dispatch, 'queue');
      let s;
      for (let i = 0; i < 40; i++) {
        s = (await b.status('ws-a', 'site-a1', r.json.job_id)).json;
        if (s.phase === 'ready') break;
        await sleep(500);
      }
      assert.equal(s.phase, 'ready', `queue consumer did not drive the job:\n${B.log().slice(-2000)}`);
      assert.equal(s.attempt, 1);
      // A retried enqueue of a finished job sends nothing more.
      const again = await b.publish('ws-a', 'site-a1', manifest({ requestId: 'queued-1' }));
      assert.deepEqual([again.json.created, again.json.dispatch, again.json.phase], [false, 'none', 'ready']);
    });
  });
});
