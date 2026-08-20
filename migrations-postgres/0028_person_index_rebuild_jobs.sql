CREATE TABLE person_index_rebuild_jobs (
    library_id TEXT PRIMARY KEY NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'QUEUED'
        CHECK (status IN ('QUEUED', 'RUNNING', 'COMPLETED', 'FAILED', 'CANCELLED')),
    cursor_id TEXT,
    processed_count BIGINT NOT NULL DEFAULT 0,
    total_count BIGINT NOT NULL DEFAULT 0,
    cancel_requested BIGINT NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    schema_version BIGINT NOT NULL DEFAULT 1,
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
);

CREATE INDEX idx_person_index_rebuild_jobs_status
    ON person_index_rebuild_jobs(status, updated_at DESC);
