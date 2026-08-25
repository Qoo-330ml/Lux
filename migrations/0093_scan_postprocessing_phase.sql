-- no-transaction
PRAGMA foreign_keys = OFF;

CREATE TABLE scan_jobs_new (
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
    finished_at INTEGER,
    discovery_completed INTEGER NOT NULL DEFAULT 0 CHECK (discovery_completed IN (0, 1)),
    auto_metadata_match INTEGER NOT NULL DEFAULT 0 CHECK (auto_metadata_match IN (0, 1)),
    current_item TEXT,
    scan_phase TEXT NOT NULL DEFAULT 'IDLE'
        CHECK (scan_phase IN ('DISCOVERY', 'INDEXING', 'FINALIZING', 'POSTPROCESSING', 'IDLE'))
);

INSERT INTO scan_jobs_new (
    id, library_id, job_type, status, generation, cursor,
    processed_count, total_count, cancel_requested, error,
    created_at, updated_at, started_at, finished_at,
    discovery_completed, auto_metadata_match, current_item, scan_phase
)
SELECT
    id, library_id, job_type, status, generation, cursor,
    processed_count, total_count, cancel_requested, error,
    created_at, updated_at, started_at, finished_at,
    discovery_completed, auto_metadata_match, current_item, scan_phase
FROM scan_jobs;

DROP TABLE scan_jobs;
ALTER TABLE scan_jobs_new RENAME TO scan_jobs;

CREATE INDEX idx_scan_jobs_library_status ON scan_jobs(library_id, status, created_at);
CREATE UNIQUE INDEX idx_scan_jobs_one_active
    ON scan_jobs(library_id, job_type)
    WHERE status IN ('PENDING', 'RUNNING');

PRAGMA foreign_keys = ON;
