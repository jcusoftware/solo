// SQL-level test for draft previews (issue #67): applies the real D1 migrations 0001 + 0002 +
// 0006 to an empty in-memory SQLite database (node:sqlite, no dependency) and runs the exact SQL
// sws-worker-studio binds, printed by `cargo run --example dump_sql -- --previews` (and
// `--assets` for the generated-artifact statements). Proves: a preview row exists only next to
// its committed artifact and copies its expiry, tenant and revision pinning, token-hash
// uniqueness, the 24 h CHECK, one-way revocation (immutability trigger), the token lookup seeing
// an evicted artifact as gone, revocation freeing the artifact and its chunks, and the bounded
// row sweep.
import { describe, it, before } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');
const MIG = resolve(ROOT, 'crates/sws-worker-studio/migrations');
const NOW = '2026-11-27T00:00:00Z';
const RES_EXP = '2026-11-27T00:10:00Z';
const DAY = '2026-11-28T00:00:00Z';
const MIB = 1024 * 1024;
const RETENTION = 24 * 3600;
const hex = (n, len = 64) => n.toString(16).padStart(len, '0');

function dump(flag) {
  const out = execFileSync('cargo', ['run', '-q', '--locked', '-p', 'sws-worker-studio', '--example', 'dump_sql', '--', flag], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  return Object.fromEntries(JSON.parse(out).map((r) => [r.name, r]));
}

const sqlOf = (f) => readFileSync(resolve(MIG, f), 'utf8');

describe('studio previews migration + SQL', () => {
  let P;
  let A;
  let db;
  const run = (Q, name, ...p) => db.prepare(Q[name].sql).run(...p);
  const get = (Q, name, ...p) => db.prepare(Q[name].sql).get(...p);

  before(() => {
    P = dump('--previews');
    A = dump('--assets');
  });

  function seed() {
    db = new DatabaseSync(':memory:');
    db.exec('PRAGMA foreign_keys = ON');
    for (const f of ['0001_registry.sql', '0002_asset_store.sql', '0006_previews.sql']) db.exec(sqlOf(f));
    db.exec(`
      INSERT INTO workspaces (id, name) VALUES ('ws-a', 'A'), ('ws-b', 'B');
      INSERT INTO users (github_id, github_login) VALUES (101, 'editor');
      INSERT INTO sites (workspace_id, id, slug, name) VALUES
        ('ws-a', 'site-1', 's1', 'S1'), ('ws-a', 'site-2', 's2', 'S2'), ('ws-b', 'site-b', 'sb', 'SB');
      INSERT INTO site_revisions (workspace_id, site_id, id, revision, ir_sha256, byte_size, created_by) VALUES
        ('ws-a', 'site-1', 'rev-1', 1, '${hex(1)}', 10, 1),
        ('ws-a', 'site-2', 'rev-2', 1, '${hex(2)}', 10, 1),
        ('ws-b', 'site-b', 'rev-b', 1, '${hex(3)}', 10, 1);
    `);
  }

  /** Reserve + commit one preview artifact the way the Worker batches it. */
  function artifact(ws, site, id, bytes = 100, { created = NOW, expires = DAY, rid = `r-${id}`, commitNow = created } = {}) {
    const reserved = run(A, 'RESERVE_GENERATED', ws, site, rid, 'preview', bytes, created, RES_EXP, 10 * MIB, 40 * MIB, 5 * MIB, 400 * MIB).changes;
    assert.equal(reserved, 1, `reserve ${id}`);
    return run(A, 'GENERATED_COMMIT', ws, site, id, 'preview', hex(bytes), bytes, 'd1', created, expires, rid, commitNow, RETENTION).changes;
  }
  const insertPreview = (ws, site, id, rev, art, tokenHash) => run(P, 'PREVIEW_INSERT', ws, site, id, rev, art, tokenHash, 1).changes;

  it('applies from empty, is re-runnable, and every statement prepares', () => {
    seed();
    db.exec(sqlOf('0006_previews.sql'));
    assert.equal(db.prepare("SELECT strict FROM pragma_table_list WHERE name = 'previews'").get().strict, 1);
    for (const r of [...Object.values(P), ...Object.values(A)]) assert.doesNotThrow(() => db.prepare(r.sql), r.name);
  });

  it('every statement except the token lookup is tenant-scoped', () => {
    for (const r of Object.values(P)) {
      if (r.name === 'PREVIEW_BY_TOKEN') {
        assert.equal(r.tenant, false);
        assert.match(r.sql, /token_hash = \?1/);
      } else {
        assert.ok(r.tenant && r.sql.includes('workspace_id = ?1'), r.name);
      }
    }
    assert.ok(A.GENERATED_CHUNK_INSERT.tenant);
  });

  it('a preview row exists only next to its committed artifact and copies its expiry', () => {
    seed();
    // No artifact yet: nothing is inserted.
    assert.equal(insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11)), 0);
    assert.equal(artifact('ws-a', 'site-1', 'g1'), 1);
    assert.equal(insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11)), 1);
    const row = db.prepare("SELECT created_at, expires_at, created_by FROM previews WHERE id = 'p1'").get();
    assert.deepEqual({ ...row }, { created_at: NOW, expires_at: DAY, created_by: 1 });
    // Another workspace or site cannot attach to that artifact.
    assert.equal(insertPreview('ws-b', 'site-b', 'p2', 'rev-b', 'g1', hex(12)), 0);
    assert.equal(insertPreview('ws-a', 'site-2', 'p2', 'rev-2', 'g1', hex(12)), 0);
  });

  it('pins the revision to the same workspace and site', () => {
    seed();
    artifact('ws-a', 'site-1', 'g1');
    assert.throws(() => insertPreview('ws-a', 'site-1', 'p1', 'rev-2', 'g1', hex(11)), /FOREIGN KEY/);
    assert.throws(() => insertPreview('ws-a', 'site-1', 'p1', 'rev-b', 'g1', hex(11)), /FOREIGN KEY/);
    assert.throws(() => insertPreview('ws-a', 'site-1', 'p1', 'nope', 'g1', hex(11)), /FOREIGN KEY/);
  });

  it('token hashes are unique and well-formed, and one artifact backs one preview', () => {
    seed();
    artifact('ws-a', 'site-1', 'g1');
    artifact('ws-a', 'site-1', 'g2');
    insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11));
    assert.throws(() => insertPreview('ws-a', 'site-1', 'p2', 'rev-1', 'g2', hex(11)), /UNIQUE/);
    assert.throws(() => insertPreview('ws-a', 'site-1', 'p2', 'rev-1', 'g1', hex(12)), /UNIQUE/);
    assert.throws(() => insertPreview('ws-a', 'site-1', 'p2', 'rev-1', 'g2', 'A'.repeat(64)), /CHECK/);
    assert.throws(() => insertPreview('ws-a', 'site-1', 'p2', 'rev-1', 'g2', 'short'), /CHECK/);
  });

  it('never stores an expiry beyond 24 hours', () => {
    seed();
    const ins = (exp) =>
      db
        .prepare(`INSERT INTO previews (workspace_id, site_id, id, revision_id, artifact_id, token_hash, created_by, created_at, expires_at)
                  VALUES ('ws-a', 'site-1', 'px', 'rev-1', 'gx', ?, 1, ?, ?)`)
        .run(hex(99), NOW, exp);
    assert.throws(() => ins('2026-11-28T00:00:01Z'), /CHECK/);
    assert.throws(() => ins(NOW), /CHECK/);
    assert.throws(() => ins('2026-11-28 00:00:00'), /CHECK/);
    assert.equal(ins(DAY).changes, 1);
  });

  it('is immutable except a one-way revoke', () => {
    seed();
    artifact('ws-a', 'site-1', 'g1');
    insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11));
    for (const col of ["token_hash = '" + hex(77) + "'", "revision_id = 'rev-1'", "expires_at = '2026-11-27T12:00:00Z'", "artifact_id = 'g9'"]) {
      assert.throws(() => db.exec(`UPDATE previews SET ${col} WHERE id = 'p1'`), /immutable/, col);
    }
    assert.equal(run(P, 'PREVIEW_REVOKE', 'ws-a', 'site-1', 'p1', NOW).changes, 1);
    // Second revoke is a no-op; un-revoking or re-stamping is refused.
    assert.equal(run(P, 'PREVIEW_REVOKE', 'ws-a', 'site-1', 'p1', DAY).changes, 0);
    assert.throws(() => db.exec("UPDATE previews SET revoked_at = NULL WHERE id = 'p1'"), /immutable/);
    assert.throws(() => db.exec(`UPDATE previews SET revoked_at = '${DAY}' WHERE id = 'p1'`), /immutable/);
    // Tenant-scoped: another workspace cannot revoke it.
    artifact('ws-a', 'site-1', 'g2');
    insertPreview('ws-a', 'site-1', 'p2', 'rev-1', 'g2', hex(12));
    assert.equal(run(P, 'PREVIEW_REVOKE', 'ws-b', 'site-1', 'p2', NOW).changes, 0);
    assert.equal(run(P, 'PREVIEW_REVOKE', 'ws-a', 'site-2', 'p2', NOW).changes, 0);
  });

  it('the token lookup returns the artifact while it exists and NULLs once it is gone', () => {
    seed();
    artifact('ws-a', 'site-1', 'g1', 300);
    insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11));
    run(A, 'GENERATED_CHUNK_INSERT', 'ws-a', 'g1', 0, 3, new Uint8Array([1, 2, 3]));
    const live = get(P, 'PREVIEW_BY_TOKEN', hex(11));
    assert.equal(live.workspace_id, 'ws-a');
    assert.equal(live.site_id, 'site-1');
    assert.equal(live.revision_id, 'rev-1');
    assert.equal(live.artifact_id, 'g1');
    assert.equal(live.artifact_bytes, 300);
    assert.equal(live.artifact_expires_at, DAY);
    assert.equal(live.revoked_at, null);
    assert.equal(get(P, 'PREVIEW_BY_TOKEN', hex(12)), undefined);
    // Retention (expiry sweep) removes the artifact: the row stays, artifact columns go NULL.
    assert.equal(run(A, 'GENERATED_EXPIRED_SWEEP', 'ws-a', 'site-1', DAY, 100).changes, 1);
    const gone = get(P, 'PREVIEW_BY_TOKEN', hex(11));
    assert.equal(gone.id, 'p1');
    assert.equal(gone.artifact_id, null);
    assert.equal(db.prepare("SELECT COUNT(*) AS n FROM blob_chunks WHERE blob_id = 'g1'").get().n, 0);
  });

  it('keep-newest eviction leaves older preview rows pointing at nothing', () => {
    seed();
    for (const n of [1, 2, 3, 4]) {
      artifact('ws-a', 'site-1', `g${n}`, 100, { created: `2026-11-27T00:00:0${n}Z`, expires: `2026-11-28T00:00:0${n}Z` });
      insertPreview('ws-a', 'site-1', `p${n}`, 'rev-1', `g${n}`, hex(10 + n));
    }
    assert.equal(run(A, 'GENERATED_EXCESS_SWEEP', 'ws-a', 'site-1', 'preview', 3, 100).changes, 1);
    assert.equal(get(P, 'PREVIEW_BY_TOKEN', hex(11)).artifact_id, null);
    assert.equal(get(P, 'PREVIEW_BY_TOKEN', hex(14)).artifact_id, 'g4');
  });

  it('revocation deletes exactly that preview artifact and its chunks', () => {
    seed();
    artifact('ws-a', 'site-1', 'g1');
    artifact('ws-a', 'site-1', 'g2');
    insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11));
    insertPreview('ws-a', 'site-1', 'p2', 'rev-1', 'g2', hex(12));
    run(A, 'GENERATED_CHUNK_INSERT', 'ws-a', 'g1', 0, 1, new Uint8Array([1]));
    run(A, 'GENERATED_CHUNK_INSERT', 'ws-a', 'g2', 0, 1, new Uint8Array([2]));
    // Wrong tenant or site: nothing.
    assert.equal(run(P, 'PREVIEW_ARTIFACT_DELETE', 'ws-b', 'site-1', 'p1').changes, 0);
    assert.equal(run(P, 'PREVIEW_ARTIFACT_DELETE', 'ws-a', 'site-2', 'p1').changes, 0);
    db.exec('BEGIN');
    assert.equal(run(P, 'PREVIEW_REVOKE', 'ws-a', 'site-1', 'p1', NOW).changes, 1);
    assert.equal(run(P, 'PREVIEW_ARTIFACT_DELETE', 'ws-a', 'site-1', 'p1').changes, 1);
    db.exec('COMMIT');
    const chunks = db.prepare('SELECT blob_id FROM blob_chunks ORDER BY blob_id').all().map((r) => r.blob_id);
    assert.deepEqual(chunks, ['g2']);
    const row = get(P, 'PREVIEW_BY_TOKEN', hex(11));
    assert.equal(row.revoked_at, NOW);
    assert.equal(row.artifact_id, null);
    assert.equal(get(P, 'PREVIEW_GET', 'ws-a', 'site-1', 'p1').revoked_at, NOW);
    assert.equal(get(P, 'PREVIEW_GET', 'ws-b', 'site-1', 'p1'), undefined);
  });

  it('generated chunks attach only to an existing generated artifact of the same workspace', () => {
    seed();
    artifact('ws-a', 'site-1', 'g1');
    db.exec(`INSERT INTO assets (workspace_id, site_id, id, sha256, byte_size, content_type, storage_backend) VALUES ('ws-a', 'site-1', 'a1', '${hex(5)}', 1, 'image/png', 'd1')`);
    const chunk = (ws, id) => run(A, 'GENERATED_CHUNK_INSERT', ws, id, 0, 1, new Uint8Array([9])).changes;
    assert.equal(chunk('ws-b', 'g1'), 0);
    assert.equal(chunk('ws-a', 'a1'), 0, 'an asset id is not a generated artifact');
    assert.equal(chunk('ws-a', 'nope'), 0);
    assert.equal(chunk('ws-a', 'g1'), 1);
    assert.throws(() => chunk('ws-a', 'g1'), /UNIQUE|PRIMARY/, 'a duplicate chunk fails the batch');
  });

  it('a commit refused for an expired reservation inserts no chunk and no preview row', () => {
    seed();
    run(A, 'RESERVE_GENERATED', 'ws-a', 'site-1', 'r1', 'preview', 100, NOW, RES_EXP, 10 * MIB, 40 * MIB, 5 * MIB, 400 * MIB);
    // Commit deadline after the reservation expiry: refused.
    assert.equal(run(A, 'GENERATED_COMMIT', 'ws-a', 'site-1', 'g1', 'preview', hex(1), 100, 'd1', NOW, DAY, 'r1', RES_EXP, RETENTION).changes, 0);
    assert.equal(run(A, 'GENERATED_CHUNK_INSERT', 'ws-a', 'g1', 0, 1, new Uint8Array([1])).changes, 0);
    assert.equal(insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11)), 0);
  });

  it('the row sweep removes only old rows of that site, in bounded batches', () => {
    seed();
    const row = (ws, site, id, rev, exp, h) =>
      db
        .prepare(`INSERT INTO previews (workspace_id, site_id, id, revision_id, artifact_id, token_hash, created_by, created_at, expires_at)
                  VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)`)
        .run(ws, site, id, rev, `g-${id}`, h, NOW, exp);
    row('ws-a', 'site-1', 'old1', 'rev-1', '2026-11-27T01:00:00Z', hex(1));
    row('ws-a', 'site-1', 'old2', 'rev-1', '2026-11-27T02:00:00Z', hex(2));
    row('ws-a', 'site-1', 'new', 'rev-1', DAY, hex(3));
    row('ws-a', 'site-2', 'other-site', 'rev-2', '2026-11-27T01:00:00Z', hex(4));
    row('ws-b', 'site-b', 'other-ws', 'rev-b', '2026-11-27T01:00:00Z', hex(5));
    const cutoff = '2026-11-27T12:00:00Z';
    assert.equal(run(P, 'PREVIEWS_SWEEP', 'ws-a', 'site-1', cutoff, 1).changes, 1, 'batch of 1');
    assert.equal(run(P, 'PREVIEWS_SWEEP', 'ws-a', 'site-1', cutoff, 100).changes, 1);
    assert.equal(run(P, 'PREVIEWS_SWEEP', 'ws-a', 'site-1', cutoff, 100).changes, 0);
    const left = db.prepare('SELECT id FROM previews ORDER BY id').all().map((r) => r.id);
    assert.deepEqual(left, ['new', 'other-site', 'other-ws']);
  });

  it('deleting a site cascades its previews', () => {
    seed();
    artifact('ws-a', 'site-1', 'g1');
    insertPreview('ws-a', 'site-1', 'p1', 'rev-1', 'g1', hex(11));
    db.exec("DELETE FROM sites WHERE workspace_id = 'ws-a' AND id = 'site-1'");
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM previews').get().n, 0);
  });
});
