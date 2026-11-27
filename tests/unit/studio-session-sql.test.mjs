// SQL-level test for the per-site Durable Object SQLite database (#62): applies the real schema
// to an in-memory SQLite (node:sqlite, no dependency) and runs the exact statements that
// sws-worker-studio binds, printed by `cargo run --example dump_session_sql`. The object itself
// only runs on Workers; this proves the SQL it sends is valid and has the intended guarantees
// (contiguous monotonic log, immutable rows, idempotent compaction, op-id index, presence expiry).
import { describe, it, before, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');

function load() {
  const out = execFileSync('cargo', ['run', '-q', '--locked', '-p', 'sws-worker-studio', '--example', 'dump_session_sql'], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  const parsed = JSON.parse(out);
  return { schema: parsed.schema, Q: Object.fromEntries(parsed.statements.map((s) => [s.name, s.sql])) };
}

describe('site session Durable Object SQL', () => {
  let schema;
  let Q;
  let db;
  const run = (name, ...p) => db.prepare(Q[name]).run(...p);
  const all = (name, ...p) => db.prepare(Q[name]).all(...p);
  const append = (seq, op = `op-${seq}`) =>
    run('LOG_APPEND', seq, op, 1, 'c-1', 'batch', null, Math.max(0, seq - 1), 0, '["site/name"]', '{}', 1000 + seq);

  before(() => ({ schema, Q } = load()));
  beforeEach(() => {
    db = new DatabaseSync(':memory:');
    for (const s of schema) db.exec(s);
  });

  it('applies idempotently and every statement prepares against it', () => {
    for (const s of schema) db.exec(s);
    for (const [name, sql] of Object.entries(Q)) assert.doesNotThrow(() => db.prepare(sql), name);
  });

  it('the log is contiguous and strictly monotonic, starting after the snapshot', () => {
    run('SNAPSHOT_PUT', 0, '{}', 'h', 1);
    assert.throws(() => append(2), /next integer/);
    append(1);
    assert.throws(() => append(1, 'dup'), /next integer|UNIQUE|constraint/);
    assert.throws(() => append(3), /next integer/);
    append(2);
    assert.equal(all('LOG_MAX_SEQ')[0].seq, 2);
    assert.throws(() => db.exec('UPDATE log SET tx = \'x\' WHERE seq = 1'), /immutable/);
  });

  it('after compaction the next seq still follows the head', () => {
    for (let i = 1; i <= 5; i++) append(i);
    run('LOG_COMPACT', 3);
    run('LOG_COMPACT', 3); // idempotent
    assert.deepEqual(all('LOG_AFTER', 0, 100).map((r) => r.seq), [4, 5]);
    append(6);
    assert.deepEqual(all('LOG_TOUCHED_AFTER', 4).map((r) => r.seq), [5, 6]);
    assert.equal(all('LOG_GET', 3).length, 0);
    // Even a fully compacted log keeps the sequence going from the snapshot.
    run('SNAPSHOT_PUT', 6, '{}', 'h', 2);
    run('LOG_COMPACT', 6);
    assert.throws(() => append(1, 'x'), /next integer/);
    append(7);
  });

  it('keeps the latest snapshot and the retained publish snapshot only', () => {
    run('SNAPSHOT_PUT', 0, 'a', 'h0', 1);
    run('SNAPSHOT_PUT', 20, 'p', 'h20', 2);
    run('SNAPSHOT_PUT', 50, 'b', 'h50', 3);
    run('SNAPSHOT_PRUNE', 50, 20);
    assert.deepEqual(db.prepare('SELECT seq FROM snapshots ORDER BY seq').all().map((r) => r.seq), [20, 50]);
    assert.deepEqual(all('SNAPSHOT_LATEST').map((r) => ({ ...r })), [{ seq: 50, doc: 'b', sha256: 'h50' }]);
    assert.equal(all('SNAPSHOT_GET', 20)[0].doc, 'p', 'the publish snapshot is fetchable by seq');
    assert.equal(all('SNAPSHOT_GET', 0).length, 0);
    run('SNAPSHOT_PUT', 50, 'b2', 'h50b', 4); // re-taking a checkpoint overwrites, never duplicates
    assert.equal(all('SNAPSHOT_LATEST')[0].doc, 'b2');
    run('SNAPSHOT_PRUNE', 50, -1); // no retained publish snapshot
    assert.deepEqual(db.prepare('SELECT seq FROM snapshots').all().map((r) => r.seq), [50]);
  });

  it('meta is one upserted JSON row', () => {
    run('META_PUT', 'meta', '{"a":1}');
    run('META_PUT', 'meta', '{"a":2}');
    assert.deepEqual(all('META_GET', 'meta').map((r) => r.value), ['{"a":2}']);
  });

  it('operation ids dedupe, expire and are capped', () => {
    run('OP_PUT', 'op-a', 1, 1, 0, 5000, 'hash-a');
    run('OP_PUT', 'op-a', 1, 1, 0, 5000, 'hash-a'); // retry of the bookkeeping is harmless
    assert.equal(all('OP_GET', 'op-a', 10)[0].payload, 'hash-a', 'the request fingerprint is stored');
    assert.equal(all('OP_GET', 'op-a', 4999).length, 1);
    assert.equal(all('OP_GET', 'op-a', 5000).length, 0, 'expired ops are invisible');
    for (let i = 0; i < 10; i++) run('OP_PUT', `op-${i}`, 1, 10 + i, 0, 9000, `h${i}`);
    run('OP_PRUNE_EXPIRED', 6000);
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM ops').get().n, 10);
    run('OP_PRUNE_EXCESS', 4);
    const kept = db.prepare('SELECT seq FROM ops ORDER BY seq').all().map((r) => r.seq);
    assert.deepEqual(kept, [16, 17, 18, 19], 'only the newest rows survive');
    assert.equal(all('OP_NEXT_EXPIRY')[0].at, 9000);
  });

  it('presence upserts per client, lists only live rows and prunes expired ones', () => {
    run('PRESENCE_PUT', 'c-1', 1, '{"k":1}', 100, 1600);
    run('PRESENCE_PUT', 'c-2', 2, '{"k":2}', 120, 1620);
    run('PRESENCE_PUT', 'c-1', 1, '{"k":3}', 900, 2400);
    assert.deepEqual(all('PRESENCE_LIST', 1000).map((r) => [r.client_id, r.state]), [['c-1', '{"k":3}'], ['c-2', '{"k":2}']]);
    assert.deepEqual(all('PRESENCE_LIST', 1700).map((r) => r.client_id), ['c-1']);
    assert.equal(all('PRESENCE_COUNT_EXPIRED', 1700)[0].n, 1);
    run('PRESENCE_PRUNE', 1700);
    assert.equal(all('PRESENCE_NEXT_EXPIRY')[0].at, 2400);
    run('PRESENCE_DELETE', 'c-1');
    assert.equal(all('PRESENCE_NEXT_EXPIRY')[0].at, null);
    assert.equal(all('PRESENCE_GET', 'c-1').length, 0);
  });

  it('log kinds are constrained', () => {
    assert.throws(
      () => run('LOG_APPEND', 1, 'op', 1, 'c', 'rewind', null, 0, 0, '[]', '{}', 1),
      /CHECK|constraint/,
    );
  });
});
