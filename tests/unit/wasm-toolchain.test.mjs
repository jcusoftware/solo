import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checkFeatures, parseTargetFeatures } from '../../core/tools/check-wasm-features.mjs';
import { checkPins, parseChannel, parseLockVersion } from '../../core/tools/check-wasm-toolchain.mjs';

const pins = { rust: '1.100.0', workerBuild: '0.9.0', wasmBindgen: '0.2.130' };

test('parseLockVersion reads exactly one crate', () => {
  const lock = '[[package]]\nname = "wasm-bindgen"\nversion = "0.2.130"\n\n[[package]]\nname = "wasm-bindgen-futures"\nversion = "0.4.80"\n';
  assert.equal(parseLockVersion(lock, 'wasm-bindgen'), '0.2.130');
  assert.throws(() => parseLockVersion(lock, 'missing'));
});

test('parseChannel reads rust-toolchain.toml', () => {
  assert.equal(parseChannel('[toolchain]\nchannel = "1.100.0"\n'), '1.100.0');
});

test('checkPins accepts matching pins and flags drift', () => {
  const good = 'toolchain: 1.100.0\nrun: cargo install worker-build@0.9.0 --locked\ntool: wasm-bindgen-cli@0.2.130\n';
  assert.deepEqual(checkPins('f', good, pins), []);
  assert.equal(checkPins('f', 'uses: dtolnay/rust-toolchain@stable', pins).length, 1);
  assert.equal(checkPins('f', 'worker-build@^0.9 --locked', pins).length, 1);
  assert.equal(checkPins('f', 'wasm-bindgen-cli@0.2.99', pins).length, 1);
  assert.equal(checkPins('f', '  toolchain: 1.99.0', pins).length, 1);
  assert.deepEqual(checkPins('f', '"check:toolchain:installed": "node x"', pins), []);
});

// Minimal module: magic, version, custom section "target_features" with the given entries.
function wasmWith(features) {
  const enc = new TextEncoder();
  const payload = [features.length];
  for (const [prefix, name] of features) {
    payload.push(prefix.charCodeAt(0), name.length, ...enc.encode(name));
  }
  const name = enc.encode('target_features');
  const body = [name.length, ...name, ...payload];
  return new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, 0, body.length, ...body]);
}

test('parseTargetFeatures decodes prefix and name', () => {
  const m = new WebAssembly.Module(wasmWith([['+', 'sign-ext'], ['-', 'simd128']]));
  const [section] = WebAssembly.Module.customSections(m, 'target_features');
  assert.deepEqual(parseTargetFeatures(section), [
    { prefix: '+', name: 'sign-ext' },
    { prefix: '-', name: 'simd128' },
  ]);
});

test('checkFeatures enforces the allowlist and section presence', () => {
  const allowed = new Set(['sign-ext']);
  assert.deepEqual(checkFeatures(wasmWith([['+', 'sign-ext']]), allowed).problems, []);
  assert.equal(checkFeatures(wasmWith([['+', 'sign-ext'], ['+', 'simd128']]), allowed).problems.length, 1);
  assert.equal(checkFeatures(wasmWith([['-', 'simd128']]), allowed).problems.length, 0);
  assert.equal(checkFeatures(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]), allowed).problems.length, 1);
});
