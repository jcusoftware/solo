// Guards that the tag release workflow runs the same Rust gates and Studio
// release build as CI, so a release tag is never verified by a weaker subset.
// Text-level on purpose: the repository has no YAML parser dependency.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const read = (path) => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const ci = read('.github/workflows/ci.yml');
const release = read('.github/workflows/release.yml');

// Body of one top-level job (from `  <name>:` up to the next two-space key).
function job(workflow, name) {
  const lines = workflow.split('\n');
  const start = lines.indexOf(`  ${name}:`);
  assert.notEqual(start, -1, `job ${name} not found`);
  const end = lines.findIndex((line, i) => i > start && /^ {2}\S/.test(line));
  return lines.slice(start, end === -1 ? undefined : end).join('\n');
}

// Shell commands starting with `prefix`, whitespace-normalized, in order.
function commands(text, prefix) {
  return text
    .split('\n')
    .map((line) => line.trim().replace(/^run:\s*/, '').replace(/\s+/g, ' '))
    .filter((line) => line.startsWith(prefix));
}

const ciRust = job(ci, 'rust');
const ciWeb = job(ci, 'web');
const verify = job(release, 'verify');

test('release verify runs the same cargo test commands as ci rust', () => {
  const expected = commands(ciRust, 'cargo test');
  assert.ok(expected.length >= 2);
  assert.deepEqual(commands(verify, 'cargo test'), expected);
});

test('release verify runs the same wasm clippy commands (same package list) as ci rust', () => {
  const expected = commands(ciRust, 'cargo clippy');
  assert.ok(expected.length >= 2);
  assert.deepEqual(commands(verify, 'cargo clippy'), expected);
});

test('release verify runs cargo deny like ci rust', () => {
  for (const body of [ciRust, verify]) {
    assert.match(body, /uses: EmbarkStudios\/cargo-deny-action@v2\n\s+with:\n\s+command: check/);
  }
});

test('release verify builds the Studio Worker and checks it for the dev-auth hook like ci web', () => {
  for (const cmd of ['npm run studio:build', 'npm run check:studio:no-dev-auth']) {
    assert.ok(commands(ciWeb, cmd).length === 1, `ci web should run ${cmd}`);
    assert.ok(commands(verify, cmd).length === 1, `release verify must run ${cmd}`);
  }
  const install = commands(ciWeb, 'cargo install worker-build');
  assert.deepEqual(commands(verify, 'cargo install worker-build'), install);
});

test('release verify has no conditional package probing', () => {
  assert.doesNotMatch(verify, /core\/Cargo\.toml/);
  assert.doesNotMatch(verify, /FLAGS/);
});

test('pre-release tags publish a pre-release and skip the static site deploys', () => {
  const publish = job(release, 'release-publish');
  assert.match(publish, /--prerelease/);
  assert.match(publish, /steps\.channel\.outputs\.prerelease \}\}" = "true" \]; then\n\s+echo "Skipping Cloudflare/);
  for (const step of ['Deploy landing site', 'Deploy docs site']) {
    const at = publish.indexOf(step);
    assert.notEqual(at, -1, `${step} step not found`);
    assert.match(publish.slice(at), /^[^\n]*\n\s+if: steps\.cf_deploy\.outputs\.enabled == 'true'/);
  }
  assert.match(job(release, 'release-pack-hospitality'), /--prerelease/);
});
