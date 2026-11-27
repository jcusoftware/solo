#!/usr/bin/env node
// Local integration tests for the Studio site command sessions (#62), zero dependencies.
//
// Builds nothing itself: `wrangler dev -c wrangler.studio-sessions.jsonc` runs that config's
// build command, which compiles sws-worker-studio WITH the `insecure-dev-auth` feature into a
// separate crates/sws-worker-studio/build-dev-auth (never the default build/). It then runs the
// real Worker + Durable Object + local D1 on its own port with its own --persist-to state, and
// drives them over real WebSockets (Node 22 global WebSocket with the X-Sws-Dev-User header).
//
// Phase 1 (fast knobs: presence TTL 1.5 s, membership recheck 1 s): auth gates, multi-client
// ordered sync, idempotent retries, stale rebase vs structural/field conflicts, collaborative
// undo, presence expiry, checkpoint every 50 commands (exactly one D1 revision row each),
// membership removal.
// Phase 2 (server restarted with the production defaults on the SAME state directory):
// storage survival across a process restart, then real hibernation (the object is evicted while
// the WebSocket stays open; a fresh instance serves the next message from the restored
// attachment and DO SQL) and the authorization recheck after it.
//
// The dev-auth build is fenced at runtime too: it answers 401 unless the Host is loopback AND the
// var SWS_INSECURE_DEV_AUTH=1 is passed, which only this script does (`--var`, never a config file).
// It also serves a few dev-only routes (`/api/dev/...`) to reach the Worker -> Durable Object
// internals that #65 and #64 will call (`entry::revoke_user`, `entry::fetch_revision`), to prove DO SQL
// `transactionSync`, and to simulate a D1 outage.
//
// Usage: node scripts/test-studio-sessions.mjs [--port 8790]
// Server-dependent: CI only (not part of `npm run verify`). Exit 0 = all passed.

import { spawn, spawnSync } from 'node:child_process';
import http from 'node:http';
import { createHash } from 'node:crypto';
import { readFileSync, rmSync, writeFileSync, mkdtempSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)));
const CFG = 'wrangler.studio-sessions.jsonc';
const STATE = '.wrangler/studio-sessions-state';
const argPort = process.argv.indexOf('--port');
const PORT = Number(argPort > 0 ? process.argv[argPort + 1] : process.env.STUDIO_SESSIONS_PORT || 8790);
const HTTP = `http://127.0.0.1:${PORT}`;
const WS_BASE = `ws://127.0.0.1:${PORT}`;
const FIXTURE_PATH = join(ROOT, 'crates/sws-worker-studio/tests/fixtures/session-site.json');
const FIXTURE = readFileSync(FIXTURE_PATH, 'utf8');
const ORIGINAL = JSON.parse(FIXTURE);

const FAST = ['--var', 'SESSION_PRESENCE_TTL_MS:1500', '--var', 'SESSION_AUTHZ_RECHECK_MS:1000', '--var', 'SESSION_IDLE_SWEEP_MS:1000'];
// Always passed: the runtime fence of the dev-auth build, and the origin this server is reached on.
const BASE_VARS = ['--var', 'SWS_INSECURE_DEV_AUTH:1', '--var', `STUDIO_PUBLIC_ORIGIN:http://127.0.0.1:${PORT}`];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sha256 = (s) => createHash('sha256').update(s).digest('hex');
let opCounter = 0;
const opId = (tag) => `op-${tag}-${String(++opCounter).padStart(5, '0')}`;

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
let serverLog = '';
async function startServer(extra) {
  assert.equal(await portOpen(), false, `port ${PORT} is already in use`);
  serverLog = '';
  server = spawn(
    'npx',
    ['wrangler', 'dev', '-c', CFG, '--persist-to', STATE, '--port', String(PORT), '--ip', '127.0.0.1', ...BASE_VARS, ...extra],
    { cwd: ROOT, detached: true, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  server.stdout.on('data', (d) => (serverLog += d));
  server.stderr.on('data', (d) => (serverLog += d));
  const end = Date.now() + 240_000;
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
  try {
    process.kill(-server.pid, 'SIGTERM');
  } catch {
    // already gone
  }
  const pid = server.pid;
  server = null;
  if (!(await waitPortFree())) {
    // wrangler left a workerd child behind: kill the whole process group we started (by PID, never by name)
    try {
      process.kill(-pid, 'SIGKILL');
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
  const start = out.indexOf('[');
  const parsed = JSON.parse(out.slice(start));
  return parsed.flatMap((p) => p.results ?? []);
}

/** Run a (possibly multi-statement, trigger-containing) SQL file against the local D1. */
function d1File(sql) {
  const dir = mkdtempSync(join(tmpdir(), 'sws-sessions-'));
  const file = join(dir, 'q.sql');
  writeFileSync(file, sql);
  try {
    wrangler(['d1', 'execute', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE, '--file', file]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** Test-only routes of the dev-auth build (loopback + SWS_INSECURE_DEV_AUTH only). */
async function dev(method, path) {
  const r = await fetch(`${HTTP}/api/dev/${path}`, { method });
  return { status: r.status, body: await r.json().catch(() => null) };
}

const revisions = (site) =>
  d1(`SELECT id, revision, ir_sha256, byte_size, created_by FROM site_revisions WHERE workspace_id = 'ws-a' AND site_id = '${site}' ORDER BY revision`);

/** Raw HTTP request that may carry Upgrade headers (fetch() forbids them). Resolves {status, headers, body}. */
function rawGet(path, headers = {}, method = 'GET', body = null) {
  return new Promise((res, rej) => {
    const req = http.request({ host: '127.0.0.1', port: PORT, path, method, headers }, (r) => {
      let text = '';
      r.on('data', (d) => (text += d));
      r.on('end', () => res({ status: r.statusCode, headers: r.headers, body: text }));
    });
    // A 101 means the upgrade was accepted: report it and drop the socket.
    req.on('upgrade', (r, socket) => (socket.destroy(), res({ status: r.statusCode, headers: r.headers, body: '' })));
    req.on('error', rej);
    req.setTimeout(10_000, () => req.destroy(new Error('timeout')));
    if (body) req.write(body);
    req.end();
  });
}
const UPGRADE = { Upgrade: 'websocket', Connection: 'Upgrade', 'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==' };
const upgrade = (path, user) => rawGet(path, { ...UPGRADE, ...(user === undefined ? {} : { 'X-Sws-Dev-User': String(user) }) });

// --- WebSocket client ---------------------------------------------------------------------

class Client {
  constructor(ws) {
    this.ws = ws;
    this.queue = [];
    this.waiters = [];
    this.closeInfo = null;
    /** Every message ever received, in arrival order (the queue is consumed by waiters). */
    this.log = [];
    this.closedP = new Promise((res) => (this.onClosed = res));
    ws.onmessage = (e) => {
      const m = JSON.parse(String(e.data));
      this.log.push(m);
      this.queue.push(m);
      this.flush();
    };
    ws.onclose = (e) => {
      this.closeInfo = { code: e.code, reason: e.reason };
      this.onClosed(this.closeInfo);
    };
  }

  static async connect(site, user, { workspace = 'ws-a' } = {}) {
    const ws = new WebSocket(`${WS_BASE}/api/sites/${workspace}/${site}/session`, {
      headers: { 'X-Sws-Dev-User': String(user) },
    });
    const c = new Client(ws);
    await new Promise((res, rej) => {
      ws.onopen = res;
      ws.addEventListener('error', () => rej(new Error('websocket error before open')), { once: true });
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
          rej(new Error(`timed out waiting for a message; queued: ${JSON.stringify(this.queue).slice(0, 600)}`));
        }
      }, ms);
    });
  }

  async none(pred, ms = 500) {
    await sleep(ms);
    const hit = this.queue.find(pred);
    assert.equal(hit, undefined, `unexpected message: ${JSON.stringify(hit)}`);
  }

  send(obj) {
    this.ws.send(JSON.stringify({ v: 1, ...obj }));
  }

  async request(obj, pred, ms) {
    this.send(obj);
    return this.next(pred, ms);
  }

  async state() {
    // Drop stale `state` broadcasts (for example the seed) so the reply below is the fresh one.
    this.queue = this.queue.filter((m) => m.type !== 'state');
    this.send({ type: 'sync' });
    const m = await this.next((x) => x.type === 'state');
    return m;
  }

  async doc() {
    return JSON.parse((await this.state()).document);
  }

  close() {
    try {
      this.ws.close();
    } catch {
      // ignore
    }
  }
}

const setText = (uid, value) => ({ op: 'set_text', uid, value });
const setAttr = (uid, attr, value) => ({ op: 'set_attr', uid, attr, value });
const batchMsg = (tag, base, commands, op = opId(tag)) => ({ type: 'batch', op_id: op, base_seq: base, batch: { version: 1, commands } });
const titleOf = (doc) => doc.pages[0].body[0].children[0].children[0].children[0].value;

async function seed(client, site = null) {
  const m = await client.request({ type: 'seed', document: FIXTURE }, (x) => x.type === 'state' || x.type === 'error');
  assert.equal(m.type, 'state', JSON.stringify(m));
  assert.equal(m.head_seq, 0);
  return m;
}

/** Apply a batch at the given base and return the ack. */
async function edit(client, base, commands, tag = 'e') {
  const m = await client.request(batchMsg(tag, base, commands), (x) => x.type === 'ack' || x.type === 'conflict' || x.type === 'error');
  assert.equal(m.type, 'ack', JSON.stringify(m));
  return m;
}

// --- Tests --------------------------------------------------------------------------------

const results = [];
// STUDIO_SESSIONS_ONLY=<regex> runs just the matching phase-1 tests (debugging aid; skips phase 2).
const ONLY = process.env.STUDIO_SESSIONS_ONLY ? new RegExp(process.env.STUDIO_SESSIONS_ONLY, 'i') : null;
async function test(name, fn) {
  if (ONLY && !ONLY.test(name)) return;
  const t0 = Date.now();
  try {
    await fn();
    results.push({ name, ok: true });
    console.log(`  ok   ${name} (${Date.now() - t0} ms)`);
  } catch (e) {
    results.push({ name, ok: false, error: e });
    console.log(`  FAIL ${name}\n       ${String(e.stack ?? e).split('\n').slice(0, 6).join('\n       ')}`);
  }
}

const SITES = [
  's-auth', 's-sync', 's-idem', 's-rebase', 's-undo', 's-pres', 's-ckpt', 's-rev1', 's-rev2', 's-persist', 's-hib', 's-roles',
  's-pipe', 's-caps', 's-race', 's-dev', 's-d1', 's-backlog', 's-perm', 's-abandon', 's-retain',
];

function seedD1() {
  const sites = SITES.map((s) => `('ws-a','${s}','${s}','${s}')`).join(',');
  const dir = mkdtempSync(join(tmpdir(), 'sws-sessions-'));
  const file = join(dir, 'seed.sql');
  writeFileSync(
    file,
    `INSERT INTO workspaces (id, name) VALUES ('ws-a','Workspace A'), ('ws-b','Workspace B');
INSERT INTO users (id, github_id, github_login) VALUES (1,1001,'owner'),(2,1002,'editor'),(3,1003,'editor3'),(4,1004,'outsider'),(5,1005,'revoked-a'),(6,1006,'revoked-b'),(7,1007,'hib-peer'),(8,1008,'publisher'),(9,1009,'cap-peer');
INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a',1,'owner'),('ws-a',2,'editor'),('ws-a',3,'editor'),('ws-a',5,'editor'),('ws-a',6,'editor'),('ws-a',7,'editor'),('ws-a',8,'publisher'),('ws-a',9,'editor'),('ws-b',4,'owner');
INSERT INTO sites (workspace_id, id, slug, name) VALUES ${sites},('ws-b','sb','sb','SB');
`,
  );
  wrangler(['d1', 'execute', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE, '--file', file]);
  rmSync(dir, { recursive: true, force: true });
}

async function phase1() {
  console.log('\nphase 1: functional flows (presence TTL 1.5 s, membership recheck 1 s)');
  const persist = { ops: [] };

  await test('route gates: 426 without upgrade, 405, 401 unauthenticated, 403 non-member, 404 unknown site', async () => {
    const plain = await fetch(`${HTTP}/api/sites/ws-a/s-auth/session`);
    assert.equal(plain.status, 426);
    assert.equal((await plain.json()).error, 'upgrade_required');
    assert.equal(plain.headers.get('cache-control'), 'no-store');
    assert.ok(plain.headers.get('strict-transport-security'));
    const post = await fetch(`${HTTP}/api/sites/ws-a/s-auth/session`, { method: 'POST', body: '{}', headers: { 'content-type': 'application/json' } });
    assert.equal(post.status, 405);
    const p = '/api/sites/ws-a/s-auth/session';
    const noAuth = await upgrade(p);
    assert.equal(noAuth.status, 401);
    assert.equal(JSON.parse(noAuth.body).error, 'unauthenticated');
    assert.equal(noAuth.headers['cache-control'], 'no-store');
    assert.ok(noAuth.headers['x-frame-options'], 'security headers on the error response');
    assert.equal((await upgrade(p, 'abc')).status, 401);
    assert.equal((await upgrade(p, 4)).status, 403, 'a user of another workspace');
    assert.equal((await upgrade(p, 999)).status, 403, 'an unknown user');
    assert.equal((await upgrade('/api/sites/ws-b/sb/session', 1)).status, 403, 'a member of ws-a has no access to ws-b');
    assert.equal((await upgrade('/api/sites/ws-a/nope/session', 1)).status, 404);
    // A site of another workspace is not reachable through ws-a (tenant scoping).
    assert.equal((await upgrade('/api/sites/ws-a/sb/session', 1)).status, 404);
    assert.equal((await upgrade(p, 1)).status, 101, 'owner is let through');
  });

  await test('roles: seed needs publisher/owner; editor can edit; second seed refused', async () => {
    const ed = await Client.connect('s-roles', 2);
    const denied = await ed.request({ type: 'seed', document: FIXTURE }, (m) => m.type === 'error');
    assert.equal(denied.code, 'forbidden');
    const pub = await Client.connect('s-roles', 8);
    await seed(pub);
    const again = await pub.request({ type: 'seed', document: FIXTURE }, (m) => m.type === 'error');
    assert.equal(again.code, 'already_seeded');
    const ack = await edit(ed, 0, [setText('home.title-text', 'Editor edit')]);
    assert.equal(ack.seq, 1);
    const noPublish = await ed.request({ type: 'checkpoint', reason: 'publish' }, (m) => m.type === 'error');
    assert.equal(noPublish.code, 'forbidden');
    ed.close();
    pub.close();
  });

  await test('seed refuses a lossy or non-upgraded document', async () => {
    const o = await Client.connect('s-auth', 1);
    const bad = await o.request({ type: 'seed', document: FIXTURE.replace('"sws.ir"', '"other"') }, (m) => m.type === 'error');
    assert.equal(bad.code, 'invalid_document');
    const edit0 = await o.request(batchMsg('x', 0, [setText('a', 'b')]), (m) => m.type === 'error');
    assert.equal(edit0.code, 'not_seeded');
    o.close();
  });

  await test('two clients see each other\'s batches, in order, and converge', async () => {
    const a = await Client.connect('s-sync', 1);
    const b = await Client.connect('s-sync', 2);
    await seed(a);
    assert.equal((await b.state()).head_seq, 0);
    let head = 0;
    const seenByA = [];
    const seenByB = [];
    for (let i = 0; i < 12; i++) {
      const [sender, other, log] = i % 2 === 0 ? [a, b, seenByB] : [b, a, seenByA];
      const ack = await edit(sender, head, [setText('home.title-text', `edit ${i}`)], 'sync');
      head = ack.seq;
      assert.equal(ack.seq, i + 1);
      const e = await other.next((m) => m.type === 'entry');
      log.push(e.seq);
      assert.equal(e.seq, i + 1);
      assert.equal(e.rebased, false);
      assert.equal(e.transaction.forward[0].value, `edit ${i}`);
    }
    assert.deepEqual(seenByB, [1, 3, 5, 7, 9, 11]);
    assert.deepEqual(seenByA, [2, 4, 6, 8, 10, 12]);
    const [da, db] = [await a.doc(), await b.doc()];
    assert.deepEqual(da, db);
    assert.equal(titleOf(da), 'edit 11');
    // A late joiner can replay a small gap or take the full state.
    const c = await Client.connect('s-sync', 3);
    const replay = await c.request({ type: 'sync', since_seq: 8 }, (m) => m.type === 'replay');
    assert.deepEqual(replay.entries.map((x) => x.seq), [9, 10, 11, 12]);
    assert.deepEqual(JSON.parse((await c.state()).document), da);
    // A client that is already current gets an empty replay.
    const none = await c.request({ type: 'sync', since_seq: 12 }, (m) => m.type === 'replay');
    assert.deepEqual(none.entries, []);
    [a, b, c].forEach((x) => x.close());
  });

  await test('idempotent operation ids: a retried batch is applied once and acknowledged again', async () => {
    const a = await Client.connect('s-idem', 1);
    const b = await Client.connect('s-idem', 2);
    await seed(a);
    const msg = batchMsg('idem', 0, [setText('home.title-text', 'once')], 'op-idem-fixed-0001');
    const first = await a.request(msg, (m) => m.type === 'ack');
    assert.deepEqual([first.seq, first.duplicate], [1, false]);
    assert.equal((await b.next((m) => m.type === 'entry')).seq, 1);
    // Retry (lost ack): same op id, same stale base.
    const again = await a.request(msg, (m) => m.type === 'ack');
    assert.deepEqual([again.seq, again.duplicate, again.head_seq], [1, true, 1]);
    await b.none((m) => m.type === 'entry', 400);
    // Even after the document moved on, the retry is still a duplicate, not a conflict.
    await edit(b, 1, [setText('home.title-text', 'later')], 'idem');
    const late = await a.request(msg, (m) => m.type === 'ack' || m.type === 'conflict');
    assert.deepEqual([late.type, late.seq, late.duplicate], ['ack', 1, true]);
    const pong = await a.request({ type: 'ping' }, (m) => m.type === 'pong');
    assert.equal(pong.head_seq, 2, 'the retries added nothing to the log');
    // Another user cannot replay my operation id.
    const stolen = await b.request(msg, (m) => m.type === 'error');
    assert.equal(stolen.code, 'op_id_reused');
    a.close();
    b.close();
  });

  await test('stale batches: independent field updates rebase; overlaps and structural commands are rejected', async () => {
    const a = await Client.connect('s-rebase', 1);
    const b = await Client.connect('s-rebase', 2);
    await seed(a);
    await edit(a, 0, [setText('home.title-text', 'A title')]);
    await edit(a, 1, [setAttr('home.hero', 'data-x', '1')]);
    // B is still at base 0 and edits unrelated fields: rebased on top.
    const ok = await b.request(batchMsg('rb', 0, [setText('home.menu-p-text', 'B soup'), { op: 'set_color_token', key: 'bg', hex: '#112233' }]), (m) => m.type === 'ack' || m.type === 'conflict');
    assert.equal(ok.type, 'ack', JSON.stringify(ok));
    assert.deepEqual([ok.seq, ok.rebased], [3, true]);
    const seenByA = await a.next((m) => m.type === 'entry' && m.seq === 3);
    assert.equal(seenByA.rebased, true);
    assert.equal(seenByA.base_seq, 0);
    const d = await a.doc();
    assert.equal(titleOf(d), 'A title');
    assert.equal(d.tokens.colors.bg, '#112233');
    // Same field concurrently changed: actionable conflict naming seq and path, nothing applied.
    const clash = await b.request(batchMsg('rb', 0, [setText('home.title-text', 'B title')]), (m) => m.type === 'conflict');
    assert.equal(clash.kind, 'field_conflict');
    assert.deepEqual(clash.seqs, [1]);
    assert.deepEqual(clash.paths, ['node:home.title-text/text']);
    assert.equal(clash.head_seq, 3);
    assert.match(clash.message, /since_seq 0/);
    // A stale structural command is rejected even though no field overlaps.
    const move = { op: 'move_node', uid: 'home.visit', container: { kind: 'node', uid: 'home.main' }, index: 0 };
    const stale = await b.request(batchMsg('rb', 2, [move]), (m) => m.type === 'conflict');
    assert.equal(stale.kind, 'stale_structural');
    assert.match(stale.message, /move_node/);
    // The same structural command against the head applies.
    const fresh = await edit(b, 3, [move], 'rb');
    assert.equal(fresh.rebased, false);
    // A stale edit of a node a concurrent batch removed: target_changed.
    await edit(a, 4, [{ op: 'remove_node', uid: 'home.menu' }], 'rb');
    const gone = await b.request(batchMsg('rb', 4, [setText('home.menu-p-text', 'late')]), (m) => m.type === 'conflict');
    assert.equal(gone.kind, 'target_changed');
    // Base ahead of the head, and malformed input, are rejected without side effects.
    const ahead = await b.request(batchMsg('rb', 99, [setText('home.title-text', 'x')]), (m) => m.type === 'error');
    assert.equal(ahead.code, 'invalid_base');
    const invalid = await b.request(batchMsg('rb', 5, [setText('no.such.uid', 'x')]), (m) => m.type === 'error');
    assert.equal(invalid.code, 'command_error');
    assert.equal(invalid.command_error.code, 'not_found');
    b.send({ type: 'batch', op_id: opId('bad'), base_seq: 5, batch: { version: 1, commands: [{ op: 'set_text', uid: 'x', value: 'y', bogus: 1 }] } });
    assert.equal((await b.next((m) => m.type === 'error')).code, 'invalid_batch');
    b.ws.send('not json');
    assert.equal((await b.next((m) => m.type === 'error')).code, 'invalid_message');
    b.ws.send(JSON.stringify({ v: 1, type: 'ping', extra: true }));
    assert.equal((await b.next((m) => m.type === 'error')).code, 'invalid_message');
    assert.equal((await a.request({ type: 'ping' }, (m) => m.type === 'pong')).head_seq, 5);
    a.close();
    b.close();
  });

  await test('collaborative undo is a new inverse command; incompatible and foreign undo are refused', async () => {
    const a = await Client.connect('s-undo', 1);
    const b = await Client.connect('s-undo', 2);
    await seed(a);
    const before = await a.doc();
    await edit(a, 0, [setText('home.title-text', 'changed')]);
    await b.next((m) => m.type === 'entry' && m.seq === 1);
    const undo = await a.request({ type: 'undo', op_id: opId('u'), target_seq: 1 }, (m) => m.type === 'ack' || m.type === 'conflict' || m.type === 'error');
    assert.equal(undo.type, 'ack', JSON.stringify(undo));
    assert.equal(undo.seq, 2, 'undo takes the next sequence number; history is not rewound');
    const seen = await b.next((m) => m.type === 'entry' && m.seq === 2);
    assert.deepEqual([seen.kind, seen.target_seq], ['undo', 1]);
    assert.deepEqual(await b.doc(), before);
    // Undo of the undo (redo) is again a new entry.
    const redo = await a.request({ type: 'undo', op_id: opId('u'), target_seq: 2 }, (m) => m.type === 'ack');
    assert.equal(redo.seq, 3);
    assert.equal(titleOf(await a.doc()), 'changed');
    // Only the author can undo; a later change to the same field blocks the undo.
    const foreign = await b.request({ type: 'undo', op_id: opId('u'), target_seq: 3 }, (m) => m.type === 'error');
    assert.equal(foreign.code, 'undo_not_owner');
    await edit(b, 3, [setText('home.title-text', 'B wins')], 'u');
    const blocked = await a.request({ type: 'undo', op_id: opId('u'), target_seq: 3 }, (m) => m.type === 'conflict');
    assert.equal(blocked.kind, 'undo_incompatible');
    assert.deepEqual(blocked.seqs, [4]);
    assert.deepEqual(blocked.paths, ['node:home.title-text/text']);
    assert.equal(titleOf(await a.doc()), 'B wins', 'the refused undo changed nothing');
    // Undo is idempotent by op id.
    const op = opId('u');
    await edit(a, 4, [setAttr('home.hero', 'data-u', '1')], 'u');
    const u1 = await a.request({ type: 'undo', op_id: op, target_seq: 5 }, (m) => m.type === 'ack');
    const u2 = await a.request({ type: 'undo', op_id: op, target_seq: 5 }, (m) => m.type === 'ack');
    assert.deepEqual([u1.seq, u2.seq, u2.duplicate], [6, 6, true]);
    a.close();
    b.close();
  });

  await test('soft presence is shared, bounded, and expires', async () => {
    const a = await Client.connect('s-pres', 1);
    const b = await Client.connect('s-pres', 2);
    await seed(a);
    a.send({ type: 'presence', state: { cursor: 'home.title' } });
    const seen = await b.next((m) => m.type === 'presence' && m.users.length === 1);
    assert.equal(seen.users[0].client_id, a.welcome.client_id);
    assert.deepEqual(seen.users[0].state, { cursor: 'home.title' });
    assert.ok(seen.users[0].expires_in_ms <= 1500);
    // Oversized or non-object presence is refused.
    a.send({ type: 'presence', state: { k: 'x'.repeat(600) } });
    assert.equal((await a.next((m) => m.type === 'error')).code, 'presence_too_large');
    // After the TTL the entry disappears, and peers are told by the alarm (no activity needed).
    const gone = await b.next((m) => m.type === 'presence' && m.users.length === 0, 6000);
    assert.deepEqual(gone.users, []);
    const list = await b.request({ type: 'presence_get' }, (m) => m.type === 'presence');
    assert.deepEqual(list.users, []);
    // A disconnecting client is removed immediately.
    b.send({ type: 'presence', state: { cursor: 'b' } });
    await a.next((m) => m.type === 'presence' && m.users.length === 1);
    b.close();
    await a.next((m) => m.type === 'presence' && m.users.length === 0, 6000);
    a.close();
  });

  await test('checkpoint: every 50 commands and on save/publish writes exactly one immutable revision row', async () => {
    const a = await Client.connect('s-ckpt', 1);
    const b = await Client.connect('s-ckpt', 2);
    await seed(a);
    let head = 0;
    for (let i = 0; i < 49; i++) head = (await edit(a, head, [setText('home.title-text', `c${i}`)], 'ck')).seq;
    assert.equal(head, 49);
    assert.deepEqual(revisions('s-ckpt'), [], '49 commands: no D1 write at all');
    head = (await edit(a, head, [setText('home.title-text', 'c49')], 'ck')).seq;
    const written = await a.next((m) => m.type === 'checkpoint' && m.status === 'written');
    assert.deepEqual([written.seq, written.reason], [50, 'auto']);
    await b.next((m) => m.type === 'checkpoint' && m.status === 'written');
    let rows = revisions('s-ckpt');
    assert.equal(rows.length, 1);
    const state = await a.state();
    assert.equal(rows[0].ir_sha256, sha256(state.document), 'the revision hashes the canonical document at seq 50');
    assert.equal(rows[0].byte_size, Buffer.byteLength(state.document));
    assert.equal(rows[0].revision, 1);
    assert.equal(rows[0].created_by, 1);
    assert.equal(state.checkpoint_seq, 50);
    for (let i = 0; i < 49; i++) head = (await edit(b, head, [setText('home.title-text', `d${i}`)], 'ck')).seq;
    assert.equal(revisions('s-ckpt').length, 1, '49 more commands: still one row');
    await edit(b, head, [setText('home.title-text', 'd49')], 'ck');
    await a.next((m) => m.type === 'checkpoint' && m.status === 'written' && m.seq === 100);
    rows = revisions('s-ckpt');
    assert.deepEqual(rows.map((r) => [r.revision, r.created_by]), [[1, 1], [2, 2]]);
    // Save with nothing new since the last checkpoint writes nothing.
    const same = await a.request({ type: 'checkpoint', reason: 'save' }, (m) => m.type === 'checkpoint');
    assert.equal(same.status, 'unchanged');
    // Explicit save, then publish, each add one row.
    await edit(a, 100, [setText('home.title-text', 'saved')], 'ck');
    const save = await a.request({ type: 'checkpoint', reason: 'save' }, (m) => m.type === 'checkpoint' && m.reason === 'save');
    assert.deepEqual([save.status, save.seq], ['written', 101]);
    await edit(a, 101, [setText('home.title-text', 'published')], 'ck');
    const pub = await a.request({ type: 'checkpoint', reason: 'publish' }, (m) => m.type === 'checkpoint' && m.reason === 'publish');
    assert.deepEqual([pub.status, pub.seq], ['written', 102]);
    rows = revisions('s-ckpt');
    assert.deepEqual(rows.map((r) => r.revision), [1, 2, 3, 4], 'the unchanged save added no row');
    // Revisions are immutable.
    assert.throws(() => d1(`UPDATE site_revisions SET byte_size = 1 WHERE workspace_id = 'ws-a' AND site_id = 's-ckpt'`), /immutable/);
    // Checkpointing also compacts the command log via the alarm; sync still works from a recent base.
    await sleep(2500);
    const replay = await a.request({ type: 'sync', since_seq: 100 }, (m) => m.type === 'replay' || m.type === 'state');
    assert.equal(replay.type, 'replay');
    assert.deepEqual(replay.entries.map((e) => e.seq), [101, 102]);
    const old = await a.request({ type: 'sync', since_seq: 1 }, (m) => m.type === 'replay' || m.type === 'state');
    assert.equal(old.type, 'state', 'entries older than the retained window force a full resync');
    // A batch based before the compaction horizon must resync.
    const tooOld = await b.request(batchMsg('ck', 2, [setAttr('home.hero', 'data-k', '1')]), (m) => m.type === 'conflict');
    assert.equal(tooOld.kind, 'base_too_old');
    a.close();
    b.close();
  });

  await test('membership removal: edits are refused and the socket is closed; idle sockets are cut off within the recheck bound', async () => {
    const a = await Client.connect('s-rev1', 1);
    const e = await Client.connect('s-rev1', 5);
    await seed(a);
    await edit(e, 0, [setText('home.title-text', 'before removal')]);
    d1(`DELETE FROM memberships WHERE workspace_id = 'ws-a' AND user_id = 5`);
    // (a) the removed editor's next edit never applies (membership is re-read before every edit).
    e.send(batchMsg('rv', 1, [setText('home.title-text', 'after removal')]));
    const revoked = await e.next((m) => m.type === 'revoked');
    assert.equal(revoked.reason, 'membership_revoked');
    assert.equal((await e.closedP).code, 4403);
    assert.equal(titleOf(await a.doc()), 'before removal');
    assert.equal((await a.request({ type: 'ping' }, (m) => m.type === 'pong')).head_seq, 1);
    // A new connection is refused outright.
    assert.equal((await upgrade('/api/sites/ws-a/s-rev1/session', 5)).status, 403);
    a.close();

    // (b) an idle removed member is cut off by the alarm sweep within the recheck bound: it is told
    // (`revoked`, then a 4403 close frame goes out) and receives no further entries.
    const o = await Client.connect('s-rev2', 1);
    const idle = await Client.connect('s-rev2', 6);
    await seed(o);
    await edit(o, 0, [setText('home.title-text', 'visible to idle member')]);
    await idle.next((m) => m.type === 'entry' && m.seq === 1);
    d1(`DELETE FROM memberships WHERE workspace_id = 'ws-a' AND user_id = 6`);
    const t0 = Date.now();
    const told = await idle.next((m) => m.type === 'revoked', 8000);
    const elapsed = Date.now() - t0;
    assert.equal(told.reason, 'membership_revoked');
    console.log(`         idle revoked member told after ${elapsed} ms (recheck interval 1000 ms)`);
    assert.ok(elapsed < 5000, `told too late: ${elapsed} ms`);
    await edit(o, 1, [setText('home.title-text', 'after revocation')]);
    await idle.none((m) => m.type === 'entry', 400);
    // The Node client only reports the close event after the TCP teardown, which the local runtime
    // can delay when the close was initiated from an alarm; when it does report, it must be 4403.
    if (idle.closeInfo) assert.equal(idle.closeInfo.code, 4403);
    o.close();
    idle.close();
  });

  await test('origin gate: a browser Origin other than STUDIO_PUBLIC_ORIGIN is refused before authentication; no Origin is allowed', async () => {
    const p = '/api/sites/ws-a/s-auth/session';
    const evil = await rawGet(p, { ...UPGRADE, 'X-Sws-Dev-User': '1', Origin: 'https://evil.example' });
    assert.equal(evil.status, 403);
    assert.equal(JSON.parse(evil.body).error, 'forbidden_origin');
    assert.equal(evil.headers['cache-control'], 'no-store');
    assert.ok(evil.headers['x-frame-options'] && evil.headers['strict-transport-security'], 'security headers on the refusal');
    // The Origin check runs BEFORE authenticate: an unauthenticated cross-origin request is 403, not 401.
    assert.equal((await rawGet(p, { ...UPGRADE, Origin: 'https://evil.example' })).status, 403);
    assert.equal((await rawGet(p, { ...UPGRADE, 'X-Sws-Dev-User': '1', Origin: 'null' })).status, 403);
    assert.equal((await rawGet(p, { ...UPGRADE, 'X-Sws-Dev-User': '1', Origin: `${HTTP}.evil.example` })).status, 403);
    // The Studio's own origin and clients that send no Origin (CLI/MCP) are let through.
    assert.equal((await rawGet(p, { ...UPGRADE, 'X-Sws-Dev-User': '1', Origin: HTTP })).status, 101);
    assert.equal((await upgrade(p, 1)).status, 101);
  });

  await test('dev-auth runtime fence: without a loopback Host the dev header is ignored (401)', async () => {
    const r = await rawGet('/api/sites/ws-a/s-auth/session', { ...UPGRADE, 'X-Sws-Dev-User': '1', Host: 'studio.example' });
    assert.equal(r.status, 401, 'a non-loopback Host never reaches the dev identity');
    const d = await rawGet('/api/dev/tx-probe/ws-a/s-dev', { Host: 'studio.example' }, 'POST');
    assert.equal(d.status, 404, 'dev routes are closed too');
  });

  await test('connection caps: 4 sockets per user and 16 per site answer 429 with Retry-After', async () => {
    const p = '/api/sites/ws-a/s-caps/session';
    const mine = [];
    for (let i = 0; i < 4; i++) mine.push(await Client.connect('s-caps', 1));
    const fifth = await upgrade(p, 1);
    assert.equal(fifth.status, 429);
    assert.equal(JSON.parse(fifth.body).error, 'too_many_connections_for_user');
    assert.equal(fifth.headers['retry-after'], '5');
    // Another user is not affected by user 1's cap.
    const others = [];
    for (const u of [2, 3, 8]) for (let i = 0; i < 4; i++) others.push(await Client.connect('s-caps', u));
    assert.equal(mine.length + others.length, 16);
    const full = await upgrade(p, 9);
    assert.equal(full.status, 429);
    assert.equal(JSON.parse(full.body).error, 'too_many_connections');
    // Closing one frees a slot.
    mine[0].close();
    await sleep(500);
    assert.equal((await upgrade(p, 9)).status, 101);
    [...mine, ...others].forEach((c) => c.close());
  });

  await test('pipelined messages from one socket are applied in the order they were sent', async () => {
    const a = await Client.connect('s-pipe', 1);
    const b = await Client.connect('s-pipe', 2);
    await seed(a);
    // Five dependent batches (batch i is based on seq i), sent back to back WITHOUT waiting for any ack.
    // Every one awaits a D1 membership read; without per-object FIFO handling a later one could commit
    // first and be refused as `invalid_base`.
    const N = 5;
    for (let i = 0; i < N; i++) a.send(batchMsg('pipe', i, [setText('home.title-text', `pipe ${i}`)], `op-pipe-${i}-fixed`));
    a.send({ type: 'undo', op_id: 'op-pipe-undo-fixed', target_seq: N }); // depends on seq N existing
    a.send({ type: 'ping' });
    const acks = [];
    for (let i = 0; i <= N; i++) acks.push(await a.next((m) => m.type === 'ack' || m.type === 'error' || m.type === 'conflict'));
    assert.deepEqual(acks.map((m) => [m.type, m.seq]), [...Array(N + 1).keys()].map((i) => ['ack', i + 1]), JSON.stringify(acks));
    // The ping that followed them is answered after them, with the final head.
    assert.equal((await a.next((m) => m.type === 'pong')).head_seq, N + 1);
    // The other client received the entries in exactly send order.
    const seen = [];
    for (let i = 0; i <= N; i++) seen.push((await b.next((m) => m.type === 'entry')).seq);
    assert.deepEqual(seen, [1, 2, 3, 4, 5, 6]);
    assert.equal(titleOf(await a.doc()), 'pipe 3', 'the undo of the last batch ran last');
    // Interleaved pipelines from two sockets keep each socket's own order.
    for (let i = 0; i < 4; i++) {
      a.send(batchMsg('pa', 6, [setAttr('home.hero', `data-a${i}`, String(i))], `op-pipe-a${i}-fixed`));
      b.send(batchMsg('pb', 6, [setAttr('home.lede', `data-b${i}`, String(i))], `op-pipe-b${i}-fixed`));
    }
    const ackA = [];
    const ackB = [];
    for (let i = 0; i < 4; i++) {
      ackA.push((await a.next((m) => m.type === 'ack' || m.type === 'conflict' || m.type === 'error')));
      ackB.push((await b.next((m) => m.type === 'ack' || m.type === 'conflict' || m.type === 'error')));
    }
    for (const m of [...ackA, ...ackB]) assert.equal(m.type, 'ack', JSON.stringify(m));
    assert.ok(ackA.every((m, i) => i === 0 || m.seq > ackA[i - 1].seq), 'a: acks in send order');
    assert.ok(ackB.every((m, i) => i === 0 || m.seq > ackB[i - 1].seq), 'b: acks in send order');
    a.close();
    b.close();
  });

  await test('revoke_user (Worker -> DO): closes that user\'s sockets at once; an edit racing the revocation is never applied after it', async () => {
    const a = await Client.connect('s-race', 1);
    await seed(a);
    const victim = await Client.connect('s-race', 2);
    const bystander = await Client.connect('s-race', 3);
    const r0 = await dev('POST', 'revoke/ws-a/s-race/2');
    assert.deepEqual([r0.status, r0.body], [200, { revoked: true }]);
    const told = await victim.next((m) => m.type === 'revoked');
    assert.equal(told.reason, 'credential_revoked');
    // The close event can lag the close frame in the local runtime (see the membership test below);
    // when it is reported it must be 4403.
    const closed = await Promise.race([victim.closedP, sleep(2000).then(() => null)]);
    if (closed) assert.equal(closed.code, 4403);
    // Only that user's sockets: the bystander and owner keep working, and membership is unchanged,
    // so the revoked user can reconnect (revocation is of the credential, not of membership).
    await edit(bystander, 0, [setText('home.title-text', 'bystander edit')], 'race');
    await a.next((m) => m.type === 'entry' && m.seq === 1);
    const again = await Client.connect('s-race', 2);
    again.close();

    // The race: a batch is in flight (it awaits D1) while the credential is revoked. Whatever the
    // interleaving, the edit is either acked BEFORE the `revoked` frame or refused: never applied after it,
    // and the owner (a delivery target) sees an entry only for what was acked.
    let head = 1;
    let acked = 0;
    const ROUNDS = 16;
    for (let round = 0; round < ROUNDS; round++) {
      const e = await Client.connect('s-race', 2);
      // Vary who goes first and by how much, so the revocation lands at different points of the
      // edit's pipeline (before its D1 membership read, during it, or after the commit).
      const gap = (round >> 1) % 4;
      let revoke;
      if (round % 2 === 0) {
        e.send(batchMsg('race', head, [setText('home.title-text', `race ${round}`)]));
        revoke = sleep(gap).then(() => dev('POST', 'revoke/ws-a/s-race/2'));
      } else {
        revoke = dev('POST', 'revoke/ws-a/s-race/2');
        await sleep(gap);
        if (e.ws.readyState === 1) e.send(batchMsg('race', head, [setText('home.title-text', `race ${round}`)]));
      }
      const rev = await revoke;
      assert.equal(rev.status, 200);
      await e.next((m) => m.type === 'revoked', 5000);
      await sleep(600);
      const kinds = e.log.map((m) => m.type).filter((t) => ['ack', 'revoked', 'error', 'conflict'].includes(t));
      const iRev = kinds.indexOf('revoked');
      assert.ok(iRev >= 0, JSON.stringify(kinds));
      assert.ok(!kinds.slice(iRev + 1).includes('ack'), `an edit was acknowledged after revocation: ${JSON.stringify(kinds)}`);
      const ack = e.log.find((m) => m.type === 'ack');
      if (ack) {
        acked++;
        head = ack.seq;
        const entry = await a.next((m) => m.type === 'entry' && m.seq === ack.seq);
        assert.equal(entry.user_id, 2);
      } else {
        await a.none((m) => m.type === 'entry' && m.user_id === 2, 300);
      }
      assert.equal((await a.request({ type: 'ping' }, (m) => m.type === 'pong')).head_seq, head, `round ${round}: log head equals the last acked edit`);
    }
    console.log(`         ${acked}/${ROUNDS} racing edits were acked before the revocation; the rest were refused`);

    // Deterministic window: with membership reads slowed to 400 ms the edit is parked awaiting its own
    // membership recheck and the revocation lands INSIDE that await. The pre-await attachment copy must
    // not be written back (that used to reset `revoked` to false and let the edit through).
    const racing = await Client.connect('s-race', 2);
    const watcher = await Client.connect('s-race', 3);
    assert.equal((await dev('POST', 'd1-fault/ws-a/s-race/slow')).status, 200);
    const base = (await a.request({ type: 'ping' }, (m) => m.type === 'pong')).head_seq;
    racing.send(batchMsg('race', base, [setText('home.title-text', 'parked during recheck')]));
    await sleep(150);
    assert.equal((await dev('POST', 'revoke/ws-a/s-race/2')).status, 200);
    assert.equal((await dev('POST', 'd1-fault/ws-a/s-race/up')).status, 200);
    await racing.next((m) => m.type === 'revoked', 5000);
    await sleep(1200); // let the parked handler resume
    assert.equal(racing.log.some((m) => m.type === 'ack'), false, 'the parked edit must not be acked after revocation');
    assert.equal(a.log.some((m) => m.type === 'entry' && m.seq > base), false, 'nothing was broadcast');
    assert.equal(watcher.log.some((m) => m.type === 'entry' && m.seq > base), false);
    assert.equal((await a.request({ type: 'ping' }, (m) => m.type === 'pong')).head_seq, base, 'the parked edit was not applied');
    console.log('         an edit parked in its membership recheck was refused when the revocation landed meanwhile');
    watcher.close();    // After a revocation lands, an edit on the dead socket is never applied.
    victim.send(batchMsg('race', head, [setText('home.title-text', 'from a revoked socket')]));
    await sleep(500);
    assert.equal((await a.request({ type: 'ping' }, (m) => m.type === 'pong')).head_seq, head);
    [a, bystander].forEach((c) => c.close());
  });

  await test('transactionSync: DO SQL is available and a failed body rolls its writes back', async () => {
    const r = await dev('POST', 'tx-probe/ws-a/s-dev');
    assert.equal(r.status, 200, JSON.stringify(r));
    assert.deepEqual(r.body, { transaction_sync_available: true, rows_after_failed_body: 0 });
    console.log(`         ${JSON.stringify(r.body)}`);
  });

  await test('D1 unavailable: edits and stale reads fail closed (sockets stay open, nothing applied or broadcast); service resumes', async () => {
    const a = await Client.connect('s-d1', 1);
    const b = await Client.connect('s-d1', 2);
    await seed(a);
    await edit(a, 0, [setText('home.title-text', 'before outage')], 'd1');
    await b.next((m) => m.type === 'entry' && m.seq === 1);
    assert.equal((await dev('POST', 'd1-fault/ws-a/s-d1/down')).status, 200);
    await sleep(1300); // the cached verification (recheck interval 1 s) is now stale
    const refused = await a.request(batchMsg('d1', 1, [setText('home.title-text', 'during outage')]), (m) => m.type === 'error' || m.type === 'ack');
    assert.equal(refused.type, 'error');
    assert.equal(refused.code, 'unavailable', 'membership could not be verified: fail closed');
    b.queue = b.queue.filter((m) => m.type !== 'state'); // drop the seed broadcast
    const readRefused = await b.request({ type: 'sync' }, (m) => m.type === 'error' || m.type === 'state');
    assert.equal(readRefused.code, 'unavailable', `a stale cached role is not trusted for reads either: ${JSON.stringify(readRefused).slice(0, 200)}`);
    await b.none((m) => m.type === 'entry', 300);
    assert.equal(a.closeInfo, null, 'an outage does not close sockets');
    assert.equal((await dev('POST', 'd1-fault/ws-a/s-d1/up')).status, 200);
    const ok = await edit(a, 1, [setText('home.title-text', 'after outage')], 'd1');
    assert.equal(ok.seq, 2, 'the refused edit left no trace');
    assert.equal((await b.next((m) => m.type === 'entry')).seq, 2);
    a.close();
    b.close();
  });

  await test('checkpoint_backlog (DO level): with the D1 write failing, edits are refused 200 commands past the last checkpoint and resume once it lands', async () => {
    const a = await Client.connect('s-backlog', 1);
    await seed(a);
    assert.equal((await dev('POST', 'd1-fault/ws-a/s-backlog/checkpoint')).status, 200);
    let head = 0;
    let refused = null;
    for (let i = 0; i < 300 && !refused; i++) {
      const m = await a.request(batchMsg('bk', head, [setText('home.title-text', `bk ${i}`)]), (x) => x.type === 'ack' || x.type === 'error');
      if (m.type === 'ack') head = m.seq;
      else refused = m;
    }
    assert.ok(refused, 'edits were never refused');
    assert.equal(refused.code, 'checkpoint_backlog');
    assert.equal(head, 250, 'refused exactly 200 commands after the checkpoint at seq 50');
    assert.equal(revisions('s-backlog').length, 0, 'D1 never received the checkpoint');
    assert.equal(a.closeInfo, null);
    assert.equal((await dev('POST', 'd1-fault/ws-a/s-backlog/up')).status, 200);
    // An explicit save resolves the outstanding row first (so the wedge clears at once), then checkpoints.
    const saved = await a.request({ type: 'checkpoint', reason: 'save' }, (m) => m.type === 'checkpoint' && m.seq === 250);
    assert.equal(saved.status, 'written');
    assert.deepEqual(revisions('s-backlog').map((r) => r.revision), [1, 2], 'the pending seq-50 row and the seq-250 row, each exactly once');
    const ok = await edit(a, 250, [setText('home.title-text', 'resumed')], 'bk');
    assert.equal(ok.seq, 251);
    a.close();
  });

  await test('a checkpoint that can never be written: re-attributed to the workspace owner, or dropped with an alert so edits resume', async () => {
    // (a) The recorded author is refused by D1 (a constraint violation, standing in for a deleted user):
    // the checkpoint is re-attributed to the workspace owner and written.
    d1File(`CREATE TRIGGER t_perm_author BEFORE INSERT ON site_revisions WHEN NEW.created_by <> 1 BEGIN SELECT RAISE(ABORT, 'constraint failed: author not permitted (test)'); END;`);
    try {
      const o = await Client.connect('s-perm', 1);
      const ed = await Client.connect('s-perm', 2);
      await seed(o);
      let head = 0;
      for (let i = 0; i < 50; i++) head = (await edit(ed, head, [setText('home.title-text', `perm ${i}`)], 'pm')).seq;
      const written = await o.next((m) => m.type === 'checkpoint' && m.seq === 50, 15_000);
      assert.equal(written.status, 'written');
      const rows = revisions('s-perm');
      assert.deepEqual(rows.map((r) => r.created_by), [1], 'written as the owner, not the refused author');
      o.close();
      ed.close();
    } finally {
      d1File('DROP TRIGGER IF EXISTS t_perm_author;');
    }
    // (b) Refused whoever the author is: dropped so the site is not wedged; edits keep working.
    d1File(`CREATE TRIGGER t_perm_all BEFORE INSERT ON site_revisions BEGIN SELECT RAISE(ABORT, 'constraint failed: unwritable (test)'); END;`);
    try {
      const o = await Client.connect('s-abandon', 1);
      await seed(o);
      let head = 0;
      for (let i = 0; i < 50; i++) head = (await edit(o, head, [setText('home.title-text', `ab ${i}`)], 'ab')).seq;
      const dropped = await o.next((m) => m.type === 'checkpoint' && m.seq === 50, 15_000);
      assert.equal(dropped.status, 'abandoned');
      assert.deepEqual(revisions('s-abandon'), []);
      // Not wedged: edits continue well past where the backlog guard would have refused (200).
      for (let i = 0; i < 12; i++) head = (await edit(o, head, [setText('home.title-text', `ab-after ${i}`)], 'ab')).seq;
      assert.equal(head, 62);
      // When the cause is fixed, an explicit save writes a fresh checkpoint.
      d1File('DROP TRIGGER IF EXISTS t_perm_all;');
      const save = await o.request({ type: 'checkpoint', reason: 'save' }, (m) => m.type === 'checkpoint' && m.seq === head);
      assert.equal(save.status, 'written');
      assert.equal(revisions('s-abandon').length, 1);
      o.close();
    } finally {
      d1File('DROP TRIGGER IF EXISTS t_perm_all;');
    }
  });

  await test('checkpoint revisions stay recoverable: unchanged publish names the last revision; publish and latest checkpoint bytes are fetchable by id', async () => {
    const a = await Client.connect('s-retain', 8); // a publisher
    await seed(a);
    let head = 0;
    for (let i = 0; i < 2; i++) head = (await edit(a, head, [setText('home.title-text', `r ${i}`)], 'rt')).seq;
    const save = await a.request({ type: 'checkpoint', reason: 'save' }, (m) => m.type === 'checkpoint');
    assert.equal(save.status, 'written');
    assert.match(save.revision_id, /^ck-[0-9a-f]{24}$/);
    head = (await edit(a, head, [setText('home.title-text', 'to publish')], 'rt')).seq;
    const pub = await a.request({ type: 'checkpoint', reason: 'publish' }, (m) => m.type === 'checkpoint' && m.reason === 'publish');
    assert.equal(pub.status, 'written');
    // Publishing again with nothing new: `unchanged` names that same revision (never an empty id).
    const again = await a.request({ type: 'checkpoint', reason: 'publish' }, (m) => m.type === 'checkpoint' && m.status === 'unchanged');
    assert.equal(again.revision_id, pub.revision_id);
    assert.equal(again.seq, pub.seq);
    // The id is the D1 row id, and its bytes are fetchable (the contract #64 consumes).
    const rows = revisions('s-retain');
    assert.equal(rows.at(-1).id, pub.revision_id);
    const got = await dev('GET', `revision/ws-a/s-retain/${pub.revision_id}`);
    assert.equal(got.status, 200, JSON.stringify(got));
    assert.equal(sha256(got.body.document), rows.at(-1).ir_sha256, 'the bytes hash to the revision\'s ir_sha256');
    assert.equal(got.body.seq, pub.seq);
    // 50 more commands: an automatic checkpoint becomes the latest, the publish snapshot is still kept,
    // and the older non-publish checkpoint is gone (retention is bounded).
    for (let i = 0; i < 50; i++) head = (await edit(a, head, [setText('home.title-text', `m ${i}`)], 'rt')).seq;
    const auto = await a.next((m) => m.type === 'checkpoint' && m.status === 'written' && m.reason === 'auto');
    assert.equal((await dev('GET', `revision/ws-a/s-retain/${pub.revision_id}`)).status, 200, 'latest publish still retained');
    assert.equal((await dev('GET', `revision/ws-a/s-retain/${auto.revision_id}`)).status, 200, 'latest checkpoint retained');
    assert.equal((await dev('GET', `revision/ws-a/s-retain/${save.revision_id}`)).status, 404, 'older non-publish snapshot dropped');
    assert.equal((await dev('GET', 'revision/ws-a/s-retain/ck-000000000000000000000000')).status, 404);
    a.close();
  });

  await test('s-persist: build up state that must survive the restart', async () => {
    const a = await Client.connect('s-persist', 1);
    const b = await Client.connect('s-persist', 2);
    await seed(a);
    let head = 0;
    for (let i = 0; i < 7; i++) {
      const op = `op-persist-${i}-fixed`;
      const msg = batchMsg('p', head, [setText('home.title-text', `persist ${i}`)], op);
      const ack = await a.request(msg, (m) => m.type === 'ack');
      persist.ops.push({ op, msg, seq: ack.seq });
      head = ack.seq;
    }
    persist.doc = (await a.state()).document;
    persist.instance = a.welcome.instance_id;
    persist.head = head;
    a.close();
    b.close();
  });

  return persist;
}

async function phase2(persist) {
  console.log('\nphase 2: server restarted on the same state with production defaults');

  await test('restart: document, log and idempotency index survive a full process restart', async () => {
    const a = await Client.connect('s-persist', 1);
    assert.equal(a.welcome.head_seq, persist.head);
    assert.equal(a.welcome.seeded, true);
    assert.notEqual(a.welcome.instance_id, persist.instance, 'a new object instance was constructed');
    const state = await a.state();
    assert.equal(state.document, persist.doc, 'the document is byte-identical after the restart');
    const replay = await a.request({ type: 'sync', since_seq: 3 }, (m) => m.type === 'replay');
    assert.deepEqual(replay.entries.map((e) => e.seq), [4, 5, 6, 7]);
    // A retry of an operation applied before the restart is still a duplicate.
    const dup = await a.request(persist.ops[2].msg, (m) => m.type === 'ack');
    assert.deepEqual([dup.seq, dup.duplicate], [persist.ops[2].seq, true]);
    const next = await edit(a, persist.head, [setText('home.title-text', 'after restart')], 'p');
    assert.equal(next.seq, persist.head + 1);
    a.close();
  });

  await test('hibernation: the object is evicted while the socket stays open; a new instance serves the next message', async () => {
    const a = await Client.connect('s-hib', 1);
    const peer = await Client.connect('s-hib', 7);
    await seed(a);
    const before = (await edit(a, 0, [setText('home.title-text', 'before sleep')], 'h')).seq;
    await peer.next((m) => m.type === 'entry' && m.seq === before);
    const i1 = a.welcome.instance_id;
    let woke = null;
    for (const idleMs of [14_000, 30_000]) {
      // No traffic at all: the runtime may now evict the object (about 10 s idle) and keep the sockets.
      await sleep(idleMs);
      assert.equal(a.closeInfo, null, 'the WebSocket must stay open across hibernation');
      const pong = await a.request({ type: 'ping' }, (m) => m.type === 'pong');
      if (pong.instance_id !== i1) {
        woke = pong;
        break;
      }
    }
    assert.ok(woke, `the object was not evicted within the idle windows (instance ${i1} still serving): hibernation not observed`);
    console.log(`         instance ${i1} -> ${woke.instance_id} on the SAME socket (hibernated and woke)`);
    assert.equal(woke.head_seq, before, 'state came back from DO SQLite');
    // Identity came back from the serialized attachment: the same client id authors the next edit.
    const ack = await edit(a, before, [setText('home.title-text', 'after wake')], 'h');
    const entry = await peer.next((m) => m.type === 'entry' && m.seq === ack.seq);
    assert.equal(entry.client_id, a.welcome.client_id);
    assert.equal(entry.user_id, 1);
    // The peer socket (also restored) kept receiving.
    assert.equal(titleOf(await peer.doc()), 'after wake');
    // Restored identity is not authority: remove the peer's membership and its next edit is refused.
    d1(`DELETE FROM memberships WHERE workspace_id = 'ws-a' AND user_id = 7`);
    peer.send(batchMsg('h', ack.seq, [setText('home.title-text', 'revoked edit')]));
    assert.equal((await peer.next((m) => m.type === 'revoked')).reason, 'membership_revoked');
    assert.equal((await peer.closedP).code, 4403);
    assert.equal(titleOf(await a.doc()), 'after wake');
    a.close();
  });
}

// --- Main ---------------------------------------------------------------------------------

async function main() {
  if (typeof WebSocket === 'undefined') {
    console.error('test-studio-sessions: needs Node 22+ (global WebSocket)');
    return 2;
  }
  let code = 1;
  try {
    rmSync(join(ROOT, STATE), { recursive: true, force: true });
    console.log('migrating and seeding the local session D1 ...');
    wrangler(['d1', 'migrations', 'apply', 'STUDIO_DB', '--local', '-c', CFG, '--persist-to', STATE]);
    seedD1();
    console.log(`starting wrangler dev on :${PORT} (builds the insecure-dev-auth Worker on first run) ...`);
    await startServer(FAST);
    const persist = await phase1();
    if (!ONLY) {
      await stopServer();
      await startServer([]);
      await phase2(persist);
    }
    const failed = results.filter((r) => !r.ok);
    console.log(`\n${results.length - failed.length}/${results.length} session tests passed`);
    code = failed.length ? 1 : 0;
    if (failed.length) console.log(`server log tail:\n${serverLog.slice(-1500)}`);
  } catch (e) {
    console.error(`test-studio-sessions: ${e.stack ?? e}`);
    console.error(serverLog.slice(-3000));
  } finally {
    await stopServer();
  }
  const free = !(await portOpen());
  console.log(free ? `port ${PORT} is free` : `WARNING: port ${PORT} is still in use`);
  return free ? code : code || 1;
}

process.exit(await main());
