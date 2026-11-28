import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import {
  parseHeadersFile,
  parseRustConst,
  compare,
  opt,
} from '../../core/tools/check-headers.mjs';

describe('parseHeadersFile', () => {
  it('parses only the requested block while ignoring comments and blank lines', () => {
    const src = `
# Global headers
/*
  Strict-Transport-Security: max-age=63072000
  # Inline comment
  X-Frame-Options: DENY

/styles.css
  Cache-Control: public, max-age=3600
`;
    assert.deepEqual(parseHeadersFile(src), [
      ['Strict-Transport-Security', 'max-age=63072000'],
      ['X-Frame-Options', 'DENY'],
    ]);
    assert.deepEqual(parseHeadersFile(src, '/styles.css'), [
      ['Cache-Control', 'public, max-age=3600'],
    ]);
  });

  it('parses core/public/_headers cleanly', () => {
    const src = readFileSync(resolve(process.cwd(), 'core/public/_headers'), 'utf8');
    const parsed = parseHeadersFile(src, '/*');
    assert.ok(parsed.length >= 7);
    assert.ok(parsed.some(([k, v]) => k === 'X-Frame-Options' && v === 'DENY'));
  });
});

describe('parseRustConst', () => {
  it('parses Rust tuple slices and unescapes quoted strings', () => {
    const rust = `
pub const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("X-Frame-Options", "DENY"),
    (
        "Content-Security-Policy",
        "default-src 'self'; report-to \\"csp\\"",
    ),
];
`;
    assert.deepEqual(parseRustConst(rust), [
      ['X-Frame-Options', 'DENY'],
      ['Content-Security-Policy', 'default-src \'self\'; report-to "csp"'],
    ]);
  });

  it('throws when the requested constant is absent or malformed', () => {
    assert.throws(() => parseRustConst('pub const OTHER: u8 = 1;'), /not found/);
    assert.throws(() => parseRustConst('pub const SECURITY_HEADERS = 1;'), /unexpected shape/);
  });

  it('matches core/public/_headers against crates/sws-security/src/headers.rs', () => {
    const fileHeaders = parseHeadersFile(
      readFileSync(resolve(process.cwd(), 'core/public/_headers'), 'utf8'),
    );
    const rustHeaders = parseRustConst(
      readFileSync(resolve(process.cwd(), 'crates/sws-security/src/headers.rs'), 'utf8'),
    );
    assert.deepEqual(compare(rustHeaders, fileHeaders, '_headers vs headers.rs'), []);
    assert.deepEqual(compare(fileHeaders, rustHeaders, 'headers.rs vs _headers'), []);
  });

  it('parses each named profile const exactly as its golden profiles/*.headers file (#180)', () => {
    const rust = readFileSync(resolve(process.cwd(), 'crates/sws-security/src/headers.rs'), 'utf8');
    for (const [name, file] of [
      ['STUDIO_HEADERS', 'studio'],
      ['STUDIO_FORM_HEADERS', 'studio-form'],
      ['PREVIEW_HEADERS', 'preview'],
      ['CANVAS_HEADERS', 'canvas'],
    ]) {
      const golden = parseHeadersFile(
        readFileSync(resolve(process.cwd(), `crates/sws-security/profiles/${file}.headers`), 'utf8'),
      );
      assert.deepEqual(parseRustConst(rust, name), golden, `${name} vs profiles/${file}.headers`);
    }
    // The default (site) const is still the first match, unaffected by the new consts.
    assert.equal(parseRustConst(rust).find(([k]) => k === 'X-Frame-Options')?.[1], 'DENY');
    assert.match(
      parseRustConst(rust).find(([k]) => k === 'Content-Security-Policy')[1],
      /challenges\.cloudflare\.com/,
    );
  });
});

describe('compare', () => {
  it('returns no problems on case-insensitive name match and exact value match', () => {
    const expected = [['X-Frame-Options', 'DENY'], ['X-Content-Type-Options', 'nosniff']];
    const actual = [['x-frame-options', 'DENY'], ['x-content-type-options', 'nosniff']];
    assert.deepEqual(compare(expected, actual, 'test'), []);
  });

  it('reports missing, duplicate, and mismatched header values', () => {
    const expected = [
      ['X-Frame-Options', 'DENY'],
      ['Referrer-Policy', 'strict-origin-when-cross-origin'],
      ['X-Content-Type-Options', 'nosniff'],
    ];
    const actual = [
      ['X-Frame-Options', 'SAMEORIGIN'],
      ['Referrer-Policy', 'strict-origin-when-cross-origin'],
      ['Referrer-Policy', 'strict-origin-when-cross-origin'],
    ];
    const problems = compare(expected, actual, 'live');
    assert.equal(problems.length, 3);
    assert.ok(problems[0].includes('X-Frame-Options differs'));
    assert.ok(problems[0].includes('expected: DENY'));
    assert.ok(problems[0].includes('actual:   SAMEORIGIN'));
    assert.ok(problems[1].includes('live: duplicate Referrer-Policy'));
    assert.ok(problems[2].includes('live: missing X-Content-Type-Options'));
  });
});

describe('opt', () => {
  it('returns default when flag is absent and value when valid', () => {
    assert.equal(opt([], '--url', null), null);
    assert.equal(opt(['--other', '1'], '--headers-file', 'core/public/_headers'), 'core/public/_headers');
    assert.equal(opt(['--url', 'http://127.0.0.1:8787'], '--url', null), 'http://127.0.0.1:8787');
  });

  it('returns undefined when value is missing or starts with "-"', () => {
    assert.equal(opt(['--url'], '--url', null), undefined);
    assert.equal(opt(['--url', '--headers-file', 'x'], '--url', null), undefined);
    assert.equal(opt(['--url', '-v'], '--url', null), undefined);
  });
});
