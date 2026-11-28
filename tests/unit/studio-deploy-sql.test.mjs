// SQL-level test for Studio publish targets (#66): applies the real D1 migrations
// (0001 registry, 0002 asset store, 0005 publish targets; 0003/0004 belong to #65/#64)
// to an in-memory SQLite database (node:sqlite, no dependency) and runs the exact
// statements sws-worker-studio binds, printed by `cargo run --example dump_sql -- --publish`.
// Proves the activation lease serializes per target, the revision order refuses older
// jobs, a lost lease records nothing, rollback can only point at a retained version of
// the same target, and retained/audit rows are immutable and tenant-scoped.
import { describe, it, before, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');
const MIGRATIONS = ['0001_registry.sql', '0002_asset_store.sql', '0003_auth.sql', '0004_publish_jobs.sql', '0005_publish_targets.sql'].map((f) =>
  resolve(ROOT, 'crates/sws-worker-studio/migrations', f),
);
const ACCOUNT = '0123456789abcdef0123456789abcdef';
const BINDINGS = '[{"type":"d1","name":"DB","id":"00000000-0000-0000-0000-000000000001"}]';
const V = (n) => `${String(n).padStart(8, '0')}-0000-4000-8000-000000000000`;
const H = 'c'.repeat(64);
const T0 = '2026-11-27T00:00:00Z';
const T1 = '2026-11-27T00:01:00Z';
const T2 = '2026-11-27T00:02:00Z';
const T3 = '2026-11-27T00:03:00Z';

function loadSql() {
  const out = execFileSync('cargo', ['run', '-q', '--locked', '-p', 'sws-worker-studio', '--example', 'dump_sql', '--', '--deploy'], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  return Object.fromEntries(JSON.parse(out).map((r) => [r.name, r]));
}

describe('studio publish targets migration (0005)', () => {
  let Q;
  let db;
  const run = (name, ...p) => db.prepare(Q[name].sql).run(...p);
  const get = (name, ...p) => db.prepare(Q[name].sql).get(...p);
  const target = (ws = 'ws-a', site = 'site-a') => get('TARGET_GET', ws, site);

  before(() => {
    Q = loadSql();
  });

  beforeEach(() => {
    db = new DatabaseSync(':memory:');
    db.exec('PRAGMA foreign_keys = ON');
    for (const m of MIGRATIONS) db.exec(readFileSync(m, 'utf8'));
    db.exec(`
      INSERT INTO workspaces (id, name) VALUES ('ws-a', 'A'), ('ws-b', 'B');
      INSERT INTO users (github_id, github_login) VALUES (1001, 'alice'), (2002, 'bob');
      INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a', 1, 'owner'), ('ws-b', 2, 'owner');
      INSERT INTO sites (workspace_id, id, slug, name) VALUES ('ws-a', 'site-a', 'hearth', 'Hearth'), ('ws-b', 'site-b', 'other', 'Other');
    `);
    for (let r = 1; r <= 6; r++) {
      db.prepare('INSERT INTO site_revisions (workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) VALUES (?, ?, ?, ?, ?, 1, 1)').run(
        'ws-a',
        'site-a',
        `rev-${r}`,
        r,
        H,
      );
    }
    db.prepare('INSERT INTO site_revisions (workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) VALUES (?, ?, ?, ?, ?, 1, 2)').run(
      'ws-b',
      'site-b',
      'rev-b1',
      1,
      H,
    );
    run('TARGET_INSERT', 'ws-a', 'site-a', 'target-a', ACCOUNT, 'hearth-site', 'CLOUDFLARE_API_TOKEN', BINDINGS, '2026-11-01', 4, V(0));
    run('TARGET_INSERT', 'ws-b', 'site-b', 'target-b', ACCOUNT, 'other-site', 'CF_PUBLISH_TOKEN_OTHER', BINDINGS, '2026-11-01', 1, null);
  });

  const acquire = (job, seq, version, until = T2, now = T0) => run('TARGET_ACQUIRE', 'ws-a', 'target-a', job, seq, version, until, now).changes;
  const retained = (id, version, rev, seq, ws = 'ws-a', site = 'site-a', tgt = 'target-a') =>
    run('VERSION_INSERT_IF_LIVE', ws, site, id, tgt, version, rev, seq, `job-${seq}`, 4, H, T1, 1).changes;

  it('applies from empty, re-applies 0005 idempotently, and every statement prepares', () => {
    db.exec(readFileSync(MIGRATIONS[4], 'utf8'));
    for (const [name, { sql }] of Object.entries(Q)) assert.doesNotThrow(() => db.prepare(sql), name);
  });

  it('stores and retrieves chunked bundles and rejects oversized chunks', () => {
    run('BUNDLE_INSERT', 'ws-a', 'site-a', H, 1024, 1, 1, T0);
    run('BUNDLE_CHUNK_INSERT', 'ws-a', 'site-a', H, 0, Buffer.alloc(1024, 0x41));
    assert.equal(get('BUNDLE_GET', 'ws-a', 'site-a', H).chunk_count, 1);
    assert.equal(db.prepare(Q.BUNDLE_CHUNKS_GET.sql).all('ws-a', 'site-a', H).length, 1);
    assert.throws(
      () => run('BUNDLE_CHUNK_INSERT', 'ws-a', 'site-a', H, 1, Buffer.alloc(524289, 0x42)),
      /CHECK/,
    );
  });

  it('CHECKs refuse bad targets, secret values and half-written leases', () => {
    const ins = (...over) => () =>
      run('TARGET_INSERT', 'ws-a', 'site-a', 'target-x', ...over);
    db.exec("DELETE FROM publish_targets WHERE id = 'target-a'");
    assert.throws(ins('NOT-HEX', 'hearth-site', 'CLOUDFLARE_API_TOKEN', BINDINGS, '2026-11-01', 4, null), /CHECK/);
    assert.throws(ins(ACCOUNT, 'Hearth Site', 'CLOUDFLARE_API_TOKEN', BINDINGS, '2026-11-01', 4, null), /CHECK/);
    // token_secret is a secret NAME from a fixed grammar, never a token value.
    assert.throws(ins(ACCOUNT, 'hearth-site', 'cfat_abcdefabcdefabcdef', BINDINGS, '2026-11-01', 4, null), /CHECK/);
    assert.throws(ins(ACCOUNT, 'hearth-site', 'CLOUDFLARE_API_TOKEN', '{"type":"d1"}', '2026-11-01', 4, null), /CHECK/);
    assert.throws(ins(ACCOUNT, 'hearth-site', 'CLOUDFLARE_API_TOKEN', BINDINGS, '2026-02-30', 4, null), /CHECK/);
    assert.throws(ins(ACCOUNT, 'hearth-site', 'CLOUDFLARE_API_TOKEN', BINDINGS, '2026-11-01', 4, 'not-a-uuid'), /CHECK/);
    ins(ACCOUNT, 'hearth-site', 'CLOUDFLARE_API_TOKEN', BINDINGS, '2026-11-01', 4, null)();
    assert.throws(() => db.exec("UPDATE publish_targets SET activating_job = 'j' WHERE id = 'target-x'"), /CHECK/);
    // One target per site.
    assert.throws(ins(ACCOUNT, 'hearth-site', 'CLOUDFLARE_API_TOKEN', BINDINGS, '2026-11-01', 4, null), /UNIQUE|PRIMARY/);
  });

  it('the lease serializes activations: one holder, renew only while held and unexpired', () => {
    assert.equal(acquire('job-1', 1, V(1)), 1);
    assert.equal(acquire('job-2', 2, V(2)), 0, 'held');
    assert.equal(acquire('rb-1', null, V(9)), 0, 'rollback also waits');
    assert.equal(run('TARGET_RENEW', 'ws-a', 'target-a', 'job-2', T3, T1).changes, 0, 'not the holder');
    assert.equal(run('TARGET_RENEW', 'ws-a', 'target-a', 'job-1', T3, T1).changes, 1);
    assert.equal(run('TARGET_RENEW', 'ws-a', 'target-a', 'job-1', T3, T3).changes, 0, 'expired leases cannot be renewed');
    assert.equal(run('TARGET_CLEAR_STALE', 'ws-a', 'target-a', 'job-1', T2).changes, 0, 'not yet expired');
    assert.equal(run('TARGET_CLEAR_STALE', 'ws-a', 'target-a', 'job-1', T3).changes, 1);
    assert.equal(target().activating_job, null);
    assert.equal(acquire('job-2', 2, V(2)), 1);
    assert.equal(run('TARGET_RELEASE', 'ws-a', 'target-a', 'job-x').changes, 0, 'only the holder releases');
    assert.equal(run('TARGET_RELEASE', 'ws-a', 'target-a', 'job-2').changes, 1);
  });

  it('adopting an expired holder that is live raises the high-water mark (never lowers it)', () => {
    assert.equal(acquire('job-5', 5, V(5)), 1);
    assert.equal(run('TARGET_ADOPT_STALE', 'ws-a', 'target-a', 'job-5', T3).changes, 1);
    let t = target();
    assert.deepEqual([t.live_version_id, t.high_water_seq, t.activating_job, t.live_published_id], [V(5), 5, null, null]);
    assert.equal(acquire('rb-1', null, V(1)), 1, 'rollback holder has no seq');
    assert.equal(run('TARGET_ADOPT_STALE', 'ws-a', 'target-a', 'rb-1', T3).changes, 1);
    t = target();
    assert.deepEqual([t.live_version_id, t.high_water_seq], [V(1), 5], 'a rollback holder keeps the mark');
  });

  it('an older (or equal) revision can never take the lease after a newer one activated', () => {
    assert.equal(acquire('job-5', 5, V(5)), 1);
    assert.equal(run('TARGET_RECORD_PUBLISH', 'ws-a', 'target-a', 'job-5', V(5), 'pv-5', 'rev-5', 5, T1).changes, 1);
    assert.equal(retained('pv-5', V(5), 'rev-5', 5), 1);
    assert.equal(target().high_water_seq, 5);
    assert.equal(acquire('job-3', 3, V(3)), 0, 'older');
    assert.equal(acquire('job-5b', 5, V(6)), 0, 'equal');
    assert.equal(acquire('job-6', 6, V(6)), 1, 'newer');
    // Even a holder cannot record an older seq.
    assert.equal(run('TARGET_RECORD_PUBLISH', 'ws-a', 'target-a', 'job-6', V(4), 'pv-4', 'rev-4', 4, T1).changes, 0);
  });

  it('a lost lease records nothing: no live change and no retained row', () => {
    assert.equal(acquire('job-2', 2, V(2)), 1);
    db.exec("UPDATE publish_targets SET activating_job = 'thief' WHERE id = 'target-a'");
    assert.equal(run('TARGET_RECORD_PUBLISH', 'ws-a', 'target-a', 'job-2', V(2), 'pv-2', 'rev-2', 2, T1).changes, 0);
    assert.equal(retained('pv-2', V(2), 'rev-2', 2), 0);
    assert.equal(get('VERSION_GET', 'ws-a', 'site-a', 'pv-2'), undefined);
    assert.equal(target().live_version_id, V(0), 'last good version kept');
  });

  it('the retained insert is idempotent and the row is immutable', () => {
    assert.equal(acquire('job-1', 1, V(1)), 1);
    run('TARGET_RECORD_PUBLISH', 'ws-a', 'target-a', 'job-1', V(1), 'pv-1', 'rev-1', 1, T1);
    assert.equal(retained('pv-1', V(1), 'rev-1', 1), 1);
    assert.equal(retained('pv-1', V(1), 'rev-1', 1), 0, 'replay is a no-op');
    assert.throws(() => db.exec("UPDATE published_versions SET schema_compat = 9 WHERE id = 'pv-1'"), /immutable/);
    assert.equal(get('VERSION_GET', 'ws-a', 'site-a', 'pv-1').schema_compat, 4);
  });

  it('rollback points only at a retained version of the same target, keeping the high-water mark', () => {
    assert.equal(acquire('job-1', 1, V(1)), 1);
    run('TARGET_RECORD_PUBLISH', 'ws-a', 'target-a', 'job-1', V(1), 'pv-1', 'rev-1', 1, T1);
    retained('pv-1', V(1), 'rev-1', 1);
    assert.equal(acquire('job-2', 2, V(2)), 1);
    run('TARGET_RECORD_PUBLISH', 'ws-a', 'target-a', 'job-2', V(2), 'pv-2', 'rev-2', 2, T1);
    retained('pv-2', V(2), 'rev-2', 2);

    assert.equal(acquire('rb-1', null, V(1)), 1);
    // Wrong version for the row, or a row that does not exist: nothing changes.
    assert.equal(run('TARGET_RECORD_ROLLBACK', 'ws-a', 'target-a', 'rb-1', V(7), 'pv-1', 'rev-1', T2).changes, 0);
    assert.equal(run('TARGET_RECORD_ROLLBACK', 'ws-a', 'target-a', 'rb-1', V(1), 'pv-x', 'rev-1', T2).changes, 0);
    assert.equal(run('TARGET_RECORD_ROLLBACK', 'ws-a', 'target-a', 'rb-1', V(1), 'pv-1', 'rev-1', T2).changes, 1);
    const t = target();
    assert.deepEqual([t.live_version_id, t.live_published_id, t.high_water_seq, t.activating_job], [V(1), 'pv-1', 2, null]);
    // The old revision stays refused after the rollback; only a newer one goes forward.
    assert.equal(acquire('job-2r', 2, V(8)), 0);
    assert.equal(acquire('job-3', 3, V(3)), 1);
    // Editable state is untouched: revisions are still all there, unchanged.
    assert.equal(db.prepare("SELECT count(*) AS n FROM site_revisions WHERE workspace_id = 'ws-a'").get().n, 6);
  });

  it('is tenant-scoped: other workspaces see nothing and cannot reference foreign targets', () => {
    assert.equal(target('ws-b', 'site-a'), undefined);
    assert.equal(run('TARGET_ACQUIRE', 'ws-b', 'target-a', 'j', 1, V(1), T2, T0).changes, 0);
    // The conditional insert refuses a foreign target outright, and the composite FK
    // backs it up for any direct write.
    assert.equal(retained('pv-b', V(1), 'rev-b1', 1, 'ws-b', 'site-b', 'target-a'), 0);
    assert.throws(
      () =>
        db
          .prepare(
            "INSERT INTO published_versions (workspace_id, site_id, id, target_id, cf_version_id, revision_id, revision_seq, schema_compat, manifest_sha256, verified_at, created_by) VALUES ('ws-b', 'site-b', 'pv-b', 'target-a', ?, 'rev-b1', 1, 1, ?, ?, 2)",
          )
          .run(V(1), H, T1),
      /FOREIGN KEY/,
    );
    assert.equal(get('VERSION_GET', 'ws-b', 'site-a', 'pv-1'), undefined);
  });

  it('audit rows are append-only and accept only known actions, results and codes', () => {
    const audit = (...over) =>
      run('AUDIT_INSERT', 'ws-a', 'site-a', 'target-a', 1, ...over);
    audit('rollback', 'refused', V(1), 'pv-1', 'rev-1', null, 'prepare', 'rollback_schema_incompatible', T1);
    audit('publish_activate', 'succeeded', V(2), null, 'rev-2', 'job-2', 'activate', null, T1);
    run('AUDIT_INSERT', 'ws-a', 'site-a', 'target-a', null, 'lease_takeover', 'succeeded', V(3), null, null, 'job-3', 'lock', 'stale_holder_live_adopted', T1);
    assert.throws(() => audit('delete_site', 'succeeded', null, null, null, null, null, null, T1), /CHECK/);
    assert.throws(() => audit('rollback', 'maybe', null, null, null, null, null, null, T1), /CHECK/);
    assert.throws(() => audit('rollback', 'failed', null, null, null, null, 'activate', 'Bearer abc', T1), /CHECK/);
    assert.throws(() => db.exec("UPDATE publish_audit SET result = 'succeeded'"), /append-only/);
    const rows = db.prepare(Q.AUDIT_LIST.sql).all('ws-a', 'site-a', 10);
    assert.deepEqual(rows.map((r) => r.action), ['lease_takeover', 'publish_activate', 'rollback']);
    assert.equal(db.prepare(Q.AUDIT_LIST.sql).all('ws-b', 'site-a', 10).length, 0);
  });

  it('deleting a site cascades its target, retained versions and audit', () => {
    assert.equal(acquire('job-1', 1, V(1)), 1);
    run('TARGET_RECORD_PUBLISH', 'ws-a', 'target-a', 'job-1', V(1), 'pv-1', 'rev-1', 1, T1);
    retained('pv-1', V(1), 'rev-1', 1);
    run('AUDIT_INSERT', 'ws-a', 'site-a', 'target-a', 1, 'publish_activate', 'succeeded', V(1), 'pv-1', 'rev-1', 'job-1', 'activate', null, T1);
    db.exec("DELETE FROM sites WHERE workspace_id = 'ws-a' AND id = 'site-a'");
    for (const t of ['publish_targets', 'published_versions', 'publish_audit']) {
      assert.equal(db.prepare(`SELECT count(*) AS n FROM ${t} WHERE workspace_id = 'ws-a'`).get().n, 0, t);
    }
  });
});
