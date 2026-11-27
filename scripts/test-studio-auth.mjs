#!/usr/bin/env node
// Local integration tests for Studio authentication and scoped CLI/MCP tokens (#65), zero
// dependencies.
//
// Runs the RELEASE Studio Worker (`wrangler dev -c wrangler.studio.jsonc`, the default
// `build/`, no `insecure-dev-auth`: there is no dev identity at all) on its own port with its
// own --persist-to state, pointed by `--var` at a FAKE GitHub OAuth server this script runs on
// a loopback port. Everything else is real: the Worker, the session Durable Object with
// hibernation, local D1, Turnstile siteverify (always-pass TEST secret with DEV_MODE, so the
// invitation step needs network access to challenges.cloudflare.com).
//
// Phase 1 (fast knobs: membership recheck 1 s, idle sweep 1 s): OAuth sign-in with owner
// bootstrap, non-owner first sign-in refused, state single use and browser binding, invitation
// form (Turnstile) and acceptance bound to the invited login, session cookie attributes, CSRF
// (Origin) on cookie writes, re-authentication window, idle and absolute expiry, scoped tokens
// (role caps, 10-token limit, rotation, revocation, tokens cannot manage tokens), tenant denial
// over HTTP and WebSocket, edit-only tokens refused on publish over the socket, sockets closed
// on token revocation, logout, membership removal and role change (immediate) and by the
// recheck alone (D1-only revocation, no immediate call).
// Phase 2 (server restarted with the PRODUCTION defaults on the same state): after a verified
// hibernation, a token revoked only in D1 stops delivery before the next broadcast, and an idle
// socket of a revoked token or a removed member is told and closed within the documented 60 s
// bound (measured and printed). Finally the captured server log is searched for every
// credential value the run handled (none may appear).
//
// Usage: node scripts/test-studio-auth.mjs [--port 8794]
// Debug: STUDIO_AUTH_PHASE1_ONLY=1 skips phase 2; the redacted server log is kept in
// .wrangler/studio-auth-server.log.
// Server-dependent: CI only (not part of `npm run verify`). Exit 0 = all passed.

import { spawn, spawnSync } from 'node:child_process';
import http from 'node:http';
import { randomBytes } from 'node:crypto';
import { readFileSync, rmSync, writeFileSync, mkdtempSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)));
const CFG = 'wrangler.studio.jsonc';
const STATE = '.wrangler/studio-auth-state';
const argPort = process.argv.indexOf('--port');
const PORT = Number(argPort > 0 ? process.argv[argPort + 1] : process.env.STUDIO_AUTH_PORT || 8794);
const HTTP = `http://127.0.0.1:${PORT}`;
const WS_BASE = `ws://127.0.0.1:${PORT}`;
const FIXTURE = readFileSync(join(ROOT, 'crates/sws-worker-studio/tests/fixtures/session-site.json'), 'utf8');
const OWNER_GH = 5001;
const CLIENT_ID = 'Iv1.testclient';
const CLIENT_SECRET = `fake-secret-${randomBytes(8).toString('hex')}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** Every credential value the run handles; none may appear in the server log. */
const SECRETS = new Set([CLIENT_SECRET]);
const keep = (v) => (v && SECRETS.add(v), v);

// --- Fake GitHub ---------------------------------------------------------------------------

const GH_USERS = {
  'c-owner': { id: OWNER_GH, login: 'Owner-One' },
  'c-stranger': { id: 5002, login: 'stranger' },
  'c-editor': { id: 5003, login: 'Editor-Two' },
  'c-pub': { id: 5004, login: 'pub-three' },
  'c-intruder': { id: 5005, login: 'intruder' },
};
const ghTokens = new Map();
let gh;
let GH_BASE;
function startFakeGithub() {
  gh = http.createServer((req, res) => {
    let body = '';
    req.on('data', (d) => (body += d));
    req.on('end', () => {
      const send = (status, obj) => {
        res.writeHead(status, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(obj));
      };
      if (req.method === 'POST' && req.url === '/login/oauth/access_token') {
        const f = new URLSearchParams(body);
        if (req.headers.accept !== 'application/json') return send(400, { error: 'accept' });
        if (f.get('client_id') !== CLIENT_ID || f.get('client_secret') !== CLIENT_SECRET) return send(200, { error: 'incorrect_client_credentials' });
        if (f.get('redirect_uri') !== `${HTTP}/auth/github/callback`) return send(200, { error: 'redirect_uri_mismatch' });
        const user = GH_USERS[f.get('code')];
        if (!user) return send(200, { error: 'bad_verification_code' });
        const tok = keep(`gho_${randomBytes(12).toString('hex')}`);
        ghTokens.set(tok, user);
        return send(200, { access_token: tok, token_type: 'bearer', scope: '' });
      }
      if (req.method === 'GET' && req.url === '/user') {
        const m = /^Bearer (\S+)$/.exec(req.headers.authorization || '');
        const user = m && ghTokens.get(m[1]);
        if (!user || !req.headers['user-agent']) return send(401, { message: 'Bad credentials' });
        return send(200, { ...user, name: 'x' });
      }
      send(404, { message: 'Not Found' });
    });
  });
  return new Promise((r) =>
    gh.listen(0, '127.0.0.1', () => {
      GH_BASE = `http://127.0.0.1:${gh.address().port}`;
      r();
    }),
  );
}

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

const FAST = ['--var', 'SESSION_AUTHZ_RECHECK_MS:1000', '--var', 'SESSION_IDLE_SWEEP_MS:1000'];
let server = null;
let serverLog = '';
let fullLog = '';
async function startServer(extra) {
  assert.equal(await portOpen(), false, `port ${PORT} is already in use`);
  const vars = {
    STUDIO_PUBLIC_ORIGIN: HTTP,
    GITHUB_OAUTH_CLIENT_ID: CLIENT_ID,
    GITHUB_OAUTH_CLIENT_SECRET: CLIENT_SECRET,
    GITHUB_OAUTH_BASE_URL: GH_BASE,
    GITHUB_API_BASE_URL: GH_BASE,
    STUDIO_OWNER_GITHUB_ID: String(OWNER_GH),
    STUDIO_BOOTSTRAP_WORKSPACE: 'studio',
    TURNSTILE_SITE_KEY: '1x00000000000000000000AA',
    TURNSTILE_SECRET_KEY: '1x0000000000000000000000000000000AA',
    DEV_MODE: 'true',
  };
  const varArgs = Object.entries(vars).flatMap(([k, v]) => ['--var', `${k}:${v}`]);
  serverLog = '';
  server = spawn(
    'npx',
    ['wrangler', 'dev', '-c', CFG, '--persist-to', STATE, '--port', String(PORT), '--ip', '127.0.0.1', ...varArgs, ...extra],
    { cwd: ROOT, detached: true, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  const onData = (d) => {
    serverLog += d;
    fullLog += d;
  };
  server.stdout.on('data', onData);
  server.stderr.on('data', onData);
  const end = Date.now() + 300_000;
  while (Date.now() < end) {
    try {
      const r = await fetch(`${HTTP}/api/health`, { signal: AbortSignal.timeout(2000) });
      if (r.ok) return;
    } catch {
      // not up yet
    }
    if (server.exitCode !== null) break;
    await sleep(1000);
  }
  throw new Error(`wrangler dev did not become ready\n${serverLog.slice(-3000)}`);
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
  const dir = mkdtempSync(join(tmpdir(), 'sws-auth-'));
  const file = join(dir, 'q.sql');
  writeFileSync(file, sql);
  try {
    wrangler(['d1', 'execute', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE, '--file', file]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const FOURTEEN_DAYS = 14 * 86_400_000;
const iso = (msAgo) => new Date(Date.now() - msAgo).toISOString().replace(/\.\d{3}Z$/, 'Z');

// --- HTTP helpers -------------------------------------------------------------------------

/** fetch with redirects off; `who` is a Session (cookie + browser Origin) or a bearer token string. */
async function call(method, path, { who, body, origin, headers = {}, form } = {}) {
  const h = { ...headers };
  if (typeof who === 'string') h.Authorization = `Bearer ${who}`;
  else if (who) {
    h.Cookie = `__Host-sws_session=${who.cookie}`;
    if (origin !== null) h.Origin = origin ?? HTTP;
  }
  let payload;
  if (form) {
    payload = new URLSearchParams(form).toString();
    h['Content-Type'] = 'application/x-www-form-urlencoded';
  } else if (body !== undefined) {
    payload = JSON.stringify(body);
    h['Content-Type'] = 'application/json';
  } else if (['POST', 'PATCH', 'PUT'].includes(method)) {
    payload = '';
  }
  const r = await request(method, path, h, payload);
  const text = r.text;
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    // HTML or empty
  }
  return { status: r.status, headers: r.headers, text, json };
}

/** One HTTP request on a FRESH connection (`agent: false`): the test pauses for seconds while it
 *  edits D1 through the wrangler CLI, and a pooled keep-alive socket closed by the server meanwhile
 *  would fail the next request for a reason that has nothing to do with the Worker. */
function request(method, path, headers, body) {
  return new Promise((res, rej) => {
    const h = { ...headers };
    if (body !== undefined) h['Content-Length'] = Buffer.byteLength(body);
    const req = http.request({ host: '127.0.0.1', port: PORT, path, method, headers: h, agent: false }, (r) => {
      let text = '';
      r.setEncoding('utf8');
      r.on('data', (d) => (text += d));
      r.on('end', () =>
        res({
          status: r.statusCode,
          text,
          headers: {
            get: (n) => {
              const v = r.headers[n.toLowerCase()];
              return v === undefined ? null : Array.isArray(v) ? v.join(', ') : v;
            },
            getSetCookie: () => r.headers['set-cookie'] ?? [],
          },
        }),
      );
    });
    req.on('error', rej);
    req.setTimeout(20_000, () => req.destroy(new Error('timeout')));
    if (body !== undefined) req.write(body);
    req.end();
  });
}

function cookieFrom(res, name) {
  const all = res.headers.getSetCookie();
  const c = all.find((x) => x.startsWith(`${name}=`));
  return c ? { raw: c, value: c.slice(name.length + 1).split(';')[0] } : null;
}

/** Full browser sign-in through the fake GitHub. Returns { cookie, res } or the failing response. */
async function signIn(code, { invitation } = {}) {
  let start;
  if (invitation) {
    start = await call('POST', '/auth/invitations/accept', {
      form: { code: invitation, 'cf-turnstile-response': 'XXXX.DUMMY.TOKEN.XXXX' },
      headers: { Origin: HTTP },
    });
  } else {
    start = await call('GET', '/auth/github/start');
  }
  if (start.status !== 303) return { res: start };
  const loc = new URL(start.headers.get('location'));
  assert.equal(`${loc.origin}${loc.pathname}`, `${GH_BASE}/login/oauth/authorize`);
  assert.equal(loc.searchParams.get('client_id'), CLIENT_ID);
  assert.equal(loc.searchParams.get('redirect_uri'), `${HTTP}/auth/github/callback`);
  const state = keep(loc.searchParams.get('state'));
  const oc = cookieFrom(start, '__Host-sws_oauth');
  assert.ok(oc, 'the OAuth state is bound to the browser by a cookie');
  assert.equal(oc.value, state);
  const cb = await call('GET', `/auth/github/callback?code=${code}&state=${state}`, { headers: { Cookie: `__Host-sws_oauth=${state}` } });
  const sc = cookieFrom(cb, '__Host-sws_session');
  if (cb.status !== 303 || !sc) return { res: cb, state };
  keep(sc.value);
  return { cookie: sc.value, raw: sc.raw, res: cb, state };
}

// --- WebSocket client ---------------------------------------------------------------------

class Client {
  constructor(ws) {
    this.ws = ws;
    this.queue = [];
    this.waiters = [];
    this.closeInfo = null;
    this.closedP = new Promise((res) => (this.onClosed = res));
    ws.onmessage = (e) => {
      this.queue.push(JSON.parse(String(e.data)));
      this.flush();
    };
    ws.onclose = (e) => {
      this.closeInfo = { code: e.code, reason: e.reason };
      this.onClosed(this.closeInfo);
    };
  }

  static async connect(ws, site, who) {
    const headers = typeof who === 'string' ? { Authorization: `Bearer ${who}` } : { Cookie: `__Host-sws_session=${who.cookie}`, Origin: HTTP };
    const sock = new WebSocket(`${WS_BASE}/api/sites/${ws}/${site}/session`, { headers });
    const c = new Client(sock);
    await new Promise((res, rej) => {
      sock.onopen = res;
      sock.addEventListener('error', () => rej(new Error('websocket error before open')), { once: true });
    });
    c.welcome = await c.next((m) => m.type === 'welcome');
    return c;
  }

  flush() {
    for (const w of [...this.waiters]) {
      const i = this.queue.findIndex(w.pred);
      if (i >= 0) {
        const [m] = this.queue.splice(i, 1);
        this.waiters.splice(this.waiters.indexOf(w), 1);
        w.res(m);
      }
    }
  }

  next(pred, ms = 8000) {
    return new Promise((res, rej) => {
      const w = { pred, res };
      this.waiters.push(w);
      this.flush();
      setTimeout(() => {
        const i = this.waiters.indexOf(w);
        if (i >= 0) {
          this.waiters.splice(i, 1);
          rej(new Error(`timed out waiting for a message; queued: ${JSON.stringify(this.queue).slice(0, 400)}`));
        }
      }, ms);
    });
  }

  send(obj) {
    this.ws.send(JSON.stringify({ v: 1, ...obj }));
  }

  request(obj, pred, ms) {
    this.send(obj);
    return this.next(pred, ms);
  }

  close() {
    try {
      this.ws.close();
    } catch {
      // ignore
    }
  }
}

let opN = 0;
const batch = (base, value) => ({
  type: 'batch',
  op_id: `op-auth-${String(++opN).padStart(5, '0')}`,
  base_seq: base,
  batch: { version: 1, commands: [{ op: 'set_text', uid: 'home.title-text', value }] },
});

/** Raw upgrade (fetch cannot send Upgrade); resolves the status. */
function upgrade(path, headers) {
  return new Promise((res, rej) => {
    const req = http.request({
      host: '127.0.0.1',
      port: PORT,
      path,
      headers: { Upgrade: 'websocket', Connection: 'Upgrade', 'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==', ...headers },
    });
    req.on('response', (r) => {
      r.resume();
      res(r.statusCode);
    });
    req.on('upgrade', (r, socket) => (socket.destroy(), res(r.statusCode)));
    req.on('error', rej);
    req.setTimeout(10_000, () => req.destroy(new Error('timeout')));
    req.end();
  });
}

// --- Tests --------------------------------------------------------------------------------

const results = [];
const ONLY = process.env.STUDIO_AUTH_ONLY ? new RegExp(process.env.STUDIO_AUTH_ONLY, 'i') : null;
async function test(name, fn) {
  if (ONLY && !ONLY.test(name)) return;
  const t0 = Date.now();
  try {
    // A hung step fails the test instead of stalling the run.
    let timer;
    await Promise.race([fn(), new Promise((_, rej) => (timer = setTimeout(() => rej(new Error('test timed out after 180 s')), 180_000)))]);
    clearTimeout(timer);
    results.push({ name, ok: true });
    console.log(`  ok   ${name} (${Date.now() - t0} ms)`);
  } catch (e) {
    results.push({ name, ok: false, error: e });
    console.log(`  FAIL ${name}\n       ${String(e.stack ?? e).split('\n').slice(0, 6).join('\n       ')}`);
  }
}

const S = {}; // sessions and tokens shared across tests

function seedD1() {
  const sites = ['s-main', 's-tok', 's-rev', 's-out', 's-rm', 's-hib', 's-hib2'].map((s) => `('studio','${s}','${s}','${s}')`).join(',');
  d1File(`INSERT INTO workspaces (id, name) VALUES ('studio','studio'), ('ws-b','Workspace B');
INSERT INTO users (id, github_id, github_login) VALUES (90, 9090, 'b-owner');
INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-b', 90, 'owner');
INSERT INTO sites (workspace_id, id, slug, name) VALUES ${sites}, ('ws-b','sb','sb','SB');
`);
}

const userId = (gid) => d1(`SELECT id FROM users WHERE github_id = ${gid}`)[0]?.id;

async function phase1() {
  console.log('\nphase 1: auth flows (membership recheck 1 s, idle sweep 1 s)');

  await test('no credential: protected routes answer 401; the dev header means nothing in the release build', async () => {
    assert.equal((await call('GET', '/auth/session')).status, 401);
    assert.equal((await call('GET', '/api/sites/studio/s-main/assets')).status, 401);
    assert.equal(await upgrade('/api/sites/studio/s-main/session', { 'X-Sws-Dev-User': '1' }), 401);
    assert.equal((await call('GET', '/api/sites/studio/s-main/assets', { headers: { 'X-Sws-Dev-User': '1' } })).status, 401);
    // A malformed or unknown bearer never falls back to anything.
    assert.equal((await call('GET', '/auth/session', { headers: { Authorization: 'Bearer nope' } })).status, 401);
    assert.equal((await call('GET', '/auth/session', { who: `swst_${'0'.repeat(24)}_${'0'.repeat(64)}` })).status, 401);
  });

  await test('non-owner first sign-in is refused and leaves no user row', async () => {
    const r = await signIn('c-stranger');
    assert.equal(r.res.status, 403);
    assert.match(r.res.text, /not been invited/);
    assert.equal(r.res.headers.get('content-type'), 'text/html; charset=utf-8');
    assert.equal(r.res.headers.get('cache-control'), 'no-store');
    assert.ok(r.res.headers.get('content-security-policy')?.includes("frame-ancestors 'none'"));
    assert.equal(userId(5002), undefined);
  });

  await test('owner bootstrap: the configured GitHub id becomes the one owner; hardened session cookie', async () => {
    const r = await signIn('c-owner');
    assert.equal(r.res.status, 303, r.res.text);
    assert.equal(r.res.headers.get('location'), '/auth/session');
    assert.match(r.raw, /^__Host-sws_session=[0-9a-f]{24}\.[0-9a-f]{64}; Path=\/; Max-Age=1209600; HttpOnly; Secure; SameSite=Lax$/);
    S.owner = { cookie: r.cookie };
    const me = await call('GET', '/auth/session', { who: S.owner });
    assert.equal(me.status, 200);
    assert.equal(me.json.github_login, 'Owner-One');
    assert.equal(me.json.credential.kind, 'session');
    assert.equal(me.json.credential.fresh, true);
    assert.deepEqual(me.json.memberships, [{ workspace_id: 'studio', role: 'owner' }]);
    assert.equal(JSON.stringify(me.json).includes(r.cookie), false, 'the cookie value is never echoed');
    // Signing in again does not create a second owner and mints a NEW session id.
    const again = await signIn('c-owner');
    assert.notEqual(again.cookie, r.cookie);
    assert.equal(d1(`SELECT COUNT(*) AS n FROM memberships WHERE workspace_id='studio' AND role='owner'`)[0].n, 1);
    // Only digests are stored.
    const rows = d1('SELECT id, secret_hash FROM auth_sessions');
    assert.ok(rows.every((x) => /^[0-9a-f]{64}$/.test(x.secret_hash) && !r.cookie.includes(x.secret_hash)));
  });

  await test('OAuth state: single use, bound to the starting browser, and validated', async () => {
    const start = await call('GET', '/auth/github/start');
    const state = keep(new URL(start.headers.get('location')).searchParams.get('state'));
    // Without the browser-binding cookie (login CSRF): refused.
    assert.equal((await call('GET', `/auth/github/callback?code=c-owner&state=${state}`)).status, 400);
    // With another browser's cookie: refused.
    assert.equal((await call('GET', `/auth/github/callback?code=c-owner&state=${state}`, { headers: { Cookie: `__Host-sws_oauth=${'f'.repeat(64)}` } })).status, 400);
    const ok = await call('GET', `/auth/github/callback?code=c-owner&state=${state}`, { headers: { Cookie: `__Host-sws_oauth=${state}` } });
    assert.equal(ok.status, 303);
    keep(cookieFrom(ok, '__Host-sws_session')?.value);
    // Replay of a consumed state: refused.
    assert.equal((await call('GET', `/auth/github/callback?code=c-owner&state=${state}`, { headers: { Cookie: `__Host-sws_oauth=${state}` } })).status, 400);
    // GitHub-side cancellation and junk.
    assert.equal((await call('GET', '/auth/github/callback?error=access_denied')).status, 400);
    assert.equal((await call('GET', '/auth/github/callback?code=x&state=<script>', { headers: { Cookie: '__Host-sws_oauth=<script>' } })).status, 400);
    // logout is never a GET (no CSRF by link).
    assert.equal((await call('GET', '/auth/logout')).status, 405);
  });

  await test('invitations: owner-only with a fresh session and Origin; form needs Turnstile; bound to the invited login; single use', async () => {
    const noOrigin = await call('POST', '/api/workspaces/studio/invitations', { who: S.owner, origin: null, body: { github_login: 'Editor-Two', role: 'editor' } });
    assert.equal(noOrigin.status, 403);
    assert.equal(noOrigin.json.error, 'forbidden_origin');
    const evil = await call('POST', '/api/workspaces/studio/invitations', { who: S.owner, origin: 'https://evil.example', body: { github_login: 'Editor-Two', role: 'editor' } });
    assert.equal(evil.status, 403);
    const owner = await call('POST', '/api/workspaces/studio/invitations', { who: S.owner, body: { github_login: 'x', role: 'owner' } });
    assert.equal(owner.json.error, 'invalid_role');
    const inv = await call('POST', '/api/workspaces/studio/invitations', { who: S.owner, body: { github_login: 'Editor-Two', role: 'editor' } });
    assert.equal(inv.status, 201, inv.text);
    const code = keep(inv.json.code);
    assert.match(code, /^swsi_[0-9a-f]{24}_[0-9a-f]{64}$/);
    assert.equal(inv.json.accept_url, `${HTTP}/invite`);
    const listed = await call('GET', '/api/workspaces/studio/invitations', { who: S.owner });
    assert.equal(listed.text.includes(code), false);
    assert.equal(listed.text.includes('secret_hash'), false);
    // The public form.
    const page = await call('GET', '/invite');
    assert.equal(page.status, 200);
    assert.match(page.text, /<form method="post" action="\/auth\/invitations\/accept">/);
    assert.match(page.text, /class="cf-turnstile" data-sitekey="1x00000000000000000000AA"/);
    // Missing Turnstile token, wrong Origin, bad code.
    const noTs = await call('POST', '/auth/invitations/accept', { form: { code }, headers: { Origin: HTTP } });
    assert.equal(noTs.status, 400);
    assert.match(noTs.text, /human check/);
    const cross = await call('POST', '/auth/invitations/accept', { form: { code, 'cf-turnstile-response': 'XXXX.DUMMY.TOKEN.XXXX' }, headers: { Origin: 'https://evil.example' } });
    assert.equal(cross.status, 403);
    const bad = await signIn('c-editor', { invitation: code.slice(0, -1) + (code.endsWith('0') ? '1' : '0') });
    assert.equal(bad.res.status, 400);
    // Another GitHub account cannot use it.
    const intr = await signIn('c-intruder', { invitation: code });
    assert.equal(intr.res.status, 403, intr.res.text);
    assert.equal(d1(`SELECT COUNT(*) AS n FROM memberships m JOIN users u ON u.id = m.user_id WHERE u.github_id = 5005`)[0].n, 0);
    // The invited login (case-insensitive) is accepted as an editor.
    const ok = await signIn('c-editor', { invitation: code });
    assert.equal(ok.res.status, 303, ok.res.text);
    S.editor = { cookie: ok.cookie };
    const me = await call('GET', '/auth/session', { who: S.editor });
    assert.deepEqual(me.json.memberships, [{ workspace_id: 'studio', role: 'editor' }]);
    // Single use.
    const again = await signIn('c-editor', { invitation: code });
    assert.equal(again.res.status, 400);
    // A publisher, for later tests.
    const p = await call('POST', '/api/workspaces/studio/invitations', { who: S.owner, body: { github_login: 'pub-three', role: 'publisher' } });
    const pr = await signIn('c-pub', { invitation: keep(p.json.code) });
    assert.equal(pr.res.status, 303);
    S.pub = { cookie: pr.cookie };
  });

  await test('scoped tokens: role caps, shown once, digests only, tokens cannot manage tokens or members', async () => {
    const ed = await call('POST', '/api/workspaces/studio/tokens', { who: S.editor, body: { scope: 'publish' } });
    assert.equal(ed.status, 403);
    assert.equal(ed.json.error, 'forbidden_role');
    const edit = await call('POST', '/api/workspaces/studio/tokens', { who: S.editor, body: { scope: 'edit', label: 'laptop' } });
    assert.equal(edit.status, 201, edit.text);
    S.editTok = keep(edit.json.token);
    S.editTokId = edit.json.id;
    assert.match(S.editTok, /^swst_[0-9a-f]{24}_[0-9a-f]{64}$/);
    const days = (Date.parse(edit.json.expires_at) - Date.parse(edit.json.created_at)) / 86_400_000;
    assert.equal(days, 30);
    const pub = await call('POST', '/api/workspaces/studio/tokens', { who: S.owner, body: { scope: 'publish' } });
    assert.equal((Date.parse(pub.json.expires_at) - Date.parse(pub.json.created_at)) / 86_400_000, 7);
    S.pubTok = keep(pub.json.token);
    const act = await call('POST', '/api/workspaces/studio/tokens', { who: S.owner, body: { scope: 'activate' } });
    assert.equal((Date.parse(act.json.expires_at) - Date.parse(act.json.created_at)) / 3_600_000, 24);
    keep(act.json.token);
    // Listing never returns the secret or its digest.
    const list = await call('GET', '/api/tokens', { who: S.editor });
    assert.equal(list.json.tokens.length, 1);
    assert.equal(list.text.includes(S.editTok), false);
    assert.equal(list.text.includes('token_hash'), false);
    assert.equal(d1(`SELECT token_hash FROM api_tokens WHERE id='${S.editTokId}'`)[0].token_hash.length, 64);
    // Bearer works and is bound to its workspace and scope.
    const me = await call('GET', '/auth/session', { who: S.editTok });
    assert.deepEqual(me.json.credential, { kind: 'token', id: S.editTokId, workspace_id: 'studio', scope: 'edit' });
    // Tokens cannot mint tokens, list tokens or manage members.
    assert.equal((await call('POST', '/api/workspaces/studio/tokens', { who: S.pubTok, body: { scope: 'edit' } })).json.error, 'session_required');
    assert.equal((await call('GET', '/api/tokens', { who: S.pubTok })).json.error, 'session_required');
    assert.equal((await call('POST', '/api/workspaces/studio/invitations', { who: S.pubTok, body: { github_login: 'a', role: 'editor' } })).json.error, 'session_required');
    // A token in a garbage Authorization header plus a valid cookie does not fall back to the cookie.
    assert.equal((await call('GET', '/auth/session', { who: S.owner, headers: { Authorization: 'Bearer junk' } })).status, 401);
  });

  await test('tenant denial: workspace A credentials cannot touch workspace B (HTTP and WebSocket)', async () => {
    for (const who of [S.owner, S.editTok, S.pubTok]) {
      assert.equal((await call('GET', '/api/sites/ws-b/sb/assets', { who })).status, 403);
      assert.equal((await call('GET', '/api/workspaces/ws-b/members', { who })).status, 403);
      const h = typeof who === 'string' ? { Authorization: `Bearer ${who}` } : { Cookie: `__Host-sws_session=${who.cookie}`, Origin: HTTP };
      assert.equal(await upgrade('/api/sites/ws-b/sb/session', h), 403);
    }
    assert.equal((await call('POST', '/api/workspaces/ws-b/tokens', { who: S.owner, body: { scope: 'edit' } })).status, 403);
    // The same credentials work in their own workspace.
    assert.equal((await call('GET', '/api/sites/studio/s-main/assets', { who: S.editTok })).status, 200);
    assert.equal((await call('GET', '/api/workspaces/studio/members', { who: S.editTok })).status, 200);
    // A cookie-borne asset write needs the Origin; a token write does not.
    const sha = 'a'.repeat(64);
    const del = await call('DELETE', `/api/sites/studio/s-main/assets/${sha}`, { who: S.owner, origin: null });
    assert.equal(del.status, 403);
    assert.equal(del.json.error, 'forbidden_origin');
  });

  await test('edit-only token over the session socket: may edit, may not seed or publish', async () => {
    const owner = await Client.connect('studio', 's-tok', S.owner);
    assert.equal(owner.welcome.role, 'owner', 'a fresh owner session acts as owner');
    const tok = await Client.connect('studio', 's-tok', S.editTok);
    assert.equal(tok.welcome.role, 'editor', 'an edit token acts as editor');
    const seedDenied = await tok.request({ type: 'seed', document: FIXTURE }, (m) => m.type === 'error');
    assert.equal(seedDenied.code, 'forbidden');
    const seeded = await owner.request({ type: 'seed', document: FIXTURE }, (m) => m.type === 'state' || m.type === 'error');
    assert.equal(seeded.type, 'state', JSON.stringify(seeded));
    const ack = await tok.request(batch(0, 'edited by token'), (m) => m.type === 'ack' || m.type === 'error');
    assert.equal(ack.type, 'ack', JSON.stringify(ack));
    const pubDenied = await tok.request({ type: 'checkpoint', reason: 'publish' }, (m) => m.type === 'error');
    assert.equal(pubDenied.code, 'forbidden');
    // A publish-scoped token of the owner may publish.
    const pubSock = await Client.connect('studio', 's-tok', S.pubTok);
    assert.equal(pubSock.welcome.role, 'owner');
    const ck = await pubSock.request({ type: 'checkpoint', reason: 'publish' }, (m) => m.type === 'checkpoint' || m.type === 'error');
    assert.equal(ck.type, 'checkpoint', JSON.stringify(ck));
    owner.close();
    tok.close();
    pubSock.close();
  });

  await test('re-authentication window and session expiry (idle 24 h, absolute 14 d)', async () => {
    const r = await signIn('c-owner');
    const sess = { cookie: r.cookie };
    const id = r.cookie.split('.')[0];
    // Signed in 16 minutes ago (the absolute end moves with it: the schema caps it at +14 d).
    d1(`UPDATE auth_sessions SET created_at = '${iso(16 * 60_000)}', last_seen_at = '${iso(16 * 60_000)}', expires_at = '${iso(16 * 60_000 - FOURTEEN_DAYS)}' WHERE id = '${id}'`);
    const stale = await call('POST', '/api/workspaces/studio/tokens', { who: sess, body: { scope: 'edit' } });
    assert.equal(stale.status, 401);
    assert.equal(stale.json.error, 'reauth_required');
    assert.equal((await call('GET', '/auth/session', { who: sess })).json.credential.fresh, false);
    // A stale session cannot publish over the socket either (it acts as editor).
    const sock = await Client.connect('studio', 's-tok', sess);
    assert.equal(sock.welcome.role, 'editor');
    sock.close();
    const day = 24 * 3_600_000;
    d1(`UPDATE auth_sessions SET created_at = '${iso(day + 60_000)}', last_seen_at = '${iso(day)}', expires_at = '${iso(day + 60_000 - FOURTEEN_DAYS)}' WHERE id = '${id}'`);
    assert.equal((await call('GET', '/auth/session', { who: sess })).status, 401, 'idle 24 h');
    const r2 = await signIn('c-owner');
    const id2 = r2.cookie.split('.')[0];
    d1(`UPDATE auth_sessions SET created_at = '${iso(14 * 86_400_000)}', last_seen_at = '${iso(1000)}', expires_at = '${iso(0)}' WHERE id = '${id2}'`);
    assert.equal((await call('GET', '/auth/session', { who: { cookie: r2.cookie } })).status, 401, 'absolute 14 d');
  });

  await test('the 11th active token is refused; rotation keeps the chain and kills the old token', async () => {
    const made = [];
    for (let n = 0; n < 9; n++) {
      const r = await call('POST', '/api/workspaces/studio/tokens', { who: S.editor, body: { scope: 'edit', label: `t${n}` } });
      assert.equal(r.status, 201, `token ${n + 2}: ${r.text}`);
      made.push(keep(r.json.token) && r.json);
    }
    const eleventh = await call('POST', '/api/workspaces/studio/tokens', { who: S.editor, body: { scope: 'edit' } });
    assert.equal(eleventh.status, 409);
    assert.equal(eleventh.json.error, 'too_many_tokens');
    // Rotation at the limit works (it replaces one).
    const old = made[0];
    const rot = await call('POST', `/api/tokens/${old.id}/rotate`, { who: S.editor });
    assert.equal(rot.status, 201, rot.text);
    keep(rot.json.token);
    assert.equal(rot.json.chain_started_at, old.chain_started_at);
    assert.equal((await call('GET', '/auth/session', { who: old.token })).status, 401, 'the rotated token is dead');
    assert.equal((await call('GET', '/auth/session', { who: rot.json.token })).status, 200);
    assert.equal((await call('POST', `/api/tokens/${old.id}/rotate`, { who: S.editor })).json.error, 'token_revoked');
    // Rotation needs a fresh browser session, never a token.
    assert.equal((await call('POST', `/api/tokens/${rot.json.id}/rotate`, { who: rot.json.token })).json.error, 'session_required');
    // A chain past 90 days cannot be rotated.
    d1(`UPDATE api_tokens SET chain_started_at = '${iso(90 * 86_400_000)}', created_at = '${iso(90 * 86_400_000)}', expires_at = '${iso(60 * 86_400_000)}' WHERE id = '${made[1].id}'`);
    assert.equal((await call('POST', `/api/tokens/${made[1].id}/rotate`, { who: S.editor })).json.error, 'token_chain_expired');
    // Someone else's token cannot be rotated or revoked.
    assert.equal((await call('POST', `/api/tokens/${made[2].id}/rotate`, { who: S.owner })).status, 404);
    assert.equal((await call('DELETE', `/api/tokens/${made[2].id}`, { who: S.owner })).status, 404);
    // Clean up: the token can revoke itself (CLI logout).
    for (const t of made.slice(2)) assert.equal((await call('DELETE', '/api/tokens/current', { who: t.token })).status, 200);
  });

  await test('token revocation closes its sockets at once, and only those; a D1-only revocation is caught by the recheck', async () => {
    const own = await call('POST', '/api/workspaces/studio/tokens', { who: S.pub, body: { scope: 'publish' } });
    const t1 = keep(own.json.token);
    const other = await call('POST', '/api/workspaces/studio/tokens', { who: S.pub, body: { scope: 'edit' } });
    const t2 = keep(other.json.token);
    const owner = await Client.connect('studio', 's-rev', S.owner);
    await owner.request({ type: 'seed', document: FIXTURE }, (m) => m.type === 'state');
    const a = await Client.connect('studio', 's-rev', t1);
    const b = await Client.connect('studio', 's-rev', t2);
    const browser = await Client.connect('studio', 's-rev', S.pub);
    const t0 = Date.now();
    const del = await call('DELETE', `/api/tokens/${own.json.id}`, { who: S.pub });
    assert.equal(del.status, 200, del.text);
    const told = await a.next((m) => m.type === 'revoked', 5000);
    const ms = Date.now() - t0;
    assert.equal(told.reason, 'credential_revoked');
    console.log(`         revoked token's socket told after ${ms} ms (immediate path)`);
    // The local runtime may delay the TCP teardown; when the close is reported it must be 4403.
    const closed = await Promise.race([a.closedP, sleep(3000).then(() => null)]);
    if (closed) assert.equal(closed.code, 4403);
    // The other credentials of the same user stay connected.
    assert.equal((await b.request({ type: 'ping' }, (m) => m.type === 'pong')).type, 'pong');
    assert.equal((await browser.request({ type: 'ping' }, (m) => m.type === 'pong')).type, 'pong');
    // D1-only revocation (no immediate call): the recheck (1 s here) closes the socket.
    const t1b = Date.now();
    d1(`UPDATE api_tokens SET revoked_at = '${iso(0)}' WHERE id = '${other.json.id}'`);
    const told2 = await b.next((m) => m.type === 'revoked', 20_000);
    console.log(`         D1-only revoked token's idle socket told ${Date.now() - t1b} ms after the D1 write started (incl. the wrangler CLI; sweep 1000 ms)`);
    assert.equal(told2.reason, 'credential_revoked');
    await owner.request(batch(0, 'after revocations'), (m) => m.type === 'ack');
    await sleep(300);
    assert.equal(b.queue.some((m) => m.type === 'entry'), false, 'no entry after revocation');
    owner.close();
    browser.close();
  });

  await test('logout revokes the session, clears the cookie and closes its sockets', async () => {
    const r = await signIn('c-pub');
    const sess = { cookie: r.cookie };
    const sock = await Client.connect('studio', 's-out', sess);
    const csrf = await call('POST', '/auth/logout', { who: sess, origin: 'https://evil.example' });
    assert.equal(csrf.status, 403);
    const out = await call('POST', '/auth/logout', { who: sess });
    assert.equal(out.status, 200);
    assert.match(cookieFrom(out, '__Host-sws_session').raw, /Max-Age=0/);
    assert.equal((await sock.next((m) => m.type === 'revoked', 5000)).reason, 'credential_revoked');
    assert.equal((await call('GET', '/auth/session', { who: sess })).status, 401);
    // The publisher's other session is unaffected.
    assert.equal((await call('GET', '/auth/session', { who: S.pub })).status, 200);
  });

  await test('member changes: owner-only, fresh session; demotion revokes publish tokens; removal deletes tokens and closes sockets', async () => {
    const edId = userId(5003);
    const pubId = userId(5004);
    const ownerId = userId(OWNER_GH);
    assert.equal((await call('DELETE', `/api/workspaces/studio/members/${ownerId}`, { who: S.owner })).json.error, 'owner_is_fixed');
    assert.equal((await call('PATCH', `/api/workspaces/studio/members/${edId}`, { who: S.pub, body: { role: 'publisher' } })).json.error, 'forbidden_role');
    assert.equal((await call('PATCH', `/api/workspaces/studio/members/${edId}`, { who: S.owner, body: { role: 'owner' } })).json.error, 'invalid_role');
    // Demotion: publish tokens revoked, sockets reopened with the new role.
    const pt = await call('POST', '/api/workspaces/studio/tokens', { who: S.pub, body: { scope: 'publish' } });
    keep(pt.json.token);
    const sock = await Client.connect('studio', 's-rm', S.pub);
    assert.equal(sock.welcome.role, 'publisher');
    const dem = await call('PATCH', `/api/workspaces/studio/members/${pubId}`, { who: S.owner, body: { role: 'editor' } });
    assert.equal(dem.status, 200, dem.text);
    assert.equal((await sock.next((m) => m.type === 'revoked', 5000)).type, 'revoked');
    assert.equal((await call('GET', '/auth/session', { who: pt.json.token })).status, 401, 'publish token revoked on demotion');
    const re = await Client.connect('studio', 's-rm', S.pub);
    assert.equal(re.welcome.role, 'editor');
    re.close();
    // Removal.
    const edSock = await Client.connect('studio', 's-rm', S.editTok);
    const t0 = Date.now();
    const rm = await call('DELETE', `/api/workspaces/studio/members/${edId}`, { who: S.owner });
    assert.equal(rm.status, 200, rm.text);
    assert.equal((await edSock.next((m) => m.type === 'revoked', 5000)).type, 'revoked');
    console.log(`         removed member's socket told after ${Date.now() - t0} ms (immediate path)`);
    assert.equal((await call('GET', '/auth/session', { who: S.editTok })).status, 401, 'tokens cascade with the membership');
    assert.equal(await upgrade('/api/sites/studio/s-rm/session', { Cookie: `__Host-sws_session=${S.editor.cookie}`, Origin: HTTP }), 403);
    // Members management needs a fresh session.
    const r = await signIn('c-owner');
    d1(`UPDATE auth_sessions SET created_at = '${iso(20 * 60_000)}', last_seen_at = '${iso(1000)}', expires_at = '${iso(20 * 60_000 - FOURTEEN_DAYS)}' WHERE id = '${r.cookie.split('.')[0]}'`);
    assert.equal((await call('DELETE', `/api/workspaces/studio/members/${pubId}`, { who: { cookie: r.cookie } })).json.error, 'reauth_required');
  });
}

async function phase2() {
  console.log('\nphase 2: production defaults (recheck 15 s, idle sweep 60 s) after hibernation');
  await test('after hibernation: a D1-only token revocation stops delivery before the next broadcast; idle sockets are closed within 60 s', async () => {
    const owner = await signIn('c-owner');
    S.owner2 = { cookie: owner.cookie };
    const t = await call('POST', '/api/workspaces/studio/tokens', { who: S.owner2, body: { scope: 'edit' } });
    const tok = keep(t.json.token);
    const t2 = await call('POST', '/api/workspaces/studio/tokens', { who: S.owner2, body: { scope: 'edit' } });
    const tok2 = keep(t2.json.token);
    const a = await Client.connect('studio', 's-hib', S.owner2);
    await a.request({ type: 'seed', document: FIXTURE }, (m) => m.type === 'state');
    const victim = await Client.connect('studio', 's-hib', tok);
    const c = await Client.connect('studio', 's-hib2', S.owner2);
    await c.request({ type: 'seed', document: FIXTURE }, (m) => m.type === 'state');
    const idle = await Client.connect('studio', 's-hib2', tok2);
    const ia = a.welcome.instance_id;
    const ic = c.welcome.instance_id;
    // No traffic: the runtime evicts idle objects (about 10 s locally) and keeps the sockets.
    await sleep(25_000);
    assert.equal(victim.closeInfo, null, 'the socket must survive hibernation');
    // Revoke both tokens in D1 ONLY (no immediate close), while the objects sleep.
    d1(`UPDATE api_tokens SET revoked_at = '${iso(0)}' WHERE id IN ('${t.json.id}', '${t2.json.id}') AND revoked_at IS NULL`);
    // (1) Traffic on s-hib: the broadcast first rechecks stale peers (their last check is older
    // than 15 s), so the victim is closed and receives NO entry.
    const ack = await a.request(batch(0, 'after hibernation'), (m) => m.type === 'ack' || m.type === 'conflict' || m.type === 'error', 15_000);
    assert.equal(ack.type, 'ack', JSON.stringify(ack));
    const pa = await a.request({ type: 'ping' }, (m) => m.type === 'pong');
    const woke = pa.instance_id !== ia;
    assert.ok(woke, 'the object was not evicted within the idle window: hibernation not observed');
    const told = await victim.next((m) => m.type === 'revoked', 5000);
    assert.equal(told.reason, 'credential_revoked');
    await sleep(300);
    assert.equal(victim.queue.some((m) => m.type === 'entry'), false, 'the revoked socket got no entry');
    // (2) No traffic on s-hib2: the idle sweep must close the revoked token's socket within the bound.
    const t0 = Date.now();
    const told2 = await idle.next((m) => m.type === 'revoked', 75_000);
    const ms = Date.now() - t0;
    console.log(`         idle revoked-token socket told after ${ms} ms after hibernation (bound: 60 s sweep + one D1 read)`);
    assert.equal(told2.reason, 'credential_revoked');
    assert.ok(ms <= 65_000, `too late: ${ms} ms`);
    const pc = await c.request({ type: 'ping' }, (m) => m.type === 'pong');
    assert.notEqual(pc.instance_id, undefined);
    if (pc.instance_id !== ic) console.log('         s-hib2 object was re-instantiated (hibernated) before the sweep');
    a.close();
    c.close();
  });

  await test('after hibernation: the immediate path (API revocation) still closes the socket at once', async () => {
    const t = await call('POST', '/api/workspaces/studio/tokens', { who: S.owner2, body: { scope: 'edit' } });
    const tok = keep(t.json.token);
    const s = await Client.connect('studio', 's-hib', tok);
    const i0 = s.welcome.instance_id;
    await sleep(20_000);
    const t0 = Date.now();
    assert.equal((await call('DELETE', `/api/tokens/${t.json.id}`, { who: S.owner2 })).status, 200);
    assert.equal((await s.next((m) => m.type === 'revoked', 5000)).reason, 'credential_revoked');
    console.log(`         told after ${Date.now() - t0} ms (socket opened on instance ${i0})`);
  });
}

function checkRedaction() {
  return test('server log contains no credential values (tokens, cookies, invitation codes, OAuth state, GitHub tokens, client secret)', async () => {
    assert.ok(SECRETS.size > 10, `too few secrets captured (${SECRETS.size})`);
    const leaks = [...SECRETS].filter((s) => s && fullLog.includes(s));
    assert.deepEqual(leaks.map((l) => `${l.slice(0, 8)}...`), []);
    console.log(`         ${SECRETS.size} credential values checked against ${fullLog.length} bytes of server log`);
  });
}

// --- Main ---------------------------------------------------------------------------------

async function main() {
  if (typeof WebSocket === 'undefined') {
    console.error('test-studio-auth: needs Node 22+ (global WebSocket)');
    return 2;
  }
  let code = 1;
  try {
    await startFakeGithub();
    rmSync(join(ROOT, STATE), { recursive: true, force: true });
    console.log('migrating and seeding the local Studio D1 ...');
    wrangler(['d1', 'migrations', 'apply', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE]);
    seedD1();
    console.log(`fake GitHub on ${GH_BASE}; starting wrangler dev (release build) on :${PORT} ...`);
    await startServer(FAST);
    await phase1();
    if (!ONLY && process.env.STUDIO_AUTH_PHASE1_ONLY !== '1') {
      await stopServer();
      await startServer([]);
      await phase2();
    }
    await stopServer();
    await checkRedaction();
    const failed = results.filter((r) => !r.ok);
    console.log(`\n${results.length - failed.length}/${results.length} auth tests passed`);
    code = failed.length ? 1 : 0;
    if (failed.length) console.log(`server log tail:\n${redactForPrint(fullLog.slice(-2000))}`);
  } catch (e) {
    console.error(`test-studio-auth: ${e.stack ?? e}`);
    console.error(redactForPrint(fullLog.slice(-3000)));
  } finally {
    await stopServer();
    gh?.close();
    // Redacted copy of the whole server log for debugging (gitignored; uploaded by CI on failure).
    writeFileSync(join(ROOT, '.wrangler', 'studio-auth-server.log'), redactForPrint(fullLog));
  }
  const free = !(await portOpen());
  console.log(free ? `port ${PORT} is free` : `WARNING: port ${PORT} is still in use`);
  return free ? code : code || 1;
}

/** Never print a credential from this script either. */
function redactForPrint(text) {
  let out = text;
  for (const s of SECRETS) if (s) out = out.split(s).join('[redacted]');
  return out;
}

process.exit(await main());
