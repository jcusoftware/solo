// SQL-level test for the Studio asset store (issue #63): applies the real D1
// migrations 0001 + 0002 to an empty in-memory SQLite database (node:sqlite, no
// dependency) and runs the exact SQL that sws-worker-studio binds, printed by
// `cargo run --example dump_sql -- --assets`. Proves the atomic conditional
// reservation, commit/release semantics, chunk row bounds, reconciliation,
// tenant isolation, and the separate generated preview/export budgets + retention.
import { describe, it, before } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');
const MIG = resolve(ROOT, 'crates/sws-worker-studio/migrations');
const NOW = '2026-11-27T00:00:00Z';
const EXP = '2026-11-27T00:10:00Z';
const LATER = '2026-11-27T00:10:01Z';
const MIB = 1024 * 1024;
const SITE_CAP = 50 * MIB;
const WS_CAP = 150 * MIB;
const MAX_ASSETS = 500;
const DB_CAP = 400 * MIB;
const sha = (n) => n.toString(16).padStart(64, '0');

function loadSql() {
  const out = execFileSync('cargo', ['run', '-q', '--locked', '-p', 'sws-worker-studio', '--example', 'dump_sql', '--', '--assets'], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  return Object.fromEntries(JSON.parse(out).map((r) => [r.name, r]));
}

const sqlOf = (f) => readFileSync(resolve(MIG, f), 'utf8');

function migrate(db) {
  for (const f of ['0001_registry.sql', '0002_asset_store.sql']) db.exec(sqlOf(f));
}

describe('studio asset store migration + SQL', () => {
  let Q;
  let db;
  const run = (name, ...p) => db.prepare(Q[name].sql).run(...p);
  const get = (name, ...p) => db.prepare(Q[name].sql).get(...p);

  const reserve = (ws, site, id, bytes, now = NOW, exp = EXP, caps = [SITE_CAP, WS_CAP, MAX_ASSETS, DB_CAP]) =>
    run('RESERVE_ASSET', ws, site, id, bytes, now, exp, ...caps).changes;
  // `now` is created_at (request start); `commitNow` is the clock read right before the commit.
  const commit = (ws, site, n, bytes, rid, now = NOW, backend = 'd1', commitNow = now, key = backend === 'r2' ? `${ws}/${site}/${sha(n)}/${rid}` : null) =>
    run('ASSET_COMMIT', ws, site, `a${n}`, sha(n), bytes, 'image/png', backend, now, rid, key, commitNow).changes;
  const chunk = (ws, n, seq, bytes) => run('CHUNK_INSERT', ws, `a${n}`, seq, bytes.length, bytes).changes;

  before(() => {
    Q = loadSql();
  });

  function seed() {
    db = new DatabaseSync(':memory:');
    db.exec('PRAGMA foreign_keys = ON');
    migrate(db);
    db.exec(`
      INSERT INTO workspaces (id, name) VALUES ('ws-a', 'A'), ('ws-b', 'B');
      INSERT INTO sites (workspace_id, id, slug, name) VALUES
        ('ws-a', 'site-1', 's1', 'S1'), ('ws-a', 'site-2', 's2', 'S2'), ('ws-b', 'site-b', 'sb', 'SB');
    `);
  }

  it('applies from empty, 0001 stays idempotent, and every statement prepares', () => {
    seed();
    // 0002 adds a column with ALTER, so it is applied exactly once (Wrangler tracks that).
    db.exec(sqlOf('0001_registry.sql'));
    assert.throws(() => db.exec(sqlOf('0002_asset_store.sql')), /duplicate column/);
    for (const t of ['blob_chunks', 'asset_reservations', 'generated_artifacts']) {
      assert.equal(db.prepare('SELECT strict FROM pragma_table_list WHERE name = ?').get(t).strict, 1, t);
    }
    for (const r of Object.values(Q)) assert.doesNotThrow(() => db.prepare(r.sql), r.name);
  });

  it('every statement is tenant-scoped (workspace_id is ?1)', () => {
    for (const r of Object.values(Q)) assert.ok(r.tenant && r.sql.includes('?1'), r.name);
  });

  it('reserves up to the site cap and then refuses atomically (60 x 1 MiB into 50 MiB)', () => {
    seed();
    let ok = 0;
    for (let i = 0; i < 60; i++) ok += reserve('ws-a', 'site-1', `r${i}`, MIB);
    assert.equal(ok, 50);
    const u = get('USAGE', 'ws-a', 'site-1', NOW);
    assert.equal(u.site_reserved, SITE_CAP);
    assert.equal(u.site_reserved_assets, 50);
    // Refused reservations left nothing behind.
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM asset_reservations').get().n, 50);
  });

  it('enforces the workspace cap across sites and the per-site asset count', () => {
    seed();
    // 3 x 50 MiB would be 150 MiB; site-2 (also ws-a) can only fill what is left.
    assert.equal(reserve('ws-a', 'site-1', 'r1', SITE_CAP, NOW, EXP, [SITE_CAP, 60 * MIB, MAX_ASSETS, DB_CAP]), 1);
    assert.equal(reserve('ws-a', 'site-2', 'r2', 20 * MIB, NOW, EXP, [SITE_CAP, 60 * MIB, MAX_ASSETS, DB_CAP]), 0);
    assert.equal(reserve('ws-a', 'site-2', 'r3', 10 * MIB, NOW, EXP, [SITE_CAP, 60 * MIB, MAX_ASSETS, DB_CAP]), 1);
    // The other workspace is unaffected by ws-a's usage.
    assert.equal(reserve('ws-b', 'site-b', 'rb', SITE_CAP, NOW, EXP, [SITE_CAP, 60 * MIB, MAX_ASSETS, DB_CAP]), 1);
    // Count cap.
    assert.equal(reserve('ws-a', 'site-2', 'r4', 1, NOW, EXP, [SITE_CAP, WS_CAP, 1, DB_CAP]), 0);
  });

  it('expired reservations stop counting immediately, and are swept in bounded batches', () => {
    seed();
    for (let i = 0; i < 5; i++) assert.equal(reserve('ws-a', 'site-1', `old${i}`, 10 * MIB), 1);
    assert.equal(reserve('ws-a', 'site-1', 'blocked', MIB), 0);
    // After the TTL the same request fits, without any sweep having run.
    assert.equal(reserve('ws-a', 'site-1', 'fits', MIB, LATER, '2026-11-27T00:20:01Z'), 1);
    assert.equal(get('USAGE', 'ws-a', 'site-1', LATER).site_reserved, MIB);
    assert.equal(run('RESERVATIONS_SWEEP', 'ws-a', LATER, 3).changes, 3);
    assert.equal(run('RESERVATIONS_SWEEP', 'ws-a', LATER, 3).changes, 2);
    assert.equal(run('RESERVATIONS_SWEEP', 'ws-a', LATER, 3).changes, 0);
    assert.equal(db.prepare('SELECT id FROM asset_reservations').all().map((r) => r.id).join(), 'fits');
  });

  it('release frees the space; releasing again or for another workspace changes nothing', () => {
    seed();
    reserve('ws-a', 'site-1', 'r1', SITE_CAP);
    assert.equal(reserve('ws-a', 'site-1', 'r2', 1), 0);
    assert.equal(run('RESERVATION_RELEASE', 'ws-b', 'site-1', 'r1').changes, 0);
    assert.equal(run('RESERVATION_RELEASE', 'ws-a', 'site-1', 'r1').changes, 1);
    assert.equal(run('RESERVATION_RELEASE', 'ws-a', 'site-1', 'r1').changes, 0);
    assert.equal(reserve('ws-a', 'site-1', 'r2', SITE_CAP), 1);
  });

  it('cannot reserve for a site in another workspace (composite FK)', () => {
    seed();
    assert.throws(() => reserve('ws-b', 'site-1', 'x', 1), /FOREIGN KEY/);
  });

  it('commit stores the ACTUAL size, releases the reservation, and counts as stored', () => {
    seed();
    reserve('ws-a', 'site-1', 'r1', MIB);
    const batch = db.prepare('BEGIN');
    batch.run();
    assert.equal(commit('ws-a', 'site-1', 1, 1000, 'r1'), 1);
    assert.equal(chunk('ws-a', 1, 0, new Uint8Array(1000)), 1);
    assert.equal(run('RESERVATION_DELETE', 'ws-a', 'r1').changes, 1);
    db.prepare('COMMIT').run();
    const u = get('USAGE', 'ws-a', 'site-1', NOW);
    assert.deepEqual({ ...u }, {
      site_stored: 1000,
      site_reserved: 0,
      site_assets: 1,
      site_reserved_assets: 0,
      workspace_stored: 1000,
      workspace_reserved: 0,
      database_blob_bytes: 1000,
    });
    assert.equal(db.prepare('SELECT byte_size FROM assets WHERE sha256 = ?').get(sha(1)).byte_size, 1000);
  });

  it('commit is refused without a live, large-enough reservation', () => {
    seed();
    assert.equal(commit('ws-a', 'site-1', 1, 10, 'nope'), 0, 'no reservation');
    reserve('ws-a', 'site-1', 'r1', 100);
    assert.equal(commit('ws-a', 'site-1', 2, 101, 'r1'), 0, 'more than reserved');
    assert.equal(commit('ws-a', 'site-1', 3, 100, 'r1', LATER), 0, 'expired');
    assert.equal(commit('ws-a', 'site-1', 5, 100, 'r1', NOW, 'd1', LATER), 0, 'started live, expired by commit time');
    assert.equal(commit('ws-b', 'site-b', 4, 50, 'r1'), 0, 'another workspace cannot use the reservation');
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM assets').get().n, 0);
  });

  it('commit liveness is strict at the bound deadline (the Worker binds commit_now + the safety margin)', () => {
    seed();
    reserve('ws-a', 'site-1', 'edge', 100);
    assert.equal(commit('ws-a', 'site-1', 1, 100, 'edge', NOW, 'd1', EXP), 0, 'deadline == expiry is refused');
    assert.equal(commit('ws-a', 'site-1', 1, 100, 'edge', NOW, 'd1', '2026-11-27T00:09:59Z'), 1, 'one second inside is accepted');
  });

  it('liveness uses the COMMIT-time clock, not the request start: a slow upload is refused and stores nothing', () => {
    seed();
    reserve('ws-a', 'site-1', 'slow', 1000);
    // Request started at NOW (created_at = NOW) but the commit batch runs after the TTL.
    db.prepare('BEGIN').run();
    assert.equal(commit('ws-a', 'site-1', 1, 1000, 'slow', NOW, 'd1', LATER), 0, 'refused');
    assert.equal(chunk('ws-a', 1, 0, new Uint8Array(1000)), 0, 'no chunk without the row');
    assert.equal(run('RESERVATION_DELETE', 'ws-a', 'slow').changes, 1, 'the batch still releases the reservation');
    db.prepare('COMMIT').run();
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM assets').get().n, 0);
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM blob_chunks').get().n, 0);
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM asset_reservations').get().n, 0);
    // The same reservation, committed before its expiry, succeeds.
    reserve('ws-a', 'site-1', 'fast', 1000);
    assert.equal(commit('ws-a', 'site-1', 1, 1000, 'fast', NOW, 'd1', EXP.replace('10:00', '09:59')), 1);
  });

  it('identical content commits once; a parallel duplicate neither errors nor double-stores', () => {
    seed();
    reserve('ws-a', 'site-1', 'r1', 1000);
    reserve('ws-a', 'site-1', 'r2', 1000);
    assert.equal(commit('ws-a', 'site-1', 1, 1000, 'r1'), 1);
    assert.equal(chunk('ws-a', 1, 0, new Uint8Array(1000)), 1);
    // Same sha, same deterministic id in the Worker: ON CONFLICT DO NOTHING on both inserts.
    assert.equal(commit('ws-a', 'site-1', 1, 1000, 'r2'), 0);
    assert.equal(chunk('ws-a', 1, 0, new Uint8Array(1000)), 0);
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM blob_chunks').get().n, 1);
  });

  it('a chunk needs its asset row, and rows are hard-capped below the D1 row limit', () => {
    seed();
    assert.equal(chunk('ws-a', 9, 0, new Uint8Array(10)), 0, 'no owning asset');
    reserve('ws-a', 'site-1', 'r1', MIB);
    commit('ws-a', 'site-1', 1, 600 * 1024, 'r1');
    assert.equal(chunk('ws-a', 1, 0, new Uint8Array(512 * 1024)), 1);
    assert.throws(() => chunk('ws-a', 1, 1, new Uint8Array(512 * 1024 + 1)), /CHECK/);
    assert.throws(() => run('CHUNK_INSERT', 'ws-a', 'a1', 2, 5, new Uint8Array(4)), /CHECK/, 'length must equal byte_size');
    assert.throws(() => run('CHUNK_INSERT', 'ws-a', 'a1', 3, 4, 'text'), /BLOB|CHECK/, 'BLOB only');
  });

  it('chunks are tenant-scoped on read and delete', () => {
    seed();
    reserve('ws-a', 'site-1', 'r1', 100);
    commit('ws-a', 'site-1', 1, 4, 'r1');
    chunk('ws-a', 1, 0, new Uint8Array([1, 2, 3, 4]));
    assert.equal(get('CHUNK_GET', 'ws-b', 'a1', 0), undefined);
    assert.deepEqual([...get('CHUNK_GET', 'ws-a', 'a1', 0).data], [1, 2, 3, 4]);
    assert.equal(run('ASSET_DELETE', 'ws-b', 'site-1', sha(1), null).changes, 0);
    assert.equal(get('CHUNK_GET', 'ws-a', 'a1', 0) !== undefined, true, 'another workspace cannot delete it');
    // The AFTER DELETE trigger removes the chunks in the same statement.
    run('ASSET_DELETE', 'ws-a', 'site-1', sha(1), null);
    assert.equal(get('CHUNK_GET', 'ws-a', 'a1', 0), undefined);
    assert.equal(get('USAGE', 'ws-a', 'site-1', NOW).site_stored, 0);
  });

  it('reconcile removes orphan chunks and incomplete D1 assets, never another workspace or R2 rows', () => {
    seed();
    reserve('ws-a', 'site-1', 'r1', MIB);
    reserve('ws-a', 'site-1', 'r2', MIB);
    reserve('ws-a', 'site-1', 'r3', MIB);
    reserve('ws-a', 'site-1', 'r4', MIB);
    commit('ws-a', 'site-1', 1, 4, 'r1'); // complete
    chunk('ws-a', 1, 0, new Uint8Array([1, 2, 3, 4]));
    commit('ws-a', 'site-1', 2, 8, 'r2'); // incomplete: 4 of 8 bytes
    chunk('ws-a', 2, 0, new Uint8Array([1, 2, 3, 4]));
    commit('ws-a', 'site-1', 3, 8, 'r3'); // incomplete: no chunks at all
    commit('ws-a', 'site-1', 4, 999, 'r4', NOW, 'r2'); // R2 rows have no chunks by design
    // Orphans: chunks of a blob that no longer exists, in both workspaces.
    db.prepare("INSERT INTO blob_chunks (workspace_id, blob_id, seq, byte_size, data) VALUES ('ws-a','gone',0,2,x'0102'), ('ws-a','gone',1,2,x'0304'), ('ws-b','gone',0,2,x'0102')").run();

    assert.equal(run('ASSETS_INCOMPLETE_SWEEP', 'ws-b', 'site-1', 100).changes, 0);
    assert.equal(run('ASSETS_INCOMPLETE_SWEEP', 'ws-a', 'site-1', 100).changes, 2);
    // The AFTER DELETE trigger already removed a2's chunk with its row, so only the two
    // truly orphaned chunks ('gone') remain for the sweep, batched.
    assert.equal(run('CHUNKS_ORPHAN_SWEEP', 'ws-a', 1).changes, 1);
    assert.equal(run('CHUNKS_ORPHAN_SWEEP', 'ws-a', 100).changes, 1);
    assert.equal(run('CHUNKS_ORPHAN_SWEEP', 'ws-a', 100).changes, 0);
    const left = db.prepare('SELECT workspace_id, blob_id FROM blob_chunks ORDER BY 1, 2').all().map((r) => `${r.workspace_id}/${r.blob_id}`);
    assert.deepEqual(left, ['ws-a/a1', 'ws-b/gone']);
    assert.deepEqual(db.prepare('SELECT sha256 FROM assets ORDER BY sha256').all().map((r) => r.sha256), [sha(1), sha(4)]);
    assert.equal(get('ASSET_OBJECT_REFERENCED', 'ws-a', `ws-a/site-1/${sha(4)}/r4`).referenced, 1);
    assert.equal(get('ASSET_OBJECT_REFERENCED', 'ws-b', `ws-a/site-1/${sha(4)}/r4`), undefined);
    assert.equal(get('ASSET_OBJECT_REFERENCED', 'ws-a', `ws-a/site-1/${sha(4)}/other`), undefined, 'exact key only');
    assert.equal(get('ASSET_OBJECT_REFERENCED', 'ws-a', `ws-a/site-1/${sha(1)}/r1`), undefined, 'd1 rows have no object');
  });

  it('refuses a reservation past the database-wide blob cap, across workspaces', () => {
    seed();
    const caps = [SITE_CAP, WS_CAP, MAX_ASSETS, 10 * MIB];
    assert.equal(reserve('ws-a', 'site-1', 'a', 6 * MIB, NOW, EXP, caps), 1);
    assert.equal(reserve('ws-b', 'site-b', 'b', 6 * MIB, NOW, EXP, caps), 0, 'another workspace fills the same database');
    assert.equal(reserve('ws-b', 'site-b', 'b2', 4 * MIB, NOW, EXP, caps), 1);
    assert.equal(reserve('ws-b', 'site-b', 'b3', 1, NOW, EXP, caps), 0);
    assert.equal(get('USAGE', 'ws-a', 'site-1', NOW).database_blob_bytes, 10 * MIB);
    // Expired reservations stop counting; unlimited (R2 backend) is not capped.
    assert.equal(reserve('ws-b', 'site-b', 'b4', 1, LATER, '2026-11-27T00:20:01Z', caps), 1);
    assert.equal(reserve('ws-b', 'site-b', 'b5', 8 * MIB, LATER, '2026-11-27T00:20:01Z', [SITE_CAP, WS_CAP, MAX_ASSETS, Number.MAX_SAFE_INTEGER]), 1);
  });

  it('database cap counts D1 assets and generated artifacts but not R2 bytes', () => {
    seed();
    const caps = [SITE_CAP, WS_CAP, MAX_ASSETS, 3 * MIB];
    reserve('ws-a', 'site-1', 'r1', 2 * MIB);
    commit('ws-a', 'site-1', 1, 2 * MIB, 'r1'); // D1: 2 MiB
    reserve('ws-a', 'site-1', 'r2', 40 * MIB);
    commit('ws-a', 'site-1', 2, 40 * MIB, 'r2', NOW, 'r2'); // R2: not in D1
    run('RESERVATION_DELETE', 'ws-a', 'r1');
    run('RESERVATION_DELETE', 'ws-a', 'r2');
    assert.equal(get('USAGE', 'ws-a', 'site-1', NOW).database_blob_bytes, 2 * MIB);
    assert.equal(reserve('ws-b', 'site-b', 'x', 2 * MIB, NOW, EXP, caps), 0);
    assert.equal(reserve('ws-b', 'site-b', 'y', MIB, NOW, EXP, caps), 1);
  });

  describe('R2 objects: per-upload keys, tombstones, no shared object', () => {
    const keyed = (rid, n = 1) => `ws-a/site-1/${sha(n)}/${rid}`;

    it('a delete then re-upload of identical bytes never shares an object key', () => {
      seed();
      reserve('ws-a', 'site-1', 'u1', MIB);
      assert.equal(commit('ws-a', 'site-1', 1, 100, 'u1', NOW, 'r2'), 1);
      assert.equal(get('ASSET_GET', 'ws-a', 'site-1', sha(1)).object_key, keyed('u1'));
      // Delete: the row goes and its key is tombstoned in the same statement.
      assert.equal(run('ASSET_DELETE', 'ws-a', 'site-1', sha(1), keyed('other')).changes, 0, 'a stale key matches nothing');
      assert.equal(run('ASSET_DELETE', 'ws-a', 'site-1', sha(1), null).changes, 0, 'an R2 row is not a D1 row');
      assert.equal(run('ASSET_DELETE', 'ws-a', 'site-1', sha(1), keyed('u1')).changes, 1);
      assert.deepEqual(db.prepare(Q.TOMBSTONES_LIST.sql).all('ws-a', 10).map((r) => r.object_key), [keyed('u1')]);
      // Re-upload (it raced the delete): a NEW key, so deleting u1's object cannot touch it.
      reserve('ws-a', 'site-1', 'u2', MIB);
      assert.equal(commit('ws-a', 'site-1', 1, 100, 'u2', NOW, 'r2'), 1);
      assert.equal(get('ASSET_OBJECT_REFERENCED', 'ws-a', keyed('u1')), undefined, 'old key is unreferenced: safe to delete');
      assert.equal(get('ASSET_OBJECT_REFERENCED', 'ws-a', keyed('u2')).referenced, 1, 'new key is referenced: never deleted');
      assert.equal(run('TOMBSTONE_CLEAR', 'ws-a', keyed('u1')).changes, 1);
      assert.equal(run('TOMBSTONE_CLEAR', 'ws-a', keyed('u1')).changes, 0);
      assert.equal(get('ASSET_GET', 'ws-a', 'site-1', sha(1)).object_key, keyed('u2'));
    });

    it('a concurrent duplicate upload does not replace the winner and its own key stays unreferenced', () => {
      seed();
      reserve('ws-a', 'site-1', 'u1', MIB);
      reserve('ws-a', 'site-1', 'u2', MIB);
      assert.equal(commit('ws-a', 'site-1', 1, 100, 'u1', NOW, 'r2'), 1);
      assert.equal(commit('ws-a', 'site-1', 1, 100, 'u2', NOW, 'r2'), 0, 'loser inserts nothing');
      assert.equal(get('ASSET_OBJECT_REFERENCED', 'ws-a', keyed('u2')), undefined, 'the loser deletes only its own object');
      assert.equal(get('ASSET_GET', 'ws-a', 'site-1', sha(1)).object_key, keyed('u1'));
    });

    it('rows are r2 <=> keyed, and keys are unique and well-formed', () => {
      seed();
      reserve('ws-a', 'site-1', 'u1', MIB);
      assert.throws(() => commit('ws-a', 'site-1', 1, 100, 'u1', NOW, 'r2', NOW, null), /CHECK/, 'r2 needs a key');
      assert.throws(() => commit('ws-a', 'site-1', 1, 100, 'u1', NOW, 'd1', NOW, 'ws-a/site-1/x/y'), /CHECK/, 'd1 has no key');
      assert.throws(() => commit('ws-a', 'site-1', 1, 100, 'u1', NOW, 'r2', NOW, '../x'), /CHECK/, 'no traversal characters');
    });

    it('deleting a site (cascade) tombstones its R2 objects for the workspace and removes its chunks', () => {
      seed();
      reserve('ws-a', 'site-2', 'u1', MIB);
      commit('ws-a', 'site-2', 7, 100, 'u1', NOW, 'r2');
      reserve('ws-a', 'site-2', 'u2', MIB);
      commit('ws-a', 'site-2', 8, 4, 'u2');
      chunk('ws-a', 8, 0, new Uint8Array([1, 2, 3, 4]));
      db.prepare("DELETE FROM sites WHERE workspace_id = 'ws-a' AND id = 'site-2'").run();
      assert.equal(db.prepare('SELECT COUNT(*) AS n FROM assets').get().n, 0);
      assert.equal(db.prepare('SELECT COUNT(*) AS n FROM blob_chunks').get().n, 0, 'trigger removed the chunks');
      const t = db.prepare(Q.TOMBSTONES_LIST.sql).all('ws-a', 10).map((r) => r.object_key);
      assert.deepEqual(t, [`ws-a/site-2/${sha(7)}/u1`]);
      assert.deepEqual(db.prepare(Q.TOMBSTONES_LIST.sql).all('ws-b', 10), [], 'tenant-scoped');
    });
  });

  it('chunks of a deleted generated artifact go with it (trigger), not via an orphan sweep', () => {
    seed();
    db.prepare("INSERT INTO generated_artifacts (workspace_id, site_id, id, kind, sha256, byte_size, storage_backend, created_at, expires_at) VALUES ('ws-a','site-1','g1','preview',?,4,'d1','2026-11-20T00:00:00Z','2026-11-21T00:00:00Z')").run(sha(9));
    db.prepare("INSERT INTO blob_chunks (workspace_id, blob_id, seq, byte_size, data) VALUES ('ws-a','g1',0,4,x'01020304')").run();
    assert.equal(run('GENERATED_EXPIRED_SWEEP', 'ws-a', 'site-1', NOW, 10).changes, 1);
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM blob_chunks').get().n, 0);
  });

  describe('generated preview/export budgets and retention', () => {
    const PREVIEW_SITE = 10 * MIB;
    const EXPORT_SITE = 20 * MIB;
    const GEN_WS = 40 * MIB;
    const gres = (ws, site, id, kind, bytes, now = NOW, exp = EXP, maxArtifact = 1e12, dbCap = DB_CAP) =>
      run('RESERVE_GENERATED', ws, site, id, kind, bytes, now, exp, kind === 'preview' ? PREVIEW_SITE : EXPORT_SITE, GEN_WS, maxArtifact, dbCap).changes;
    const RETENTION = 30 * 86400;
    const gcommit = (ws, site, id, kind, n, bytes, rid, created, expires, commitNow = created, retention = RETENTION) =>
      run('GENERATED_COMMIT', ws, site, id, kind, sha(n), bytes, 'd1', created, expires, rid, commitNow, retention).changes;

    it('is budgeted separately from assets (neither consumes the other)', () => {
      seed();
      assert.equal(reserve('ws-a', 'site-1', 'fill', SITE_CAP), 1, 'assets fill their whole site budget');
      assert.equal(gres('ws-a', 'site-1', 'g1', 'preview', PREVIEW_SITE), 1, 'preview budget untouched');
      assert.equal(gres('ws-a', 'site-1', 'g2', 'preview', 1), 0, 'preview site cap reached');
      assert.equal(gres('ws-a', 'site-1', 'g3', 'export', EXPORT_SITE), 1, 'export has its own cap');
      assert.equal(get('USAGE', 'ws-a', 'site-1', NOW).site_reserved, SITE_CAP, 'asset usage ignores generated reservations');
    });

    it('enforces the workspace-wide generated cap across sites and kinds', () => {
      seed();
      assert.equal(gres('ws-a', 'site-1', 'g1', 'export', EXPORT_SITE), 1);
      assert.equal(gres('ws-a', 'site-2', 'g2', 'export', EXPORT_SITE), 1);
      assert.equal(gres('ws-a', 'site-2', 'g3', 'preview', 1), 0, '40 MiB workspace cap reached');
      assert.equal(gres('ws-b', 'site-b', 'g4', 'preview', MIB), 1, 'other workspace unaffected');
    });

    it('commits against a live reservation only, and a committed artifact keeps counting', () => {
      seed();
      assert.equal(gcommit('ws-a', 'site-1', 'ga', 'preview', 1, 100, 'none', NOW, '2026-11-28T00:00:00Z'), 0);
      gres('ws-a', 'site-1', 'g1', 'preview', MIB);
      assert.equal(gcommit('ws-a', 'site-1', 'ga', 'preview', 1, MIB, 'g1', NOW, '2026-11-28T00:00:00Z'), 1);
      run('RESERVATION_DELETE', 'ws-a', 'g1');
      assert.equal(gres('ws-a', 'site-1', 'g2', 'preview', PREVIEW_SITE), 0, 'stored bytes still count');
      assert.equal(gres('ws-a', 'site-1', 'g3', 'preview', PREVIEW_SITE - MIB), 1);
      assert.deepEqual(db.prepare(Q.GENERATED_USAGE.sql).all('ws-a', 'site-1').map((r) => ({ ...r })), [
        { kind: 'preview', artifacts: 1, bytes: MIB },
      ]);
    });

    it('only UNEXPIRED artifacts count toward the generated budgets', () => {
      seed();
      const mk = (id, n, created, expires) => {
        gres('ws-a', 'site-1', `res-${id}`, 'preview', MIB, created, '2026-12-31T00:00:00Z');
        assert.equal(gcommit('ws-a', 'site-1', id, 'preview', n, MIB, `res-${id}`, created, expires), 1);
        run('RESERVATION_DELETE', 'ws-a', `res-${id}`);
      };
      mk('old', 1, '2026-11-20T00:00:00Z', '2026-11-21T00:00:00Z'); // already expired at NOW
      mk('live', 2, '2026-11-26T00:00:00Z', '2026-11-29T00:00:00Z');
      // 10 MiB per-site preview cap: 1 MiB live counts, the expired 1 MiB does not.
      assert.equal(gres('ws-a', 'site-1', 'fits', 'preview', PREVIEW_SITE - MIB), 1);
      assert.equal(gres('ws-a', 'site-1', 'over', 'preview', 1), 0);
    });

    it('enforces the per-artifact cap and the database-wide cap in SQL', () => {
      seed();
      assert.equal(gres('ws-a', 'site-1', 'big', 'preview', 6 * MIB, NOW, EXP, 5 * MIB), 0, 'over max_artifact_bytes');
      assert.equal(gres('ws-a', 'site-1', 'ok', 'preview', 5 * MIB, NOW, EXP, 5 * MIB), 1);
      assert.equal(gres('ws-b', 'site-b', 'dbfull', 'preview', MIB, NOW, EXP, 5 * MIB, 5 * MIB), 0, 'the whole database is at its cap');
    });

    it('refuses a commit that outlives its reservation or its retention', () => {
      seed();
      gres('ws-a', 'site-1', 'g1', 'preview', MIB);
      // Reservation expired by the time of the commit (clock read just before it).
      assert.equal(gcommit('ws-a', 'site-1', 'ga', 'preview', 1, MIB, 'g1', NOW, '2026-11-28T00:00:00Z', LATER), 0);
      // expires_at beyond created_at + retention is refused even when a caller skips the clamp.
      assert.equal(gcommit('ws-a', 'site-1', 'gb', 'preview', 1, MIB, 'g1', NOW, '2026-11-28T00:00:01Z', NOW, 86400), 0);
      assert.equal(gcommit('ws-a', 'site-1', 'gc', 'preview', 1, MIB, 'g1', NOW, '2026-11-28T00:00:00Z', NOW, 86400), 1);
    });

    it('generated artifacts are D1-only until #64/#66 (an R2 row is impossible)', () => {
      seed();
      gres('ws-a', 'site-1', 'g1', 'export', MIB);
      assert.throws(
        () => run('GENERATED_COMMIT', 'ws-a', 'site-1', 'gr', 'export', sha(1), MIB, 'r2', NOW, '2026-11-28T00:00:00Z', 'g1', NOW, 7 * 86400),
        /CHECK/,
      );
    });

    it('the excess sweep deletes at most one batch per call', () => {
      seed();
      for (let i = 0; i < 6; i++) {
        const created = `2026-11-27T0${i}:00:00Z`;
        gres('ws-a', 'site-1', `rs${i}`, 'preview', 100, created, '2026-12-31T00:00:00Z');
        gcommit('ws-a', 'site-1', `p${i}`, 'preview', 20 + i, 100, `rs${i}`, created, '2026-11-28T23:00:00Z');
        run('RESERVATION_DELETE', 'ws-a', `rs${i}`);
      }
      assert.equal(run('GENERATED_EXCESS_SWEEP', 'ws-a', 'site-1', 'preview', 1, 2).changes, 2);
      assert.equal(run('GENERATED_EXCESS_SWEEP', 'ws-a', 'site-1', 'preview', 1, 2).changes, 2);
      assert.equal(run('GENERATED_EXCESS_SWEEP', 'ws-a', 'site-1', 'preview', 1, 2).changes, 1);
      assert.deepEqual(db.prepare('SELECT id FROM generated_artifacts').all().map((r) => r.id), ['p5']);
    });

    it('retention deletes expired artifacts and keeps only the newest N per kind', () => {
      seed();
      const mk = (id, kind, n, created, expires) => {
        gres('ws-a', 'site-1', `res-${id}`, kind, 100, created, '2026-12-31T00:00:00Z');
        assert.equal(gcommit('ws-a', 'site-1', id, kind, n, 100, `res-${id}`, created, expires), 1);
        run('RESERVATION_DELETE', 'ws-a', `res-${id}`);
      };
      mk('p-old', 'preview', 1, '2026-11-20T00:00:00Z', '2026-11-21T00:00:00Z'); // expired
      for (const [i, hh] of [10, 11, 12, 13, 14].entries()) mk(`p${i}`, 'preview', 10 + i, `2026-11-27T${hh}:00:00Z`, '2026-11-28T23:00:00Z');
      mk('e0', 'export', 30, '2026-11-26T00:00:00Z', '2026-12-03T00:00:00Z');
      mk('e1', 'export', 31, '2026-11-26T01:00:00Z', '2026-12-03T00:00:00Z');
      mk('e2', 'export', 32, '2026-11-26T02:00:00Z', '2026-12-03T00:00:00Z');

      assert.equal(run('GENERATED_EXPIRED_SWEEP', 'ws-b', 'site-1', NOW, 100).changes, 0, 'tenant-scoped');
      assert.equal(run('GENERATED_EXPIRED_SWEEP', 'ws-a', 'site-1', '2026-11-27T12:00:00Z', 100).changes, 1);
      // keep_latest: preview 3, export 2 (sws-assets limits).
      assert.equal(run('GENERATED_EXCESS_SWEEP', 'ws-a', 'site-1', 'preview', 3, 100).changes, 2);
      assert.equal(run('GENERATED_EXCESS_SWEEP', 'ws-a', 'site-1', 'export', 2, 100).changes, 1);
      assert.equal(run('GENERATED_EXCESS_SWEEP', 'ws-b', 'site-1', 'export', 0, 100).changes, 0, 'tenant-scoped');
      const left = db.prepare('SELECT id FROM generated_artifacts ORDER BY id').all().map((r) => r.id);
      assert.deepEqual(left, ['e1', 'e2', 'p2', 'p3', 'p4']);
    });
  });
});
