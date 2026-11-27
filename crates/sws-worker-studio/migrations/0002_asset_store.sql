-- SWS Studio asset store v1 (#63): bounded blob storage, quota reservations and
-- generated preview/export budgets. Builds on 0001_registry.sql (`assets` holds the
-- metadata; this migration adds bytes, reservations and generated artifacts).
-- All numbers are PROVISIONAL assumptions (see docs/ARCHITECTURE.md, "Studio asset
-- storage"); the application caps and these CHECKs reduce billing risk but are not
-- provider-side hard stops.
--
-- Ids are app-generated opaque TEXT (1..64 chars of [a-z0-9_-]); `assets.id` is a
-- deterministic hash of workspace/site/sha256, `generated_artifacts.id` and
-- `asset_reservations.id` are app-generated. Timestamps are canonical UTC text.

-- Default (cardless) byte storage: D1 BLOB rows of at most 256 KiB each. The CHECK
-- hard-caps a row at 512 KiB, an order of magnitude below the assumed ~2 MB D1
-- row/value limit, so no code path can write an oversized row. `blob_id` is an
-- `assets.id` or a `generated_artifacts.id` (polymorphic, hence no FK): orphan
-- chunks are therefore possible in principle and are swept by reconciliation.
CREATE TABLE IF NOT EXISTS blob_chunks (
    workspace_id TEXT    NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    blob_id      TEXT    NOT NULL
                         CHECK (length(blob_id) BETWEEN 1 AND 64 AND blob_id NOT GLOB '*[^a-z0-9_-]*'),
    seq          INTEGER NOT NULL CHECK (seq >= 0),
    byte_size    INTEGER NOT NULL CHECK (byte_size BETWEEN 1 AND 524288),
    data         BLOB    NOT NULL CHECK (typeof(data) = 'blob' AND length(data) = byte_size),
    PRIMARY KEY (workspace_id, blob_id, seq)
) STRICT;

-- Capacity reservations. Usage = committed rows + live (expires_at > now)
-- reservations, so there is no counter to drift: a crashed request's reservation
-- just stops counting at `expires_at` and is deleted by reconciliation.
CREATE TABLE IF NOT EXISTS asset_reservations (
    workspace_id TEXT    NOT NULL,
    site_id      TEXT    NOT NULL,
    id           TEXT    NOT NULL
                         CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    kind         TEXT    NOT NULL CHECK (kind IN ('asset', 'preview', 'export')),
    byte_size    INTEGER NOT NULL CHECK (byte_size > 0),
    created_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    expires_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', expires_at) IS expires_at),
    PRIMARY KEY (workspace_id, id),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS idx_asset_reservations_site
    ON asset_reservations (workspace_id, site_id, kind, expires_at);

-- Generated preview/export artifacts: budgeted and retained separately from assets.
CREATE TABLE IF NOT EXISTS generated_artifacts (
    workspace_id    TEXT    NOT NULL,
    site_id         TEXT    NOT NULL,
    id              TEXT    NOT NULL
                            CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    kind            TEXT    NOT NULL CHECK (kind IN ('preview', 'export')),
    sha256          TEXT    NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    byte_size       INTEGER NOT NULL CHECK (byte_size > 0),
    storage_backend TEXT    NOT NULL CHECK (storage_backend IN ('d1', 'r2')),
    created_at      TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    expires_at      TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', expires_at) IS expires_at),
    PRIMARY KEY (workspace_id, id),
    CHECK (expires_at > created_at),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS idx_generated_site_kind
    ON generated_artifacts (workspace_id, site_id, kind, created_at);
