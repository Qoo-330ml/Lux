ALTER TABLE chapter_detection_job_items
    ADD COLUMN input_fingerprint BYTEA NOT NULL DEFAULT ''::bytea;
ALTER TABLE chapter_detection_job_items
    ADD COLUMN is_context BIGINT NOT NULL DEFAULT 0 CHECK (is_context IN (0, 1));

CREATE TABLE chapter_detection_source_states (
    source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL,
    input_fingerprint BYTEA NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('FOUND', 'NOT_FOUND', 'FAILED')),
    last_checked_at BIGINT NOT NULL,
    last_success_at BIGINT,
    next_retry_at BIGINT,
    error TEXT,
    intro_fingerprint BYTEA,
    credits_fingerprint BYTEA,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (source_id, plugin_id)
);

CREATE INDEX idx_chapter_detection_source_states_retry
    ON chapter_detection_source_states(plugin_id, next_retry_at, source_id);
