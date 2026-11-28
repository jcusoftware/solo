#!/usr/bin/env node
// Local integration tests for Studio Direct Upload publish (#66), #65 activation auth,
// and #64 per-file byte verification. Server-dependent: CI only (not part of `npm run verify`).
//
// Ports:
//   8800  Studio Worker (build-insecure-dev-auth, --test-scheduled)
//   8801  Fake Cloudflare API HTTP server (tests/fakes/cloudflare/http.mjs)
//   9398  Wrangler inspector

import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseRustConst } from '../core/tools/check-headers.mjs';
import { createFakeCloudflare } from '../tests/fakes/cloudflare/http.mjs';

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)));
const WRANGLER = join(ROOT, 'node_modules', '.bin', 'wrangler');
const CRATE = join(ROOT, 'crates', 'sws-worker-studio');
const STATE = join(ROOT, '.wrangler', 'studio-deploy-test');
const STUDIO_PORT = 8800;
const CF_PORT = 8801;
const INSPECTOR_PORT = 9398;
const ORIGIN = `http://127.0.0.1:${STUDIO_PORT}`;

const ACCOUNT_ID = '0123456789abcdef0123456789abcdef';
const SCRIPT_NAME = 'hearth-site';
const CF_TOKEN = `cfat_supersecret_${randomBytes(12).toString('hex')}`;
const SITE_HEADERS = parseRustConst(
  readFileSync(join(ROOT, 'crates/sws-security/src/headers.rs'), 'utf8'),
  'SECURITY_HEADERS',
);

const SHA = (s) => createHash('sha256').update(s).digest('hex');
const hex = (n) => randomBytes(n).toString('hex');
const isoAgo = (secs) => new Date(Date.now() - secs * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const jobIdOf = (ws, site, req) => `j${SHA(`publish\0${ws}\0${site}\0${req}`).slice(0, 47)}`;

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

function buildBundle({ revisionId, revisionSha, schemaLevel = 4, files }) {
  const sorted = [...files]
    .map(([path, data]) => {
      const buf = Buffer.isBuffer(data) ? data : Buffer.from(data, 'utf8');
      return { path, buf, sha256: SHA(buf), size: buf.length };
    })
    .sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  const totalBytes = sorted.reduce((acc, f) => acc + f.size, 0);
  const manifest = {
    format: 'sws-artifact-manifest/1',
    revision_id: revisionId,
    revision_sha256: revisionSha,
    compiler: { name: 'sws-cli', version: '0.2.0' },
    files: sorted.map((f) => ({ path: f.path, sha256: f.sha256, size: f.size })),
    file_count: sorted.length,
    total_bytes: totalBytes,
  };
  const canonical = JSON.stringify(manifest);
  const manifestSha = SHA(canonical);

  const workerCode = Buffer.from('export default { fetch() { return new Response("ok"); } };', 'utf8');
  const headerObj = {
    revision_id: revisionId,
    revision_sha256: revisionSha,
    compiler: { name: 'sws-cli', version: '0.2.0' },
    schema_level: schemaLevel,
    worker: {
      main_module: 'index.js',
      modules: [{ name: 'index.js', content_type: 'application/javascript+module', size: workerCode.length }],
    },
    files: sorted.map((f) => ({ path: f.path, size: f.size })),
  };
  const headerBuf = Buffer.from(JSON.stringify(headerObj), 'utf8');
  const lenBuf = Buffer.alloc(4);
  lenBuf.writeUInt32BE(headerBuf.length, 0);
  const bundle = Buffer.concat([Buffer.from('SWSBNDL1', 'ascii'), lenBuf, headerBuf, workerCode, ...sorted.map((f) => f.buf)]);
  return { manifest, manifestSha, bundle };
}

function request(port, method, path, headers, body) {
  return new Promise((resolveP, reject) => {
    const h = { ...headers };
    if (body !== undefined && h['Content-Length'] === undefined) h['Content-Length'] = Buffer.byteLength(body);
    const req = http.request({ host: '127.0.0.1', port, path, method, headers: h, agent: false }, (r) => {
      const chunks = [];
      r.on('data', (d) => chunks.push(d));
      r.on('end', () => {
        const text = Buffer.concat(chunks).toString('utf8');
        let json = null;
        try { json = JSON.parse(text); } catch { /* non-json */ }
        resolveP({ status: r.statusCode, text, json, headers: r.headers });
      });
    });
    req.on('error', reject);
    req.setTimeout(30_000, () => req.destroy(new Error('timeout')));
    if (body !== undefined) req.write(body);
    req.end();
  });
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

let studioChild = null;
let cfFake = null;

function stopAll() {
  if (studioChild?.pid) {
    try { process.kill(-studioChild.pid, 'SIGTERM'); } catch { /* gone */ }
  }
  if (cfFake?.server) {
    try { cfFake.server.close(); } catch { /* gone */ }
  }
}
process.on('exit', stopAll);
process.on('SIGINT', () => { stopAll(); process.exit(1); });
process.on('SIGTERM', () => { stopAll(); process.exit(1); });

async function main() {
  rmSync(STATE, { recursive: true, force: true });
  mkdirSync(STATE, { recursive: true });

  // Build test-only dev-auth Worker into build-insecure-dev-auth/.
  const build = spawnSync(process.execPath, [join(ROOT, 'scripts', 'build-wasm.mjs'), '--profile', 'dev', '--crate', 'crates/sws-worker-studio', '--features', 'insecure-dev-auth'], {
    cwd: ROOT,
    encoding: 'utf8',
    env: { ...process.env, CARGO_TERM_COLOR: 'never' },
  });
  assert.equal(build.status, 0, `worker-build failed:\n${build.stdout}\n${build.stderr}`);

  cfFake = createFakeCloudflare({ accountId: ACCOUNT_ID, scriptName: SCRIPT_NAME, token: CF_TOKEN });
  await new Promise((r) => cfFake.server.listen(CF_PORT, '127.0.0.1', r));

  const cfg = join(STATE, 'wrangler.jsonc');
  const persist = join(STATE, 'state');
  writeFileSync(
    cfg,
    JSON.stringify({
      name: 'sws-studio-deploy-test',
      main: join(CRATE, 'build-insecure-dev-auth', 'index.js'),
      compatibility_date: '2026-11-01',
      workers_dev: false,
      d1_databases: [
        {
          binding: 'STUDIO_DB',
          database_name: 'sws-studio-deploy-test',
          migrations_dir: join(CRATE, 'migrations'),
          database_id: '00000000-0000-0000-0000-000000000066',
        },
      ],
      vars: { STUDIO_VERSION: 'deploy-test', STUDIO_PUBLIC_ORIGIN: ORIGIN },
    }),
  );

  wr(['d1', 'migrations', 'apply', 'STUDIO_DB', '--local', '-c', cfg, '--persist-to', persist]);

  const creds = {
    ownerFresh: { ...mintSession(), user: 1, ageSecs: 60 },
    ownerStale: { ...mintSession(), user: 1, ageSecs: 20 * 60 },
    editorFresh: { ...mintSession(), user: 3, ageSecs: 60 },
    editToken: { ...mintToken(), user: 4, ws: 'ws-a', scope: 'edit' },
    publishToken: { ...mintToken(), user: 4, ws: 'ws-a', scope: 'publish' },
    activateToken: { ...mintToken(), user: 1, ws: 'ws-a', scope: 'activate' },
    pubActivateToken: { ...mintToken(), user: 4, ws: 'ws-a', scope: 'activate' },
    revocableToken: { ...mintToken(), user: 1, ws: 'ws-a', scope: 'activate' },
    otherWsToken: { ...mintToken(), user: 4, ws: 'ws-b', scope: 'activate' },
  };

  const REV1_SHA = SHA('rev-1-ir');
  const REV2_SHA = SHA('rev-2-ir');
  const REV3_SHA = SHA('rev-3-ir');
  const REV4_SHA = SHA('rev-4-ir');
  const REV5_SHA = SHA('rev-5-ir');
  const BINDINGS = JSON.stringify([
    { type: 'd1', name: 'DB', id: '00000000-0000-0000-0000-000000000001' },
    { type: 'plain_text', name: 'TURNSTILE_SITE_KEY', text: '1x00000000000000000000AA' },
  ]);

  const seedLines = [
    `INSERT INTO workspaces (id, name) VALUES ('ws-a', 'Workspace A'), ('ws-b', 'Workspace B');`,
    `INSERT INTO users (github_id, github_login) VALUES (1001, 'alice'), (2002, 'bob'), (3003, 'carol'), (4004, 'dave');`,
    `INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a', 1, 'owner'), ('ws-a', 3, 'editor'), ('ws-a', 4, 'publisher'), ('ws-b', 2, 'owner'), ('ws-b', 4, 'publisher');`,
    `INSERT INTO sites (workspace_id, id, slug, name) VALUES ('ws-a', 'site-a', 'hearth', 'Hearth'), ('ws-b', 'site-b', 'other', 'Other');`,
    `INSERT INTO site_revisions (workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) VALUES
      ('ws-a', 'site-a', 'rev-1', 1, '${REV1_SHA}', 100, 1),
      ('ws-a', 'site-a', 'rev-2', 2, '${REV2_SHA}', 100, 1),
      ('ws-a', 'site-a', 'rev-3', 3, '${REV3_SHA}', 100, 1),
      ('ws-a', 'site-a', 'rev-4', 4, '${REV4_SHA}', 100, 1),
      ('ws-a', 'site-a', 'rev-5', 5, '${REV5_SHA}', 100, 1);`,
    `INSERT INTO publish_targets (workspace_id, site_id, id, account_id, script_name, token_secret, bindings_json, compatibility_date, site_schema_level) VALUES
      ('ws-a', 'site-a', 'target-a', '${ACCOUNT_ID}', '${SCRIPT_NAME}', 'CLOUDFLARE_API_TOKEN', '${BINDINGS}', '2026-11-01', 4);`,
  ];
  for (const v of Object.values(creds)) {
    if (v.ageSecs !== undefined) {
      const created = isoAgo(v.ageSecs);
      seedLines.push(
        `INSERT INTO auth_sessions (id, user_id, secret_hash, created_at, last_seen_at, expires_at) VALUES ('${v.id}', ${v.user}, '${v.hash}', '${created}', '${isoAgo(30)}', '${isoAgo(v.ageSecs - 86_400)}');`,
      );
    } else {
      const created = isoAgo(60);
      seedLines.push(
        `INSERT INTO api_tokens (id, workspace_id, user_id, token_hash, scope, label, created_at, expires_at, chain_started_at) VALUES ('${v.id}', '${v.ws}', ${v.user}, '${v.hash}', '${v.scope}', 'deploy-test', '${created}', '${isoAgo(60 - 23 * 3600)}', '${created}');`,
      );
    }
  }
  const seedFile = join(STATE, 'seed.sql');
  writeFileSync(seedFile, seedLines.join('\n'));
  wr(['d1', 'execute', 'STUDIO_DB', '--local', '-c', cfg, '--persist-to', persist, '--file', seedFile]);

  const args = [
    'dev', '-c', cfg, '--port', String(STUDIO_PORT), '--ip', '127.0.0.1',
    '--inspector-port', String(INSPECTOR_PORT), '--persist-to', persist, '--test-scheduled',
    '--var', 'SWS_INSECURE_DEV_AUTH:1',
    '--var', `STUDIO_DEV_CF_API_BASE:http://127.0.0.1:${CF_PORT}/client/v4`,
    '--var', `CLOUDFLARE_API_TOKEN:${CF_TOKEN}`,
  ];
  studioChild = spawn(WRANGLER, args, {
    cwd: ROOT,
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, CI: '1', WRANGLER_SEND_METRICS: 'false' },
  });
  let serverLog = '';
  studioChild.stdout.on('data', (d) => (serverLog += d));
  studioChild.stderr.on('data', (d) => (serverLog += d));

  let ready = false;
  for (let i = 0; i < 90; i++) {
    try {
      const r = await request(STUDIO_PORT, 'GET', '/api/health', {});
      if (r.status === 200) { ready = true; break; }
    } catch { /* wait */ }
    await sleep(1000);
  }
  assert.ok(ready, `Studio server failed to start:\n${serverLog}`);

  const tok = (t) => ({ Authorization: `Bearer ${t.value}` });
  const cookie = (s, withOrigin = true) => ({
    Cookie: `__Host-sws_session=${s.value}`,
    ...(withOrigin ? { Origin: ORIGIN } : {}),
  });
  const runCron = async () => {
    const r = await request(STUDIO_PORT, 'GET', '/__scheduled', {});
    assert.equal(r.status, 200);
  };
  const d1Exec = (sql) => wr(['d1', 'execute', 'STUDIO_DB', '--local', '-c', cfg, '--persist-to', persist, '--command', sql]);

  // 1. Bundle upload + tamper / reserved-path rejection.
  const b1 = buildBundle({
    revisionId: 'rev-1',
    revisionSha: REV1_SHA,
    files: [['index.html', '<h1>Hearth v1</h1>'], ['styles.css', 'body{color:#241207}']],
  });
  const tampered = Buffer.from(b1.bundle);
  tampered[tampered.length - 1] ^= 0xff;
  const rTamper = await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${b1.manifestSha}`, {
    ...tok(creds.publishToken),
    'Content-Type': 'application/octet-stream',
  }, tampered);
  assert.equal(rTamper.status, 422);
  assert.equal(rTamper.json?.error, 'manifest_mismatch');

  const bReserved = buildBundle({ revisionId: 'rev-1', revisionSha: REV1_SHA, files: [['_headers', '/*\n']] });
  const rRes = await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${bReserved.manifestSha}`, {
    ...tok(creds.publishToken),
    'Content-Type': 'application/octet-stream',
  }, bReserved.bundle);
  assert.equal(rRes.status, 400);
  assert.equal(rRes.json?.error, 'reserved_path');

  const rUp1 = await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${b1.manifestSha}`, {
    ...tok(creds.publishToken),
    'Content-Type': 'application/octet-stream',
  }, b1.bundle);
  assert.equal(rUp1.status, 201);
  assert.equal(rUp1.json?.created, true);

  const rUp1Dup = await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${b1.manifestSha}`, {
    ...tok(creds.publishToken),
    'Content-Type': 'application/octet-stream',
  }, b1.bundle);
  assert.equal(rUp1Dup.status, 200);
  assert.equal(rUp1Dup.json?.created, false);

  // 2. Enqueue with activate:true + cron drive -> Direct Upload + metadata + 100% deployment.
  const rEnq1 = await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', {
    ...tok(creds.activateToken),
    'Content-Type': 'application/json',
  }, JSON.stringify({ request_id: 'req-1', manifest: b1.manifest, activate: true }));
  assert.equal(rEnq1.status, 202);
  await runCron();
  const j1Id = jobIdOf('ws-a', 'site-a', 'req-1');
  const rSt1 = await request(STUDIO_PORT, 'GET', `/api/sites/ws-a/site-a/publish/${j1Id}`, tok(creds.activateToken));
  assert.equal(rSt1.status, 200);
  assert.equal(rSt1.json.phase, 'ready', JSON.stringify(rSt1.json));
  assert.equal(rSt1.json.bytes_verified, true);
  assert.ok(rSt1.json.deployment?.published_id);
  const pv1 = rSt1.json.deployment.published_id;
  const v1Cf = rSt1.json.deployment.cf_version_id;
  assert.equal(cfFake.state.uploaded.length, 2);
  assert.equal(cfFake.state.deployments[0].versions[0].version_id, v1Cf);
  assert.equal(cfFake.state.deployments[0].versions[0].percentage, 100);
  const v1Meta = cfFake.state.versions.get(v1Cf);
  assert.deepEqual(v1Meta.assets.config.run_worker_first, ['/api/*']);
  assert.deepEqual(v1Meta.keep_bindings, ['secret_text', 'secret_key']);
  assert.ok(v1Meta.bindings.some((b) => b.type === 'd1' && b.name === 'DB'));
  assert.ok(v1Meta.bindings.some((b) => b.type === 'plain_text' && b.name === 'TURNSTILE_SITE_KEY'));
  for (const forbidden of ['STUDIO_DB', 'SITE_SESSION', 'PUBLISH_QUEUE']) {
    assert.ok(!v1Meta.bindings.some((b) => b.name === forbidden), `Studio binding leaked: ${forbidden}`);
  }
  for (const [k, v] of SITE_HEADERS) {
    assert.ok(v1Meta.assets.config._headers.includes(`${k}: ${v}`), `canonical _headers missing ${k}`);
  }

  // 3. Deduplicated second publish (stage with publishToken, then manual POST .../activate with activateToken).
  const b2 = buildBundle({
    revisionId: 'rev-2',
    revisionSha: REV2_SHA,
    files: [['index.html', '<h1>Hearth v1</h1>'], ['styles.css', 'body{color:#241207}']],
  });
  assert.equal((await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${b2.manifestSha}`, {
    ...tok(creds.publishToken),
    'Content-Type': 'application/octet-stream',
  }, b2.bundle)).status, 201);
  assert.equal((await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', {
    ...tok(creds.publishToken),
    'Content-Type': 'application/json',
  }, JSON.stringify({ request_id: 'req-2', manifest: b2.manifest }))).status, 202);
  await runCron();
  const j2Id = jobIdOf('ws-a', 'site-a', 'req-2');
  const rSt2 = await request(STUDIO_PORT, 'GET', `/api/sites/ws-a/site-a/publish/${j2Id}`, tok(creds.publishToken));
  assert.equal(rSt2.json.phase, 'ready');
  assert.equal(rSt2.json.bytes_verified, true);
  assert.equal(rSt2.json.deployment, undefined);

  const uploadedBefore = cfFake.state.uploaded.length;
  const rAct2 = await request(STUDIO_PORT, 'POST', `/api/sites/ws-a/site-a/publish/${j2Id}/activate`, {
    ...tok(creds.activateToken),
    'Content-Type': 'application/json',
  }, '{}');
  assert.equal(rAct2.status, 200);
  assert.equal(rAct2.json.deployment.uploaded_files, 0, 'deduplicated second publish uploads 0 files');
  assert.equal(cfFake.state.uploaded.length, uploadedBefore);
  const v2Cf = rAct2.json.deployment.cf_version_id;

  // 4. Full #65 auth matrix on publish / activate / rollback.
  const actPath = `/api/sites/ws-a/site-a/publish/${j2Id}/activate`;
  const rbPath = `/api/sites/ws-a/site-a/deployments/${pv1}/rollback`;
  // edit token -> 403 insufficient_scope
  assert.equal((await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', { ...tok(creds.editToken), 'Content-Type': 'application/json' }, '{}')).status, 403);
  assert.equal((await request(STUDIO_PORT, 'POST', actPath, { ...tok(creds.editToken), 'Content-Type': 'application/json' }, '{}')).json?.error, 'insufficient_scope');
  assert.equal((await request(STUDIO_PORT, 'POST', rbPath, { ...tok(creds.editToken), 'Content-Type': 'application/json' }, '{}')).json?.error, 'insufficient_scope');
  // publish token -> 403 insufficient_scope on activate:true, POST .../activate, POST .../rollback
  const rPubAct = await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', { ...tok(creds.publishToken), 'Content-Type': 'application/json' }, JSON.stringify({ request_id: 'req-pub-act', manifest: b2.manifest, activate: true }));
  assert.equal(rPubAct.status, 403);
  assert.equal(rPubAct.json?.error, 'insufficient_scope');
  assert.equal((await request(STUDIO_PORT, 'POST', actPath, { ...tok(creds.publishToken), 'Content-Type': 'application/json' }, '{}')).json?.error, 'insufficient_scope');
  assert.equal((await request(STUDIO_PORT, 'POST', rbPath, { ...tok(creds.publishToken), 'Content-Type': 'application/json' }, '{}')).json?.error, 'insufficient_scope');
  // editor role -> 403 forbidden_role
  assert.equal((await request(STUDIO_PORT, 'POST', actPath, { ...cookie(creds.editorFresh), 'Content-Type': 'application/json' }, '{}')).json?.error, 'forbidden_role');
  assert.equal((await request(STUDIO_PORT, 'POST', rbPath, { ...cookie(creds.editorFresh), 'Content-Type': 'application/json' }, '{}')).json?.error, 'forbidden_role');
  // stale session (>15 min) -> 401 reauth_required
  const rStaleAct = await request(STUDIO_PORT, 'POST', actPath, { ...cookie(creds.ownerStale), 'Content-Type': 'application/json' }, '{}');
  assert.equal(rStaleAct.status, 401);
  assert.equal(rStaleAct.json?.error, 'reauth_required');
  const rStaleRb = await request(STUDIO_PORT, 'POST', rbPath, { ...cookie(creds.ownerStale), 'Content-Type': 'application/json' }, '{}');
  assert.equal(rStaleRb.status, 401);
  assert.equal(rStaleRb.json?.error, 'reauth_required');
  // cross-workspace -> 403 forbidden
  assert.equal((await request(STUDIO_PORT, 'POST', actPath, { ...tok(creds.otherWsToken), 'Content-Type': 'application/json' }, '{}')).status, 403);
  assert.equal((await request(STUDIO_PORT, 'POST', rbPath, { ...tok(creds.otherWsToken), 'Content-Type': 'application/json' }, '{}')).status, 403);
  // cookie CSRF without Origin -> 403 forbidden_origin
  assert.equal((await request(STUDIO_PORT, 'POST', rbPath, { ...cookie(creds.ownerFresh, false), 'Content-Type': 'application/json' }, '{}')).json?.error, 'forbidden_origin');

  // Revoked token between enqueue and cron run -> blocked at activation time.
  const b3 = buildBundle({ revisionId: 'rev-3', revisionSha: REV3_SHA, files: [['index.html', '<h1>v3</h1>']] });
  assert.equal((await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${b3.manifestSha}`, { ...tok(creds.revocableToken), 'Content-Type': 'application/octet-stream' }, b3.bundle)).status, 201);
  assert.equal((await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', { ...tok(creds.revocableToken), 'Content-Type': 'application/json' }, JSON.stringify({ request_id: 'req-revoked', manifest: b3.manifest, activate: true }))).status, 202);
  d1Exec(`UPDATE api_tokens SET revoked_at = '${isoAgo(1)}' WHERE id = '${creds.revocableToken.id}'`);
  await runCron();
  const rRevSt = await request(STUDIO_PORT, 'GET', `/api/sites/ws-a/site-a/publish/${jobIdOf('ws-a', 'site-a', 'req-revoked')}`, tok(creds.activateToken));
  assert.equal(rRevSt.json.phase, 'failed');
  assert.equal(rRevSt.json.error?.code, 'activation_unauthorized');

  // Demoted publisher between enqueue and cron run -> blocked at activation time.
  assert.equal((await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', { ...tok(creds.pubActivateToken), 'Content-Type': 'application/json' }, JSON.stringify({ request_id: 'req-demoted', manifest: b3.manifest, activate: true }))).status, 202);
  d1Exec(`UPDATE memberships SET role = 'editor' WHERE workspace_id = 'ws-a' AND user_id = 4`);
  await runCron();
  const rDemSt = await request(STUDIO_PORT, 'GET', `/api/sites/ws-a/site-a/publish/${jobIdOf('ws-a', 'site-a', 'req-demoted')}`, tok(creds.activateToken));
  assert.equal(rDemSt.json.phase, 'failed');
  assert.equal(rDemSt.json.error?.code, 'activation_unauthorized');
  assert.equal(cfFake.state.deployments[0].versions[0].version_id, v2Cf);

  // 5. Older revision job refused over newer high_water_seq.
  assert.equal((await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', { ...tok(creds.activateToken), 'Content-Type': 'application/json' }, JSON.stringify({ request_id: 'req-stale-rev', manifest: b1.manifest, activate: true }))).status, 202);
  await runCron();
  const rOld = await request(STUDIO_PORT, 'GET', `/api/sites/ws-a/site-a/publish/${jobIdOf('ws-a', 'site-a', 'req-stale-rev')}`, tok(creds.activateToken));
  assert.equal(rOld.json.phase, 'failed');
  assert.equal(rOld.json.error?.code, 'stale_revision');

  // 6. Pre-activation failure keeps live version.
  const b4 = buildBundle({ revisionId: 'rev-4', revisionSha: REV4_SHA, files: [['index.html', '<h1>v4</h1>']] });
  assert.equal((await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${b4.manifestSha}`, { ...tok(creds.activateToken), 'Content-Type': 'application/octet-stream' }, b4.bundle)).status, 201);
  assert.equal((await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', { ...tok(creds.activateToken), 'Content-Type': 'application/json' }, JSON.stringify({ request_id: 'req-4', manifest: b4.manifest }))).status, 202);
  await runCron();
  cfFake.pushFault('CreateVersion', { type: 'StatusBefore', status: 400 });
  const rFailPre = await request(STUDIO_PORT, 'POST', `/api/sites/ws-a/site-a/publish/${jobIdOf('ws-a', 'site-a', 'req-4')}/activate`, { ...tok(creds.activateToken), 'Content-Type': 'application/json' }, '{}');
  assert.equal(rFailPre.status, 502);
  assert.equal(rFailPre.json.activation, 'not_attempted');
  assert.equal(cfFake.state.deployments[0].versions[0].version_id, v2Cf);

  // 7. Ambiguous activation timeout reconciled cleanly.
  const b5 = buildBundle({ revisionId: 'rev-5', revisionSha: REV5_SHA, files: [['index.html', '<h1>v5</h1>']] });
  assert.equal((await request(STUDIO_PORT, 'PUT', `/api/sites/ws-a/site-a/publish/bundles/${b5.manifestSha}`, { ...tok(creds.activateToken), 'Content-Type': 'application/octet-stream' }, b5.bundle)).status, 201);
  assert.equal((await request(STUDIO_PORT, 'POST', '/api/sites/ws-a/site-a/publish', { ...tok(creds.activateToken), 'Content-Type': 'application/json' }, JSON.stringify({ request_id: 'req-5', manifest: b5.manifest }))).status, 202);
  await runCron();
  cfFake.pushFault('CreateDeployment', { type: 'DropAfter' });
  const rRecon = await request(STUDIO_PORT, 'POST', `/api/sites/ws-a/site-a/publish/${jobIdOf('ws-a', 'site-a', 'req-5')}/activate`, { ...tok(creds.activateToken), 'Content-Type': 'application/json' }, '{}');
  assert.equal(rRecon.status, 200);
  const v5Cf = rRecon.json.deployment.cf_version_id;
  assert.equal(cfFake.state.deployments[0].versions[0].version_id, v5Cf);

  // 8. Audited rollback works on matching schema_compat and fails closed on mismatch.
  const rRbOk = await request(STUDIO_PORT, 'POST', rbPath, { ...cookie(creds.ownerFresh), 'Content-Type': 'application/json' }, '{}');
  assert.equal(rRbOk.status, 200);
  assert.equal(rRbOk.json.rollback.cf_version_id, v1Cf);
  assert.equal(cfFake.state.deployments[0].versions[0].version_id, v1Cf);

  d1Exec(`UPDATE publish_targets SET site_schema_level = 5 WHERE id = 'target-a'`);
  const rRbIncompat = await request(STUDIO_PORT, 'POST', `/api/sites/ws-a/site-a/deployments/${rRecon.json.deployment.published_id}/rollback`, { ...cookie(creds.ownerFresh), 'Content-Type': 'application/json' }, '{}');
  assert.equal(rRbIncompat.status, 409);
  assert.equal(rRbIncompat.json.error, 'rollback_schema_incompatible');

  // 9. Secret hygiene: Cloudflare API token never in audit rows or wrangler logs.
  const auditOut = d1Exec(`SELECT action, result, error_code FROM publish_audit WHERE workspace_id = 'ws-a'`);
  assert.ok(!auditOut.includes(CF_TOKEN), 'CF token leaked in publish_audit');
  assert.ok(!serverLog.includes(CF_TOKEN), 'CF token leaked in wrangler log');

  console.log('test:studio:deploy OK (all Direct Upload, byte verification, #65 auth matrix, activation recheck, reconciliation, and rollback checks passed)');
}

main()
  .catch((e) => {
    console.error(e);
    process.exitCode = 1;
  })
  .finally(stopAll);
