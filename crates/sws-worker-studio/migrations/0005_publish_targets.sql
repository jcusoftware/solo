-- SWS Studio publish targets v1 (#66): Direct Upload publishing to the standalone
-- site Worker. Numbered 0005 on purpose: 0003 is reserved for auth (#65) and 0004 for
-- publish jobs (#64), which land separately. This migration is ADDITIVE ONLY: it adds
-- new tables and does not alter `deployments` (0001), which #64 owns and extends with
-- job phase/attempt/lease columns. A job row and a retained version are linked by
-- `published_versions.job_id` (no FK until #64's job table shape is settled).
--
-- Ids are app-generated opaque TEXT (1..64 chars of [a-z0-9_-]); timestamps are
-- canonical UTC text checked like 0001. Cloudflare version ids are UUIDs.
-- No secret is ever stored here: `token_secret` is the NAME of a Worker secret.

-- One publish target (one site Worker on one Cloudflare account) per site.
-- Activation lease: `activating_job` + `lease_expires_at` serialize activations per
-- target; `activating_seq` / `activating_version_id` record what the holder is
-- activating, so a later holder can reconcile an expired lease with Cloudflare.
-- `high_water_seq` is the highest revision seq ever activated; a publish must exceed
-- it (an older job can never overtake a newer one) and rollback never lowers it.
CREATE TABLE IF NOT EXISTS publish_targets (
    workspace_id          TEXT    NOT NULL,
    site_id               TEXT    NOT NULL,
    id                    TEXT    NOT NULL PRIMARY KEY
                                  CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    account_id            TEXT    NOT NULL
                                  CHECK (length(account_id) = 32 AND account_id NOT GLOB '*[^0-9a-f]*'),
    script_name           TEXT    NOT NULL
                                  CHECK (length(script_name) BETWEEN 1 AND 63 AND script_name NOT GLOB '*[^a-z0-9_-]*'),
    token_secret          TEXT    NOT NULL DEFAULT 'CLOUDFLARE_API_TOKEN'
                                  CHECK (token_secret = 'CLOUDFLARE_API_TOKEN'
                                      OR (length(token_secret) BETWEEN 18 AND 64
                                          AND token_secret GLOB 'CF_PUBLISH_TOKEN_*'
                                          AND token_secret NOT GLOB '*[^A-Z0-9_]*')),
    bindings_json         TEXT    NOT NULL
                                  CHECK (length(bindings_json) <= 131072 AND json_valid(bindings_json)
                                         AND json_type(bindings_json) = 'array'),
    compatibility_date    TEXT    NOT NULL
                                  CHECK (date(compatibility_date) IS compatibility_date),
    site_schema_level     INTEGER NOT NULL CHECK (site_schema_level >= 0),
    live_version_id       TEXT    CHECK (live_version_id IS NULL OR (length(live_version_id) = 36
                                         AND live_version_id NOT GLOB '*[^0-9a-f-]*')),
    -- No FK: set in the same batch as the retained row is inserted (see VERSION_INSERT_IF_LIVE).
    live_published_id     TEXT    CHECK (live_published_id IS NULL OR (length(live_published_id) BETWEEN 1 AND 64
                                         AND live_published_id NOT GLOB '*[^a-z0-9_-]*')),
    live_revision_id      TEXT    CHECK (live_revision_id IS NULL OR length(live_revision_id) BETWEEN 1 AND 64),
    high_water_seq        INTEGER NOT NULL DEFAULT 0 CHECK (high_water_seq >= 0),
    activating_job        TEXT    CHECK (activating_job IS NULL OR (length(activating_job) BETWEEN 1 AND 64
                                         AND activating_job NOT GLOB '*[^a-z0-9_-]*')),
    activating_seq        INTEGER CHECK (activating_seq IS NULL OR activating_seq >= 1),
    activating_version_id TEXT    CHECK (activating_version_id IS NULL OR (length(activating_version_id) = 36
                                         AND activating_version_id NOT GLOB '*[^0-9a-f-]*')),
    lease_expires_at      TEXT    CHECK (lease_expires_at IS NULL
                                         OR strftime('%Y-%m-%dT%H:%M:%SZ', lease_expires_at) IS lease_expires_at),
    created_at            TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                                  CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    updated_at            TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                                  CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', updated_at) IS updated_at),
    -- A lease is all-or-nothing: holder, version and expiry together.
    CHECK ((activating_job IS NULL) = (lease_expires_at IS NULL)
       AND (activating_job IS NULL) = (activating_version_id IS NULL)
       AND (activating_job IS NOT NULL OR activating_seq IS NULL)),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE,
    UNIQUE (workspace_id, site_id),
    UNIQUE (workspace_id, id)
) STRICT;

-- Retained, VERIFIED deployments: Cloudflare versions Solo activated and then saw
-- serving 100% of traffic. Only these can be rollback targets. Immutable.
-- `schema_compat` is the site D1 migration level the version's Worker was built for;
-- rollback is refused when it differs from the target's current `site_schema_level`.
CREATE TABLE IF NOT EXISTS published_versions (
    workspace_id    TEXT    NOT NULL,
    site_id         TEXT    NOT NULL,
    id              TEXT    NOT NULL PRIMARY KEY
                            CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    target_id       TEXT    NOT NULL,
    cf_version_id   TEXT    NOT NULL
                            CHECK (length(cf_version_id) = 36 AND cf_version_id NOT GLOB '*[^0-9a-f-]*'),
    revision_id     TEXT    NOT NULL,
    revision_seq    INTEGER NOT NULL CHECK (revision_seq >= 1),
    job_id          TEXT    CHECK (job_id IS NULL OR (length(job_id) BETWEEN 1 AND 64 AND job_id NOT GLOB '*[^a-z0-9_-]*')),
    schema_compat   INTEGER NOT NULL CHECK (schema_compat >= 0),
    manifest_sha256 TEXT    NOT NULL CHECK (length(manifest_sha256) = 64 AND manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
    verified_at     TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', verified_at) IS verified_at),
    created_by      INTEGER NOT NULL REFERENCES users (id),
    FOREIGN KEY (workspace_id, target_id) REFERENCES publish_targets (workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, site_id, revision_id)
        REFERENCES site_revisions (workspace_id, site_id, id) ON DELETE NO ACTION,
    UNIQUE (workspace_id, target_id, cf_version_id)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_published_versions_site
    ON published_versions (workspace_id, site_id, revision_seq);

CREATE TRIGGER IF NOT EXISTS trg_published_versions_immutable
BEFORE UPDATE ON published_versions
BEGIN
    SELECT RAISE(ABORT, 'published_versions rows are immutable');
END;

-- Append-only audit of activations, rollbacks and lease takeovers. Ids and codes only:
-- never a token, JWT or request body. `actor_user_id` NULL = the system.
CREATE TABLE IF NOT EXISTS publish_audit (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id  TEXT    NOT NULL,
    site_id       TEXT    NOT NULL,
    target_id     TEXT    NOT NULL,
    actor_user_id INTEGER REFERENCES users (id),
    action        TEXT    NOT NULL CHECK (action IN ('publish_activate', 'rollback', 'lease_takeover')),
    result        TEXT    NOT NULL CHECK (result IN ('succeeded', 'failed', 'refused')),
    cf_version_id TEXT    CHECK (cf_version_id IS NULL OR (length(cf_version_id) = 36
                                 AND cf_version_id NOT GLOB '*[^0-9a-f-]*')),
    published_id  TEXT    CHECK (published_id IS NULL OR (length(published_id) BETWEEN 1 AND 64
                                 AND published_id NOT GLOB '*[^a-z0-9_-]*')),
    revision_id   TEXT    CHECK (revision_id IS NULL OR length(revision_id) BETWEEN 1 AND 64),
    job_id        TEXT    CHECK (job_id IS NULL OR length(job_id) BETWEEN 1 AND 64),
    stage         TEXT    CHECK (stage IS NULL OR stage IN ('prepare', 'upload_session', 'upload_assets',
                                 'create_version', 'lock', 'activate', 'verify', 'record')),
    error_code    TEXT    CHECK (error_code IS NULL OR (length(error_code) BETWEEN 1 AND 64
                                 AND error_code NOT GLOB '*[^a-z0-9_]*')),
    at            TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', at) IS at),
    FOREIGN KEY (workspace_id, target_id) REFERENCES publish_targets (workspace_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS idx_publish_audit_site ON publish_audit (workspace_id, site_id, id);

CREATE TRIGGER IF NOT EXISTS trg_publish_audit_no_update
BEFORE UPDATE ON publish_audit
BEGIN
    SELECT RAISE(ABORT, 'publish_audit rows are append-only');
END;

-- Verified compiled bundles uploaded via PUT /api/sites/{ws}/{site}/publish/bundles/{manifest_sha256},
-- stored in 256 KiB chunks. Content-addressed per site by `manifest_sha256`.
CREATE TABLE IF NOT EXISTS publish_bundles (
    workspace_id    TEXT    NOT NULL,
    site_id         TEXT    NOT NULL,
    manifest_sha256 TEXT    NOT NULL
                            CHECK (length(manifest_sha256) = 64 AND manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
    total_bytes     INTEGER NOT NULL CHECK (total_bytes >= 0),
    chunk_count     INTEGER NOT NULL CHECK (chunk_count >= 1),
    created_by      INTEGER NOT NULL REFERENCES users (id),
    created_at      TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    PRIMARY KEY (workspace_id, site_id, manifest_sha256),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS publish_bundle_chunks (
    workspace_id    TEXT    NOT NULL,
    site_id         TEXT    NOT NULL,
    manifest_sha256 TEXT    NOT NULL,
    seq             INTEGER NOT NULL CHECK (seq >= 0),
    data            BLOB    NOT NULL CHECK (length(data) <= 524288),
    PRIMARY KEY (workspace_id, site_id, manifest_sha256, seq),
    FOREIGN KEY (workspace_id, site_id, manifest_sha256)
        REFERENCES publish_bundles (workspace_id, site_id, manifest_sha256) ON DELETE CASCADE
) STRICT;

-- Per-job activation metadata (kept in 0005 so 0004 remains standalone):
-- auto_activate + credential_tag for activation-time auth recheck, bytes_verified
-- status, and the resulting published_id / cf_version_id once activated.
CREATE TABLE IF NOT EXISTS publish_activations (
    workspace_id    TEXT    NOT NULL,
    site_id         TEXT    NOT NULL,
    job_id          TEXT    NOT NULL,
    auto_activate   INTEGER NOT NULL DEFAULT 0 CHECK (auto_activate IN (0, 1)),
    credential_tag  TEXT    CHECK (credential_tag IS NULL OR length(credential_tag) BETWEEN 3 AND 64),
    bytes_verified  INTEGER NOT NULL DEFAULT 0 CHECK (bytes_verified IN (0, 1)),
    published_id    TEXT    CHECK (published_id IS NULL OR (length(published_id) BETWEEN 1 AND 64 AND published_id NOT GLOB '*[^a-z0-9_-]*')),
    cf_version_id   TEXT    CHECK (cf_version_id IS NULL OR (length(cf_version_id) = 36 AND cf_version_id NOT GLOB '*[^0-9a-f-]*')),
    PRIMARY KEY (workspace_id, job_id),
    FOREIGN KEY (workspace_id, job_id) REFERENCES publish_jobs (workspace_id, id) ON DELETE CASCADE
) STRICT;
