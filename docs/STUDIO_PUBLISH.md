# Studio publishing (Direct Upload) and rollback

`MIT OR Apache-2.0`

Implemented by issue #66 in `crates/sws-worker-studio/src/publish/`. Studio publishes a compiled site to its **standalone** site Worker through Cloudflare's Workers static-assets Direct Upload API, and can roll a site back to a retained, verified deployment. Pure logic (manifest and hashing, multipart encoding, version metadata, the Cloudflare client with retries, the activation protocol, rollback policy, SQL) is host-tested against a Cloudflare fake (`tests/fakes/cloudflare/fake_api.rs`); only the `fetch` transport, D1 adapter and route glue are wasm-only. Publish jobs (#64) call `publish::run_publish`; this module does no queueing.

The API shapes below were checked against the Cloudflare documentation on 2026-11-27 (Direct Upload guide, the Workers Scripts / Assets / Versions / Deployments API reference, multipart upload metadata, static-asset headers, rollbacks, limits). Points the documentation did not settle are listed under [Assumptions](#assumptions) and are part of the human publish check (#69).

## Sequence

1. **Prepare** (no API call). Load the site's publish target, refuse a job whose revision is not newer than the highest revision ever activated there (`stale_revision`), validate the target's bindings, check that the site Worker bundle expects the target's current site D1 migration level (`schema_mismatch`), resolve the API token, hash every artifact, and compare the manifest digest with the job's pin (`artifact_mismatch`).
2. **Upload session.** `POST /accounts/{account}/workers/scripts/{script}/assets-upload-session` with the whole manifest (`{path: {hash, size}}`). Hash = first 32 hex of `sha256(base64(content) + extension)`. Cloudflare answers an upload JWT and `buckets` of the hashes it does not already hold. Empty buckets mean nothing needs uploading: the JWT is already the completion token (an identical republish uploads zero files).
3. **Upload.** `POST /accounts/{account}/workers/assets/upload?base64=true`, `multipart/form-data`, one part per hash (base64 body, the file's `Content-Type`), authorized with the upload JWT, never the account token. The final upload answers 201 with the completion token. `_headers` and `_redirects` are not uploaded as assets; they travel in the version metadata (`assets.config._headers` / `_redirects`), so the static security headers (AGENTS.md §4) still apply.
4. **Create version.** `POST /accounts/{account}/workers/scripts/{script}/versions` (multipart: JSON `metadata` plus the site Worker modules). The metadata carries `assets.jwt`, `assets.config.run_worker_first: ["/api/*"]`, `not_found_handling: "404-page"`, `html_handling: "auto-trailing-slash"`, the `ASSETS` binding, the target's stored D1 / plain-text / rate-limit bindings, and `keep_bindings: ["secret_text", "secret_key"]`, so Worker secrets such as `TURNSTILE_SECRET_KEY` stay on the Worker and Solo never sees them. This does **not** deploy anything.
5. **Activate**, under the target's lease (below): `POST .../deployments` with `strategy: "percentage"` and the version at 100%, then **verify** with `GET .../deployments` that it is the first deployment, at 100%.

Steps 2, 3, 4 and the deployments list retry 429, 5xx and transport errors (4 attempts, exponential backoff from 500 ms, `Retry-After` honoured, capped at 8 s). A failed bucket after an ambiguous attempt is re-planned once with a fresh upload session. Nothing before step 5 can change live traffic: any earlier failure leaves the last known good version serving and is reported with its stage and the pinned revision.

## Activation protocol

Each target row (`publish_targets`) carries a lease (`activating_job`, `lease_expires_at`, 120 s) and a high-water mark (`high_water_seq`, the highest revision sequence ever activated).

- **Serialized.** The lease is taken with a conditional `UPDATE ... WHERE activating_job IS NULL AND high_water_seq < ?seq`. A second activation on the same target is refused `target_busy` (retryable).
- **Ordered.** A job whose revision is not above the high-water mark is refused `stale_revision`, before uploading and again atomically at the lease. An older job can never activate over a newer version, also when the newer one finished while the older one was still uploading.
- **Never blind.** The lease is renewed immediately before every activation request; each request is sent once with a 30 s bound. A definitive rejection (4xx) stops. Any other outcome (timeout, transport error, 429, 5xx) is reconciled with the deployments list first: serving means done (no second request); not serving means retry (at most 3 activation requests in total); unreadable means `activation_unknown`.
- **Unknown outcomes are kept, not guessed.** On `activation_unknown` the lease is kept until it expires. The next holder reconciles the stale holder's version with Cloudflare before acting: if it is serving, it is adopted (live version and high-water mark recorded, `lease_takeover` audit row); if not, the lease is cleared; if the list is still unreadable, the target stays busy.
- **Recorded atomically.** One D1 batch sets the target live (only while the job holds the lease and the revision is above the mark), inserts the retained version only if that update applied, and appends the audit row.

There is no compare-and-swap precondition in the Deployments API, so the lease is enforced on Solo's side. Deploys made outside Solo (dashboard, `wrangler`) are detected at verify time (`activation_superseded`) but not prevented.

## Failure report

`PublishFailure` carries `stage` (`prepare`, `upload_session`, `upload_assets`, `create_version`, `lock`, `activate`, `verify`, `record`), `revision_id` and `revision_seq`, a `code`, a redacted `message`, `retryable`, `activation` (`not_attempted`, `not_activated`, `unknown`, `activated`) and the target's last known good version.

## Rollback

`POST /api/sites/{workspace_id}/{site_id}/deployments/{published_id}/rollback` (Studio header profile, `Cache-Control: no-store`; optional JSON object body). `published_id` names a row of `published_versions`, which holds only versions Solo activated and then saw serving 100% of traffic.

- **Authorized.** `publish::require_activate`: workspace role `owner` or `publisher` (`403 forbidden` otherwise; non-members get the same 403 and no audit row is written into that workspace). Until #65 ships, authentication rejects everyone (`401`). #65 adds the `activate` token scope and the 15-minute re-authentication window in that one function.
- **Audited.** Every refusal by role or compatibility, every activation that was attempted and failed, and every success appends a `publish_audit` row (actor, action, target, version, result, stage, code, time); so does every publish activation failure and every lease takeover. Lease contention (`target_busy`) and unknown ids are not audited. Rows are append-only.
- **No rewind, no replay.** Rollback creates one deployment of an existing Cloudflare version under the same lease and reconcile protocol. It never uploads, never creates a version, never touches `site_revisions` or the editing session, and never re-runs a job. The high-water mark is unchanged, so jobs for older revisions stay refused. Going forward means publishing a newer revision.
- **Compatibility.** Each retained version records the site D1 migration level its Worker was built for (`schema_compat`). If the target's `site_schema_level` differs, the rollback is refused `409 rollback_schema_incompatible` with a forward-fix message. If Cloudflare refuses the deployment (version past retention, secrets changed since), the answer is `409 rollback_rejected`; Solo never retries with `force`.
- Other answers: `404 deployment_not_found`, `404 target_not_found`, `409 already_live`, `409 target_busy`, `502` for an unconfirmed or failed activation.

Whoever applies site D1 migrations must update `publish_targets.site_schema_level` (statement `TARGET_SET_SCHEMA_LEVEL`) in the same change window; the CLI (#68) is expected to own this.

## Credentials

- **Token.** A Cloudflare API token stored as a Studio Worker secret, by default `CLOUDFLARE_API_TOKEN` (`wrangler secret put CLOUDFLARE_API_TOKEN -c <studio config>`). A target may name another secret matching `CF_PUBLISH_TOKEN_[A-Z0-9_]+` (`publish_targets.token_secret`, a name, never a value). The token is read only when a publish or rollback runs, sent only to `https://api.cloudflare.com/client/v4` (a constant, not configurable), never to the upload endpoint, and redacted from every error, log line and audit row together with the per-session upload JWTs.
- **Scope (least privilege).** An account API token with **Account · Workers Scripts · Edit** (`Workers Scripts Write` in the API reference), restricted to the single account that hosts the site Worker, with no zone permissions. The upload-session endpoint accepts only `Workers Scripts Write` or `Workers Admin` (per-Worker "Individual Workers" permissions are accepted by the version and deployment endpoints but not by upload-session, per the reference read on 2026-11-27). Add an IP filter and an expiry if your setup allows. Rotate by putting a new secret value; no D1 change is needed.
- The site Worker must already exist (first deploy it once with `wrangler deploy` and set its secrets there); Studio then publishes new versions of it.

## Standalone site

The published version is self-contained. Bindings are restricted to `d1`, `plain_text` and `ratelimit` (plus Solo's own `assets`); `service`, Durable Object, dispatch, queue, secret and any other binding types in a target's configuration are refused (`invalid_target_bindings`), so a published site has no binding or route back to Studio and keeps serving when Studio is down. `DB` (D1) and `TURNSTILE_SITE_KEY` are required. The host tests assert this on the uploaded metadata. Serving with Studio stopped on real Cloudflare is human evidence (#69).

## Limits

At most 20,000 assets per version (the Workers Free figure) and 10 MiB per file. That file bound is Solo's own, below Cloudflare's 25 MiB, so a file's base64 and multipart copies stay far below the 128 MB isolate limit. Upload requests are batched to about 12 MiB of base64 each.

## Assumptions

Each of these was not settled by the documentation and is checked in the first real publish (#69):

- the Version Upload endpoint accepts the same `assets.config` keys (`run_worker_first`, `_headers`, `_redirects`, `html_handling`, `not_found_handling`) that the Script Upload reference lists;
- `keep_bindings` keeps secrets from the latest uploaded version (which, after a failed publish, may be a version that was never deployed but was created with the same bindings);
- a Cloudflare bucket may be sent in several upload requests;
- the completion token remains valid for a retried version upload within its hour.

## Tests

`cargo test -p sws-worker-studio` (fake-driven: full sequence, dedup, every failure stage, bounded retries, no blind activation retry, ambiguous outcomes reconciled both ways, stale and concurrent jobs, lease loss and takeover, rollback allowed and refused, token redaction, standalone metadata) and `tests/unit/studio-publish-sql.test.mjs` (the real migrations and statements on SQLite: lease, ordering, atomic record, rollback constraints, immutability, tenant scoping). Both run in `npm run verify`.
