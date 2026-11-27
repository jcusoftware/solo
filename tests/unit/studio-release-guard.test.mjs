// Unit tests for scripts/check-studio-release.mjs (the insecure-dev-auth release guard, #62).
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import {
  ALLOWED_SCRIPTS,
  DEV_MARKERS,
  checkArtifact,
  checkSessionsConfig,
  checkStatic,
  defaultFeatures,
  enabledByDefault,
  featureTable,
  enablesOnDependency,
  scanCi,
  scanPackageScripts,
  wasmHasDevAuth,
} from '../../scripts/check-studio-release.mjs';

const ROOT = resolve(import.meta.dirname, '..', '..');

/** A minimal repository for checkStatic/checkArtifact (ported from the former
 *  tests/unit/studio-no-dev-auth.test.mjs, #63). */
function fakeRepo({ defaults = '"console_error_panic_hook"', scripts = {}, wf = '', wasm = null } = {}) {
  const r = mkdtempSync(join(tmpdir(), 'studio-release-guard-'));
  mkdirSync(join(r, 'crates/sws-worker-studio/build'), { recursive: true });
  mkdirSync(join(r, '.github/workflows'), { recursive: true });
  mkdirSync(join(r, 'deploy'), { recursive: true });
  writeFileSync(join(r, 'crates/sws-worker-studio/Cargo.toml'), `[features]\ndefault = [${defaults}]\ninsecure-dev-auth = []\n`);
  writeFileSync(join(r, 'package.json'), JSON.stringify({ scripts }));
  writeFileSync(join(r, '.github/workflows/ci.yml'), wf);
  writeFileSync(join(r, 'wrangler.studio.jsonc'), '{ "main": "crates/sws-worker-studio/build/index.js" }\n');
  if (wasm) writeFileSync(join(r, 'crates/sws-worker-studio/build/index_bg.wasm'), wasm);
  return r;
}

describe('studio release guard', () => {
  it('the repository passes its own static checks', () => {
    assert.deepEqual(checkStatic(ROOT), []);
  });

  it('parses default features', () => {
    assert.deepEqual(defaultFeatures('[features]\ndefault = ["a", "b"]\nx = []\n'), ['a', 'b']);
    assert.deepEqual(defaultFeatures('[features]\nx = []\n'), []);
    assert.ok(defaultFeatures('default = ["console_error_panic_hook", "insecure-dev-auth"]').includes('insecure-dev-auth'));
  });

  it('flags a dependency that turns the feature on, but not the feature definition', () => {
    assert.equal(enablesOnDependency('insecure-dev-auth = []\n'), false);
    assert.equal(enablesOnDependency('# features = ["insecure-dev-auth"]\n'), false);
    assert.equal(
      enablesOnDependency('sws-worker-studio = { path = "x", features = ["insecure-dev-auth"] }\n'),
      true,
    );
  });

  it('flags multi-line features arrays and crate/feature members in other manifests', () => {
    assert.equal(
      enablesOnDependency('[dependencies.sws-worker-studio]\npath = "x"\nfeatures = [\n  "serde",\n  "insecure-dev-auth", # dev\n]\n'),
      true,
    );
    assert.equal(
      enablesOnDependency('sws-worker-studio = { path = "x", features = [\n"a",\n"insecure-dev-auth"] }\n'),
      true,
    );
    assert.equal(enablesOnDependency('[features]\ndev = ["sws-worker-studio/insecure-dev-auth"]\n'), true);
    assert.equal(enablesOnDependency('[features]\ndefault = ["x", "sws-worker-studio?/insecure-dev-auth"]\n'), true);
    // Not flagged: comments, unrelated features, the definition itself, a path mentioning no feature.
    assert.equal(enablesOnDependency('# dev = ["sws-worker-studio/insecure-dev-auth"]\n'), false);
    assert.equal(enablesOnDependency('x = { path = "y", features = [\n"serde"\n] }\ninsecure-dev-auth = []\n'), false);
    assert.equal(enablesOnDependency('[features]\ndefault = ["x"]\ninsecure-dev-auth = []\n'), false);
  });

  it('only the session test and clippy scripts may mention the feature', () => {
    assert.deepEqual(scanPackageScripts({ 'studio:build': 'node b.mjs', 'lint:clippy': 'cargo clippy --features insecure-dev-auth' }), []);
    assert.equal(scanPackageScripts({ 'studio:build': 'node b.mjs --features insecure-dev-auth' }).length, 1);
    assert.equal(scanPackageScripts({ deploy: 'wrangler deploy --var x insecure-dev-auth' }).length, 1);
    assert.ok(ALLOWED_SCRIPTS.has('test:studio:sessions'));
  });

  it('CI may mention the feature only on cargo clippy/test lines', () => {
    assert.deepEqual(scanCi('run: cargo clippy -p x --features insecure-dev-auth', 'ci.yml'), []);
    assert.equal(scanCi('run: npm run studio:build -- --features insecure-dev-auth', 'ci.yml').length, 1);
    assert.deepEqual(scanCi('run: npm run studio:build', 'ci.yml'), []);
    // The words alone are not enough: it must BE a cargo clippy/test invocation.
    assert.equal(scanCi('run: echo cargo test --features insecure-dev-auth', 'ci.yml').length, 1);
    assert.equal(scanCi('run: npm run studio:build && cargo test --features insecure-dev-auth', 'ci.yml').length, 1);
    assert.equal(scanCi('run: wrangler deploy # cargo clippy insecure-dev-auth', 'ci.yml').length, 1);
    assert.deepEqual(scanCi('  - run: cargo test -p sws-worker-studio --features insecure-dev-auth', 'ci.yml'), []);
    assert.deepEqual(scanCi('        cargo +nightly clippy --features insecure-dev-auth', 'ci.yml'), []);
  });

  it('resolves feature aliases, so default = ["dev"] cannot hide the dev-auth feature', () => {
    const aliased = '[features]\ndefault = ["panic", "dev"]\npanic = []\ndev = [\n  "other", # note\n  "insecure-dev-auth",\n]\ninsecure-dev-auth = []\n';
    assert.deepEqual(featureTable(aliased).dev, ['other', 'insecure-dev-auth']);
    assert.ok(enabledByDefault(aliased).includes('insecure-dev-auth'));
    const chained = '[features]\ndefault = ["a"]\na = ["b"]\nb = ["insecure-dev-auth"]\ninsecure-dev-auth = []\n';
    assert.ok(enabledByDefault(chained).includes('insecure-dev-auth'));
    const dep = '[features]\ndefault = ["a"]\na = ["sws-worker-studio/insecure-dev-auth"]\n';
    assert.ok(enabledByDefault(dep).includes('insecure-dev-auth'));
    assert.ok(!enabledByDefault('[features]\ndefault = ["x"]\nx = []\ninsecure-dev-auth = []\n').includes('insecure-dev-auth'));
    assert.ok(!enabledByDefault(readFileSync(resolve(ROOT, 'crates/sws-worker-studio/Cargo.toml'), 'utf8')).includes('insecure-dev-auth'));
  });

  it('the dev-auth test config must close workers.dev and must not set the fence var', () => {
    assert.deepEqual(checkSessionsConfig('{ "workers_dev": false }'), []);
    assert.equal(checkSessionsConfig('{ "workers_dev": true }').length, 1);
    assert.equal(checkSessionsConfig('{ }').length, 1);
    assert.equal(checkSessionsConfig('{ "workers_dev": false, "vars": { "SWS_INSECURE_DEV_AUTH": "1" } }').length, 1);
    // A comment may explain the var without setting it.
    assert.deepEqual(checkSessionsConfig('// SWS_INSECURE_DEV_AUTH is passed by --var\n{ "workers_dev": false }'), []);
  });

  it('detects the dev header name in Wasm bytes', () => {
    assert.equal(wasmHasDevAuth(Buffer.from('\0\0X-Sws-Dev-User\0')), true);
    assert.equal(wasmHasDevAuth(Buffer.from('\0\0x-sws-other\0')), false);
    assert.equal(wasmHasDevAuth(Buffer.from('\0SWS_INSECURE_DEV_AUTH\0')), true, 'the fence var');
    assert.equal(wasmHasDevAuth(Buffer.from('\0/api/dev/revoke\0')), true, 'the test-only routes');
  });

  it('detects every dev header name (#62 and #63) and the feature name in Wasm bytes', () => {
    for (const m of [
      'X-Sws-Dev-User',
      'X-Sws-Dev-Now',
      'X-Sws-Dev-Commit-Skew',
      'X-Sws-Dev-Batch',
      'X-Sws-Dev-Fail-Put',
      'X-Sws-Dev-Fail-Batch-After-Commit',
      'X-Sws-Dev-Hold-Slot',
      'insecure-dev-auth',
    ]) {
      assert.ok(DEV_MARKERS.includes(m), m);
      assert.equal(wasmHasDevAuth(Buffer.from(`..${m}..`)), true, m);
    }
  });

  // Ported from the former tests/unit/studio-no-dev-auth.test.mjs (#63).
  it('flags the feature as a default, in release scripts, in workflows, and in the release Wasm', () => {
    assert.match(checkStatic(fakeRepo({ defaults: '"insecure-dev-auth"' }))[0], /default features/);
    assert.match(checkStatic(fakeRepo({ scripts: { 'studio:build': 'x --features insecure-dev-auth' } }))[0], /studio:build/);
    assert.match(checkStatic(fakeRepo({ wf: 'run: worker-build --features insecure-dev-auth' }))[0], /outside a cargo clippy\/test/);
    assert.match(checkArtifact(fakeRepo({ wasm: Buffer.from('..X-Sws-Dev-User..') }))[0], /dev-auth marker/);
    assert.match(checkArtifact(fakeRepo({ wasm: Buffer.from('..X-Sws-Dev-Hold-Slot..') }))[0], /dev-auth marker/);
  });

  it('allows the test scripts, clippy lines and a clean Wasm; a missing Wasm fails only when required', () => {
    const r = fakeRepo({
      scripts: {
        'test:studio:assets': 'node --test x --features insecure-dev-auth',
        'test:studio:sessions': 'node x --features insecure-dev-auth',
        'lint:clippy': 'cargo clippy --features insecure-dev-auth',
      },
      wf: 'run: cargo clippy --features insecure-dev-auth',
      wasm: Buffer.from('clean'),
    });
    assert.deepEqual(checkStatic(r), []);
    assert.deepEqual(checkArtifact(r, { require: true }), []);
    assert.deepEqual(checkArtifact(fakeRepo()), []);
    assert.match(checkArtifact(fakeRepo(), { require: true })[0], /missing/);
  });
});
