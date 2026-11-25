import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import {
  CITATIONS,
  PHRASE_RULES,
  lintText,
  lintHappyHourStart,
  lintHappyHourEnd,
  lintJson,
  lintHtml,
} from '../../packs/hospitality/tools/abc-lint.mjs';

const ABC_RS_PATH = resolve(process.cwd(), 'packs/hospitality/crate/src/abc.rs');

function parseRustStringLiteral(raw) {
  return raw.replace(/\\(.)/g, (_, ch) => {
    if (ch === 'n') return '\n';
    if (ch === 'r') return '\r';
    if (ch === 't') return '\t';
    return ch;
  });
}

function parseRustAbcConstants(src) {
  const citMatch = src.match(/pub const CITATIONS:\s*&str\s*=\s*"((?:[^"\\]|\\.)*)"\s*;/);
  assert.ok(citMatch, 'CITATIONS constant not found in packs/hospitality/crate/src/abc.rs');
  const citations = parseRustStringLiteral(citMatch[1]);

  const rulesBlockMatch = src.match(/pub const PHRASE_RULES:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[([\s\S]*?)\];/);
  assert.ok(rulesBlockMatch, 'PHRASE_RULES constant not found in packs/hospitality/crate/src/abc.rs');
  const tupleRe = /\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*,?\s*\)/g;
  const phraseRules = [...rulesBlockMatch[1].matchAll(tupleRe)].map((m) => [
    parseRustStringLiteral(m[1]),
    parseRustStringLiteral(m[2]),
  ]);

  const lateStartMatch = src.match(/pub const HH_LATE_START_MIN:\s*u32\s*=\s*([^;]+);/);
  const overnightEndMatch = src.match(/pub const HH_OVERNIGHT_END_MIN:\s*u32\s*=\s*([^;]+);/);
  assert.ok(lateStartMatch, 'HH_LATE_START_MIN not found in packs/hospitality/crate/src/abc.rs');
  assert.ok(overnightEndMatch, 'HH_OVERNIGHT_END_MIN not found in packs/hospitality/crate/src/abc.rs');

  return {
    citations,
    phraseRules,
    lateStartExpr: lateStartMatch[1].trim(),
    overnightEndExpr: overnightEndMatch[1].trim(),
  };
}

describe('abc-lint phrase normalization and Unicode handling', () => {
  it('flags en-dash, em-dash, minus, NBSP, repeated spaces, and multiline phrases', () => {
    const cases = [
      ['2\u2013for\u20131 drafts', 'two_for_one', '2\u2013for\u20131'],
      ['2\u2014for\u20141 cocktails', 'two_for_one', '2\u2014for\u20141'],
      ['Two\u2212for\u2212One pints', 'two_for_one', 'Two\u2212for\u2212One'],
      ['2  for  1 wines', 'two_for_one', '2  for  1'],
      ['2\u00A0for\u00A01 beers', 'two_for_one', '2\u00A0for\u00A01'],
      ['Unlimited\ndrinks nightly', 'unlimited_drinks', 'Unlimited\ndrinks'],
      ['Unlimited\r\n  drinks', 'unlimited_drinks', 'Unlimited\r\n  drinks'],
    ];
    for (const [text, expectedRule, expectedMatched] of cases) {
      const f = lintText(text);
      assert.equal(f.length, 1, `expected 1 finding for ${JSON.stringify(text)}`);
      assert.equal(f[0].rule, expectedRule);
      assert.equal(f[0].matched, expectedMatched);
      assert.equal(f[0].offset, 0);
    }
  });

  it('respects UTF-16 code-point word boundaries and computes UTF-8 byte offsets', () => {
    // U+1D41B is mathematical bold small b (surrogate pair in UTF-16, letter in Unicode)
    assert.deepEqual(lintText('\u{1D41B}bottomless'), []);

    // U+093E (Devanagari vowel sign AA) is Other_Alphabetic: Rust char::is_alphanumeric() is true
    assert.deepEqual(lintText('x\u093Ebottomless'), []);

    // U+FEFF (BOM/ZWNBSP) is NOT Rust char::is_whitespace(); U+0085 (NEL) IS White_Space
    assert.deepEqual(lintText('2\uFEFFfor\uFEFF1'), []);
    const nel = lintText('2\u0085for\u00851');
    assert.equal(nel.length, 1);
    assert.equal(nel[0].rule, 'two_for_one');
    assert.equal(nel[0].matched, '2\u0085for\u00851');

    // U+1F942 clinking glasses (4 UTF-8 bytes) + space (1 byte) => offset 5
    const findings = lintText('\u{1F942} 2\u2013for\u20131 drafts');
    assert.equal(findings.length, 1);
    assert.equal(findings[0].rule, 'two_for_one');
    assert.equal(findings[0].offset, 5);
    assert.equal(findings[0].matched, '2\u2013for\u20131');
  });
});

describe('abc-lint HTML extraction', () => {
  it('catches inline HTML-tag-split phrases', () => {
    const html = '<p><strong>2</strong>-for-1 martinis</p>\n<p><em>Unlimited</em> <span>drinks</span></p>';
    const f = lintHtml(html);
    assert.equal(f.length, 2);
    assert.equal(f[0].rule, 'two_for_one');
    assert.equal(f[0].line, 1);
    assert.equal(f[1].rule, 'unlimited_drinks');
    assert.equal(f[1].line, 2);
  });

  it('lints img alt attributes, meta content attributes, and JSON-LD script strings', () => {
    const html = `<!doctype html>
<html>
<head>
<meta name="description" content="Join us for 2-for-1 drafts">
<script type="application/ld+json">{"@type":"Menu","description":"Unlimited drinks on Friday"}</script>
</head>
<body>
<img src="/brunch.jpg" alt="Bottomless mimosas every Sunday">
</body>
</html>`;
    const f = lintHtml(html);
    assert.equal(f.length, 3);
    assert.deepEqual(
      f.map((x) => x.rule),
      ['two_for_one', 'unlimited_drinks', 'bottomless'],
    );
  });
});

describe('abc-lint happy-hour windows and nested JSON', () => {
  it('flags overnight end times like 03:00 and checks happy_hour.start boundaries', () => {
    assert.equal(lintHappyHourEnd('03:00')?.rule, 'happy_hour_late_end');
    assert.equal(lintHappyHourEnd('21:00')?.rule, 'happy_hour_late_end');
    assert.equal(lintHappyHourEnd('05:59')?.rule, 'happy_hour_late_end');
    assert.equal(lintHappyHourEnd('06:00'), null);
    assert.equal(lintHappyHourEnd('20:59'), null);

    assert.equal(lintHappyHourStart('22:00')?.rule, 'happy_hour_late_start');
    assert.equal(lintHappyHourStart('01:00')?.rule, 'happy_hour_late_start');
    assert.equal(lintHappyHourStart('21:00')?.rule, 'happy_hour_late_start');
    assert.equal(lintHappyHourStart('05:59')?.rule, 'happy_hour_late_start');
    assert.equal(lintHappyHourStart('06:00'), null);
    assert.equal(lintHappyHourStart('16:00'), null);
    assert.equal(lintHappyHourStart('20:59'), null);
  });

  it('walks nested happy_hour objects in lintJson', () => {
    const doc = {
      events: {
        late_night: {
          happy_hour: {
            start: '22:00',
            end: '03:00',
            offers: ['Bottomless sangria'],
          },
        },
      },
    };
    const f = lintJson(doc);
    assert.deepEqual(
      f.map((x) => [x.path, x.rule]),
      [
        ['$.events.late_night.happy_hour.offers[0]', 'bottomless'],
        ['$.events.late_night.happy_hour.start', 'happy_hour_late_start'],
        ['$.events.late_night.happy_hour.end', 'happy_hour_late_end'],
      ],
    );
  });
});

describe('abc-lint Rust <-> Node rule parity', () => {
  it('matches PHRASE_RULES, CITATIONS, and restricted window thresholds in packs/hospitality/crate/src/abc.rs', () => {
    const rustSrc = readFileSync(ABC_RS_PATH, 'utf8');
    const rust = parseRustAbcConstants(rustSrc);

    assert.equal(CITATIONS, rust.citations);
    assert.deepEqual(PHRASE_RULES, rust.phraseRules);
    assert.equal(rust.lateStartExpr, '21 * 60');
    assert.equal(rust.overnightEndExpr, '6 * 60');

    // Verify happy_hour threshold behavior matches 21:00..05:59 on both boundaries
    assert.equal(lintHappyHourStart('20:59'), null);
    assert.equal(lintHappyHourStart('21:00')?.rule, 'happy_hour_late_start');
    assert.equal(lintHappyHourStart('05:59')?.rule, 'happy_hour_late_start');
    assert.equal(lintHappyHourStart('06:00'), null);

    assert.equal(lintHappyHourEnd('20:59'), null);
    assert.equal(lintHappyHourEnd('21:00')?.rule, 'happy_hour_late_end');
    assert.equal(lintHappyHourEnd('05:59')?.rule, 'happy_hour_late_end');
    assert.equal(lintHappyHourEnd('06:00'), null);
  });
});
