# Studio draft previews (#67)

Draft previews let a workspace member look at unpublished work. The Studio Worker (`crates/sws-worker-studio`, module `preview`) serves them from a **separate, non-indexed origin** so that draft content can never reach Studio credentials or APIs and can only be framed by the Studio.

## Origins

| Var | Meaning |
| --- | --- |
| `STUDIO_PUBLIC_ORIGIN` | The Studio (API, sign-in, future Builder). |
| `STUDIO_PREVIEW_ORIGIN` | The preview host, routed to the same Worker. Unset means previews are off. |

Both must be plain origins accepted by `sws_security::headers::frame_origin`: `https://host[:port]`, or `http://` for `localhost` and `127.0.0.1` only. No path, trailing `/`, wildcard or userinfo.

The Worker decides the surface from the request host **before any other routing**:

- Host and port equal to the preview origin: **preview surface**. Only the preview routes below exist; every other path, including every Studio API, auth and asset route, is `404` with the `preview` headers.
- Any other host: **Studio surface**. Preview paths (`/p`, `/p/...`) are never served there (`404`).

Configuration fails closed. Previews are disabled, and both `POST .../previews` and any `/p/...` path answer `503 preview_origin_not_configured`, when `STUDIO_PREVIEW_ORIGIN` is unset or invalid, when `STUDIO_PUBLIC_ORIGIN` is unset or invalid, or when the two share a host. Sharing a host means the same host name (whatever the port: browsers scope cookies by host, not port) or one host being a dot-suffix of the other (a parent domain can set cookies its subdomains receive).

**Use a different registrable domain** (for example `studio.example.com` and `drafts.example-preview.net`). The Worker cannot check the Public Suffix List. Two sibling subdomains of one site pass the check but are same-site to browsers, so `SameSite` cookies would still flow between them.

Locally, one `wrangler dev` serves both: `STUDIO_PUBLIC_ORIGIN=http://127.0.0.1:<port>` and `STUDIO_PREVIEW_ORIGIN=http://localhost:<port>` (two host names, so two cookie jars).

## Producing a preview (Studio surface)

`POST /api/sites/{workspace_id}/{site_id}/previews?revision_id={id}&revision_sha256={ir_sha256}`

- Body: a preview bundle (below), `Content-Type: application/vnd.sws.preview-bundle`, with a `Content-Length` (`411` without one) of at most 5 MiB (`413`).
- Authentication: session cookie or bearer token (#65). Permission `edit`: any member role (owner, publisher, editor) and an `edit`-scoped token are enough. A token for another workspace, or a non-member, gets a uniform `403 forbidden`. Cookie-borne requests need `Origin` equal to `STUDIO_PUBLIC_ORIGIN` (`403 forbidden_origin`).
- The preview is pinned to an existing `site_revisions` row of that site: an unknown id is `404 revision_not_found`, and a sha256 that is not the row's `ir_sha256` is `409 revision_mismatch`.
- Storage: one generated artifact of kind `preview` (#63) under the existing Preview budget (5 MiB each, 10 MiB per site, 24 h retention, newest 3 kept). A full budget answers `409 generated_budget_exceeded`. After each upload only the newest 3 previews of the site are kept, so older links start answering `410`.
- Response: `201 {"ok":true,"preview_id","url","expires_at","revision_id","sha256","byte_size","file_count"}`, where `url` is `{STUDIO_PREVIEW_ORIGIN}/p/{site_id}/{revision_id}/{token}/`. The token is 128 random bits (32 lowercase hex) and is returned only in this response. D1 stores only its SHA-256. Preview bytes never change; a new upload creates a new preview.

`DELETE /api/sites/{workspace_id}/{site_id}/previews/{preview_id}` (same authentication and permission) revokes the link and deletes the artifact and its chunks at once. It answers `200 {"revoked":true}`, and `200` with `"already_revoked":true` when repeated. An unknown id is `404 preview_not_found`.

## The bundle format (`sws-preview-bundle/1`)

```text
sws-preview-bundle/1\n
<sha256> <size> <path>\n      one line per file, paths strictly ascending (bytewise)
\n                            an empty line ends the header
<file bytes, concatenated in header order, nothing after>
```

`sha256` is 64 lowercase hex and `size` is decimal without leading zeros. The header is ASCII and at most 192 KiB (so it always sits in the first stored 256 KiB chunk). The same file set always yields the same bytes (`preview::bundle::build`), so the artifact digest identifies the content. The format needs no new dependency.

The upload is refused (`400`, with `error` and, where it applies, the zero-based `file_index`, never the path itself) for any of these:

- the wrong magic line;
- an oversized or malformed header;
- more than 512 files;
- a path that is not `/`-separated `[A-Za-z0-9._~-]` segments, at most 200 bytes and 16 segments deep. Empty, `.` and `..` segments, dot-segments other than `.well-known`, `%`, backslash, NUL, control and non-ASCII bytes are all refused;
- the Cloudflare control files `_headers`, `_redirects`, `_routes.json` and `_worker.js`, which a preview never applies;
- unsorted or duplicate paths, paths equal ignoring ASCII case, or a file that is also a directory;
- sizes that do not add up to the body, or a per-file SHA-256 mismatch;
- binary bytes that do not match their extension (PNG, JPEG, GIF, WebP, AVIF, WOFF2, ICO);
- no root `index.html`.

Allowed types, by extension (case-insensitive):

| Extension | Served as |
| --- | --- |
| `html` | `text/html; charset=utf-8` |
| `css` | `text/css; charset=utf-8` |
| `js` | `text/javascript; charset=utf-8` |
| `json` | `application/json` |
| `txt` | `text/plain; charset=utf-8` |
| `xml` | `application/xml` |
| `svg` | `image/svg+xml` |
| `png`, `jpg`/`jpeg`, `gif`, `webp`, `avif`, `ico` | the matching `image/*` type |
| `woff2` | `font/woff2` |

SVG and XML can carry script. They are allowed because the draft HTML itself can too. Everything runs on the isolated preview origin under the `preview` CSP. PDF, WASM, source maps and extensionless files are refused.

## Serving (preview surface)

| Path | Answer |
| --- | --- |
| `/robots.txt` | `User-agent: *` / `Disallow: /` |
| `/p/{site}/{revision}/{token}` | `308` to the same path plus `/` |
| `/p/{site}/{revision}/{token}/{path}` | The file. An empty path, or one ending in `/`, serves that directory's `index.html`. A directory without the slash gets a `308` to add it. |

The preview surface has these properties:

- **Methods.** Only `GET` and `HEAD` are allowed; anything else is `405` with `Allow: GET, HEAD`.
- **Credentials.** The request's `Cookie` and `Authorization` headers are never read, nothing is forwarded, and no response sets a cookie.
- **Headers.** Every response, errors included, carries the `preview` header profile (`docs/ARCHITECTURE.md` §5), `Cache-Control: no-store` and `X-Robots-Tag: noindex, nofollow`.
- **Framing.** `frame-ancestors` names the validated `STUDIO_PUBLIC_ORIGIN`, or `'none'` if it is invalid. The CSP's `connect-src 'none'` and `form-action 'none'` mean a draft can neither call any API (the Studio's or its own) nor submit a form.
- **Integrity.** Each file's bytes are hashed as they stream from D1. A mismatch fails the transfer rather than serving corrupt bytes.

### 404 or 410

Two outcomes are distinguished:

- **`404 Not found`:** an unknown token, a valid token presented with another site or revision, a malformed path, or a missing file.
- **`410`:** the preview expired (24 h, or earlier if its artifact expired), was revoked, or was evicted by the keep-newest-3 rule.

Both carry the same generic plain-text body shape and the full header set. A `410` only tells someone who already holds a valid 128-bit token that it once worked, so it is not a guessing oracle, and it tells the viewer the link is stale rather than mistyped. A `previews` row is kept for 7 days after it expires and then swept (on the next upload to that site); after that, the link answers `404`.

### Root-absolute links

Previews live under a path prefix. A bundle whose HTML uses root-absolute URLs (`/styles.css`, `/events`), as the current compiler output does, resolves those against the preview host's root, where they are `404`. Producers must emit relative URLs for previews. Producing previews from the editor or the publish pipeline is #64/#75.

## Storage

Each preview is stored in two places:

- **The `previews` table.** Defined in `crates/sws-worker-studio/migrations/0006_previews.sql`; it holds the token digest, site, pinned revision, artifact id, author, `created_at` and `expires_at` (copied from the artifact, at most 24 h), and `revoked_at`. Rows are immutable except a one-way revoke (enforced by a trigger).
- **The `generated_artifacts` and `blob_chunks` tables (#63).** These hold the bundle bytes. The preview upload shares the per-isolate upload-memory guard and download slots with asset uploads.

## Local tests and what they prove

| Command | What it proves |
| --- | --- |
| `cargo test -p sws-worker-studio` | Bundle validation with hostile inputs, path resolution, the origin-dispatch matrix (unset, equal, same host on another port, parent/child hosts fail closed), token format, the 404/410 decision and expiry boundary, the exact header set on every preview response. |
| `npm run test:studio` (`tests/unit/studio-previews.test.mjs`) | The real migrations and the exact SQL: tenant and revision pinning, token-hash uniqueness, the 24 h CHECK, one-way revocation, eviction and revocation leaving the row pointing at nothing, bounded row sweep. |
| `npm run test:studio:previews` (Worker on port 8796 plus a stand-in Studio shell on 8797, own `.wrangler/studio-previews-state`; run under `flock /tmp/solo-studio-8796.lock` on shared machines) | The release Worker end to end: producer authorization and tenant denial, revision pin, hostile bundles refused with nothing stored, exact `preview` headers on files and errors, credentials ignored on the preview host, Studio routes unreachable there, preview paths `404` on the Studio host, expiry/revocation/eviction `410`, a multi-chunk file and a near-cap bundle byte-exact, then Chromium: a page on the configured Studio origin can frame the preview, a third origin cannot, the framed draft sees no Studio cookie and cannot `fetch` a Studio API, and a restart with the preview origin on the Studio host fails closed. |

The browser checks frame the preview from a small stand-in shell that the suite serves on the configured `STUDIO_PUBLIC_ORIGIN`. The Worker's own `studio` profile (`default-src 'none'`) lets no Studio page frame anything yet. The Builder shell that will frame previews needs `frame-src` for the preview origin (#72/#77).
