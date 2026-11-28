#!/usr/bin/env node
// Local integration test for `solo studio` native CLI (#68) on ports 8804-8805.
// Server-dependent: NOT part of `npm run verify`; CI runs it in the `web` job
// (`npm run test:studio:cli`). Lock: /tmp/solo-studio-8804.lock.
//
// Builds the Studio release Worker (`npm run studio:build`) and the native
// `solo` binary, seeds local D1 with real #65 scoped tokens, starts `wrangler dev`
// on 127.0.0.1:8804 (inspector 8805) in its own process group, and exercises:
//   1. `solo studio enable-r2` (offline output, quotas, wrangler commands, config diff)
//   2. `solo studio login` (mode 0600 config outside repo)
//   3. Token scope enforcement (`edit` token rejected on `seed`, `publish` token accepted)
//   4. `pull -> solo edit apply -> push -> pull` byte-identical round-trip
//   5. Structured `409` conflict reporting on stale `base_seq`
//   6. Cookie `POST /sync` CSRF `Origin` gate (`403 forbidden_origin`)
//   7. `solo studio publish` + `solo studio status` (`origin: cli`, `server_verified: false`,
//      distinct compile/upload/activation stages)

import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)));
const PORT = 8804;
const INSPECTOR_PORT = 8805;
const BASE_URL = `http://127.0.0.1:${PORT}`;
const STATE = join(ROOT, '.wrangler', 'studio-cli-state');
const WRANGLER = join(ROOT, 'node_modules', '.bin', 'wrangler');
const FIXTURE_PATH = join(ROOT, 'crates/sws-worker-studio/tests/fixtures/session-site.json');
const SHA = (s) => createHash('sha256').update(s).digest('hex');
const hex = (n) => randomBytes(n).toString('hex');
const isoAgo = (secs) => new Date(Date.now() - secs * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function runSync(cmd, args, opts = {}) {
  const res = spawnSync(cmd, args, {
    cwd: ROOT,
    encoding: 'utf8',
    ...opts,
  });
  return res;
}

function mustRun(cmd, args, opts = {}) {
  const res = runSync(cmd, args, opts);
  if (res.status !== 0) {
    throw new Error(
      `Command failed (${res.status}): ${cmd} ${args.join(' ')}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`,
    );
  }
  return res;
}

function portOpen(port) {
  return new Promise((res) => {
    const s = createConnection({ host: '127.0.0.1', port });
    s.once('connect', () => {
      s.destroy();
      res(true);
    });
    s.once('error', () => res(false));
  });
}

function mintToken() {
  const id = hex(12);
  const value = `swst_${id}_${hex(32)}`;
  return { id, value, hash: SHA(value) };
}

function mintSession() {
  const id = hex(12);
  const value = `${id}.${hex(32)}`;
  return { id, value, hash: SHA(value) };
}

async function main() {
  assert.equal(await portOpen(PORT), false, `port ${PORT} is already in use`);
  assert.equal(await portOpen(INSPECTOR_PORT), false, `inspector port ${INSPECTOR_PORT} is already in use`);

  // 1. Build Studio Worker release artifact and native `solo` binary.
  mustRun('npm', ['run', 'studio:build']);
  mustRun('cargo', ['build', '-p', 'sws-cli', '--features', 'pack-hospitality', '--locked']);
  const soloBin = join(ROOT, 'target', 'debug', process.platform === 'win32' ? 'solo.exe' : 'solo');

  // 2. Prepare clean local D1 state and seed users, workspace, site, and #65 credentials.
  rmSync(STATE, { recursive: true, force: true });
  mkdirSync(STATE, { recursive: true });
  const cfgPath = join(STATE, 'wrangler.jsonc');
  writeFileSync(
    cfgPath,
    JSON.stringify({
      name: 'sws-studio-cli-test',
      main: join(ROOT, 'crates/sws-worker-studio/build/index.js'),
      compatibility_date: '2026-11-01',
      workers_dev: false,
      d1_databases: [
        {
          binding: 'STUDIO_DB',
          database_name: 'sws-studio-cli-db',
          database_id: '00000000-0000-0000-0000-000000000068',
          migrations_dir: join(ROOT, 'crates/sws-worker-studio/migrations'),
        },
      ],
      durable_objects: {
        bindings: [{ name: 'SITE_SESSION', class_name: 'SiteSession' }],
      },
      migrations: [{ tag: 'v1-site-session', new_sqlite_classes: ['SiteSession'] }],
      vars: {
        STUDIO_VERSION: '0.2.0-cli-test',
        STUDIO_PUBLIC_ORIGIN: BASE_URL,
        STUDIO_BOOTSTRAP_WORKSPACE: 'ws-a',
      },
    }),
  );

  mustRun(WRANGLER, ['d1', 'migrations', 'apply', 'STUDIO_DB', '--local', '-c', cfgPath, '--persist-to', STATE]);

  const editToken = mintToken();
  const publishToken = mintToken();
  const ownerSession = mintSession();
  const created = isoAgo(60);
  const expires = isoAgo(-86_400);
  const seedSql = `
    INSERT INTO users (id, github_id, github_login) VALUES (1, 1001, 'alice'), (4, 1004, 'dave');
    INSERT INTO workspaces (id, name) VALUES ('ws-a', 'Workspace A');
    INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a', 1, 'owner'), ('ws-a', 4, 'publisher');
    INSERT INTO sites (id, workspace_id, slug, name) VALUES ('site-1', 'ws-a', 'site-1', 'Site One');
    INSERT INTO api_tokens (id, workspace_id, user_id, token_hash, scope, label, created_at, expires_at, chain_started_at)
      VALUES
        ('${editToken.id}', 'ws-a', 4, '${editToken.hash}', 'edit', 'cli-edit', '${created}', '${expires}', '${created}'),
        ('${publishToken.id}', 'ws-a', 4, '${publishToken.hash}', 'publish', 'cli-pub', '${created}', '${expires}', '${created}');
    INSERT INTO auth_sessions (id, user_id, secret_hash, created_at, last_seen_at, expires_at)
      VALUES ('${ownerSession.id}', 1, '${ownerSession.hash}', '${created}', '${created}', '${expires}');
  `;
  const sqlFile = join(STATE, 'seed.sql');
  writeFileSync(sqlFile, seedSql);
  mustRun(WRANGLER, ['d1', 'execute', 'STUDIO_DB', '--local', '-c', cfgPath, '--persist-to', STATE, '--file', sqlFile]);

  const workDir = mkdtempSync(join(tmpdir(), 'solo-studio-cli-'));
  let server = null;
  let serverLog = '';
  try {
    // 3. Test offline `solo studio enable-r2`.
    const r2Out = mustRun(soloBin, ['studio', 'enable-r2']);
    assert.match(r2Out.stdout, /MEDIA_BUCKET/);
    assert.match(r2Out.stdout, /STUDIO_ASSET_BACKEND/);
    assert.match(r2Out.stdout, /wrangler r2 bucket create/);

    // 4. Test `solo studio login` (mode 0600 outside repo).
    const studioCfgFile = join(workDir, 'studio-config.json');
    mustRun(soloBin, ['studio', 'login', '--url', BASE_URL, '--token-stdin'], {
      input: `${editToken.value}\n`,
      env: { ...process.env, SOLO_STUDIO_CONFIG: studioCfgFile },
    });
    const mode = statSync(studioCfgFile).mode & 0o777;
    assert.equal(mode, 0o600, `expected 0600 config mode, got ${mode.toString(8)}`);

    // 5. Start local `wrangler dev` in its own process group.
    server = spawn(
      WRANGLER,
      [
        'dev',
        '-c',
        cfgPath,
        '--persist-to',
        STATE,
        '--port',
        String(PORT),
        '--inspector-port',
        String(INSPECTOR_PORT),
        '--ip',
        '127.0.0.1',
        '--test-scheduled',
      ],
      { cwd: ROOT, detached: true, stdio: ['ignore', 'pipe', 'pipe'] },
    );
    server.stdout.on('data', (d) => (serverLog += d));
    server.stderr.on('data', (d) => (serverLog += d));

    const deadline = Date.now() + 120_000;
    let ready = false;
    while (Date.now() < deadline) {
      try {
        const r = await fetch(`${BASE_URL}/api/health`, { signal: AbortSignal.timeout(2000) });
        if (r.ok) {
          ready = true;
          break;
        }
      } catch {
        // retry
      }
      await sleep(500);
    }
    if (!ready) {
      throw new Error(`wrangler dev failed to become ready on ${PORT}:\n${serverLog}`);
    }

    const rawInitDir = join(workDir, 'init-proj');
    const seedFixture = join(workDir, 'seed.ir.json');
    mustRun(soloBin, ['init', rawInitDir]);
    mustRun(soloBin, ['edit', 'upgrade', join(rawInitDir, 'site.ir.json'), '--out', seedFixture]);

    // 6. Token scope rejection: edit-only token cannot seed; publish token can seed.
    const seedFail = runSync(
      soloBin,
      ['studio', 'push', '--site', 'ws-a/site-1', '--seed', seedFixture],
      { env: { ...process.env, SOLO_STUDIO_CONFIG: studioCfgFile } },
    );
    assert.notEqual(seedFail.status, 0, 'edit token must be rejected for seed');
    assert.match(seedFail.stderr, /403.*insufficient_scope/);

    const seedOk = mustRun(
      soloBin,
      ['studio', 'push', '--site', 'ws-a/site-1', '--seed', seedFixture],
      {
        env: {
          ...process.env,
          SOLO_STUDIO_CONFIG: studioCfgFile,
          SOLO_STUDIO_TOKEN: publishToken.value,
        },
      },
    );
    assert.match(seedOk.stdout, /seeded head_seq=0/);

    // 7. Cookie POST /sync CSRF Origin check.
    const csrfResp = await fetch(`${BASE_URL}/api/sites/ws-a/site-1/sync`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Cookie: `__Host-sws_session=${ownerSession.value}`,
        Origin: 'https://evil.example',
      },
      body: JSON.stringify({ v: 1, type: 'checkpoint', reason: 'save' }),
    });
    assert.equal(csrfResp.status, 403);
    const csrfJson = await csrfResp.json();
    assert.equal(csrfJson.error, 'forbidden_origin');

    // 8. `pull -> solo edit -> push -> pull` byte-identical round-trip.
    const pulled1 = join(workDir, 'pulled1.ir.json');
    const edited = join(workDir, 'edited.ir.json');
    const pulled2 = join(workDir, 'pulled2.ir.json');
    const batch1 = join(workDir, 'batch1.json');
    const tx1 = join(workDir, 'tx1.json');

    mustRun(
      soloBin,
      ['studio', 'pull', '--site', 'ws-a/site-1', '--out', pulled1],
      { env: { ...process.env, SOLO_STUDIO_CONFIG: studioCfgFile } },
    );

    const doc1 = JSON.parse(readFileSync(pulled1, 'utf8'));
    const findTextUid = (nodes) => {
      for (const n of nodes || []) {
        if (n.type === 'text' && n.uid) return n.uid;
        const inner = findTextUid(n.children);
        if (inner) return inner;
      }
      return null;
    };
    const h1Uid = findTextUid(doc1.pages[0].body);
    assert.ok(h1Uid, 'expected uid on pulled fixture node');

    writeFileSync(
      batch1,
      JSON.stringify({
        version: 1,
        commands: [{ op: 'set_text', uid: h1Uid, value: 'CLI Synced Title' }],
      }),
    );
    mustRun(soloBin, [
      'edit',
      'apply',
      pulled1,
      batch1,
      '--out',
      edited,
      '--transaction',
      tx1,
    ]);

    const pushOk = mustRun(
      soloBin,
      ['studio', 'push', '--site', 'ws-a/site-1', '--transaction', tx1, '--base-seq', '0'],
      { env: { ...process.env, SOLO_STUDIO_CONFIG: studioCfgFile } },
    );
    assert.match(pushOk.stdout, /ack seq=1 head_seq=1/);

    mustRun(
      soloBin,
      ['studio', 'pull', '--site', 'ws-a/site-1', '--out', pulled2],
      { env: { ...process.env, SOLO_STUDIO_CONFIG: studioCfgFile } },
    );
    assert.equal(
      readFileSync(pulled2, 'utf8'),
      readFileSync(edited, 'utf8'),
      'pull -> solo edit -> push -> pull must be byte-identical',
    );

    // 9. Conflict reporting (409) on stale base_seq touching the same node.
    const batchConflict = join(workDir, 'batch-conflict.json');
    writeFileSync(
      batchConflict,
      JSON.stringify({
        version: 1,
        commands: [{ op: 'set_text', uid: h1Uid, value: 'Conflicting Title' }],
      }),
    );
    const conflictRes = runSync(
      soloBin,
      ['studio', 'push', '--site', 'ws-a/site-1', '--batch', batchConflict, '--base-seq', '0'],
      { env: { ...process.env, SOLO_STUDIO_CONFIG: studioCfgFile } },
    );
    assert.notEqual(conflictRes.status, 0, 'stale conflicting push must fail');
    assert.match(conflictRes.stderr, /409 conflict/);

    // 10. `solo studio publish` + `solo studio status` (origin: cli, not server-verified).
    const pubInvalid = runSync(
      soloBin,
      ['studio', 'publish', '--site', 'ws-a/site-1', '--input', FIXTURE_PATH],
      {
        env: {
          ...process.env,
          SOLO_STUDIO_CONFIG: studioCfgFile,
          SOLO_STUDIO_TOKEN: publishToken.value,
        },
      },
    );
    assert.notEqual(pubInvalid.status, 0, 'inaccessible fixture must fail local verification');
    assert.match(pubInvalid.stderr, /local verification failed/);

    const pubFail = runSync(
      soloBin,
      ['studio', 'publish', '--site', 'ws-a/site-1', '--input', pulled2],
      { env: { ...process.env, SOLO_STUDIO_CONFIG: studioCfgFile } },
    );
    assert.notEqual(pubFail.status, 0, 'edit-only token must be rejected on publish');

    const pubOk = mustRun(
      soloBin,
      ['studio', 'publish', '--site', 'ws-a/site-1', '--input', pulled2, '--bundle', '--json'],
      {
        env: {
          ...process.env,
          SOLO_STUDIO_CONFIG: studioCfgFile,
          SOLO_STUDIO_TOKEN: publishToken.value,
        },
      },
    );
    const pubJson = JSON.parse(pubOk.stdout);
    assert.equal(pubJson.origin, 'cli');
    assert.equal(pubJson.server_verified, false);
    assert.equal(pubJson.stages.compile.status, 'ok');
    assert.equal(pubJson.stages.upload.status, 'enqueued');
    const jobId = pubJson.job.job_id;
    assert.ok(jobId, 'expected job_id from publish');

    const statusOut = mustRun(
      soloBin,
      ['studio', 'status', '--site', 'ws-a/site-1', '--job', jobId],
      {
        env: {
          ...process.env,
          SOLO_STUDIO_CONFIG: studioCfgFile,
          SOLO_STUDIO_TOKEN: publishToken.value,
        },
      },
    );
    assert.match(statusOut.stdout, /origin=cli \(locally compiled; not server-verified\)/);
    assert.match(statusOut.stdout, /server_verified=false/);

    console.log('test-studio-cli: all assertions passed');
  } finally {
    if (server?.pid) {
      try {
        process.kill(-server.pid, 'SIGTERM');
      } catch {
        // already exited
      }
    }
    rmSync(workDir, { recursive: true, force: true });
    rmSync(STATE, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
