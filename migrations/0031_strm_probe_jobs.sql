CREATE TABLE strm_probe_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    operation_id TEXT NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'CANCELLED', 'FAILED')),
    concurrency INTEGER NOT NULL CHECK (concurrency BETWEEN 1 AND 64),
    include_ready INTEGER NOT NULL DEFAULT 0 CHECK (include_ready IN (0, 1)),
    write_sidecars INTEGER NOT NULL DEFAULT 0 CHECK (write_sidecars IN (0, 1)),
    cursor TEXT,
    processed_count INTEGER NOT NULL DEFAULT 0,
    total_count INTEGER NOT NULL DEFAULT 0,
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    started_at INTEGER,
    finished_at INTEGER
);

CREATE INDEX idx_strm_probe_jobs_operation
    ON strm_probe_jobs(operation_id, created_at, id);
CREATE INDEX idx_strm_probe_jobs_status
    ON strm_probe_jobs(status, created_at, id);
CREATE UNIQUE INDEX idx_strm_probe_jobs_one_active_library
    ON strm_probe_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');
