# Solo Release Bot (Devvit)

Reddit Devvit integration that watches GitHub Releases for [`jcusoftware/solo`](https://github.com/jcusoftware/solo) and posts each new published `v*` release as a title-only link post (for example, `Solo Release 0.1.0`) to [`r/soloweb`](https://www.reddit.com/r/soloweb) under the dedicated Devvit app account (`u/solo-release-bot`).

## How It Fits Into Solo's Release Flow

1. You tag and push a release (`git tag -a v0.1.0 -m "Release v0.1.0" && git push origin v0.1.0`).
2. [`.github/workflows/release.yml`](../../.github/workflows/release.yml) runs Rust/Wasm checks, builds cross-platform `solo` binaries, publishes the GitHub Release, and deploys `sws-landing` (`https://solo.jcusoftware.com`) and `sws-docs` (`https://docs.jcusoftware.com`) to Cloudflare Workers.
3. This Devvit app checks GitHub Releases every 15 minutes via Devvit Scheduler (or immediately via the moderator menu item **Check for Solo release** in `r/soloweb`), tracks announced tags in Devvit Redis so each tag is posted only once, and submits a link post pointing to the GitHub Release URL with no body text.

Non-CLI tags such as `pack-hospitality-v*`, drafts, and prereleases are ignored.

## Fetch Domains

The following domains are requested in [`devvit.json`](./devvit.json) (`permissions.http.domains`):

- `api.github.com` — Read-only `GET /repos/jcusoftware/solo/releases?per_page=10` lookup of public release tags (`tag_name`) and release page links (`html_url`).

## Local Verification

```bash
cd integrations/devvit-release-bot
npm install
npm run build
npm test
```

## Deploy to `r/soloweb`

1. Log in to Reddit's Devvit CLI using the moderator account that manages `r/soloweb`:

   ```bash
   npx devvit login
   ```

2. Build and upload the app (the first upload registers the app slug from `devvit.json` — if `solo-release-bot` is taken, update `"name"` in `devvit.json` first):

   ```bash
   npm run build
   npx devvit upload
   ```

3. Install the app onto `r/soloweb`:

   ```bash
   npx devvit install r/soloweb
   ```

4. Approve external HTTP fetch for `api.github.com`:
   - Uploading/playtesting submits `api.github.com` to Reddit's domain review queue automatically (usually reviewed within 1–2 business days).
   - Check approval status in the Devvit Developer Portal under your app's **Developer Settings** (`https://developers.reddit.com`). Until `api.github.com` is approved, scheduled or menu checks will log `PERMISSION_DENIED` on outbound `fetch()`.
