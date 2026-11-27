-- SWS Studio registry schema v1 (Cloudflare D1 / SQLite, STRICT tables).
-- Metadata only: workspaces, sites, users, memberships, API-token hashes, asset
-- and revision/deployment metadata. Blob storage (#63), sessions/OAuth (#65),
-- Durable Object edit sessions (#62) and the publish pipeline (#64/#66) are
-- NOT part of this schema.
--
-- Write-frequency invariant: this registry is written only on explicit
-- save/publish/checkpoint actions, never once per edit. Per-edit state lives in
-- the (future) session layer; `site_revisions` rows are immutable checkpoints.
--
-- Timestamps are canonical UTC text `YYYY-MM-DDTHH:MM:SSZ`. Every `*_at` column
-- is checked with `strftime(<fmt>, col) IS col`: a malformed or impossible date
-- (e.g. `0000-00-00T00:00:00Z`, `2026-02-30...`, space-separated or offset forms)
-- yields NULL or a different string and is rejected. `IS` (not `=`) is used so a
-- NULL result fails the CHECK instead of passing it.
--
-- `created_by` on site_revisions/deployments references global `users`, not
-- `memberships`, on purpose: a membership FK would either block removing a
-- member who authored history (NO ACTION) or erase history (CASCADE), and
-- SET NULL would break NOT NULL authorship. Whether the author is a member of
-- the workspace (with a sufficient role) is enforced by the application layer
-- (#65) at write time; the row keeps the author after they leave.
--
-- Tenant scoping: every tenant-owned table carries `workspace_id`, and child
-- rows reference their parent through a composite (workspace_id, id) foreign
-- key, so a row can never point at a parent in another workspace.

-- ids are app-generated opaque TEXT: 1..64 chars of [a-z0-9_-], lowercase.
CREATE TABLE IF NOT EXISTS workspaces (
    id         TEXT NOT NULL PRIMARY KEY
                    CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                    CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at)
) STRICT;

-- Keyed by the immutable GitHub numeric id; the login is display-only and mutable.
CREATE TABLE IF NOT EXISTS users (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    github_id    INTEGER NOT NULL UNIQUE CHECK (github_id > 0),
    github_login TEXT    NOT NULL CHECK (length(github_login) BETWEEN 1 AND 39),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    updated_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', updated_at) IS updated_at)
) STRICT;

CREATE TABLE IF NOT EXISTS memberships (
    workspace_id TEXT    NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role         TEXT    NOT NULL CHECK (role IN ('owner', 'publisher', 'editor')),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    PRIMARY KEY (workspace_id, user_id)
) STRICT;
-- At most one owner per workspace.
CREATE UNIQUE INDEX IF NOT EXISTS idx_memberships_one_owner
    ON memberships (workspace_id) WHERE role = 'owner';
CREATE INDEX IF NOT EXISTS idx_memberships_user ON memberships (user_id);

CREATE TABLE IF NOT EXISTS sites (
    id           TEXT NOT NULL PRIMARY KEY
                      CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    workspace_id TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    slug         TEXT NOT NULL
                      CHECK (length(slug) BETWEEN 1 AND 63
                         AND slug GLOB '[a-z0-9]*'
                         AND slug NOT GLOB '*[^a-z0-9-]*'
                         AND slug NOT GLOB '*-'),
    name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                      CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    UNIQUE (workspace_id, slug),
    UNIQUE (workspace_id, id)
) STRICT;

-- Only a SHA-256 hex digest of the token is stored, never the token itself.
-- Auth flows (issue, verify, rotate) are #65; this is the metadata table.
CREATE TABLE IF NOT EXISTS api_tokens (
    id             TEXT    NOT NULL PRIMARY KEY
                           CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    workspace_id   TEXT    NOT NULL,
    user_id        INTEGER NOT NULL,
    token_hash     TEXT    NOT NULL UNIQUE
                           CHECK (length(token_hash) = 64 AND token_hash NOT GLOB '*[^0-9a-f]*'),
    scope          TEXT    NOT NULL CHECK (scope IN ('edit', 'publish', 'activate')),
    label          TEXT    NOT NULL DEFAULT '' CHECK (length(label) <= 120),
    created_at     TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    expires_at     TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', expires_at) IS expires_at),
    -- Start of the renewal chain; a chain may live at most 90 days.
    chain_started_at TEXT  NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', chain_started_at) IS chain_started_at),
    last_used_at   TEXT    CHECK (last_used_at IS NULL OR strftime('%Y-%m-%dT%H:%M:%SZ', last_used_at) IS last_used_at),
    revoked_at     TEXT    CHECK (revoked_at IS NULL OR strftime('%Y-%m-%dT%H:%M:%SZ', revoked_at) IS revoked_at),
    FOREIGN KEY (workspace_id, user_id) REFERENCES memberships (workspace_id, user_id) ON DELETE CASCADE,
    CHECK (chain_started_at <= created_at),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', chain_started_at, '+90 days')),
    -- Per-scope lifetime caps from #60. Every timestamp above is validated as a
    -- canonical UTC value, so these strftime() calls can never yield NULL (which
    -- would let a CHECK pass).
    CHECK (expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', created_at,
        CASE scope WHEN 'edit' THEN '+30 days' WHEN 'publish' THEN '+7 days' ELSE '+24 hours' END))
) STRICT;
CREATE INDEX IF NOT EXISTS idx_api_tokens_workspace ON api_tokens (workspace_id, user_id);
-- Serves the "at most 10 active tokens per user" count (#60), which is not workspace-scoped.
CREATE INDEX IF NOT EXISTS idx_api_tokens_user_active ON api_tokens (user_id) WHERE revoked_at IS NULL;

-- Asset metadata only. `storage_backend` says where the bytes live (#63).
CREATE TABLE IF NOT EXISTS assets (
    id              TEXT    NOT NULL PRIMARY KEY
                            CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    workspace_id    TEXT    NOT NULL,
    site_id         TEXT    NOT NULL,
    sha256          TEXT    NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    byte_size       INTEGER NOT NULL CHECK (byte_size >= 0),
    content_type    TEXT    NOT NULL CHECK (length(content_type) BETWEEN 3 AND 127 AND instr(content_type, '/') > 1),
    storage_backend TEXT    NOT NULL CHECK (storage_backend IN ('d1', 'r2')),
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                            CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE,
    UNIQUE (workspace_id, site_id, sha256)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_assets_workspace_site ON assets (workspace_id, site_id);

-- Immutable checkpoints, written on explicit save/publish only (never per edit).
CREATE TABLE IF NOT EXISTS site_revisions (
    id           TEXT    NOT NULL PRIMARY KEY
                         CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    workspace_id TEXT    NOT NULL,
    site_id      TEXT    NOT NULL,
    revision     INTEGER NOT NULL CHECK (revision >= 1),
    ir_sha256    TEXT    NOT NULL CHECK (length(ir_sha256) = 64 AND ir_sha256 NOT GLOB '*[^0-9a-f]*'),
    byte_size    INTEGER NOT NULL CHECK (byte_size >= 0),
    created_by   INTEGER NOT NULL REFERENCES users (id),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE,
    UNIQUE (workspace_id, site_id, revision),
    UNIQUE (workspace_id, site_id, id)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_site_revisions_workspace ON site_revisions (workspace_id, site_id);

CREATE TRIGGER IF NOT EXISTS trg_site_revisions_immutable
BEFORE UPDATE ON site_revisions
BEGIN
    SELECT RAISE(ABORT, 'site_revisions rows are immutable');
END;

-- Deployment metadata; each deployment points at one immutable revision.
CREATE TABLE IF NOT EXISTS deployments (
    id           TEXT    NOT NULL PRIMARY KEY
                         CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    workspace_id TEXT    NOT NULL,
    site_id      TEXT    NOT NULL,
    revision_id  TEXT    NOT NULL,
    status       TEXT    NOT NULL DEFAULT 'queued'
                         CHECK (status IN ('queued', 'building', 'uploading', 'succeeded', 'failed', 'cancelled')),
    error_code   TEXT    CHECK (error_code IS NULL OR length(error_code) BETWEEN 1 AND 64),
    created_by   INTEGER NOT NULL REFERENCES users (id),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    updated_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', updated_at) IS updated_at),
    finished_at  TEXT    CHECK (finished_at IS NULL OR strftime('%Y-%m-%dT%H:%M:%SZ', finished_at) IS finished_at),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, site_id, revision_id)
        REFERENCES site_revisions (workspace_id, site_id, id) ON DELETE NO ACTION
) STRICT;
CREATE INDEX IF NOT EXISTS idx_deployments_workspace_site ON deployments (workspace_id, site_id, created_at);
