CREATE TABLE metadata_capability_attempts (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    capability TEXT NOT NULL CHECK (capability IN ('CREDITS', 'EXTERNAL_IDS', 'TRAILERS')),
    status TEXT NOT NULL CHECK (status IN ('AVAILABLE', 'UNAVAILABLE', 'FAILED')),
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_attempt_at BIGINT,
    next_retry_at BIGINT,
    error_code TEXT,
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    PRIMARY KEY (item_id, provider, provider_id, capability)
);

CREATE INDEX idx_metadata_capability_attempts_item
    ON metadata_capability_attempts(item_id, provider, provider_id, capability);

CREATE INDEX idx_metadata_capability_attempts_retry
    ON metadata_capability_attempts(status, next_retry_at);
