-- no-transaction
-- SQLite cannot alter a CHECK constraint in place, so rebuild the job table.
PRAGMA foreign_keys = OFF;

ALTER TABLE metadata_reidentify_job_items RENAME TO metadata_reidentify_job_items_legacy;

ALTER TABLE metadata_reidentify_jobs RENAME TO metadata_reidentify_jobs_legacy;

CREATE TABLE metadata_reidentify_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'QUEUED', 'RUNNING', 'COMPLETED', 'COMPLETED_WITH_ISSUES', 'DEFERRED',
        'FAILED', 'CANCELLED'
    )),
    processed_count INTEGER NOT NULL DEFAULT 0,
    total_count INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    started_at INTEGER,
    finished_at INTEGER,
    mode TEXT NOT NULL DEFAULT 'REIDENTIFY'
        CHECK (mode IN ('REIDENTIFY', 'FILL_MISSING', 'FULL_REFRESH')),
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1))
);

INSERT INTO metadata_reidentify_jobs (
    id, status, processed_count, total_count, error,
    created_at, updated_at, started_at, finished_at, mode, cancel_requested
)
SELECT
    id, status, processed_count, total_count, error,
    created_at, updated_at, started_at, finished_at, mode, cancel_requested
FROM metadata_reidentify_jobs_legacy;

DROP TABLE metadata_reidentify_jobs_legacy;

CREATE TABLE metadata_reidentify_job_items (
    job_id TEXT NOT NULL REFERENCES metadata_reidentify_jobs(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'FAILED')),
    candidate_count INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, item_id)
);

INSERT INTO metadata_reidentify_job_items (
    job_id, item_id, status, candidate_count, error, updated_at
)
SELECT
    job_id, item_id, status, candidate_count, error, updated_at
FROM metadata_reidentify_job_items_legacy;

DROP TABLE metadata_reidentify_job_items_legacy;

CREATE INDEX idx_metadata_reidentify_items_status
    ON metadata_reidentify_job_items(job_id, status, item_id);

PRAGMA foreign_keys = ON;
