#!/usr/bin/env node
// Cached Worker Wasm build (dev or size-optimized release), zero dependencies.
//
// Hashes only the Rust inputs of the Worker crate (root manifests, lockfile,
// toolchain pins, the crate and its path dependencies). When the hash and
// profile match the last successful build and the artifacts exist, Cargo and
// worker-build are NOT invoked, so frontend-only edits (static assets, sites,
// JS/CSS/HTML) cost nothing. After a real build it verifies the pinned tools
// and the Wasm target features of the artifacts.
//
// Usage: node scripts/build-wasm.mjs --profile dev|release [--force] [--crate crates/sws-worker-site]
// Stamp: target/wasm-build/<crate-name>-stamp.json, one per Worker crate so switching
// Workers does not force rebuilds (gitignored with target/).

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { checkInstalled, loadPins } from '../core/tools/check-wasm-toolchain.mjs';
import { checkFiles } from '../core/tools/check-wasm-features.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SKIP_DIRS = new Set(['build', 'target', 'node_modules', 'pkg', '.git']);
const ROOT_INPUTS = ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'wasm-toolchain.json', '.cargo/config.toml'];

function walk(dir, out) {
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) walk(join(dir, entry.name), out);
    } else if (entry.isFile()) {
      out.push(join(dir, entry.name));
    }
  }
}

// The crate plus every `path = "..."` dependency, transitively.
export function crateClosure(root, crateDir) {
  const seen = new Set();
  const queue = [resolve(root, crateDir)];
  while (queue.length) {
    const dir = queue.pop();
    if (seen.has(dir)) continue;
    seen.add(dir);
    const manifest = readFileSync(join(dir, 'Cargo.toml'), 'utf8');
    for (const m of manifest.matchAll(/path\s*=\s*"([^"]+)"/g)) queue.push(resolve(dir, m[1]));
  }
  return [...seen].sort();
}

export function inputHash(root, crateDir) {
  const files = [];
  for (const f of ROOT_INPUTS) if (existsSync(join(root, f))) files.push(join(root, f));
  for (const dir of crateClosure(root, crateDir)) walk(dir, files);
  const h = createHash('sha256');
  for (const f of files) {
    h.update(relative(root, f)).update('\0').update(readFileSync(f)).update('\0');
  }
  return h.digest('hex');
}

function size(p) {
  return `${(statSync(p).size / 1024).toFixed(0)} KiB`;
}

function main(argv) {
  let profile = null;
  let force = false;
  let crateDir = 'crates/sws-worker-site';
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--profile') profile = argv[++i];
    else if (argv[i] === '--force') force = true;
    else if (argv[i] === '--crate') crateDir = argv[++i];
    else {
      console.error(`unknown argument ${argv[i]}`);
      return 2;
    }
  }
  if (!['dev', 'release'].includes(profile)) {
    console.error('usage: build-wasm.mjs --profile dev|release [--force] [--crate dir]');
    return 2;
  }

  const crateAbs = resolve(ROOT, crateDir);
  const outputs = [join(crateAbs, 'build', 'index.js'), join(crateAbs, 'build', 'index_bg.wasm')];
  const stampPath = join(ROOT, 'target', 'wasm-build', `${basename(crateAbs)}-stamp.json`);
  const hash = inputHash(ROOT, crateDir);

  if (!force && existsSync(stampPath) && outputs.every(existsSync)) {
    const stamp = JSON.parse(readFileSync(stampPath, 'utf8'));
    if (stamp.profile === profile && stamp.hash === hash && stamp.crate === crateDir) {
      console.log(`wasm ${profile} build is up to date (Rust inputs unchanged, ${size(outputs[1])}); skipping Cargo`);
      return 0;
    }
  }

  const build = spawnSync('worker-build', [profile === 'release' ? '--release' : '--dev'], {
    cwd: crateAbs,
    stdio: 'inherit',
  });
  if (build.status !== 0) {
    console.error(`worker-build --${profile} failed (${build.error ? build.error.message : `exit ${build.status}`})`);
    return build.status || 1;
  }

  return loadPins(ROOT).then((pins) => {
    const { config } = pins;
    const problems = checkInstalled(pins, { strictBinaryen: profile === 'release' }).problems;
    const targetDir = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(ROOT, 'target');
    const crateName = readFileSync(join(crateAbs, 'Cargo.toml'), 'utf8').match(/^name\s*=\s*"([^"]+)"/m)?.[1]?.replaceAll('-', '_');
    const cargoWasm = join(targetDir, 'wasm32-unknown-unknown', profile === 'release' ? 'release' : 'debug', `${crateName}.wasm`);
    const stages = [
      ['cargo', [cargoWasm]],
      ['final', [outputs[1]]],
    ];
    for (const [stage, files] of stages) {
      for (const r of checkFiles(files.filter(existsSync), config, stage)) {
        console.log(`features (${stage}) ${relative(ROOT, r.file)}: ${r.features.join(', ')}`);
        problems.push(...r.problems.map((p) => `${r.file}: ${p}`));
      }
    }
    if (problems.length) {
      for (const p of problems) console.error(`TOOLCHAIN: ${p}`);
      return 1;
    }
    mkdirSync(dirname(stampPath), { recursive: true });
    writeFileSync(stampPath, JSON.stringify({ profile, hash, crate: crateDir }, null, 2));
    console.log(`wasm ${profile} build OK: ${relative(ROOT, outputs[1])} ${size(outputs[1])}`);
    return 0;
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  process.exit(await main(process.argv.slice(2)));
}
