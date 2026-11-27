// Editing gate (issue #55): before any Studio integration, prove that the
// canonical reducer behaves identically natively and in Wasm and that the saved
// IR compiles to byte-identical production output on both targets.
//
// Scenario: load the legacy fictional demo (lowered IR of
// packs/hospitality/demo/venue.json), upgrade it, change a token, move a
// section, undo and redo both, save and reopen, then compare the file map
// compiled by the native `solo build` with the Wasm `compile_site` export.
//
// Run via `npm run gate:editing` (crates/sws-wasm/run-editing-gate.sh), which
// builds the native CLI and the Wasm bindings first. Not part of `npm run verify`.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, '../../..');
const solo = process.env.SOLO_BIN ?? path.join(repoRoot, 'target/debug/solo');
const pkgDir = process.env.SWS_WASM_PKG;
assert.ok(pkgDir, 'SWS_WASM_PKG must point at the wasm-bindgen --target nodejs output');
const wasm = createRequire(import.meta.url)(path.join(pkgDir, 'sws_wasm.cjs'));

const venue = path.join(repoRoot, 'packs/hospitality/demo/venue.json');
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'solo-editing-gate-'));
const sha = (buf) => createHash('sha256').update(buf).digest('hex');
const cli = (...args) =>
  execFileSync(solo, args, { cwd: repoRoot, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
const file = (name) => path.join(work, name);
const read = (name) => fs.readFileSync(file(name), 'utf8');
const step = (msg) => console.log(`gate: ${msg}`);

// Native apply: input file + batch object -> output document text and transaction.
function nativeApply(inputName, batch, outName, txName) {
  fs.writeFileSync(file(`${outName}.batch.json`), JSON.stringify(batch));
  cli('edit', 'apply', file(inputName), file(`${outName}.batch.json`), '--out', file(outName),
    '--transaction', file(txName));
  return { doc: read(outName), bytes: fs.readFileSync(file(outName)), tx: JSON.parse(read(txName)) };
}

// Wasm apply: same inputs through the Wasm export.
function wasmApply(docText, batch) {
  const out = JSON.parse(wasm.apply_command_batch(docText, JSON.stringify(batch)));
  return { doc: out.document, tx: out.transaction };
}

const batchOf = (commands) => ({ version: 1, commands });

try {
  // 1. Load the legacy demo (one-way eject of the fictional venue manifest).
  const venueHash = sha(fs.readFileSync(venue));
  cli('edit', 'eject', venue, '--out', file('legacy.json'));
  const legacy = read('legacy.json');
  assert.equal(JSON.parse(legacy).pages.length, 5, 'demo lowers to 5 pages');
  step('ejected legacy IR');

  // 2. Explicit upgrade, natively and in Wasm: identical bytes, idempotent.
  cli('edit', 'upgrade', file('legacy.json'), '--out', file('base.json'), '--report', file('report.json'));
  const base = read('base.json');
  const wasmUp = JSON.parse(wasm.upgrade_stable_ids(legacy));
  assert.equal(wasmUp.document, base, 'native and Wasm upgrade differ');
  assert.equal(JSON.parse(wasm.upgrade_stable_ids(base)).document, base, 'upgrade is not idempotent');
  assert.equal(JSON.parse(read('report.json')).assigned.length, wasmUp.report.assigned.length);
  step(`upgraded: ${wasmUp.report.assigned.length} stable ids (native == wasm, idempotent)`);

  // 3. Find a section on the index page and its parent by uid.
  const baseDoc = JSON.parse(base);
  let section = null;
  const find = (nodes, parent) => {
    for (const node of nodes ?? []) {
      if (!section && node.type === 'element' && node.tag === 'section' && parent) {
        section = { uid: node.uid, parent };
      }
      find(node.children, node);
    }
  };
  find(baseDoc.pages.find((p) => p.id === 'index').body, null);
  assert.ok(section, 'demo index page has a section element');
  const parentDoc = (function findNode(nodes, uid) {
    for (const n of nodes ?? []) {
      if (n.uid === uid) return n;
      const hit = findNode(n.children, uid);
      if (hit) return hit;
    }
    return null;
  })(baseDoc.pages.find((p) => p.id === 'index').body, section.parent.uid);
  const lastIndex = parentDoc.children.length - 1;
  assert.ok(lastIndex > 0, 'parent has more than one child');

  const tokenBatch = batchOf([{ op: 'set_color_token', key: 'accent', hex: '#701B27' }]);
  const moveBatch = batchOf([{
    op: 'move_node',
    uid: section.uid,
    container: { kind: 'node', uid: section.parent.uid, slot: 'children' },
    index: lastIndex,
  }]);

  // 4. Edit natively and in Wasm; documents and transactions must agree.
  const n1 = nativeApply('base.json', tokenBatch, 'edit1.json', 'tx1.json');
  const n2 = nativeApply('edit1.json', moveBatch, 'edit2.json', 'tx2.json');
  const w1 = wasmApply(base, tokenBatch);
  const w2 = wasmApply(w1.doc, moveBatch);
  assert.equal(w1.doc, n1.doc, 'token edit differs native vs Wasm');
  assert.equal(w2.doc, n2.doc, 'section move differs native vs Wasm');
  assert.ok(Buffer.from(w1.doc, 'utf8').equals(n1.bytes), 'token edit bytes differ native vs Wasm');
  assert.ok(Buffer.from(w2.doc, 'utf8').equals(n2.bytes), 'section move bytes differ native vs Wasm');
  assert.deepEqual(w2.tx, n2.tx, 'transaction differs native vs Wasm');
  assert.notEqual(n1.doc, base);
  assert.notEqual(n2.doc, n1.doc);
  assert.ok(n2.tx.touched.includes(`node:${section.uid}`), 'touched fields are uid-addressed');
  step(`edited: token + move of ${section.uid} (native == wasm)`);

  // 5. Undo both, redo both (replaying the recorded transactions).
  const undo2 = nativeApply('edit2.json', batchOf(n2.tx.inverse), 'undo2.json', 'utx2.json');
  assert.equal(undo2.doc, n1.doc, 'undo of the move is not exact');
  const undo1 = nativeApply('undo2.json', batchOf(n1.tx.inverse), 'undo1.json', 'utx1.json');
  assert.equal(undo1.doc, base, 'undo of the token change is not exact');
  const redo1 = nativeApply('undo1.json', batchOf(n1.tx.forward), 'redo1.json', 'rtx1.json');
  assert.equal(redo1.doc, n1.doc, 'redo of the token change differs');
  const redo2 = nativeApply('redo1.json', batchOf(n2.tx.forward), 'redo2.json', 'rtx2.json');
  assert.equal(redo2.doc, n2.doc, 'redo of the move differs');
  // Same undo/redo through Wasm.
  const wUndo = wasmApply(wasmApply(w2.doc, batchOf(w2.tx.inverse)).doc, batchOf(w1.tx.inverse));
  assert.equal(wUndo.doc, base, 'Wasm undo differs');
  // Redo through Wasm: replay the recorded forward commands in order.
  const wRedo1 = wasmApply(wUndo.doc, batchOf(w1.tx.forward));
  assert.equal(wRedo1.doc, n1.doc, 'Wasm redo of the token change differs');
  const wRedo2 = wasmApply(wRedo1.doc, batchOf(w2.tx.forward));
  assert.equal(wRedo2.doc, n2.doc, 'Wasm redo of the move differs');
  assert.ok(Buffer.from(wRedo2.doc, 'utf8').equals(redo2.bytes), 'Wasm redo bytes differ from native redo');
  assert.deepEqual(wRedo2.tx.forward, n2.tx.forward, 'Wasm redo replays the recorded forward commands');
  step('undo/redo exact (native and Wasm)');

  // 6. Save and reopen: the saved file re-opens byte-stably (upgrade is a no-op,
  //    nothing unsupported is dropped) and still passes `solo check`.
  const saved = redo2.doc;
  fs.writeFileSync(file('saved.json'), saved);
  cli('edit', 'upgrade', file('saved.json'), '--out', file('reopened.json'));
  assert.equal(read('reopened.json'), saved, 'reopened document differs from saved');
  cli('check', file('saved.json'));
  step('saved and reopened byte-stably; solo check passes');

  // 7. Compile the same saved IR natively and in Wasm; compare the file maps.
  cli('build', file('saved.json'), file('out-native'));
  // Raw bytes on both sides: no UTF-8 decoding before comparing.
  const nativeFiles = {};
  (function walk(dir, prefix) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) walk(path.join(dir, entry.name), rel);
      else nativeFiles[rel] = fs.readFileSync(path.join(dir, entry.name));
    }
  })(file('out-native'), '');
  const wasmFiles = JSON.parse(wasm.compile_site(saved)).files;
  assert.deepEqual(Object.keys(wasmFiles).sort(), Object.keys(nativeFiles).sort(), 'file sets differ');
  for (const name of Object.keys(nativeFiles)) {
    assert.ok(
      nativeFiles[name].equals(Buffer.from(wasmFiles[name], 'utf8')),
      `native and Wasm output differ for ${name}`,
    );
  }
  const total = Object.values(nativeFiles).reduce((n, b) => n + b.length, 0);
  const digest = createHash('sha256');
  for (const name of Object.keys(nativeFiles).sort()) digest.update(name).update('\0').update(nativeFiles[name]);
  step(`native == wasm: ${Object.keys(nativeFiles).length} files, ${total} bytes, sha256 ${digest.digest('hex').slice(0, 16)}`);

  // 8. The edits are real and `uid` never reaches the output.
  const baseFiles = JSON.parse(wasm.compile_site(base)).files;
  const legacyFiles = JSON.parse(wasm.compile_site(legacy)).files;
  assert.deepEqual(baseFiles, legacyFiles, 'upgrade changed compiled output');
  assert.notEqual(wasmFiles['index.html'], baseFiles['index.html'], 'move did not change index.html');
  assert.notEqual(wasmFiles['styles.css'], baseFiles['styles.css'], 'token change did not change styles.css');
  assert.ok(!wasmFiles['index.html'].includes(section.uid), 'uid leaked into HTML');

  // 9. Read-only guarantee: the source manifest was never written.
  assert.equal(sha(fs.readFileSync(venue)), venueHash, 'venue.json was modified');
  console.log('gate: editing gate PASSED');
} finally {
  fs.rmSync(work, { recursive: true, force: true });
}
