// SQL-level test for the Studio registry (issue #61): applies the real D1
// migration to an empty in-memory SQLite database (node:sqlite, no dependency)
// and runs the exact repository SQL that sws-worker-studio binds, printed by
// `cargo run --example dump_sql`. Proves migrations apply from empty, CHECK /
// FK / unique constraints hold, and cross-workspace queries return nothing.
import { describe, it, before } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');
const MIGRATION = resolve(ROOT, 'crates/sws-worker-studio/migrations/0001_registry.sql');
const NOW = '2026-11-27T00:00:00Z';
const H1 = 'a'.repeat(64);
const H2 = 'b'.repeat(64);

function loadSql() {
  const out = execFileSync('cargo', ['run', '-q', '--locked', '-p', 'sws-worker-studio', '--example', 'dump_sql'], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  return Object.fromEntries(JSON.parse(out).map((r) => [r.name, r]));
}

function freshDb() {
  const db = new DatabaseSync(':memory:');
  db.exec('PRAGMA foreign_keys = ON');
  db.exec(readFileSync(MIGRATION, 'utf8'));
  return db;
}

describe('studio registry migration', () => {
  let Q;
  let db;
  const run = (name, ...p) => db.prepare(Q[name].sql).run(...p);
  const all = (name, ...p) => db.prepare(Q[name].sql).all(...p);

  before(() => {
    Q = loadSql();
  });

  function seed() {
    db = freshDb();
    run('WORKSPACE_INSERT', 'ws-a', 'Workspace A');
    run('WORKSPACE_INSERT', 'ws-b', 'Workspace B');
    run('USER_UPSERT', 1001, 'alice');
    run('USER_UPSERT', 2002, 'bob');
    run('MEMBERSHIP_INSERT', 'ws-a', 1, 'owner');
    run('MEMBERSHIP_INSERT', 'ws-b', 2, 'owner');
    run('SITE_INSERT', 'ws-a', 'site-a1', 'hearth', 'Hearth');
    run('SITE_INSERT', 'ws-b', 'site-b1', 'hearth', 'Hearth B');
    run('REVISION_CHECKPOINT', 'ws-a', 'site-a1', 'rev-a1', H1, 100, 1);
    run('ASSET_INSERT', 'ws-a', 'site-a1', 'asset-a1', H1, 10, 'image/png', 'd1');
    run('TOKEN_INSERT', 'ws-a', 'tok-a1', 1, H1, 'edit', 'laptop', NOW, '2026-12-27T00:00:00Z', NOW);
    run('DEPLOYMENT_INSERT', 'ws-a', 'site-a1', 'dep-a1', 'rev-a1', 1);
  }

  it('applies cleanly from empty and is idempotent', () => {
    const d = freshDb();
    d.exec(readFileSync(MIGRATION, 'utf8'));
    const tables = d.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").all();
    assert.deepEqual(
      tables.map((t) => t.name).sort(),
      ['api_tokens', 'assets', 'deployments', 'memberships', 'site_revisions', 'sites', 'users', 'workspaces'],
    );
    for (const t of tables) {
      const strict = d.prepare('SELECT strict FROM pragma_table_list WHERE name = ?').get(t.name);
      assert.equal(strict.strict, 1, `${t.name} must be STRICT`);
    }
  });

  it('every repository statement prepares against the schema', () => {
    const d = freshDb();
    for (const r of Object.values(Q)) assert.doesNotThrow(() => d.prepare(r.sql), r.name);
  });

  it('tenant-scoped reads return rows in the right workspace only', () => {
    seed();
    assert.equal(all('SITE_GET', 'ws-a', 'site-a1').length, 1);
    assert.equal(all('SITE_GET', 'ws-b', 'site-a1').length, 0);
    assert.equal(all('SITE_BY_SLUG', 'ws-a', 'hearth')[0].id, 'site-a1');
    assert.equal(all('SITE_BY_SLUG', 'ws-b', 'hearth')[0].id, 'site-b1');
    assert.deepEqual(all('SITE_LIST', 'ws-a', 50).map((s) => s.id), ['site-a1']);
    assert.equal(all('MEMBERSHIP_GET', 'ws-b', 1).length, 0);
    assert.equal(all('TOKEN_GET', 'ws-b', 'tok-a1').length, 0);
    assert.equal(all('ASSET_BY_SHA256', 'ws-b', 'site-a1', H1).length, 0);
    assert.equal(all('ASSET_LIST', 'ws-b', 'site-a1', 50).length, 0);
    assert.equal(all('REVISION_GET', 'ws-b', 'site-a1', 'rev-a1').length, 0);
    assert.equal(all('REVISION_LATEST', 'ws-b', 'site-a1').length, 0);
    assert.equal(all('DEPLOYMENT_GET', 'ws-b', 'site-a1', 'dep-a1').length, 0);
    assert.equal(all('DEPLOYMENT_LIST', 'ws-b', 'site-a1', 50).length, 0);
    // Same ids, right workspace, do match.
    assert.equal(all('REVISION_LATEST', 'ws-a', 'site-a1')[0].revision, 1);
    assert.equal(all('DEPLOYMENT_GET', 'ws-a', 'site-a1', 'dep-a1').length, 1);
  });

  it('cross-workspace writes change nothing', () => {
    seed();
    assert.equal(run('TOKEN_REVOKE', 'ws-b', 'tok-a1', NOW).changes, 0);
    assert.equal(run('DEPLOYMENT_SET_STATUS', 'ws-b', 'site-a1', 'dep-a1', 'failed', 'x', NOW, NOW).changes, 0);
    assert.equal(all('TOKEN_GET', 'ws-a', 'tok-a1')[0].revoked_at, null);
    assert.equal(run('TOKEN_REVOKE', 'ws-a', 'tok-a1', NOW).changes, 1);
    assert.equal(run('TOKEN_REVOKE', 'ws-a', 'tok-a1', NOW).changes, 0);
  });

  it('children cannot reference a parent in another workspace (composite FKs)', () => {
    seed();
    assert.throws(() => run('ASSET_INSERT', 'ws-b', 'site-a1', 'asset-x', H2, 1, 'image/png', 'd1'), /FOREIGN KEY/);
    assert.throws(() => run('REVISION_CHECKPOINT', 'ws-b', 'site-a1', 'rev-x', H2, 1, 2), /FOREIGN KEY/);
    assert.throws(() => run('DEPLOYMENT_INSERT', 'ws-b', 'site-b1', 'dep-x', 'rev-a1', 2), /FOREIGN KEY/);
    // Token user must be a member of the same workspace.
    assert.throws(
      () => run('TOKEN_INSERT', 'ws-b', 'tok-x', 1, H2, 'edit', '', NOW, '2026-12-27T00:00:00Z', NOW),
      /FOREIGN KEY/,
    );
  });

  it('slug is unique per workspace, not globally', () => {
    seed();
    assert.throws(() => run('SITE_INSERT', 'ws-a', 'site-a2', 'hearth', 'Dup'), /UNIQUE/);
    assert.doesNotThrow(() => run('SITE_INSERT', 'ws-a', 'site-a2', 'other', 'Other'));
  });

  it('rejects bad roles, scopes, backends and statuses', () => {
    seed();
    run('USER_UPSERT', 3003, 'carol');
    assert.throws(() => run('MEMBERSHIP_INSERT', 'ws-a', 3, 'admin'), /CHECK/);
    assert.throws(
      () => run('TOKEN_INSERT', 'ws-a', 'tok-bad', 1, H2, 'root', '', NOW, '2026-12-27T00:00:00Z', NOW),
      /CHECK/,
    );
    assert.throws(() => run('ASSET_INSERT', 'ws-a', 'site-a1', 'asset-bad', H2, 1, 'image/png', 's3'), /CHECK/);
    assert.throws(() => run('DEPLOYMENT_SET_STATUS', 'ws-a', 'site-a1', 'dep-a1', 'weird', null, NOW, null), /CHECK/);
  });

  it('allows at most one owner per workspace', () => {
    seed();
    run('USER_UPSERT', 3003, 'carol');
    assert.throws(() => run('MEMBERSHIP_INSERT', 'ws-a', 3, 'owner'), /UNIQUE/);
    assert.doesNotThrow(() => run('MEMBERSHIP_INSERT', 'ws-a', 3, 'publisher'));
  });

  it('keys users by the numeric GitHub id; login is mutable display data', () => {
    seed();
    run('USER_UPSERT', 1001, 'alice-renamed');
    const rows = all('USER_BY_GITHUB_ID', 1001);
    assert.equal(rows.length, 1);
    assert.equal(rows[0].github_login, 'alice-renamed');
    assert.equal(rows[0].id, 1);
    assert.throws(() => run('USER_UPSERT', 0, 'zero'), /CHECK/);
  });

  it('stores only a token hash and bounds the renewal chain to 90 days', () => {
    seed();
    const cols = db.prepare('SELECT name FROM pragma_table_info(?)').all('api_tokens').map((c) => c.name);
    assert.ok(cols.includes('token_hash'));
    assert.ok(!cols.includes('token'));
    assert.throws(
      () => run('TOKEN_INSERT', 'ws-a', 'tok-raw', 1, 'not-a-hash', 'edit', '', NOW, '2026-12-27T00:00:00Z', NOW),
      /CHECK/,
    );
    // chain started 2026-08-01; 90-day cap is 2026-10-30.
    assert.throws(
      () => run('TOKEN_INSERT', 'ws-a', 'tok-long', 1, H2, 'edit', '', '2026-08-02T00:00:00Z', '2026-12-27T00:00:00Z', '2026-08-01T00:00:00Z'),
      /CHECK/,
    );
    assert.doesNotThrow(() =>
      run('TOKEN_INSERT', 'ws-a', 'tok-ok', 1, H2, 'publish', '', '2026-08-02T00:00:00Z', '2026-10-30T00:00:00Z', '2026-08-01T00:00:00Z'),
    );
  });

  it('allocates immutable, increasing revision numbers per site', () => {
    seed();
    run('REVISION_CHECKPOINT', 'ws-a', 'site-a1', 'rev-a2', H2, 200, 1);
    assert.equal(all('REVISION_LATEST', 'ws-a', 'site-a1')[0].revision, 2);
    run('SITE_INSERT', 'ws-a', 'site-a2', 'second', 'Second');
    run('REVISION_CHECKPOINT', 'ws-a', 'site-a2', 'rev-s2', H1, 1, 1);
    assert.equal(all('REVISION_LATEST', 'ws-a', 'site-a2')[0].revision, 1);
    assert.throws(() => db.exec("UPDATE site_revisions SET byte_size = 1 WHERE id = 'rev-a1'"), /immutable/);
    // A deployed revision cannot be deleted out from under a deployment.
    assert.throws(() => db.exec("DELETE FROM site_revisions WHERE id = 'rev-a1'"), /FOREIGN KEY/);
  });

  it('deployment status updates stay inside the workspace', () => {
    seed();
    assert.equal(run('DEPLOYMENT_SET_STATUS', 'ws-a', 'site-a1', 'dep-a1', 'succeeded', null, NOW, NOW).changes, 1);
    assert.equal(all('DEPLOYMENT_GET', 'ws-a', 'site-a1', 'dep-a1')[0].status, 'succeeded');
  });
});
