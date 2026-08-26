CREATE TABLE metadata_image_attempts (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    image_type TEXT NOT NULL,
    candidate_key TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('RUNNING', 'AVAILABLE', 'UNAVAILABLE', 'FAILED')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_attempt_at INTEGER,
    next_retry_at INTEGER,
    claimed_until INTEGER,
    error_code TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (item_id, image_type, candidate_key)
);

CREATE INDEX idx_metadata_image_attempts_retry
    ON metadata_image_attempts(status, next_retry_at);

CREATE INDEX idx_metadata_image_attempts_item
    ON metadata_image_attempts(item_id, image_type);
