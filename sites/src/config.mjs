// Configuration parsing, validation, and release target matrix for SWS static sites.

import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

export const DEFAULT_LANDING_URL = 'https://solo.jcusoftware.com';
export const DEFAULT_DOCS_URL = 'https://docs.jcusoftware.com';
export const DEFAULT_GITHUB_REPO = 'jcusoftware/solo';
export const DEFAULT_VERSION = 'dev';
export const DEFAULT_HAS_CLI = false;
export const DEFAULT_SHOWCASE_URL = '';

// Must stay aligned with `.github/workflows/release.yml` (`release-cli-build` matrix).
export const CLI_TARGETS = Object.freeze([
  Object.freeze({
    target: 'x86_64-unknown-linux-gnu',
    os: 'Linux',
    arch: 'x86_64',
    label: 'Linux (x86_64)',
    ext: 'tar.gz',
    bin: 'solo',
  }),
  Object.freeze({
    target: 'aarch64-unknown-linux-gnu',
    os: 'Linux',
    arch: 'ARM64',
    label: 'Linux (ARM64)',
    ext: 'tar.gz',
    bin: 'solo',
  }),
  Object.freeze({
    target: 'aarch64-apple-darwin',
    os: 'macOS',
    arch: 'Apple Silicon',
    label: 'macOS (Apple Silicon)',
    ext: 'tar.gz',
    bin: 'solo',
  }),
  Object.freeze({
    target: 'x86_64-apple-darwin',
    os: 'macOS',
    arch: 'Intel x86_64',
    label: 'macOS (Intel)',
    ext: 'tar.gz',
    bin: 'solo',
  }),
  Object.freeze({
    target: 'x86_64-pc-windows-msvc',
    os: 'Windows',
    arch: 'x86_64',
    label: 'Windows (x86_64)',
    ext: 'zip',
    bin: 'solo.exe',
  }),
]);

export function validateOriginUrl(raw, fieldName) {
  if (typeof raw !== 'string') {
    throw new Error(`${fieldName} must be a string`);
  }
  const trimmed = raw.trim();
  if (!trimmed) {
    throw new Error(`${fieldName} must not be empty`);
  }
  if (/[\u0000-\u001F\u007F\s]/.test(trimmed)) {
    throw new Error(`${fieldName} must not contain whitespace or control characters`);
  }
  let parsed;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new Error(`${fieldName} is not a valid URL: ${trimmed}`);
  }
  if (parsed.protocol !== 'https:' && parsed.protocol !== 'http:') {
    throw new Error(`${fieldName} must use http:// or https://: ${trimmed}`);
  }
  if (parsed.username || parsed.password) {
    throw new Error(`${fieldName} must not contain embedded credentials`);
  }
  if (parsed.search || parsed.hash || trimmed.includes('?') || trimmed.includes('#')) {
    throw new Error(`${fieldName} must not contain query strings or fragments: ${trimmed}`);
  }
  if (parsed.pathname.replace(/\/+$/, '') !== '') {
    throw new Error(`${fieldName} must be an origin without a path: ${trimmed}`);
  }
  return parsed.origin;
}

export function validateOptionalUrl(raw, fieldName) {
  if (raw == null) return '';
  if (typeof raw !== 'string') {
    throw new Error(`${fieldName} must be a string`);
  }
  const trimmed = raw.trim();
  if (!trimmed) return '';
  if (/[\u0000-\u001F\u007F\s]/.test(trimmed)) {
    throw new Error(`${fieldName} must not contain whitespace or control characters`);
  }
  let parsed;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new Error(`${fieldName} is not a valid URL: ${trimmed}`);
  }
  if (parsed.protocol !== 'https:' && parsed.protocol !== 'http:') {
    throw new Error(`${fieldName} must use http:// or https://: ${trimmed}`);
  }
  if (parsed.username || parsed.password) {
    throw new Error(`${fieldName} must not contain embedded credentials`);
  }
  return trimmed.replace(/\/+$/, '');
}

export function validateRepo(raw) {
  if (typeof raw !== 'string') {
    throw new Error('repo must be a string');
  }
  const trimmed = raw.trim();
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(trimmed)) {
    throw new Error(`repo must match owner/name: ${trimmed}`);
  }
  return trimmed;
}

export function validateVersion(raw) {
  if (typeof raw !== 'string') {
    throw new Error('version must be a string');
  }
  const trimmed = raw.trim();
  if (!/^[A-Za-z0-9._+-]{1,64}$/.test(trimmed)) {
    throw new Error(`version contains unsupported characters: ${trimmed}`);
  }
  return trimmed;
}

export function parseBool(raw, fallback, fieldName) {
  if (raw == null) return fallback;
  if (typeof raw === 'boolean') return raw;
  const norm = String(raw).trim().toLowerCase();
  if (norm === '') return fallback;
  if (norm === 'true' || norm === '1' || norm === 'yes') return true;
  if (norm === 'false' || norm === '0' || norm === 'no') return false;
  throw new Error(`${fieldName} must be true or false (got ${JSON.stringify(raw)})`);
}

export function validateOutDir(raw, fieldName) {
  if (typeof raw !== 'string') {
    throw new Error(`${fieldName} must be a string`);
  }
  const trimmed = raw.trim();
  if (!trimmed) {
    throw new Error(`${fieldName} must not be empty`);
  }
  if (/[\u0000-\u001F\u007F]/.test(trimmed)) {
    throw new Error(`${fieldName} must not contain control characters`);
  }
  const resolved = path.resolve(trimmed);
  if (resolved === path.parse(resolved).root) {
    throw new Error(`${fieldName} must not be a filesystem root: ${trimmed}`);
  }
  if (resolved === REPO_ROOT || resolved === path.resolve(process.cwd())) {
    throw new Error(`${fieldName} must not be the repository or working directory root: ${trimmed}`);
  }
  return trimmed;
}

function isSameOrNested(a, b) {
  const rel = path.relative(a, b);
  return rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel));
}

function envOrDefault(value, fallback) {
  if (typeof value !== 'string') return fallback;
  const trimmed = value.trim();
  return trimmed ? trimmed : fallback;
}

export function normalizeConfig(input = {}) {
  const version = validateVersion(input.version ?? DEFAULT_VERSION);
  const hasCli = parseBool(input.hasCli, DEFAULT_HAS_CLI, 'hasCli');
  const repo = validateRepo(input.repo ?? DEFAULT_GITHUB_REPO);
  const landingUrl = validateOriginUrl(input.landingUrl ?? DEFAULT_LANDING_URL, 'landingUrl');
  const docsUrl = validateOriginUrl(input.docsUrl ?? DEFAULT_DOCS_URL, 'docsUrl');
  const showcaseUrl = validateOptionalUrl(input.showcaseUrl ?? DEFAULT_SHOWCASE_URL, 'showcaseUrl');
  const outLanding = validateOutDir(input.outLanding ?? 'sites/landing/public', 'outLanding');
  const outDocs = validateOutDir(input.outDocs ?? 'sites/docs/public', 'outDocs');
  const resolvedLanding = path.resolve(outLanding);
  const resolvedDocs = path.resolve(outDocs);
  if (isSameOrNested(resolvedLanding, resolvedDocs) || isSameOrNested(resolvedDocs, resolvedLanding)) {
    throw new Error('outLanding and outDocs must not be identical or nested inside each other');
  }
  const check = Boolean(input.check);
  const help = Boolean(input.help);

  return {
    version,
    hasCli,
    repo,
    repoName: repo.split('/')[1],
    githubUrl: `https://github.com/${repo}`,
    releasesUrl: `https://github.com/${repo}/releases`,
    landingUrl,
    docsUrl,
    showcaseUrl,
    outLanding,
    outDocs,
    check,
    help,
  };
}

export function parseCliArgs(argv, env = process.env) {
  const opts = {
    version: envOrDefault(env.SWS_RELEASE_VERSION, DEFAULT_VERSION),
    hasCli: env.SWS_HAS_CLI ?? DEFAULT_HAS_CLI,
    repo: envOrDefault(env.SWS_GITHUB_REPO, DEFAULT_GITHUB_REPO),
    landingUrl: envOrDefault(env.SWS_LANDING_URL, DEFAULT_LANDING_URL),
    docsUrl: envOrDefault(env.SWS_DOCS_URL, DEFAULT_DOCS_URL),
    showcaseUrl: envOrDefault(env.SWS_SHOWCASE_URL, DEFAULT_SHOWCASE_URL),
    outLanding: 'sites/landing/public',
    outDocs: 'sites/docs/public',
    check: false,
    help: false,
  };

  const valueFlags = new Map([
    ['--version', 'version'],
    ['--has-cli', 'hasCli'],
    ['--repo', 'repo'],
    ['--landing-url', 'landingUrl'],
    ['--docs-url', 'docsUrl'],
    ['--showcase-url', 'showcaseUrl'],
    ['--out-landing', 'outLanding'],
    ['--out-docs', 'outDocs'],
  ]);

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--help' || arg === '-h') {
      opts.help = true;
      continue;
    }
    if (arg === '--check') {
      opts.check = true;
      continue;
    }
    if (valueFlags.has(arg)) {
      const next = argv[i + 1];
      if (next === undefined || next.startsWith('-')) {
        throw new Error(`Missing value for ${arg}`);
      }
      opts[valueFlags.get(arg)] = next;
      i += 1;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }

  return normalizeConfig(opts);
}

export function releaseTagUrl(config) {
  if (/^v\d/.test(config.version)) {
    return `${config.releasesUrl}/tag/${encodeURIComponent(config.version)}`;
  }
  return config.releasesUrl;
}

export function releaseAssetUrl(config, filename) {
  return `${config.releasesUrl}/download/${encodeURIComponent(config.version)}/${encodeURIComponent(filename)}`;
}

export function cliAssetFilename(version, target) {
  return `solo-${version}-${target.target}.${target.ext}`;
}
