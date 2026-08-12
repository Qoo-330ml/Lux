ALTER TABLE chapter_detection_job_items
    ADD COLUMN input_fingerprint BLOB NOT NULL DEFAULT X'';
ALTER TABLE chapter_detection_job_items
    ADD COLUMN is_context INTEGER NOT NULL DEFAULT 0 CHECK (is_context IN (0, 1));

CREATE TABLE chapter_detection_source_states (
    source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL,
    input_fingerprint BLOB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('FOUND', 'NOT_FOUND', 'FAILED')),
    last_checked_at INTEGER NOT NULL,
    last_success_at INTEGER,
    next_retry_at INTEGER,
    error TEXT,
    intro_fingerprint BLOB,
    credits_fingerprint BLOB,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (source_id, plugin_id)
);

CREATE INDEX idx_chapter_detection_source_states_retry
    ON chapter_detection_source_states(plugin_id, next_retry_at, source_id);
