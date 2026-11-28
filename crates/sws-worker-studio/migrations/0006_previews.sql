-- SWS Studio draft previews (#67). Builds on 0001_registry.sql and
-- 0002_asset_store.sql. Numbered 0006 because 0004 (#64 publish jobs) and 0005
-- (#66 publish targets) are reserved by other in-flight work; it depends on
-- neither.
--
-- A preview is ONE generated artifact (`generated_artifacts`, kind 'preview')
-- holding a `sws-preview-bundle/1` (see `src/preview/bundle.rs`), so it lives
-- under the existing Preview budget and retention (5 MiB each, 10 MiB per site,
-- 24 h, newest 3). This table only maps an unguessable link token to that
-- artifact, pinned to one immutable `site_revisions` row.
--
-- Only the SHA-256 hex digest of the 128-bit token is stored. A row outlives
-- its artifact on purpose (retention, eviction or revocation remove the
-- artifact): the serving route then answers 410 instead of 404 until the row is
-- swept, 7 days after `expires_at`.

CREATE TABLE IF NOT EXISTS previews (
    workspace_id TEXT    NOT NULL,
    site_id      TEXT    NOT NULL,
    id           TEXT    NOT NULL
                         CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^a-z0-9_-]*'),
    revision_id  TEXT    NOT NULL,
    artifact_id  TEXT    NOT NULL
                         CHECK (length(artifact_id) BETWEEN 1 AND 64 AND artifact_id NOT GLOB '*[^a-z0-9_-]*'),
    token_hash   TEXT    NOT NULL UNIQUE
                         CHECK (length(token_hash) = 64 AND token_hash NOT GLOB '*[^0-9a-f]*'),
    created_by   INTEGER NOT NULL REFERENCES users (id),
    created_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', created_at) IS created_at),
    expires_at   TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', expires_at) IS expires_at),
    revoked_at   TEXT    CHECK (revoked_at IS NULL OR strftime('%Y-%m-%dT%H:%M:%SZ', revoked_at) IS revoked_at),
    PRIMARY KEY (workspace_id, id),
    UNIQUE (workspace_id, artifact_id),
    CHECK (expires_at > created_at),
    -- Never longer than the Preview retention (24 h).
    CHECK (expires_at <= strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+86400 seconds')),
    FOREIGN KEY (workspace_id, site_id) REFERENCES sites (workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, site_id, revision_id)
        REFERENCES site_revisions (workspace_id, site_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS idx_previews_site_expiry ON previews (workspace_id, site_id, expires_at);

-- Immutable: the only permitted change is a one-way revoke (NULL -> timestamp).
CREATE TRIGGER IF NOT EXISTS trg_previews_immutable
BEFORE UPDATE ON previews
WHEN OLD.revoked_at IS NOT NULL
  OR NEW.revoked_at IS NULL
  OR NEW.workspace_id IS NOT OLD.workspace_id
  OR NEW.site_id IS NOT OLD.site_id
  OR NEW.id IS NOT OLD.id
  OR NEW.revision_id IS NOT OLD.revision_id
  OR NEW.artifact_id IS NOT OLD.artifact_id
  OR NEW.token_hash IS NOT OLD.token_hash
  OR NEW.created_by IS NOT OLD.created_by
  OR NEW.created_at IS NOT OLD.created_at
  OR NEW.expires_at IS NOT OLD.expires_at
BEGIN
    SELECT RAISE(ABORT, 'previews rows are immutable except a one-way revoke');
END;
