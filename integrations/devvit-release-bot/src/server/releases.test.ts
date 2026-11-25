import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parseAppConfig } from '@devvit/shared-types/schemas/config-file.v1.js';

import {
  formatReleaseTitle,
  selectLatestRelease,
  syncLatestRelease,
  type ReleaseStore,
} from './releases.ts';

function createMemoryStore(initial: Record<string, string> = {}): ReleaseStore & {
  records: Map<string, string>;
} {
  const records = new Map(Object.entries(initial));
  return {
    records,
    async hasPosted(tag: string) {
      return records.has(tag);
    },
    async markPosted(tag: string, postId: string) {
      records.set(tag, postId);
    },
  };
}

describe('formatReleaseTitle', () => {
  it('formats standard semver tags without a body description', () => {
    assert.equal(formatReleaseTitle('v0.1.0'), 'Solo Release 0.1.0');
    assert.equal(formatReleaseTitle('V1.2.0'), 'Solo Release 1.2.0');
    assert.equal(formatReleaseTitle('0.3.0'), 'Solo Release 0.3.0');
  });

  it('rejects invalid tags', () => {
    assert.throws(() => formatReleaseTitle(''), /empty/);
    assert.throws(() => formatReleaseTitle('v'), /version/);
    assert.throws(() => formatReleaseTitle('v0.1.0\nbad'), /control characters/);
  });
});

describe('selectLatestRelease', () => {
  it('skips drafts, prereleases, and non-CLI pack tags', () => {
    const selected = selectLatestRelease([
      {
        tag_name: 'pack-hospitality-v0.2.0',
        html_url: 'https://github.com/jcusoftware/solo/releases/tag/pack-hospitality-v0.2.0',
        draft: false,
        prerelease: false,
      },
      {
        tag_name: 'v0.2.0-rc.1',
        html_url: 'https://github.com/jcusoftware/solo/releases/tag/v0.2.0-rc.1',
        draft: false,
        prerelease: true,
      },
      {
        tag_name: 'v0.2.0',
        html_url: 'https://github.com/jcusoftware/solo/releases/tag/v0.2.0',
        draft: true,
        prerelease: false,
      },
      {
        tag_name: 'v0.1.0',
        html_url: 'https://github.com/jcusoftware/solo/releases/tag/v0.1.0',
        draft: false,
        prerelease: false,
      },
    ]);

    assert.deepEqual(selected, {
      tag: 'v0.1.0',
      title: 'Solo Release 0.1.0',
      url: 'https://github.com/jcusoftware/solo/releases/tag/v0.1.0',
    });
  });

  it('returns null when no eligible release exists', () => {
    assert.equal(selectLatestRelease([]), null);
    assert.equal(
      selectLatestRelease([
        {
          tag_name: 'v0.1.0',
          html_url: 'https://evil.example/not-github',
          draft: false,
          prerelease: false,
        },
      ]),
      null,
    );
  });
});

describe('syncLatestRelease', () => {
  it('submits once and stays idempotent on subsequent checks', async () => {
    const store = createMemoryStore();
    const submitted: Array<{ title: string; url: string }> = [];

    const mockFetch = (async () =>
      new Response(
        JSON.stringify([
          {
            tag_name: 'v0.1.0',
            html_url: 'https://github.com/jcusoftware/solo/releases/tag/v0.1.0',
            draft: false,
            prerelease: false,
          },
        ]),
        { status: 200, headers: { 'content-type': 'application/json' } },
      )) as typeof globalThis.fetch;

    const first = await syncLatestRelease({
      repo: 'jcusoftware/solo',
      fetchImpl: mockFetch,
      store,
      submitLinkPost: async (input) => {
        submitted.push(input);
        return { id: 't3_post1' };
      },
    });

    assert.deepEqual(first, {
      status: 'posted',
      tag: 'v0.1.0',
      title: 'Solo Release 0.1.0',
      url: 'https://github.com/jcusoftware/solo/releases/tag/v0.1.0',
      postId: 't3_post1',
    });
    assert.equal(submitted.length, 1);

    const second = await syncLatestRelease({
      repo: 'jcusoftware/solo',
      fetchImpl: mockFetch,
      store,
      submitLinkPost: async (input) => {
        submitted.push(input);
        return { id: 't3_post2' };
      },
    });

    assert.equal(second.status, 'already-posted');
    assert.equal(submitted.length, 1);
  });

  it('handles empty release feeds and non-200 GitHub errors', async () => {
    const store = createMemoryStore();
    const emptyRes = await syncLatestRelease({
      repo: 'jcusoftware/solo',
      fetchImpl: (async () => new Response('[]', { status: 200 })) as typeof globalThis.fetch,
      store,
      submitLinkPost: async () => ({ id: 'unused' }),
    });
    assert.deepEqual(emptyRes, { status: 'no-release' });

    await assert.rejects(
      () =>
        syncLatestRelease({
          repo: 'jcusoftware/solo',
          fetchImpl: (async () => new Response('rate limit', { status: 403 })) as typeof globalThis.fetch,
          store,
          submitLinkPost: async () => ({ id: 'unused' }),
        }),
      /status 403/,
    );
  });
});

describe('devvit.json manifest', () => {
  it('validates against the official Devvit v1 config schema', () => {
    const raw = readFileSync(resolve(import.meta.dirname, '../../devvit.json'), 'utf8');
    const parsed = parseAppConfig(raw, false);

    assert.equal(parsed.name, 'solo-release-bot');
    assert.equal(parsed.permissions.http.enable, true);
    assert.deepEqual(parsed.permissions.http.domains, ['api.github.com']);
    assert.equal(parsed.permissions.redis, true);
    assert.equal(parsed.permissions.reddit.enable, true);
    assert.equal(
      parsed.scheduler?.tasks['check-github-releases']?.endpoint,
      '/internal/scheduler/check-releases',
    );
    assert.equal(parsed.scheduler?.tasks['check-github-releases']?.cron, '*/15 * * * *');
    assert.equal(parsed.menu?.items[0]?.endpoint, '/internal/menu/check-releases');
  });
});
