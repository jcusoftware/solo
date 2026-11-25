// Pure GitHub release selection and idempotent Devvit sync logic (host-testable).

const CONTROL_CHAR_RE = /[\u0000-\u001F\u007F]/;
const REPO_RE = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const CLI_TAG_RE = /^v\d/i;

export interface GitHubRelease {
  tag_name?: unknown;
  html_url?: unknown;
  draft?: unknown;
  prerelease?: unknown;
}

export interface SelectedRelease {
  tag: string;
  title: string;
  url: string;
}

export interface ReleaseStore {
  hasPosted(tag: string): Promise<boolean>;
  markPosted(tag: string, postId: string): Promise<void>;
}

export interface SubmitLinkPostInput {
  title: string;
  url: string;
}

export type SyncResult =
  | { status: 'no-release' }
  | { status: 'already-posted'; tag: string; title: string; url: string }
  | { status: 'posted'; tag: string; title: string; url: string; postId: string };

/**
 * Formats a git tag (e.g. "v0.1.0") into "Solo Release 0.1.0".
 */
export function formatReleaseTitle(tag: string, productName = 'Solo'): string {
  if (typeof tag !== 'string') {
    throw new Error('Release tag must be a string');
  }
  const trimmed = tag.trim();
  if (!trimmed) {
    throw new Error('Release tag must not be empty');
  }
  if (CONTROL_CHAR_RE.test(trimmed)) {
    throw new Error('Release tag must not contain control characters');
  }
  const version = trimmed.replace(/^[vV]/, '');
  if (!version) {
    throw new Error('Release tag must include a version after v');
  }
  const cleanProduct = productName.trim() || 'Solo';
  return `${cleanProduct} Release ${version}`;
}

/**
 * Selects the newest published semver `v*` release (ignoring drafts, prereleases, and pack tags).
 */
export function selectLatestRelease(
  releases: readonly GitHubRelease[],
  productName = 'Solo',
): SelectedRelease | null {
  if (!Array.isArray(releases)) {
    return null;
  }

  for (const release of releases) {
    if (!release || release.draft === true || release.prerelease === true) {
      continue;
    }
    if (typeof release.tag_name !== 'string' || typeof release.html_url !== 'string') {
      continue;
    }
    const tag = release.tag_name.trim();
    const url = release.html_url.trim();
    if (!CLI_TAG_RE.test(tag)) {
      continue;
    }
    if (!url.startsWith('https://github.com/')) {
      continue;
    }
    return {
      tag,
      title: formatReleaseTitle(tag, productName),
      url,
    };
  }

  return null;
}

export interface SyncLatestReleaseOptions {
  repo: string;
  productName?: string;
  fetchImpl?: typeof globalThis.fetch;
  store: ReleaseStore;
  submitLinkPost: (input: SubmitLinkPostInput) => Promise<{ id: string }>;
}

/**
 * Checks GitHub Releases for `repo` and submits a Reddit link post once per tag.
 */
export async function syncLatestRelease(options: SyncLatestReleaseOptions): Promise<SyncResult> {
  const repo = options.repo.trim();
  if (!REPO_RE.test(repo)) {
    throw new Error(`Invalid GitHub repository slug: ${JSON.stringify(options.repo)}`);
  }

  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  const response = await fetchImpl(
    `https://api.github.com/repos/${repo}/releases?per_page=10`,
    {
      headers: {
        Accept: 'application/vnd.github+json',
        'User-Agent': 'solo-release-bot-devvit',
      },
    },
  );

  if (!response.ok) {
    throw new Error(`GitHub releases API returned status ${response.status}`);
  }

  const payload = (await response.json()) as GitHubRelease[];
  const latest = selectLatestRelease(payload, options.productName ?? 'Solo');
  if (!latest) {
    return { status: 'no-release' };
  }

  if (await options.store.hasPosted(latest.tag)) {
    return {
      status: 'already-posted',
      tag: latest.tag,
      title: latest.title,
      url: latest.url,
    };
  }

  const post = await options.submitLinkPost({
    title: latest.title,
    url: latest.url,
  });

  await options.store.markPosted(latest.tag, post.id);

  return {
    status: 'posted',
    tag: latest.tag,
    title: latest.title,
    url: latest.url,
    postId: post.id,
  };
}
