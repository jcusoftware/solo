#!/usr/bin/env node
// Wasm toolchain pin / drift check (zero dependencies, tenant-agnostic).
//
// Static (default, no network, no Rust tools needed): wasm-toolchain.json and
// rust-toolchain.toml are the single source of truth. Fails when
//   - a CI workflow / the devcontainer / package.json / wrangler.jsonc pins a
//     different Rust channel, worker-build or wasm-bindgen-cli version, or uses
//     a floating pin (`dtolnay/rust-toolchain@stable`, `worker-build@^0.9`);
//   - a `wasm-bindgen-cli@X` pin differs from the wasm-bindgen crate in Cargo.lock.
// --installed: additionally compares the tools on this machine with the pins
//   (rustc, worker-build, wasm-bindgen CLI if present, Binaryen wasm-opt that
//   worker-build downloaded). --strict-binaryen makes a missing wasm-opt an error.
//
// Usage: node core/tools/check-wasm-toolchain.mjs [--installed] [--strict-binaryen] [--root <dir>]
// Exit: 0 = OK, 1 = drift, 2 = usage / read error.

import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const SEMVER = /^\d+\.\d+\.\d+$/;

export function parseLockVersion(lock, crate) {
  const re = new RegExp(`\\[\\[package\\]\\]\\r?\\nname = "${crate}"\\r?\\nversion = "([^"]+)"`, 'g');
  const found = [...lock.matchAll(re)].map((m) => m[1]);
  if (found.length !== 1) throw new Error(`expected exactly one ${crate} in Cargo.lock, found ${found.length}`);
  return found[0];
}

export function parseChannel(toml) {
  const m = toml.match(/^\s*channel\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error('rust-toolchain.toml has no channel');
  return m[1];
}

// Returns problems found in one file's text against the pins.
export function checkPins(name, text, pins) {
  const problems = [];
  if (/dtolnay\/rust-toolchain@stable/.test(text)) {
    problems.push(`${name}: floating dtolnay/rust-toolchain@stable (use @master with toolchain: ${pins.rust})`);
  }
  for (const m of text.matchAll(/^\s*toolchain:\s*['"]?([^\s'"]+)/gm)) {
    if (m[1] !== pins.rust) problems.push(`${name}: toolchain ${m[1]} != pinned Rust ${pins.rust}`);
  }
  for (const m of text.matchAll(/rustup toolchain install\s+([^\s'"]+)/g)) {
    if (m[1] !== pins.rust) problems.push(`${name}: rustup toolchain install ${m[1]} != pinned Rust ${pins.rust}`);
  }
  for (const m of text.matchAll(/worker-build@([^\s'"`,)]+)/g)) {
    if (m[1] !== pins.workerBuild) problems.push(`${name}: worker-build@${m[1]} != pinned ${pins.workerBuild}`);
  }
  for (const m of text.matchAll(/wasm-bindgen-cli@([^\s'"`,)]+)/g)) {
    if (m[1] !== pins.wasmBindgen) problems.push(`${name}: wasm-bindgen-cli@${m[1]} != Cargo.lock wasm-bindgen ${pins.wasmBindgen}`);
  }
  return problems;
}

const PIN_FILES = [
  '.github/workflows/ci.yml',
  '.github/workflows/release.yml',
  '.devcontainer/devcontainer.json',
  'package.json',
  'wrangler.jsonc',
];

export async function loadPins(root) {
  const config = JSON.parse(await readFile(join(root, 'wasm-toolchain.json'), 'utf8'));
  const rust = parseChannel(await readFile(join(root, 'rust-toolchain.toml'), 'utf8'));
  const wasmBindgen = parseLockVersion(await readFile(join(root, 'Cargo.lock'), 'utf8'), 'wasm-bindgen');
  return { config, rust, wasmBindgen, workerBuild: config.workerBuild, binaryen: config.binaryen };
}

export async function checkStatic(root) {
  const pins = await loadPins(root);
  const problems = [];
  if (!SEMVER.test(pins.rust)) problems.push(`rust-toolchain.toml: channel ${pins.rust} must be an exact x.y.z version`);
  if (!SEMVER.test(pins.workerBuild)) problems.push(`wasm-toolchain.json: workerBuild ${pins.workerBuild} must be an exact x.y.z version`);
  if (!/^\d+$/.test(String(pins.binaryen))) problems.push(`wasm-toolchain.json: binaryen ${pins.binaryen} must be a release number`);
  for (const f of PIN_FILES) {
    const p = join(root, f);
    if (!existsSync(p)) continue;
    problems.push(...checkPins(f, await readFile(p, 'utf8'), pins));
  }
  return { pins, problems };
}

function run(cmd, args) {
  return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

function findWasmOpt() {
  if (process.env.WASM_OPT_BIN) return process.env.WASM_OPT_BIN;
  // worker-build caches its download at <cache_dir>/worker-build/wasm-opt-<target>-<version>/bin/wasm-opt.
  const cacheDir = process.env.XDG_CACHE_HOME
    || (process.platform === 'darwin' ? join(homedir(), 'Library', 'Caches') : join(homedir(), '.cache'));
  const base = join(cacheDir, 'worker-build');
  if (!existsSync(base)) return null;
  const dir = readdirSync(base).find((d) => d.startsWith('wasm-opt-'));
  return dir ? join(base, dir, 'bin', 'wasm-opt') : null;
}

export function checkInstalled(pins, { strictBinaryen = false } = {}) {
  const problems = [];
  const notes = [];
  const probe = (label, fn, expected) => {
    try {
      const got = fn();
      if (got !== expected) problems.push(`${label}: installed ${got} != pinned ${expected}`);
      else notes.push(`${label} ${got}`);
    } catch (e) {
      problems.push(`${label}: not runnable (${e.message.split('\n')[0]})`);
    }
  };
  probe('rustc', () => run('rustc', ['--version']).split(/\s+/)[1], pins.rust);
  probe('worker-build', () => run('worker-build', ['--version']).split(/\s+/).pop(), pins.workerBuild);
  try {
    const v = run('wasm-bindgen', ['--version']).split(/\s+/)[1];
    if (v !== pins.wasmBindgen) problems.push(`wasm-bindgen CLI on PATH: ${v} != Cargo.lock ${pins.wasmBindgen}`);
    else notes.push(`wasm-bindgen CLI ${v}`);
  } catch {
    notes.push('wasm-bindgen CLI not on PATH (worker-build downloads the Cargo.lock version itself)');
  }
  const opt = findWasmOpt();
  if (!opt || !existsSync(opt)) {
    const msg = 'wasm-opt (Binaryen) not downloaded yet; it appears after the first worker-build release build';
    if (strictBinaryen) problems.push(msg);
    else notes.push(msg);
  } else {
    probe('wasm-opt (Binaryen)', () => run(opt, ['--version']).match(/version (\d+)/)?.[1], String(pins.binaryen));
  }
  return { problems, notes };
}

async function main(argv) {
  let root = process.cwd();
  const flags = new Set();
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--root') root = resolve(argv[++i] ?? '');
    else if (['--installed', '--strict-binaryen'].includes(argv[i])) flags.add(argv[i]);
    else {
      console.error(`unknown argument ${argv[i]}`);
      return 2;
    }
  }
  let result;
  try {
    result = await checkStatic(root);
  } catch (e) {
    console.error(`check-wasm-toolchain: ${e.message}`);
    return 2;
  }
  const { pins } = result;
  const problems = [...result.problems];
  console.log(`pins: rust ${pins.rust}, worker-build ${pins.workerBuild}, Binaryen ${pins.binaryen}, wasm-bindgen ${pins.wasmBindgen} (Cargo.lock)`);
  if (flags.has('--installed')) {
    const inst = checkInstalled(pins, { strictBinaryen: flags.has('--strict-binaryen') });
    for (const n of inst.notes) console.log(`ok: ${n}`);
    problems.push(...inst.problems);
  }
  if (problems.length) {
    for (const p of problems) console.error(`DRIFT: ${p}`);
    return 1;
  }
  console.log('Wasm toolchain pins OK');
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  process.exit(await main(process.argv.slice(2)));
}
