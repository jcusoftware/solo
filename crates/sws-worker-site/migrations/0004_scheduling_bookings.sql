-- Call & booking scheduling storage (issue #44; adapted from the old
-- core/migrations/0002_scheduling.sql).
--
-- The legacy `events` columns and indexes that the old migration added
-- (`external_uid`, `location`, `start_utc`, `end_utc`, `is_free`, `updated_at`,
-- `idx_events_external_uid`, `idx_events_free_start`) already exist here in
-- their final shape because migrations/0002_public_events.sql creates the
-- `events` table in its deployed form for fresh databases; the old backfill
-- UPDATE has no rows to migrate and is therefore not carried over.

-- Bookable call claims: `pending` before provider writes, `confirmed` after,
-- `uncertain` when a provider write needs manual reconciliation (never retried
-- automatically). Guest details mirror the inquiry validation caps.
CREATE TABLE IF NOT EXISTS bookings (
    id INTEGER PRIMARY KEY,
    tenant_slug TEXT NOT NULL,
    guest_name TEXT NOT NULL CHECK (length(guest_name) BETWEEN 1 AND 100),
    guest_email TEXT NOT NULL CHECK (length(guest_email) BETWEEN 3 AND 254),
    guest_notes TEXT CHECK (length(guest_notes) <= 4000),
    start_utc TEXT NOT NULL,
    end_utc TEXT NOT NULL,
    buffer_minutes INTEGER NOT NULL CHECK (buffer_minutes BETWEEN 0 AND 1440),
    zoom_meeting_id TEXT,
    zoom_join_url TEXT,
    gcal_event_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'uncertain', 'confirmed', 'cancelled')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (unixepoch(start_utc) IS NOT NULL AND unixepoch(end_utc) IS NOT NULL
        AND unixepoch(start_utc) < unixepoch(end_utc))
);

CREATE INDEX IF NOT EXISTS idx_bookings_tenant_start_status ON bookings (tenant_slug, start_utc, status);

-- SQLite serializes writes: the overlap check and claim cannot race another insert.
-- Intervals are half-open, with the larger of the two buffers required between calls.
CREATE TRIGGER IF NOT EXISTS bookings_guard_insert BEFORE INSERT ON bookings
WHEN NEW.status IN ('pending', 'uncertain', 'confirmed') AND EXISTS (
    SELECT 1 FROM bookings AS b
    WHERE b.tenant_slug = NEW.tenant_slug AND b.status IN ('pending', 'uncertain', 'confirmed')
      AND unixepoch(NEW.start_utc) < unixepoch(b.end_utc) + 60 * max(b.buffer_minutes, NEW.buffer_minutes)
      AND unixepoch(b.start_utc) < unixepoch(NEW.end_utc) + 60 * max(b.buffer_minutes, NEW.buffer_minutes)
)
BEGIN
    SELECT RAISE(ABORT, 'booking_overlap');
END;

CREATE TRIGGER IF NOT EXISTS bookings_guard_update BEFORE UPDATE OF tenant_slug, start_utc, end_utc, buffer_minutes, status ON bookings
WHEN NEW.status IN ('pending', 'uncertain', 'confirmed') AND EXISTS (
    SELECT 1 FROM bookings AS b
    WHERE b.id != OLD.id AND b.tenant_slug = NEW.tenant_slug
      AND b.status IN ('pending', 'uncertain', 'confirmed')
      AND unixepoch(NEW.start_utc) < unixepoch(b.end_utc) + 60 * max(b.buffer_minutes, NEW.buffer_minutes)
      AND unixepoch(b.start_utc) < unixepoch(NEW.end_utc) + 60 * max(b.buffer_minutes, NEW.buffer_minutes)
)
BEGIN
    SELECT RAISE(ABORT, 'booking_overlap');
END;
