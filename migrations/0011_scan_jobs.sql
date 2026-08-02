CREATE TABLE scan_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    job_type TEXT NOT NULL CHECK (job_type IN ('RECONCILE_LIBRARY', 'INCREMENTAL_SCAN')),
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'CANCELLED', 'FAILED')),
    generation TEXT NOT NULL,
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

CREATE INDEX idx_scan_jobs_library_status ON scan_jobs(library_id, status, created_at);
CREATE UNIQUE INDEX idx_scan_jobs_one_active
    ON scan_jobs(library_id, job_type)
    WHERE status IN ('PENDING', 'RUNNING');
