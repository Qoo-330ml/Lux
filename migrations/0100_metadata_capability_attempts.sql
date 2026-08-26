CREATE TABLE metadata_capability_attempts (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    capability TEXT NOT NULL CHECK (capability IN ('CREDITS', 'EXTERNAL_IDS', 'TRAILERS')),
    status TEXT NOT NULL CHECK (status IN ('AVAILABLE', 'UNAVAILABLE', 'FAILED')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_attempt_at INTEGER,
    next_retry_at INTEGER,
    error_code TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (item_id, provider, provider_id, capability)
);

CREATE INDEX idx_metadata_capability_attempts_item
    ON metadata_capability_attempts(item_id, provider, provider_id, capability);

CREATE INDEX idx_metadata_capability_attempts_retry
    ON metadata_capability_attempts(status, next_retry_at);
