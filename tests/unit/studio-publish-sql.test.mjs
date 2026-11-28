// SQL-level test for Studio publish jobs (issue #64): applies the real D1 migrations
// 0001 + 0002 + 0004 to an empty in-memory SQLite database (node:sqlite, no
// dependency) and runs the exact SQL that sws-worker-studio binds, printed by
// `cargo run --example dump_sql -- --publish`. Parameter order mirrors the builders in
// crates/sws-worker-studio/src/publish/sql.rs. Proves the migration applies on top of
// the existing ones, the row invariants hold, every conditional transition changes 0
// rows on a stale phase/attempt (duplicate delivery), the enqueue caps, the recovery
// scan, exhaustion and retention.
import { describe, it, before } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');
const MIG = resolve(ROOT, 'crates/sws-worker-studio/migrations');
const MIGRATIONS = ['0001_registry.sql', '0002_asset_store.sql', '0004_publish_jobs.sql'];
const BASE = Date.parse('2026-11-27T00:00:00Z') / 1000;
/** Canonical UTC text for BASE + s seconds. */
const T = (s = 0) => new Date((BASE + s) * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
const H1 = 'a'.repeat(64);
const H2 = 'b'.repeat(64);
const LEASE = 120; // job::LEASE_SECS
const MAX_ATTEMPTS = 5; // job::MAX_ATTEMPTS
const ACTIVE_CAP = 3; // sql::MAX_ACTIVE_JOBS_PER_SITE
const DAILY_CAP = 50; // sql::MAX_JOBS_PER_SITE_PER_DAY
const RETENTION = 30 * 86400; // sql::JOB_RETENTION_SECS

function loadSql() {
  const out = execFileSync('cargo', ['run', '-q', '--locked', '-p', 'sws-worker-studio', '--example', 'dump_sql', '--', '--publish'], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  return Object.fromEntries(JSON.parse(out).map((r) => [r.name, r]));
}

const sqlOf = (f) => readFileSync(resolve(MIG, f), 'utf8');

describe('studio publish jobs migration + SQL', () => {
  let Q;
  let db;
  const run = (name, ...p) => db.prepare(Q[name].sql).run(...p).changes;
  const get = (name, ...p) => db.prepare(Q[name].sql).get(...p);
  const all = (name, ...p) => db.prepare(Q[name].sql).all(...p);

  // --- builders (same parameter order as publish/sql.rs) ---
  const insertJob = (ws, site, id, rev, req, now = T(), user = 1) =>
    run('PUBLISH_JOB_INSERT', ws, site, id, rev, req, now, user, ACTIVE_CAP, T(-86400 + (Date.parse(now) / 1000 - BASE)), DAILY_CAP);
  const insertArtifact = (ws, id, digest = H1, revSha = H1, now = T()) =>
    run('PUBLISH_ARTIFACT_INSERT', ws, id, digest, revSha, 'sws-cli', '0.2.0', 1, 5, '{"files":[]}', now);
  const job = (ws, site, id) => get('PUBLISH_JOB_GET', ws, site, id);
  const claim = (ws, id, phase, attempt, now = 0) => run('PUBLISH_JOB_CLAIM', ws, id, phase, attempt, T(now), T(now + LEASE), MAX_ATTEMPTS);
  const advance = (ws, id, from, attempt, to, now = 0) =>
    run('PUBLISH_JOB_ADVANCE', ws, id, from, attempt, to, ['ready', 'failed'].includes(to) ? null : T(now + LEASE), T(now));
  const fail = (ws, id, from, attempt, code, last = null, now = 0) => run('PUBLISH_JOB_FAIL', ws, id, from, attempt, code, last, T(now));
  const retry = (ws, id, from, attempt, nextAt, last, now = 0) => run('PUBLISH_JOB_RETRY', ws, id, from, attempt, T(nextAt), last, T(now));
  const exhaust = (ws, id, phase, attempt, now = 0) => run('PUBLISH_JOB_EXHAUST', ws, id, phase, attempt, T(now), MAX_ATTEMPTS);

  before(() => {
    Q = loadSql();
  });

  function seed() {
    db = new DatabaseSync(':memory:');
    db.exec('PRAGMA foreign_keys = ON');
    for (const f of MIGRATIONS) db.exec(sqlOf(f));
    db.exec(`
      INSERT INTO workspaces (id, name) VALUES ('ws-a', 'A'), ('ws-b', 'B');
      INSERT INTO users (github_id, github_login) VALUES (1001, 'alice'), (2002, 'bob');
      INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a', 1, 'owner'), ('ws-b', 2, 'owner');
      INSERT INTO sites (workspace_id, id, slug, name) VALUES
        ('ws-a', 'site-1', 's1', 'S1'), ('ws-a', 'site-2', 's2', 'S2'), ('ws-b', 'site-b', 'sb', 'SB');
      INSERT INTO site_revisions (workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) VALUES
        ('ws-a', 'site-1', 'rev-1', 1, '${H1}', 10, 1), ('ws-a', 'site-2', 'rev-2', 1, '${H1}', 10, 1),
        ('ws-b', 'site-b', 'rev-b', 1, '${H1}', 10, 2);
    `);
  }

  /** A queued job with its artifact, as one enqueue batch writes it. */
  function enqueue(id = 'job-1', { ws = 'ws-a', site = 'site-1', rev = 'rev-1', req = id, now = T() } = {}) {
    const n = insertJob(ws, site, id, rev, req, now);
    insertArtifact(ws, id, H1, H1, now);
    return n;
  }

  it('applies on top of 0001 + 0002, is re-runnable, and every statement prepares', () => {
    seed();
    db.exec(sqlOf('0004_publish_jobs.sql'));
    for (const t of ['publish_jobs', 'publish_artifacts']) {
      assert.equal(db.prepare('SELECT strict FROM pragma_table_list WHERE name = ?').get(t).strict, 1, t);
    }
    for (const r of Object.values(Q)) assert.doesNotThrow(() => db.prepare(r.sql), r.name);
    // 0004 depends on 0001 only (0003 is reserved for #65 and may land in either order).
    const alone = new DatabaseSync(':memory:');
    alone.exec(sqlOf('0001_registry.sql'));
    assert.doesNotThrow(() => alone.exec(sqlOf('0004_publish_jobs.sql')));
  });

  it('enqueue writes a queued CLI job once; a retry with the same id changes nothing', () => {
    seed();
    assert.equal(enqueue('job-1'), 1);
    const j = job('ws-a', 'site-1', 'job-1');
    assert.deepEqual(
      [j.phase, j.origin, j.server_verified, j.attempt, j.next_attempt_at, j.lease_expires_at, j.manifest_sha256],
      ['queued', 'cli', 0, 0, T(), null, H1],
    );
    assert.equal(insertJob('ws-a', 'site-1', 'job-1', 'rev-1', 'job-1'), 0, 'same id: no second row');
    assert.equal(insertArtifact('ws-a', 'job-1', H2), 0, 'the artifact is never overwritten');
    assert.equal(job('ws-a', 'site-1', 'job-1').manifest_sha256, H1);
    assert.throws(() => db.prepare("UPDATE publish_artifacts SET total_bytes = 9").run(), /immutable/);
    // An artifact without its job is never written.
    assert.equal(insertArtifact('ws-a', 'no-such-job'), 0);
    // The same request id on the same site is a unique key, whatever the job id: the
    // enqueue statement writes nothing, and a raw insert fails loudly.
    assert.equal(insertJob('ws-a', 'site-1', 'job-x', 'rev-1', 'job-1'), 0);
    assert.equal(job('ws-a', 'site-1', 'job-x'), undefined);
    assert.throws(
      () =>
        db.exec(`INSERT INTO publish_jobs (workspace_id, site_id, id, revision_id, request_id, origin, next_attempt_at, created_by, created_at, updated_at)
                 VALUES ('ws-a', 'site-1', 'job-x', 'rev-1', 'job-1', 'cli', '${T()}', 1, '${T()}', '${T()}')`),
      /UNIQUE/,
    );
  });

  it('a job must point at an existing revision of the same site and workspace', () => {
    seed();
    assert.throws(() => insertJob('ws-a', 'site-1', 'j1', 'rev-missing', 'r1'), /FOREIGN KEY/);
    assert.throws(() => insertJob('ws-a', 'site-1', 'j2', 'rev-2', 'r2'), /FOREIGN KEY/, 'revision of another site');
    assert.throws(() => insertJob('ws-a', 'site-1', 'j3', 'rev-b', 'r3'), /FOREIGN KEY/, 'revision of another workspace');
  });

  it('row invariants: CLI jobs are never server-verified or compiling; failures carry code and phase', () => {
    seed();
    enqueue('job-1');
    const bad = [
      "UPDATE publish_jobs SET server_verified = 1",
      "UPDATE publish_jobs SET phase = 'compiling'",
      "UPDATE publish_jobs SET phase = 'failed'",
      "UPDATE publish_jobs SET phase = 'failed', error_code = 'x', finished_at = '2026-11-27T00:00:00Z'",
      "UPDATE publish_jobs SET phase = 'ready'",
      "UPDATE publish_jobs SET phase = 'ready', finished_at = '2026-11-27T00:00:00Z', lease_expires_at = '2026-11-27T00:00:00Z'",
      "UPDATE publish_jobs SET phase = 'succeeded'",
      "UPDATE publish_jobs SET origin = 'browser'",
      "UPDATE publish_jobs SET error_code = 'Bad Code'",
      "UPDATE publish_jobs SET next_attempt_at = '2026-02-30T00:00:00Z'",
    ];
    for (const sql of bad) assert.throws(() => db.prepare(sql).run(), /CHECK constraint/, sql);
    // A server job may be compiling (and may fail there).
    db.exec(`INSERT INTO publish_jobs (workspace_id, site_id, id, revision_id, request_id, origin, phase, next_attempt_at, created_by, created_at, updated_at)
             VALUES ('ws-a', 'site-1', 'srv-1', 'rev-1', 'srv-1', 'server', 'compiling', '${T()}', 1, '${T()}', '${T()}')`);
  });

  it('claim and every transition change 0 rows on a stale phase or attempt', () => {
    seed();
    enqueue('job-1');
    assert.equal(claim('ws-a', 'job-1', 'validating', 0), 0, 'stale phase');
    assert.equal(claim('ws-a', 'job-1', 'queued', 1), 0, 'stale attempt');
    assert.equal(claim('ws-b', 'job-1', 'queued', 0), 0, 'other workspace');
    assert.equal(claim('ws-a', 'job-1', 'queued', 0, -1), 0, 'not due yet');
    assert.equal(claim('ws-a', 'job-1', 'queued', 0), 1);
    let j = job('ws-a', 'site-1', 'job-1');
    assert.deepEqual([j.attempt, j.lease_expires_at], [1, T(LEASE)]);
    assert.equal(claim('ws-a', 'job-1', 'queued', 1, 10), 0, 'leased');
    // Transitions guarded by the owning attempt.
    assert.equal(advance('ws-a', 'job-1', 'queued', 0, 'validating'), 0, 'stale attempt');
    assert.equal(advance('ws-a', 'job-1', 'validating', 1, 'uploading'), 0, 'stale phase');
    assert.equal(fail('ws-a', 'job-1', 'validating', 1, 'x'), 0);
    assert.equal(retry('ws-a', 'job-1', 'validating', 1, 60, 'x'), 0);
    assert.equal(advance('ws-a', 'job-1', 'queued', 1, 'validating'), 1);
    assert.equal(advance('ws-a', 'job-1', 'validating', 1, 'uploading'), 1);
    assert.equal(advance('ws-a', 'job-1', 'uploading', 1, 'ready'), 1);
    j = job('ws-a', 'site-1', 'job-1');
    assert.deepEqual([j.phase, j.lease_expires_at, j.finished_at], ['ready', null, T()]);
    // Terminal: nothing matches any more, whatever is replayed.
    assert.equal(claim('ws-a', 'job-1', 'ready', 1, 10_000), 0);
    assert.equal(advance('ws-a', 'job-1', 'uploading', 1, 'ready'), 0);
    assert.equal(fail('ws-a', 'job-1', 'ready', 1, 'x'), 0);
    assert.equal(exhaust('ws-a', 'job-1', 'ready', 1, 10_000), 0);
  });

  it('duplicate delivery: two drivers with the same snapshot, only one owns the job', () => {
    seed();
    enqueue('job-1');
    const snap = job('ws-a', 'site-1', 'job-1');
    assert.equal(claim('ws-a', 'job-1', snap.phase, snap.attempt), 1, 'driver A');
    assert.equal(claim('ws-a', 'job-1', snap.phase, snap.attempt), 0, 'driver B, same pointer');
    // A finishes; B's replay of the whole run is a no-op.
    for (const [from, to] of [['queued', 'validating'], ['validating', 'uploading'], ['uploading', 'ready']]) {
      assert.equal(advance('ws-a', 'job-1', from, 1, to), 1);
    }
    const done = job('ws-a', 'site-1', 'job-1');
    assert.equal(claim('ws-a', 'job-1', snap.phase, snap.attempt), 0);
    for (const [from, to] of [['queued', 'validating'], ['validating', 'uploading'], ['uploading', 'ready']]) {
      assert.equal(advance('ws-a', 'job-1', from, 0, to), 0);
      assert.equal(advance('ws-a', 'job-1', from, 1, to), 0);
    }
    assert.deepEqual(job('ws-a', 'site-1', 'job-1'), done);
  });

  it('a crashed driver is recovered after its lease; its late writes are rejected', () => {
    seed();
    enqueue('job-1');
    assert.equal(claim('ws-a', 'job-1', 'queued', 0), 1);
    assert.equal(advance('ws-a', 'job-1', 'queued', 1, 'validating'), 1);
    assert.equal(advance('ws-a', 'job-1', 'validating', 1, 'uploading'), 1);
    // Crash. The recovery scan ignores it while the lease lives...
    assert.deepEqual(all('PUBLISH_JOBS_DUE', T(LEASE - 1), 10), []);
    // ...and returns it once the lease is over.
    assert.deepEqual(all('PUBLISH_JOBS_DUE', T(LEASE), 10).map((r) => r.id), ['job-1']);
    assert.equal(claim('ws-a', 'job-1', 'uploading', 1, LEASE), 1, 'recovery claims attempt 2');
    assert.equal(advance('ws-a', 'job-1', 'uploading', 1, 'ready', LEASE), 0, 'the dead driver cannot finish it');
    assert.equal(advance('ws-a', 'job-1', 'uploading', 2, 'ready', LEASE), 1);
    assert.equal(job('ws-a', 'site-1', 'job-1').attempt, 2);
  });

  it('retries back off, and the attempt cap ends in a phase-specific failure', () => {
    seed();
    enqueue('job-1');
    assert.equal(claim('ws-a', 'job-1', 'queued', 0), 1);
    assert.equal(advance('ws-a', 'job-1', 'queued', 1, 'validating'), 1);
    assert.equal(retry('ws-a', 'job-1', 'validating', 1, 30, 'd1_unavailable'), 1);
    let j = job('ws-a', 'site-1', 'job-1');
    assert.deepEqual([j.phase, j.next_attempt_at, j.lease_expires_at, j.last_error], ['validating', T(30), null, 'd1_unavailable']);
    assert.equal(claim('ws-a', 'job-1', 'validating', 1, 29), 0, 'backing off');
    assert.equal(claim('ws-a', 'job-1', 'validating', 1, 30), 1);
    // Final transient failure on the last attempt: retries_exhausted, phase kept, cause kept.
    db.prepare("UPDATE publish_jobs SET attempt = 5 WHERE id = 'job-1'").run();
    assert.equal(fail('ws-a', 'job-1', 'validating', 5, 'retries_exhausted', 'd1_unavailable', 40), 1);
    j = job('ws-a', 'site-1', 'job-1');
    assert.deepEqual([j.phase, j.error_code, j.error_phase, j.last_error, j.finished_at], ['failed', 'retries_exhausted', 'validating', 'd1_unavailable', T(40)]);
  });

  it('exhaust fails a job whose last attempt crashed, and never claims past the cap', () => {
    seed();
    enqueue('job-1');
    db.prepare(`UPDATE publish_jobs SET phase = 'uploading', attempt = 5, lease_expires_at = '${T(LEASE)}' WHERE id = 'job-1'`).run();
    assert.equal(claim('ws-a', 'job-1', 'uploading', 5, LEASE), 0, 'the cap holds at write time');
    assert.equal(exhaust('ws-a', 'job-1', 'uploading', 5, LEASE - 1), 0, 'lease still live');
    assert.equal(exhaust('ws-a', 'job-1', 'uploading', 4, LEASE), 0, 'stale attempt');
    assert.equal(exhaust('ws-a', 'job-1', 'uploading', 5, LEASE), 1);
    const j = job('ws-a', 'site-1', 'job-1');
    assert.deepEqual([j.phase, j.error_code, j.error_phase, j.last_error], ['failed', 'retries_exhausted', 'uploading', 'lease_expired']);
    // Below the cap, exhaust does not apply.
    enqueue('job-2');
    assert.equal(exhaust('ws-a', 'job-2', 'queued', 0), 0);
  });

  it('enqueue caps: unfinished jobs per site and jobs per rolling day', () => {
    seed();
    for (let i = 0; i < ACTIVE_CAP; i++) assert.equal(enqueue(`a${i}`), 1);
    assert.equal(enqueue('a-over'), 0, 'the fourth unfinished job is refused');
    assert.equal(job('ws-a', 'site-1', 'a-over'), undefined, 'and leaves no row (nor artifact)');
    assert.equal(db.prepare("SELECT COUNT(*) AS n FROM publish_artifacts WHERE job_id = 'a-over'").get().n, 0);
    assert.equal(enqueue('other-site', { site: 'site-2', rev: 'rev-2' }), 1, 'per site');
    // Finished jobs free a slot.
    db.prepare(`UPDATE publish_jobs SET phase = 'ready', finished_at = '${T()}' WHERE id = 'a0'`).run();
    assert.equal(enqueue('a3'), 1);
    // Daily cap: count jobs created in the last 24 h.
    seed();
    const stmt = db.prepare(`INSERT INTO publish_jobs (workspace_id, site_id, id, revision_id, request_id, origin, phase, next_attempt_at, created_by, created_at, updated_at, finished_at)
                             VALUES ('ws-a', 'site-1', ?, 'rev-1', ?, 'cli', 'ready', ?, 1, ?, ?, ?)`);
    for (let i = 0; i < DAILY_CAP; i++) stmt.run(`d${i}`, `d${i}`, T(-3600), T(-3600), T(-3600), T(-3600));
    assert.equal(enqueue('d-over'), 0, 'the 51st job in a day is refused');
    assert.equal(enqueue('d-tomorrow', { now: T(86400 - 3600 + 1) }), 1, 'a day later it is accepted');
  });

  it('the recovery scan returns due unleased unfinished jobs only, oldest first, via its index', () => {
    seed();
    enqueue('q-late', { now: T(10) });
    enqueue('q-early', { now: T(5) });
    enqueue('leased', { site: 'site-2', rev: 'rev-2' });
    claim('ws-a', 'leased', 'queued', 0);
    enqueue('b-done', { ws: 'ws-b', site: 'site-b', rev: 'rev-b' });
    db.prepare(`UPDATE publish_jobs SET phase = 'failed', error_code = 'x', error_phase = 'queued', finished_at = '${T()}' WHERE id = 'b-done'`).run();
    assert.deepEqual(all('PUBLISH_JOBS_DUE', T(20), 10).map((r) => r.id), ['q-early', 'q-late']);
    assert.deepEqual(all('PUBLISH_JOBS_DUE', T(7), 10).map((r) => r.id), ['q-early'], 'not due yet');
    assert.deepEqual(all('PUBLISH_JOBS_DUE', T(20), 1).map((r) => r.id), ['q-early'], 'capped per run');
    assert.deepEqual(Object.keys(all('PUBLISH_JOBS_DUE', T(20), 1)[0]).sort(), ['id', 'site_id', 'workspace_id'], 'ids only');
    const plan = db.prepare(`EXPLAIN QUERY PLAN ${Q.PUBLISH_JOBS_DUE.sql}`).all(T(20), 10).map((r) => r.detail).join(' ');
    assert.match(plan, /idx_publish_jobs_due/);
  });

  it('retention prunes old finished jobs (with their artifacts) but keeps each site\'s newest ready job', () => {
    seed();
    const old = -RETENTION - 100;
    const mk = (id, phase, finished, site = 'site-1', rev = 'rev-1') => {
      enqueue(id, { site, rev, now: T(finished - 10) });
      const err = phase === 'failed' ? ", error_code = 'x', error_phase = 'validating'" : '';
      db.prepare(`UPDATE publish_jobs SET phase = '${phase}', finished_at = '${T(finished)}'${err} WHERE id = '${id}'`).run();
    };
    mk('old-ready-1', 'ready', old - 50);
    mk('old-ready-2', 'ready', old);
    mk('old-failed', 'failed', old);
    mk('new-failed', 'failed', 0);
    mk('old-ready-s2', 'ready', old, 'site-2', 'rev-2');
    enqueue('old-queued', { now: T(old) });
    const n = db.prepare(Q.PUBLISH_JOBS_PRUNE.sql).run(T(-RETENTION), 50).changes;
    assert.equal(n, 2);
    const left = db.prepare('SELECT id FROM publish_jobs ORDER BY id').all().map((r) => r.id);
    assert.deepEqual(left, ['new-failed', 'old-queued', 'old-ready-2', 'old-ready-s2']);
    assert.equal(db.prepare("SELECT COUNT(*) AS n FROM publish_artifacts WHERE job_id IN ('old-ready-1', 'old-failed')").get().n, 0, 'artifacts cascade');
    // Batch-limited.
    mk('x1', 'failed', old);
    mk('x2', 'failed', old);
    assert.equal(db.prepare(Q.PUBLISH_JOBS_PRUNE.sql).run(T(-RETENTION), 1).changes, 1);
  });

  it('is tenant-scoped, and deleting a site removes its jobs and artifacts', () => {
    seed();
    enqueue('job-1');
    assert.equal(job('ws-b', 'site-1', 'job-1'), undefined);
    assert.equal(job('ws-a', 'site-2', 'job-1'), undefined);
    assert.equal(get('PUBLISH_ARTIFACT_GET', 'ws-b', 'job-1'), undefined);
    assert.equal(get('PUBLISH_ARTIFACT_GET', 'ws-a', 'job-1').manifest_sha256, H1);
    for (const r of Object.values(Q).filter((x) => x.tenant)) assert.match(r.sql, /workspace_id = \?1/, r.name);
    db.prepare("DELETE FROM sites WHERE workspace_id = 'ws-a' AND id = 'site-1'").run();
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM publish_jobs').get().n, 0);
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM publish_artifacts').get().n, 0);
  });
});
