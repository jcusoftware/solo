// Unit tests for scripts/check-studio-release.mjs (the insecure-dev-auth release guard, #62).
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import {
  ALLOWED_SCRIPTS,
  checkStatic,
  defaultFeatures,
  enablesOnDependency,
  scanCi,
  scanPackageScripts,
  wasmHasDevAuth,
} from '../../scripts/check-studio-release.mjs';

const ROOT = resolve(import.meta.dirname, '..', '..');

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
  });

  it('detects the dev header name in Wasm bytes', () => {
    assert.equal(wasmHasDevAuth(Buffer.from('\0\0X-Sws-Dev-User\0')), true);
    assert.equal(wasmHasDevAuth(Buffer.from('\0\0x-sws-other\0')), false);
  });
});
