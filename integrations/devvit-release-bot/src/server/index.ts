import type { IncomingMessage, ServerResponse } from 'node:http';
import {
  context,
  createServer,
  getServerPort,
  reddit,
  redis,
  settings,
} from '@devvit/web/server';
import type { UiResponse } from '@devvit/web/shared';

import { syncLatestRelease, type ReleaseStore, type SyncResult } from './releases.ts';

const DEFAULT_REPO = 'jcusoftware/solo';
const DEFAULT_PRODUCT_NAME = 'Solo';

function sendJson(res: ServerResponse, statusCode: number, payload: unknown): void {
  const body = JSON.stringify(payload);
  res.writeHead(statusCode, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': Buffer.byteLength(body),
  });
  res.end(body);
}

function createRedisStore(): ReleaseStore {
  return {
    async hasPosted(tag: string): Promise<boolean> {
      const existing = await redis.get(`solo:posted:${tag}`);
      return typeof existing === 'string' && existing.length > 0;
    },
    async markPosted(tag: string, postId: string): Promise<void> {
      await redis.set(`solo:posted:${tag}`, postId);
    },
  };
}

async function runReleaseSync(): Promise<SyncResult> {
  const subredditName = context.subredditName;
  if (!subredditName) {
    throw new Error('Missing subredditName in Devvit request context');
  }

  const configuredRepo = await settings.get<string>('githubRepo');
  const configuredProduct = await settings.get<string>('productName');
  const repo = configuredRepo?.trim() || DEFAULT_REPO;
  const productName = configuredProduct?.trim() || DEFAULT_PRODUCT_NAME;

  return syncLatestRelease({
    repo,
    productName,
    store: createRedisStore(),
    submitLinkPost: async ({ title, url }) => {
      const post = await reddit.submitPost({
        subredditName,
        title,
        url,
      });
      return { id: post.id };
    },
  });
}

function formatToast(result: SyncResult): UiResponse {
  switch (result.status) {
    case 'posted':
      return {
        showToast: {
          text: `Posted "${result.title}"`,
          appearance: 'success',
        },
      };
    case 'already-posted':
      return {
        showToast: {
          text: `${result.title} is already posted.`,
          appearance: 'neutral',
        },
      };
    case 'no-release':
      return {
        showToast: {
          text: 'No published v* GitHub release found yet.',
          appearance: 'neutral',
        },
      };
  }
}

async function handleRequest(req: IncomingMessage, res: ServerResponse): Promise<void> {
  req.resume();
  const pathname = new URL(req.url ?? '/', 'http://webbit.local').pathname;

  if (req.method !== 'POST') {
    sendJson(res, 405, { error: 'Method not allowed' });
    return;
  }

  try {
    if (pathname === '/internal/scheduler/check-releases') {
      const result = await runReleaseSync();
      console.log('Scheduled Solo release check:', JSON.stringify(result));
      sendJson(res, 200, result);
      return;
    }

    if (pathname === '/internal/menu/check-releases') {
      const result = await runReleaseSync();
      console.log('Manual Solo release check:', JSON.stringify(result));
      sendJson(res, 200, formatToast(result));
      return;
    }

    sendJson(res, 404, { error: `Unknown endpoint: ${pathname}` });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`Devvit release check failed (${pathname}):`, message);
    if (pathname === '/internal/menu/check-releases') {
      sendJson(res, 200, {
        showToast: {
          text: `Release check failed: ${message}`,
          appearance: 'neutral',
        },
      } satisfies UiResponse);
      return;
    }
    sendJson(res, 500, { error: message });
  }
}

const server = createServer(handleRequest);
server.listen(getServerPort(), () => {
  console.log(`solo-release-bot listening on port ${getServerPort()}`);
});
