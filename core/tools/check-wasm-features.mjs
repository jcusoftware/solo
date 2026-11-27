#!/usr/bin/env node
// Wasm target-feature check (zero dependencies, tenant-agnostic).
//
// Reads the `target_features` custom section of each given .wasm and fails when
//   - the module does not validate,
//   - the section is missing (wasm-bindgen / wasm-opt need it), or
//   - a feature outside the allowlist from wasm-toolchain.json is used
//     (e.g. simd128, atomics, relaxed-simd, tail-call, gc, extended-const).
// --stage cargo     : raw cargo output, allowlist = wasmFeatures.cargo
// --stage final     : worker-build output, allowlist = cargo + wasmFeatures.optimizer (default)
//
// Usage: node core/tools/check-wasm-features.mjs [--stage cargo|final] [--config wasm-toolchain.json] <file.wasm>...
// Exit: 0 = OK, 1 = violation, 2 = usage / read error.

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

function leb(bytes, pos) {
  let result = 0;
  let shift = 0;
  for (;;) {
    const b = bytes[pos.i++];
    if (b === undefined) throw new Error('truncated target_features section');
    result |= (b & 0x7f) << shift;
    if (!(b & 0x80)) return result;
    shift += 7;
  }
}

// Parses the tool-conventions "target_features" payload into [{prefix, name}].
export function parseTargetFeatures(payload) {
  const bytes = new Uint8Array(payload);
  const pos = { i: 0 };
  const count = leb(bytes, pos);
  const out = [];
  for (let n = 0; n < count; n++) {
    const prefix = String.fromCharCode(bytes[pos.i++]);
    const len = leb(bytes, pos);
    const name = new TextDecoder().decode(bytes.subarray(pos.i, pos.i + len));
    pos.i += len;
    out.push({ prefix, name });
  }
  return out;
}

export function checkFeatures(buffer, allowed) {
  const problems = [];
  if (!WebAssembly.validate(buffer)) return { features: [], problems: ['module does not validate'] };
  const sections = WebAssembly.Module.customSections(new WebAssembly.Module(buffer), 'target_features');
  if (sections.length !== 1) {
    return { features: [], problems: [`expected one target_features section, found ${sections.length}`] };
  }
  const entries = parseTargetFeatures(sections[0]);
  const features = entries.filter((e) => e.prefix === '+').map((e) => e.name).sort();
  for (const e of entries) {
    if (e.prefix === '=') problems.push(`feature ${e.name} is marked required-by-all ('='), not allowed`);
  }
  for (const f of features) {
    if (!allowed.has(f)) problems.push(`feature ${f} is not in the allowlist`);
  }
  return { features, problems };
}

export function allowlist(config, stage) {
  const list = [...config.wasmFeatures.cargo];
  if (stage === 'final') list.push(...config.wasmFeatures.optimizer);
  return new Set(list);
}

export function checkFiles(files, config, stage) {
  const allowed = allowlist(config, stage);
  const results = [];
  for (const file of files) {
    const { features, problems } = checkFeatures(readFileSync(file), allowed);
    results.push({ file, features, problems });
  }
  return results;
}

function main(argv) {
  let stage = 'final';
  let configPath = 'wasm-toolchain.json';
  const files = [];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--stage') stage = argv[++i];
    else if (argv[i] === '--config') configPath = argv[++i];
    else files.push(argv[i]);
  }
  if (!['cargo', 'final'].includes(stage) || files.length === 0) {
    console.error('usage: check-wasm-features.mjs [--stage cargo|final] [--config file] <file.wasm>...');
    return 2;
  }
  let results;
  try {
    results = checkFiles(files, JSON.parse(readFileSync(configPath, 'utf8')), stage);
  } catch (e) {
    console.error(`check-wasm-features: ${e.message}`);
    return 2;
  }
  let bad = false;
  for (const r of results) {
    console.log(`${r.file} (${stage}): ${r.features.join(', ') || 'none'}`);
    for (const p of r.problems) {
      console.error(`FEATURE: ${r.file}: ${p}`);
      bad = true;
    }
  }
  if (!bad) console.log('Wasm target features OK');
  return bad ? 1 : 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  process.exit(main(process.argv.slice(2)));
}
