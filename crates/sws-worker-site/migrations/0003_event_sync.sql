-- The deployed site owns one public Google Calendar snapshot per D1 database.
--
-- `identity_fingerprint` is carried over from the old
-- 0004_event_snapshot_identity.sql: it stays reserved so a future
-- identity-bound import can harden snapshot visibility without another ALTER
-- on deployed databases (the current reader hides only stale gcal snapshots).
CREATE TABLE IF NOT EXISTS event_sync_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_schedule_ms INTEGER NOT NULL,
    identity_fingerprint TEXT
);
INSERT INTO event_sync_state (id, last_schedule_ms) VALUES (1, 0);
