#!/usr/bin/env node
// Advisory Virginia ABC advertising lint (zero dependencies, tenant-agnostic).
// Node port of core/src/abc.rs — keep the rules in sync.
//
// Usage: node core/tools/abc-lint.mjs [--json] <file...>
//   .json      every string value is linted (reported by JSON path), plus
//              happy_hour.start and happy_hour.end are checked against the
//              21:00–05:59 overnight window
//   .md/.txt   linted line by line
//   .html/.htm tags/scripts stripped, then linted line by line
// Exit: 0 = no findings, 1 = findings, 2 = usage / read / parse error.
//
// Rules cite 3 VAC 5-20-10 through 3 VAC 5-20-100 (advertising) and
// 3 VAC 5-50-160 (happy hour). Advisory only: re-verify against current
// Virginia ABC regulations before client use; human sign-off still required.

import { readFile } from 'node:fs/promises';
import { extname } from 'node:path';
import { pathToFileURL } from 'node:url';

export const CITATIONS =
  '3 VAC 5-20-10 through 3 VAC 5-20-100 (advertising); 3 VAC 5-50-160 (happy hour). Advisory only: re-verify against current Virginia ABC regulations; human sign-off required.';

export const PHRASE_RULES = [
  ['bottomless', 'bottomless'],
  ['two_for_one', '2-for-1'],
  ['two_for_one', '2 for 1'],
  ['two_for_one', '2for1'],
  ['two_for_one', 'two for one'],
  ['two_for_one', 'two-for-one'],
  ['unlimited_drinks', 'unlimited drinks'],
];

const HH_LATE_START_MIN = 21 * 60;
const HH_OVERNIGHT_END_MIN = 6 * 60;

const DASH_RE = /[\u002D\u2013\u2014\u2212]/u;
// Match Rust char::is_whitespace() (Unicode White_Space, which excludes U+FEFF unlike JS \s).
const WS_RE = /\p{White_Space}/u;
const INLINE_TAGS = new Set(['strong', 'em', 'span', 'b', 'i', 'a']);
const NAMED_ENTITIES = {
  amp: '&',
  nbsp: '\u00A0',
  apos: "'",
  quot: '"',
  lt: '<',
  gt: '>',
  ndash: '\u2013',
  mdash: '\u2014',
  minus: '\u2212',
};

// Match Rust char::is_alphanumeric() (Unicode Alphabetic || Number, including Other_Alphabetic like U+093E).
const isBoundary = (ch) => ch === undefined || !/[\p{Alphabetic}\p{N}]/u.test(ch);

function utf8ByteLen(ch) {
  const cp = ch.codePointAt(0);
  if (cp <= 0x7f) return 1;
  if (cp <= 0x7ff) return 2;
  if (cp <= 0xffff) return 3;
  return 4;
}

function normalizeForLint(text) {
  const cps = Array.from(text);
  const cpByteStart = new Array(cps.length);
  const cpCuStart = new Array(cps.length);
  const cpCuEnd = new Array(cps.length);
  let bytePos = 0;
  let cuPos = 0;
  for (let i = 0; i < cps.length; i++) {
    cpByteStart[i] = bytePos;
    cpCuStart[i] = cuPos;
    bytePos += utf8ByteLen(cps[i]);
    cuPos += cps[i].length;
    cpCuEnd[i] = cuPos;
  }

  let norm = '';
  const startBytes = [];
  const startCus = [];
  const endCus = [];
  const prevCps = [];
  const nextCps = [];

  let i = 0;
  while (i < cps.length) {
    const ch = cps[i];
    if (WS_RE.test(ch)) {
      const firstIdx = i;
      let lastIdx = i;
      while (i + 1 < cps.length && WS_RE.test(cps[i + 1])) {
        i++;
        lastIdx = i;
      }
      norm += ' ';
      startBytes.push(cpByteStart[firstIdx]);
      startCus.push(cpCuStart[firstIdx]);
      endCus.push(cpCuEnd[lastIdx]);
      prevCps.push(firstIdx > 0 ? cps[firstIdx - 1] : undefined);
      nextCps.push(lastIdx + 1 < cps.length ? cps[lastIdx + 1] : undefined);
      i++;
    } else {
      const mapped = DASH_RE.test(ch)
        ? '-'
        : ch.replace(/[A-Z]/, (c) => c.toLowerCase());
      norm += mapped;
      for (let k = 0; k < mapped.length; k++) {
        startBytes.push(cpByteStart[i]);
        startCus.push(cpCuStart[i]);
        endCus.push(cpCuEnd[i]);
        prevCps.push(i > 0 ? cps[i - 1] : undefined);
        nextCps.push(i + 1 < cps.length ? cps[i + 1] : undefined);
      }
      i++;
    }
  }
  return { norm, startBytes, startCus, endCus, prevCps, nextCps };
}

export function lintText(text) {
  const { norm, startBytes, startCus, endCus, prevCps, nextCps } = normalizeForLint(text);
  const findings = [];
  for (const [rule, phrase] of PHRASE_RULES) {
    let start = 0;
    for (;;) {
      const at = norm.indexOf(phrase, start);
      if (at < 0) break;
      const end = at + phrase.length;
      if (isBoundary(prevCps[at]) && isBoundary(nextCps[end - 1])) {
        findings.push({
          rule,
          matched: text.slice(startCus[at], endCus[end - 1]),
          offset: startBytes[at],
          message: `Flagged promotional phrase "${phrase}". ${CITATIONS}`,
        });
      }
      start = end;
    }
  }
  return findings.sort((a, b) => a.offset - b.offset);
}

export function parseHHMM(value) {
  const m = /^(\d{2}):(\d{2})$/.exec(String(value));
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  return h < 24 && min < 60 ? h * 60 + min : null;
}

function isRestrictedHHMinute(min) {
  return min >= HH_LATE_START_MIN || min < HH_OVERNIGHT_END_MIN;
}

export function lintHappyHourStart(start) {
  const min = parseHHMM(start);
  if (min === null) {
    return {
      rule: 'happy_hour_start_invalid',
      matched: String(start),
      offset: 0,
      message: `Happy-hour start "${start}" is not a 24-hour HH:MM time. ${CITATIONS}`,
    };
  }
  if (isRestrictedHHMinute(min)) {
    return {
      rule: 'happy_hour_late_start',
      matched: String(start),
      offset: 0,
      message: `Happy-hour start "${start}" falls within restricted 21:00-05:59 overnight window. ${CITATIONS}`,
    };
  }
  return null;
}

export function lintHappyHourEnd(end) {
  const min = parseHHMM(end);
  if (min === null) {
    return {
      rule: 'happy_hour_end_invalid',
      matched: String(end),
      offset: 0,
      message: `Happy-hour end "${end}" is not a 24-hour HH:MM time. ${CITATIONS}`,
    };
  }
  if (isRestrictedHHMinute(min)) {
    return {
      rule: 'happy_hour_late_end',
      matched: String(end),
      offset: 0,
      message: `Happy-hour end "${end}" falls within restricted 21:00-05:59 overnight window. ${CITATIONS}`,
    };
  }
  return null;
}

function walkJson(node, path, out) {
  if (typeof node === 'string') {
    for (const f of lintText(node)) out.push({ path, ...f });
  } else if (Array.isArray(node)) {
    node.forEach((v, i) => walkJson(v, `${path}[${i}]`, out));
  } else if (node && typeof node === 'object') {
    for (const [k, v] of Object.entries(node)) {
      const key = /^[A-Za-z_$][\w$]*$/.test(k) ? `.${k}` : `[${JSON.stringify(k)}]`;
      const nextPath = path + key;
      walkJson(v, nextPath, out);
      if (k === 'happy_hour' && v && typeof v === 'object' && !Array.isArray(v)) {
        if ('start' in v && v.start !== undefined) {
          const f = lintHappyHourStart(v.start);
          if (f) out.push({ path: `${nextPath}.start`, ...f });
        }
        if ('end' in v && v.end !== undefined) {
          const f = lintHappyHourEnd(v.end);
          if (f) out.push({ path: `${nextPath}.end`, ...f });
        }
      }
    }
  }
}

export function lintJson(doc) {
  const out = [];
  walkJson(doc, '$', out);
  return out;
}

function collectJsonStrings(node, out) {
  if (typeof node === 'string') {
    out.push(node);
  } else if (Array.isArray(node)) {
    for (const v of node) collectJsonStrings(v, out);
  } else if (node && typeof node === 'object') {
    for (const v of Object.values(node)) collectJsonStrings(v, out);
  }
}

function decodeHtmlEntities(str) {
  return str.replace(/&(?:#(\d+)|#x([0-9a-fA-F]+)|([a-zA-Z]+));/g, (m, dec, hex, name) => {
    if (dec !== undefined) {
      const cp = Number.parseInt(dec, 10);
      return cp >= 0 && cp <= 0x10ffff ? String.fromCodePoint(cp) : m;
    }
    if (hex !== undefined) {
      const cp = Number.parseInt(hex, 16);
      return cp >= 0 && cp <= 0x10ffff ? String.fromCodePoint(cp) : m;
    }
    const lower = name.toLowerCase();
    return Object.hasOwn(NAMED_ENTITIES, lower) ? NAMED_ENTITIES[lower] : m;
  });
}

function extractTagAttributes(attrs) {
  const values = [];
  const re = /(?:^|\s)(alt|content)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))/gi;
  let match;
  while ((match = re.exec(attrs)) !== null) {
    const val = match[2] ?? match[3] ?? match[4] ?? '';
    if (val) values.push(val);
  }
  return values;
}

export function stripHtml(src) {
  const stripped = src
    .replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi, (m, attrs, body) => {
      const nlCount = (m.match(/\n/g) || []).length;
      if (/(?:^|\s)type\s*=\s*(?:"application\/ld\+json"|'application\/ld\+json'|application\/ld\+json(?:\s|$))/i.test(attrs)) {
        try {
          const parsed = JSON.parse(body);
          const strings = [];
          collectJsonStrings(parsed, strings);
          return ` ${strings.join(' | ')} ${'\n'.repeat(nlCount)}`;
        } catch {
          return ` ${body.replace(/\r?\n/g, ' ')} ${'\n'.repeat(nlCount)}`;
        }
      }
      return '\n'.repeat(nlCount);
    })
    .replace(/<style\b[^>]*>[\s\S]*?<\/style>/gi, (m) => '\n'.repeat((m.match(/\n/g) || []).length))
    .replace(/<(\/?)([a-zA-Z!][a-zA-Z0-9:-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>/g, (m, slash, tag, attrs) => {
      const nlCount = (m.match(/\n/g) || []).length;
      const attrTexts = slash ? [] : extractTagAttributes(attrs);
      if (attrTexts.length) {
        return ` ${attrTexts.join(' ')} ${'\n'.repeat(nlCount)}`;
      }
      if (INLINE_TAGS.has(tag.toLowerCase())) {
        return '\n'.repeat(nlCount);
      }
      return ` ${'\n'.repeat(nlCount)}`;
    });
  return decodeHtmlEntities(stripped);
}

export function lintHtml(src) {
  return lintLines(stripHtml(src));
}

export function lintLines(text) {
  const findings = lintText(text);
  if (!findings.length) return [];
  const buf = Buffer.from(text, 'utf8');
  const newlineOffsets = [];
  for (let i = 0; i < buf.length; i++) {
    if (buf[i] === 0x0a) newlineOffsets.push(i);
  }
  return findings.map((f) => {
    let line = 1;
    for (const nlOffset of newlineOffsets) {
      if (nlOffset < f.offset) line++;
      else break;
    }
    return { line, ...f };
  });
}

export async function lintFile(file) {
  const src = await readFile(file, 'utf8');
  const ext = extname(file).toLowerCase();
  if (ext === '.json') return lintJson(JSON.parse(src));
  if (ext === '.html' || ext === '.htm') return lintHtml(src);
  return lintLines(src);
}

export async function main(argv) {
  const asJson = argv.includes('--json');
  const files = argv.filter((a) => !a.startsWith('--'));
  if (!files.length) {
    console.error('usage: abc-lint.mjs [--json] <file...>');
    return 2;
  }
  const results = [];
  let failed = false;
  for (const file of files) {
    try {
      for (const f of await lintFile(file)) results.push({ file, ...f });
    } catch (e) {
      console.error(`abc-lint: ${file}: ${e.message}`);
      failed = true;
    }
  }
  if (asJson) {
    console.log(JSON.stringify({ findings: results, citations: CITATIONS }, null, 2));
  } else {
    for (const r of results) {
      const where = r.path ?? `line ${r.line}`;
      console.log(`${r.file}: ${where}: [${r.rule}] "${r.matched}"`);
    }
    if (results.length) {
      console.log(`\nabc-lint: ${results.length} finding(s). ${CITATIONS}`);
    } else if (!failed) {
      console.log(`abc-lint: OK (${files.length} file(s)). Advisory only; human sign-off still required.`);
    }
  }
  if (failed) return 2;
  return results.length ? 1 : 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = await main(process.argv.slice(2));
}
