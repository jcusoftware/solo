// SQL-level test for Studio authentication (#65): applies every real D1 migration
// (0001 registry, 0002 asset store, 0003 auth) to an empty in-memory SQLite
// database (node:sqlite, no dependency) and runs the exact auth SQL the Worker
// binds, printed by `cargo run --example dump_sql -- --auth`. Proves the limits
// that must hold under concurrency (10 active tokens per user, pending
// invitations, one successor per rotated token) live inside the inserting
// statement, single use of invitations, that no OAuth state is stored, the
// one-owner bootstrap, owner protection, token workspace binding and cascade on
// membership removal.
import { describe, it, before } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync, readdirSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..', '..');
const MIGRATIONS = resolve(ROOT, 'crates/sws-worker-studio/migrations');
const NOW = '2026-11-27T00:00:00Z';
const LATER = '2026-11-27T00:05:00Z';
const id = (n) => String(n).padStart(24, '0');
const h = (n) => String(n).padStart(64, '0');

function loadSql() {
  const out = execFileSync('cargo', ['run', '-q', '--locked', '-p', 'sws-worker-studio', '--example', 'dump_sql', '--', '--auth'], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  return Object.fromEntries(JSON.parse(out).map((r) => [r.name, r]));
}

function freshDb() {
  const db = new DatabaseSync(':memory:');
  db.exec('PRAGMA foreign_keys = ON');
  for (const f of readdirSync(MIGRATIONS).filter((n) => n.endsWith('.sql')).sort()) {
    db.exec(readFileSync(resolve(MIGRATIONS, f), 'utf8'));
  }
  return db;
}

describe('studio auth migration and statements', () => {
  let Q;
  let db;
  const run = (name, ...p) => db.prepare(Q[name].sql).run(...p);
  const get = (name, ...p) => db.prepare(Q[name].sql).get(...p);
  const all = (name, ...p) => db.prepare(Q[name].sql).all(...p);

  before(() => {
    Q = loadSql();
  });

  function seed() {
    db = freshDb();
    db.exec(`INSERT INTO workspaces (id, name) VALUES ('ws-a','A'),('ws-b','B');
INSERT INTO users (id, github_id, github_login) VALUES (1,1001,'owner'),(2,1002,'editor'),(3,1003,'outsider');
INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-a',1,'owner'),('ws-a',2,'editor'),('ws-b',3,'owner');`);
  }

  const token = (n, over = {}) => {
    const t = { ws: 'ws-a', user: 1, scope: 'edit', created: NOW, expires: '2026-12-27T00:00:00Z', chain: NOW, replaces: '', max: 10, ...over };
    return run('TOKEN_INSERT_LIMITED', t.ws, id(n), t.user, h(n), t.scope, 'lbl', t.created, t.expires, t.chain, t.replaces, t.max);
  };

  it('applies from empty; 0003 is idempotent; new tables are STRICT', () => {
    const d = freshDb();
    d.exec(readFileSync(resolve(MIGRATIONS, '0003_auth.sql'), 'utf8'));
    for (const t of ['auth_sessions', 'invitations']) {
      assert.equal(d.prepare('SELECT strict FROM pragma_table_list WHERE name = ?').get(t).strict, 1, t);
    }
  });

  it('every auth statement prepares against the schema', () => {
    const d = freshDb();
    for (const r of Object.values(Q)) assert.doesNotThrow(() => d.prepare(r.sql), r.name);
  });

  it('sessions: lifetime capped at 14 days, digest-only, touch is monotone, revoke is per user', () => {
    seed();
    run('SESSION_INSERT', id(1), 1, h(1), NOW, '2026-12-11T00:00:00Z');
    assert.throws(() => run('SESSION_INSERT', id(2), 1, h(2), NOW, '2026-12-11T00:00:01Z'), /CHECK/);
    assert.throws(() => run('SESSION_INSERT', id(3), 1, 'not-a-digest', NOW, '2026-12-01T00:00:00Z'), /CHECK/);
    assert.throws(() => run('SESSION_INSERT', 'SHORT', 1, h(4), NOW, '2026-12-01T00:00:00Z'), /CHECK/);
    assert.throws(() => run('SESSION_INSERT', id(5), 1, h(1), NOW, '2026-12-01T00:00:00Z'), /UNIQUE/);
    const s = get('SESSION_GET', id(1));
    assert.equal(s.user_id, 1);
    assert.equal(s.last_seen_at, NOW);
    assert.equal(run('SESSION_TOUCH', id(1), LATER).changes, 1);
    assert.equal(run('SESSION_TOUCH', id(1), NOW).changes, 0, 'never moves backwards');
    // DO revalidation is bound to the socket's user.
    assert.ok(get('SESSION_CHECK', id(1), 1));
    assert.equal(get('SESSION_CHECK', id(1), 2), undefined);
    assert.equal(run('SESSION_REVOKE', id(1), 2, LATER).changes, 0, 'another user cannot revoke it');
    assert.equal(run('SESSION_REVOKE', id(1), 1, LATER).changes, 1);
    assert.equal(run('SESSION_TOUCH', id(1), '2026-11-28T00:00:00Z').changes, 0, 'a revoked session is never touched');
    assert.equal(get('SESSION_GET', id(1)).revoked_at, LATER);
    // Prune removes dead sessions for that user only.
    run('SESSION_INSERT', id(6), 2, h(6), NOW, '2026-12-11T00:00:00Z');
    assert.equal(run('SESSION_PRUNE_USER', 1, LATER, '2026-11-26T00:05:00Z').changes, 1);
    assert.ok(get('SESSION_GET', id(6)));
  });

  it('tokens: the 11th active token is refused inside the INSERT; expired and revoked ones do not count', () => {
    seed();
    for (let i = 1; i <= 10; i++) assert.equal(token(i).changes, 1, `token ${i}`);
    assert.equal(token(11).changes, 0, 'eleventh active token refused');
    assert.equal(get('TOKEN_COUNT_ACTIVE', 1, NOW).n, 10);
    // The limit is per user across workspaces, not per workspace.
    db.exec(`INSERT INTO memberships (workspace_id, user_id, role) VALUES ('ws-b',1,'editor')`);
    assert.equal(token(12, { ws: 'ws-b' }).changes, 0);
    // A revoked token frees a slot.
    assert.equal(run('TOKEN_REVOKE_OWN', id(1), 1, NOW).changes, 1);
    assert.equal(token(13).changes, 1);
    // Rotation at the limit: the token being replaced is excluded from the count.
    assert.equal(token(14, { replaces: id(2) }).changes, 1);
    assert.equal(run('TOKEN_REVOKE_ROTATED', id(2), 1, NOW, id(14)).changes, 1);
    assert.equal(get('TOKEN_COUNT_ACTIVE', 1, NOW).n, 10);
    // A rotation whose successor was not inserted revokes nothing.
    assert.equal(run('TOKEN_REVOKE_ROTATED', id(3), 1, NOW, id(99)).changes, 0);
    // Rotating an already revoked (or another user's, or unknown) token inserts nothing.
    assert.equal(token(15, { replaces: id(2) }).changes, 0, 'old token already rotated');
    assert.equal(token(16, { replaces: id(77) }).changes, 0, 'unknown old token');
    // Expired tokens do not count either.
    const after = '2027-01-01T00:00:00Z';
    assert.equal(get('TOKEN_COUNT_ACTIVE', 1, after).n, 0);
  });

  it('tokens: two concurrent rotations of one token cannot fork its chain', () => {
    seed();
    assert.equal(token(1).changes, 1);
    // Each rotation is one D1 batch (one transaction): insert successor, revoke old.
    const rotate = (n) => {
      db.exec('BEGIN');
      const ins = token(n, { replaces: id(1) }).changes;
      const rev = run('TOKEN_REVOKE_ROTATED', id(1), 1, LATER, id(n)).changes;
      db.exec('COMMIT');
      return [ins, rev];
    };
    assert.deepEqual(rotate(2), [1, 1], 'first rotation wins');
    assert.deepEqual(rotate(3), [0, 0], 'second rotation inserts nothing and revokes nothing');
    assert.equal(get('TOKEN_GET_OWN', id(3), 1), undefined);
    const active = db.prepare("SELECT id FROM api_tokens WHERE user_id = 1 AND revoked_at IS NULL").all();
    assert.deepEqual(active.map((r) => r.id), [id(2)], 'exactly one live successor');
  });

  it('tokens: lifetime CHECKs per scope and chain, workspace binding, listing without digests', () => {
    seed();
    assert.throws(() => token(1, { scope: 'publish', expires: '2026-12-04T00:00:01Z' }), /CHECK/);
    assert.throws(() => token(2, { scope: 'activate', expires: '2026-11-28T00:00:01Z' }), /CHECK/);
    assert.throws(() => token(3, { chain: '2026-09-01T00:00:00Z', expires: '2026-12-01T00:00:00Z' }), /CHECK/, '90-day chain');
    // A token can only exist for a member of its workspace (FK to memberships).
    assert.throws(() => token(4, { ws: 'ws-b', user: 2 }), /FOREIGN KEY/);
    token(5, { scope: 'publish', expires: '2026-12-04T00:00:00Z' });
    const auth = get('TOKEN_AUTH', id(5));
    assert.equal(auth.workspace_id, 'ws-a');
    assert.equal(auth.token_hash, h(5));
    // DO revalidation is bound to workspace and user.
    assert.ok(get('TOKEN_CHECK', 'ws-a', id(5), 1));
    assert.equal(get('TOKEN_CHECK', 'ws-b', id(5), 1), undefined, 'tenant denial');
    assert.equal(get('TOKEN_CHECK', 'ws-a', id(5), 2), undefined);
    const listed = all('TOKEN_LIST_OWN', 1, 10);
    assert.equal(listed.length, 1);
    assert.equal('token_hash' in listed[0], false);
    assert.equal(all('TOKEN_LIST_OWN', 2, 10).length, 0);
    assert.equal(get('TOKEN_GET_OWN', id(5), 2), undefined, 'not your token');
    assert.equal(run('TOKEN_REVOKE_OWN', id(5), 2, NOW).changes, 0);
    assert.equal(run('TOKEN_TOUCH', id(5), LATER).changes, 1);
  });

  it('demotion revokes publish/activate tokens; removal cascades every token; the owner is untouchable', () => {
    seed();
    db.exec(`UPDATE memberships SET role = 'publisher' WHERE workspace_id = 'ws-a' AND user_id = 2`);
    token(1, { user: 2, scope: 'publish', expires: '2026-12-04T00:00:00Z' });
    token(2, { user: 2, scope: 'edit' });
    assert.equal(run('MEMBER_SET_ROLE', 'ws-a', 2, 'editor').changes, 1);
    assert.equal(run('TOKEN_REVOKE_ABOVE_EDIT', 'ws-a', 2, NOW).changes, 1);
    assert.equal(get('TOKEN_AUTH', id(1)).revoked_at, NOW);
    assert.equal(get('TOKEN_AUTH', id(2)).revoked_at, null);
    // Owner protection.
    assert.equal(run('MEMBER_SET_ROLE', 'ws-a', 1, 'editor').changes, 0);
    assert.equal(run('MEMBER_SET_ROLE', 'ws-a', 2, 'owner').changes, 0, 'never grants owner');
    assert.equal(run('MEMBER_REMOVE', 'ws-a', 1).changes, 0);
    // Removal cascades the member's tokens in that workspace.
    assert.equal(run('MEMBER_REMOVE', 'ws-a', 2).changes, 1);
    assert.equal(get('TOKEN_AUTH', id(2)), undefined);
    // Tenant scoping of member statements.
    assert.equal(run('MEMBER_REMOVE', 'ws-b', 1).changes, 0);
    assert.deepEqual(all('MEMBERS_LIST', 'ws-a', 10).map((m) => [m.user_id, m.github_login, m.role]), [[1, 'owner', 'owner']]);
  });

  it('owner bootstrap: only while no owner exists; exactly one owner ever', () => {
    db = freshDb();
    run('BOOTSTRAP_WORKSPACE', 'studio', 'studio');
    run('BOOTSTRAP_WORKSPACE', 'studio', 'other name');
    assert.equal(db.prepare("SELECT name FROM workspaces WHERE id='studio'").get().name, 'studio');
    db.exec(`INSERT INTO users (github_id, github_login) VALUES (7, 'first'), (8, 'second')`);
    assert.equal(run('BOOTSTRAP_OWNER', 'studio', 7).changes, 1);
    assert.equal(run('BOOTSTRAP_OWNER', 'studio', 7).changes, 0, 'idempotent');
    assert.equal(run('BOOTSTRAP_OWNER', 'studio', 8).changes, 0, 'a second owner is impossible');
    assert.equal(run('BOOTSTRAP_OWNER', 'studio', 999).changes, 0, 'unknown github id');
    assert.equal(db.prepare("SELECT COUNT(*) AS n FROM memberships WHERE workspace_id='studio' AND role='owner'").get().n, 1);
    assert.equal(get('USER_MEMBERSHIP_COUNT', 1).n, 1);
    assert.equal(get('USER_MEMBERSHIP_COUNT', 2).n, 0);
  });

  it('invitations: bound to a login, single use, expire, never owner, pending cap inside the INSERT', () => {
    seed();
    const inv = (n, over = {}) => {
      const v = { ws: 'ws-a', login: 'newbie', role: 'editor', by: 1, created: NOW, expires: '2026-12-04T00:00:00Z', max: 50, ...over };
      return run('INVITE_INSERT_LIMITED', v.ws, id(n), h(n), v.login, v.role, v.by, v.created, v.expires, v.max);
    };
    assert.throws(() => inv(1, { role: 'owner' }), /CHECK/);
    assert.throws(() => inv(2, { login: 'NewBie' }), /CHECK/, 'stored lowercase');
    assert.throws(() => inv(3, { expires: '2026-12-04T00:00:01Z' }), /CHECK/, '7-day cap');
    assert.equal(inv(4).changes, 1);
    assert.equal(inv(5, { max: 1 }).changes, 0, 'pending cap');
    // Accept: wrong login, then the right one, then a replay.
    db.exec(`INSERT INTO users (github_id, github_login) VALUES (5005, 'newbie'), (6006, 'intruder')`);
    assert.equal(run('INVITE_ACCEPT', id(4), LATER, 6006, 'intruder').changes, 0);
    assert.equal(run('INVITE_ACCEPT', id(4), LATER, 5005, 'newbie').changes, 1);
    assert.equal(run('INVITE_MEMBERSHIP', id(4), LATER, 5005).changes, 1);
    assert.equal(run('INVITE_ACCEPT', id(4), LATER, 5005, 'newbie').changes, 0, 'single use');
    assert.equal(run('INVITE_MEMBERSHIP', id(4), LATER, 6006).changes, 0, 'membership only for the acceptor');
    const m = db.prepare("SELECT role FROM memberships m JOIN users u ON u.id=m.user_id WHERE u.github_id=5005 AND m.workspace_id='ws-a'").get();
    assert.equal(m.role, 'editor');
    // Expired and revoked invitations cannot be accepted; accepted ones cannot be revoked.
    inv(6, { login: 'late' });
    assert.equal(run('INVITE_ACCEPT', id(6), '2026-12-04T00:00:00Z', 5005, 'late').changes, 0, 'expired');
    assert.equal(run('INVITE_REVOKE', 'ws-a', id(6), LATER).changes, 1);
    assert.equal(run('INVITE_ACCEPT', id(6), LATER, 5005, 'late').changes, 0, 'revoked');
    assert.equal(run('INVITE_REVOKE', 'ws-a', id(4), LATER).changes, 0, 'already accepted');
    assert.equal(run('INVITE_REVOKE', 'ws-b', id(6), LATER).changes, 0, 'tenant scoped');
    const list = all('INVITE_LIST', 'ws-a', 10);
    assert.equal(list.length, 2);
    assert.equal('secret_hash' in list[0], false);
    assert.equal(all('INVITE_LIST', 'ws-b', 10).length, 0);
  });

  it('no server-side OAuth state: no table and no statement for it (sign-in start writes nothing)', () => {
    const d = freshDb();
    assert.equal(d.prepare("SELECT name FROM sqlite_master WHERE name LIKE '%oauth%'").all().length, 0);
    for (const r of Object.values(Q)) assert.doesNotMatch(r.sql, /oauth_state/i, r.name);
  });

  it('users: login update only when it changed; workspaces listing', () => {
    seed();
    assert.equal(run('USER_UPDATE_LOGIN', 1001, 'owner', NOW).changes, 0);
    assert.equal(run('USER_UPDATE_LOGIN', 1001, 'renamed', NOW).changes, 1);
    assert.equal(get('USER_GET', 1).github_login, 'renamed');
    assert.deepEqual(all('USER_WORKSPACES', 1, 10).map((r) => [r.workspace_id, r.role]), [['ws-a', 'owner']]);
  });
});
