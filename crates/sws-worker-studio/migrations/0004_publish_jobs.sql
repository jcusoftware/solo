-- SWS Studio publish jobs v1 (#64): asynchronous publish jobs against an immutable
-- `site_revisions` row, and the validated CLI artifact manifest each job carries.
-- Builds on 0001_registry.sql only. 0003 is reserved for #65 (auth); this file neither
-- reads nor writes anything 0003 adds, so Wrangler may apply the two in either order.
--
-- Why a new table instead of `deployments` (0001): its `status` CHECK cannot hold the
-- job phases (validating/compiling/ready) and SQLite cannot alter a CHECK in place, and
-- a deployment is the activation of a revision on the live site (#66), a different
-- lifecycle. #66 inserts a `deployments` row for a `ready` job.
--
-- Concurrency: every phase change is a conditional UPDATE guarded by
-- `phase = ? AND attempt = ?` (see src/publish/job.rs), so duplicate delivery is
-- harmless. Ids are app-generated opaque TEXT (1..64 chars of [a-z0-9_-]); a job id is
-- a hash of (workspace, site, request_id), so a retried enqueue maps to the same row.
-- Timestamps are canonical UTC text, checked like 0001's.

CREATE TABLE IF NOT EXISTS publish_jobs (
    id               TEXT    NOT NULL PRIMARY KEY
                             CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    workspace_id     TEXT    NOT NULL,
    site_id          TEXT    NOT NULL,
    revision_id      TEXT    NOT NULL,
    -- The client's idempotency key.
    request_id       TEXT    NOT NULL
                             CHECK (length(request_id) BETWEEN 1 AND 64 AND request_id NOT GLOB '*[^a-z0-9_-]*'),
    -- `cli`: compiled on the publisher's machine (#68). `server`: compiled by Studio
    -- (not implemented; such a job fails in `compiling`).
    origin           TEXT    NOT NULL CHECK (origin IN ('cli', 'server')),
    -- Studio never labels a CLI artifact as independently verified.
    server_verified  INTEGER NOT NULL DEFAULT 0
                             CHECK (server_verified IN (0, 1) AND (server_verified = 0 OR origin = 'server')),
    phase            TEXT    NOT NULL DEFAULT 'queued'
                             CHECK (phase IN ('queued', 'validating', 'compiling', 'uploading', 'ready', 'failed')),
    -- Claims so far; the current value is the owning driver's token.
    attempt          INTEGER NOT NULL DEFAULT 0 CHECK (attempt BETWEEN 0 AND 100),
    next_attempt_at  TEXT    NOT NULL
                             CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', next_attempt_at) IS next_attempt_at),
    lease_expires_at TEXT    CHECK (lease_expires_at IS NULL
                                    OR strftime('%Y-%m-%dT%H:%M:%SZ', lease_expires_at) IS lease_expires_at),
    -- Final failure: the code, and the phase it happened in.
    error_code       TEXT    CHECK (error_code IS NULL OR (length(error_code) BETWEEN 1 AND 64
                                                           AND error_code NOT GLOB '*[^a-z0-9_]*')),
    error_phase      TEXT    CHECK (error_phase IS NULL
                                    OR error_phase IN ('queued', 'validating', 'compiling', 'uploading')),
    -- Most recent transient error (kept across retries, and behind `retries_exhausted`).
    last_error       TEXT    CHECK (last_error IS NULL OR (length(last_error) BETWEEN 1 AND 64
                                                           AND last_error NOT GLOB '*[^a-z0-9_]*')),
    created_by       INTEGER NOT NULL REFERENCES users (id),
    created_at       TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    updated_at       TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', updated_at) IS updated_at),
    finished_at      TEXT    CHECK (finished_at IS NULL
                                    OR strftime('%Y-%m-%dT%H:%M:%SZ', finished_at) IS finished_at),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, site_id, revision_id)
        REFERENCES site_revisions (workspace_id, site_id, id) ON DELETE NO ACTION,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, site_id, request_id),
    -- Row invariants the state machine relies on.
    CHECK ((phase = 'failed') = (error_code IS NOT NULL)),
    CHECK ((phase = 'failed') = (error_phase IS NOT NULL)),
    CHECK ((phase IN ('ready', 'failed')) = (finished_at IS NOT NULL)),
    CHECK (phase NOT IN ('ready', 'failed') OR lease_expires_at IS NULL),
    CHECK (origin = 'server' OR phase <> 'compiling'),
    CHECK (origin = 'server' OR error_phase IS NULL OR error_phase <> 'compiling')
) STRICT;
-- The recovery sweep's "due" scan (the predicate must match PUBLISH_JOBS_DUE).
CREATE INDEX IF NOT EXISTS idx_publish_jobs_due
    ON publish_jobs (next_attempt_at) WHERE phase NOT IN ('ready', 'failed');
CREATE INDEX IF NOT EXISTS idx_publish_jobs_site
    ON publish_jobs (workspace_id, site_id, created_at);

-- The validated manifest of a job's artifact (canonical JSON: files sorted by path,
-- each with its SHA-256 and size). One row per job, immutable once written.
CREATE TABLE IF NOT EXISTS publish_artifacts (
    workspace_id     TEXT    NOT NULL,
    job_id           TEXT    NOT NULL,
    -- SHA-256 of `manifest`: the artifact identity.
    manifest_sha256  TEXT    NOT NULL
                             CHECK (length(manifest_sha256) = 64 AND manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
    -- The revision IR digest the CLI says it compiled; equal to site_revisions.ir_sha256
    -- when the job was enqueued and re-checked when it is validated.
    revision_sha256  TEXT    NOT NULL
                             CHECK (length(revision_sha256) = 64 AND revision_sha256 NOT GLOB '*[^0-9a-f]*'),
    compiler_name    TEXT    NOT NULL CHECK (length(compiler_name) BETWEEN 1 AND 64),
    compiler_version TEXT    NOT NULL CHECK (length(compiler_version) BETWEEN 1 AND 64),
    file_count       INTEGER NOT NULL CHECK (file_count BETWEEN 1 AND 100000),
    total_bytes      INTEGER NOT NULL CHECK (total_bytes >= 0),
    -- Capped at 256 KiB (sws_assets::limits::PUBLISH_ARTIFACT.max_manifest_bytes), far
    -- below the assumed ~2 MB D1 row limit.
    manifest         TEXT    NOT NULL
                             CHECK (length(manifest) BETWEEN 2 AND 262144 AND json_valid(manifest)),
    created_at       TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    PRIMARY KEY (workspace_id, job_id),
    FOREIGN KEY (workspace_id, job_id) REFERENCES publish_jobs (workspace_id, id) ON DELETE CASCADE
) STRICT;

CREATE TRIGGER IF NOT EXISTS trg_publish_artifacts_immutable
BEFORE UPDATE ON publish_artifacts
BEGIN
    SELECT RAISE(ABORT, 'publish_artifacts rows are immutable');
END;
