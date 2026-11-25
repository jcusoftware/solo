-- Public events calendar storage (issue #43; ported from the pre-refactor
-- core/migrations chain).
--
-- This workspace ships no legacy `events` table, so the old 0001 (M6 events)
-- + 0002_public_events (public-event columns) pair is consolidated here into a
-- single fresh CREATE that matches the post-migration deployed shape exactly:
-- legacy columns (`starts_at`, `ends_at`, `is_ticketed`) are preserved because
-- the sync importer and reader SQL continue to reference them.

-- M6: events calendar (legacy columns kept for the ported SQL contract).
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    slug        TEXT    NOT NULL UNIQUE
                        CHECK (slug GLOB '[a-z0-9]*'
                           AND slug NOT GLOB '*[^a-z0-9-]*'
                           AND slug NOT GLOB '*-'
                           AND slug NOT GLOB '*--*'),
    title       TEXT    NOT NULL CHECK (length(trim(title)) >= 1),
    description TEXT,
    starts_at   TEXT    NOT NULL  -- normalized UTC ISO-8601 (YYYY-MM-DDTHH:MM:SSZ)
                        CHECK (length(starts_at) = 20
                           AND substr(starts_at, 5, 1) = '-'
                           AND substr(starts_at, 8, 1) = '-'
                           AND substr(starts_at, 11, 1) = 'T'
                           AND substr(starts_at, 14, 1) = ':'
                           AND substr(starts_at, 17, 1) = ':'
                           AND substr(starts_at, 20, 1) = 'Z'
                           AND (substr(starts_at, 1, 4) || substr(starts_at, 6, 2) || substr(starts_at, 9, 2) || substr(starts_at, 12, 2) || substr(starts_at, 15, 2) || substr(starts_at, 18, 2)) NOT GLOB '*[^0-9]*'),
    ends_at     TEXT              -- normalized UTC ISO-8601 (YYYY-MM-DDTHH:MM:SSZ)
                        CHECK (ends_at IS NULL
                            OR (length(ends_at) = 20
                            AND substr(ends_at, 5, 1) = '-'
                            AND substr(ends_at, 8, 1) = '-'
                            AND substr(ends_at, 11, 1) = 'T'
                            AND substr(ends_at, 14, 1) = ':'
                            AND substr(ends_at, 17, 1) = ':'
                            AND substr(ends_at, 20, 1) = 'Z'
                            AND (substr(ends_at, 1, 4) || substr(ends_at, 6, 2) || substr(ends_at, 9, 2) || substr(ends_at, 12, 2) || substr(ends_at, 15, 2) || substr(ends_at, 18, 2)) NOT GLOB '*[^0-9]*')),
    is_ticketed INTEGER NOT NULL DEFAULT 0 CHECK (is_ticketed IN (0, 1)),
    ticket_url  TEXT             CHECK (ticket_url IS NULL OR ticket_url LIKE 'https://%'),
    -- Public-event columns (old 0002_public_events.sql).
    external_uid TEXT,
    location     TEXT,
    start_utc    TEXT,
    end_utc      TEXT,
    is_free      INTEGER NOT NULL DEFAULT 1 CHECK (is_free IN (0, 1)),
    updated_at   TEXT,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                        CHECK (length(created_at) = 20
                           AND substr(created_at, 5, 1) = '-'
                           AND substr(created_at, 8, 1) = '-'
                           AND substr(created_at, 11, 1) = 'T'
                           AND substr(created_at, 14, 1) = ':'
                           AND substr(created_at, 17, 1) = ':'
                           AND substr(created_at, 20, 1) = 'Z'
                           AND (substr(created_at, 1, 4) || substr(created_at, 6, 2) || substr(created_at, 9, 2) || substr(created_at, 12, 2) || substr(created_at, 15, 2) || substr(created_at, 18, 2)) NOT GLOB '*[^0-9]*'),
    CHECK (ends_at IS NULL OR ends_at >= starts_at)
);
CREATE INDEX IF NOT EXISTS idx_events_starts_at ON events (starts_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_external_uid ON events (external_uid);
CREATE INDEX IF NOT EXISTS idx_events_free_start ON events (is_free, start_utc);
