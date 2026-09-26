CREATE TABLE thumbnail_scraper_retries (
    item_id TEXT PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETE')),
    attempt_count BIGINT NOT NULL CHECK (attempt_count BETWEEN 1 AND 3),
    first_attempt_at BIGINT NOT NULL,
    next_retry_at BIGINT,
    claimed_until BIGINT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    CHECK (
        (status = 'PENDING' AND next_retry_at IS NOT NULL AND claimed_until IS NULL)
        OR (status = 'RUNNING' AND next_retry_at IS NOT NULL AND claimed_until IS NOT NULL)
        OR (status = 'COMPLETE' AND next_retry_at IS NULL AND claimed_until IS NULL)
    )
);

CREATE INDEX idx_thumbnail_scraper_retries_due
    ON thumbnail_scraper_retries(status, next_retry_at, claimed_until);
