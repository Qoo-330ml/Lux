CREATE TABLE library_cover_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    is_manual INTEGER NOT NULL DEFAULT 0 CHECK (is_manual IN (0, 1)),
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'CANCELLED', 'FAILED')),
    processed_count INTEGER NOT NULL DEFAULT 0 CHECK (processed_count >= 0),
    total_count INTEGER NOT NULL DEFAULT 1 CHECK (total_count >= 0),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
);

CREATE INDEX idx_library_cover_jobs_status
    ON library_cover_jobs(status, created_at, id);

CREATE UNIQUE INDEX idx_library_cover_jobs_one_active_library
    ON library_cover_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');
