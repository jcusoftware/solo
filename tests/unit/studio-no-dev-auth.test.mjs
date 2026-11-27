// Guard test: `insecure-dev-auth` (test-only auth bypass header) must not reach a release build.
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { check } from '../../scripts/check-studio-no-dev-auth.mjs';

const ROOT = resolve(import.meta.dirname, '..', '..');

function fakeRepo({ defaults = '"console_error_panic_hook"', scripts = {}, wf = '', wasm = null } = {}) {
  const r = mkdtempSync(join(tmpdir(), 'no-dev-auth-'));
  mkdirSync(join(r, 'crates/sws-worker-studio/build'), { recursive: true });
  mkdirSync(join(r, '.github/workflows'), { recursive: true });
  writeFileSync(join(r, 'crates/sws-worker-studio/Cargo.toml'), `[features]\ndefault = [${defaults}]\ninsecure-dev-auth = []\n`);
  writeFileSync(join(r, 'package.json'), JSON.stringify({ scripts }));
  writeFileSync(join(r, '.github/workflows/ci.yml'), wf);
  if (wasm) writeFileSync(join(r, 'crates/sws-worker-studio/build/index_bg.wasm'), wasm);
  return r;
}

describe('insecure-dev-auth guard', () => {
  it('passes on this repository', () => {
    assert.deepEqual(check(ROOT), []);
  });

  it('flags the feature as a default, in release scripts, in workflows, and in the release Wasm', () => {
    assert.match(check(fakeRepo({ defaults: '"insecure-dev-auth"' }))[0], /default feature/);
    assert.match(check(fakeRepo({ scripts: { 'studio:build': 'x --features insecure-dev-auth' } }))[0], /studio:build/);
    assert.match(check(fakeRepo({ wf: 'run: worker-build --features insecure-dev-auth' }))[0], /outside a clippy line/);
    assert.match(check(fakeRepo({ wasm: Buffer.from('..X-Sws-Dev-User..') }))[0], /embeds/);
  });

  it('allows the test script, clippy lines and a clean Wasm', () => {
    const r = fakeRepo({
      scripts: { 'test:studio:assets': 'node --test x --features insecure-dev-auth', 'lint:clippy': 'cargo clippy --features insecure-dev-auth' },
      wf: 'run: cargo clippy --features insecure-dev-auth',
      wasm: Buffer.from('clean'),
    });
    assert.deepEqual(check(r, { requireWasm: true }), []);
    assert.match(check(fakeRepo(), { requireWasm: true })[0], /missing/);
  });
});
