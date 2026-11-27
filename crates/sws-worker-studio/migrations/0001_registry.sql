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
-- Tenant scoping: every tenant-owned table carries `workspace_id`, and child
-- rows reference their parent through a composite (workspace_id, id) foreign
-- key, so a row can never point at a parent in another workspace.

-- ids are app-generated opaque TEXT: 1..64 chars of [a-z0-9_-], lowercase.
CREATE TABLE IF NOT EXISTS workspaces (
    id         TEXT NOT NULL PRIMARY KEY
                    CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                    CHECK (length(created_at) = 20)
) STRICT;

-- Keyed by the immutable GitHub numeric id; the login is display-only and mutable.
CREATE TABLE IF NOT EXISTS users (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    github_id    INTEGER NOT NULL UNIQUE CHECK (github_id > 0),
    github_login TEXT    NOT NULL CHECK (length(github_login) BETWEEN 1 AND 39),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (length(created_at) = 20),
    updated_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (length(updated_at) = 20)
) STRICT;

CREATE TABLE IF NOT EXISTS memberships (
    workspace_id TEXT    NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role         TEXT    NOT NULL CHECK (role IN ('owner', 'publisher', 'editor')),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (length(created_at) = 20),
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
                         AND slug NOT GLOB '*[^a-z0-9-]*'),
    name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                      CHECK (length(created_at) = 20),
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
    created_at     TEXT    NOT NULL CHECK (length(created_at) = 20),
    expires_at     TEXT    NOT NULL CHECK (length(expires_at) = 20),
    -- Start of the renewal chain; a chain may live at most 90 days.
    chain_started_at TEXT  NOT NULL CHECK (length(chain_started_at) = 20),
    last_used_at   TEXT    CHECK (last_used_at IS NULL OR length(last_used_at) = 20),
    revoked_at     TEXT    CHECK (revoked_at IS NULL OR length(revoked_at) = 20),
    FOREIGN KEY (workspace_id, user_id) REFERENCES memberships (workspace_id, user_id) ON DELETE CASCADE,
    CHECK (chain_started_at <= created_at),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', chain_started_at, '+90 days'))
) STRICT;
CREATE INDEX IF NOT EXISTS idx_api_tokens_workspace ON api_tokens (workspace_id, user_id);

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
                            CHECK (length(created_at) = 20),
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
                         CHECK (length(created_at) = 20),
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
                         CHECK (length(created_at) = 20),
    updated_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (length(updated_at) = 20),
    finished_at  TEXT    CHECK (finished_at IS NULL OR length(finished_at) = 20),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, site_id, revision_id)
        REFERENCES site_revisions (workspace_id, site_id, id) ON DELETE RESTRICT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_deployments_workspace_site ON deployments (workspace_id, site_id, created_at);
