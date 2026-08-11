DROP INDEX idx_strm_probe_jobs_one_active_library;
DROP INDEX idx_strm_probe_jobs_operation;
DROP INDEX idx_strm_probe_jobs_status;
DROP INDEX idx_strm_probe_jobs_target_scan_job;

ALTER TABLE strm_probe_jobs RENAME TO strm_probe_jobs_legacy;

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
    finished_at INTEGER,
    media_info_enabled INTEGER NOT NULL DEFAULT 1 CHECK (media_info_enabled IN (0, 1)),
    thumbnail_enabled INTEGER NOT NULL DEFAULT 0 CHECK (thumbnail_enabled IN (0, 1)),
    thumbnail_position_percent INTEGER NOT NULL DEFAULT 30
        CHECK (thumbnail_position_percent BETWEEN 1 AND 99),
    target_scan_job_id TEXT REFERENCES scan_jobs(id) ON DELETE RESTRICT
);

INSERT INTO strm_probe_jobs (
    id, operation_id, library_id, status, concurrency,
    include_ready, write_sidecars, cursor, processed_count, total_count,
    cancel_requested, error, created_at, updated_at, started_at, finished_at,
    media_info_enabled, thumbnail_enabled, thumbnail_position_percent,
    target_scan_job_id
)
SELECT
    id, operation_id, library_id, status, concurrency,
    include_ready, write_sidecars, cursor, processed_count, total_count,
    cancel_requested, error, created_at, updated_at, started_at, finished_at,
    media_info_enabled, thumbnail_enabled, thumbnail_position_percent,
    target_scan_job_id
FROM strm_probe_jobs_legacy;

DROP TABLE strm_probe_jobs_legacy;

CREATE INDEX idx_strm_probe_jobs_operation
    ON strm_probe_jobs(operation_id, created_at, id);
CREATE INDEX idx_strm_probe_jobs_status
    ON strm_probe_jobs(status, created_at, id);
CREATE UNIQUE INDEX idx_strm_probe_jobs_one_active_library
    ON strm_probe_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');
CREATE INDEX idx_strm_probe_jobs_target_scan_job
    ON strm_probe_jobs(target_scan_job_id);
