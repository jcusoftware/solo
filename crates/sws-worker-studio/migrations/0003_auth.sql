-- SWS Studio authentication (#65): browser sessions and explicit workspace
-- invitations. CLI/MCP tokens reuse `api_tokens` from 0001_registry.sql
-- unchanged. OAuth `state` is not stored at all: it is bound to the browser by
-- the `__Host-sws_oauth` cookie, so an anonymous sign-in start writes nothing.
--
-- Only SHA-256 hex digests of credentials are stored (`secret_hash`), never a
-- cookie value, token, invitation code or OAuth state.
-- Each credential has a separate random public lookup `id` (24 lowercase hex);
-- the digest is compared in constant time by the Worker.
--
-- Timestamps are canonical UTC text, validated exactly like 0001 (`strftime(...)
-- IS col`), so the lifetime CHECKs below can never compare against NULL.

CREATE TABLE IF NOT EXISTS auth_sessions (
    id           TEXT    NOT NULL PRIMARY KEY
                         CHECK (length(id) = 24 AND id NOT GLOB '*[^0-9a-f]*'),
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    secret_hash  TEXT    NOT NULL UNIQUE
                         CHECK (length(secret_hash) = 64 AND secret_hash NOT GLOB '*[^0-9a-f]*'),
    created_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    last_seen_at TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', last_seen_at) IS last_seen_at),
    -- Absolute end of the session (#60: 14 days after sign-in).
    expires_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', expires_at) IS expires_at),
    revoked_at   TEXT    CHECK (revoked_at IS NULL OR strftime('%Y-%m-%dT%H:%M:%SZ', revoked_at) IS revoked_at),
    CHECK (last_seen_at >= created_at),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+14 days'))
) STRICT;
CREATE INDEX IF NOT EXISTS idx_auth_sessions_user ON auth_sessions (user_id);

-- Explicit invitations. Never for the owner role (one owner, bootstrapped from
-- configuration). Bound to a GitHub login (stored lowercase) and single use.
CREATE TABLE IF NOT EXISTS invitations (
    id           TEXT    NOT NULL PRIMARY KEY
                         CHECK (length(id) = 24 AND id NOT GLOB '*[^0-9a-f]*'),
    workspace_id TEXT    NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    secret_hash  TEXT    NOT NULL UNIQUE
                         CHECK (length(secret_hash) = 64 AND secret_hash NOT GLOB '*[^0-9a-f]*'),
    github_login TEXT    NOT NULL
                         CHECK (length(github_login) BETWEEN 1 AND 39
                            AND github_login NOT GLOB '*[^a-z0-9-]*'),
    role         TEXT    NOT NULL CHECK (role IN ('publisher', 'editor')),
    invited_by   INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    expires_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', expires_at) IS expires_at),
    accepted_at  TEXT    CHECK (accepted_at IS NULL OR strftime('%Y-%m-%dT%H:%M:%SZ', accepted_at) IS accepted_at),
    accepted_by  INTEGER REFERENCES users (id) ON DELETE SET NULL,
    revoked_at   TEXT    CHECK (revoked_at IS NULL OR strftime('%Y-%m-%dT%H:%M:%SZ', revoked_at) IS revoked_at),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+7 days')),
    CHECK (accepted_at IS NULL OR revoked_at IS NULL)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_invitations_workspace ON invitations (workspace_id, created_at);

