-- SWS worker-site schema v1 (Cloudflare D1 / SQLite).
-- Domain-agnostic storage for schema-driven form submissions and CAN-SPAM newsletter consent.

-- Schema-driven form submissions.
CREATE TABLE IF NOT EXISTS form_submissions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    form_id      TEXT    NOT NULL
                         CHECK (length(form_id) BETWEEN 1 AND 64
                            AND form_id GLOB '[a-z0-9]*'
                            AND form_id NOT GLOB '*[^a-z0-9_-]*'),
    payload_json TEXT    NOT NULL CHECK (length(payload_json) >= 2 AND json_valid(payload_json)),
    source_route TEXT    NOT NULL CHECK (length(source_route) BETWEEN 1 AND 200 AND substr(source_route, 1, 1) = '/'),
    status       TEXT    NOT NULL DEFAULT 'new' CHECK (status IN ('new', 'reviewed', 'archived', 'spam')),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                         CHECK (length(created_at) = 20
                            AND substr(created_at, 5, 1) = '-'
                            AND substr(created_at, 8, 1) = '-'
                            AND substr(created_at, 11, 1) = 'T'
                            AND substr(created_at, 14, 1) = ':'
                            AND substr(created_at, 17, 1) = ':'
                            AND substr(created_at, 20, 1) = 'Z'
                            AND (substr(created_at, 1, 4) || substr(created_at, 6, 2) || substr(created_at, 9, 2) || substr(created_at, 12, 2) || substr(created_at, 15, 2) || substr(created_at, 18, 2)) NOT GLOB '*[^0-9]*')
);
CREATE INDEX IF NOT EXISTS idx_form_submissions_form_created ON form_submissions (form_id, created_at);
CREATE INDEX IF NOT EXISTS idx_form_submissions_status_created ON form_submissions (status, created_at);

-- Newsletter subscribers with CAN-SPAM consent record.
CREATE TABLE IF NOT EXISTS newsletter_subscribers (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    email             TEXT    NOT NULL UNIQUE COLLATE NOCASE CHECK (length(email) BETWEEN 3 AND 254),
    status            TEXT    NOT NULL DEFAULT 'subscribed' CHECK (status IN ('subscribed', 'unsubscribed', 'bounced')),
    consent_timestamp TEXT    NOT NULL
                              CHECK (substr(consent_timestamp, 5, 1) = '-'
                                 AND substr(consent_timestamp, 8, 1) = '-'
                                 AND substr(consent_timestamp, 11, 1) = 'T'
                                 AND substr(consent_timestamp, 14, 1) = ':'
                                 AND substr(consent_timestamp, 17, 1) = ':'
                                 AND (substr(consent_timestamp, 1, 4) || substr(consent_timestamp, 6, 2) || substr(consent_timestamp, 9, 2) || substr(consent_timestamp, 12, 2) || substr(consent_timestamp, 15, 2) || substr(consent_timestamp, 18, 2)) NOT GLOB '*[^0-9]*'
                                 AND ((length(consent_timestamp) = 20 AND substr(consent_timestamp, 20, 1) = 'Z')
                                   OR (length(consent_timestamp) = 24 AND substr(consent_timestamp, 20, 1) = '.' AND substr(consent_timestamp, 24, 1) = 'Z' AND substr(consent_timestamp, 21, 3) NOT GLOB '*[^0-9]*'))),
    source_route      TEXT    NOT NULL CHECK (length(source_route) BETWEEN 1 AND 200 AND substr(source_route, 1, 1) = '/'),
    consent_text      TEXT    CHECK (consent_text IS NULL OR length(consent_text) <= 500),
    created_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                              CHECK (length(created_at) = 20
                                 AND substr(created_at, 5, 1) = '-'
                                 AND substr(created_at, 8, 1) = '-'
                                 AND substr(created_at, 11, 1) = 'T'
                                 AND substr(created_at, 14, 1) = ':'
                                 AND substr(created_at, 17, 1) = ':'
                                 AND substr(created_at, 20, 1) = 'Z'
                                 AND (substr(created_at, 1, 4) || substr(created_at, 6, 2) || substr(created_at, 9, 2) || substr(created_at, 12, 2) || substr(created_at, 15, 2) || substr(created_at, 18, 2)) NOT GLOB '*[^0-9]*'),
    updated_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                              CHECK (length(updated_at) = 20
                                 AND substr(updated_at, 5, 1) = '-'
                                 AND substr(updated_at, 8, 1) = '-'
                                 AND substr(updated_at, 11, 1) = 'T'
                                 AND substr(updated_at, 14, 1) = ':'
                                 AND substr(updated_at, 17, 1) = ':'
                                 AND substr(updated_at, 20, 1) = 'Z'
                                 AND (substr(updated_at, 1, 4) || substr(updated_at, 6, 2) || substr(updated_at, 9, 2) || substr(updated_at, 12, 2) || substr(updated_at, 15, 2) || substr(updated_at, 18, 2)) NOT GLOB '*[^0-9]*'),
    unsubscribed_at   TEXT
                              CHECK (unsubscribed_at IS NULL
                                  OR (substr(unsubscribed_at, 5, 1) = '-'
                                  AND substr(unsubscribed_at, 8, 1) = '-'
                                  AND substr(unsubscribed_at, 11, 1) = 'T'
                                  AND substr(unsubscribed_at, 14, 1) = ':'
                                  AND substr(unsubscribed_at, 17, 1) = ':'
                                  AND (substr(unsubscribed_at, 1, 4) || substr(unsubscribed_at, 6, 2) || substr(unsubscribed_at, 9, 2) || substr(unsubscribed_at, 12, 2) || substr(unsubscribed_at, 15, 2) || substr(unsubscribed_at, 18, 2)) NOT GLOB '*[^0-9]*'
                                  AND ((length(unsubscribed_at) = 20 AND substr(unsubscribed_at, 20, 1) = 'Z')
                                    OR (length(unsubscribed_at) = 24 AND substr(unsubscribed_at, 20, 1) = '.' AND substr(unsubscribed_at, 24, 1) = 'Z' AND substr(unsubscribed_at, 21, 3) NOT GLOB '*[^0-9]*'))))
);
CREATE INDEX IF NOT EXISTS idx_newsletter_status ON newsletter_subscribers (status);
