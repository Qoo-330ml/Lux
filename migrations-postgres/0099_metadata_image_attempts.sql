CREATE TABLE metadata_image_attempts (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    image_type TEXT NOT NULL,
    candidate_key TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('RUNNING', 'AVAILABLE', 'UNAVAILABLE', 'FAILED')),
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_attempt_at BIGINT,
    next_retry_at BIGINT,
    claimed_until BIGINT,
    error_code TEXT,
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    PRIMARY KEY (item_id, image_type, candidate_key)
);

CREATE INDEX idx_metadata_image_attempts_retry
    ON metadata_image_attempts(status, next_retry_at);

CREATE INDEX idx_metadata_image_attempts_item
    ON metadata_image_attempts(item_id, image_type);
